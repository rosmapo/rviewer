//! Minimálny klient Wayfire IPC (unix socket z `WAYFIRE_SOCKET`, správy = 4 bajty dĺžky LE + JSON).
//!
//! Použitie v rviewer:
//! * `spawn_first_map_placer()` – pred zobrazením prvého okna odoberá udalosť `view-pre-map`; Wayfire okno
//!   podrží, kým klient nezavolá `unblock-map`, takže ho tu vycentrujeme ešte pred tým, než sa objaví.
//! * `place_self()` – pri zmene veľkosti (iný obrázok/video) nastaví veľkosť aj polohu naraz.
//!
//! Bez externého skriptu a bez bežiacej slučky okolo okna; mimo Wayfiru sa nič nedeje.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

// ---------------------------------------------------------------- JSON (len to, čo treba)

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(s: &str) -> Option<Json> {
        let mut p = Parser { b: s.as_bytes(), i: 0 };
        let v = p.value(0)?;
        p.ws();
        (p.i == p.b.len()).then_some(v)
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) => Some(*n as i64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(v) => Some(v),
            _ => None,
        }
    }

    fn int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(Json::as_i64)
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.b[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: u32) -> Option<Json> {
        if depth > 64 {
            return None;
        }
        self.ws();
        match *self.b.get(self.i)? {
            b'n' => self.eat("null").then_some(Json::Null),
            b't' => self.eat("true").then_some(Json::Bool(true)),
            b'f' => self.eat("false").then_some(Json::Bool(false)),
            b'"' => self.string().map(Json::Str),
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Some(Json::Arr(v));
                }
                loop {
                    v.push(self.value(depth + 1)?);
                    self.ws();
                    match *self.b.get(self.i)? {
                        b',' => self.i += 1,
                        b']' => {
                            self.i += 1;
                            return Some(Json::Arr(v));
                        }
                        _ => return None,
                    }
                }
            }
            b'{' => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Some(Json::Obj(v));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    if *self.b.get(self.i)? != b':' {
                        return None;
                    }
                    self.i += 1;
                    v.push((k, self.value(depth + 1)?));
                    self.ws();
                    match *self.b.get(self.i)? {
                        b',' => self.i += 1,
                        b'}' => {
                            self.i += 1;
                            return Some(Json::Obj(v));
                        }
                        _ => return None,
                    }
                }
            }
            _ => {
                let start = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[start..self.i]).ok()?.parse().ok().map(Json::Num)
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let s = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(s, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        if *self.b.get(self.i)? != b'"' {
            return None;
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.eat("\\u") {
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + lo.wrapping_sub(0xDC00);
                            }
                            char::from_u32(cp).unwrap_or('\u{FFFD}')
                        }
                        other => other as char,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }
}

// ---------------------------------------------------------------- spojenie

fn socket_path() -> Option<String> {
    std::env::var("WAYFIRE_SOCKET").ok().filter(|s| !s.is_empty())
}

/// Beží rviewer pod Wayfirom (je nastavený `WAYFIRE_SOCKET`)?
pub fn available() -> bool {
    socket_path().is_some()
}

struct Conn(UnixStream);

impl Conn {
    fn open(timeout: Option<Duration>) -> Result<Conn, String> {
        let path = socket_path().ok_or("WAYFIRE_SOCKET nie je nastavený")?;
        let s = UnixStream::connect(&path).map_err(|e| format!("{path}: {e}"))?;
        s.set_read_timeout(timeout).map_err(|e| e.to_string())?;
        s.set_write_timeout(Some(Duration::from_secs(2))).map_err(|e| e.to_string())?;
        Ok(Conn(s))
    }

    fn send(&mut self, method: &str, data: &str) -> Result<(), String> {
        let msg = format!("{{\"method\":\"{method}\",\"data\":{data}}}");
        let mut buf = (msg.len() as u32).to_le_bytes().to_vec();
        buf.extend_from_slice(msg.as_bytes());
        self.0.write_all(&buf).map_err(|e| e.to_string())
    }

