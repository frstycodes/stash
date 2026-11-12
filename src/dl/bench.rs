//! Download benchmark (see `docs/performance.md` and `bench/bench.py`).
//!
//! Two ignored tests, each meant to run as its own process:
//! - `dl::bench::serve` is a small local HTTP/1.1 file server whose behavior is set per
//!   request in the URL: `/f/<name>?size=<bytes>&rate=<bytes/s per connection>
//!   &ttfb=<ms>&ranges=0|1&maxconn=<n>`, plus a shared link cap (`STASH_BENCH_LINK`,
//!   bytes/s). It sends a known byte pattern, so the client can check every byte.
//! - `dl::bench::run` downloads `STASH_BENCH_URLS` (space separated) with the real
//!   engine and prints one `BENCH {json}` line: wall time, average and peak speed.
//!   CPU time and peak memory are measured from outside, by the driver.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// The byte at offset `i` of every served file.
pub fn pattern(i: u64) -> u8 {
    (i.wrapping_mul(0x9E37_79B1) >> 13) as u8 ^ (i >> 20) as u8
}

fn env_u64(k: &str, default: u64) -> u64 {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[test]
#[ignore]
fn serve() {
    serve_on(env_u64("STASH_BENCH_PORT", 47900), env_u64("STASH_BENCH_LINK", 0));
}

/// The test server in a thread of this process (for correctness tests, not timing).
fn serve_in_background(port: u64) {
    std::thread::spawn(move || serve_on(port, 0));
    std::thread::sleep(Duration::from_millis(300));
}

fn serve_on(port: u64, link: u64) {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port as u16)).await.unwrap();
        println!("serving on {port}, link cap {link} B/s");
        let link = Arc::new(limiter::Limiter::new(link));
        let active = Arc::new(AtomicUsize::new(0));
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let _ = sock.set_nodelay(true);
            let (link, active) = (link.clone(), active.clone());
            tokio::spawn(async move {
                let _ = conn(sock, link, active).await;
            });
        }
    });
}

async fn conn(mut sock: tokio::net::TcpStream, link: Arc<limiter::Limiter>, active: Arc<AtomicUsize>) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(4096);
    loop {
        // read one request head
        let head = loop {
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..p]).into_owned();
                buf.drain(..p + 4);
                break head;
            }
            let mut tmp = [0u8; 2048];
            let n = sock.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
        };
        let mut lines = head.lines();
        let target = lines.next().unwrap_or("").split(' ').nth(1).unwrap_or("/").to_string();
        let range = lines.find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case("range").then(|| v.trim().to_string())
        });
        let q = |k: &str, d: u64| -> u64 {
            target.split_once('?').map(|(_, q)| q).unwrap_or("").split('&').find_map(|kv| kv.strip_prefix(k)?.strip_prefix('=')?.parse().ok()).unwrap_or(d)
        };
        let (size, rate, ttfb, ranges, maxconn) = (q("size", 10 << 20), q("rate", 0), q("ttfb", 0), q("ranges", 1) == 1, q("maxconn", 0));
        // at most `maxreq` requests per 5 seconds (like some mirrors)
        let maxreq = q("maxreq", 0);
        if maxreq > 0 {
            static TIMES: std::sync::Mutex<std::collections::VecDeque<Instant>> = std::sync::Mutex::new(std::collections::VecDeque::new());
            let refuse = {
                let mut t = TIMES.lock().unwrap();
                while t.front().is_some_and(|x| x.elapsed() > Duration::from_secs(5)) {
                    t.pop_front();
                }
                let over = t.len() as u64 >= maxreq;
                if !over {
                    t.push_back(Instant::now());
                }
                over
            };
            if refuse {
                sock.write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n").await?;
                continue;
            }
        }

        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
        let _guard = Dec(active.clone());
        if maxconn > 0 && now as u64 > maxconn {
            sock.write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n").await?;
            continue;
        }
        if ttfb > 0 {
            tokio::time::sleep(Duration::from_millis(ttfb)).await;
        }
        let parsed = range.as_deref().and_then(|r| r.strip_prefix("bytes=")).and_then(|r| {
            let (a, b) = r.split_once('-')?;
            let a: u64 = a.parse().ok()?;
            let b: u64 = if b.is_empty() { size - 1 } else { b.parse::<u64>().ok()?.min(size - 1) };
            Some((a, b + 1))
        });
        // `lie=N`: like a CDN that answers a range with the size of a compressed copy
        // (N) but sends the whole file, without a Content-Length
        let lie = q("lie", 0);
        if lie > 0 && parsed.is_some() {
            let head = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{lie}\r\nConnection: close\r\n\r\n", lie - 1);
            sock.write_all(head.as_bytes()).await?;
            let body: Vec<u8> = (0..size).map(pattern).collect();
            sock.write_all(&body).await?;
            return Ok(());
        }
        let (start, end, head) = match parsed.filter(|_| ranges) {
            Some((a, b)) if a < size && a < b => {
                (a, b, format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {a}-{}/{size}\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\n\r\n", b - a, b - 1))
            }
            _ => (0, size, format!("HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n{}Content-Type: application/octet-stream\r\n\r\n", if ranges { "Accept-Ranges: bytes\r\n" } else { "" })),
        };
        sock.write_all(head.as_bytes()).await?;
        let began = Instant::now();
        let mut pos = start;
        let mut chunk = vec![0u8; 64 * 1024];
        while pos < end {
            let n = ((end - pos) as usize).min(chunk.len());
            for (k, b) in chunk[..n].iter_mut().enumerate() {
                *b = pattern(pos + k as u64);
            }
            link.acquire(n).await;
            sock.write_all(&chunk[..n]).await?;
            pos += n as u64;
            if rate > 0 {
                let due = Duration::from_secs_f64((pos - start) as f64 / rate as f64);
                let el = began.elapsed();
                if due > el {
                    tokio::time::sleep(due - el).await;
                }
            }
        }
    }
}

