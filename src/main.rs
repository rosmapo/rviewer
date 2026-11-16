mod bindings;
mod config;
mod gfx;
mod hud;
mod imageload;
mod keymap;
mod osd;
mod playlist;
mod subs;
mod video;
mod view;
mod wl;

use anyhow::{bail, Result};
use bindings::{char_to_keysym, modifier_bit, us_char, Bindings, Command, Source, Trigger, BTN_LEFT, SHIFT};
use config::Config;
use hud::{fmt_time, human_size};
use imageload::LoadedImage;
use osd::{BarState, Osd};
use playlist::{Entry, Kind};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use config::SizeSpec;
use video::{Player, StreamOpts, SubOpts, Thumber};
use view::View;
use wayland_client::{backend::WaylandError, globals::registry_queue_init, Connection};
use wl::Input;

const HELP: &str = "\
rviewer – minimálny fullscreen prehliadač obrázkov a videí (Wayland)

Použitie: rviewer [voľby] [súbor|adresár|URL ...]

  Jeden súbor: načíta sa celý jeho adresár (autoload) a začne sa na ňom.
  Viac súborov/adresárov: zoznam v zadanom poradí.

Voľby:
  --config=SÚBOR    iný konfig (predvolene ~/.config/rviewer/rviewer.conf)
  --no-config       bez konfigurácie
  --input-conf=SÚBOR  iné skratky (predvolene ~/.config/rviewer/input.conf)
  --no-input-conf   len vstavané skratky
  --config-dir=ADRESÁR  použije ADRESÁR/rviewer.conf a ADRESÁR/input.conf
  --nogui           bez okna a bez Waylandu: prehrá len zvuk (rádiá, hudba), ovláda sa z terminála
  --window          okno veľkosti obsahu namiesto fullscreenu (--fullscreen = naspäť)
  --slideshow       spustí slideshow (dĺžka obrázka: --slideshow-delay=SEKUNDY)
  --KĽÚČ=HODNOTA    prepíše ľubovoľnú voľbu z konfigu (napr. --background=#202020)
  -h, --help        táto pomoc

Skratky sa menia v input.conf (vzor v repozitári), pozri README.md
";

/// Zobudí hlavnú slučku (volané aj z iných vlákien).
pub fn wake(fd: RawFd) {
    let b = 1u8;
    unsafe {
        libc::write(fd, &b as *const u8 as *const _, 1);
    }
}

struct Anim {
    frames: Vec<(Vec<u8>, u32)>,
    i: usize,
    next: Instant,
    w: u32,
    h: u32,
}

enum Content {
    None,
    Image { anim: Option<Anim> },
    Video(Player),
}

/// Nastavené signálmi SIGINT/SIGTERM/SIGHUP: slučka sa ukončí čisto a terminál sa vráti do pôvodného režimu.
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    QUIT.store(true, AtomicOrdering::SeqCst);
}

/// Terminál v režime po znakoch (bez echa), kým beží zvuk bez okna; pri zahodení sa obnoví.
struct TtyGuard {
    orig: libc::termios,
}

impl TtyGuard {
    /// Len ak je stdin terminál a rviewer je jeho popredný proces (inak by ho shell zastavil).
    fn enable() -> Option<TtyGuard> {
        unsafe {
            if libc::isatty(0) == 0 || libc::tcgetpgrp(0) != libc::getpgrp() {
                return None;
            }
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return None;
            }
            let orig = t;
            t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            t.c_cc[libc::VMIN] = 0;
            t.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(0, libc::TCSANOW, &t) != 0 {
                return None;
            }
            Some(TtyGuard { orig })
        }
    }
}

impl Drop for TtyGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.orig);
        }
    }
}

type LoadMsg = (u64, Result<LoadedImage, String>);
type PreMsg = (PathBuf, Result<LoadedImage, String>);

struct App {
    cfg: Config,
    list: Vec<Entry>,
    idx: usize,
    /// Posledný smer navigácie (+1 dopredu, -1 dozadu) – kam sa ide po preskočení chybného súboru.
    nav_dir: i64,
    wl: wl::State,
    gfx: gfx::Gfx,
    view: View,
    scale: i32,
    wake_w: RawFd,
    generation: u64,
    tx: mpsc::Sender<LoadMsg>,
    rx: mpsc::Receiver<LoadMsg>,
    /// Predčítané susedné obrázky (najviac 2).
    ptx: mpsc::Sender<PreMsg>,
    prx: mpsc::Receiver<PreMsg>,
    cache: Vec<(PathBuf, LoadedImage)>,
    inflight: std::collections::HashSet<PathBuf>,
    content: Content,
    dirty: bool,
    mods: u8,
    bindings: Bindings,
    drag_button: Option<u32>,
    /// Prebieha `peek` (držanie tlačidla = 100 %).
    peeking: bool,
    center_at: Option<Instant>,
    /// Čakáme na configure od kompozitora po `window-place-command`: (šírka, výška, termín).
    pending_place: Option<(i32, i32, Instant)>,
    cursor: (f64, f64),
    dragging: Option<(f64, f64)>,
    axis_acc: f64,
    axis_t: Instant,
    held: Option<(u32, Instant, Command)>,
    last_motion: Instant,
    volume: f64,
    muted: bool,
    /// Režim `fit` pre video (prepína `cycle-fit`).
    video_fit: bool,
    osd: Osd,
    /// Trvalý info panel (príkaz `info`).
    info_on: bool,
    /// Do kedy je viditeľná vyhľadávacia lišta videa.
    bar_until: Option<Instant>,
    bar_drag: bool,
    seek_target: Option<f64>,
    last_drag_seek: Instant,
    cur_size: u64,
    /// Krátka správa v OSD (zmena fit, hlasitosti, titulkov...).
    toast: Option<(String, Instant)>,
    /// Slideshow beží.
    slideshow: bool,
    /// Kedy sa má prepnúť z aktuálneho obrázka (len pri slideshow).
    slide_next: Option<Instant>,
    loop_file: bool,
    plist_open: bool,
    plist_sel: usize,
    plist_top: usize,
    plist_rows: usize,
    /// Prúd má len zvuk: okno sa vôbec nezobrazí a ovláda sa z terminálu.
    audio_only: bool,
    /// Okno už dostalo prvý obraz (je zobrazené).
    mapped: bool,
    /// Dokedy sa čaká s mapovaním okna na prvý snímok videa (potom sa zobrazí aj prázdne).
    map_deadline: Instant,
    tty: Option<TtyGuard>,
    /// Náhľady pri lište: druhý pipeline, vytvorí sa pri prvom pohybe myši nad lištou.
    thumber: Option<Thumber>,
    thumb_failed: bool,
    /// Čas, pre ktorý sa náhľad zobrazuje (None = skrytý).
    thumb_time: Option<f64>,
    thumb_dim: Option<(u32, u32)>,
    /// Skutočný čas zobrazeného náhľadu (z dekodéra).
    thumb_pts: Option<f64>,
}

impl App {
    fn current_is_video(&self) -> bool {
        matches!(self.content, Content::Video(_))
    }

