//! Videos from web pages (YouTube and the many sites yt-dlp knows): reading a page's
//! video (title, picture, length, sizes) for the New Download sheet, and the download
//! itself, run by yt-dlp with its progress read line by line.
//!
//! A video download is an ordinary job (`Kind::Video`): it queues, pauses (yt-dlp is
//! stopped and later picks up its partial files with `--continue`), resumes after a
//! restart, and is cancelled with its partial files cleaned up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::mpsc::UnboundedSender;

use super::Ev;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Quality {
    #[default]
    Best,
    P1080,
    P720,
    P480,
    AudioM4a,
    AudioMp3,
}

impl Quality {
    pub const ALL: [Quality; 6] = [Quality::Best, Quality::P1080, Quality::P720, Quality::P480, Quality::AudioM4a, Quality::AudioMp3];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Best => "Best",
            Quality::P1080 => "1080p",
            Quality::P720 => "720p",
            Quality::P480 => "480p",
            Quality::AudioM4a => "Audio (m4a)",
            Quality::AudioMp3 => "Audio (mp3)",
        }
    }

    pub fn audio(self) -> bool {
        matches!(self, Quality::AudioM4a | Quality::AudioMp3)
    }

    fn height(self) -> Option<u32> {
        match self {
            Quality::P1080 => Some(1080),
            Quality::P720 => Some(720),
            Quality::P480 => Some(480),
            _ => None,
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Quality::AudioM4a => "m4a",
            Quality::AudioMp3 => "mp3",
            _ => "mp4",
        }
    }

    /// yt-dlp's options for this choice: picture and sound joined into an mp4 that plays
    /// everywhere (H.264 preferred at the same size), or just the sound.
    fn args(self) -> Vec<String> {
        let mut a: Vec<String> = Vec::new();
        match self {
            Quality::AudioM4a => a.extend(["-f", "ba[ext=m4a]/ba", "-x", "--audio-format", "m4a"].map(String::from)),
            Quality::AudioMp3 => a.extend(["-f", "ba", "-x", "--audio-format", "mp3", "--audio-quality", "0"].map(String::from)),
            q => {
                let f = match q.height() {
                    Some(h) => format!("bv*[height<={h}]+ba/b[height<={h}]/bv*+ba/b"),
                    None => "bv*+ba/b".into(),
                };
                a.extend(["-f".into(), f]);
                a.extend(["-S", "res,vcodec:h264,ext:mp4:m4a", "--merge-output-format", "mp4", "--remux-video", "mp4"].map(String::from));
            }
        }
        a
    }
}

/// What a video download needs to know, kept with the job.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spec {
    pub quality: Quality,
    pub title: String,
    /// The size the page's formats suggest, until the download knows better.
    pub expected: Option<u64>,
    /// The page's picture, shown on the row while it downloads.
    pub thumbnail: Option<String>,
    /// Only this part of the video: start and end, in seconds.
    pub clip: Option<(f64, f64)>,
}

impl Spec {
    /// The file name it's saved under (yt-dlp is told this exact name). A clip says
    /// which part it is: "Title (1m05s-2m30s).mp4".
    pub fn file_name(&self) -> String {
        let stem = super::safe_name(&self.title).unwrap_or_else(|| "Video".into());
        // leave room for yt-dlp's ".f137.mp4.part" and long folder paths
        let stem: String = stem.chars().take(120).collect();
        let stem = stem.trim_end_matches(['.', ' ']);
        match self.clip {
            Some((a, b)) => format!("{stem} ({}-{}).{}", stamp(a), stamp(b), self.quality.ext()),
            None => format!("{stem}.{}", self.quality.ext()),
        }
    }

    /// yt-dlp's options for downloading only the clip: that section, cut exactly at its
    /// ends (ffmpeg re-encodes around the cuts, so it starts on the right frame).
    fn clip_args(&self) -> Vec<String> {
        match self.clip {
            Some((a, b)) => vec!["--download-sections".into(), format!("*{a:.1}-{b:.1}"), "--force-keyframes-at-cuts".into()],
            None => Vec::new(),
        }
    }
}

/// "1m05s", "1h02m03s": a time that can go in a file name (no colons on Windows).
fn stamp(secs: f64) -> String {
    let s = secs.round() as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}h{m:02}m{s:02}s") } else { format!("{m}m{s:02}s") }
}

// ---------- reading a page ----------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub title: String,
    /// The page itself (yt-dlp's tidy form of the link).
    pub url: String,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub uploader: Option<String>,
    pub max_height: Option<u32>,
    pub is_live: bool,
    /// The expected size of each choice (None: unknown).
    pub sizes: Vec<(Quality, Option<u64>)>,
    /// The playlist the link is (or is part of).
    pub playlist: Option<Playlist>,
    /// The link is the playlist itself, not one video in it.
    pub only_playlist: bool,
    /// Little frames from across the video (YouTube's storyboards), for previews.
    pub storyboard: Option<Storyboard>,
}

/// A video's storyboard: sprite sheets, each a `cols` × `rows` grid of `width` × `height`
/// frames, one frame every 1/`fps` seconds, sheet after sheet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Storyboard {
    pub width: u32,
    pub height: u32,
    pub cols: u32,
    pub rows: u32,
    pub fps: f64,
    pub sheets: Vec<String>,
}

impl Storyboard {
    /// Which sheet, and which cell in it (column, row), shows the moment `t`.
    pub fn cell(&self, t: f64) -> Option<(usize, u32, u32)> {
        let per = (self.cols * self.rows) as usize;
        if per == 0 || self.fps <= 0.0 || self.sheets.is_empty() {
            return None;
        }
        let i = ((t.max(0.0) * self.fps).floor() as usize).min(self.sheets.len() * per - 1);
        let cell = (i % per) as u32;
        Some((i / per, cell % self.cols, cell / self.cols))
    }
}

