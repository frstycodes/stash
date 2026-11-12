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
/// ... and this big on a server that has turned connections away.
const FRUGAL_SPLIT: u64 = 4 * 1024 * 1024;
const RETRIES: u32 = 6;
/// At most this many connections per download when it pays to add more (see `Task::go`).
const RAMP_MAX: usize = 16;
/// How often to try one more connection on a server that turned some away.
const PROBE_EVERY: Duration = Duration::from_secs(10);
const OPEN: u64 = u64::MAX; // a range whose end isn't known (no Content-Length)

#[derive(Clone, Copy, Debug)]
struct Seg {
    pos: u64,
    end: u64,
    busy: bool,
    /// how far the request now reading this range runs (0: none yet)
    asked: u64,
}

/// Shared between the connections of one download and the engine (which reads it for
/// progress and saves the remaining ranges when pausing).
pub struct Progress {
    pub done: AtomicU64,
    pub connections: AtomicUsize,
    segs: Mutex<Vec<Seg>>,
    /// how long a range request takes to answer (ms, smoothed), and what one
    /// connection brings in (bytes/s): together, what taking over part of a range costs
    ttfb_ms: AtomicU64,
    conn_bps: AtomicU64,
}

impl Progress {
    pub fn new(done: u64, remaining: &[(u64, u64)]) -> Self {
        let segs = remaining.iter().map(|&(pos, end)| Seg { pos, end, busy: false, asked: 0 }).collect();
        Self { done: AtomicU64::new(done), connections: AtomicUsize::new(0), segs: Mutex::new(segs), ttfb_ms: AtomicU64::new(0), conn_bps: AtomicU64::new(0) }
    }

    pub fn remaining(&self) -> Vec<(u64, u64)> {
        self.segs.lock().unwrap().iter().filter(|s| s.pos < s.end).map(|s| (s.pos, s.end)).collect()
    }

    fn reset(&self, total: Option<u64>) {
        *self.segs.lock().unwrap() = vec![Seg { pos: 0, end: total.unwrap_or(OPEN), busy: true, asked: OPEN }];
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
        segs[i].asked = OPEN; // the resume request runs to the end of the file
        Some(i)
    }

    fn asking(&self, i: usize, end: u64) {
        self.segs.lock().unwrap()[i].asked = end;
    }

    /// A range for a free connection: an unclaimed one, or the back half of the largest
    /// (when `split`, and each half would be at least `min`).
    fn claim(&self, split: bool, min: u64) -> Option<usize> {
        let mut segs = self.segs.lock().unwrap();
        if let Some(i) = segs.iter().position(|s| !s.busy && s.pos < s.end) {
            segs[i].busy = true;
            return Some(i);
        }
        if !split {
            return None;
        }
        let (i, rem) = segs.iter().enumerate().filter(|(_, s)| s.end != OPEN).map(|(i, s)| (i, s.end.saturating_sub(s.pos))).max_by_key(|(_, r)| *r)?;
        // A new request takes a while to answer, and meanwhile the connection that has the
        // range keeps going: it only pays to take over when there's more left than that
        // connection gets through in that time, and the split goes where both finish
        // together.
        let lead = self.conn_bps.load(Ordering::Relaxed) * self.ttfb_ms.load(Ordering::Relaxed) / 1000;
        if rem < min * 2 + lead {
            return None;
        }
        let mid = (segs[i].pos + (rem + lead) / 2) & !0xFFFF; // 64 KiB aligned
        let end = segs[i].end;
        segs[i].end = mid;
        segs.push(Seg { pos: mid, end, busy: true, asked: 0 });
        Some(segs.len() - 1)
    }

    /// At the end of range `i`: when the range right after it is free and the request
    /// reading `i` runs on through it, take it over (no new request).
    fn read_on(&self, i: usize) -> bool {
        let mut segs = self.segs.lock().unwrap();
        let s = segs[i];
        let Some(k) = segs.iter().position(|n| !n.busy && n.pos == s.end && n.pos < n.end && s.asked >= n.end) else { return false };
        segs[i].end = segs[k].end;
        segs[k].end = segs[k].pos;
        true
    }

