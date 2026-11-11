//! Searching the downloads list: loose (fuzzy) matching on names, sites and links,
//! `key:value` filters, and step-by-step completions for those filters.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    Video,
    Audio,
    Archive,
    Document,
    Image,
    App,
    Other,
}

pub const TYPES: [(FileType, &str, &str); 6] = [
    (FileType::Video, "video", "Videos and movies"),
    (FileType::Audio, "audio", "Music and podcasts"),
    (FileType::Archive, "archive", "Zip, 7z, rar and the like"),
    (FileType::Document, "document", "PDFs, docs and sheets"),
    (FileType::Image, "image", "Photos and pictures"),
    (FileType::App, "app", "Installers and programs"),
];

pub fn file_type(name: &str) -> FileType {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "m4v" | "wmv" | "flv" | "ts" => FileType::Video,
        "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "opus" | "wma" => FileType::Audio,
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "zst" | "iso" => FileType::Archive,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "csv" | "epub" | "odt" | "rtf" => FileType::Document,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "avif" | "bmp" | "svg" | "tif" | "tiff" | "raw" => FileType::Image,
        "exe" | "msi" | "msix" | "appx" | "apk" | "dmg" | "deb" | "rpm" | "appimage" => FileType::App,
        _ => FileType::Other,
    }
}

