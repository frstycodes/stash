//! Signing in to sites that answer 401 (HTTP Basic and Digest).
//!
//! Sign-ins live in memory for the session, per host; "remember" keeps them in Windows
//! Credential Manager (never in Stash's own files). The request code asks
//! `header_for` for each request, so a sign-in only ever goes to the host it's for.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use sha2::Digest;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    pub user: String,
    pub pass: String,
}

/// A server's Digest challenge.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Challenge {
    pub realm: String,
    pub nonce: String,
    pub opaque: Option<String>,
    pub qop: Option<String>,
    pub algorithm: Option<String>,
}

struct Site {
    login: Login,
    digest: Option<Challenge>,
}

static SITES: Mutex<Option<HashMap<String, Site>>> = Mutex::new(None);
static NC: AtomicU32 = AtomicU32::new(0);

/// "host:port" of a link (the key sign-ins are kept under).
pub fn site_of(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    Some(format!("{}:{}", u.host_str()?.to_ascii_lowercase(), u.port_or_known_default()?))
}

/// Use this sign-in for `site` from now on (this session).
pub fn set(site: &str, login: Login) {
    SITES.lock().unwrap().get_or_insert_with(HashMap::new).insert(site.to_string(), Site { login, digest: None });
}

pub fn has(site: &str) -> bool {
    SITES.lock().unwrap().as_ref().is_some_and(|m| m.contains_key(site))
}

pub fn forget(site: &str) {
    if let Some(m) = SITES.lock().unwrap().as_mut() {
        m.remove(site);
    }
}

/// Note a server's challenge (from WWW-Authenticate), so the next requests answer it.
pub fn challenged(site: &str, www_authenticate: &str) {
    if let Some(c) = parse_digest(www_authenticate) {
        if let Some(s) = SITES.lock().unwrap().as_mut().and_then(|m| m.get_mut(site)) {
            s.digest = Some(c);
        }
    }
}

/// The Authorization header for a GET of `url`, if we have a sign-in for its site.
pub fn header_for(url: &str) -> Option<String> {
    let site = site_of(url)?;
    let guard = SITES.lock().unwrap();
    let s = guard.as_ref()?.get(&site)?;
    Some(match &s.digest {
        Some(c) => {
            let u = reqwest::Url::parse(url).ok()?;
            let uri = match u.query() {
                Some(q) => format!("{}?{q}", u.path()),
                None => u.path().to_string(),
            };
            let nc = NC.fetch_add(1, Ordering::Relaxed) + 1;
            let cnonce = format!("{:016x}", rand_u64());
            digest_header(&s.login, c, "GET", &uri, nc, &cnonce)
        }
        None => basic_header(&s.login),
    })
}

/// The scheme a 401 asks for, if we can answer it.
pub fn supported(www_authenticate: &str) -> bool {
    let l = www_authenticate.trim_start().to_ascii_lowercase();
    l.starts_with("basic") || l.starts_with("digest")
}

pub fn basic_header(l: &Login) -> String {
    format!("Basic {}", base64(format!("{}:{}", l.user, l.pass).as_bytes()))
}

pub fn parse_digest(h: &str) -> Option<Challenge> {
    let rest = h.trim_start();
    if !rest.to_ascii_lowercase().starts_with("digest") {
        return None;
    }
    let params = params(&rest[6..]);
    let get = |k: &str| params.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
    Some(Challenge { realm: get("realm").unwrap_or_default(), nonce: get("nonce")?, opaque: get("opaque"), qop: get("qop"), algorithm: get("algorithm") })
}

/// `a="x, y", b=z` to pairs (keys lowercased).
fn params(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
            chars.next();
        }
        let key: String = std::iter::from_fn(|| chars.next_if(|c| *c != '=' && *c != ',')).collect();
        if key.trim().is_empty() {
            break;
        }
        if chars.next() != Some('=') {
            continue;
        }
        let value = if chars.peek() == Some(&'"') {
            chars.next();
            let mut v = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => v.extend(chars.next()),
                    '"' => break,
                    c => v.push(c),
                }
            }
            v
        } else {
            std::iter::from_fn(|| chars.next_if(|c| *c != ',')).collect::<String>().trim().to_string()
        };
        out.push((key.trim().to_ascii_lowercase(), value));
    }
    out
}

fn hash(algo_sha256: bool, s: &str) -> String {
    if algo_sha256 { super::checksum::hex(&sha2::Sha256::digest(s.as_bytes())) } else { super::checksum::hex(&md5::Md5::digest(s.as_bytes())) }
}

