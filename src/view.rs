//! Zoom a posun obsahu (rovnaké správanie ako image-positioning.lua s margin=0).

use crate::config::SizeSpec;

#[derive(Clone, Debug)]
pub struct View {
    /// Rozmery obsahu (zobrazovaná veľkosť v px pri mierke 1.0).
    pub cw: f64,
    pub ch: f64,
    /// Rozmery obrazovky (buffer px).
    pub sw: f64,
    pub sh: f64,
    base: f64,
    /// log2 používateľského zoomu (>= 0).
    zoom: f64,
    /// Posun stredu obsahu od stredu obrazovky.
    pan: (f64, f64),
    min_size: SizeSpec,
    max_size: SizeSpec,
    /// Veľkosť obrazovky pre percentá (v okne = výstup, nie okno). None = rozmery obrazovky.
    reference: Option<(f64, f64)>,
    /// true = zväčšiť/zmenšiť tak, aby sa obsah zmestil na obrazovku (inak skutočná veľkosť).
    upscale: bool,
}

const MAX_ZOOM_LOG2: f64 = 8.0;

impl View {
    pub fn new() -> Self {
        View { cw: 0.0, ch: 0.0, sw: 1.0, sh: 1.0, base: 1.0, zoom: 0.0, pan: (0.0, 0.0), min_size: SizeSpec::NONE, max_size: SizeSpec::NONE, reference: None, upscale: false }
    }

    pub fn has_content(&self) -> bool {
        self.cw > 0.0 && self.ch > 0.0
    }

    pub fn clear_content(&mut self) {
        self.cw = 0.0;
        self.ch = 0.0;
        self.zoom = 0.0;
        self.pan = (0.0, 0.0);
    }

    fn recompute_base(&mut self) {
        if !self.has_content() {
            self.base = 1.0;
            return;
        }
        let fit = (self.sw / self.cw).min(self.sh / self.ch);
        if self.upscale {
            self.base = fit;
            return;
        }
        let (rw, rh) = self.reference.unwrap_or((self.sw, self.sh));
        let (mw, mh) = self.min_size.resolve(rw, rh);
        let (xw, xh) = self.max_size.resolve(rw, rh);
        // Skutočná veľkosť, prípadne zväčšená na minimum, zmenšená na maximum a nakoniec na obrazovku.
        let mut wanted = 1.0f64.max(mw / self.cw).max(mh / self.ch);
        if xw > 0.0 {
            wanted = wanted.min(xw / self.cw);
        }
        if xh > 0.0 {
            wanted = wanted.min(xh / self.ch);
        }
        self.base = wanted.min(fit);
    }

    pub fn set_screen(&mut self, w: f64, h: f64) {
        self.sw = w.max(1.0);
        self.sh = h.max(1.0);
        self.recompute_base();
        self.clamp_pan();
    }

    /// Nový obsah: reset zoomu a posunu (ako reset-on-next-file).
    pub fn set_reference(&mut self, w: f64, h: f64) {
        self.reference = Some((w.max(1.0), h.max(1.0)));
        self.recompute_base();
    }

    pub fn set_content(&mut self, w: f64, h: f64, min_size: SizeSpec, max_size: SizeSpec, upscale: bool) {
        self.cw = w;
        self.ch = h;
        self.min_size = min_size;
        self.max_size = max_size;
        self.upscale = upscale;
        self.zoom = 0.0;
        self.pan = (0.0, 0.0);
        self.recompute_base();
    }

    /// Prepne režim real/fit pre aktuálny obsah (zoom a posun sa vynulujú).
    pub fn set_upscale(&mut self, on: bool) {
        self.upscale = on;
        self.reset();
        self.recompute_base();
    }

    pub fn reset(&mut self) {
        self.zoom = 0.0;
        self.pan = (0.0, 0.0);
    }

    pub fn scale(&self) -> f64 {
        self.base * 2f64.powf(self.zoom)
    }

    pub fn clamp_pan(&mut self) {
        let s = self.scale();
        let lim = |content: f64, screen: f64| ((content * s - screen) / 2.0).max(0.0);
        let (lx, ly) = (lim(self.cw, self.sw), lim(self.ch, self.sh));
        self.pan.0 = self.pan.0.clamp(-lx, lx);
        self.pan.1 = self.pan.1.clamp(-ly, ly);
    }

    pub fn drag(&mut self, dx: f64, dy: f64) {
        self.pan.0 += dx;
        self.pan.1 += dy;
        self.clamp_pan();
    }

    /// Zoom o `delta` (log2) okolo bodu (cx, cy) v px obrazovky (počiatok vľavo hore).
    pub fn zoom_at(&mut self, delta: f64, cx: f64, cy: f64) {
        if !self.has_content() {
            return;
        }
        let s0 = self.scale();
        // Zoom out pod "celý obsah viditeľný" sa nepovoľuje (dezoom_if_full_view=no).
        let z1 = (self.zoom + delta).clamp(0.0, MAX_ZOOM_LOG2);
        if (z1 - self.zoom).abs() < 1e-12 {
            return;
        }
        let px = cx - self.sw / 2.0;
        let py = cy - self.sh / 2.0;
        let qx = (px - self.pan.0) / s0;
        let qy = (py - self.pan.1) / s0;
        self.zoom = z1;
        let s1 = self.scale();
        self.pan = (px - qx * s1, py - qy * s1);
        self.clamp_pan();
    }

