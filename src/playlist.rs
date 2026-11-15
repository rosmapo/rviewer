//! Zoznam súborov: autoload adresára, triedenie (ako autoload-ex + mpv_sort_script).

use crate::config::{Config, SortKey};
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: Kind,
    /// Názov z playlistu (#EXTINF / Title v .pls).
    pub title: Option<String>,
    /// HTTP hlavičky z playlistu (http-referrer, http-user-agent).
    pub referrer: Option<String>,
    pub user_agent: Option<String>,
}

impl Entry {
    pub fn new(path: PathBuf, kind: Kind) -> Entry {
        Entry { path, kind, title: None, referrer: None, user_agent: None }
    }

    /// Text do zoznamu a titulku okna: názov z playlistu, inak názov súboru alebo celá adresa.
    pub fn display_name(&self) -> String {
        if let Some(t) = &self.title {
            return t.clone();
        }
        let s = self.path.to_string_lossy();
        if is_url(&s) {
            return s.into_owned();
        }
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| s.into_owned())
    }
}

pub fn classify(path: &Path, cfg: &Config) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    if cfg.image_ext.iter().any(|e| *e == ext) {
        Some(Kind::Image)
    } else if cfg.video_ext.iter().any(|e| *e == ext) {
        Some(Kind::Video)
    } else {
        None
    }
}

/// Prirodzené porovnanie: "img2" < "img10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                let mut nb = String::new();
                while let Some(&c) = ai.peek() {
                    if c.is_ascii_digit() { na.push(c); ai.next(); } else { break; }
                }
                while let Some(&c) = bi.peek() {
                    if c.is_ascii_digit() { nb.push(c); bi.next(); } else { break; }
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

struct Item {
    entry: Entry,
    name: String,
    mtime: SystemTime,
    size: u64,
}

fn list_dir(dir: &Path, cfg: &Config) -> Vec<Entry> {
    let mut items = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if cfg.ignore_hidden && name.starts_with('.') {
                continue;
            }
            let Ok(meta) = fs::metadata(&path) else { continue };
            if !meta.is_file() {
                continue;
            }
            let Some(kind) = classify(&path, cfg) else { continue };
            items.push(Item {
                entry: Entry::new(path, kind),
                name,
                mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                size: meta.len(),
            });
        }
    }
    items.sort_by(|a, b| {
        let o = match cfg.sort {
            SortKey::Date => a.mtime.cmp(&b.mtime),
            SortKey::Size => a.size.cmp(&b.size),
            _ => Ordering::Equal,
        }
        .then_with(|| natural_cmp(&a.name, &b.name));
        if cfg.sort_desc { o.reverse() } else { o }
    });
    items.into_iter().map(|i| i.entry).collect()
}

fn absolute(p: &str) -> PathBuf {
    let p = PathBuf::from(p);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    }
}

fn file_entry(p: PathBuf, cfg: &Config) -> Entry {
    // Neznáma prípona: nech rozhodne GStreamer.
    let kind = classify(&p, cfg).unwrap_or(Kind::Video);
    Entry::new(p, kind)
}

/// Sieťová adresa (http://, https://, rtsp://...), nie lokálny súbor.
pub fn is_url(s: &str) -> bool {
    match s.split_once("://") {
        Some((scheme, rest)) => {
            scheme.len() >= 2
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
                && !rest.is_empty()
                && !scheme.eq_ignore_ascii_case("file")
        }
        None => false,
    }
}

/// Hodnota atribútu `key="..."` z riadku #EXTINF.
fn extinf_attr(s: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = s.find(&pat)? + pat.len();
    let j = s[i..].find('"')?;
    Some(s[i..i + j].to_string()).filter(|v| !v.is_empty())
}

/// Názov z #EXTINF: text za prvou čiarkou mimo úvodzoviek, inak `tvg-name`.
fn extinf_title(s: &str) -> Option<String> {
    let mut in_q = false;
    for (i, c) in s.char_indices() {
        match c {
            '"' => in_q = !in_q,
            ',' if !in_q => {
                let t = s[i + 1..].trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
                break;
            }
            _ => {}
        }
    }
    extinf_attr(s, "tvg-name")
}