/// The sharpest storyboard that isn't too many sheets to fetch (long videos have lots).
fn pick_storyboard(formats: &[&Value]) -> Option<Storyboard> {
    let mut boards: Vec<Storyboard> = formats
        .iter()
        .filter(|f| str_of(f, "format_id").is_some_and(|id| id.starts_with("sb")))
        .filter_map(|f| {
            let sheets: Vec<String> = f.get("fragments")?.as_array()?.iter().filter_map(|x| str_of(x, "url")).collect();
            let b = Storyboard {
                width: num(f, "width")? as u32,
                height: num(f, "height")? as u32,
                cols: num(f, "columns")? as u32,
                rows: num(f, "rows")? as u32,
                fps: num(f, "fps")?,
                sheets,
            };
            (b.width > 0 && b.cols > 0 && b.rows > 0 && b.fps > 0.0 && !b.sheets.is_empty()).then_some(b)
        })
        .collect();
    boards.sort_by_key(|b| std::cmp::Reverse(b.width));
    let fits = boards.iter().position(|b| b.sheets.len() <= 30);
    match fits {
        Some(i) => Some(boards.swap_remove(i)),
        None => boards.pop(),
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playlist {
    pub title: String,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub url: String,
    pub title: String,
}

impl Info {
    pub fn size(&self, q: Quality) -> Option<u64> {
        self.sizes.iter().find(|(x, _)| *x == q).and_then(|(_, s)| *s)
    }

    /// A choice that would get nothing more than the one above it (no 1080p of a 720p video).
    pub fn offers(&self, q: Quality) -> bool {
        match (q.height(), self.max_height) {
            (Some(h), Some(max)) => h <= max || q == Quality::P480,
            (Some(_), None) => false,
            _ => true,
        }
    }

    /// What the sheet picks first: 1080p when there's more than that, else the best.
    pub fn default_quality(&self) -> Quality {
        if self.max_height.is_some_and(|h| h > 1080) { Quality::P1080 } else { Quality::Best }
    }
}

/// "3:07", "1:02:03"
pub fn duration_text(secs: f64) -> String {
    let s = secs.round() as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty())
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0)
}

/// yt-dlp's `-J` output to what the sheet shows.
pub fn parse_info(json: &str) -> Result<Info, String> {
    let v: Value = serde_json::from_str(json).map_err(|_| "The video page couldn't be read.".to_string())?;
    let kind = v.get("_type").and_then(Value::as_str).unwrap_or("video");
    let title = str_of(&v, "title").unwrap_or_else(|| "Video".into());
    let url = str_of(&v, "webpage_url").or_else(|| str_of(&v, "original_url")).unwrap_or_default();
    if kind == "playlist" || kind == "multi_video" {
        let playlist = parse_playlist(&v);
        return Ok(Info {
            title: title.clone(),
            url,
            thumbnail: pick_thumbnail(&v),
            uploader: str_of(&v, "uploader").or_else(|| str_of(&v, "channel")),
            playlist: Some(playlist),
            only_playlist: true,
            ..Default::default()
        });
    }
    let duration = num(&v, "duration");
    let formats: Vec<&Value> = v.get("formats").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    let max_height = formats.iter().filter(|f| is_video(f)).filter_map(|f| num(f, "height")).map(|h| h as u32).max();
    let sizes = Quality::ALL.iter().map(|q| (*q, estimate(&formats, *q, duration))).collect();
    Ok(Info {
        title,
        url,
        duration,
        thumbnail: pick_thumbnail(&v),
        uploader: str_of(&v, "uploader").or_else(|| str_of(&v, "channel")),
        max_height,
        is_live: v.get("is_live").and_then(Value::as_bool).unwrap_or(false),
        sizes,
        playlist: None,
        only_playlist: false,
        storyboard: pick_storyboard(&formats),
    })
}

fn parse_playlist(v: &Value) -> Playlist {
    let entries = v
        .get("entries")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let url = str_of(e, "url").or_else(|| str_of(e, "webpage_url"))?;
                    url.starts_with("http").then(|| Entry { url, title: str_of(e, "title").unwrap_or_else(|| "Video".into()) })
                })
                .collect()
        })
        .unwrap_or_default();
    Playlist { title: str_of(v, "title").unwrap_or_else(|| "Playlist".into()), entries }
}

fn codec(f: &Value, k: &str) -> String {
    f.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn is_video(f: &Value) -> bool {
    let vc = codec(f, "vcodec");
    !vc.is_empty() && vc != "none" && codec(f, "ext") != "mhtml"
}

fn is_audio_only(f: &Value) -> bool {
    let ac = codec(f, "acodec");
    !ac.is_empty() && ac != "none" && codec(f, "vcodec") == "none"
}

fn size_of(f: &Value, duration: Option<f64>) -> Option<u64> {
    num(f, "filesize")
        .or_else(|| num(f, "filesize_approx"))
        .or_else(|| Some(num(f, "tbr")? * 1000.0 / 8.0 * duration?))
        .map(|s| s as u64)
}

/// About what yt-dlp would pick for `q` (`-S res,vcodec:h264,ext:mp4:m4a`), and its size.
fn estimate(formats: &[&Value], q: Quality, duration: Option<f64>) -> Option<u64> {
    let audios: Vec<&&Value> = formats.iter().filter(|f| is_audio_only(f)).collect();
    // the best sound: m4a first, then the higher bitrate
    let best_audio = audios.iter().max_by(|a, b| {
        let key = |f: &Value| (codec(f, "ext") == "m4a", num(f, "tbr").unwrap_or(0.0));
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
    });
    match q {
        Quality::AudioM4a => best_audio.and_then(|a| size_of(a, duration)),
        // mp3 at the best VBR setting: about 245 kbps
        Quality::AudioMp3 => duration.map(|d| (d * 245_000.0 / 8.0) as u64),
        _ => {
            let cap = q.height().unwrap_or(u32::MAX) as f64;
            let video = formats.iter().filter(|f| is_video(f) && num(f, "height").is_some_and(|h| h <= cap)).max_by(|a, b| {
                let key = |f: &Value| {
                    let vc = codec(f, "vcodec");
                    (
                        num(f, "height").unwrap_or(0.0) as u32,
                        vc.starts_with("avc") || vc.starts_with("h264"),
                        codec(f, "ext") == "mp4",
                        codec(f, "protocol").starts_with("http"),
                        num(f, "tbr").unwrap_or(0.0),
                    )
                };
                let (ka, kb) = (key(a), key(b));
                (ka.0, ka.1, ka.2, ka.3).cmp(&(kb.0, kb.1, kb.2, kb.3)).then(ka.4.total_cmp(&kb.4))
            })?;
            let v = size_of(video, duration)?;
            if codec(video, "acodec") != "none" && !codec(video, "acodec").is_empty() {
                return Some(v);
            }
            Some(v + best_audio.and_then(|a| size_of(a, duration)).unwrap_or(0))
        }
    }
}

/// The page's picture: a JPEG (which every Windows can read) about the size shown.
fn pick_thumbnail(v: &Value) -> Option<String> {
    let thumbs = v.get("thumbnails").and_then(Value::as_array);
    let jpg = thumbs.and_then(|t| {
        t.iter()
            .filter_map(|x| {
                let url = str_of(x, "url")?;
                let path = url.split('?').next().unwrap_or("").to_ascii_lowercase();
                let w = num(x, "width").unwrap_or(0.0);
                (path.ends_with(".jpg") && w <= 1280.0).then_some((w, url))
            })
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, u)| u)
    });
    jpg.or_else(|| str_of(v, "thumbnail"))
        .or_else(|| thumbs.and_then(|t| t.iter().rev().find_map(|x| str_of(x, "url"))))
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
}

