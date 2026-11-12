//! FTP downloads (ftp://): one connection, passive mode, binary, resumed with REST.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc::UnboundedSender;

use super::Ev;
use super::http::{self, Progress};
use super::limiter::Limiter;
use super::retry::Failure;

const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Task {
    pub id: u64,
    pub url: String,
    pub dir: PathBuf,
    pub name: Option<String>,
    pub temp: Option<PathBuf>,
    pub total: Option<u64>,
    pub limiter: Arc<Limiter>,
    pub own_limit: Arc<Limiter>,
    pub progress: Arc<Progress>,
    pub events: UnboundedSender<Ev>,
    pub sort: Option<super::SortRule>,
}

/// The parts of an ftp:// link.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    pub path: String,
}

pub fn parse(url: &str) -> Option<Target> {
    let u = reqwest::Url::parse(url).ok()?;
    if u.scheme() != "ftp" {
        return None;
    }
    let dec = |s: &str| percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned();
    let user = if u.username().is_empty() { "anonymous".to_string() } else { dec(u.username()) };
    let pass = u.password().map(dec).unwrap_or_else(|| if user == "anonymous" { "anonymous@".into() } else { String::new() });
    let path = dec(u.path());
    if path.is_empty() || path.ends_with('/') {
        return None;
    }
    Some(Target { host: u.host_str()?.trim_matches(['[', ']']).to_string(), port: u.port().unwrap_or(21), user, pass, path })
}

/// "227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)": the port.
pub fn pasv_port(reply: &str) -> Option<u16> {
    let inner = reply.split_once('(')?.1.split(')').next()?;
    let n: Vec<u16> = inner.split(',').map(|x| x.trim().parse().ok()).collect::<Option<_>>()?;
    (n.len() == 6 && n[4] < 256 && n[5] < 256).then(|| n[4] * 256 + n[5])
}

/// "229 Entering Extended Passive Mode (|||6446|)": the port.
pub fn epsv_port(reply: &str) -> Option<u16> {
    let inner = reply.split_once('(')?.1.split(')').next()?;
    inner.trim_matches('|').parse().ok()
}

struct Control {
    r: BufReader<OwnedReadHalf>,
    w: OwnedWriteHalf,
}

impl Control {
    /// One reply (multi-line ones too): its code and its last line.
    async fn reply(&mut self) -> Result<(u16, String), Failure> {
        let mut line = String::new();
        let mut first: Option<String> = None;
        loop {
            line.clear();
            let n = tokio::time::timeout(TIMEOUT, self.r.read_line(&mut line)).await.map_err(|_| Failure::transient("The server stopped responding"))?.map_err(net)?;
            if n == 0 {
                return Err(Failure::transient("The server closed the connection"));
            }
            let l = line.trim_end().to_string();
            let code = l.get(0..3).filter(|c| c.bytes().all(|b| b.is_ascii_digit()));
            match (&first, code) {
                // the end of a multi-line reply repeats the code, then a space
                (Some(c), Some(code)) if code == c && l.as_bytes().get(3) == Some(&b' ') => break,
                (Some(_), _) => continue,
                (None, Some(code)) if l.as_bytes().get(3) == Some(&b'-') => first = Some(code.to_string()),
                (None, Some(_)) => break,
                (None, None) => return Err(Failure::fatal("That isn't an FTP server")),
            }
        }
        let l = line.trim_end().to_string();
        Ok((l[0..3].parse().unwrap_or(0), l))
    }

    async fn cmd(&mut self, c: &str) -> Result<(u16, String), Failure> {
        self.w.write_all(format!("{c}\r\n").as_bytes()).await.map_err(net)?;
        self.reply().await
    }

    /// A command that must succeed (2xx or 3xx).
    async fn ok(&mut self, c: &str) -> Result<(u16, String), Failure> {
        let (code, l) = self.cmd(c).await?;
        if code >= 400 {
            return Err(ftp_fail(code, &l));
        }
        Ok((code, l))
    }
}