    fn recv(&mut self) -> Result<Json, String> {
        let mut h = [0u8; 4];
        self.0.read_exact(&mut h).map_err(|e| e.to_string())?;
        let n = u32::from_le_bytes(h) as usize;
        if n > 16 << 20 {
            return Err("príliš veľká správa".into());
        }
        let mut body = vec![0u8; n];
        self.0.read_exact(&mut body).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&body);
        Json::parse(&text).ok_or_else(|| "neplatná odpoveď Wayfiru".to_string())
    }
}

fn check(j: Json) -> Result<Json, String> {
    match j.get("error").and_then(Json::as_str) {
        Some(e) => Err(e.to_string()),
        None => Ok(j),
    }
}

/// Jedna požiadavka na novom spojení.
fn request(method: &str, data: &str) -> Result<Json, String> {
    let mut c = Conn::open(Some(Duration::from_secs(2)))?;
    c.send(method, data)?;
    check(c.recv()?)
}

// ---------------------------------------------------------------- umiestnenie

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}

fn rect(j: &Json) -> Option<Rect> {
    Some(Rect { x: j.int("x")?, y: j.int("y")?, w: j.int("width")?, h: j.int("height")? })
}

/// Stred pracovnej oblasti výstupu (bez panelov) pre okno `w`×`h`. Súradnice okien sú v rámci výstupu vzhľadom na
/// aktuálnu pracovnú plochu (nezahŕňajú jej posun), preto sa číslo plochy nepripočítava.
fn centered(out: &Json, w: i64, h: i64) -> Option<(i64, i64)> {
    let wa = out.get("workarea").and_then(rect).or_else(|| out.get("geometry").and_then(rect))?;
    Some((wa.x + (wa.w - w) / 2, wa.y + (wa.h - h) / 2))
}

/// Výstup, na ktorom je okno (alebo zaostrený výstup, ak okno ešte nemá výstup).
fn output_for(view: &Json) -> Option<Json> {
    let outs = request("window-rules/list-outputs", "{}").ok()?;
    let outs = outs.as_arr()?;
    let want = view.int("output-id").filter(|&i| i >= 0).or_else(|| {
        request("window-rules/get-focused-output", "{}").ok()?.get("info")?.int("id")
    });
    want.and_then(|id| outs.iter().find(|o| o.int("id") == Some(id))).or_else(|| outs.first()).cloned()
}

fn configure(view_id: i64, r: Rect) -> Result<(), String> {
    let data = format!(
        "{{\"id\":{view_id},\"geometry\":{{\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}}}",
        r.x, r.y, r.w, r.h
    );
    request("window-rules/configure-view", &data).map(|_| ())
}

/// Nastaví veľkosť `w`×`h` (logické px) a vycentruje okno, ktoré patrí tomuto procesu.
pub fn place_self(w: i32, h: i32) -> Result<(), String> {
    let pid = std::process::id() as i64;
    let views = request("window-rules/list-views", "{}")?;
    let view = views
        .as_arr()
        .ok_or("neočakávaná odpoveď list-views")?
        .iter()
        .find(|v| v.int("pid") == Some(pid) && v.get("mapped") != Some(&Json::Bool(false)))
        .ok_or("okno rviewera sa vo Wayfire nenašlo")?;
    let id = view.int("id").ok_or("okno bez id")?;
    // Geometria okna vo Wayfire zahŕňa aj dekoráciu (rámik, titulok); veľkosť obsahu je `base-geometry`.
    // Cieľová veľkosť obsahu `w`×`h` sa preto musí zväčšiť o tento rozdiel, inak by sa okno po každej zmene zmenšovalo.
    let cur = view.get("geometry").and_then(rect);
    let (dw, dh) = match (cur, view.get("base-geometry").and_then(rect)) {
        (Some(g), Some(b)) if (0..400).contains(&(g.w - b.w)) && (0..400).contains(&(g.h - b.h)) => (g.w - b.w, g.h - b.h),
        _ => (0, 0),
    };
    let (w, h) = (w as i64 + dw, h as i64 + dh);
    let out = output_for(view).ok_or("žiadny výstup")?;
    let (x, y) = centered(&out, w, h).ok_or("výstup bez geometrie")?;
    let target = Rect { x, y, w, h };
    if std::env::var_os("RVIEWER_DEBUG").is_some() {
        eprintln!("wayfire ipc: okno {id}: teraz {cur:?}, dekorácia {dw}x{dh}, cieľ {target:?}, výstup {:?}", out.get("workarea"));
    }
    if cur == Some(target) {
        return Ok(());
    }
    configure(id, target)
}

