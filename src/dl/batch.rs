//! Many downloads at once: numbered patterns (`photo[001-120].jpg`) and lists of links
//! read from a text file.

/// At most this many links from one pattern (a typo shouldn't queue a million).
pub const MAX_EXPAND: usize = 10_000;

/// A link with a numbered range in square brackets becomes one link per number:
/// `file[1-3].jpg` gives file1.jpg, file2.jpg, file3.jpg; `[001-120]` keeps the zeros;
/// `[a-e]` runs over letters; `[0-100:10]` counts in tens. Several ranges multiply.
/// Anything else (an IPv6 host like `[::1]`) is left as it is.
pub fn expand(link: &str) -> Vec<String> {
    let Some((start, end, items)) = first_range(link) else { return vec![link.to_string()] };
    let (head, tail) = (&link[..start], &link[end..]);
    let mut out = Vec::new();
    for item in items {
        for rest in expand(tail) {
            if out.len() >= MAX_EXPAND {
                return out;
            }
            out.push(format!("{head}{item}{rest}"));
        }
    }
    out
}

/// How many links `expand` makes, without making them.
pub fn count(link: &str) -> usize {
    match first_range(link) {
        Some((_, end, items)) => items.len().saturating_mul(count(&link[end..])).min(MAX_EXPAND),
        None => 1,
    }
}

/// Every link in `links`, patterns expanded, duplicates dropped (order kept).
pub fn expand_all<'a>(links: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for l in links {
        for e in expand(l) {
            if seen.insert(e.clone()) {
                out.push(e);
            }
            if out.len() >= MAX_EXPAND {
                return out;
            }
        }
    }
    out
}

/// The first `[a-b]` (or `[a-b:step]`) range: its byte span and what it stands for.
fn first_range(s: &str) -> Option<(usize, usize, Vec<String>)> {
    let mut from = 0;
    while let Some(open) = s[from..].find('[').map(|i| i + from) {
        let Some(close) = s[open..].find(']').map(|i| i + open) else { return None };
        if let Some(items) = range_items(&s[open + 1..close]) {
            return Some((open, close + 1, items));
        }
        from = open + 1;
    }
    None
}

fn range_items(inner: &str) -> Option<Vec<String>> {
    let (range, step) = match inner.split_once(':') {
        Some((r, st)) => (r, st.parse::<u64>().ok().filter(|s| *s > 0)?),
        None => (inner, 1),
    };
    let (a, b) = range.split_once('-')?;
    if a.is_empty() || b.is_empty() {
        return None;
    }
    if a.bytes().all(|c| c.is_ascii_digit()) && b.bytes().all(|c| c.is_ascii_digit()) {
        let (x, y) = (a.parse::<u64>().ok()?, b.parse::<u64>().ok()?);
        // zero-padded when the first number is written with leading zeros
        let width = if a.len() > 1 && a.starts_with('0') { a.len() } else { 0 };
        let fmt = |n: u64| format!("{n:0width$}");
        let mut v = Vec::new();
        if x <= y {
            let mut n = x;
            while n <= y && v.len() < MAX_EXPAND {
                v.push(fmt(n));
                n += step;
            }
        } else {
            let mut n = x;
            loop {
                v.push(fmt(n));
                if n < y + step || v.len() >= MAX_EXPAND {
                    break;
                }
                n -= step;
            }
        }
        return Some(v);
    }
    let (ca, cb) = (single_letter(a)?, single_letter(b)?);
    if ca.is_ascii_lowercase() != cb.is_ascii_lowercase() || ca > cb {
        return None;
    }
    Some((ca..=cb).step_by(step as usize).map(String::from).collect())
}

fn single_letter(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next().filter(|c| c.is_ascii_alphabetic())?;
    it.next().is_none().then_some(c)
}

/// The links in a text file: one or more per line, `#` starts a comment.
pub fn links_in_text(text: &str) -> Vec<String> {
    text.lines()
        // a comment runs to the end of the line ("#" inside a link is its fragment)
        .flat_map(|l| l.split_whitespace().take_while(|w| !w.starts_with('#')))
        .map(|w| w.trim_matches(|c| matches!(c, '"' | '\'' | '<' | '>' | ',')).to_string())
        .filter(|w| super::is_source(w))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(expand("https://x.net/f[1-3].jpg"), ["https://x.net/f1.jpg", "https://x.net/f2.jpg", "https://x.net/f3.jpg"]);
        let v = expand("https://x.net/file[001-120].jpg");
        assert_eq!(v.len(), 120);
        assert_eq!(v[0], "https://x.net/file001.jpg");
        assert_eq!(v[119], "https://x.net/file120.jpg");
        assert_eq!(expand("a[0-20:10]"), ["a0", "a10", "a20"]);
        assert_eq!(expand("a[3-1]"), ["a3", "a2", "a1"]);
        assert_eq!(count("https://x.net/file[001-120].jpg"), 120);
    }

    #[test]
    fn letters_and_several() {
        assert_eq!(expand("p[a-c]"), ["pa", "pb", "pc"]);
        assert_eq!(expand("s[1-2]e[1-2]"), ["s1e1", "s1e2", "s2e1", "s2e2"]);
        assert_eq!(count("s[1-10]e[01-12]"), 120);
        assert_eq!(expand("x[a-C]"), ["x[a-C]"]);
    }

    #[test]
    fn leaves_other_brackets() {
        assert_eq!(expand("http://[::1]:8080/a.zip"), ["http://[::1]:8080/a.zip"]);
        assert_eq!(expand("http://[::1]:8080/a[1-2].zip"), ["http://[::1]:8080/a1.zip", "http://[::1]:8080/a2.zip"]);
        assert_eq!(expand("no[range"), ["no[range"]);
        assert_eq!(expand("a[1-]"), ["a[1-]"]);
    }

    #[test]
    fn caps_runaway_patterns() {
        assert_eq!(expand("a[1-999999]").len(), MAX_EXPAND);
        assert_eq!(count("a[1-999999]b[1-999999]"), MAX_EXPAND);
    }

    #[test]
    fn text_files() {
        let t = "# my list\nhttps://a.net/1.zip\n\n  https://a.net/2.zip  https://a.net/3.zip # third\nnot a link\nmagnet:?xt=urn:btih:abc\n";
        assert_eq!(links_in_text(t), ["https://a.net/1.zip", "https://a.net/2.zip", "https://a.net/3.zip", "magnet:?xt=urn:btih:abc"]);
        assert_eq!(expand_all(["a[1-2]", "a1", "b"]), ["a1", "a2", "b"]);
    }
}
