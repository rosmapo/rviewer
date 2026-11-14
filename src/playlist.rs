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
                entry: Entry { path, kind },
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
    Entry { path: p, kind }
}

/// Vráti zoznam a index súboru, od ktorého sa začína.
pub fn build(args: &[String], cfg: &Config) -> (Vec<Entry>, usize) {
    let mut out = Vec::new();
    let mut start = 0;
    if args.is_empty() {
        out = list_dir(&absolute("."), cfg);
    } else if args.len() == 1 {
        let p = absolute(&args[0]);
        if p.is_dir() {
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
            let p = absolute(a);
            if p.is_dir() {
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
