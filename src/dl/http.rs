//! HTTP(S) downloads: several connections per file (byte ranges), work stealing so
//! fast connections take over the rest of slow ones, retries, and resume.
//!
//! The file is written straight into place as `<name>.stashpart` (hidden, so the stack
//! ignores it) and renamed when complete.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;

use super::Ev;
use super::limiter::Limiter;

/// A connection only takes over half of another's range when that half is at least this big.
const MIN_SPLIT: u64 = 512 * 1024;
const RETRIES: u32 = 6;
const OPEN: u64 = u64::MAX; // a range whose end isn't known (no Content-Length)

#[derive(Clone, Copy, Debug)]
struct Seg {
    pos: u64,
    end: u64,
    busy: bool,
}

/// Shared between the connections of one download and the engine (which reads it for
/// progress and saves the remaining ranges when pausing).
pub struct Progress {
    pub done: AtomicU64,
    pub connections: AtomicUsize,
    segs: Mutex<Vec<Seg>>,
}

impl Progress {
    pub fn new(done: u64, remaining: &[(u64, u64)]) -> Self {
        let segs = remaining.iter().map(|&(pos, end)| Seg { pos, end, busy: false }).collect();
        Self { done: AtomicU64::new(done), connections: AtomicUsize::new(0), segs: Mutex::new(segs) }
    }

    pub fn remaining(&self) -> Vec<(u64, u64)> {
        self.segs.lock().unwrap().iter().filter(|s| s.pos < s.end).map(|s| (s.pos, s.end)).collect()
    }

    fn reset(&self, total: Option<u64>) {
        *self.segs.lock().unwrap() = vec![Seg { pos: 0, end: total.unwrap_or(OPEN), busy: true }];
        self.done.store(0, Ordering::Relaxed);
    }

    fn seg(&self, i: usize) -> Seg {
        self.segs.lock().unwrap()[i]
    }

    /// Mark the range starting at `pos` as taken by the caller.
    fn take_at(&self, pos: u64) -> Option<usize> {
        let mut segs = self.segs.lock().unwrap();
        let i = segs.iter().position(|s| s.pos == pos && s.pos < s.end)?;
        segs[i].busy = true;
        Some(i)
    }

    /// A range for a free connection: an unclaimed one, or the back half of the largest.
    fn claim(&self, split: bool) -> Option<usize> {
        let mut segs = self.segs.lock().unwrap();
        if let Some(i) = segs.iter().position(|s| !s.busy && s.pos < s.end) {
            segs[i].busy = true;
            return Some(i);
        }
        if !split {
            return None;
        }
        let (i, rem) = segs.iter().enumerate().filter(|(_, s)| s.end != OPEN).map(|(i, s)| (i, s.end.saturating_sub(s.pos))).max_by_key(|(_, r)| *r)?;
        if rem < MIN_SPLIT * 2 {
            return None;
        }
        let mid = (segs[i].pos + rem / 2) & !0xFFFF; // 64 KiB aligned
        let end = segs[i].end;
        segs[i].end = mid;
        segs.push(Seg { pos: mid, end, busy: true });
        Some(segs.len() - 1)
    }

    fn release(&self, i: usize) {
        self.segs.lock().unwrap()[i].busy = false;
    }
}

pub struct Task {
    pub id: u64,
    pub url: String,
    pub dir: PathBuf,
    pub name: Option<String>,
    pub temp: Option<PathBuf>,
    pub total: Option<u64>,
    pub resumable: bool,
    pub headers: Vec<(String, String)>,
    /// `name` came from the browser (trust it over the server's)
    pub from_browser: bool,
    pub connections: usize,
    pub client: reqwest::Client,
    pub limiter: Arc<Limiter>,
    pub progress: Arc<Progress>,
    pub events: UnboundedSender<Ev>,
}

struct Fail {
    msg: String,
    fatal: bool,
}

impl Fail {
    fn net(e: impl std::fmt::Display) -> Self {
        Fail { msg: e.to_string(), fatal: false }
    }
    fn fatal(e: impl std::fmt::Display) -> Self {
        Fail { msg: e.to_string(), fatal: true }
    }
}

struct Ctx {
    url: String,
    headers: Vec<(String, String)>,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    progress: Arc<Progress>,
    file: Arc<std::fs::File>,
    split: bool,
}

impl Task {
    pub async fn run(self) {
        let (id, events) = (self.id, self.events.clone());
        match self.go().await {
            Ok((path, total)) => {
                let _ = events.send(Ev::HttpDone { id, path, total });
            }
            Err(e) => {
                let _ = events.send(Ev::Failed { id, error: e });
            }
        }
    }