struct Dec(Arc<AtomicUsize>);
impl Drop for Dec {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
#[ignore]
fn run() {
    let urls: Vec<String> = std::env::var("STASH_BENCH_URLS").expect("STASH_BENCH_URLS").split_whitespace().map(String::from).collect();
    let dir = std::env::temp_dir().join(format!("stash-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("STASH_DATA", dir.join("data")) };
    let config = Config {
        max_active: env_u64("STASH_BENCH_MAX_ACTIVE", 3) as usize,
        connections: env_u64("STASH_BENCH_CONNS", Config::default().connections as u64) as usize,
        limit_bps: env_u64("STASH_BENCH_LIMIT", 0),
        seed: false,
    };
    let engine = Engine::start(config, || {});
    let start = Instant::now();
    for u in &urls {
        engine.add(AddRequest { source: u.clone(), dir: dir.clone(), ..Default::default() });
    }
    let (mut peak, mut max_conns) = (0f64, 0usize);
    let mut window: std::collections::VecDeque<(Instant, u64)> = Default::default();
    let jobs = loop {
        std::thread::sleep(Duration::from_millis(50));
        let s = engine.snapshot();
        let done: u64 = s.jobs.iter().map(|j| j.done).sum();
        max_conns = max_conns.max(s.jobs.iter().map(|j| j.connections).sum());
        let t = Instant::now();
        if std::env::var_os("STASH_BENCH_TRACE").is_some() {
            let st: Vec<String> = s.jobs.iter().map(|j| format!("{:?}/{}c/{:?}", j.state, j.connections, j.error)).collect();
            eprintln!("{:6.2}s {:8.1} MB {}", start.elapsed().as_secs_f64(), done as f64 / 1048576.0, st.join(" "));
        }
        window.push_back((t, done));
        while window.len() > 2 && t.duration_since(window[0].0) > Duration::from_secs(1) {
            window.pop_front();
        }
        let (t0, d0) = window[0];
        if t.duration_since(t0) >= Duration::from_millis(900) {
            peak = peak.max((done - d0) as f64 / t.duration_since(t0).as_secs_f64());
        }
        if s.jobs.len() == urls.len() && s.jobs.iter().all(|j| matches!(j.state, State::Done | State::Failed)) {
            break s.jobs;
        }
        assert!(start.elapsed() < Duration::from_secs(env_u64("STASH_BENCH_TIMEOUT", 300)), "timed out");
    };
    let wall = start.elapsed().as_secs_f64();
    let cpu = cpu_secs();
    let mut bytes = 0;
    let mut failed = Vec::new();
    for j in &jobs {
        if j.state != State::Done {
            failed.push(format!("{}: {:?}", j.source, j.error));
            continue;
        }
        let p = j.path.clone().unwrap();
        let len = std::fs::metadata(&p).unwrap().len();
        assert_eq!(len, j.total.unwrap(), "size of {}", p.display());
        if j.source.contains("127.0.0.1") {
            // every byte, a megabyte at a time
            use std::io::Read;
            let mut f = std::fs::File::open(&p).unwrap();
            let (mut buf, mut off) = (vec![0u8; 1 << 20], 0u64);
            loop {
                let n = f.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                let bad = buf[..n].iter().enumerate().position(|(i, b)| *b != pattern(off + i as u64));
                assert!(bad.is_none(), "wrong byte at {} in {}", off + bad.unwrap() as u64, p.display());
                off += n as u64;
            }
        }
        bytes += len;
    }
    engine.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
    println!(
        "BENCH {{\"wall\":{wall:.3},\"cpu\":{cpu:.3},\"bytes\":{bytes},\"avg\":{:.0},\"peak\":{peak:.0},\"max_conns\":{max_conns},\"failed\":{}}}",
        bytes as f64 / wall,
        serde_json::to_string(&failed).unwrap()
    );
    assert!(failed.is_empty(), "failed: {failed:?}");
}

/// User + kernel time of this process so far.
fn cpu_secs() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut a, mut b, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut a, &mut b, &mut k, &mut u);
    }
    let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
    t(k) + t(u)
}