fn net(e: std::io::Error) -> Failure {
    Failure::transient(format!("Can't reach the server: {e}"))
}

fn ftp_fail(code: u16, line: &str) -> Failure {
    let msg = match code {
        530 => "The server refused the sign-in".to_string(),
        550 => "The server doesn't have that file".to_string(),
        _ => format!("The server answered {}", line.chars().take(80).collect::<String>()),
    };
    // 4xx is "try again later" in FTP
    Failure { msg, transient: (400..500).contains(&code), ..Default::default() }
}

impl Task {
    pub async fn run(self) {
        let (id, events) = (self.id, self.events.clone());
        match self.go().await {
            Ok((path, total)) => {
                let _ = events.send(Ev::HttpDone { id, path, total });
            }
            Err(f) => {
                let _ = events.send(Ev::HttpFailed { id, failure: f });
            }
        }
    }

    async fn go(self) -> Result<(PathBuf, u64), Failure> {
        let t = parse(&self.url).ok_or("That isn't an FTP link to a file")?;
        let stream = tokio::time::timeout(TIMEOUT, TcpStream::connect((t.host.as_str(), t.port))).await.map_err(|_| Failure::transient("Can't connect to the server"))?.map_err(net)?;
        let peer = stream.peer_addr().map_err(net)?.ip();
        let (r, w) = stream.into_split();
        let mut c = Control { r: BufReader::new(r), w };
        let (code, l) = c.reply().await?;
        if code >= 400 {
            return Err(ftp_fail(code, &l));
        }
        let (code, l) = c.cmd(&format!("USER {}", t.user)).await?;
        if code == 331 {
            c.ok(&format!("PASS {}", t.pass)).await?;
        } else if code >= 400 {
            return Err(ftp_fail(code, &l));
        }
        c.ok("TYPE I").await?;
        let size = match c.cmd(&format!("SIZE {}", t.path)).await? {
            (213, l) => l[4..].trim().parse::<u64>().ok(),
            _ => None,
        };
        // carry on from where we stopped if it's the same size as before
        let resume_at = self.progress.first_pos().filter(|p| *p > 0 && self.temp.as_ref().is_some_and(|f| f.exists()) && size.is_some() && size == self.total);
        let (temp, file, offset) = match resume_at {
            Some(pos) => {
                let temp = self.temp.clone().unwrap();
                let f = std::fs::OpenOptions::new().write(true).open(&temp).map_err(|e| e.to_string())?;
                let name = self.name.clone().unwrap_or_else(|| "download".into());
                let _ = self.events.send(Ev::HttpMeta { id: self.id, name, total: size, resumable: true, temp: temp.clone(), validator: None, dir: self.dir.clone() });
                (temp, f, pos)
            }
            None => {
                if let Some(old) = &self.temp {
                    let _ = std::fs::remove_file(old);
                }
                let name = self.name.clone().or_else(|| http::name_from_url(&self.url)).unwrap_or_else(|| "download".into());
                let dir = match &self.sort {
                    Some(rule) => rule.dir_for(&self.dir, &name),
                    None => self.dir.clone(),
                };
                std::fs::create_dir_all(&dir).map_err(|e| format!("Can't create the folder: {e}"))?;
                let (temp, f) = http::claim(&dir, &name)?;
                if let Some(s) = size {
                    f.set_len(s).map_err(|e| format!("Not enough disk space: {e}"))?;
                }
                http::set_hidden(&temp, true);
                self.progress.reset(size);
                let _ = self.events.send(Ev::HttpMeta { id: self.id, name, total: size, resumable: size.is_some(), temp: temp.clone(), validator: None, dir: dir.clone() });
                (temp, f, 0)
            }
        };
        // passive mode: extended first (works over IPv6 and NAT), then classic
        let port = match c.cmd("EPSV").await? {
            (229, l) => epsv_port(&l),
            _ => match c.cmd("PASV").await? {
                (227, l) => pasv_port(&l),
                (code, l) => return Err(ftp_fail(code, &l)),
            },
        }
        .ok_or("The server's passive-mode answer didn't make sense")?;
        // connect to the address we already reach (not the one it names: NAT gets that wrong)
        let mut data = tokio::time::timeout(TIMEOUT, TcpStream::connect((peer, port))).await.map_err(|_| Failure::transient("Can't open the data connection"))?.map_err(net)?;
        if offset > 0 {
            c.ok(&format!("REST {offset}")).await?;
        }
        c.ok(&format!("RETR {}", t.path)).await?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut pos = offset;
        loop {
            let n = tokio::time::timeout(TIMEOUT, data.read(&mut buf)).await.map_err(|_| Failure::transient("The server stopped sending"))?.map_err(net)?;
            if n == 0 {
                break;
            }
            self.limiter.acquire(n).await;
            self.own_limit.acquire(n).await;
            {
                use std::os::windows::fs::FileExt;
                let mut written = 0;
                while written < n {
                    let w = file.seek_write(&buf[written..n], pos + written as u64).map_err(|e| format!("Can't write the file: {e}"))?;
                    if w == 0 {
                        return Err("Can't write the file: the disk is full".into());
                    }
                    written += w;
                }
            }
            pos += n as u64;
            self.progress.advance_first(n as u64);
        }
        drop(data);
        let (code, l) = c.reply().await?;
        if code >= 400 {
            return Err(ftp_fail(code, &l));
        }
        let _ = c.cmd("QUIT").await;
        if size.is_some_and(|s| pos < s) {
            return Err(Failure::transient("The download stopped before the end"));
        }
        self.progress.close_open();
        file.sync_all().ok();
        drop(file);
        http::set_hidden(&temp, false);
        let final_name = temp.file_name().unwrap().to_string_lossy().trim_end_matches(".stashpart").to_string();
        let dir = temp.parent().map(PathBuf::from).unwrap_or(self.dir.clone());
        let dest = http::finish(&temp, &dir, &final_name)?;
        let _ = self.progress.done.load(Ordering::Relaxed);
        Ok((dest, pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        let t = parse("ftp://ftp.example.com/pub/file%20one.iso").unwrap();
        assert_eq!((t.host.as_str(), t.port, t.user.as_str(), t.path.as_str()), ("ftp.example.com", 21, "anonymous", "/pub/file one.iso"));
        let t = parse("ftp://me:s%40cret@host:2121/a.zip").unwrap();
        assert_eq!((t.user.as_str(), t.pass.as_str(), t.port), ("me", "s@cret", 2121));
        assert!(parse("ftp://host/dir/").is_none());
        assert!(parse("http://host/a.zip").is_none());
    }

    #[test]
    fn passive_replies() {
        assert_eq!(pasv_port("227 Entering Passive Mode (192,168,1,2,19,137)."), Some(19 * 256 + 137));
        assert_eq!(epsv_port("229 Entering Extended Passive Mode (|||6446|)"), Some(6446));
        assert_eq!(pasv_port("227 nonsense"), None);
    }

    /// Network test: `cargo test dl::ftp::tests::download -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn download() {
        let dir = std::env::temp_dir().join("stash-ftp-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let url = std::env::var("STASH_TEST_FTP").unwrap_or("ftp://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz.sig".into());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let task = Task {
            id: 1,
            url,
            dir: dir.clone(),
            name: None,
            temp: None,
            total: None,
            limiter: Arc::new(Limiter::new(0)),
            own_limit: Arc::new(Limiter::new(0)),
            progress: Arc::new(Progress::new(0, &[])),
            events: tx,
            sort: None,
        };
        rt.block_on(task.run());
        let mut done = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                Ev::HttpDone { path, total, .. } => {
                    println!("saved {} ({total} bytes)", path.display());
                    assert_eq!(std::fs::metadata(&path).unwrap().len(), total);
                    done = true;
                }
                Ev::HttpFailed { failure, .. } => panic!("{failure:?}"),
                _ => {}
            }
        }
        assert!(done);
    }
}