    async fn go(self) -> Result<(PathBuf, u64), String> {
        let first_remaining = self.progress.remaining().first().map(|r| r.0);
        let can_resume = self.resumable && self.temp.is_some() && self.total.is_some() && first_remaining.is_some();

        // Try to carry on from where we stopped; if the file on the server changed (or it
        // no longer serves ranges), start over.
        let mut resumed = None;
        if can_resume {
            let start = first_remaining.unwrap();
            let resp = request(&self.client, &self.url, &self.headers, start, OPEN).await.map_err(|e| describe(&e))?;
            let total = content_range_total(&resp);
            if resp.status().as_u16() == 206 && total == self.total {
                resumed = Some((resp, start));
            }
        }

        let (temp, total, resumable, file, first) = match resumed {
            Some((resp, start)) => {
                let temp = self.temp.clone().unwrap();
                let file = std::fs::OpenOptions::new().write(true).open(&temp).map_err(|e| e.to_string())?;
                let name = self.name.clone().unwrap_or_else(|| "download".into());
                let _ = self.events.send(Ev::HttpMeta { id: self.id, name, total: self.total, resumable: true, temp: temp.clone() });
                let i = self.progress.take_at(start).ok_or("lost track of the download")?;
                (temp, self.total, true, file, (i, resp))
            }
            None => {
                if let Some(t) = &self.temp {
                    let _ = std::fs::remove_file(t);
                }
                let resp = first_request(&self.client, &self.url, &self.headers).await?;
                let status = resp.status();
                let ranges = status.as_u16() == 206 && content_range_total(&resp).is_some();
                let total = if status.as_u16() == 206 { content_range_total(&resp) } else { resp.content_length() };
                let name = self.name.clone().filter(|_| self.from_browser).unwrap_or_else(|| file_name(&resp, &self.url));
                std::fs::create_dir_all(&self.dir).map_err(|e| format!("Can't create the folder: {e}"))?;
                let temp = crate::files::unique_dest(&self.dir, &format!("{name}.stashpart"));
                let file = std::fs::File::create(&temp).map_err(|e| format!("Can't create the file: {e}"))?;
                if let Some(t) = total {
                    file.set_len(t).map_err(|e| format!("Not enough disk space: {e}"))?;
                }
                set_hidden(&temp, true);
                self.progress.reset(total);
                let _ = self.events.send(Ev::HttpMeta { id: self.id, name, total, resumable: ranges, temp: temp.clone() });
                (temp, total, ranges, file, (0, resp))
            }
        };

        let ctx = Arc::new(Ctx {
            url: resp_url(&first.1).unwrap_or(self.url.clone()),
            headers: self.headers.clone(),
            client: self.client.clone(),
            limiter: self.limiter.clone(),
            progress: self.progress.clone(),
            file: Arc::new(file),
            split: resumable && total.is_some(),
        });
        // an app shows its own icon long before it's done: fetch just that, on the side
        let shown_name = temp.file_name().unwrap_or_default().to_string_lossy().trim_end_matches(".stashpart").to_lowercase();
        if resumable && total.is_some() && [".exe", ".dll", ".scr"].iter().any(|e| shown_name.ends_with(e)) {
            let icon = super::app_icon(self.id);
            if !icon.exists() {
                let (client, url, headers) = (self.client.clone(), ctx.url.clone(), self.headers.clone());
                tokio::spawn(async move {
                    let _ = early_icon(&client, &url, &headers, &icon).await;
                });
            }
        }
        // a photo's own little preview, from the start of the file
        if resumable && total.is_some() && [".jpg", ".jpeg", ".jfif"].iter().any(|e| shown_name.ends_with(e)) {
            let preview = super::photo_preview(self.id);
            if !preview.exists() {
                let (client, url, headers) = (self.client.clone(), ctx.url.clone(), self.headers.clone());
                tokio::spawn(async move {
                    let _ = early_photo(&client, &url, &headers, &preview).await;
                });
            }
        }
        let n = if ctx.split { self.connections.max(1) } else { 1 };
        let mut set = JoinSet::new();
        set.spawn(worker(ctx.clone(), Some(first)));
        for k in 1..n {
            let ctx = ctx.clone();
            set.spawn(async move {
                // open the extra connections a moment apart, like a browser would
                tokio::time::sleep(Duration::from_millis(150 * k as u64)).await;
                worker(ctx, None).await
            });
        }
        while let Some(r) = set.join_next().await {
            match r {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e.msg),
                Err(e) if e.is_cancelled() => return Err("Stopped".into()),
                Err(e) => return Err(e.to_string()),
            }
        }
        if !self.progress.remaining().is_empty() {
            return Err("The download stopped before the end".into());
        }
        let done = self.progress.done.load(Ordering::Relaxed);
        drop(ctx);
        set_hidden(&temp, false);
        let final_name = temp.file_name().unwrap().to_string_lossy().trim_end_matches(".stashpart").to_string();
        let dest = crate::files::unique_dest(&self.dir, &final_name);
        std::fs::rename(&temp, &dest).map_err(|e| format!("Can't finish the file: {e}"))?;
        // the real file has its own icon or thumbnail now
        let _ = std::fs::remove_file(super::app_icon(self.id));
        let _ = std::fs::remove_file(super::photo_preview(self.id));
        Ok((dest, total.unwrap_or(done)))
    }
}

