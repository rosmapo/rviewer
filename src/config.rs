//! Konfigurácia vo formáte `kľúč=hodnota` (ako mpv.conf).

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    None,
    Name,
    Date,
    Size,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Linear,
    Nearest,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Straight (nepremultiplikované) RGBA 0..1.
    pub background: [f32; 4],
    /// Minimálna veľkosť videa (menšie sa zväčší). (0,0) = vypnuté.
    pub video_min_size: SizeSpec,
    pub video_max_size: SizeSpec,
    /// true = video sa zväčší/zmenší na obrazovku (`fit`), false = skutočná veľkosť (`real`).
    pub video_fit: bool,
    /// Hlasitosť v percentách.
    pub volume: f64,
    pub autoload: bool,
    pub ignore_hidden: bool,
    /// Súbory, ktoré sa nepodarí zobraziť/prehrať, sa preskočia (a vyradia zo zoznamu).
    pub skip_broken: bool,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub image_ext: Vec<String>,
    pub video_ext: Vec<String>,
    /// 0 = kurzor sa neskrýva.
    pub cursor_autohide_ms: u64,
    pub filter: Filter,
    /// true = fullscreen, false = okno veľkosti obsahu.
    pub fullscreen: bool,
    /// Príkaz (cez `sh -c`) spustený po zmene veľkosti okna, napr. na vycentrovanie.
    pub window_center_cmd: String,
    /// Príkaz, ktorý kompozitoru zmení veľkosť AJ polohu okna naraz (bez skákania).
    pub window_place_cmd: String,
    /// Cesta k fontu pre OSD; prázdne = nájde sa cez fc-match.
    pub osd_font: String,
    /// Veľkosť písma OSD (logické px).
    pub osd_font_size: f32,
    pub osd_color: [f32; 4],
    pub osd_accent: [f32; 4],
    pub seekbar: bool,
    /// Zobrazovať titulky (vložené aj externé).
    pub subtitles: bool,
    /// Preferované jazyky externých titulkov (v názve súboru: video.sk.srt).
    pub sub_languages: Vec<String>,
    /// Kódovanie textových titulkov, napr. CP1250 (prázdne = automaticky).
    pub sub_encoding: String,
    /// Pango popis písma titulkov, napr. "Sans Bold 24" (prázdne = predvolené).
    pub sub_font: String,
    /// Skrytie lišty po nečinnosti (ms), 0 = vždy viditeľná pri videu.
    pub seekbar_timeout_ms: u64,
}

fn exts(s: &str) -> Vec<String> {
    s.split(',').map(|e| e.trim().trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty()).collect()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            background: [0.0, 0.0, 0.0, 1.0],
            video_min_size: SizeSpec::NONE,
            video_max_size: SizeSpec::NONE,
            video_fit: false,
            volume: 100.0,
            autoload: true,
            ignore_hidden: true,
            skip_broken: true,
            sort: SortKey::Name,
            sort_desc: false,
            image_ext: exts("jpg,jpeg,png,bmp,gif,webp,tif,tiff"),
            video_ext: exts("mkv,avi,mp4,ogv,webm,rmvb,flv,wmv,mpeg,mpg,m4v,3gp,mov,ts"),
            cursor_autohide_ms: 1000,
            filter: Filter::Linear,
            fullscreen: true,
            window_center_cmd: String::new(),
            window_place_cmd: String::new(),
            osd_font: String::new(),
            osd_font_size: 18.0,
            osd_color: [1.0, 1.0, 1.0, 1.0],
            osd_accent: [0.90, 0.63, 0.05, 1.0],
            seekbar: true,
            subtitles: true,
            sub_languages: exts("sk,slk,cs,cze,en,eng"),
            sub_encoding: String::new(),
            sub_font: String::new(),
            seekbar_timeout_ms: 1500,
        }
    }
}

fn parse_bool(v: &str) -> Result<bool, String> {
    match v.to_lowercase().as_str() {
        "yes" | "true" | "1" | "on" => Ok(true),
        "no" | "false" | "0" | "off" => Ok(false),
        _ => Err(format!("očakávané yes/no, dostal som '{v}'")),
    }
}