fn playlist_entry(target: &str, base: &Path, cfg: &Config) -> Entry {
    if is_url(target) {
        return Entry::new(PathBuf::from(target), Kind::Video);
    }
    let p = PathBuf::from(target);
    let p = if p.is_absolute() { p } else { base.join(p) };
    file_entry(p, cfg)
}

/// Text playlistu .m3u (aj rozšíreného) alebo .pls; relatívne cesty sa berú od `base`.
pub fn parse_playlist(text: &str, is_pls: bool, base: &Path, cfg: &Config) -> Vec<Entry> {
    let mut out = Vec::new();
    if is_pls {
        let mut files: Vec<(u32, String)> = Vec::new();
        let mut titles = std::collections::HashMap::new();
        for line in text.lines().map(str::trim) {
            let Some((k, v)) = line.split_once('=') else { continue };
            let k = k.trim().to_ascii_lowercase();
            if let Some(n) = k.strip_prefix("file").and_then(|n| n.parse::<u32>().ok()) {
                files.push((n, v.trim().to_string()));
            } else if let Some(n) = k.strip_prefix("title").and_then(|n| n.parse::<u32>().ok()) {
                titles.insert(n, v.trim().to_string());
            }
        }
        files.sort_by_key(|(n, _)| *n);
        for (n, f) in files {
            let mut e = playlist_entry(&f, base, cfg);
            e.title = titles.get(&n).cloned().filter(|t| !t.is_empty());
            out.push(e);
        }
        return out;
    }
    let (mut title, mut referrer, mut agent) = (None, None, None);
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            title = extinf_title(rest);
            referrer = extinf_attr(rest, "http-referrer");
            agent = extinf_attr(rest, "http-user-agent");
        } else if let Some(rest) = line.strip_prefix("#EXTVLCOPT:") {
            if let Some(v) = rest.strip_prefix("http-referrer=") {
                referrer = Some(v.trim().to_string());
            } else if let Some(v) = rest.strip_prefix("http-user-agent=") {
                agent = Some(v.trim().to_string());
            }
        } else if line.starts_with('#') {
            // komentár alebo nepodporovaná direktíva
        } else {
            let mut e = playlist_entry(line, base, cfg);
            e.title = title.take();
            e.referrer = referrer.take();
            e.user_agent = agent.take();
            out.push(e);
        }
    }
    out
}

/// Načíta playlist zo súboru .m3u / .m3u8 / .pls. `None`, ak to playlist nie je (aj lokálny HLS súbor s #EXT-X-).
pub fn load_playlist(path: &Path, cfg: &Config) -> Option<Vec<Entry>> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(ext.as_str(), "m3u" | "m3u8" | "pls") {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    if text.contains("#EXT-X-") {
        return None;
    }
    let base = path.parent().unwrap_or(Path::new("/"));
    let list = parse_playlist(&text, ext == "pls", base, cfg);
    if list.is_empty() { None } else { Some(list) }
}

