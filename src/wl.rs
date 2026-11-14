//! Holý Wayland klient: okno (xdg-shell, fullscreen), vstup, skrývanie kurzora.

use anyhow::Result;
use wayland_client::{
    delegate_noop,
    globals::{GlobalList, GlobalListContents},
    protocol::{wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_surface},
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::wp::cursor_shape::v1::client::{wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

#[derive(Debug)]
pub enum Input {
    /// `sym0` = znak klávesy bez modifikátorov, `sym` = znak s modifikátormi (napr. '+' pri Shift+=).
    Key { code: u32, pressed: bool, sym0: Option<u32>, sym: Option<u32> },
    /// Fokus klávesnice stratený (pustiť všetky klávesy).
    Blur,
    Button { button: u32, pressed: bool },
    Motion { x: f64, y: f64 },
    Enter { x: f64, y: f64 },
    Leave,
    Axis(f64),
}

struct OutputInfo {
    output: wl_output::WlOutput,
    scale: i32,
    size: (i32, i32),
}

pub struct State {
    pub running: bool,
    pub configured: bool,
    /// Logická veľkosť okna.
    pub width: i32,
    pub height: i32,
    /// Skutočný stav podľa kompozitora (zo `states` v configure), nie len naša požiadavka.
    pub fullscreen: bool,
    pending_fullscreen: Option<bool>,
    /// Fullscreen sa zmenil (aj mimo nás, napr. F11 v kompozitore).
    pub fullscreen_changed: bool,
    pub resized: bool,
    pending_size: Option<(i32, i32)>,
    pub inputs: Vec<Input>,
    pub surface: wl_surface::WlSurface,
    pub toplevel: xdg_toplevel::XdgToplevel,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    shape_mgr: Option<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>,
    pub shape_dev: Option<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    compositor: wl_compositor::WlCompositor,
    shm: Option<wl_shm::WlShm>,
    /// Záloha, keď kompozitor nemá cursor-shape-v1: xcursor téma + vlastný povrch kurzora.
    cursor_theme: Option<wayland_cursor::CursorTheme>,
    cursor_surface: Option<wl_surface::WlSurface>,
    pointer_serial: u32,
    pub cursor_visible: bool,
    /// (rate/s, delay ms)
    pub repeat: (i32, i32),
    outputs: Vec<OutputInfo>,
    entered: Vec<wl_output::WlOutput>,
    xkb: crate::keymap::Xkb,
}

impl State {
    pub fn new(globals: &GlobalList, qh: &QueueHandle<State>, fullscreen: bool) -> Result<State> {
        let compositor: wl_compositor::WlCompositor = globals.bind(qh, 3..=4, ())?;
        let wm_base: xdg_wm_base::XdgWmBase = globals.bind(qh, 1..=2, ())?;
        let _seat: wl_seat::WlSeat = globals.bind(qh, 1..=5, ())?;
        let shm: Option<wl_shm::WlShm> = globals.bind(qh, 1..=1, ()).ok();
        let shape_mgr: Option<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1> = globals.bind(qh, 1..=1, ()).ok();

        let mut outputs = Vec::new();
        globals.contents().with_list(|list| {
            for g in list {
                if g.interface == "wl_output" {
                    let output = globals.registry().bind::<wl_output::WlOutput, _, _>(g.name, g.version.min(3), qh, ());
                    outputs.push(OutputInfo { output, scale: 1, size: (0, 0) });
                }
            }
        });

        let surface = compositor.create_surface(qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, qh, ());
        let toplevel = xdg_surface.get_toplevel(qh, ());
        toplevel.set_app_id("rviewer".into());
        toplevel.set_title("rviewer".into());
        if fullscreen {
            toplevel.set_fullscreen(None);
        }
        surface.commit();

        Ok(State {
            running: true,
            configured: false,
            width: 0,
            height: 0,
            fullscreen,
            pending_fullscreen: None,
            fullscreen_changed: false,
            resized: false,
            pending_size: None,
            inputs: Vec::new(),
            surface,
            toplevel,
            pointer: None,
            keyboard: None,
            shape_mgr,
            shape_dev: None,
            compositor,
            shm,
            cursor_theme: None,
            cursor_surface: None,
            pointer_serial: 0,
            cursor_visible: true,
            repeat: (25, 400),
            outputs,
            entered: Vec::new(),
            xkb: crate::keymap::Xkb::new(),
        })
    }

    /// Celočíselná mierka buffera (max cez výstupy, na ktorých je okno).
    pub fn scale(&self) -> i32 {
        let s = self
            .outputs
            .iter()
            .filter(|o| self.entered.contains(&o.output))
            .map(|o| o.scale)
            .max()
            .or_else(|| self.outputs.first().map(|o| o.scale))
            .unwrap_or(1);
        s.max(1)
    }

    /// Rozlíšenie výstupu, na ktorom je okno (v px buffera).
    pub fn output_size_px(&self) -> Option<(i32, i32)> {
        self.outputs
            .iter()
            .filter(|o| self.entered.contains(&o.output))
            .chain(self.outputs.iter())
            .map(|o| o.size)
            .find(|s| s.0 > 0 && s.1 > 0)
    }

    /// Požiada kompozitor o zmenu; skutočný stav príde v ďalšom configure.
    pub fn request_fullscreen(&self, on: bool) {
        if on {
            self.toplevel.set_fullscreen(None);
        } else {
            self.toplevel.unset_fullscreen();
        }
    }

    /// Môžeme kurzor skryť a potom vrátiť? (cursor-shape-v1 alebo xcursor téma)
    pub fn can_hide_cursor(&self) -> bool {
        self.pointer.is_some() && (self.shape_dev.is_some() || self.cursor_theme.is_some())
    }

    fn init_cursor_theme(&mut self, conn: &Connection, qh: &QueueHandle<State>) {
        let Some(shm) = self.shm.clone() else { return };
        let size = std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24);
        match wayland_cursor::CursorTheme::load(conn, shm, size) {
            Ok(t) => {
                self.cursor_theme = Some(t);
                self.cursor_surface = Some(self.compositor.create_surface(qh, ()));
            }
            Err(e) => eprintln!("rviewer: xcursor téma sa nenačítala, kurzor sa nebude skrývať: {e}"),
        }
    }

    pub fn apply_cursor(&mut self) {
        let Some(p) = self.pointer.clone() else { return };
        if !self.cursor_visible {
            p.set_cursor(self.pointer_serial, None, 0, 0);
            return;
        }
        if let Some(d) = &self.shape_dev {
            d.set_shape(self.pointer_serial, wp_cursor_shape_device_v1::Shape::Default);
            return;
        }
        let (Some(theme), Some(surf)) = (self.cursor_theme.as_mut(), self.cursor_surface.as_ref()) else { return };
        let name = ["default", "left_ptr", "arrow"].into_iter().find(|n| theme.get_cursor(n).is_some());
        let Some(name) = name else { return };
        let Some(cursor) = theme.get_cursor(name) else { return };
        let image = &cursor[0];
        let (hx, hy) = image.hotspot();
        let (iw, ih) = image.dimensions();
        surf.attach(Some(&**image), 0, 0);
        surf.damage_buffer(0, 0, iw as i32, ih as i32);
        surf.commit();
        p.set_cursor(self.pointer_serial, Some(surf), hx as i32, hy as i32);
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wp_cursor_shape_manager_v1::WpCursorShapeManagerV1);
delegate_noop!(State: ignore wp_cursor_shape_device_v1::WpCursorShapeDeviceV1);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(_: &mut Self, wm: &xdg_wm_base::XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(st: &mut Self, xs: &xdg_surface::XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            xs.ack_configure(serial);
            if let Some(fs) = st.pending_fullscreen.take() {
                if fs != st.fullscreen {
                    st.fullscreen = fs;
                    st.fullscreen_changed = true;
                }
            }
            let mut size = st.pending_size.take();
            if size.is_none() && st.width == 0 {
                // Kompozitor nechal veľkosť na nás: použi rozlíšenie výstupu.
                if !st.fullscreen {
                    size = Some((640, 480)); // okno: veľkosť určí obsah
                } else if let Some(o) = st.outputs.iter().find(|o| o.size.0 > 0) {
                    size = Some((o.size.0 / o.scale.max(1), o.size.1 / o.scale.max(1)));
                } else {
                    size = Some((1280, 720));
                }
            }
            if let Some((w, h)) = size {
                if (w, h) != (st.width, st.height) {
                    st.width = w;
                    st.height = h;
                    st.resized = true;
                }
            }
            st.configured = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(st: &mut Self, _: &xdg_toplevel::XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            xdg_toplevel::Event::Configure { width, height, states } => {
                let fs = states
                    .chunks_exact(4)
                    .any(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]) == xdg_toplevel::State::Fullscreen as u32);
                st.pending_fullscreen = Some(fs);
                if width > 0 && height > 0 {
                    st.pending_size = Some((width, height));
                }
            }
            xdg_toplevel::Event::Close => st.running = false,
            _ => {}
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(st: &mut Self, _: &wl_surface::WlSurface, event: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_surface::Event::Enter { output } => st.entered.push(output),
            wl_surface::Event::Leave { output } => st.entered.retain(|o| *o != output),
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(st: &mut Self, out: &wl_output::WlOutput, event: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some(o) = st.outputs.iter_mut().find(|o| o.output == *out) else { return };
        match event {
            wl_output::Event::Scale { factor } => o.scale = factor.max(1),
            wl_output::Event::Mode { flags, width, height, .. } => {
                if let WEnum::Value(f) = flags {
                    if f.contains(wl_output::Mode::Current) {
                        o.size = (width, height);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(st: &mut Self, seat: &wl_seat::WlSeat, event: wl_seat::Event, _: &(), conn: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) && st.pointer.is_none() {
                let p = seat.get_pointer(qh, ());
                if let Some(m) = &st.shape_mgr {
                    st.shape_dev = Some(m.get_pointer(&p, qh, ()));
                }
                st.pointer = Some(p);
                if st.shape_dev.is_none() {
                    st.init_cursor_theme(conn, qh);
                }
            }
            if caps.contains(wl_seat::Capability::Keyboard) && st.keyboard.is_none() {
                st.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(st: &mut Self, _: &wl_pointer::WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                st.pointer_serial = serial;
                st.apply_cursor();
                st.inputs.push(Input::Enter { x: surface_x, y: surface_y });
            }
            wl_pointer::Event::Leave { .. } => st.inputs.push(Input::Leave),
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                st.inputs.push(Input::Motion { x: surface_x, y: surface_y })
            }
            wl_pointer::Event::Button { button, state: bs, .. } => {
                if let WEnum::Value(bs) = bs {
                    st.inputs.push(Input::Button { button, pressed: bs == wl_pointer::ButtonState::Pressed });
                }
            }
            wl_pointer::Event::Axis { axis: WEnum::Value(wl_pointer::Axis::VerticalScroll), value, .. } => {
                st.inputs.push(Input::Axis(value))
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(st: &mut Self, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if let WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) = format {
                    use std::os::unix::fs::FileExt;
                    let file = std::fs::File::from(fd);
                    let mut buf = vec![0u8; size as usize];
                    if file.read_exact_at(&mut buf, 0).is_ok() {
                        while buf.last() == Some(&0) {
                            buf.pop();
                        }
                        if let Ok(text) = String::from_utf8(buf) {
                            st.xkb.set_keymap(text);
                        }
                    }
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                st.xkb.update_mask(mods_depressed, mods_latched, mods_locked, group);
            }
            wl_keyboard::Event::Key { key, state: ks, .. } => {
                if let WEnum::Value(ks) = ks {
                    let (sym0, sym) = st.xkb.syms(key);
                    st.inputs.push(Input::Key { code: key, pressed: ks == wl_keyboard::KeyState::Pressed, sym0, sym });
                }
            }
            wl_keyboard::Event::Leave { .. } => st.inputs.push(Input::Blur),
            wl_keyboard::Event::RepeatInfo { rate, delay } => st.repeat = (rate, delay),
            _ => {}
        }
    }
}