/// The icon of an app (a Windows PE file) that's still downloading. Its header, at the
/// start, says where the resources (the icons) are; those are usually small, and often
/// near the start too. Fetch the header and the resources with two range requests and
/// write them at their offsets into an otherwise empty file: the shell reads the icon
/// from that like from the real app.
async fn early_icon(client: &reqwest::Client, url: &str, headers: &[(String, String)], dest: &Path) -> Option<()> {
    const MAX_RESOURCES: u64 = 24 * 1024 * 1024;
    let get = |pos: u64, end: u64| async move {
        let r = request(client, url, headers, pos, end).await.ok()?;
        if r.status().as_u16() != 206 {
            return None;
        }
        r.bytes().await.ok()
    };
    let head = get(0, 64 * 1024).await?;
    let u16_at = |o: usize| Some(u16::from_le_bytes(head.get(o..o + 2)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(head.get(o..o + 4)?.try_into().ok()?));
    if head.get(0..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(0x3C)? as usize;
    if head.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let sections = u16_at(pe + 6)? as usize;
    let table = pe + 24 + u16_at(pe + 20)? as usize;
    let (ptr, size) = (0..sections).find_map(|i| {
        let s = table + i * 40;
        (head.get(s..s + 5)? == b".rsrc").then(|| Some((u32_at(s + 20)? as u64, u32_at(s + 16)? as u64))).flatten()
    })?;
    if size == 0 || size > MAX_RESOURCES {
        return None;
    }
    let res = get(ptr, ptr + size).await?;
    let dir = dest.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    let part = dest.with_extension("part");
    {
        use std::os::windows::fs::FileExt;
        let f = std::fs::File::create(&part).ok()?;
        f.set_len(ptr + res.len() as u64).ok()?;
        let head_len = (table + sections * 40).max(4096).min(head.len());
        f.seek_write(&head[..head_len], 0).ok()?;
        f.seek_write(&res, ptr).ok()?;
    }
    std::fs::rename(&part, dest).ok()
}

/// The preview a camera or phone stores near the start of a JPEG: in the EXIF block
/// (an APP1 segment), the second image directory points at a small JPEG of its own.
/// Fetch the start of the file, find that, and save it as a picture.
async fn early_photo(client: &reqwest::Client, url: &str, headers: &[(String, String)], dest: &Path) -> Option<()> {
    let r = request(client, url, headers, 0, 128 * 1024).await.ok()?;
    if r.status().as_u16() != 206 {
        return None;
    }
    let head = r.bytes().await.ok()?;
    let thumb = exif_thumbnail(&head)?;
    std::fs::create_dir_all(dest.parent()?).ok()?;
    let part = dest.with_extension("part");
    std::fs::write(&part, thumb).ok()?;
    std::fs::rename(&part, dest).ok()
}

/// The EXIF thumbnail inside the start of a JPEG, if it has one.
pub(crate) fn exif_thumbnail(b: &[u8]) -> Option<&[u8]> {
    if b.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    // walk the segments up to the image data, looking for APP1 "Exif"
    while i + 4 <= b.len() && b[i] == 0xFF {
        let marker = b[i + 1];
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if marker == 0xDA {
            break; // the picture itself begins
        }
        if marker == 0xE1 && b.get(i + 4..i + 10)? == b"Exif\0\0" {
            let tiff = b.get(i + 10..(i + 2 + len).min(b.len()))?;
            let be = match tiff.get(0..2)? {
                b"MM" => true,
                b"II" => false,
                _ => return None,
            };
            let u16_at = |o: usize| -> Option<usize> {
                let x = tiff.get(o..o + 2)?;
                Some(if be { u16::from_be_bytes([x[0], x[1]]) } else { u16::from_le_bytes([x[0], x[1]]) } as usize)
            };
            let u32_at = |o: usize| -> Option<usize> {
                let x = tiff.get(o..o + 4)?;
                Some(if be { u32::from_be_bytes([x[0], x[1], x[2], x[3]]) } else { u32::from_le_bytes([x[0], x[1], x[2], x[3]]) } as usize)
            };
            // IFD0, then the one after it (IFD1) describes the thumbnail
            let ifd0 = u32_at(4)?;
            let ifd1 = u32_at(ifd0 + 2 + u16_at(ifd0)? * 12)?;
            if ifd1 == 0 {
                return None;
            }
            let (mut off, mut size) = (None, None);
            for e in 0..u16_at(ifd1)? {
                let entry = ifd1 + 2 + e * 12;
                match u16_at(entry)? {
                    0x0201 => off = u32_at(entry + 8),
                    0x0202 => size = u32_at(entry + 8),
                    _ => {}
                }
            }
            let (off, size) = (off?, size?);
            let thumb = tiff.get(off..off + size)?;
            return (thumb.len() > 100 && thumb[0..2] == [0xFF, 0xD8]).then_some(thumb);
        }
        i += 2 + len;
    }
    None
}

fn resp_url(r: &reqwest::Response) -> Option<String> {
    Some(r.url().to_string())
}

async fn worker(ctx: Arc<Ctx>, first: Option<(usize, reqwest::Response)>) -> Result<(), Fail> {
    let (mut i, mut resp) = match first {
        Some((i, r)) => (i, Some(r)),
        None => match ctx.progress.claim(ctx.split) {
            Some(i) => (i, None),
            None => return Ok(()),
        },
    };
    loop {
        let mut tries = 0;
        loop {
            let seg = ctx.progress.seg(i);
            if seg.pos >= seg.end {
                break;
            }
            let attempt = async {
                let r = match resp.take() {
                    Some(r) => r,
                    None => {
                        let r = request(&ctx.client, &ctx.url, &ctx.headers, seg.pos, seg.end).await.map_err(|e| Fail::net(describe(&e)))?;
                        let ok = if seg.pos == 0 && seg.end == OPEN { r.status().is_success() } else { r.status().as_u16() == 206 };
                        if !ok {
                            let fatal = r.status().is_client_error() || r.status().is_success();
                            return Err(Fail { msg: format!("The server answered {}", status_text(r.status())), fatal });
                        }
                        r
                    }
                };
                pump(&ctx, i, r).await
            };
            ctx.progress.connections.fetch_add(1, Ordering::Relaxed);
            let result = attempt.await;
            ctx.progress.connections.fetch_sub(1, Ordering::Relaxed);
            match result {
                Ok(()) => tries = 0,
                Err(e) if e.fatal => return Err(e),
                Err(e) => {
                    tries += 1;
                    if tries > RETRIES {
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_secs(1 << tries.min(4))).await;
                }
            }
        }
        ctx.progress.release(i);
        match ctx.progress.claim(ctx.split) {
            Some(next) => i = next,
            None => return Ok(()),
        }
    }
}

/// Copy one response body into its range of the file, stopping at the range's end
/// (which another connection may pull in by taking over the back half).
async fn pump(ctx: &Ctx, i: usize, resp: reqwest::Response) -> Result<(), Fail> {
    use std::os::windows::fs::FileExt;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Fail::net(describe(&e)))?;
        ctx.limiter.acquire(chunk.len()).await;
        let (off, n, finished) = {
            let mut segs = ctx.progress.segs.lock().unwrap();
            let s = &mut segs[i];
            let n = (chunk.len() as u64).min(s.end.saturating_sub(s.pos)) as usize;
            let off = s.pos;
            s.pos += n as u64;
            (off, n, s.pos >= s.end)
        };
        let mut written = 0;
        while written < n {
            let w = ctx.file.seek_write(&chunk[written..n], off + written as u64).map_err(|e| Fail::fatal(format!("Can't write the file: {e}")))?;
            if w == 0 {
                return Err(Fail::fatal("Can't write the file: the disk is full"));
            }
            written += w;
        }
        ctx.progress.done.fetch_add(n as u64, Ordering::Relaxed);
        if finished {
            return Ok(());
        }
    }
    let mut segs = ctx.progress.segs.lock().unwrap();
    let s = &mut segs[i];
    if s.end == OPEN {
        s.end = s.pos; // the server didn't say how long it was: the end of the stream is the end
        return Ok(());
    }
    if s.pos < s.end { Err(Fail::net("The connection closed early")) } else { Ok(()) }
}