/// Vráti zoznam a index súboru, od ktorého sa začína.
pub fn build(args: &[String], cfg: &Config) -> (Vec<Entry>, usize) {
    let mut out = Vec::new();
    let mut start = 0;
    if args.is_empty() {
        out = list_dir(&absolute("."), cfg);
    } else if args.len() == 1 && !is_url(&args[0]) {
        let p = absolute(&args[0]);
        if let Some(list) = load_playlist(&p, cfg) {
            out = list;
        } else if p.is_dir() {
            out = list_dir(&p, cfg);
        } else if cfg.autoload {
            let dir = p.parent().unwrap_or(Path::new("/")).to_path_buf();
            out = list_dir(&dir, cfg);
            match out.iter().position(|e| e.path.file_name() == p.file_name()) {
                Some(i) => start = i,
                None => out.insert(0, file_entry(p, cfg)),
            }
        } else {
            out.push(file_entry(p, cfg));
        }
    } else {
        for a in args {
            if is_url(a) {
                out.push(Entry::new(PathBuf::from(a), Kind::Video));
                continue;
            }
            let p = absolute(a);
            if let Some(list) = load_playlist(&p, cfg) {
                out.extend(list);
            } else if p.is_dir() {
                out.extend(list_dir(&p, cfg));
            } else {
                out.push(file_entry(p, cfg));
            }
        }
    }
    (out, start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_playlist() {
        let text = "#EXTM3U\n# STVR\n#EXTINF:-1 tvg-name=\"Jednotka FHD\" group-title=\"STVR\",Jednotka FHD\nhttp://88.212.15.19/live/j/playlist.m3u8\n\n\
#EXTINF:-1,Markíza HD\nhttps://cdn/Markiza/index.m3u8\n\
#EXTINF:-1 tvg-name=\"JOJ\" http-referrer=\"https://media.joj.sk/\",JOJ, live\n#EXTVLCOPT:http-user-agent=UA/1\nhttp://88.212.15.27/live/joj.m3u8\n\
#EXTINF:-1,Bez adresy\n\n#EXTINF:-1 tvg-name=\"TA3\" group-title=\"x\",\nhttps://x/get.php?x=TA3\n/tmp/a.mp3\nrel/b.ogg\n";
        let l = parse_playlist(text, false, Path::new("/base"), &Config::default());
        assert_eq!(l.len(), 6);
        assert_eq!(l[0].title.as_deref(), Some("Jednotka FHD"));
        assert_eq!(l[1].title.as_deref(), Some("Markíza HD"));
        assert_eq!(l[2].title.as_deref(), Some("JOJ, live"));
        assert_eq!(l[2].referrer.as_deref(), Some("https://media.joj.sk/"));
        assert_eq!(l[2].user_agent.as_deref(), Some("UA/1"));
        // EXTINF bez adresy sa nepreniesie na ďalšiu položku; prázdny názov -> tvg-name
        assert_eq!(l[3].title.as_deref(), Some("TA3"));
        assert_eq!(l[3].referrer, None);
        assert_eq!(l[4].path, PathBuf::from("/tmp/a.mp3"));
        assert_eq!(l[5].path, PathBuf::from("/base/rel/b.ogg"));
        assert_eq!(l[5].display_name(), "b.ogg");
        assert_eq!(l[0].kind, Kind::Video);
    }

    #[test]
    fn pls_playlist() {
        let t = "[playlist]\nFile2=http://b/2\nTitle2=Dva\nFile1=http://a/1\nNumberOfEntries=2\n";
        let l = parse_playlist(t, true, Path::new("/"), &Config::default());
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].path, PathBuf::from("http://a/1"));
        assert_eq!(l[1].title.as_deref(), Some("Dva"));
    }

    #[test]
    fn urls() {
        assert!(is_url("https://example.com/a.mp4"));
        assert!(is_url("rtsp://host/stream"));
        assert!(!is_url("file:///tmp/a.mp4"));
        assert!(!is_url("/home/a/b.mp4"));
        assert!(!is_url("a://"));
        let (l, s) = build(&["https://example.com/v".to_string()], &Config::default());
        assert_eq!((l.len(), s, l[0].kind), (1, 0, Kind::Video));
    }

    #[test]
    fn natural() {
        assert_eq!(natural_cmp("img2.jpg", "img10.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("a01", "a1"), Ordering::Equal);
        assert_eq!(natural_cmp("B", "a"), Ordering::Greater);
    }

    #[test]
    fn autoload_and_sort() {
        let dir = std::env::temp_dir().join(format!("rviewer-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for n in ["b10.jpg", "b2.png", ".hidden.jpg", "x.txt", "c.mp4"] {
            fs::write(dir.join(n), b"x").unwrap();
        }
        let mut cfg = Config::default();
        cfg.sort = SortKey::Name;
        let (list, start) = build(&[dir.join("b10.jpg").to_string_lossy().into_owned()], &cfg);
        let names: Vec<_> = list.iter().map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["b2.png", "b10.jpg", "c.mp4"]);
        assert_eq!(start, 1);
        assert_eq!(list[2].kind, Kind::Video);
        let _ = fs::remove_dir_all(&dir);
    }
}