/// Worth asking yt-dlp about: a web page rather than a file. Before the video tools are
/// here, only well-known video sites (so pasting any link doesn't fetch them).
pub fn worth_probing(link: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(link.trim()) else { return false };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    let host = u.host_str().unwrap_or("").trim_start_matches("www.").trim_start_matches("m.").to_ascii_lowercase();
    let last = u.path_segments().and_then(|mut s| s.next_back().map(str::to_string)).unwrap_or_default();
    let ext = last.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    // a file's own link downloads as a file
    let page_like = ext.as_deref().is_none_or(|e| matches!(e, "html" | "htm" | "php" | "asp" | "aspx" | "jsp") || e.len() > 5 || e.chars().any(|c| c.is_ascii_digit()) && e.len() > 4);
    if !page_like {
        return false;
    }
    if crate::tools::has_ytdlp() {
        return true;
    }
    const SITES: &[&str] = &[
        "youtube.com", "youtu.be", "music.youtube.com", "vimeo.com", "dailymotion.com", "twitch.tv", "tiktok.com", "x.com", "twitter.com", "instagram.com",
        "facebook.com", "fb.watch", "reddit.com", "v.redd.it", "soundcloud.com", "bandcamp.com", "bilibili.com", "nicovideo.jp", "streamable.com",
        "rumble.com", "archive.org", "ted.com", "bitchute.com", "odysee.com", "mixcloud.com", "loom.com", "kick.com",
    ];
    SITES.iter().any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

/// Read a page (off the UI thread): its video, and the playlist it's in, if any.
pub async fn probe(link: &str) -> Result<Info, String> {
    let exe = crate::tools::ytdlp().await?;
    let link = link.trim().to_string();
    // a video in a playlist ("watch?v=…&list=…"): read the whole list too, at the same time
    let in_list = reqwest::Url::parse(&link).is_ok_and(|u| u.query_pairs().any(|(k, _)| k == "list"));
    let video = run_json(&exe, &["-J", "--no-playlist", "--flat-playlist"], &link);
    let list = async {
        if in_list { run_json(&exe, &["-J", "--yes-playlist", "--flat-playlist"], &link).await.ok() } else { None }
    };
    let (json, list) = tokio::join!(video, list);
    let mut info = parse_info(&json?)?;
    if info.url.is_empty() {
        info.url = link.clone();
    }
    if info.playlist.is_none() {
        if let Some(Ok(Info { playlist: Some(p), .. })) = list.as_deref().map(parse_info) {
            if p.entries.len() > 1 {
                info.playlist = Some(p);
            }
        }
    }
    Ok(info)
}

async fn run_json(exe: &Path, args: &[&str], link: &str) -> Result<String, String> {
    let mut c = crate::tools::command(exe);
    c.args(["--ignore-config", "--no-warnings", "--ies", "default,-generic"]).args(args).args(proxy_args()).arg("--").arg(link).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(60), c.output()).await.map_err(|_| "The video page took too long to answer.".to_string())?.map_err(|e| format!("Couldn't start the video tools: {e}"))?;
    if !out.status.success() {
        return Err(plain_error(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The proxy to send yt-dlp through. Stash has no proxy setting of its own (yet): yt-dlp
/// then uses Windows' (the same one Stash's own downloads use).
fn proxy_args() -> Vec<String> {
    match std::env::var("STASH_PROXY").ok().filter(|p| !p.trim().is_empty()) {
        Some(p) => vec!["--proxy".into(), p],
        None => Vec::new(),
    }
}

/// yt-dlp's complaint, in plain words.
pub fn plain_error(stderr: &str) -> String {
    let line = stderr.lines().rev().find(|l| l.starts_with("ERROR:")).or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("").trim();
    let msg = line.trim_start_matches("ERROR:").trim();
    // "[youtube] abc123: Video unavailable" -> "Video unavailable"
    let msg = match msg.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((_, rest)) => rest.split_once(": ").map(|(id, m)| if id.contains(' ') { rest } else { m }).unwrap_or(rest),
        None => msg,
    };
    let lower = msg.to_ascii_lowercase();
    let friendly = if lower.contains("unsupported url") {
        "There's no video Stash can get on this page."
    } else if lower.contains("private video") {
        "This video is private."
    } else if lower.contains("confirm your age") || lower.contains("age-restricted") || lower.contains("inappropriate for some users") {
        "This video is age-restricted, so it needs a sign-in Stash doesn't have."
    } else if lower.contains("not a bot") {
        "The site wants to check you're not a bot. Try again later."
    } else if lower.contains("members-only") || lower.contains("join this channel") || lower.contains("premium") {
        "This video is only for the channel's members or subscribers."
    } else if lower.contains("video unavailable") || lower.contains("has been removed") || lower.contains("does not exist") || lower.contains("404") {
        "This video isn't available any more."
    } else if lower.contains("not available in your country") || lower.contains("geo") && lower.contains("restrict") {
        "This video isn't available in your country."
    } else if lower.contains("live event will begin") || lower.contains("premieres in") {
        "This video hasn't started yet."
    } else if lower.contains("requested format is not available") {
        "That quality isn't available for this video. Try another."
    } else if lower.contains("ffmpeg") && (lower.contains("not found") || lower.contains("not installed")) {
        "The video tools are missing a part (ffmpeg). Open Settings to get them again."
    } else if lower.contains("unable to download") || lower.contains("timed out") || lower.contains("getaddrinfo") || lower.contains("connection") {
        "Couldn't reach the video's site. Check your connection."
    } else if lower.contains("http error 403") {
        "The site refused the download (403). Try again later."
    } else if lower.contains("no space left") {
        "There's no room left on the drive."
    } else {
        ""
    };
    if !friendly.is_empty() {
        return friendly.into();
    }
    let msg: String = msg.chars().take(200).collect();
    if msg.is_empty() { "The video couldn't be downloaded.".into() } else { msg }
}

// ---------- downloading ----------

const PHASES: [Option<&str>; 6] = [None, Some("Starting…"), Some("Getting video tools…"), Some("Joining video and audio…"), Some("Converting…"), Some("Finishing…")];

/// Live progress of a running video download, read by the engine every tick.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub speed: AtomicU64,
    phase: AtomicU8,
}

impl Progress {
    pub fn phase(&self) -> Option<&'static str> {
        PHASES.get(self.phase.load(Ordering::Relaxed) as usize).copied().flatten()
    }
    fn set_phase(&self, i: u8) {
        self.phase.store(i, Ordering::Relaxed);
    }
}

static LIVE: Mutex<Option<HashMap<u64, Arc<Progress>>>> = Mutex::new(None);

/// A running video download's progress.
pub fn live(id: u64) -> Option<Arc<Progress>> {
    LIVE.lock().unwrap().as_ref()?.get(&id).cloned()
}

/// Drops out of `LIVE` when the task ends (or is stopped).
struct Registered(u64);
impl Drop for Registered {
    fn drop(&mut self) {
        if let Some(m) = LIVE.lock().unwrap().as_mut() {
            m.remove(&self.0);
        }
    }
}

/// One line of `--progress-template` output (see `TEMPLATE`).
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub finished: bool,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub speed: Option<u64>,
}