/// The first request, retried through brief outages and "too many requests".
async fn first_request(client: &reqwest::Client, url: &str, headers: &[(String, String)]) -> Result<reqwest::Response, String> {
    let mut tries = 0;
    loop {
        let err = match request(client, url, headers, 0, OPEN).await {
            Ok(r) if r.status().is_success() => return Ok(r),
            Ok(r) => {
                let s = r.status();
                let busy = s.as_u16() == 429 || s.is_server_error() || s.as_u16() == 408;
                if !busy || tries >= 4 {
                    return Err(format!("The server answered {}", status_text(s)));
                }
                // honor Retry-After (in seconds), within reason
                let wait = r.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
                (format!("The server answered {}", status_text(s)), wait)
            }
            Err(e) if tries < 4 && !e.is_builder() => (describe(&e), None),
            Err(e) => return Err(describe(&e)),
        };
        tries += 1;
        let wait = err.1.unwrap_or(2 << tries).min(30);
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}

async fn request(client: &reqwest::Client, url: &str, headers: &[(String, String)], pos: u64, end: u64) -> reqwest::Result<reqwest::Response> {
    let range = if end == OPEN { format!("bytes={pos}-") } else { format!("bytes={pos}-{}", end - 1) };
    let mut req = client.get(url);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    req.header(RANGE, range).send().await
}

fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "The server stopped responding".into()
    } else if e.is_connect() {
        "Can't connect to the server".into()
    } else if e.is_builder() {
        "That link isn't a valid web address".into()
    } else {
        let mut s = e.to_string();
        let mut src = std::error::Error::source(e);
        while let Some(inner) = src {
            s = inner.to_string();
            src = inner.source();
        }
        s
    }
}

