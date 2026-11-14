//! Skratky: parsovanie `input.conf` (kľúč príkaz argument) a vyhľadávanie podľa vstupu.

use std::collections::HashMap;

pub const CTRL: u8 = 1;
pub const SHIFT: u8 = 2;
pub const ALT: u8 = 4;

pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// Fyzická klávesa (evdev kód): ESC, šípky, F-klávesy, KP_*...
    Key(u32),
    /// Znak podľa aktuálneho rozloženia (xkb keysym), napr. '=' alebo 'q'.
    Sym(u32),
    Button(u32),
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Trigger {
    pub src: Source,
    pub mods: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Quit,
    Pause,
    Next,
    Prev,
    First,
    Last,
    Seek(f64),
    SeekOrStep(f64),
    Zoom(f64),
    ZoomReset,
    Volume(f64),
    Mute,
    Info,
    ToggleFullscreen,
    CycleFit,
    DragPan,
    Ignore,
}

impl Command {
    /// Opakuje sa pri držaní klávesy.
    pub fn repeatable(&self) -> bool {
        matches!(self, Command::Seek(_) | Command::SeekOrStep(_) | Command::Zoom(_) | Command::Volume(_))
    }
}

/// Evdev kód modifikátora -> bit.
pub fn modifier_bit(code: u32) -> Option<u8> {
    match code {
        29 | 97 => Some(CTRL),
        42 | 54 => Some(SHIFT),
        56 | 100 => Some(ALT),
        _ => None,
    }
}

fn key_code(name: &str) -> Option<u32> {
    let n = name.to_lowercase();
    if n.chars().count() == 1 {
        let c = n.chars().next().unwrap();
        for (row, first) in [("qwertyuiop", 16u32), ("asdfghjkl", 30), ("zxcvbnm", 44)] {
            if let Some(i) = row.find(c) {
                return Some(first + i as u32);
            }
        }
        if let Some(d) = c.to_digit(10) {
            return Some(if d == 0 { 11 } else { 1 + d });
        }
        return match c {
            '-' => Some(12),
            '=' => Some(13),
            '[' => Some(26),
            ']' => Some(27),
            ';' => Some(39),
            '\'' => Some(40),
            '`' => Some(41),
            '\\' => Some(43),
            ',' => Some(51),
            '.' => Some(52),
            '/' => Some(53),
            _ => None,
        };
    }
    if let Some(f) = n.strip_prefix('f').and_then(|x| x.parse::<u32>().ok()) {
        return match f {
            1..=10 => Some(58 + f),
            11 => Some(87),
            12 => Some(88),
            _ => None,
        };
    }
    if let Some(k) = n.strip_prefix("kp").and_then(|x| x.parse::<u32>().ok()) {
        return [82, 79, 80, 81, 75, 76, 77, 71, 72, 73].get(k as usize).copied();
    }
    Some(match n.as_str() {
        "esc" => 1,
        "bs" | "backspace" => 14,
        "tab" => 15,
        "enter" | "return" => 28,
        "space" => 57,
        "kp_mult" => 55,
        "kp_minus" => 74,
        "kp_plus" => 78,
        "kp_dec" => 83,
        "kp_enter" => 96,
        "kp_div" => 98,
        "home" => 102,
        "up" => 103,
        "pgup" => 104,
        "left" => 105,
        "right" => 106,
        "end" => 107,
        "down" => 108,
        "pgdwn" | "pgdn" => 109,
        "ins" | "insert" => 110,
        "del" | "delete" => 111,
        _ => return None,
    })
}

/// Keysym pre znak (Latin-1 = kód znaku, ostatné Unicode keysymy).
pub fn char_to_keysym(c: char) -> u32 {
    let cp = c as u32;
    if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) {
        cp
    } else {
        0x0100_0000 + cp
    }
}

/// Znak, ktorý by mala fyzická klávesa v US rozložení (záloha pre nelatinské rozloženia).
pub fn us_char(code: u32) -> Option<char> {
    "abcdefghijklmnopqrstuvwxyz0123456789-=[];'`\\,./".chars().find(|c| key_code(&c.to_string()) == Some(code))
}

pub fn parse_trigger(s: &str) -> Result<Trigger, String> {
    let mut rest = s.trim();
    let mut mods = 0u8;
    loop {
        let l = rest.to_lowercase();
        if l.starts_with("ctrl+") && rest.len() > 5 {
            mods |= CTRL;
            rest = &rest[5..];
        } else if l.starts_with("shift+") && rest.len() > 6 {
            mods |= SHIFT;
            rest = &rest[6..];
        } else if l.starts_with("alt+") && rest.len() > 4 {
            mods |= ALT;
            rest = &rest[4..];
        } else {
            break;
        }
    }
    let src = match rest.to_lowercase().as_str() {
        "mbtn_left" => Source::Button(BTN_LEFT),
        "mbtn_right" => Source::Button(BTN_RIGHT),
        "mbtn_middle" => Source::Button(BTN_MIDDLE),
        "wheel_up" => Source::WheelUp,
        "wheel_down" => Source::WheelDown,
        _ if rest.chars().count() == 1 => Source::Sym(char_to_keysym(rest.chars().next().unwrap().to_lowercase().next().unwrap())),
        _ => Source::Key(key_code(rest).ok_or_else(|| format!("neznáma klávesa '{rest}'"))?),
    };
    Ok(Trigger { src, mods })
}

