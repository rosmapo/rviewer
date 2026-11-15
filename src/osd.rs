//! OSD: textový panel (info) a vyhľadávacia lišta videa. Text cez `fontdue` + atlas glyfov v GLES2.

use crate::config::Config;
use crate::gfx::Gfx;
use crate::hud::{fmt_time, BarGeom};
use std::collections::HashMap;

const ATLAS: u32 = 1024;

#[derive(Clone, Copy)]
struct Glyph {
    uv: [f32; 4],
    w: f32,
    h: f32,
    xmin: f32,
    ymin: f32,
}

pub struct BarState {
    pub pos: f64,
    pub dur: f64,
    pub paused: bool,
    pub hover: bool,
}

pub struct Osd {
    font: Option<fontdue::Font>,
    px: f32,
    glyphs: HashMap<char, Glyph>,
    atlas_ready: bool,
    /// Posuvné miesto v atlase: (x, y, výška riadku).
    pen: (u32, u32, u32),
    bar: Option<BarGeom>,
    list: Option<ListGeom>,
}

/// Poloha zoznamu na obrazovke (px buffera) pre klikanie myšou.
#[derive(Clone, Copy)]
struct ListGeom {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rows_y: f32,
    lh: f32,
    top: usize,
    rows: usize,
}

fn load_font(cfg: &Config) -> Option<fontdue::Font> {
    let mut paths: Vec<String> = Vec::new();
    if !cfg.osd_font.is_empty() {
        paths.push(cfg.osd_font.clone());
    } else {
        if let Ok(o) = std::process::Command::new("fc-match").args(["-f", "%{file}", "sans-serif:lang=sk"]).output() {
            if o.status.success() {
                paths.push(String::from_utf8_lossy(&o.stdout).trim().to_string());
            }
        }
        for p in [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        ] {
            paths.push(p.to_string());
        }
    }
    for p in paths {
        if let Ok(bytes) = std::fs::read(&p) {
            if let Ok(font) = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()) {
                return Some(font);
            }
        }
    }
    None
}

impl Osd {
    pub fn load(cfg: &Config) -> Osd {
        let font = load_font(cfg);
        if font.is_none() {
            eprintln!("rviewer: nenašiel som font pre OSD (nastav osd-font=/cesta/k/fontu.ttf); info a lišta sú vypnuté");
        }
        Osd { font, px: 0.0, glyphs: HashMap::new(), atlas_ready: false, pen: (0, 0, 0), bar: None, list: None }
    }

    pub fn available(&self) -> bool {
        self.font.is_some()
    }

    /// Veľkosť písma v px buffera; zmena zahodí atlas.
    pub fn set_px(&mut self, px: f32) {
        if (px - self.px).abs() > 0.01 {
            self.px = px;
            self.glyphs.clear();
            self.atlas_ready = false;
        }
    }

    /// Zoznam v strede obrazovky; vráti počet viditeľných riadkov (na posúvanie výberu).
    pub fn draw_list(
        &mut self, gfx: &mut Gfx, s: f32, screen: (f32, f32), header: &str, items: &[String], top: usize, sel: usize,
        color: [f32; 4], accent: [f32; 4],
    ) -> usize {
        let (_, lh) = self.line_metrics();
        let pad = 12.0 * s;
        let w = (screen.0 * 0.7).min(900.0 * s).max(200.0 * s).min(screen.0 - 2.0 * pad);
        let max_h = (screen.1 * 0.8).max(lh * 4.0);
        let rows_max = (((max_h - 2.0 * pad - lh - 6.0 * s) / lh).floor() as usize).max(1);
        let rows = rows_max.min(items.len()).max(1);
        let h = 2.0 * pad + lh + 6.0 * s + rows as f32 * lh;
        let (x, y) = (((screen.0 - w) / 2.0).round(), ((screen.1 - h) / 2.0).max(0.0).round());
        gfx.fill_rect(x, y, w, h, [0.0, 0.0, 0.0, 0.82]);
        let head = self.fit(header, w - 2.0 * pad);
        self.draw_text(gfx, x + pad, y + pad, &head, color);
        let rows_y = y + pad + lh + 6.0 * s;
        let top = top.min(items.len().saturating_sub(rows));
        for r in 0..rows {
            let i = top + r;
            let Some(item) = items.get(i) else { break };
            let ry = rows_y + r as f32 * lh;
            if i == sel {
                gfx.fill_rect(x + pad / 2.0, ry, w - pad, lh, [accent[0], accent[1], accent[2], 0.45]);
            }
            let t = self.fit(item, w - 2.0 * pad);
            self.draw_text(gfx, x + pad, ry, &t, color);
        }
        self.list = Some(ListGeom { x, y, w, h, rows_y, lh, top, rows });
        rows
    }