/// RFC 7616 (and 2617) response for one request.
pub fn digest_header(l: &Login, c: &Challenge, method: &str, uri: &str, nc: u32, cnonce: &str) -> String {
    let algo = c.algorithm.clone().unwrap_or_else(|| "MD5".into());
    let upper = algo.to_ascii_uppercase();
    let sha = upper.starts_with("SHA-256");
    let mut ha1 = hash(sha, &format!("{}:{}:{}", l.user, c.realm, l.pass));
    if upper.ends_with("-SESS") {
        ha1 = hash(sha, &format!("{ha1}:{}:{cnonce}", c.nonce));
    }
    let ha2 = hash(sha, &format!("{method}:{uri}"));
    let auth_qop = c.qop.as_deref().is_some_and(|q| q.split(',').any(|x| x.trim() == "auth"));
    let nc = format!("{nc:08x}");
    let response = if auth_qop { hash(sha, &format!("{ha1}:{}:{nc}:{cnonce}:auth:{ha2}", c.nonce)) } else { hash(sha, &format!("{ha1}:{}:{ha2}", c.nonce)) };
    let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut h = format!(r#"Digest username="{}", realm="{}", nonce="{}", uri="{}", response="{response}""#, q(&l.user), q(&c.realm), q(&c.nonce), q(uri));
    if c.algorithm.is_some() {
        h += &format!(", algorithm={algo}");
    }
    if auth_qop {
        h += &format!(r#", qop=auth, nc={nc}, cnonce="{cnonce}""#);
    }
    if let Some(o) = &c.opaque {
        h += &format!(r#", opaque="{}""#, q(o));
    }
    h
}

fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    h.finish()
}

pub fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

// ---------- Windows Credential Manager ----------

fn target(site: &str) -> Vec<u16> {
    format!("Stash/{site}").encode_utf16().chain(std::iter::once(0)).collect()
}

/// Keep a sign-in in Credential Manager (per user, on this PC).
pub fn store(site: &str, l: &Login) -> bool {
    use windows::Win32::Security::Credentials::{CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredWriteW};
    let mut name = target(site);
    let mut user: Vec<u16> = l.user.encode_utf16().chain(std::iter::once(0)).collect();
    let blob: Vec<u8> = l.pass.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let cred = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: windows::core::PWSTR(name.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_ptr() as *mut u8,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: windows::core::PWSTR(user.as_mut_ptr()),
        ..Default::default()
    };
    unsafe { CredWriteW(&cred, 0) }.is_ok()
}

/// A sign-in kept in Credential Manager, if there is one.
pub fn load(site: &str) -> Option<Login> {
    use windows::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW};
    let name = target(site);
    let mut p: *mut CREDENTIALW = std::ptr::null_mut();
    unsafe {
        CredReadW(windows::core::PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &mut p).ok()?;
        let c = &*p;
        let user = c.UserName.to_string().unwrap_or_default();
        let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let wide: Vec<u16> = blob.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        let pass = String::from_utf16_lossy(&wide);
        CredFree(p as *const _);
        Some(Login { user, pass })
    }
}

pub fn delete(site: &str) {
    use windows::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CredDeleteW};
    let name = target(site);
    unsafe {
        let _ = CredDeleteW(windows::core::PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic() {
        assert_eq!(base64(b"Aladdin:open sesame"), "QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(basic_header(&Login { user: "Aladdin".into(), pass: "open sesame".into() }), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==");
    }

    /// The example in RFC 2617, section 3.5.
    #[test]
    fn digest_rfc2617() {
        let c = parse_digest(r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#).unwrap();
        assert_eq!(c.realm, "testrealm@host.com");
        assert_eq!(c.qop.as_deref(), Some("auth,auth-int"));
        let l = Login { user: "Mufasa".into(), pass: "Circle Of Life".into() };
        let h = digest_header(&l, &c, "GET", "/dir/index.html", 1, "0a4f113b");
        assert!(h.contains(r#"response="6629fae49393a05397450978507c4ef1""#), "{h}");
        assert!(h.contains("nc=00000001") && h.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#));
    }

    #[test]
    fn sites() {
        assert_eq!(site_of("https://Example.com/a").as_deref(), Some("example.com:443"));
        assert_eq!(site_of("http://x.test:8080/a").as_deref(), Some("x.test:8080"));
        set("only.test:443", Login { user: "u".into(), pass: "p".into() });
        assert!(header_for("https://only.test/file").is_some_and(|h| h.starts_with("Basic ")));
        assert!(header_for("https://other.test/file").is_none(), "a sign-in only goes to its own site");
        assert!(header_for("http://only.test/file").is_none(), "nor to another port");
        forget("only.test:443");
        assert!(supported("Basic realm=\"x\"") && supported("Digest realm=\"x\", nonce=\"y\"") && !supported("Bearer"));
    }
}