pub fn parse_command(s: &str) -> Result<Command, String> {
    let mut it = s.split_whitespace();
    let name = it.next().ok_or("chýba príkaz")?;
    let arg = it.next();
    if it.next().is_some() {
        return Err(format!("priveľa argumentov v '{s}'"));
    }
    let num = || -> Result<f64, String> {
        arg.ok_or_else(|| format!("príkaz '{name}' potrebuje číselný argument"))?
            .parse::<f64>()
            .map_err(|_| format!("'{}' nie je číslo", arg.unwrap_or("")))
    };
    let none = |c: Command| -> Result<Command, String> {
        if arg.is_some() { Err(format!("príkaz '{name}' nemá argument")) } else { Ok(c) }
    };
    match name {
        "quit" => none(Command::Quit),
        "pause" => none(Command::Pause),
        "next" => none(Command::Next),
        "prev" => none(Command::Prev),
        "first" => none(Command::First),
        "last" => none(Command::Last),
        "seek" => Ok(Command::Seek(num()?)),
        "seek-or-step" => Ok(Command::SeekOrStep(num()?)),
        "zoom" => Ok(Command::Zoom(num()?)),
        "zoom-reset" => none(Command::ZoomReset),
        "volume" => Ok(Command::Volume(num()?)),
        "mute" => none(Command::Mute),
        "info" => none(Command::Info),
        "toggle-fullscreen" => none(Command::ToggleFullscreen),
        "cycle-fit" => none(Command::CycleFit),
        "drag-pan" => none(Command::DragPan),
        "ignore" | "nop" => none(Command::Ignore),
        _ => Err(format!("neznámy príkaz '{name}'")),
    }
}

#[derive(Default)]
pub struct Bindings {
    map: HashMap<Trigger, Command>,
}

impl Bindings {
    /// Vstavané predvolené skratky (súbor input.conf z repozitára).
    pub fn defaults() -> Bindings {
        let mut b = Bindings::default();
        for w in b.load_str(include_str!("../input.conf")) {
            eprintln!("vstavané skratky: {w}");
        }
        b
    }

    /// Načíta riadky; prepíše skratky s rovnakým vstupom. Vráti varovania.
    pub fn load_str(&mut self, text: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let mut line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(p) = line.find(" #").or_else(|| line.find("\t#")) {
                line = line[..p].trim();
            }
            let Some((key, cmd)) = line.split_once(char::is_whitespace) else {
                warnings.push(format!("riadok {}: chýba príkaz", n + 1));
                continue;
            };
            match (parse_trigger(key), parse_command(cmd.trim())) {
                (Ok(t), Ok(c)) => {
                    if c == Command::DragPan && !matches!(t.src, Source::Button(_)) {
                        warnings.push(format!("riadok {}: drag-pan sa dá priradiť len tlačidlu myši", n + 1));
                    } else {
                        self.map.insert(t, c);
                    }
                }
                (Err(e), _) | (_, Err(e)) => warnings.push(format!("riadok {}: {e}", n + 1)),
            }
        }
        warnings
    }

    pub fn get(&self, t: &Trigger) -> Option<&Command> {
        self.map.get(t).filter(|c| **c != Command::Ignore)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers() {
        assert_eq!(parse_trigger("ESC").unwrap(), Trigger { src: Source::Key(1), mods: 0 });
        assert_eq!(parse_trigger("Ctrl+=").unwrap(), Trigger { src: Source::Sym(0x3d), mods: CTRL });
        assert_eq!(parse_trigger("ctrl+shift+A").unwrap(), Trigger { src: Source::Sym(0x61), mods: CTRL | SHIFT });
        assert_eq!(parse_trigger("q").unwrap().src, Source::Sym(0x71));
        assert_eq!(parse_trigger("0").unwrap().src, Source::Sym(0x30));
        assert_eq!(us_char(12), Some('-'));
        assert_eq!(us_char(13), Some('='));
        assert_eq!(us_char(16), Some('q'));
        assert_eq!(us_char(1), None);
        assert_eq!(parse_trigger("KP0").unwrap().src, Source::Key(82));
        assert_eq!(parse_trigger("F11").unwrap().src, Source::Key(87));
        assert_eq!(parse_trigger("WHEEL_UP").unwrap().src, Source::WheelUp);
        assert!(parse_trigger("blabla").is_err());
    }

    #[test]
    fn defaults_load_cleanly() {
        let mut b = Bindings::default();
        let w = b.load_str(include_str!("../input.conf"));
        assert!(w.is_empty(), "{w:?}");
        let t = parse_trigger("Ctrl+=").unwrap();
        assert_eq!(b.get(&t), Some(&Command::Zoom(0.1)));
        // Ctrl+0 a 0 sú rôzne skratky
        assert_eq!(b.get(&parse_trigger("0").unwrap()), Some(&Command::Volume(2.0)));
        assert_eq!(b.get(&parse_trigger("Ctrl+0").unwrap()), Some(&Command::ZoomReset));
    }

    #[test]
    fn override_and_unbind() {
        let mut b = Bindings::default();
        b.load_str(include_str!("../input.conf"));
        let w = b.load_str("ESC ignore\nx quit   # komentár\nCtrl+WHEEL_UP zoom 0.2\nA drag-pan\nfoo bar\nLEFT seek\n");
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!(b.get(&parse_trigger("ESC").unwrap()), None);
        assert_eq!(b.get(&parse_trigger("x").unwrap()), Some(&Command::Quit));
        assert_eq!(b.get(&parse_trigger("Ctrl+WHEEL_UP").unwrap()), Some(&Command::Zoom(0.2)));
    }
}