const TAG: &str = "[stash]";
const TEMPLATE: &str = "download:[stash] %(progress.status)s %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s";

/// "[stash] downloading 1048576 52428800 NA 2097152.5" (missing values are "NA").
pub fn parse_line(line: &str) -> Option<Line> {
    let rest = line.trim().strip_prefix(TAG)?;
    let mut f = rest.split_whitespace();
    let status = f.next()?;
    let n = |s: Option<&str>| s.and_then(|s| s.parse::<f64>().ok()).filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as u64);
    let downloaded = n(f.next());
    let total = n(f.next());
    let estimate = n(f.next());
    let speed = n(f.next());
    Some(Line { finished: status == "finished", downloaded: downloaded.unwrap_or(0), total: total.or(estimate), speed })
}

/// What the other lines say yt-dlp is doing now (an index into `PHASES`).
fn phase_of(line: &str) -> Option<u8> {
    let l = line.trim_start();
    if l.starts_with("[Merger]") {
        Some(3)
    } else if l.starts_with("[ExtractAudio]") || l.starts_with("[VideoRemuxer]") || l.starts_with("[VideoConvertor]") {
        Some(4)
    } else if l.starts_with("[EmbedThumbnail]") || l.starts_with("[Metadata]") || l.starts_with("[FixupM") {
        Some(5)
    } else if l.starts_with("[download] Destination") {
        Some(0)
    } else {
        None
    }
}

/// Adds up a download made of several files (picture, then sound) into one progress.
#[derive(Default, Debug)]
pub struct Tally {
    base: u64,
    cur_done: u64,
    cur_total: u64,
    expected: u64,
}

impl Tally {
    pub fn new(expected: Option<u64>) -> Self {
        Tally { expected: expected.unwrap_or(0), ..Default::default() }
    }

    /// (done, total) after this line.
    pub fn add(&mut self, l: &Line) -> (u64, u64) {
        if l.finished {
            self.base += l.total.unwrap_or(l.downloaded).max(l.downloaded);
            self.cur_done = 0;
            self.cur_total = 0;
        } else {
            self.cur_done = l.downloaded;
            self.cur_total = l.total.unwrap_or(0).max(l.downloaded);
        }
        let done = self.base + self.cur_done;
        let total = self.expected.max(self.base + self.cur_total).max(done);
        (done, total)
    }
}

pub(crate) struct Task {
    pub id: u64,
    pub url: String,
    pub dir: PathBuf,
    /// The file name chosen at the start ("Title.mp4"); its stem names every part file.
    pub name: String,
    /// The claim on the name, from an earlier start (resuming).
    pub claim: Option<PathBuf>,
    pub spec: Spec,
    pub limit_bps: u64,
    pub connections: usize,
    pub events: UnboundedSender<Ev>,
}

/// Stops yt-dlp and everything it started (ffmpeg) when dropped: a pause or a cancel.
struct KillTree(windows::Win32::Foundation::HANDLE);
unsafe impl Send for KillTree {}
unsafe impl Sync for KillTree {}
impl Drop for KillTree {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

fn kill_tree(child: &tokio::process::Child) -> Option<KillTree> {
    use windows::Win32::System::JobObjects::*;
    unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let guard = KillTree(job);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of_val(&info) as u32).ok()?;
        let handle = windows::Win32::Foundation::HANDLE(child.raw_handle()?);
        AssignProcessToJobObject(job, handle).ok()?;
        Some(guard)
    }
}

impl Task {
    pub async fn run(self) {
        let id = self.id;
        let events = self.events.clone();
        if let Err(error) = self.go().await {
            let _ = events.send(Ev::Failed { id, error });
        }
    }