    fn load_current(&mut self) {
        self.audio_only = false;
        self.tty = None;
        let wait = if playlist::is_url(&self.list[self.idx].path.to_string_lossy()) { 4000 } else { 1500 };
        self.map_deadline = Instant::now() + Duration::from_millis(wait);
        self.content = Content::None; // zahodí prípadný Player
        self.slide_next = None;
        self.thumber = None;
        self.thumb_failed = false;
        self.thumb_time = None;
        self.thumb_dim = None;
        self.thumb_pts = None;
        self.generation += 1;
        self.gfx.clear_content();
        self.view.clear_content();
        self.dirty = true;
        self.bar_until = None;
        self.bar_drag = false;
        self.seek_target = None;
        let entry = self.list[self.idx].clone();
        self.cur_size = std::fs::metadata(&entry.path).map(|m| m.len()).unwrap_or(0);
        let name = entry.display_name();
        self.wl.toplevel.set_title(name);
        match entry.kind {
            Kind::Image => {
                if let Some(i) = self.cache.iter().position(|(p, _)| *p == entry.path) {
                    let (_, img) = self.cache.remove(i);
                    self.show_image(img);
                    return;
                }
                let (gen, tx, wake_w, max) = (self.generation, self.tx.clone(), self.wake_w, self.gfx.max_tex);
                std::thread::spawn(move || {
                    let r = imageload::load(&entry.path, max).map_err(|e| format!("{}: {e}", entry.path.display()));
                    let _ = tx.send((gen, r));
                    wake(wake_w);
                });
            }
            Kind::Video => match video::to_uri(&entry.path).and_then(|uri| {
                    let stream = StreamOpts {
                        net_buffer: self.cfg.net_buffer,
                        audio_only: false,
                        referrer: entry.referrer.clone(),
                        user_agent: entry.user_agent.clone(),
                    };
                    Player::new(&uri, self.volume, self.muted, self.wake_w, &self.sub_opts(&entry.path), &stream)
                }) {
                Ok(p) => self.content = Content::Video(p),
                Err(e) => {
                    eprintln!("{}: {e}", entry.path.display());
                    if self.cfg.skip_broken {
                        self.skip_broken();
                    }
                }
            },
        }
    }

    fn sub_opts(&self, video: &std::path::Path) -> SubOpts {
        let is_net = playlist::is_url(&video.to_string_lossy());
        let ext_uri = if self.cfg.subtitles && !is_net {
            subs::find_external(video, &self.cfg.sub_languages)
                .and_then(|p| gst_uri(&p))
        } else {
            None
        };
        SubOpts { enabled: self.cfg.subtitles, ext_uri, encoding: self.cfg.sub_encoding.clone(), font: self.cfg.sub_font.clone() }
    }

    fn goto(&mut self, new: usize) {
        if new < self.list.len() && new != self.idx {
            self.nav_dir = if new > self.idx { 1 } else { -1 };
            self.idx = new;
            self.load_current();
        }
    }

    /// Súbor sa nedal zobraziť/prehrať: vyradí ho zo zoznamu a ide na ďalší v smere navigácie.
    fn skip_broken(&mut self) {
        eprintln!("preskakujem nezobraziteľný súbor: {}", self.list[self.idx].path.display());
        self.remove_current();
    }

    /// Vyradí aktuálny súbor zo zoznamu a ide na ďalší v smere navigácie.
    fn remove_current(&mut self) {
        self.list.remove(self.idx);
        if self.list.is_empty() {
            eprintln!("rviewer: žiadne zobraziteľné súbory");
            self.wl.running = false;
            return;
        }
        if self.nav_dir >= 0 {
            self.idx = self.idx.min(self.list.len() - 1); // nasledujúci súbor sa posunul na tento index
        } else {
            self.idx = self.idx.saturating_sub(1);
        }
        self.load_current();
    }

    fn navigate(&mut self, delta: i64) {
        let n = (self.idx as i64 + delta).clamp(0, self.list.len() as i64 - 1);
        self.goto(n as usize);
    }

    /// Na pozadí načíta susedné obrázky (najprv v smere navigácie), aby listovanie nečakalo na dekódovanie.
    fn preload_neighbors(&mut self) {
        if !self.cfg.preload {
            return;
        }
        let n = self.list.len() as i64;
        let dir: i64 = if self.nav_dir >= 0 { 1 } else { -1 };
        let wanted: Vec<usize> = [dir, -dir]
            .iter()
            .filter_map(|d| {
                let i = self.idx as i64 + d;
                (0..n).contains(&i).then_some(i as usize)
            })
            .collect();
        let keep: Vec<PathBuf> = wanted.iter().map(|&i| self.list[i].path.clone()).collect();
        self.cache.retain(|(p, _)| keep.contains(p));
        for &i in &wanted {
            let e = self.list[i].clone();
            let is_gif = e.path.extension().and_then(|x| x.to_str()).map_or(false, |x| x.eq_ignore_ascii_case("gif"));
            if !matches!(e.kind, Kind::Image)
                || is_gif
                || self.cache.iter().any(|(p, _)| *p == e.path)
                || self.inflight.contains(&e.path)
            {
                continue;
            }
            self.inflight.insert(e.path.clone());
            let (ptx, wake_w, max) = (self.ptx.clone(), self.wake_w, self.gfx.max_tex);
            std::thread::spawn(move || {
                let r = imageload::load(&e.path, max).map_err(|er| format!("{}: {er}", e.path.display()));
                let _ = ptx.send((e.path, r));
                wake(wake_w);
            });
        }
    }