/// `#RRGGBB` alebo `#AARRGGBB` (rovnako ako v mpv).
pub fn parse_color(v: &str) -> Result<[f32; 4], String> {
    let h = v.trim().trim_start_matches('#');
    let byte = |s: &str| u8::from_str_radix(s, 16).map(|b| b as f32 / 255.0).map_err(|_| format!("zlá farba '{v}'"));
    match h.len() {
        6 => Ok([byte(&h[0..2])?, byte(&h[2..4])?, byte(&h[4..6])?, 1.0]),
        8 => Ok([byte(&h[2..4])?, byte(&h[4..6])?, byte(&h[6..8])?, byte(&h[0..2])?]),
        _ => Err(format!("farba musí byť #RRGGBB alebo #AARRGGBB, dostal som '{v}'")),
    }
}

/// Jeden rozmer: pixely alebo percentá obrazovky.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dim {
    Px(u32),
    Pct(f64),
}

/// Šírka x výška; 0 = bez obmedzenia.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeSpec {
    pub w: Dim,
    pub h: Dim,
}

impl SizeSpec {
    pub const NONE: SizeSpec = SizeSpec { w: Dim::Px(0), h: Dim::Px(0) };

    /// Prepočet na pixely voči referenčnej veľkosti obrazovky.
    pub fn resolve(&self, rw: f64, rh: f64) -> (f64, f64) {
        let f = |d: Dim, r: f64| match d {
            Dim::Px(p) => p as f64,
            Dim::Pct(p) => (r * p / 100.0).round(),
        };
        (f(self.w, rw), f(self.h, rh))
    }
}

fn parse_dim(s: &str) -> Result<Dim, String> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        let v: f64 = p.trim().parse().map_err(|_| format!("'{s}' nie je percento"))?;
        if v < 0.0 {
            return Err(format!("'{s}' je záporné"));
        }
        Ok(Dim::Pct(v))
    } else {
        s.parse().map(Dim::Px).map_err(|_| format!("'{s}' nie je veľkosť v px ani v %"))
    }
}

/// `0`, `1280x720`, `50%` (šírka aj výška), `50%x40%`, `1280x50%`.
fn parse_size(v: &str) -> Result<SizeSpec, String> {
    let v = v.trim();
    if v == "0" || v.is_empty() {
        return Ok(SizeSpec::NONE);
    }
    let parts: Vec<&str> = v.split(|c| c == 'x' || c == 'X' || c == ',').collect();
    match parts.as_slice() {
        [one] if one.trim().ends_with('%') => {
            let d = parse_dim(one)?;
            Ok(SizeSpec { w: d, h: d })
        }
        [w, h] => Ok(SizeSpec { w: parse_dim(w)?, h: parse_dim(h)? }),
        _ => Err(format!("veľkosť musí byť ŠxV (1280x720) alebo percentá (50% / 50%x40%), dostal som '{v}'")),
    }
}