    async fn go(self) -> Result<(), String> {
        let progress = Arc::new(Progress::default());
        progress.set_phase(1);
        LIVE.lock().unwrap().get_or_insert_with(HashMap::new).insert(self.id, progress.clone());
        let _reg = Registered(self.id);

        // claim the name first (a fresh start), so part files are only ever ours
        let (claim, name) = match self.claim.clone().filter(|c| c.exists()) {
            Some(c) => (c, self.name.clone()),
            None => claim(&self.dir, &self.name)?,
        };
        let stem = stem_of(&name);
        let _ = self.events.send(Ev::HttpMeta { id: self.id, name: name.clone(), total: self.spec.expected, resumable: true, temp: claim.clone(), validator: None, dir: self.dir.clone() });

        // the page's picture, for the row while it downloads
        if let Some(t) = self.spec.thumbnail.clone() {
            let pic = super::photo_preview(self.id);
            if !pic.exists() {
                tokio::spawn(async move { save_picture(&t, &pic).await });
            }
        }

        progress.set_phase(2);
        let exe = crate::tools::ytdlp().await?;
        let ffmpeg = crate::tools::ffmpeg().await?;
        progress.set_phase(1);

        let printed = claim.with_extension("stashvideo");
        let _ = std::fs::remove_file(&printed);
        let mut c = crate::tools::command(&exe);
        c.args(["--ignore-config", "--no-playlist", "--newline", "--progress", "--no-colors", "--continue", "--no-mtime", "--ies", "default,-generic"])
            .args(["--progress-template", TEMPLATE])
            .args(self.spec.quality.args())
            .args(self.spec.clip_args())
            .args(["--embed-thumbnail", "--embed-metadata"])
            .arg("--ffmpeg-location")
            .arg(&ffmpeg)
            .arg("-N")
            .arg(self.connections.clamp(1, 8).to_string())
            .arg("-P")
            .arg(&self.dir)
            .arg("-o")
            .arg(format!("{}.%(ext)s", stem.replace('%', "%%")))
            .arg("--print-to-file")
            .arg("after_move:filepath")
            .arg(&printed)
            .args(proxy_args());
        if self.limit_bps > 0 {
            c.arg("--limit-rate").arg(self.limit_bps.max(1024).to_string());
        }
        c.arg("--").arg(&self.url);
        c.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        let mut child = c.spawn().map_err(|e| format!("Couldn't start the video tools: {e}"))?;
        let _tree = kill_tree(&child);
        let stdout = child.stdout.take().ok_or("no output")?;
        let stderr = child.stderr.take().ok_or("no output")?;
        // stderr: kept for the error message, and for a clip, ffmpeg's progress (yt-dlp
        // hands a section to ffmpeg and reports nothing itself until it's done). ffmpeg
        // ends its progress lines with \r, so split on that too.
        let clip_len = self.spec.clip.map(|(a, b)| b - a).filter(|l| *l > 0.0);
        let (cprogress, expected) = (progress.clone(), self.spec.expected);
        let errors = tokio::spawn(async move {
            let mut stderr = stderr;
            let mut all = String::new();
            let mut pending: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            let mut meter = ClipMeter::default();
            loop {
                let n = match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                pending.extend_from_slice(&buf[..n]);
                while let Some(i) = pending.iter().position(|b| *b == b'\r' || *b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=i).collect();
                    let l = String::from_utf8_lossy(&line[..line.len() - 1]);
                    if let (Some(len), Some(p)) = (clip_len, ffmpeg_progress(&l)) {
                        let (done, total, speed) = meter.add(p, len, expected);
                        cprogress.done.store(done, Ordering::Relaxed);
                        cprogress.total.store(total, Ordering::Relaxed);
                        cprogress.speed.store(speed, Ordering::Relaxed);
                        cprogress.set_phase(0);
                        continue; // (hundreds of these: not worth keeping)
                    }
                    if !l.trim().is_empty() && all.len() < 64_000 {
                        all.push_str(&l);
                        all.push('\n');
                    }
                }
            }
            all
        });
        let mut tally = Tally::new(self.spec.expected);
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            if let Some(p) = parse_line(&l) {
                let (done, total) = tally.add(&p);
                progress.done.store(done, Ordering::Relaxed);
                progress.total.store(total, Ordering::Relaxed);
                progress.speed.store(if p.finished { 0 } else { p.speed.unwrap_or(0) }, Ordering::Relaxed);
                if !p.finished {
                    progress.set_phase(0);
                }
            } else if let Some(ph) = phase_of(&l) {
                progress.set_phase(ph);
                progress.speed.store(0, Ordering::Relaxed);
            }
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        let stderr = errors.await.unwrap_or_default();
        if !status.success() {
            return Err(plain_error(&stderr));
        }
        let path = std::fs::read_to_string(&printed)
            .ok()
            .and_then(|t| t.lines().rev().map(str::trim).find(|l| !l.is_empty()).map(PathBuf::from))
            .filter(|p| p.is_file())
            .or_else(|| {
                let p = self.dir.join(&name);
                p.is_file().then_some(p)
            })
            .ok_or("The video downloaded, but its file couldn't be found.")?;
        let _ = std::fs::remove_file(&printed);
        let _ = std::fs::remove_file(&claim);
        mark_from_internet(&path, &self.url);
        let _ = std::fs::remove_file(super::photo_preview(self.id));
        let total = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let _ = self.events.send(Ev::HttpDone { id: self.id, path, total });
        Ok(())
    }
}

/// One ffmpeg progress line: how far into the clip it is (seconds) and how big the
/// output is so far. "frame=  656 fps= 38 q=32.0 size=    1536KiB time=00:00:27.33 …"
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FfProgress {
    pub time: f64,
    pub size: Option<u64>,
}

pub fn ffmpeg_progress(line: &str) -> Option<FfProgress> {
    let after = |key: &str| -> Option<&str> {
        let i = line.find(key)?;
        line[i + key.len()..].split_whitespace().next()
    };
    let t = after("time=")?;
    let mut secs = 0.0;
    for part in t.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    let size = after("size=").and_then(|s| {
        let digits = s.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let n: f64 = digits.parse().ok()?;
        let mult = match &s[digits.len()..] {
            "KiB" | "kB" => 1024.0,
            "MiB" | "MB" => 1024.0 * 1024.0,
            "GiB" | "GB" => 1024.0 * 1024.0 * 1024.0,
            _ => 1.0,
        };
        Some((n * mult) as u64)
    });
    Some(FfProgress { time: secs, size })
}

/// Turns ffmpeg's progress on a clip into the row's numbers: done and total (by how far
/// through the clip it is, against the expected size) and the speed (how fast the output
/// grows, smoothed).
#[derive(Default)]
struct ClipMeter {
    last: Option<(std::time::Instant, u64)>,
    speed: f64,
}

impl ClipMeter {
    fn add(&mut self, p: FfProgress, len: f64, expected: Option<u64>) -> (u64, u64, u64) {
        let frac = (p.time / len).clamp(0.0, 0.99);
        let size = p.size.unwrap_or(0);
        let now = std::time::Instant::now();
        match self.last {
            Some((t0, s0)) => {
                let dt = now.duration_since(t0).as_secs_f64();
                if dt >= 0.25 {
                    let rate = size.saturating_sub(s0) as f64 / dt;
                    self.speed = if self.speed == 0.0 { rate } else { self.speed * 0.7 + rate * 0.3 };
                    self.last = Some((now, size));
                }
            }
            None => self.last = Some((now, size)),
        }
        let (done, total) = match expected.filter(|e| *e > 0) {
            Some(e) => ((e as f64 * frac) as u64, e),
            // no size to go by (and ffmpeg holds its output back at first): once it has
            // written some, the size so far scaled up; before that, a stand-in total so
            // the bar still follows the clip
            None if size > 64 * 1024 && frac > 0.05 => (size, (size as f64 / frac) as u64),
            None => ((frac * 1_000_000.0) as u64, 1_000_000),
        };
        (done, total.max(done), self.speed as u64)
    }
}

fn stem_of(name: &str) -> String {
    Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.to_string())
}

/// Reserve a stem no file in the folder starts with yet ("Title (2).mp4" otherwise),
/// by creating `<stem>.stashpart`: then every `<stem>.*` file is this download's.
fn claim(dir: &Path, name: &str) -> Result<(PathBuf, String), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Can't create the folder: {e}"))?;
    let stem = stem_of(name);
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
    let taken: Vec<String> = std::fs::read_dir(dir).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().to_lowercase()).collect()).unwrap_or_default();
    for n in 1..10_000 {
        let s = if n == 1 { stem.clone() } else { format!("{stem} ({n})") };
        let prefix = format!("{}.", s.to_lowercase());
        if taken.iter().any(|t| t.starts_with(&prefix)) {
            continue;
        }
        let marker = dir.join(format!("{s}.stashpart"));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&marker) {
            Ok(_) => return Ok((marker, format!("{s}.{ext}"))),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Can't create the file: {e}")),
        }
    }
    Err("Can't find a free name for the video".into())
}