    /// `run` / `run-remove`: spustí shellový príkaz s hodnotami {path}, {dir}, {name}, {stem}.
    fn run_external(&mut self, cmd: &str, remove: bool) {
        let path = self.list[self.idx].path.clone();
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let part = |o: Option<&std::ffi::OsStr>| o.map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let full = path.to_string_lossy().into_owned();
        let expanded = cmd
            .replace("{path}", &quote(&full))
            .replace("{dir}", &quote(&part(path.parent().map(|p| p.as_os_str()))))
            .replace("{name}", &quote(&part(path.file_name())))
            .replace("{stem}", &quote(&part(path.file_stem())));
        let spawned = std::process::Command::new("sh")
            .arg("-c")
            .arg(&expanded)
            .env("RVIEWER_PATH", &full)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                let short: String = cmd.chars().take(40).collect();
                self.toast(format!("Spustené: {short}"));
                if remove {
                    self.remove_current();
                }
            }
            Err(e) => self.toast(format!("Príkaz zlyhal: {e}")),
        }
    }

    fn show_image(&mut self, img: LoadedImage) {
        self.view.set_content(img.disp_w as f64, img.disp_h as f64, SizeSpec::NONE, SizeSpec::NONE, false);
        self.fit_window();
        let animated = img.frames.len() > 1;
        let mut frames = img.frames;
        self.gfx.upload_rgba(img.tex_w, img.tex_h, &frames[0].0, !animated, false);
        self.content = if animated {
            let next = Instant::now() + Duration::from_millis(frames[0].1 as u64);
            Content::Image { anim: Some(Anim { frames: std::mem::take(&mut frames), i: 0, next, w: img.tex_w, h: img.tex_h }) }
        } else {
            Content::Image { anim: None }
        };
        self.preload_neighbors();
        if self.slideshow {
            self.slide_next = Some(Instant::now() + Duration::from_secs_f64(self.cfg.slideshow_delay));
        }
        self.dirty = true;
    }

    /// Spracuje výsledky z pracovných vlákien, snímky videa a časovače animácie.
    fn poll_content(&mut self) {
        if let Some(t) = self.thumber.as_ref().and_then(|th| th.take()) {
            self.gfx.upload_thumb(t.w, t.h, &t.rgba);
            self.thumb_dim = Some((t.w, t.h));
            self.thumb_pts = t.pts;
            self.dirty = true;
        }
        while let Ok((p, res)) = self.prx.try_recv() {
            self.inflight.remove(&p);
            let neighbor = [self.idx.wrapping_sub(1), self.idx + 1]
                .iter()
                .any(|&i| self.list.get(i).map_or(false, |e| e.path == p));
            if let (true, Ok(img)) = (neighbor, res) {
                if !self.cache.iter().any(|(q, _)| *q == p) {
                    self.cache.push((p, img));
                }
            }
        }
        while let Ok((g, res)) = self.rx.try_recv() {
            if g != self.generation {
                continue;
            }
            match res {
                Ok(img) => self.show_image(img),
                Err(e) => {
                    eprintln!("{e}");
                    if self.cfg.skip_broken {
                        self.skip_broken();
                    }
                }
            }
        }
        let mut refit = false;
        let mut broken = false;
        let mut advance = false;
        let mut detect_audio = false;
        let fit = self.video_fit && self.wl.fullscreen;
        match &mut self.content {
            Content::Video(p) => {
                if let Some(f) = p.take_frame() {
                    let dw = (f.w as f64 * f.par.0 as f64 / f.par.1 as f64).round().max(1.0);
                    let dh = f.h as f64;
                    if self.view.cw != dw || self.view.ch != dh {
                        if let Some((ow, oh)) = self.wl.output_size_px() {
                            self.view.set_reference(ow as f64, oh as f64);
                        }
                        self.view.set_content(dw, dh, self.cfg.video_min_size, self.cfg.video_max_size, fit);
                        refit = true;
                    }
                    self.gfx.upload_yuv(&f);
                    self.dirty = true;
                }
                if self.loop_file && p.eos {
                    p.restart();
                } else if self.slideshow && p.eos {
                    advance = true;
                }
                if !self.audio_only && p.is_audio_only() {
                    detect_audio = true;
                }
                for e in p.poll_bus() {
                    eprintln!("video: {e}");
                    if !p.got_frame {
                        // Chyba pred prvým snímkom = súbor sa nedá prehrať.
                        broken = true;
                    }
                }
            }
            Content::Image { anim: Some(a) } => {
                let now = Instant::now();
                if now >= a.next {
                    a.i = (a.i + 1) % a.frames.len();
                    self.gfx.upload_rgba(a.w, a.h, &a.frames[a.i].0, false, true);
                    a.next = now + Duration::from_millis(a.frames[a.i].1 as u64);
                    self.dirty = true;
                }
            }
            _ => {}
        }
        if refit {
            self.fit_window();
        }
        if detect_audio {
            self.audio_only = true;
            if !self.mapped {
                self.tty = TtyGuard::enable();
                eprintln!(
                    "zvuk bez obrazu: okno sa nezobrazuje{}",
                    if self.tty.is_some() { ", ovládanie z terminálu (skratky z input.conf, q = koniec)" } else { " (koniec: Ctrl+C / kill)" }
                );
            }
        }
        if broken && self.cfg.skip_broken {
            self.skip_broken();
        } else if advance && self.slideshow {
            self.slide_advance();
        }
    }

    /// Slideshow: ďalší súbor; po poslednom od začiatku (slideshow-loop) alebo koniec slideshow.
    fn slide_advance(&mut self) {
        self.slide_next = None;
        let last = self.list.len() - 1;
        if self.idx < last {
            self.navigate(1);
        } else if self.cfg.slideshow_loop && last > 0 {
            self.goto(0);
            self.nav_dir = 1;
        } else {
            self.slideshow = false;
            self.toast("Slideshow: koniec".into());
        }
    }

    fn toggle_slideshow(&mut self) {
        self.slideshow = !self.slideshow;
        if self.slideshow {
            if let Content::Image { .. } = self.content {
                self.slide_next = Some(Instant::now() + Duration::from_secs_f64(self.cfg.slideshow_delay));
            }
            self.toast(format!("Slideshow: zapnutý ({} s)", self.cfg.slideshow_delay));
        } else {
            self.slide_next = None;
            self.toast("Slideshow: vypnutý".into());
        }
    }

    fn apply_resize(&mut self) {
        let scale = self.wl.scale();
        let fs_changed = std::mem::take(&mut self.wl.fullscreen_changed);
        if fs_changed && self.current_is_video() {
            // `fit` platí len vo fullscreene; stav berieme z kompozitora (f aj F11 idú tou istou cestou).
            self.view.set_upscale(self.video_fit && self.wl.fullscreen);
        }
        if self.wl.resized || scale != self.scale || fs_changed {
            if self.wl.resized || fs_changed {
                self.pending_place = None; // kompozitor odpovedal (alebo sa zmenil režim)
            }
            self.wl.resized = false;
            self.scale = scale;
            self.wl.surface.set_buffer_scale(scale);
            let (w, h) = (self.wl.width * scale, self.wl.height * scale);
            self.gfx.resize(w, h);
            self.view.set_screen(w as f64, h as f64);
            if fs_changed && !self.wl.fullscreen {
                // Po opustení fullscreenu: okno veľkosti obsahu.
                self.fit_window();
            }
            self.dirty = true;
        }
    }

    /// Režim okna: veľkosť okna = veľkosť obsahu (zmenšená, ak je väčší ako obrazovka).
    fn fit_window(&mut self) {
        if self.wl.fullscreen || !self.view.has_content() {
            return;
        }
        let (ow, oh) = self.wl.output_size_px().unwrap_or((1920, 1080));
        self.view.set_reference(ow as f64, oh as f64);
        let mut probe = self.view.clone();
        probe.reset();
        probe.set_screen(ow as f64, oh as f64);
        let (_, _, w, h) = probe.rect();
        let s = self.scale.max(1);
        let lw = ((w / s as f64).round() as i32).max(1);
        let lh = ((h / s as f64).round() as i32).max(1);
        if (lw, lh) != (self.wl.width, self.wl.height) {
            if !self.mapped {
                // Okno ešte nie je zobrazené (video sa zobrazí až s prvým snímkom). Rovnako ako mpv ho zobrazíme hneď
                // v cieľovej veľkosti (aj prvý buffer) a polohu necháme na kompozitore; hook sa spustí až pri zmene veľkosti.
                let s = self.scale.max(1);
                if let Err(e) = self.gfx.recreate_surface(&self.wl.surface, lw * s, lh * s) {
                    eprintln!("EGL: {e}");
                }
                self.apply_window_size(lw, lh);
                return;
            }
            if !self.cfg.window_place_cmd.trim().is_empty() {
                // Kompozitor zmení veľkosť aj polohu naraz; nový obsah nakreslíme až po jeho configure,
                // takže okno neskáče. Do tej doby ostáva na obrazovke posledný nakreslený snímok.
                if self.pending_place.map(|(pw, ph, _)| (pw, ph)) != Some((lw, lh)) {
                    self.pending_place = Some((lw, lh, Instant::now() + Duration::from_millis(400)));
                    self.run_hook(&self.cfg.window_place_cmd.clone(), lw, lh);
                }
                self.dirty = true;
                return;
            }
            self.apply_window_size(lw, lh);
            // Wayland nedovoľuje klientovi okno premiestniť; centrovanie rieši externý príkaz.
            self.center_at = Some(Instant::now() + Duration::from_millis(150));
        }
        self.view.set_screen((lw * s) as f64, (lh * s) as f64);
        self.dirty = true;
    }

    /// Zmení veľkosť okna sami (logické px); okno ostane ukotvené tam, kde ho kompozitor nechá.
    fn apply_window_size(&mut self, lw: i32, lh: i32) {
        let s = self.scale.max(1);
        self.wl.width = lw;
        self.wl.height = lh;
        self.gfx.resize(lw * s, lh * s);
        self.view.set_screen((lw * s) as f64, (lh * s) as f64);
        self.dirty = true;
    }

    fn run_center_hook(&self) {
        let cmd = self.cfg.window_center_cmd.trim().to_string();
        if !cmd.is_empty() {
            self.run_hook(&cmd, self.wl.width, self.wl.height);
        }
    }

    /// Spustí príkaz cez `sh -c`; veľkosť okna (logické px, ako ju vidí kompozitor) dostane v premenných.
    fn run_hook(&self, cmd: &str, w: i32, h: i32) {
        let spawned = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .env("RVIEWER_WIDTH", w.to_string())
            .env("RVIEWER_HEIGHT", h.to_string())
            .env("RVIEWER_PID", std::process::id().to_string())
            .spawn();
        match spawned {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => eprintln!("window command: {e}"),
        }
    }

    fn bar_enabled(&self) -> bool {
        self.cfg.seekbar && self.osd.available() && self.current_is_video()
    }

    fn bar_visible(&self) -> bool {
        self.bar_enabled()
            && (self.bar_drag
                || self.cfg.seekbar_timeout_ms == 0
                || self.bar_until.map_or(false, |t| Instant::now() < t))
    }

    /// Zobrazí vyhľadávaciu lištu na `seekbar-timeout` ms.
    fn show_bar(&mut self) {
        if self.bar_enabled() {
            self.bar_until = Some(Instant::now() + Duration::from_millis(self.cfg.seekbar_timeout_ms));
            self.dirty = true;
        }
    }

    fn seek_frac(&mut self, f: f64, accurate: bool) {
        if let Content::Video(p) = &mut self.content {
            if let Some(d) = p.duration_secs() {
                let t = f * d;
                self.seek_target = Some(t);
                p.seek_abs(t, accurate);
            }
        }
        self.dirty = true;
    }

    fn info_lines(&self) -> Vec<String> {
        let e = &self.list[self.idx];
        let name = e.display_name();
        let mut l2 = format!("[{}/{}]", self.idx + 1, self.list.len());
        if self.view.has_content() {
            l2 += &format!("  {}x{}  {:.0}%", self.view.cw as u32, self.view.ch as u32, self.view.scale() * 100.0);
        }
        if self.cur_size > 0 {
            l2 += &format!("  {}", human_size(self.cur_size));
        }
        if self.slideshow {
            l2 += &format!("  slideshow {} s", self.cfg.slideshow_delay);
        }
        let mut v = vec![name, l2];
        if let Content::Video(p) = &self.content {
            let pos = p.position_secs().map(fmt_time).unwrap_or_else(|| "--:--".into());
            let dur = p.duration_secs().map(fmt_time).unwrap_or_else(|| "--:--".into());
            let mut l3 = format!("{pos} / {dur}");
            if p.paused {
                l3 += "  (pauza)";
            }
            l3 += &format!("  hlasitosť {:.0}%  [{}]", self.volume, if self.video_fit && self.wl.fullscreen { "fit" } else { "real" });
            if self.muted {
                l3 += " (stlmené)";
            }
            v.push(l3);
            let mut l4 = format!("titulky: {}  zvuk: {}", p.sub_label(), p.audio_label());
            if p.speed != 1.0 {
                l4 += &format!("  rýchlosť {}×", p.speed);
            }
            if self.loop_file {
                l4 += "  (opakovanie)";
            }
            v.push(l4);
            if p.buffering {
                v.push(format!("načítavam: {} %", p.buffer_pct));
            }
        }
        v
    }

    fn draw_overlay(&mut self) {
        if !self.osd.available() {
            return;
        }
        let show_bar = self.bar_visible();
        let toast = self.toast.as_ref().filter(|(_, t)| Instant::now() < *t).map(|(s, _)| s.clone());
        if !self.info_on && !show_bar && toast.is_none() && !self.plist_open {
            self.osd.clear_bar();
            return;
        }
        let s = self.scale as f32;
        let screen = ((self.wl.width * self.scale) as f32, (self.wl.height * self.scale) as f32);
        self.osd.set_px(self.cfg.osd_font_size * s);
        if self.plist_open {
            let items: Vec<String> = self
                .list
                .iter()
                .enumerate()
                .map(|(i, e)| format!("{} {:>3}. {}", if i == self.idx { ">" } else { " " }, i + 1, e.display_name()))
                .collect();
            self.plist_scroll();
            let header = format!("Zoznam {}/{}   ↑↓ výber · Enter prehrať · Esc zavrieť", self.plist_sel + 1, items.len());
            let (color, accent) = (self.cfg.osd_color, self.cfg.osd_accent);
            let rows = self.osd.draw_list(&mut self.gfx, s, screen, &header, &items, self.plist_top, self.plist_sel, color, accent);
            if rows != self.plist_rows {
                self.plist_rows = rows;
                self.plist_scroll();
                self.dirty = true;
            }
        }
        if !self.info_on {
            if let Some(t) = &toast {
                self.osd.draw_info(&mut self.gfx, s, screen, &[t.clone()], self.cfg.osd_color);
            }
        }
        if self.info_on {
            let lines = self.info_lines();
            self.osd.draw_info(&mut self.gfx, s, screen, &lines, self.cfg.osd_color);
        }
        if show_bar {
            let st = if let Content::Video(p) = &self.content {
                p.duration_secs().map(|dur| {
                    let pos = self.seek_target.or_else(|| p.position_secs()).unwrap_or(0.0);
                    BarState { pos: pos.min(dur), dur, paused: p.paused, hover: false }
                })
            } else {
                None
            };
            match st {
                Some(mut st) => {
                    st.hover = self.bar_drag || self.osd.bar_zone(self.cursor.0, self.cursor.1);
                    self.osd.draw_bar(&mut self.gfx, s, screen, &st, self.cfg.osd_color, self.cfg.osd_accent);
                    if let (Some(t), Some(dim)) = (self.thumb_time, self.thumb_dim) {
                        let x = self.cursor.0 as f32;
                        let max = self.cfg.thumbnail_size as f32;
                        self.osd.draw_thumb(&mut self.gfx, s, screen, x, self.thumb_pts.unwrap_or(t), dim, max, self.cfg.osd_color);
                    }
                }
                None => self.osd.clear_bar(),
            }
        } else {
            self.osd.clear_bar();
        }
    }

    fn render(&mut self) -> Result<()> {
        if self.pending_place.is_some() {
            return Ok(()); // čakáme na veľkosť od kompozitora; dirty ostáva
        }
        if !self.mapped {
            // Okno sa nezobrazí, kým nie je čo ukázať: zvukový prúd ho nezobrazí vôbec,
            // video až s prvým snímkom (alebo po uplynutí map_deadline, aby chyba nezostala skrytá).
            let hold = match &self.content {
                Content::Video(p) => self.audio_only || (!p.got_frame && Instant::now() < self.map_deadline),
                _ => false,
            };
            if hold {
                self.dirty = false;
                return Ok(());
            }
        }
        self.dirty = false;
        let rect = self.view.rect();
        self.gfx.draw(rect)?;
        self.draw_overlay();
        let r = self.gfx.present();
        self.mapped = true;
        r
    }

    /// Znaky z terminála (zvuk bez okna) prevedené na skratky z input.conf.
    fn handle_tty(&mut self) {
        let (sources, quit) = read_tty_sources();
        if quit {
            self.wl.running = false;
        }
        for src in sources {
            if let Some(cmd) = self.bindings.get(&Trigger { src, mods: 0 }).cloned() {
                self.run_command(&cmd);
            }
        }
    }

    fn px(&self, x: f64, y: f64) -> (f64, f64) {
        (x * self.scale as f64, y * self.scale as f64)
    }

    fn show_cursor(&mut self) {
        self.last_motion = Instant::now();
        if !self.wl.cursor_visible {
            self.wl.cursor_visible = true;
            self.wl.apply_cursor();
        }
    }

    fn zoom(&mut self, delta: f64) {
        self.view.zoom_at(delta, self.cursor.0, self.cursor.1);
        self.dirty = true;
    }

    fn toggle_pause(&mut self) {
        if let Content::Video(p) = &mut self.content {
            p.toggle_pause();
        }
    }

    fn seek(&mut self, secs: f64) {
        if let Content::Video(p) = &mut self.content {
            p.seek_rel(secs);
        }
    }

    /// Náhľad pri myši nad dráhou lišty (alebo pri ťahaní po nej).
    fn update_thumb(&mut self) {
        let is_net = playlist::is_url(&self.list[self.idx].path.to_string_lossy());
        let want = if self.cfg.thumbnails && !is_net && self.bar_visible() {
            match &self.content {
                Content::Video(p) => p.duration_secs().and_then(|d| {
                    let f = if self.bar_drag {
                        self.osd.bar_fraction(self.cursor.0)
                    } else {
                        self.osd.bar_hit(self.cursor.0, self.cursor.1)
                    };
                    f.map(|f| f * d)
                }),
                _ => None,
            }
        } else {
            None
        };
        match want {
            Some(t) => {
                if self.thumber.is_none() && !self.thumb_failed {
                    let path = self.list[self.idx].path.clone();
                    // Pomer strán podľa zobrazeného videa (po rotácii a PAR); najdlhšia strana = thumbnail-size.
                    let (cw, ch) = (self.view.cw.max(1.0), self.view.ch.max(1.0));
                    let k = self.cfg.thumbnail_size as f64 / cw.max(ch);
                    let even = |v: f64| (((v * k).round() as u32).max(2) + 1) & !1;
                    let dim = (even(cw), even(ch));
                    match Thumber::new(&path, dim, self.wake_w) {
                        Ok(th) => self.thumber = Some(th),
                        Err(e) => {
                            eprintln!("náhľady: {e}");
                            self.thumb_failed = true;
                        }
                    }
                }
                if let Some(th) = &mut self.thumber {
                    th.request(t);
                }
                self.thumb_time = Some(t);
                self.dirty = true;
            }
            None => {
                if self.thumb_time.take().is_some() {
                    self.dirty = true;
                }
            }
        }
    }

    fn plist_scroll(&mut self) {
        let rows = self.plist_rows.max(1);
        if self.plist_sel < self.plist_top {
            self.plist_top = self.plist_sel;
        } else if self.plist_sel >= self.plist_top + rows {
            self.plist_top = self.plist_sel + 1 - rows;
        }
    }

    fn plist_toggle(&mut self) {
        self.plist_open = !self.plist_open;
        if self.plist_open {
            self.plist_sel = self.idx;
            self.plist_scroll();
        }
        self.dirty = true;
    }

    fn plist_move(&mut self, delta: i64) {
        let n = self.list.len() as i64;
        self.plist_sel = (self.plist_sel as i64).saturating_add(delta).clamp(0, n - 1) as usize;
        self.plist_scroll();
        self.dirty = true;
    }

    fn plist_play(&mut self, i: usize) {
        self.plist_open = false;
        self.dirty = true;
        if i != self.idx {
            self.goto(i);
        }
    }

    /// Klávesa pri otvorenom zozname; `true` = spracovaná (nepustí sa ďalej).
    fn plist_key(&mut self, code: u32, sym0: Option<u32>, sym: Option<u32>) -> bool {
        let rows = self.plist_rows.max(1) as i64;
        match code {
            103 => self.plist_move(-1),
            108 => self.plist_move(1),
            104 => self.plist_move(-rows),
            109 => self.plist_move(rows),
            102 => self.plist_move(i64::MIN / 2),
            107 => self.plist_move(i64::MAX / 2),
            28 | 96 => {
                let i = self.plist_sel;
                self.plist_play(i);
            }
            1 => {
                self.plist_open = false;
                self.dirty = true;
            }
            _ => {
                if let Some(cmd) = self.lookup_key(code, sym0, sym) {
                    if matches!(cmd, Command::Playlist | Command::Quit) {
                        self.run_command(&cmd);
                    }
                }
            }
        }
        true
    }

    fn toast(&mut self, text: String) {
        eprintln!("{text}");
        self.toast = Some((text, Instant::now() + Duration::from_millis(2000)));
        self.dirty = true;
    }

    fn change_volume(&mut self, d: f64) {
        self.volume = (self.volume + d).clamp(0.0, 200.0);
        if let Content::Video(p) = &self.content {
            p.set_volume(self.volume);
        }
        self.toast(format!("Hlasitosť: {:.0}%", self.volume));
    }

    fn end_peek(&mut self) {
        if std::mem::take(&mut self.peeking) && self.view.peek_end() {
            self.dirty = true;
        }
    }

    fn run_command(&mut self, cmd: &Command) {
        let last = self.list.len() - 1;
        if matches!(cmd, Command::Pause | Command::Seek(_) | Command::SeekOrStep(_)) {
            self.show_bar();
        }
        match cmd {
            Command::Quit => self.wl.running = false,
            Command::Pause => {
                // Pri obrázku medzerník spúšťa/zastavuje slideshow, pri videu pauzuje.
                if self.current_is_video() {
                    self.toggle_pause()
                } else {
                    self.toggle_slideshow()
                }
            }
            Command::Slideshow => self.toggle_slideshow(),
            Command::Playlist => self.plist_toggle(),
            Command::Run { cmd, remove } => self.run_external(cmd, *remove),
            Command::Next => self.navigate(1),
            Command::Prev => self.navigate(-1),
            Command::First => self.goto(0),
            Command::Last => self.goto(last),
            Command::Seek(s) => self.seek(*s),
            Command::SeekOrStep(s) => {
                if self.current_is_video() {
                    self.seek(*s)
                } else {
                    self.navigate(if *s >= 0.0 { 1 } else { -1 })
                }
            }
            Command::Zoom(d) => self.zoom(*d),
            Command::ZoomReset => {
                self.view.reset();
                self.dirty = true;
            }
            Command::Volume(d) => self.change_volume(*d),
            Command::Mute => {
                self.muted = !self.muted;
                if let Content::Video(p) = &self.content {
                    p.set_mute(self.muted);
                }
                self.toast(format!("Zvuk: {}", if self.muted { "stlmený" } else { "zapnutý" }));
            }
            Command::CycleSub => {
                if let Content::Video(p) = &mut self.content {
                    let msg = p.cycle_subtitle();
                    self.toast(msg);
                }
            }
            Command::SubDelay(d) => {
                if let Content::Video(p) = &mut self.content {
                    let msg = p.change_sub_delay(*d);
                    self.toast(msg);
                }
            }
            Command::Speed(f) => {
                if let Content::Video(p) = &mut self.content {
                    let msg = p.change_speed(*f);
                    self.toast(msg);
                }
            }
            Command::SpeedReset => {
                if let Content::Video(p) = &mut self.content {
                    let msg = p.set_speed(1.0);
                    self.toast(msg);
                }
            }
            Command::FrameStep | Command::FrameBackStep => {
                if let Content::Video(p) = &mut self.content {
                    if *cmd == Command::FrameStep {
                        p.frame_step();
                    } else {
                        p.frame_back_step();
                    }
                    self.show_bar();
                }
            }
            Command::LoopFile => {
                self.loop_file = !self.loop_file;
                self.toast(format!("Opakovanie videa: {}", if self.loop_file { "zapnuté" } else { "vypnuté" }));
            }
            Command::CycleAudio => {
                if let Content::Video(p) = &mut self.content {
                    let msg = p.cycle_audio();
                    self.toast(msg);
                }
            }
            Command::Info => {
                self.info_on = !self.info_on;
                self.dirty = true;
            }
            Command::CycleFit => {
                self.video_fit = !self.video_fit;
                if self.current_is_video() {
                    self.view.set_upscale(self.video_fit && self.wl.fullscreen);
                    self.dirty = true;
                }
                self.toast(format!(
                    "Video: {}{}",
                    if self.video_fit { "fit" } else { "real" },
                    if self.wl.fullscreen { "" } else { " (platí len vo fullscreene)" }
                ));
            }
            Command::ToggleFullscreen => {
                // Stav sa zmení až po odpovedi kompozitora (apply_resize).
                self.wl.request_fullscreen(!self.wl.fullscreen);
            }
            Command::DragPan | Command::Peek | Command::Ignore => {}
        }
    }

    fn lookup(&self, src: Source) -> Option<Command> {
        self.bindings.get(&Trigger { src, mods: self.mods }).cloned()
    }

    /// Klávesa: najprv znak podľa rozloženia, potom znak s modifikátormi (bez Shift),
    /// potom fyzická klávesa; pre nelatinské rozloženia aj US znak tej istej klávesy.
    fn lookup_key(&self, code: u32, sym0: Option<u32>, sym: Option<u32>) -> Option<Command> {
        let m = self.mods;
        let mut cands = Vec::new();
        if let Some(s) = sym0 {
            cands.push(Trigger { src: Source::Sym(s), mods: m });
        }
        if let Some(s) = sym {
            cands.push(Trigger { src: Source::Sym(s), mods: m & !SHIFT });
        }
        cands.push(Trigger { src: Source::Key(code), mods: m });
        if sym0.map_or(true, |s| s >= 0x100) {
            if let Some(c) = us_char(code) {
                cands.push(Trigger { src: Source::Sym(char_to_keysym(c)), mods: m });
            }
        }
        cands.iter().find_map(|t| self.bindings.get(t).cloned())
    }

    fn handle_input(&mut self, inp: Input) {
        match inp {
            Input::Key { code, pressed, sym0, sym } => {
                if let Some(bit) = modifier_bit(code) {
                    // Stav modifikátorov (ľavý aj pravý držia ten istý bit; zjednodušene).
                    if pressed { self.mods |= bit } else { self.mods &= !bit }
                    return;
                }
                if !pressed {
                    if matches!(&self.held, Some((c, _, _)) if *c == code) {
                        self.held = None;
                    }
                    return;
                }
                if self.plist_open && self.plist_key(code, sym0, sym) {
                    return;
                }
                if let Some(cmd) = self.lookup_key(code, sym0, sym) {
                    self.run_command(&cmd);
                    if cmd.repeatable() && self.wl.repeat.0 > 0 {
                        let at = Instant::now() + Duration::from_millis(self.wl.repeat.1.max(0) as u64);
                        self.held = Some((code, at, cmd));
                    }
                }
            }
            Input::Blur => {
                self.held = None;
                self.mods = 0;
            }
            Input::Enter { x, y } => {
                self.cursor = self.px(x, y);
                self.show_cursor();
                self.show_bar();
            }
            Input::Leave => {
                self.dragging = None;
                self.drag_button = None;
                self.end_peek();
            }
            Input::Motion { x, y } => {
                let p = self.px(x, y);
                self.cursor = p;
                if let Some(last) = self.dragging {
                    self.view.drag(p.0 - last.0, p.1 - last.1);
                    self.dragging = Some(p);
                    self.dirty = true;
                }
                if self.bar_drag {
                    // Ťahanie po lište: seek najviac každých ~60 ms (rýchly, na keyframe).
                    if let Some(f) = self.osd.bar_fraction(p.0) {
                        if self.last_drag_seek.elapsed() >= Duration::from_millis(60) {
                            self.last_drag_seek = Instant::now();
                            self.seek_frac(f, false);
                        } else if let Content::Video(pl) = &self.content {
                            self.seek_target = pl.duration_secs().map(|d| f * d);
                            self.dirty = true;
                        }
                    }
                }
                self.show_cursor();
                self.show_bar();
                self.update_thumb();
            }
            Input::Button { button, pressed } => {
                if self.plist_open {
                    if button == BTN_LEFT && pressed {
                        match self.osd.list_hit(self.cursor.0, self.cursor.1) {
                            Some(Some(i)) => self.plist_play(i),
                            Some(None) => {}
                            None => {
                                self.plist_open = false;
                                self.dirty = true;
                            }
                        }
                    }
                    return;
                }
                if button == BTN_LEFT {
                    if pressed {
                        if self.bar_visible() {
                            if let Some(f) = self.osd.bar_hit(self.cursor.0, self.cursor.1) {
                                self.bar_drag = true;
                                self.last_drag_seek = Instant::now();
                                self.seek_frac(f, false);
                                return;
                            }
                        }
                    } else if self.bar_drag {
                        // Pustenie: presný seek na konečnú pozíciu.
                        self.bar_drag = false;
                        if let Some(f) = self.osd.bar_fraction(self.cursor.0) {
                            self.seek_frac(f, true);
                        }
                        self.seek_target = None;
                        self.show_bar();
                        return;
                    }
                }
                if !pressed {
                    if self.drag_button == Some(button) {
                        self.dragging = None;
                        self.drag_button = None;
                        self.end_peek();
                    }
                    return;
                }
                if let Some(cmd) = self.lookup(Source::Button(button)) {
                    if cmd == Command::DragPan {
                        self.dragging = Some(self.cursor);
                        self.drag_button = Some(button);
                    } else if cmd == Command::Peek {
                        self.view.peek_begin(self.cursor.0, self.cursor.1);
                        self.peeking = true;
                        self.dragging = Some(self.cursor);
                        self.drag_button = Some(button);
                        self.dirty = true;
                    } else {
                        self.run_command(&cmd);
                    }
                }
            }
            Input::Axis(v) => {
                let now = Instant::now();
                if now.duration_since(self.axis_t) > Duration::from_millis(300) {
                    self.axis_acc = 0.0;
                }
                self.axis_t = now;
                self.axis_acc += v;
                let src = if self.axis_acc >= 15.0 {
                    Some(Source::WheelDown)
                } else if self.axis_acc <= -15.0 {
                    Some(Source::WheelUp)
                } else {
                    None
                };
                if let Some(src) = src {
                    self.axis_acc = 0.0;
                    if self.plist_open {
                        self.plist_move(if matches!(src, Source::WheelDown) { 3 } else { -3 });
                        return;
                    }
                    if let Some(cmd) = self.lookup(src) {
                        self.run_command(&cmd);
                    }
                }
            }
        }
    }

    fn tick_timers(&mut self) {
        if !self.mapped && !self.audio_only && matches!(self.content, Content::Video(_)) && Instant::now() >= self.map_deadline {
            self.dirty = true;
        }
        if let Some(th) = &mut self.thumber {
            th.service();
        }
        if let Some((_, t)) = &self.toast {
            if Instant::now() >= *t {
                self.toast = None;
                self.dirty = true;
            }
        }
        if let Some(t) = self.slide_next {
            if Instant::now() >= t {
                self.slide_advance();
            }
        }
        if let Some((w, h, t)) = self.pending_place {
            if Instant::now() >= t {
                // Kompozitor nereagoval: zmeň veľkosť sami (a vycentruj starým hookom, ak je).
                self.pending_place = None;
                self.apply_window_size(w, h);
                self.center_at = Some(Instant::now() + Duration::from_millis(150));
            }
        }
        if let Some(t) = self.center_at {
            if Instant::now() >= t {
                self.center_at = None;
                self.run_center_hook();
            }
        }
        if let Some((code, next, cmd)) = self.held.clone() {
            let now = Instant::now();
            if now >= next {
                self.run_command(&cmd);
                let period = Duration::from_millis((1000 / self.wl.repeat.0.max(1)) as u64);
                self.held = Some((code, now + period, cmd));
            }
        }
        if let Some(t) = self.bar_until {
            if Instant::now() >= t {
                if self.bar_drag || self.osd.bar_zone(self.cursor.0, self.cursor.1) {
                    // Myš je nad lištou: nechaj ju zobrazenú.
                    self.bar_until = Some(Instant::now() + Duration::from_millis(self.cfg.seekbar_timeout_ms));
                } else {
                    self.bar_until = None;
                    self.dirty = true;
                }
            }
        }
        let ms = self.cfg.cursor_autohide_ms;
        if ms > 0
            && self.wl.can_hide_cursor()
            && self.wl.cursor_visible
            && self.dragging.is_none()
            && self.last_motion.elapsed() >= Duration::from_millis(ms)
        {
            self.wl.cursor_visible = false;
            self.wl.apply_cursor();
        }
    }

    /// Kedy treba najneskôr znova prebudiť slučku (animácia, opakovanie klávesy, skrytie kurzora).
    fn next_deadline(&self) -> Option<Instant> {
        let mut d: Option<Instant> = None;
        let mut put = |t: Instant| d = Some(d.map_or(t, |c| c.min(t)));
        if let Content::Image { anim: Some(a) } = &self.content {
            put(a.next);
        }
        if let Some(t) = self.center_at {
            put(t);
        }
        if !self.mapped && !self.audio_only && matches!(self.content, Content::Video(_)) && Instant::now() < self.map_deadline {
            put(self.map_deadline);
        }
        if let Some(t) = self.slide_next {
            put(t);
        }
        if let Some(t) = self.thumber.as_ref().and_then(|th| th.deadline()) {
            put(t);
        }
        if let Some((_, _, t)) = self.pending_place {
            put(t);
        }
        if let Some((_, t)) = &self.toast {
            put(*t);
        }
        if let Some(t) = self.bar_until {
            put(t);
        }
        if let Some((_, t, _)) = &self.held {
            let t = *t;
            put(t);
        }
        let ms = self.cfg.cursor_autohide_ms;
        if ms > 0 && self.wl.can_hide_cursor() && self.wl.cursor_visible && self.dragging.is_none() {
            put(self.last_motion + Duration::from_millis(ms));
        }
        d
    }
}