    fn answered_in(&self, d: Duration) {
        let ms = d.as_millis() as u64;
        let old = self.ttfb_ms.load(Ordering::Relaxed);
        self.ttfb_ms.store(if old == 0 { ms } else { (old * 3 + ms) / 4 }, Ordering::Relaxed);
    }

    fn release(&self, i: usize) {
        let mut segs = self.segs.lock().unwrap();
        segs[i].busy = false;
        segs[i].asked = 0;
        // When the connection just before this range is still reading, and its request
        // already runs past this range, it simply reads on through: no new request (which
        // a server that limits requests may well turn away).
        let s = segs[i];
        if s.pos < s.end {
            if let Some(j) = segs.iter().position(|p| p.busy && p.end == s.pos && p.asked >= s.end) {
                segs[j].end = s.end;
                segs[i].end = s.pos;
            }
        }
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
    /// the server turned this connection away for now (429, 503)
    busy: bool,
}

impl Fail {
    fn net(e: impl std::fmt::Display) -> Self {
        Fail { msg: e.to_string(), fatal: false, busy: false }
    }
    fn fatal(e: impl std::fmt::Display) -> Self {
        Fail { msg: e.to_string(), fatal: true, busy: false }
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
    /// connections still running (one always stays)
    workers: AtomicUsize,
    /// the server turned a connection away: don't open more
    refused: std::sync::atomic::AtomicBool,
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
            if resp.status().as_u16() == 206 && total == self.total && ranges_add_up(&resp, start) {
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
                // Only trust ranges when the answer adds up: some CDNs give the size of the
                // compressed file in Content-Range and then send the whole uncompressed
                // one. Then fetch it in one piece, to the end of the stream.
                let ranges = status.as_u16() == 206 && ranges_add_up(&resp, 0);
                let total = match status.as_u16() {
                    206 if ranges => content_range_total(&resp),
                    206 => None,
                    _ => resp.content_length(),
                };
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
            workers: AtomicUsize::new(0),
            refused: Default::default(),
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
        let n = if ctx.split { self.connections.max(1).min(host_limit(&ctx.url)) } else { 1 };
        ctx.workers.store(n, Ordering::SeqCst);
        let mut set = JoinSet::new();
        // All at once: the server has just said it takes ranges. (Opening them a moment
        // apart cost every download up to a second at the end, waiting on the last one.)
        set.spawn(worker(ctx.clone(), Some(first)));
        for _ in 1..n {
            set.spawn(worker(ctx.clone(), None));
        }
        // Adaptive: while each added batch of connections still raises the speed by a
        // real margin, add another, up to twice the setting (at most `RAMP_MAX`). Stops at
        // the first batch that doesn't pay, or when the server turns one away. Never under
        // a speed limit, or when the setting asks for a single connection.
        let ramp_max = if ctx.split && n > 1 && !self.limiter.limited() { (n * 2).min(RAMP_MAX).max(n) } else { 0 };
        let mut spawned = n;
        let mut tick = tokio::time::interval(Duration::from_millis(1000));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        let (mut last_done, mut last_rate, mut ramping) = (self.progress.done.load(Ordering::Relaxed), 0f64, ramp_max > n);
        // after the server turned connections away: now and then, while plenty is left,
        // see whether it takes one more again
        let mut probe_at: Option<std::time::Instant> = None;
        loop {
            tokio::select! {
                r = set.join_next() => {
                    match r {
                        None => break,
                        // done once nothing is left, without waiting on a connection
                        // that's sleeping before a retry
                        Some(Ok(Ok(()))) if self.progress.remaining().is_empty() && self.progress.connections.load(Ordering::SeqCst) == 0 => {
                            set.shutdown().await;
                            break;
                        }
                        Some(Ok(Ok(()))) => {}
                        Some(Ok(Err(e))) => return Err(e.msg),
                        Some(Err(e)) if e.is_cancelled() => return Err("Stopped".into()),
                        Some(Err(e)) => return Err(e.to_string()),
                    }
                }
                _ = tick.tick(), if ctx.split => {
                    let done = self.progress.done.load(Ordering::Relaxed);
                    let rate = done.saturating_sub(last_done) as f64;
                    last_done = done;
                    let conns = self.progress.connections.load(Ordering::Relaxed).max(1) as f64;
                    self.progress.conn_bps.store((rate / conns) as u64, Ordering::Relaxed);
                    let left: u64 = self.progress.remaining().iter().map(|r| r.1.saturating_sub(r.0)).sum();
                    if ctx.refused.load(Ordering::Relaxed) && n > 1 {
                        let at = *probe_at.get_or_insert_with(|| std::time::Instant::now() + PROBE_EVERY);
                        // worth it with at least 10 s of work left at this speed
                        if std::time::Instant::now() >= at && left > rate.max(1.0) as u64 * 10 && ctx.workers.load(Ordering::SeqCst) < n {
                            probe_at = Some(std::time::Instant::now() + PROBE_EVERY);
                            ctx.workers.fetch_add(1, Ordering::SeqCst);
                            set.spawn(worker(ctx.clone(), None));
                        }
                    }
                    if !ramping {
                        continue;
                    }
                    // the first second sets the bar (and opens the first batch); then each
                    // batch must beat the speed before it by 15%
                    let pays = last_rate == 0.0 || rate > last_rate * 1.15;
                    // enough left that new connections would get a share worth opening
                    let worth = left > rate.max(1.0) as u64 * 2 && left / MIN_SPLIT > spawned as u64;
                    if !pays || !worth || ctx.refused.load(Ordering::Relaxed) || spawned >= ramp_max {
                        ramping = false;
                        continue;
                    }
                    last_rate = rate;
                    let add = (ramp_max - spawned).min(4);
                    spawned += add;
                    ctx.workers.fetch_add(add, Ordering::SeqCst);
                    for _ in 0..add {
                        set.spawn(worker(ctx.clone(), None));
                    }
                }
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

/// One connection: fetch a range, then the next free one (or half of the biggest left),
/// until nothing is left. A failed range is handed back so the others can carry on with
/// it while this one waits to retry; a server that refuses more connections ("too many
/// requests") just has this one leave, as long as another is still at work.
async fn worker(ctx: Arc<Ctx>, mut first: Option<(usize, reqwest::Response)>) -> Result<(), Fail> {
    let mut tries = 0;
    let mut served = false;
    loop {
        let (i, resp) = match first.take() {
            Some((i, r)) => (i, Some(r)),
            // once the server has turned a connection away, only split big pieces: each
            // split is another request it may turn away
            None => match ctx.progress.claim(ctx.split, if ctx.refused.load(Ordering::Relaxed) { FRUGAL_SPLIT } else { MIN_SPLIT }) {
                Some(i) => (i, None),
                None => break,
            },
        };
        let seg = ctx.progress.seg(i);
        let result = if seg.pos < seg.end {
            ctx.progress.connections.fetch_add(1, Ordering::Relaxed);
            let r = attempt(&ctx, i, seg, resp).await;
            ctx.progress.connections.fetch_sub(1, Ordering::Relaxed);
            r
        } else {
            Ok(())
        };
        ctx.progress.release(i);
        match result {
            Ok(()) => {
                tries = 0;
                if !served && ctx.refused.load(Ordering::Relaxed) {
                    // the server takes this many after all
                    raise_limit(&ctx.url, ctx.workers.load(Ordering::SeqCst));
                }
                served = true;
            }
            Err(e) if e.fatal => return Err(e),
            Err(e) => {
                // turned away before it ever got going: the server allows fewer
                // connections than this, so leave it to the others
                if e.busy && !served && ctx.workers.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n > 1).then(|| n - 1)).is_ok() {
                    ctx.refused.store(true, Ordering::Relaxed);
                    remember_limit(&ctx.url, ctx.workers.load(Ordering::SeqCst));
                    return Ok(());
                }
                // cut off mid-way after bringing data: reconnect right away
                let moved = ctx.progress.seg(i).pos > seg.pos;
                if moved && !e.busy {
                    tries = 0;
                    continue;
                }
                tries += 1;
                if tries > RETRIES {
                    // the download only fails with its last connection
                    if ctx.workers.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n > 1).then(|| n - 1)).is_ok() {
                        return Ok(());
                    }
                    return Err(e);
                }
                // a connection that was working and got turned away just once (others
                // finishing at the same moment) tries again soon
                let wait = if e.busy && served { Duration::from_millis(500 * tries as u64) } else { Duration::from_millis(250 << tries.min(6)) };
                tokio::time::sleep(wait).await;
            }
        }
    }
    ctx.workers.fetch_sub(1, Ordering::SeqCst);
    Ok(())
}

async fn attempt(ctx: &Ctx, i: usize, seg: Seg, resp: Option<reqwest::Response>) -> Result<(), Fail> {
    let r = match resp {
        Some(r) => r,
        None => {
            let asked = std::time::Instant::now();
            // a server that limits requests: ask for everything from here on, so this
            // connection can read on into ranges nobody else is getting
            let to = if ctx.refused.load(Ordering::Relaxed) { OPEN } else { seg.end };
            ctx.progress.asking(i, to);
            let r = request(&ctx.client, &ctx.url, &ctx.headers, seg.pos, to).await.map_err(|e| Fail::net(describe(&e)))?;
            ctx.progress.answered_in(asked.elapsed());
            let ok = if seg.pos == 0 && seg.end == OPEN { r.status().is_success() } else { r.status().as_u16() == 206 };
            if !ok {
                let busy = matches!(r.status().as_u16(), 429 | 503);
                let fatal = !busy && (r.status().is_client_error() || r.status().is_success());
                return Err(Fail { msg: format!("The server answered {}", status_text(r.status())), fatal, busy });
            }
            r
        }
    };
    pump(ctx, i, r).await
}

/// Copy one response body into its range of the file, stopping at the range's end
/// (which another connection may pull in by taking over the back half), or reading on
/// into the next range when nobody has it and this request covers it.
async fn pump(ctx: &Ctx, i: usize, resp: reqwest::Response) -> Result<(), Fail> {
    use std::os::windows::fs::FileExt;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Fail::net(describe(&e)))?;
        ctx.limiter.acquire(chunk.len()).await;
        let mut from = 0;
        while from < chunk.len() {
            let (off, n, finished) = {
                let mut segs = ctx.progress.segs.lock().unwrap();
                let s = &mut segs[i];
                let n = ((chunk.len() - from) as u64).min(s.end.saturating_sub(s.pos)) as usize;
                let off = s.pos;
                s.pos += n as u64;
                (off, n, s.pos >= s.end)
            };
            let mut written = 0;
            while written < n {
                let w = ctx.file.seek_write(&chunk[from + written..from + n], off + written as u64).map_err(|e| Fail::fatal(format!("Can't write the file: {e}")))?;
                if w == 0 {
                    return Err(Fail::fatal("Can't write the file: the disk is full"));
                }
                written += w;
            }
            ctx.progress.done.fetch_add(n as u64, Ordering::Relaxed);
            from += n;
            if finished && !ctx.progress.read_on(i) {
                return Ok(());
            }
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

/// Hosts that turned connections away ("too many requests"), and how many they took:
/// later downloads from them start with that many instead of asking again (for half an
/// hour; then they get asked again).
type Limits = std::collections::HashMap<String, (usize, std::time::Instant)>;
static HOST_LIMITS: Mutex<Option<Limits>> = Mutex::new(None);
const LIMIT_FOR: Duration = Duration::from_secs(30 * 60);

fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase()
}

fn remember_limit(url: &str, n: usize) {
    let mut m = HOST_LIMITS.lock().unwrap();
    let now = std::time::Instant::now();
    let e = m.get_or_insert_with(Default::default).entry(host_of(url)).or_insert((n.max(1), now));
    if e.1.elapsed() > LIMIT_FOR {
        *e = (n.max(1), now);
    }
    e.0 = e.0.min(n.max(1));
}

fn raise_limit(url: &str, n: usize) {
    if let Some(e) = HOST_LIMITS.lock().unwrap().as_mut().and_then(|m| m.get_mut(&host_of(url))) {
        e.0 = e.0.max(n);
    }
}

fn host_limit(url: &str) -> usize {
    let m = HOST_LIMITS.lock().unwrap();
    m.as_ref().and_then(|m| m.get(&host_of(url))).filter(|(_, at)| at.elapsed() < LIMIT_FOR).map(|(n, _)| *n).unwrap_or(usize::MAX)
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

/// A range answer (to a request from `start` to the end) that says how long it is, and
/// whose length matches its Content-Range.
fn ranges_add_up(r: &reqwest::Response, start: u64) -> bool {
    match (content_range_total(r), r.content_length()) {
        (Some(total), Some(len)) => total.checked_sub(start) == Some(len),
        _ => false,
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

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1 << 20;

    #[test]
    fn handed_back_range_is_read_on_through() {
        let p = Progress::new(0, &[]);
        p.reset(Some(10 * MB)); // the first request asks for the whole file
        let i = p.claim(true, MIN_SPLIT).unwrap();
        assert_eq!(p.seg(0).end, p.seg(i).pos);
        // the new connection is turned away and hands its range back: the first one,
        // whose request runs to the end anyway, just reads on
        p.release(i);
        assert_eq!(p.seg(0).end, 10 * MB);
        assert_eq!(p.remaining(), vec![(0, 10 * MB)]);
    }

    #[test]
    fn handed_back_range_stays_when_no_request_covers_it() {
        let p = Progress::new(0, &[]);
        p.reset(Some(10 * MB));
        let i = p.claim(true, MIN_SPLIT).unwrap();
        let mid = p.seg(i).pos;
        p.asking(0, mid); // as if the first connection had asked for just its half
        p.release(i);
        assert_eq!(p.seg(0).end, mid);
        assert_eq!(p.remaining(), vec![(0, mid), (mid, 10 * MB)]);
        assert_eq!(p.claim(true, MIN_SPLIT), Some(i), "a free range goes before any split");
    }

    #[test]
    fn split_weighs_the_wait_for_a_new_request() {
        let p = Progress::new(0, &[]);
        p.reset(Some(10 * MB));
        // each connection does 1 MB/s and a request takes 1 s to answer: the connection
        // that has the range gets 1 MB further meanwhile, so the split moves up by half that
        p.conn_bps.store(MB, Ordering::Relaxed);
        p.answered_in(Duration::from_millis(1000));
        let i = p.claim(true, MIN_SPLIT).unwrap();
        assert_eq!(p.seg(i).pos, (11 * MB / 2) & !0xFFFF);
        // at 2 MB/s, 2.5 MB left can't pay: it needs 2 x 512 KiB + 2 MB of lead
        let q = Progress::new(0, &[(0, 2 * MB + MB / 2)]);
        q.claim(false, MIN_SPLIT).unwrap();
        q.conn_bps.store(2 * MB, Ordering::Relaxed);
        q.answered_in(Duration::from_millis(1000));
        assert_eq!(q.claim(true, MIN_SPLIT), None);
    }

    #[test]
    fn host_limits() {
        assert_eq!(host_of("https://Example.com:8443/a/b?c"), "example.com:8443");
        assert_eq!(host_limit("https://limits.test/x"), usize::MAX);
        remember_limit("https://limits.test/a", 3);
        remember_limit("https://limits.test/b", 2);
        remember_limit("https://limits.test/c", 0);
        assert_eq!(host_limit("https://limits.test/z"), 1);
        assert_eq!(host_limit("https://other.test/z"), usize::MAX);
    }
}

#[cfg(test)]
mod read_on_tests {
    use super::*;

    #[test]
    fn reads_on_into_a_free_range_its_request_covers() {
        let p = Progress::new(0, &[(0, 100), (100, 200), (200, 300)]);
        assert_eq!(p.claim(false, MIN_SPLIT), Some(0));
        assert_eq!(p.claim(false, MIN_SPLIT), Some(1));
        p.asking(0, OPEN);
        // range 2 is taken: nothing to read on into once range 0 is done
        assert!(!p.read_on(1));
        p.release(1); // range 1 handed back; range 0's request covers it
        assert_eq!(p.seg(0).end, 200, "merged into range 0 on release");
        // the end of range 0 is now 200; range 2 (200..300) is free and covered too
        assert!(p.read_on(0));
        assert_eq!(p.seg(0).end, 300);
        assert_eq!(p.remaining(), vec![(0, 300)]);
    }
}