fn status_text(s: reqwest::StatusCode) -> String {
    match s.canonical_reason() {
        Some(r) => format!("{} {r}", s.as_u16()),
        None => s.as_u16().to_string(),
    }
}

/// The size from `Content-Range: bytes a-b/size`.
fn content_range_total(r: &reqwest::Response) -> Option<u64> {
    r.headers().get(CONTENT_RANGE)?.to_str().ok()?.rsplit('/').next()?.trim().parse().ok()
}

fn file_name(resp: &reqwest::Response, original: &str) -> String {
    let from_header = resp.headers().get(CONTENT_DISPOSITION).and_then(|v| v.to_str().ok()).and_then(disposition_name);
    let mut name = from_header
        .or_else(|| name_from_url(resp.url().as_str()))
        .or_else(|| name_from_url(original))
        .map(|n| sanitize(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "download".into());
    if Path::new(&name).extension().is_none() {
        let ext = resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).and_then(ext_for_type);
        if let Some(ext) = ext {
            name = format!("{name}.{ext}");
        }
    }
    name
}

fn disposition_name(h: &str) -> Option<String> {
    let parts: Vec<&str> = h.split(';').map(str::trim).collect();
    for p in &parts {
        if let Some(v) = p.strip_prefix("filename*=") {
            let v = v.trim_matches('"');
            let v = v.splitn(3, '\'').nth(2).unwrap_or(v);
            return Some(percent_encoding::percent_decode_str(v).decode_utf8_lossy().into_owned());
        }
    }
    parts.iter().find_map(|p| p.strip_prefix("filename=")).map(|v| v.trim_matches('"').to_string())
}

pub fn name_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let after_host = path.splitn(4, '/').nth(3)?;
    let last = after_host.rsplit('/').find(|s| !s.is_empty())?;
    let name = percent_encoding::percent_decode_str(last).decode_utf8_lossy().into_owned();
    Some(sanitize(&name)).filter(|n| !n.is_empty())
}

fn sanitize(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']);
    trimmed.chars().take(180).collect()
}

fn ext_for_type(t: &str) -> Option<&'static str> {
    let t = t.split(';').next()?.trim().to_ascii_lowercase();
    Some(match t.as_str() {
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/x-msdownload" | "application/vnd.microsoft.portable-executable" => "exe",
        "application/x-msi" => "msi",
        "application/json" => "json",
        "text/html" => "html",
        "text/plain" => "txt",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "audio/mpeg" => "mp3",
        "application/x-bittorrent" => "torrent",
        _ => return None,
    })
}

fn set_hidden(path: &Path, hidden: bool) {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL, SetFileAttributesW};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = SetFileAttributesW(windows::core::PCWSTR(wide.as_ptr()), if hidden { FILE_ATTRIBUTE_HIDDEN } else { FILE_ATTRIBUTE_NORMAL });
    }
}