/// file:// URI pre GStreamer.
fn gst_uri(p: &std::path::Path) -> Option<String> {
    gstreamer::glib::filename_to_uri(p, None).ok().map(|u| u.to_string())
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("rviewer").join("rviewer.conf"))
}

/// Prečíta znaky z terminála (stdin) a prevedie ich na zdroje skratiek; druhá hodnota = Ctrl+C.
fn read_tty_sources() -> (Vec<Source>, bool) {
    let mut out = Vec::new();
    let mut quit = false;
    let mut buf = [0u8; 32];
    let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut _, buf.len()) };
    if n <= 0 {
        return (out, quit);
    }
    let b = &buf[..n as usize];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == 0x1b && b.get(i + 1) == Some(&b'[') {
            // Šípky a pod.: ESC [ A/B/C/D/H/F alebo ESC [ N ~
            let (code, used) = match (b.get(i + 2), b.get(i + 3)) {
                (Some(b'A'), _) => (Some(103), 3),
                (Some(b'B'), _) => (Some(108), 3),
                (Some(b'C'), _) => (Some(106), 3),
                (Some(b'D'), _) => (Some(105), 3),
                (Some(b'H'), _) => (Some(102), 3),
                (Some(b'F'), _) => (Some(107), 3),
                (Some(b'3'), Some(b'~')) => (Some(111), 4),
                (Some(b'5'), Some(b'~')) => (Some(104), 4),
                (Some(b'6'), Some(b'~')) => (Some(109), 4),
                _ => (None, 3),
            };
            out.extend(code.map(Source::Key));
            i += used;
        } else {
            match c {
                0x03 => quit = true,
                0x1b => out.push(Source::Key(1)),
                b' ' => out.push(Source::Key(57)),
                b'\n' | b'\r' => out.push(Source::Key(28)),
                0x7f => out.push(Source::Key(14)),
                0x20..=0x7e => out.push(Source::Sym(char_to_keysym(c as char))),
                _ => {}
            }
            i += 1;
        }
    }
    (out, quit)
}

