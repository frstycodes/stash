//! Which failures are worth trying again, and how long to wait before each try.

use std::time::Duration;

/// Tries before a download is left failed (about an hour of waiting in all).
pub const MAX_TRIES: u32 = 12;

/// A server status that may well work a little later: busy, overloaded, or a gateway
/// that lost its way. 4xx other than "too many requests" and timeouts is the link's
/// fault, and won't fix itself.
pub fn transient_status(code: u16) -> bool {
    matches!(code, 408 | 425 | 429) || (code >= 500 && !matches!(code, 501 | 505 | 506 | 508 | 510 | 511))
}

/// How long to wait before try `n` (1-based): 5 s, 10 s, 20 s ... up to 5 minutes,
/// or what the server asked for (Retry-After), within reason.
pub fn backoff(n: u32, retry_after: Option<u64>) -> Duration {
    if let Some(s) = retry_after {
        return Duration::from_secs(s.clamp(1, 15 * 60));
    }
    let secs = 5u64.saturating_mul(1 << n.saturating_sub(1).min(10));
    Duration::from_secs(secs.min(300))
}

/// Retry-After as seconds ("120"); an HTTP date counts as a minute.
pub fn parse_retry_after(v: &str) -> Option<u64> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    v.parse().ok().or(Some(60))
}

/// How a download failed, for the engine to decide what's next.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Failure {
    pub msg: String,
    /// Worth trying again later (network, timeouts, a busy server).
    pub transient: bool,
    /// The server asked to wait this long (seconds).
    pub retry_after: Option<u64>,
    /// The server wants a sign-in (401): its WWW-Authenticate challenge.
    pub auth: Option<String>,
    /// The file on the server is no longer the one we started (resume refused).
    pub changed: bool,
}

impl Failure {
    pub fn fatal(msg: impl Into<String>) -> Self {
        Failure { msg: msg.into(), ..Default::default() }
    }
    pub fn transient(msg: impl Into<String>) -> Self {
        Failure { msg: msg.into(), transient: true, ..Default::default() }
    }
    /// From a server's status.
    pub fn status(code: u16, text: String, retry_after: Option<u64>) -> Self {
        Failure { msg: format!("The server answered {text}"), transient: transient_status(code), retry_after, ..Default::default() }
    }
}

impl From<String> for Failure {
    fn from(msg: String) -> Self {
        Failure::fatal(msg)
    }
}

impl From<&str> for Failure {
    fn from(msg: &str) -> Self {
        Failure::fatal(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies() {
        for c in [408, 429, 500, 502, 503, 504, 520, 522] {
            assert!(transient_status(c), "{c}");
        }
        for c in [400, 401, 403, 404, 410, 416, 501, 505, 511] {
            assert!(!transient_status(c), "{c}");
        }
        assert!(Failure::status(503, "503".into(), None).transient);
        assert!(!Failure::status(404, "404".into(), None).transient);
        assert!(!Failure::from("disk full").transient);
    }

    #[test]
    fn backs_off() {
        assert_eq!(backoff(1, None), Duration::from_secs(5));
        assert_eq!(backoff(2, None), Duration::from_secs(10));
        assert_eq!(backoff(4, None), Duration::from_secs(40));
        assert_eq!(backoff(9, None), Duration::from_secs(300));
        assert_eq!(backoff(40, None), Duration::from_secs(300));
        assert_eq!(backoff(3, Some(7)), Duration::from_secs(7));
        assert_eq!(backoff(3, Some(99_999)), Duration::from_secs(900));
        assert_eq!(parse_retry_after("120"), Some(120));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), Some(60));
        assert_eq!(parse_retry_after(""), None);
    }
}