/// A cancelled video's part files (`<stem>.*`, which its claim made sure are its own).
/// yt-dlp may take a moment to let go of them after it's stopped.
pub(crate) fn clean_partials(dir: &Path, name: &str) {
    let (dir, stem) = (dir.to_path_buf(), stem_of(name));
    if stem.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let prefix = format!("{}.", stem.to_lowercase());
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(300));
            let left: Vec<PathBuf> = std::fs::read_dir(&dir)
                .map(|r| r.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && e.file_name().to_string_lossy().to_lowercase().starts_with(&prefix)).map(|e| e.path()).collect())
                .unwrap_or_default();
            if left.is_empty() {
                return;
            }
            for p in left {
                let _ = std::fs::remove_file(p);
            }
        }
    });
}

async fn save_picture(url: &str, dest: &Path) {
    let Ok(r) = reqwest::get(url).await else { return };
    if !r.status().is_success() {
        return;
    }
    let Ok(b) = r.bytes().await else { return };
    // only a JPEG goes where the row looks for one
    if b.len() > 4 && b[0] == 0xFF && b[1] == 0xD8 {
        if let Some(d) = dest.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(dest, &b);
    }
}

/// Note that the file came from the internet, as a browser does (see `http.rs`).
fn mark_from_internet(path: &Path, url: &str) {
    let mut zone = String::from("[ZoneTransfer]\r\nZoneId=3\r\n");
    if url.starts_with("http") {
        zone += &format!("HostUrl={}\r\n", url.replace(['\r', '\n'], ""));
    }
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    let _ = std::fs::write(PathBuf::from(stream), zone);
}