/// `--nogui`: bez okna a bez Waylandu. Prehráva zvuk zo zoznamu (súbory aj adresy), ovláda sa z terminála.
fn headless(list: Vec<playlist::Entry>, start: usize, cfg: Config, bindings: Bindings, wake_r: RawFd, wake_w: RawFd) -> Result<()> {
    unsafe {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::signal(sig, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    let tty = TtyGuard::enable();
    if tty.is_some() {
        eprintln!("bez okna: ovládanie z terminála podľa input.conf (q = koniec, medzerník = pauza)");
    }
    let sub = SubOpts { enabled: false, ext_uri: None, encoding: String::new(), font: String::new() };
    let n = list.len();
    let (mut idx, mut volume, mut muted) = (start, cfg.volume, false);
    'outer: loop {
        if QUIT.load(AtomicOrdering::SeqCst) {
            break;
        }
        let entry = list[idx].clone();
        let opened = video::to_uri(&entry.path).and_then(|uri| {
                let stream = StreamOpts {
                    net_buffer: cfg.net_buffer,
                    audio_only: true,
                    referrer: entry.referrer.clone(),
                    user_agent: entry.user_agent.clone(),
                };
                Player::new(&uri, volume, muted, wake_w, &sub, &stream)
            });
        let mut p = match opened {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{}: {e}", entry.path.display());
                if idx + 1 < n {
                    idx += 1;
                    continue;
                }
                break;
            }
        };
        eprintln!("Prehrávam: {}", entry.path.display());
        let (mut advance, mut target) = (false, None::<usize>);
        loop {
            if QUIT.load(AtomicOrdering::SeqCst) {
                break 'outer;
            }
            for e in p.poll_bus() {
                eprintln!("chyba: {e}");
                advance = true;
            }
            if let Some(t) = p.take_title() {
                eprintln!("♪ {t}");
            }
            if p.eos {
                advance = true;
            }
            if advance {
                if idx + 1 < n {
                    idx += 1;
                    continue 'outer;
                }
                break 'outer;
            }
            if let Some(t) = target.take() {
                idx = t;
                continue 'outer;
            }

            let mut pfds = [
                libc::pollfd { fd: wake_r, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 },
            ];
            let nfds: libc::nfds_t = if tty.is_some() { 2 } else { 1 };
            unsafe { libc::poll(pfds.as_mut_ptr(), nfds, -1) };
            if pfds[0].revents & libc::POLLIN != 0 {
                let mut buf = [0u8; 64];
                while unsafe { libc::read(wake_r, buf.as_mut_ptr() as *mut _, buf.len()) } > 0 {}
            }
            if nfds == 2 && pfds[1].revents & libc::POLLIN != 0 {
                let (sources, quit) = read_tty_sources();
                if quit {
                    break 'outer;
                }
                for src in sources {
                    let Some(cmd) = bindings.get(&Trigger { src, mods: 0 }).cloned() else { continue };
                    match cmd {
                        Command::Quit => break 'outer,
                        Command::Pause => p.toggle_pause(),
                        Command::Next if idx + 1 < n => target = Some(idx + 1),
                        Command::Prev if idx > 0 => target = Some(idx - 1),
                        Command::First => target = Some(0),
                        Command::Last => target = Some(n - 1),
                        Command::Playlist => {
                            for (i, e) in list.iter().enumerate() {
                                eprintln!("{} {:>3}. {}", if i == idx { ">" } else { " " }, i + 1, e.display_name());
                            }
                        }
                        Command::Seek(s) | Command::SeekOrStep(s) => p.seek_rel(s),
                        Command::Volume(d) => {
                            volume = (volume + d).clamp(0.0, 200.0);
                            p.set_volume(volume);
                            eprintln!("Hlasitosť: {volume:.0}%");
                        }
                        Command::Mute => {
                            muted = !muted;
                            p.set_mute(muted);
                            eprintln!("Zvuk: {}", if muted { "stlmený" } else { "zapnutý" });
                        }
                        Command::Info => {
                            let pos = p.position_secs().map(fmt_time).unwrap_or_else(|| "-".into());
                            let dur = p.duration_secs().map(fmt_time).unwrap_or_else(|| "LIVE".into());
                            eprintln!("{}  {pos} / {dur}", entry.path.display());
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}

struct Args {
    cfg: Config,
    files: Vec<String>,
    input_path: Option<PathBuf>,
    input_explicit: bool,
    nogui: bool,
}

fn parse_args() -> Result<Args> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cfg = Config::default();
    let mut config_dir: Option<PathBuf> = None;
    let mut cfg_file: Option<PathBuf> = None;
    let mut input_file: Option<PathBuf> = None;
    let (mut no_config, mut no_input) = (false, false);
    let mut overrides = Vec::new();
    let mut files = Vec::new();
    let mut only_files = false;
    let mut nogui = false;
    for a in args {
        if only_files || !a.starts_with("--") && a != "-h" {
            files.push(a);
        } else if a == "--" {
            only_files = true;
        } else if a == "-h" || a == "--help" {
            print!("{HELP}");
            std::process::exit(0);
        } else if a == "--no-config" {
            no_config = true;
        } else if let Some(p) = a.strip_prefix("--config-dir=") {
            config_dir = Some(PathBuf::from(p));
        } else if let Some(p) = a.strip_prefix("--config=") {
            cfg_file = Some(PathBuf::from(p));
        } else if let Some(p) = a.strip_prefix("--input-conf=") {
            input_file = Some(PathBuf::from(p));
        } else if a == "--no-input-conf" {
            no_input = true;
        } else if a == "--nogui" || a == "--no-window" {
            nogui = true;
        } else if a == "--window" {
            overrides.push(("fullscreen".to_string(), "no".to_string()));
        } else if a == "--slideshow" {
            overrides.push(("slideshow".to_string(), "yes".to_string()));
        } else if a == "--fullscreen" {
            overrides.push(("fullscreen".to_string(), "yes".to_string()));
        } else if let Some((k, v)) = a[2..].split_once('=') {
            overrides.push((k.to_string(), v.to_string()));
        } else {
            bail!("neznáma voľba '{a}' (skús --help)");
        }
    }
    let explicit_dir = config_dir.is_some();
    let dir = config_dir.or_else(|| config_path().and_then(|p| p.parent().map(|d| d.to_path_buf())));
    let cfg_explicit = cfg_file.is_some() || explicit_dir;
    let cfg_path = if no_config { None } else { cfg_file.or_else(|| dir.as_ref().map(|d| d.join("rviewer.conf"))) };
    let input_explicit = input_file.is_some() || explicit_dir;
    let input_path = if no_input { None } else { input_file.or_else(|| dir.as_ref().map(|d| d.join("input.conf"))) };
    if let Some(p) = cfg_path {
        match cfg.load_file(&p) {
            Ok(w) => {
                eprintln!("konfig: {}", p.display());
                w.iter().for_each(|w| eprintln!("{}: {w}", p.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !cfg_explicit => {}
            Err(e) => eprintln!("{}: {e}", p.display()),
        }
    }
    for (k, v) in overrides {
        cfg.set(&k, &v).map_err(|e| anyhow::anyhow!("--{k}: {e}"))?;
    }
    Ok(Args { cfg, files, input_path, input_explicit, nogui })
}

fn main() -> Result<()> {
    let Args { cfg, files, input_path, input_explicit, nogui } = parse_args()?;
    let mut bindings = if cfg.default_bindings { Bindings::defaults() } else { Bindings::default() };
    if let Some(p) = &input_path {
        match std::fs::read_to_string(p) {
            Ok(t) => {
                eprintln!("skratky: {}", p.display());
                bindings.load_str(&t).iter().for_each(|w| eprintln!("{}: {w}", p.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !input_explicit => {}
            Err(e) => eprintln!("{}: {e}", p.display()),
        }
    }
    let (list, start) = playlist::build(&files, &cfg);
    if list.is_empty() {
        bail!("žiadne súbory na zobrazenie");
    }
    gstreamer::init()?;

    // Prebúdzacia rúra pre vlákna (dekódovanie, GStreamer).
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
        bail!("pipe2 zlyhal");
    }
    let (wake_r, wake_w) = (fds[0], fds[1]);

    if nogui {
        return headless(list, start, cfg, bindings, wake_r, wake_w);
    }

    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<wl::State>(&conn)?;
    let qh = queue.handle();
    let mut st = wl::State::new(&globals, &qh, cfg.fullscreen)?;
    while !st.configured {
        queue.blocking_dispatch(&mut st)?;
    }
    // Pár udalostí navyše (výstupy, seat), aby bola známa mierka.
    queue.roundtrip(&mut st)?;
    let scale = st.scale();
    st.surface.set_buffer_scale(scale);
    st.resized = false;
    let (bw, bh) = (st.width * scale, st.height * scale);
    let gfx = gfx::Gfx::new(&conn, &st.surface, bw, bh, &cfg)?;

    let (tx, rx) = mpsc::channel();
    let (ptx, prx) = mpsc::channel();
    let mut view = View::new();
    view.set_screen(bw as f64, bh as f64);
    let volume = cfg.volume;
    let osd = Osd::load(&cfg);
    let video_fit = cfg.video_fit;
    let cfg_slideshow = cfg.slideshow;
    let cfg_loop = cfg.loop_file;
    let mut app = App {
        cfg,
        list,
        idx: start,
        nav_dir: 1,
        wl: st,
        gfx,
        view,
        scale,
        wake_w,
        generation: 0,
        ptx,
        prx,
        cache: Vec::new(),
        inflight: std::collections::HashSet::new(),
        tx,
        rx,
        content: Content::None,
        dirty: true,
        mods: 0,
        bindings,
        drag_button: None,
        peeking: false,
        center_at: None,
        pending_place: None,
        cursor: (bw as f64 / 2.0, bh as f64 / 2.0),
        dragging: None,
        axis_acc: 0.0,
        axis_t: Instant::now(),
        held: None,
        last_motion: Instant::now(),
        volume,
        muted: false,
        video_fit,
        osd,
        info_on: false,
        bar_until: None,
        bar_drag: false,
        seek_target: None,
        last_drag_seek: Instant::now(),
        cur_size: 0,
        toast: None,
        slideshow: cfg_slideshow,
        slide_next: None,
        loop_file: cfg_loop,
        plist_open: false,
        plist_sel: 0,
        plist_top: 0,
        plist_rows: 12,
        audio_only: false,
        mapped: false,
        map_deadline: Instant::now(),
        tty: None,
        thumber: None,
        thumb_failed: false,
        thumb_time: None,
        thumb_dim: None,
        thumb_pts: None,
    };
    unsafe {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::signal(sig, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    app.load_current();

    while app.wl.running {
        if QUIT.load(AtomicOrdering::SeqCst) {
            break;
        }
        queue.dispatch_pending(&mut app.wl)?;
        let inputs = std::mem::take(&mut app.wl.inputs);
        for i in inputs {
            app.handle_input(i);
        }
        app.apply_resize();
        app.poll_content();
        app.tick_timers();
        if app.dirty {
            app.render()?;
        }
        if !app.wl.running {
            break;
        }

        let _ = conn.flush();
        let Some(guard) = queue.prepare_read() else { continue };
        let wl_fd = guard.connection_fd().as_raw_fd();
        let timeout = match app.next_deadline() {
            Some(t) => t.saturating_duration_since(Instant::now()).as_millis().min(i32::MAX as u128 - 1) as i32 + 1,
            None => -1,
        };
        let mut pfds = [
            libc::pollfd { fd: wl_fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: wake_r, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 },
        ];
        let nfds: libc::nfds_t = if app.tty.is_some() { 3 } else { 2 };
        unsafe { libc::poll(pfds.as_mut_ptr(), nfds, timeout) };
        if pfds[0].revents & libc::POLLIN != 0 {
            if let Err(e) = guard.read() {
                if !matches!(&e, WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock) {
                    return Err(e.into());
                }
            }
        } else {
            drop(guard);
        }
        if nfds == 3 && pfds[2].revents & libc::POLLIN != 0 {
            app.handle_tty();
        }
        if pfds[1].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 64];
            while unsafe { libc::read(wake_r, buf.as_mut_ptr() as *mut _, buf.len()) } > 0 {}
        }
    }
    Ok(())
}
