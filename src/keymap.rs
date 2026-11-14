//! Tenký obal nad xkbcommon: z kódu klávesy zistí znak podľa aktuálneho rozloženia.

use xkbcommon::xkb;

pub struct Xkb {
    ctx: xkb::Context,
    keymap: Option<xkb::Keymap>,
    state: Option<xkb::State>,
}

impl Xkb {
    pub fn new() -> Xkb {
        Xkb { ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS), keymap: None, state: None }
    }

    pub fn set_keymap(&mut self, text: String) {
        if let Some(km) =
            xkb::Keymap::new_from_string(&self.ctx, text, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS)
        {
            self.state = Some(xkb::State::new(&km));
            self.keymap = Some(km);
        }
    }

    pub fn update_mask(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        if let Some(s) = &mut self.state {
            s.update_mask(depressed, latched, locked, 0, 0, group);
        }
    }

    /// (keysym bez modifikátorov v aktuálnej skupine, keysym s modifikátormi) pre evdev kód.
    pub fn syms(&self, evdev: u32) -> (Option<u32>, Option<u32>) {
        let (Some(km), Some(st)) = (&self.keymap, &self.state) else { return (None, None) };
        let kc: xkb::Keycode = (evdev + 8).into();
        let produced = u32::from(st.key_get_one_sym(kc));
        let layout = st.key_get_layout(kc);
        let base = km.key_get_syms_by_level(kc, layout, 0).first().map(|k| u32::from(*k)).filter(|s| *s != 0);
        (base, if produced == 0 { None } else { Some(produced) })
    }
}