/// Fetch a page's picture for the sheet: a small JPEG in the temp folder.
pub async fn sheet_picture(url: &str) -> Option<PathBuf> {
    let dest = std::env::temp_dir().join(format!("stash-video-{}.jpg", std::process::id()));
    let r = reqwest::get(url).await.ok()?;
    let b = r.bytes().await.ok()?;
    std::fs::write(&dest, &b).ok()?;
    Some(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines() {
        let l = parse_line("[stash] downloading 1048576 52428800 NA 2097152.5").unwrap();
        assert_eq!(l, Line { finished: false, downloaded: 1048576, total: Some(52428800), speed: Some(2097152) });
        // fragments: no total, an estimate
        let l = parse_line("[stash] downloading 2000 NA 90000.7 NA").unwrap();
        assert_eq!(l.total, Some(90000));
        assert_eq!(l.speed, None);
        let l = parse_line("  [stash] finished 52428800 52428800 NA NA").unwrap();
        assert!(l.finished);
        assert_eq!(parse_line("[download] Destination: x.mp4"), None);
        assert_eq!(parse_line("[stash] downloading NA NA NA NA").unwrap().downloaded, 0);
    }

    #[test]
    fn picture_then_sound_add_up() {
        let mut t = Tally::new(Some(1100));
        let line = |s: &str| parse_line(s).unwrap();
        assert_eq!(t.add(&line("[stash] downloading 500 1000 NA 10")), (500, 1100));
        assert_eq!(t.add(&line("[stash] finished 1000 1000 NA NA")), (1000, 1100));
        assert_eq!(t.add(&line("[stash] downloading 50 200 NA 10")), (1050, 1200));
        assert_eq!(t.add(&line("[stash] finished 200 200 NA NA")), (1200, 1200));
        // nothing expected: the total grows as parts are found
        let mut t = Tally::new(None);
        assert_eq!(t.add(&line("[stash] downloading 10 100 NA 1")), (10, 100));
    }

    #[test]
    fn phases() {
        assert_eq!(phase_of("[Merger] Merging formats into \"a.mp4\""), Some(3));
        assert_eq!(phase_of("[ExtractAudio] Destination: a.mp3"), Some(4));
        assert_eq!(phase_of("[EmbedThumbnail] ffmpeg: Adding thumbnail"), Some(5));
        assert_eq!(phase_of("[youtube] Extracting URL"), None);
    }

    const VIDEO: &str = r#"{
        "_type": "video", "id": "aqz-KE-bpKQ", "title": "Big Buck Bunny", "duration": 635, "uploader": "Blender",
        "webpage_url": "https://www.youtube.com/watch?v=aqz-KE-bpKQ", "is_live": false,
        "thumbnail": "https://i.ytimg.com/vi_webp/aqz-KE-bpKQ/maxresdefault.webp",
        "thumbnails": [
            {"url": "https://i.ytimg.com/vi/aqz-KE-bpKQ/sddefault.jpg"},
            {"url": "https://i.ytimg.com/vi/aqz-KE-bpKQ/hq720.jpg?sqp=x", "width": 1280},
            {"url": "https://i.ytimg.com/vi_webp/aqz-KE-bpKQ/maxresdefault.webp", "width": 1920}
        ],
        "formats": [
            {"format_id": "sb0", "ext": "mhtml", "vcodec": "none", "acodec": "none", "height": 180},
            {"format_id": "139", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.5", "filesize": 3871021, "tbr": 48.7},
            {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "filesize": 10271496, "tbr": 129.4},
            {"format_id": "251", "ext": "webm", "vcodec": "none", "acodec": "opus", "filesize": 10202210, "tbr": 128.6},
            {"format_id": "135", "ext": "mp4", "vcodec": "avc1.4D401F", "acodec": "none", "height": 480, "filesize": 28207144, "tbr": 355.6, "protocol": "https"},
            {"format_id": "244", "ext": "webm", "vcodec": "vp9", "acodec": "none", "height": 480, "filesize": 33138062, "tbr": 417.7, "protocol": "https"},
            {"format_id": "312", "ext": "mp4", "vcodec": "avc1.64002A", "acodec": "none", "height": 1080, "tbr": 7417.7, "protocol": "m3u8_native"},
            {"format_id": "299", "ext": "mp4", "vcodec": "avc1.64002A", "acodec": "none", "height": 1080, "filesize": 257619653, "tbr": 3247.8, "protocol": "https"},
            {"format_id": "399", "ext": "mp4", "vcodec": "av01.0.09M.08", "acodec": "none", "height": 1080, "filesize": 124386876, "tbr": 1568.1, "protocol": "https"},
            {"format_id": "401", "ext": "mp4", "vcodec": "av01.0.13M.08", "acodec": "none", "height": 2160, "filesize_approx": 712445280, "tbr": 8981.8, "protocol": "https"},
            {"format_id": "18", "ext": "mp4", "vcodec": "avc1.42001E", "acodec": "mp4a.40.2", "height": 360, "tbr": 500.0, "protocol": "https"}
        ]
    }"#;

    #[test]
    fn page_json_to_choices() {
        let i = parse_info(VIDEO).unwrap();
        assert_eq!(i.title, "Big Buck Bunny");
        assert_eq!(i.duration, Some(635.0));
        assert_eq!(i.max_height, Some(2160));
        assert_eq!(i.uploader.as_deref(), Some("Blender"));
        assert!(!i.only_playlist);
        assert_eq!(i.thumbnail.as_deref(), Some("https://i.ytimg.com/vi/aqz-KE-bpKQ/hq720.jpg?sqp=x"));
        // best: the 4K picture and the m4a sound
        assert_eq!(i.size(Quality::Best), Some(712445280 + 10271496));
        // 1080p: H.264 over https (as yt-dlp's sort picks), not the smaller AV1
        assert_eq!(i.size(Quality::P1080), Some(257619653 + 10271496));
        // 720p: none at 720, so the next lower (480p H.264)
        assert_eq!(i.size(Quality::P720), Some(28207144 + 10271496));
        assert_eq!(i.size(Quality::AudioM4a), Some(10271496));
        assert_eq!(i.size(Quality::AudioMp3), Some((635.0 * 245_000.0 / 8.0) as u64));
        assert_eq!(i.default_quality(), Quality::P1080);
        assert!(i.offers(Quality::P1080));
    }

    #[test]
    fn small_video_offers_less() {
        let i = parse_info(r#"{"title": "Clip", "duration": 10, "formats": [{"vcodec": "avc1", "acodec": "mp4a", "ext": "mp4", "height": 720, "tbr": 800}]}"#).unwrap();
        assert!(!i.offers(Quality::P1080));
        assert!(i.offers(Quality::P720));
        assert_eq!(i.default_quality(), Quality::Best);
        // a muxed format has its own sound; size from bitrate and length
        assert_eq!(i.size(Quality::P720), Some(1_000_000));
    }

    #[test]
    fn playlist_json() {
        let i = parse_info(
            r#"{"_type": "playlist", "title": "Shorts", "entries": [
                {"_type": "url", "url": "https://www.youtube.com/watch?v=a", "title": "One"},
                {"_type": "url", "url": "https://www.youtube.com/watch?v=b", "title": "Two"},
                {"_type": "url", "url": "javascript:x", "title": "Bad"}
            ]}"#,
        )
        .unwrap();
        assert!(i.only_playlist);
        let p = i.playlist.unwrap();
        assert_eq!(p.title, "Shorts");
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.entries[1].title, "Two");
        assert!(parse_info("not json").is_err());
    }

    #[test]
    fn errors_in_plain_words() {
        assert_eq!(plain_error("WARNING: x\nERROR: [youtube] abc123: Video unavailable. This video has been removed"), "This video isn't available any more.");
        assert_eq!(plain_error("ERROR: [youtube] abc: Private video. Sign in if you've been granted access"), "This video is private.");
        assert_eq!(plain_error("ERROR: Unsupported URL: https://example.com/"), "There's no video Stash can get on this page.");
        assert_eq!(plain_error("ERROR: [vimeo] 123: Something odd happened"), "Something odd happened");
        assert_eq!(plain_error(""), "The video couldn't be downloaded.");
    }

    #[test]
    fn names_and_links() {
        let s = Spec { title: "What: a <video>?".into(), quality: Quality::AudioMp3, ..Default::default() };
        assert_eq!(s.file_name(), "What_ a _video__.mp3");
        assert_eq!(Spec { title: "..".into(), ..Default::default() }.file_name(), "Video.mp4");
        assert_eq!(duration_text(635.0), "10:35");
        assert_eq!(duration_text(3723.0), "1:02:03");
        assert!(worth_probing("https://www.youtube.com/watch?v=aqz-KE-bpKQ"));
        assert!(worth_probing("https://youtu.be/aqz-KE-bpKQ"));
        assert!(!worth_probing("https://www.youtube.com/file.zip"));
        assert!(!worth_probing("magnet:?xt=urn:btih:x"));
    }

    #[test]
    fn ffmpeg_lines() {
        let p = ffmpeg_progress("frame=  656 fps= 38 q=32.0 size=    1536KiB time=00:00:27.33 bitrate= 460.4kbits/s speed=1.59x").unwrap();
        assert_eq!(p, FfProgress { time: 27.33, size: Some(1536 * 1024) });
        let end = ffmpeg_progress("frame=  720 fps= 40 q=32.0 Lsize=    1751KiB time=00:00:30.00 bitrate= 478.2kbits/s").unwrap();
        assert_eq!(end.time, 30.0);
        assert_eq!(ffmpeg_progress("frame=  696 size=  1536KiB time=N/A bitrate=N/A"), None);
        assert_eq!(ffmpeg_progress("[youtube] YE7VzlLtp-4: Downloading webpage"), None);
        assert_eq!(ffmpeg_progress("size=1kB time=01:02:03.50").unwrap().time, 3723.5);
        // halfway through a 30 s clip expected at 4 MB
        let mut m = ClipMeter::default();
        let (done, total, _) = m.add(FfProgress { time: 15.0, size: Some(1_000_000) }, 30.0, Some(4_000_000));
        assert_eq!((done, total), (2_000_000, 4_000_000));
        // never "done" before ffmpeg is
        assert_eq!(m.add(FfProgress { time: 31.0, size: None }, 30.0, Some(100)).0, 99);
        // no estimate, nothing written yet: still follows the clip
        assert_eq!(m.add(FfProgress { time: 7.5, size: Some(0) }, 30.0, None), (250_000, 1_000_000, m.speed as u64));
    }

    #[test]
    fn storyboards() {
        let json = r#"{"title":"T","duration":600,"formats":[
            {"format_id":"sb2","width":48,"height":27,"rows":10,"columns":10,"fps":0.17,"fragments":[{"url":"a"}]},
            {"format_id":"sb0","width":159,"height":90,"rows":5,"columns":5,"fps":0.2,"fragments":[{"url":"s0"},{"url":"s1"}]}]}"#;
        let b = parse_info(json).unwrap().storyboard.unwrap();
        assert_eq!((b.width, b.cols, b.sheets.len()), (159, 5, 2));
        // a frame every 5 s, 25 to a sheet
        assert_eq!(b.cell(0.0), Some((0, 0, 0)));
        assert_eq!(b.cell(31.0), Some((0, 1, 1)));
        assert_eq!(b.cell(126.0), Some((1, 0, 0)));
        assert_eq!(b.cell(10_000.0), Some((1, 4, 4)), "past the end: the last frame");
    }

    #[test]
    fn clips() {
        let s = Spec { title: "Talk".into(), clip: Some((65.0, 150.4)), ..Default::default() };
        assert_eq!(s.file_name(), "Talk (1m05s-2m30s).mp4");
        assert_eq!(s.clip_args(), ["--download-sections", "*65.0-150.4", "--force-keyframes-at-cuts"]);
        let long = Spec { title: "Film".into(), quality: Quality::AudioM4a, clip: Some((3723.0, 3730.0)), ..Default::default() };
        assert_eq!(long.file_name(), "Film (1h02m03s-1h02m10s).m4a");
        assert!(Spec::default().clip_args().is_empty());
    }

    /// Network test: downloads 5 seconds out of the middle of a video, and checks the file
    /// is about that long (ffprobe-free: by size against the whole video's estimate).
    /// `cargo test dl::video::tests::clip_end_to_end -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn clip_end_to_end() {
        use super::super::*;
        let dir = std::env::temp_dir().join("stash-dl-test-clip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("STASH_DATA", dir.join("data")) };
        let url = std::env::var("STASH_TEST_URL").unwrap_or("https://www.youtube.com/watch?v=YE7VzlLtp-4".into());
        let info = crate::tools::runtime().block_on(probe(&url)).expect("probe");
        let d = info.duration.expect("duration");
        let (a, b) = ((d / 2.0).floor(), (d / 2.0).floor() + 5.0);
        let q = Quality::P480;
        // the size the New Download sheet would expect for this clip
        let expected = info.size(q).map(|s| (s as f64 * (b - a) / d) as u64);
        let spec = Spec { quality: q, title: info.title.clone(), expected, thumbnail: None, clip: Some((a, b)) };
        let engine = Engine::start(Config { max_active: 1, connections: 4, limit_bps: 0, seed: false }, || {});
        engine.add(AddRequest { source: info.url.clone(), dir: dir.clone(), video: Some(spec), ..Default::default() });
        let start = std::time::Instant::now();
        let mut moved = false;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let s = engine.snapshot();
            let j = &s.jobs[0];
            println!("{:?} {:?} {} {}/{:?} {:?}", j.state, j.phase, j.name, j.done, j.total, j.error);
            // part-way through, the row shows how far the clip has got
            moved |= j.state == State::Active && j.done > 0 && j.total.is_some_and(|t| j.done < t);
            if matches!(j.state, State::Done | State::Failed) || start.elapsed().as_secs() > 300 {
                assert_eq!(j.state, State::Done, "{:?}", j.error);
                let p = j.path.clone().unwrap();
                let len = std::fs::metadata(&p).unwrap().len();
                println!("saved {} ({len} bytes, whole video about {:?})", p.display(), info.size(q));
                assert!(p.file_name().unwrap().to_string_lossy().contains("-"), "the name says which part");
                assert!(len > 10_000);
                assert!(moved, "progress showed while the clip downloaded");
                if let Some(whole) = info.size(q) {
                    assert!((len as f64) < whole as f64 * 0.6, "only a part was downloaded");
                }
                break;
            }
        }
    }

    #[test]
    fn claims_a_free_stem() {
        let dir = std::env::temp_dir().join("stash-video-claim-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Clip.webm"), b"x").unwrap();
        let (marker, name) = claim(&dir, "Clip.mp4").unwrap();
        assert_eq!(name, "Clip (2).mp4");
        assert!(marker.ends_with("Clip (2).stashpart"));
        // a cancel takes every part file of that stem, and nothing else
        std::fs::write(dir.join("Clip (2).f137.mp4.part"), b"x").unwrap();
        clean_partials(&dir, &name);
        std::thread::sleep(Duration::from_millis(800));
        assert!(!dir.join("Clip (2).f137.mp4.part").exists());
        assert!(!marker.exists());
        assert!(dir.join("Clip.webm").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Network test: downloads Big Buck Bunny (Creative Commons, Blender Foundation) as
    /// audio and at 480p through the engine, pausing and resuming the video on the way.
    /// The tools go to STASH_TOOLS if set (to keep them between runs).
    /// `cargo test dl::video::tests::end_to_end -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn end_to_end() {
        use super::super::*;
        let dir = std::env::temp_dir().join("stash-dl-test-video");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("STASH_DATA", dir.join("data")) };
        // "Big Buck Bunny" teaser-sized upload; override with STASH_TEST_URL
        let url = std::env::var("STASH_TEST_URL").unwrap_or("https://www.youtube.com/watch?v=YE7VzlLtp-4".into());
        let info = crate::tools::runtime().block_on(probe(&url)).expect("probe");
        println!("{} {:?} {:?}", info.title, info.duration.map(duration_text), info.sizes);
        let engine = Engine::start(Config { max_active: 2, connections: 4, limit_bps: 0, seed: false }, || {});
        for (i, q) in [Quality::AudioM4a, Quality::P480].into_iter().enumerate() {
            let spec = Spec { quality: q, title: info.title.clone(), expected: info.size(q), thumbnail: info.thumbnail.clone(), clip: None };
            // (the same link twice would be one download)
            let source = if i == 0 { info.url.clone() } else { format!("{}&stash={i}", info.url) };
            engine.add(AddRequest { source, dir: dir.join(q.label().replace(['(', ')', ' '], "")), video: Some(spec), ..Default::default() });
        }
        let start = std::time::Instant::now();
        let mut paused = false;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let s = engine.snapshot();
            for j in &s.jobs {
                println!("{:?} {:?} {} {}/{:?} {} B/s {:?}", j.state, j.phase, j.name, j.done, j.total, j.speed, j.error);
            }
            // pause the video part-way, then carry on from its part files
            if let Some(j) = s.jobs.get(1).filter(|j| !paused && j.state == State::Active && j.done > 3_000_000) {
                paused = true;
                let before = j.done;
                engine.pause(j.id);
                std::thread::sleep(Duration::from_secs(2));
                assert_eq!(engine.snapshot().jobs[1].state, State::Paused);
                engine.resume(j.id);
                println!("paused at {before}, resumed");
            }
            if s.jobs.len() == 2 && s.jobs.iter().all(|j| matches!(j.state, State::Done | State::Failed)) || start.elapsed().as_secs() > 600 {
                for j in &s.jobs {
                    assert_eq!(j.state, State::Done, "{:?}", j.error);
                    let p = j.path.clone().unwrap();
                    assert!(std::fs::metadata(&p).unwrap().len() > 100_000);
                    println!("saved {}", p.display());
                }
                // no part files left behind
                for q in [Quality::AudioM4a, Quality::P480] {
                    let d = dir.join(q.label().replace(['(', ')', ' '], ""));
                    let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
                    assert_eq!(names.len(), 1, "{names:?}");
                }
                assert!(paused, "never got to pause the video");
                break;
            }
        }
    }
}