    /// (left, top, width, height) v px; left/top zaokrúhlené, aby bola skutočná veľkosť ostrá.
    pub fn rect(&self) -> (f64, f64, f64, f64) {
        let s = self.scale();
        let (w, h) = (self.cw * s, self.ch * s);
        let left = (self.sw / 2.0 + self.pan.0 - w / 2.0).round();
        let top = (self.sh / 2.0 + self.pan.1 - h / 2.0).round();
        (left, top, w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Dim;

    fn px(w: u32, h: u32) -> SizeSpec {
        SizeSpec { w: Dim::Px(w), h: Dim::Px(h) }
    }

    fn view(cw: f64, ch: f64) -> View {
        let mut v = View::new();
        v.set_screen(1920.0, 1080.0);
        v.set_content(cw, ch, SizeSpec::NONE, SizeSpec::NONE, false);
        v
    }

    #[test]
    fn small_is_real_size_big_is_fit() {
        assert_eq!(view(800.0, 600.0).scale(), 1.0);
        assert!((view(3840.0, 2160.0).scale() - 0.5).abs() < 1e-9);
        let (l, t, w, h) = view(800.0, 600.0).rect();
        assert_eq!((l, t, w, h), (560.0, 240.0, 800.0, 600.0));
    }

    #[test]
    fn video_min_size() {
        let mut v = View::new();
        v.set_screen(1920.0, 1080.0);
        v.set_content(640.0, 360.0, px(1280, 720), SizeSpec::NONE, false);
        assert_eq!(v.scale(), 2.0);
        v.set_content(640.0, 360.0, px(5000, 5000), SizeSpec::NONE, false); // väčšie ako obrazovka -> fit
        assert!((v.scale() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn video_max_size_and_percent() {
        let pct = |p| SizeSpec { w: Dim::Pct(p), h: Dim::Pct(p) };
        let mut v = View::new();
        v.set_screen(1920.0, 1080.0);
        v.set_content(1600.0, 900.0, SizeSpec::NONE, pct(50.0), false); // max 960x540
        assert!((v.scale() - 0.6).abs() < 1e-9);
        v.set_content(640.0, 360.0, SizeSpec::NONE, pct(50.0), false); // menšie ostane
        assert_eq!(v.scale(), 1.0);
        v.set_content(320.0, 180.0, pct(25.0), pct(50.0), false); // min 480x270 -> 1.5
        assert_eq!(v.scale(), 1.5);
        // percentá sa počítajú voči obrazovke aj keď je okno menšie
        v.set_reference(1920.0, 1080.0);
        v.set_screen(800.0, 450.0);
        v.set_content(1600.0, 900.0, SizeSpec::NONE, pct(50.0), true);
        v.set_upscale(false);
        assert!((v.scale() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn upscale_fit() {
        let mut v = View::new();
        v.set_screen(1920.0, 1080.0);
        v.set_content(640.0, 360.0, SizeSpec::NONE, SizeSpec::NONE, true);
        assert_eq!(v.scale(), 3.0); // zväčší na celú obrazovku
        v.set_content(1000.0, 1000.0, SizeSpec::NONE, SizeSpec::NONE, true);
        assert!((v.scale() - 1.08).abs() < 1e-9); // pomer strán sa zachová
        v.set_content(3840.0, 2160.0, SizeSpec::NONE, SizeSpec::NONE, true);
        assert_eq!(v.scale(), 0.5); // veľké sa zmenší
        v.set_upscale(false);
        assert_eq!(v.scale(), 0.5);
        v.set_content(640.0, 360.0, SizeSpec::NONE, SizeSpec::NONE, false);
        assert_eq!(v.scale(), 1.0);
        v.set_upscale(true);
        assert_eq!(v.scale(), 3.0);
    }

    #[test]
    fn no_dezoom_when_fully_visible_and_pan_clamped() {
        let mut v = view(800.0, 600.0);
        v.zoom_at(-0.1, 100.0, 100.0);
        assert_eq!(v.scale(), 1.0);
        v.drag(50.0, 50.0);
        assert_eq!(v.rect().0, 560.0);
        for _ in 0..20 {
            v.zoom_at(0.1, 1900.0, 1000.0);
        }
        let (l, t, w, h) = v.rect();
        assert!(l <= 0.0 && l + w >= 1920.0 && t <= 0.0 && t + h >= 1080.0);
    }

    #[test]
    fn cursor_centric_keeps_point() {
        let mut v = view(4000.0, 3000.0);
        let s0 = v.scale();
        let (l0, t0, _, _) = v.rect();
        let (cx, cy) = (1000.0, 500.0);
        let (qx, qy) = ((cx - l0) / s0, (cy - t0) / s0);
        v.zoom_at(1.0, cx, cy);
        let s1 = v.scale();
        let (l1, t1, _, _) = v.rect();
        // bod pod kurzorom ostáva (až na zaokrúhlenie a clamp) na mieste
        assert!(((l1 + qx * s1) - cx).abs() < 1.5 || l1 == 0.0);
        assert!(((t1 + qy * s1) - cy).abs() < 1.5 || t1 == 0.0);
    }
}
