//! Checking a finished file against the checksum its site published.

use std::io::Read;
use std::path::Path;

use sha2::Digest;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algo {
    Md5,
    Sha1,
    Sha256,
}

/// What was typed: a bare hex digest (its length says which kind) or one with a prefix
/// ("sha256:…", "SHA-1 …", "md5=…"). Returns the kind and the lowercase digest.
pub fn parse(s: &str) -> Option<(Algo, String)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    let (named, hex) = match lower.find([':', '=', ' ']) {
        Some(i) => (Some(lower[..i].replace(['-', '_'], "")), lower[i + 1..].trim().to_string()),
        None => (None, lower.clone()),
    };
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let by_len = match hex.len() {
        32 => Algo::Md5,
        40 => Algo::Sha1,
        64 => Algo::Sha256,
        _ => return None,
    };
    let algo = match named.as_deref() {
        None => by_len,
        Some("md5") => Algo::Md5,
        Some("sha1") => Algo::Sha1,
        Some("sha256") => Algo::Sha256,
        Some(_) => return None,
    };
    (algo == by_len).then_some((algo, hex))
}

/// The file's digest, as lowercase hex.
pub fn digest_file(path: &Path, algo: Algo) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut md5 = md5::Md5::new();
    let mut sha1 = sha1::Sha1::new();
    let mut sha256 = sha2::Sha256::new();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        match algo {
            Algo::Md5 => md5.update(&buf[..n]),
            Algo::Sha1 => sha1.update(&buf[..n]),
            Algo::Sha256 => sha256.update(&buf[..n]),
        }
    }
    let bytes: Vec<u8> = match algo {
        Algo::Md5 => md5.finalize().to_vec(),
        Algo::Sha1 => sha1.finalize().to_vec(),
        Algo::Sha256 => sha256.finalize().to_vec(),
    };
    Ok(hex(&bytes))
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Does the file match? Err when it can't be read.
pub fn verify(path: &Path, expected: &str) -> Result<bool, String> {
    let (algo, want) = parse(expected).ok_or("That checksum isn't MD5, SHA-1 or SHA-256")?;
    let got = digest_file(path, algo).map_err(|e| format!("Can't read the file: {e}"))?;
    Ok(got == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let sha = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855";
        assert_eq!(parse(sha), Some((Algo::Sha256, sha.to_ascii_lowercase())));
        assert_eq!(parse(&format!("sha256:{sha}")).unwrap().0, Algo::Sha256);
        assert_eq!(parse(&format!("SHA-256 {sha}")).unwrap().0, Algo::Sha256);
        assert_eq!(parse("md5=d41d8cd98f00b204e9800998ecf8427e").unwrap().0, Algo::Md5);
        assert_eq!(parse("da39a3ee5e6b4b0d3255bfef95601890afd80709").unwrap().0, Algo::Sha1);
        assert_eq!(parse("md5:da39a3ee5e6b4b0d3255bfef95601890afd80709"), None, "name and length disagree");
        assert_eq!(parse("xyz"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn verifies() {
        let dir = std::env::temp_dir().join("stash-checksum-test");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("abc.txt");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(digest_file(&f, Algo::Md5).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(digest_file(&f, Algo::Sha1).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(verify(&f, "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"), Ok(true));
        assert_eq!(verify(&f, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ae"), Ok(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