    /// Index položky pod bodom; `Some(None)` = klik na zoznam mimo riadkov, `None` = mimo zoznamu.
    pub fn list_hit(&self, px: f64, py: f64) -> Option<Option<usize>> {
        let g = self.list?;
        let (px, py) = (px as f32, py as f32);
        if px < g.x || px > g.x + g.w || py < g.y || py > g.y + g.h {
            return None;
        }
        if py >= g.rows_y {
            let r = ((py - g.rows_y) / g.lh) as usize;
            if r < g.rows {
                return Some(Some(g.top + r));
            }
        }
        Some(None)
    }

    pub fn clear_bar(&mut self) {
        self.bar = None;
    }

    /// Pozícia 0..1, ak bod trafil dráhu lišty.
    pub fn bar_hit(&self, x: f64, y: f64) -> Option<f64> {
        self.bar.as_ref()?.hit(x, y)
    }

    pub fn bar_zone(&self, x: f64, y: f64) -> bool {
        self.bar.as_ref().map_or(false, |b| b.zone(x, y))
    }

    pub fn bar_fraction(&self, x: f64) -> Option<f64> {
        self.bar.as_ref().map(|b| b.fraction(x))
    }

    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if self.pen.0 + w + 1 > ATLAS {
            self.pen = (0, self.pen.1 + self.pen.2 + 1, 0);
        }
        if self.pen.1 + h + 1 > ATLAS {
            return None;
        }
        let pos = (self.pen.0, self.pen.1);
        self.pen.0 += w + 1;
        self.pen.2 = self.pen.2.max(h);
        Some(pos)
    }

    fn glyph(&mut self, gfx: &mut Gfx, c: char) -> Option<Glyph> {
        if let Some(g) = self.glyphs.get(&c) {
            return Some(*g);
        }
        let (m, bitmap) = {
            let font = self.font.as_ref()?;
            font.rasterize(c, self.px)
        };
        if !self.atlas_ready {
            gfx.atlas_reset(ATLAS);
            self.pen = (0, 0, 0);
            self.atlas_ready = true;
        }
        let (w, h) = (m.width as u32, m.height as u32);
        let mut uv = [0.0f32; 4];
        if w > 0 && h > 0 {
            let (x, y) = match self.alloc(w, h) {
                Some(p) => p,
                None => {
                    // Atlas plný: začni odznova.
                    self.glyphs.clear();
                    gfx.atlas_reset(ATLAS);
                    self.pen = (0, 0, 0);
                    self.alloc(w, h)?
                }
            };
            gfx.atlas_put(x, y, w, h, &bitmap);
            let a = ATLAS as f32;
            uv = [x as f32 / a, y as f32 / a, (x + w) as f32 / a, (y + h) as f32 / a];
        }
        let g = Glyph { uv, w: w as f32, h: h as f32, xmin: m.xmin as f32, ymin: m.ymin as f32 };
        self.glyphs.insert(c, g);
        Some(g)
    }

    fn advance(&self, c: char) -> f32 {
        self.font.as_ref().map_or(0.0, |f| f.metrics(c, self.px).advance_width)
    }

    /// (ascent, výška riadku)
    fn line_metrics(&self) -> (f32, f32) {
        match self.font.as_ref().and_then(|f| f.horizontal_line_metrics(self.px)) {
            Some(m) => (m.ascent, m.new_line_size),
            None => (self.px * 0.9, self.px * 1.25),
        }
    }

    pub fn measure(&self, text: &str) -> f32 {
        text.lines().map(|l| l.chars().map(|c| self.advance(c)).sum::<f32>()).fold(0.0, f32::max)
    }

    /// Skráti riadok na šírku `max_w` (…).
    fn fit(&self, line: &str, max_w: f32) -> String {
        if self.measure(line) <= max_w {
            return line.to_string();
        }
        let mut s: Vec<char> = line.chars().collect();
        while !s.is_empty() && self.measure(&format!("{}…", s.iter().collect::<String>())) > max_w {
            s.pop();
        }
        format!("{}…", s.iter().collect::<String>())
    }

    pub fn draw_text(&mut self, gfx: &mut Gfx, x: f32, y_top: f32, text: &str, color: [f32; 4]) {
        let (asc, lh) = self.line_metrics();
        for (i, line) in text.lines().enumerate() {
            let base = (y_top + asc + lh * i as f32).round();
            let mut pen = x;
            for c in line.chars() {
                let adv = self.advance(c);
                if let Some(g) = self.glyph(gfx, c) {
                    if g.w > 0.0 {
                        gfx.glyph((pen + g.xmin).round(), base - g.ymin - g.h, g.w, g.h, g.uv, color);
                    }
                }
                pen += adv;
            }
        }
    }

    /// Info panel vľavo hore.
    pub fn draw_info(&mut self, gfx: &mut Gfx, s: f32, screen: (f32, f32), lines: &[String], color: [f32; 4]) {
        let (pad, margin) = (10.0 * s, 16.0 * s);
        let max_w = (screen.0 - 2.0 * (margin + pad)).max(10.0);
        let lines: Vec<String> = lines.iter().map(|l| self.fit(l, max_w)).collect();
        let text = lines.join("\n");
        let (_, lh) = self.line_metrics();
        let (w, h) = (self.measure(&text), lh * lines.len() as f32);
        gfx.fill_rect(margin, margin, w + 2.0 * pad, h + 2.0 * pad, [0.0, 0.0, 0.0, 0.6]);
        self.draw_text(gfx, margin + pad, margin + pad, &text, color);
    }

    /// Vyhľadávacia lišta dole.
    /// Náhľad nad lištou: obrázok vycentrovaný na x (clamp na okraje) a čas pod ním.
    pub fn draw_thumb(
        &mut self, gfx: &mut Gfx, s: f32, screen: (f32, f32), x: f32, t: f64, dim: (u32, u32), max: f32, text: [f32; 4],
    ) {
        let Some(g) = self.bar else { return };
        let (w, h) = (dim.0 as f32, dim.1 as f32);
        if w < 1.0 || h < 1.0 {
            return;
        }
        let k = (max * s / w).min(max * s / h);
        let (dw, dh) = ((w * k).round(), (h * k).round());
        let label = fmt_time(t);
        let tw = self.measure(&label);
        let (_, lh) = self.line_metrics();
        let m = 3.0 * s;
        let bw = dw.max(tw + 2.0 * m);
        let total = dh + lh + m;
        let x0 = (x - bw / 2.0).clamp(g.pad, (screen.0 - bw - g.pad).max(g.pad)).round();
        let y0 = (g.strip_y - total - 2.0 * m - 6.0 * s).max(0.0).round();
        gfx.fill_rect(x0 - m, y0 - m, bw + 2.0 * m, total + 2.0 * m, [0.0, 0.0, 0.0, 0.75]);
        gfx.draw_thumb((x0 + (bw - dw) / 2.0).round(), y0, dw, dh);
        self.draw_text(gfx, (x0 + (bw - tw) / 2.0).round(), y0 + dh + m / 2.0, &label, text);
    }

    pub fn draw_bar(&mut self, gfx: &mut Gfx, s: f32, screen: (f32, f32), st: &BarState, text: [f32; 4], accent: [f32; 4]) {
        let label_w = self.measure(&fmt_time(st.dur)).max(self.measure("00:00")).ceil();
        let g = BarGeom::compute(screen, s, label_w);
        self.bar = Some(g);
        let (_, lh) = self.line_metrics();

        gfx.fill_rect(0.0, g.strip_y, g.width, g.strip_h, [0.0, 0.0, 0.0, 0.55]);

        if st.paused {
            let (bw, bh) = (4.0 * s, 14.0 * s);
            gfx.fill_rect(g.icon_x, g.cy - bh / 2.0, bw, bh, text);
            gfx.fill_rect(g.icon_x + bw + 4.0 * s, g.cy - bh / 2.0, bw, bh, text);
        }

        let ty = g.cy - lh / 2.0;
        self.draw_text(gfx, g.pos_x, ty, &fmt_time(st.pos), text);
        self.draw_text(gfx, g.dur_x, ty, &fmt_time(st.dur), text);

        let th = (if st.hover { 6.0 } else { 4.0 }) * s;
        let tw = g.track_x1 - g.track_x;
        let frac = if st.dur > 0.0 { (st.pos / st.dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
        gfx.fill_rect(g.track_x, g.cy - th / 2.0, tw, th, [1.0, 1.0, 1.0, 0.25]);
        gfx.fill_rect(g.track_x, g.cy - th / 2.0, tw * frac, th, accent);
        let (kw, kh) = (4.0 * s, 16.0 * s);
        gfx.fill_rect((g.track_x + tw * frac - kw / 2.0).round(), g.cy - kh / 2.0, kw, kh, text);
    }
}