impl Config {
    pub fn set(&mut self, key: &str, val: &str) -> Result<(), String> {
        let val = val.trim().trim_matches('"');
        let num = |v: &str| v.parse::<f64>().map_err(|_| format!("'{v}' nie je číslo"));
        match key {
            "background" | "background-color" => self.background = parse_color(val)?,
            "video-fit" => {
                self.video_fit = match val {
                    "real" => false,
                    "fit" => true,
                    _ => return Err(format!("video-fit: real alebo fit, dostal som '{val}'")),
                }
            }
            "video-min-size" => self.video_min_size = parse_size(val)?,
            "video-max-size" => self.video_max_size = parse_size(val)?,
            "volume" => self.volume = num(val)?.clamp(0.0, 1000.0),
            "autoload" => self.autoload = parse_bool(val)?,
            "ignore-hidden" => self.ignore_hidden = parse_bool(val)?,
            "skip-broken" => self.skip_broken = parse_bool(val)?,
            "sort" => {
                let (k, d) = match val.split_once('-') {
                    Some((k, d)) => (k, d),
                    None => (val, "asc"),
                };
                self.sort = match k {
                    "none" => SortKey::None,
                    "name" => SortKey::Name,
                    "date" => SortKey::Date,
                    "size" => SortKey::Size,
                    _ => return Err(format!("neznáme triedenie '{val}'")),
                };
                self.sort_desc = match d {
                    "asc" => false,
                    "desc" => true,
                    _ => return Err(format!("neznáme triedenie '{val}'")),
                };
            }
            "image-extensions" => self.image_ext = exts(val),
            "video-extensions" => self.video_ext = exts(val),
            "cursor-autohide" => self.cursor_autohide_ms = num(val)?.max(0.0) as u64,
            "fullscreen" => self.fullscreen = parse_bool(val)?,
            "osd-font" => self.osd_font = val.to_string(),
            "osd-font-size" => self.osd_font_size = (num(val)? as f32).clamp(6.0, 200.0),
            "osd-color" => self.osd_color = parse_color(val)?,
            "osd-accent" => self.osd_accent = parse_color(val)?,
            "subtitles" => self.subtitles = parse_bool(val)?,
            "sub-languages" => self.sub_languages = exts(val),
            "sub-encoding" => self.sub_encoding = val.to_string(),
            "sub-font" => self.sub_font = val.to_string(),
            "seekbar" => self.seekbar = parse_bool(val)?,
            "seekbar-timeout" => self.seekbar_timeout_ms = num(val)?.max(0.0) as u64,
            "window-place-command" => self.window_place_cmd = val.to_string(),
            "window-center-command" => self.window_center_cmd = val.to_string(),
            // Zrušené voľby (posun je teraz v input.conf: `seek`, `seek-or-step`).
            "seek-small" | "seek-big" => {}
            "scale" => {
                self.filter = match val {
                    "linear" => Filter::Linear,
                    "nearest" => Filter::Nearest,
                    _ => return Err(format!("scale: linear alebo nearest, dostal som '{val}'")),
                }
            }
            _ => return Err(format!("neznámy kľúč '{key}'")),
        }
        Ok(())
    }

    pub fn load_str(&mut self, text: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let mut line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // Inline komentár iba po medzere (aby sa nezrušila farba #RRGGBB).
            if let Some(p) = line.find(" #").or_else(|| line.find("\t#")) {
                line = line[..p].trim();
            }
            match line.split_once('=') {
                Some((k, v)) => {
                    if let Err(e) = self.set(k.trim(), v) {
                        warnings.push(format!("riadok {}: {e}", n + 1));
                    }
                }
                None => warnings.push(format!("riadok {}: chýba '='", n + 1)),
            }
        }
        warnings
    }

    pub fn load_file(&mut self, path: &Path) -> std::io::Result<Vec<String>> {
        let text = std::fs::read_to_string(path)?;
        Ok(self.load_str(&text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpv_like_config() {
        let mut c = Config::default();
        let w = c.load_str(
            "# komentár\nbackground = \"#B218120c\"\nvideo-min-size=1280x720   # inline\nsort=date-desc\nvolume=80\nbogus=1\n",
        );
        assert_eq!(w.len(), 1);
        assert!((c.background[3] - 0xB2 as f32 / 255.0).abs() < 1e-6);
        assert!((c.background[0] - 0x18 as f32 / 255.0).abs() < 1e-6);
        assert_eq!(c.video_min_size, SizeSpec { w: Dim::Px(1280), h: Dim::Px(720) });
        assert_eq!(parse_size("50%").unwrap(), SizeSpec { w: Dim::Pct(50.0), h: Dim::Pct(50.0) });
        assert_eq!(parse_size("1280x40%").unwrap().h, Dim::Pct(40.0));
        assert_eq!(parse_size("50%x25%").unwrap().resolve(1920.0, 1080.0), (960.0, 270.0));
        assert!(parse_size("abc").is_err());
        assert_eq!((c.sort, c.sort_desc), (SortKey::Date, true));
        assert_eq!(c.volume, 80.0);
        assert!(c.set("video-fit", "fit").is_ok() && c.video_fit);
        assert!(c.set("video-fit", "fill").is_err());
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("#ff0000").unwrap(), [1.0, 0.0, 0.0, 1.0]);
        assert!(parse_color("red").is_err());
    }
}
