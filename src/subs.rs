//! Hľadanie externých titulkov k videu (ako mpv `sub-auto=fuzzy`, ale jednoduchšie).

use std::path::{Path, PathBuf};

const EXT: [&str; 6] = ["srt", "ass", "ssa", "vtt", "sub", "smi"];

/// Nájde súbor titulkov vedľa videa: `video.srt`, `video.sk.srt`, `video.en.forced.ass`...
/// Poradie: preferované jazyky (v poradí `langs`), potom súbor bez jazyka, potom ostatné.
pub fn find_external(video: &Path, langs: &[String]) -> Option<PathBuf> {
    let dir = video.parent()?;
    let stem = video.file_stem()?.to_string_lossy().to_lowercase();
    let mut found: Vec<(usize, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let (Some(name), Some(ext)) = (p.file_name(), p.extension()) else { continue };
        let (name, ext) = (name.to_string_lossy().to_lowercase(), ext.to_string_lossy().to_lowercase());
        if !EXT.contains(&ext.as_str()) {
            continue;
        }
        let base = &name[..name.len() - ext.len() - 1];
        let rest = if base == stem {
            ""
        } else if let Some(r) = base.strip_prefix(&format!("{stem}.")) {
            r
        } else {
            continue;
        };
        let rank = if rest.is_empty() {
            langs.len()
        } else {
            langs.iter().position(|l| rest.split('.').any(|t| t == l)).unwrap_or(langs.len() + 1)
        };
        found.push((rank, p));
    }
    found.sort();
    found.into_iter().next().map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn picks_by_language_then_plain() {
        let dir = std::env::temp_dir().join(format!("rviewer-subs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for n in ["Film.mp4", "Film.srt", "Film.en.srt", "Film.SK.forced.ass", "Other.srt", "Film2.srt", "Film.txt"] {
            fs::write(dir.join(n), b"x").unwrap();
        }
        let v = dir.join("Film.mp4");
        let name = |p: Option<PathBuf>| p.map(|p| p.file_name().unwrap().to_string_lossy().into_owned());
        assert_eq!(name(find_external(&v, &["sk".into(), "en".into()])), Some("Film.SK.forced.ass".into()));
        assert_eq!(name(find_external(&v, &["en".into()])), Some("Film.en.srt".into()));
        assert_eq!(name(find_external(&v, &[])), Some("Film.srt".into()));
        assert_eq!(name(find_external(&dir.join("Nothing.mp4"), &[])), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
