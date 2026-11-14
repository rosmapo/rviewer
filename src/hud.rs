//! Čistá logika OSD: formátovanie a geometria vyhľadávacej lišty (bez GL, testovateľné).

pub fn fmt_time(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    let (h, m, s) = (t / 3600, (t / 60) % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn human_size(bytes: u64) -> String {
    const U: [&str; 4] = ["B", "kB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Rozloženie lišty (px buffera). Pás zasahuje po spodný okraj obrazovky.
#[derive(Clone, Copy, Debug)]
pub struct BarGeom {
    pub strip_y: f32,
    pub strip_h: f32,
    pub width: f32,
    pub icon_x: f32,
    pub pos_x: f32,
    pub track_x: f32,
    pub track_x1: f32,
    pub dur_x: f32,
    pub cy: f32,
    pub pad: f32,
}

impl BarGeom {
    /// `label_w` = šírka textu času (rovnaká pre aktuálny aj celkový čas).
    pub fn compute(screen: (f32, f32), s: f32, label_w: f32) -> BarGeom {
        let (w, h) = screen;
        let m = 16.0 * s;
        let strip_h = 56.0 * s;
        let strip_y = h - strip_h;
        let pos_x = m + 26.0 * s;
        let track_x = pos_x + label_w + 12.0 * s;
        let dur_x = w - m - label_w;
        BarGeom {
            strip_y,
            strip_h,
            width: w,
            icon_x: m,
            pos_x,
            track_x,
            track_x1: (dur_x - 12.0 * s).max(track_x + 1.0),
            dur_x,
            cy: strip_y + strip_h / 2.0,
            pad: 8.0 * s,
        }
    }

    /// Pozícia na lište 0..1 pre x (aj mimo lišty – orezané).
    pub fn fraction(&self, x: f64) -> f64 {
        let (a, b) = (self.track_x as f64, self.track_x1 as f64);
        ((x - a) / (b - a)).clamp(0.0, 1.0)
    }

    /// Je bod nad pásom lišty?
    pub fn zone(&self, x: f64, y: f64) -> bool {
        x >= 0.0 && x <= self.width as f64 && y >= self.strip_y as f64
    }

    /// Klik na dráhu (s rezervou po stranách) -> pozícia 0..1.
    pub fn hit(&self, x: f64, y: f64) -> Option<f64> {
        if self.zone(x, y) && x >= (self.track_x - self.pad) as f64 && x <= (self.track_x1 + self.pad) as f64 {
            Some(self.fraction(x))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_and_size() {
        assert_eq!(fmt_time(65.4), "01:05");
        assert_eq!(fmt_time(3725.0), "1:02:05");
        assert_eq!(fmt_time(-3.0), "00:00");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(12_345_678), "12.3 MB");
    }

    #[test]
    fn bar_geometry_and_hit() {
        let g = BarGeom::compute((1920.0, 1080.0), 1.0, 50.0);
        assert_eq!(g.strip_y, 1024.0);
        assert!(g.track_x < g.track_x1);
        assert_eq!(g.fraction(g.track_x as f64 - 100.0), 0.0);
        assert_eq!(g.fraction(g.track_x1 as f64 + 100.0), 1.0);
        let mid = (g.track_x as f64 + g.track_x1 as f64) / 2.0;
        assert!((g.fraction(mid) - 0.5).abs() < 1e-9);
        assert!(g.hit(mid, 1050.0).is_some());
        assert!(g.hit(mid, 500.0).is_none()); // nad pásom
        assert!(g.hit(5.0, 1050.0).is_none()); // mimo dráhy
        assert!(g.zone(5.0, 1050.0));
    }
}