/// The site a link came from ("github.com"), without "www.".
pub fn site(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or("");
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = host.split(':').next().unwrap_or("");
    host.trim_start_matches("www.").to_ascii_lowercase()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum When {
    Today,
    Yesterday,
    Week,
    Month,
    Older,
}

impl When {
    pub fn from_age_days(days: i64) -> When {
        match days {
            ..=0 => When::Today,
            1 => When::Yesterday,
            2..=6 => When::Week,
            7..=30 => When::Month,
            _ => When::Older,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            When::Today => "Today",
            When::Yesterday => "Yesterday",
            When::Week => "Earlier this week",
            When::Month => "Last month",
            When::Older => "Older",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Is {
    Missing,
    Failed,
    Paused,
    Downloading,
    Done,
    Torrent,
}

const IS: [(Is, &str, &str); 6] = [
    (Is::Missing, "missing", "Finished, but the file was moved or deleted"),
    (Is::Failed, "failed", "Stopped with an error"),
    (Is::Paused, "paused", "Paused or waiting"),
    (Is::Downloading, "downloading", "Downloading right now"),
    (Is::Done, "done", "Finished"),
    (Is::Torrent, "torrent", "Torrents and magnet links"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    pub key: &'static str,
    pub label: String,
    /// The text in the query that made it, to remove it again.
    pub raw: String,
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub words: Vec<String>,
    pub site: Option<String>,
    pub kind: Option<FileType>,
    pub is: Option<Is>,
    pub min: Option<u64>,
    pub max: Option<u64>,
    pub when: Option<When>,
    pub chips: Vec<Chip>,
}


fn size_bytes(n: f64, unit: &str) -> u64 {
    let k = match unit {
        "kb" | "k" => 1e3,
        "gb" | "g" => 1e9,
        "tb" | "t" => 1e12,
        _ => 1e6,
    };
    (n * k) as u64
}

pub fn parse(q: &str) -> Query {
    let mut f = Query::default();
    for raw in q.split_whitespace() {
        let t = raw.to_lowercase();
        // a filter still being typed ("type:", ">", "si") narrows nothing yet
        if matches!(t.as_str(), ">" | "<" | ">=" | "<=") || (t.ends_with(':') && ["type:", "site:", "from:", "is:"].contains(&t.as_str())) {
            continue;
        }
        let chip = |key: &'static str, label: String| Chip { key, label, raw: raw.to_string() };
        if let Some(v) = t.strip_prefix("site:").or_else(|| t.strip_prefix("from:")).filter(|v| !v.is_empty()) {
            f.chips.push(chip("site", v.to_string()));
            f.site = Some(v.to_string());
        } else if let Some((kind, name, _)) = t.strip_prefix("type:").filter(|v| !v.is_empty()).and_then(|v| TYPES.iter().find(|(_, n, _)| n.starts_with(v))) {
            f.kind = Some(*kind);
            f.chips.push(chip("type", name.to_string()));
        } else if let Some((is, name, _)) = t.strip_prefix("is:").filter(|v| !v.is_empty()).and_then(|v| IS.iter().find(|(_, n, _)| n.starts_with(v))) {
            f.is = Some(*is);
            f.chips.push(chip("is", name.to_string()));
        } else if let Some((op, n, unit)) = parse_size(&t) {
            let bytes = size_bytes(n, unit);
            let label = format!("{} {}{}", if op == '>' { "over" } else { "under" }, trim_num(n), if unit.is_empty() { "MB".into() } else { unit.to_uppercase() });
            if op == '>' {
                f.min = Some(bytes);
            } else {
                f.max = Some(bytes);
            }
            f.chips.push(chip("size", label));
        } else if let Some(w) = match t.as_str() {
            "today" => Some(When::Today),
            "yesterday" => Some(When::Yesterday),
            "week" => Some(When::Week),
            "month" => Some(When::Month),
            _ => None,
        } {
            f.when = Some(w);
            f.chips.push(chip("when", t.clone()));
        } else {
            f.words.push(t);
        }
    }
    f
}

fn trim_num(n: f64) -> String {
    if n.fract() == 0.0 { format!("{}", n as u64) } else { format!("{n}") }
}

/// ">100mb", "<1.5gb", ">20" (MB)
fn parse_size(t: &str) -> Option<(char, f64, &str)> {
    let op = t.chars().next().filter(|c| *c == '>' || *c == '<')?;
    let rest = t[1..].trim_start_matches('=');
    let split = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    let (num, unit) = rest.split_at(split);
    let n: f64 = num.parse().ok()?;
    matches!(unit, "" | "k" | "kb" | "m" | "mb" | "g" | "gb" | "t" | "tb").then_some((op, n, unit))
}

// ---------- matching ----------

/// What a download looks like to search.
pub struct Item<'a> {
    pub name: &'a str,
    pub site: &'a str,
    pub url: &'a str,
    pub kind: FileType,
    pub size: Option<u64>,
    pub when: When,
    pub missing: bool,
    pub failed: bool,
    pub paused: bool,
    pub downloading: bool,
    pub done: bool,
    pub torrent: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Match {
    pub score: f32,
    /// Character indices in the name that matched, for highlighting.
    pub pos: Vec<usize>,
}

fn is_sep(c: char) -> bool {
    c.is_whitespace() || "._-/:?=&()[]+,".contains(c)
}

/// The letters of `word` in order within `text`, preferring runs and word starts.
fn subseq(word: &[char], text: &[char]) -> Option<Match> {
    let mut pos = Vec::with_capacity(word.len());
    let (mut score, mut k, mut prev) = (0.0f32, 0usize, usize::MAX);
    for &w in word {
        let mut found = None;
        for x in k..text.len() {
            if text[x] != w {
                continue;
            }
            let start = x == 0 || is_sep(text[x - 1]);
            if found.is_none() {
                found = Some(x);
            }
            if start || (prev != usize::MAX && x == prev + 1) {
                found = Some(x);
                break;
            }
        }
        let x = found?;
        let start = x == 0 || is_sep(text[x - 1]);
        let run = prev != usize::MAX && x == prev + 1;
        score += if run { 6.0 } else if start { 5.0 } else { 1.0 };
        let gap = if prev == usize::MAX { 0 } else { x - prev - 1 };
        score -= (gap as f32 * 0.05).min(4.0);
        pos.push(x);
        prev = x;
        k = x + 1;
    }
    Some(Match { score, pos })
}

fn edits(a: &[char], b: &[char], max: usize) -> usize {
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut next = vec![i; b.len() + 1];
        let mut best = i;
        for j in 1..=b.len() {
            next[j] = (row[j] + 1).min(next[j - 1] + 1).min(row[j - 1] + usize::from(a[i - 1] != b[j - 1]));
            best = best.min(next[j]);
        }
        if best > max {
            return max + 1;
        }
        row = next;
    }
    row[b.len()]
}

/// Words of `text` with their starting character index.
fn tokens(text: &[char]) -> Vec<(usize, &[char])> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.iter().enumerate() {
        match (is_sep(*c), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                out.push((s, &text[s..i]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, &text[s..]));
    }
    out
}

/// One typed word against one text: exact substring, an abbreviation of one word
/// ("rprt" -> report), letters in order ("bbb" -> Big Buck Bunny), or a typo or two
/// ("blendr" -> blender).
pub fn match_word(word: &str, text: &str) -> Option<Match> {
    let w: Vec<char> = word.chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    if w.is_empty() {
        return Some(Match::default());
    }
    if let Some(at) = t.windows(w.len()).position(|win| win == w.as_slice()) {
        return Some(Match { score: 100.0 + w.len() as f32 * 3.0 - at as f32 * 0.01, pos: (at..at + w.len()).collect() });
    }
    if w.len() >= 2 {
        for (i, tok) in tokens(&t) {
            if tok.first() != w.first() || tok.len() < w.len() {
                continue;
            }
            if let Some(m) = subseq(&w, tok) {
                return Some(Match { score: 60.0 + w.len() as f32 * 4.0 - (tok.len() - w.len()) as f32, pos: m.pos.iter().map(|p| p + i).collect() });
            }
        }
        if let Some(m) = subseq(&w, &t) {
            if m.score >= w.len() as f32 * 2.2 {
                return Some(m);
            }
        }
    }
    if w.len() >= 4 {
        let max = if w.len() >= 7 { 2 } else { 1 };
        for (i, tok) in tokens(&t) {
            let target = if tok.len() > w.len() + max { &tok[..w.len() + 1] } else { tok };
            let d = edits(&w, target, max);
            if d <= max {
                return Some(Match { score: 40.0 - d as f32 * 10.0, pos: (i..i + tok.len().min(w.len())).collect() });
            }
        }
    }
    None
}

/// The whole query against one download. Every word must match its name, site or link.
pub fn matches(item: &Item, q: &Query) -> Option<Match> {
    if q.site.as_ref().is_some_and(|s| !item.site.contains(s.as_str())) {
        return None;
    }
    if q.kind.is_some_and(|k| k != item.kind) {
        return None;
    }
    if let Some(min) = q.min {
        if item.size.is_none_or(|s| s < min) {
            return None;
        }
    }
    if let Some(max) = q.max {
        if item.size.is_none_or(|s| s > max) {
            return None;
        }
    }
    if q.when.is_some_and(|w| item.when > w) {
        return None;
    }
    if let Some(is) = q.is {
        let ok = match is {
            Is::Missing => item.missing,
            Is::Failed => item.failed,
            Is::Paused => item.paused,
            Is::Downloading => item.downloading,
            Is::Done => item.done,
            Is::Torrent => item.torrent,
        };
        if !ok {
            return None;
        }
    }
    let rest = format!("{} {}", item.site, item.url);
    let mut total = Match::default();
    for w in &q.words {
        if let Some(m) = match_word(w, item.name) {
            total.score += m.score * 1.5;
            total.pos.extend(m.pos);
        } else {
            total.score += match_word(w, &rest)?.score;
        }
    }
    total.pos.sort_unstable();
    total.pos.dedup();
    Some(total)
}

// ---------- completions ----------

#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub text: String,
    pub desc: String,
    /// A finished filter (a space follows); otherwise more is expected ("type:").
    pub done: bool,
}

const KEYS: [(&str, &str); 9] = [
    ("type:", "Kind of file: video, audio, archive, document, image or app"),
    ("site:", "The site it came from, like github.com"),
    ("is:", "Its state: missing, failed, paused, downloading, done or torrent"),
    (">", "Bigger than a size, like >100mb"),
    ("<", "Smaller than a size, like <1gb"),
    ("today", "Downloaded today"),
    ("yesterday", "Downloaded yesterday or today"),
    ("week", "Downloaded in the last 7 days"),
    ("month", "Downloaded in the last 30 days"),
];
const SIZES: [&str; 5] = ["10mb", "100mb", "500mb", "1gb", "5gb"];

/// What can follow the word being typed. `count_type` and `sites` give live counts.
pub fn complete(token: &str, count_type: impl Fn(FileType) -> usize, sites: &[(String, usize)]) -> Vec<Completion> {
    let t = token.to_lowercase();
    let plural = |n: usize| format!("{n} download{}", if n == 1 { "" } else { "s" });
    if let Some(v) = t.strip_prefix("type:") {
        return TYPES
            .iter()
            .filter(|(_, n, _)| n.starts_with(v))
            .map(|(k, n, d)| Completion { text: format!("type:{n}"), desc: format!("{d} · {}", plural(count_type(*k))), done: true })
            .collect();
    }
    if let Some(v) = t.strip_prefix("site:").or_else(|| t.strip_prefix("from:")) {
        return sites
            .iter()
            .filter(|(s, _)| s.contains(v))
            .take(8)
            .map(|(s, n)| Completion { text: format!("site:{s}"), desc: plural(*n), done: true })
            .collect();
    }
    if let Some(v) = t.strip_prefix("is:") {
        return IS.iter().filter(|(_, n, _)| n.starts_with(v)).map(|(_, n, d)| Completion { text: format!("is:{n}"), desc: d.to_string(), done: true }).collect();
    }
    if let Some(op) = t.chars().next().filter(|c| *c == '>' || *c == '<') {
        let typed = &t[1..];
        let bigger = op == '>';
        return SIZES
            .iter()
            .filter(|s| s.starts_with(typed))
            .map(|s| Completion { text: format!("{op}{s}"), desc: format!("{} than {}", if bigger { "Bigger" } else { "Smaller" }, s.to_uppercase()), done: true })
            .collect();
    }
    KEYS.iter()
        .filter(|(k, _)| k.starts_with(&t) && *k != t)
        .map(|(k, d)| Completion { text: k.to_string(), desc: d.to_string(), done: !k.ends_with(':') && k.len() > 1 })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str) -> Item<'_> {
        Item {
            name,
            site: "example.com",
            url: "https://example.com/x",
            kind: file_type(name),
            size: Some(5_000_000),
            when: When::Today,
            missing: false,
            failed: false,
            paused: false,
            downloading: false,
            done: true,
            torrent: false,
        }
    }

    #[test]
    fn fuzzy() {
        let hit = |q: &str, name: &str| matches(&item(name), &parse(q)).is_some();
        assert!(hit("bbb", "Big Buck Bunny"));
        assert!(hit("blendr", "blender-4.2.0-windows-x64.zip"));
        assert!(hit("rprt fnl", "Quarterly report final v3.pdf"));
        assert!(hit("vacaton", "vacation-photos.zip"));
        assert!(hit("lease pdf", "lease-agreement-signed.pdf"));
        assert!(hit("example", "anything.bin")); // via the site
        assert!(!hit("zzz", "Big Buck Bunny"));
        assert!(!hit("xyz", "node-v24.4.0-x64.msi"));
    }

    #[test]
    fn filters() {
        let q = parse("type:vid >1mb is:done today report");
        assert_eq!(q.kind, Some(FileType::Video));
        assert_eq!(q.min, Some(1_000_000));
        assert_eq!(q.is, Some(Is::Done));
        assert_eq!(q.when, Some(When::Today));
        assert_eq!(q.words, vec!["report"]);
        assert_eq!(q.chips.len(), 4);
        assert!(parse("type: >").words.is_empty());
        assert!(matches(&item("movie.mp4"), &parse("type:video >1mb")).is_some());
        assert!(matches(&item("movie.mp4"), &parse("<1mb")).is_none());
    }

    #[test]
    fn completions() {
        let none = |_| 0;
        assert_eq!(complete("ty", none, &[])[0].text, "type:");
        assert_eq!(complete("type:", none, &[]).len(), 6);
        assert_eq!(complete("type:a", none, &[]).iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["type:audio", "type:archive", "type:app"]);
        assert_eq!(complete("site:git", none, &[("github.com".into(), 3), ("gitlab.com".into(), 1), ("nodejs.org".into(), 1)]).len(), 2);
        assert_eq!(complete(">1", none, &[]).iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), [">10mb", ">100mb", ">1gb"]);
        assert!(complete("", none, &[]).len() >= 9);
    }

    #[test]
    fn helpers() {
        assert_eq!(site("https://www.GitHub.com/rsms/inter"), "github.com");
        assert_eq!(site("http://user@host.net:8080/x"), "host.net");
        assert_eq!(site("magnet:?xt=urn:btih:abc"), "");
        assert_eq!(When::from_age_days(3), When::Week);
    }
}