/// Which HTTP version each public benchmark host speaks to the engine's client.
#[test]
#[ignore]
fn protocols() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let client = reqwest::Client::builder().build().unwrap();
        for u in std::env::var("STASH_BENCH_URLS").unwrap().split_whitespace() {
            match client.get(u).header("range", "bytes=0-").send().await {
                Ok(r) => println!("{:?} {} len={:?} range={:?} {}", r.version(), r.status(), r.content_length(), r.headers().get("content-range"), r.url().as_str().split('?').next().unwrap_or("")),
                Err(e) => println!("{u}: {e:?}"),
            }
        }
    });
}

fn wait_for(engine: &Engine, k: usize, what: &str, secs: u64, ok: impl Fn(&JobView) -> bool) -> JobView {
    let start = Instant::now();
    loop {
        let s = engine.snapshot();
        if let Some(j) = s.jobs.get(k) {
            if ok(j) {
                return j.clone();
            }
            assert!(j.state != State::Failed, "{what}: failed: {:?}", j.error);
        }
        assert!(start.elapsed() < Duration::from_secs(secs), "{what}: timed out: {:?}", s.jobs.get(k));
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn check_pattern(p: &std::path::Path, len: u64) {
    let data = std::fs::read(p).unwrap();
    assert_eq!(data.len() as u64, len);
    let bad = data.iter().enumerate().position(|(i, b)| *b != pattern(i as u64));
    assert!(bad.is_none(), "wrong byte at {bad:?}");
}

/// Pause in the middle (with every connection busy), resume, and check every byte;
/// then the same against a server that turns connections away, and one that limits
/// requests. `cargo test --release dl::bench::pause_resume -- --ignored --nocapture`
#[test]
#[ignore]
fn pause_resume() {
    let dir = std::env::temp_dir().join("stash-dl-test-resume");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("STASH_DATA", dir.join("data")) };
    serve_in_background(47911);
    let engine = Engine::start(Config { max_active: 1, connections: 8, limit_bps: 0, seed: false }, || {});
    let size = 64u64 << 20;
    for (k, extra) in ["", "&maxconn=3", "&maxreq=3"].iter().enumerate() {
        let url = format!("http://127.0.0.1:47911/f/r{k}.bin?size={size}&rate={}{extra}", 3u64 << 20);
        engine.add(AddRequest { source: url.clone(), dir: dir.clone(), ..Default::default() });
        let j = wait_for(&engine, k, "first third", 60, |j| j.done > size / 3);
        engine.pause(j.id);
        let paused = wait_for(&engine, k, "pause", 5, |j| j.state == State::Paused);
        println!("{extra:12} paused at {} of {size} bytes", paused.done);
        std::thread::sleep(Duration::from_millis(if extra.contains("maxreq") { 5500 } else { 500 }));
        engine.resume(j.id);
        let done = wait_for(&engine, k, "finish", 120, |j| j.state == State::Done);
        check_pattern(done.path.as_ref().unwrap(), size);
        println!("{extra:12} resumed and finished: every byte right");
    }
    engine.shutdown();
}

/// A server whose range answers don't add up (Content-Range says one size, it sends
/// another): the whole file still arrives, in one piece.
/// `cargo test --release dl::bench::lying_ranges -- --ignored --nocapture`
#[test]
#[ignore]
fn lying_ranges() {
    let dir = std::env::temp_dir().join("stash-dl-test-lie");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("STASH_DATA", dir.join("data")) };
    serve_in_background(47913);
    let engine = Engine::start(Config::default(), || {});
    let size = 3u64 << 20;
    engine.add(AddRequest { source: format!("http://127.0.0.1:47913/f/lie.js?size={size}&lie=400000"), dir: dir.clone(), ..Default::default() });
    let done = wait_for(&engine, 0, "download", 30, |j| j.state == State::Done);
    check_pattern(done.path.as_ref().unwrap(), size);
    println!("got all {size} bytes");
    engine.shutdown();
}

/// The speed limit still holds with the ramp and the new connection handling.
/// `cargo test --release dl::bench::limit -- --ignored --nocapture`
#[test]
#[ignore]
fn limit() {
    let dir = std::env::temp_dir().join("stash-dl-test-limit");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("STASH_DATA", dir.join("data")) };
    serve_in_background(47912);
    let limit = 4u64 << 20;
    let engine = Engine::start(Config { max_active: 2, connections: 8, limit_bps: limit, seed: false }, || {});
    let size = 24u64 << 20;
    engine.add(AddRequest { source: format!("http://127.0.0.1:47912/f/l.bin?size={size}"), dir: dir.clone(), ..Default::default() });
    let start = Instant::now();
    let done = wait_for(&engine, 0, "limited download", 60, |j| j.state == State::Done);
    let rate = size as f64 / start.elapsed().as_secs_f64();
    println!("limit {limit} B/s, got {rate:.0} B/s over {:?}", start.elapsed());
    check_pattern(done.path.as_ref().unwrap(), size);
    assert!(rate < limit as f64 * 1.15 && rate > limit as f64 * 0.75, "rate {rate} vs limit {limit}");
    engine.shutdown();
}