/// Pred zobrazením okna ho vycentruje: Wayfire okno po `view-pre-map` podrží, kým nezavoláme `unblock-map`.
/// Vlákno skončí po vlastnom okne (spojenie sa zavrie, takže sa odber zruší). Cudzie okná odblokuje hneď.
pub fn spawn_first_map_placer() {
    if !available() {
        return;
    }
    std::thread::spawn(|| {
        if let Err(e) = premap_loop() {
            eprintln!("wayfire ipc: {e}");
        }
    });
}

fn premap_loop() -> Result<(), String> {
    let mut c = Conn::open(None)?;
    c.send("window-rules/events/watch", "{\"events\":[\"view-pre-map\"]}")?;
    check(c.recv()?)?;
    let pid = std::process::id() as i64;
    loop {
        let msg = c.recv()?;
        if msg.get("event").and_then(Json::as_str) != Some("view-pre-map") {
            continue;
        }
        let Some(view) = msg.get("view") else { continue };
        let Some(id) = view.int("id") else { continue };
        let mine = view.int("pid") == Some(pid);
        if mine {
            let res = (|| {
                let g = view.get("geometry").and_then(rect).ok_or("okno bez geometrie")?;
                let out = output_for(view).ok_or("žiadny výstup")?;
                let (x, y) = centered(&out, g.w, g.h).ok_or("výstup bez geometrie")?;
                if (x, y) != (g.x, g.y) {
                    configure(id, Rect { x, y, ..g })?;
                }
                Ok::<(), String>(())
            })();
            if let Err(e) = res {
                eprintln!("wayfire ipc: umiestnenie okna: {e}");
            }
        }
        // Vždy odblokovať, inak by sa okno (aj cudzie) nezobrazilo.
        if let Err(e) = request("window-rules/unblock-map", &format!("{{\"id\":{id}}}")) {
            eprintln!("wayfire ipc: unblock-map: {e}");
        }
        if mine {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_view_list() {
        let j = Json::parse(
            r#"[{"id":5,"pid":123,"title":"Kôň \"x\" é😀","geometry":{"x":-3,"y":10,"width":800,"height":600.0},"mapped":true,"parent":-1}]"#,
        )
        .unwrap();
        let v = &j.as_arr().unwrap()[0];
        assert_eq!(v.int("pid"), Some(123));
        assert_eq!(v.get("title").unwrap().as_str(), Some("Kôň \"x\" é😀"));
        assert_eq!(rect(v.get("geometry").unwrap()), Some(Rect { x: -3, y: 10, w: 800, h: 600 }));
        assert_eq!(v.get("mapped"), Some(&Json::Bool(true)));
    }

    #[test]
    fn parse_errors_and_edge() {
        assert!(Json::parse("").is_none());
        assert!(Json::parse("{").is_none());
        assert!(Json::parse("[1,]").is_none());
        assert_eq!(Json::parse(" [ ] "), Some(Json::Arr(vec![])));
        assert_eq!(Json::parse("{\"a\":null}").unwrap().get("a"), Some(&Json::Null));
        assert!(Json::parse(&"[".repeat(200)).is_none());
    }

    #[test]
    fn center_math() {
        let out = Json::parse(
            r#"{"id":1,"geometry":{"x":0,"y":0,"width":1920,"height":1080},"workarea":{"x":0,"y":40,"width":1920,"height":1040},"workspace":{"x":0,"y":0}}"#,
        )
        .unwrap();
        assert_eq!(centered(&out, 800, 600), Some((560, 40 + 220)));
    }

    fn rd(s: &mut UnixStream) -> Option<Json> {
        let mut h = [0u8; 4];
        s.read_exact(&mut h).ok()?;
        let mut b = vec![0u8; u32::from_le_bytes(h) as usize];
        s.read_exact(&mut b).ok()?;
        Json::parse(&String::from_utf8_lossy(&b))
    }

    fn wr(s: &mut UnixStream, text: &str) {
        let mut b = (text.len() as u32).to_le_bytes().to_vec();
        b.extend_from_slice(text.as_bytes());
        s.write_all(&b).unwrap();
    }

    /// Falošný Wayfire: pošle `view-pre-map` pre cudzie a potom pre naše okno a skontroluje odpovede klienta.
    #[test]
    fn premap_flow() {
        use std::os::unix::net::UnixListener;
        use std::sync::{Arc, Mutex};
        let path = std::env::temp_dir().join(format!("wfipc-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        std::env::set_var("WAYFIRE_SOCKET", &path);
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let pid = std::process::id();
        let log2 = log.clone();
        let server = std::thread::spawn(move || {
            // Prvé spojenie = odber udalostí.
            let (mut watch, _) = listener.accept().unwrap();
            let req = rd(&mut watch).unwrap();
            assert_eq!(req.get("method").and_then(Json::as_str), Some("window-rules/events/watch"));
            wr(&mut watch, "{\"result\":\"ok\"}");
            wr(&mut watch, "{\"event\":\"view-focused\",\"view\":null}");
            wr(&mut watch, "{\"event\":\"view-pre-map\",\"view\":{\"id\":7,\"pid\":1,\"geometry\":{\"x\":0,\"y\":0,\"width\":10,\"height\":10}}}");
            let mine = format!("{{\"event\":\"view-pre-map\",\"view\":{{\"id\":9,\"pid\":{pid},\"output-id\":-1,\"geometry\":{{\"x\":0,\"y\":0,\"width\":800,\"height\":600}}}}}}");
            wr(&mut watch, &mine);
            // Ďalšie spojenia = jednotlivé požiadavky, kým klient nezavrie odber.
            for conn in listener.incoming() {
                let mut c = conn.unwrap();
                let Some(req) = rd(&mut c) else { continue };
                let m = req.get("method").and_then(Json::as_str).unwrap().to_string();
                let d = req.get("data").unwrap();
                let reply = match m.as_str() {
                    "window-rules/list-outputs" => "[{\"id\":1,\"geometry\":{\"x\":0,\"y\":0,\"width\":1920,\"height\":1080},\"workarea\":{\"x\":0,\"y\":40,\"width\":1920,\"height\":1040},\"workspace\":{\"x\":0,\"y\":0}}]".to_string(),
                    "window-rules/get-focused-output" => "{\"result\":\"ok\",\"info\":{\"id\":1}}".to_string(),
                    _ => "{\"result\":\"ok\"}".to_string(),
                };
                let g = d.get("geometry").map(|g| format!("{},{}", g.int("x").unwrap(), g.int("y").unwrap()));
                log2.lock().unwrap().push(format!("{m} id={:?} g={:?}", d.int("id"), g));
                wr(&mut c, &reply);
                if m == "window-rules/unblock-map" && d.int("id") == Some(9) {
                    break;
                }
            }
        });
        premap_loop().unwrap();
        server.join().unwrap();
        let log = log.lock().unwrap().clone();
        assert_eq!(log.first().map(String::as_str), Some("window-rules/unblock-map id=Some(7) g=None"));
        assert!(log.contains(&"window-rules/configure-view id=Some(9) g=Some(\"560,260\")".to_string()), "{log:?}");
        assert_eq!(log.last().map(String::as_str), Some("window-rules/unblock-map id=Some(9) g=None"));
        let _ = std::fs::remove_file(&path);
    }
}
