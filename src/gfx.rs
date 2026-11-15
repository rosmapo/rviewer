//! EGL + OpenGL ES 2.0: vykreslenie jednej textúry (obrázok RGBA alebo video YUV) na pozadie.

use crate::config::{Config, Filter};
use crate::video::VideoFrame;
use anyhow::{anyhow, Result};
use khronos_egl as egl;
use std::ffi::{c_void, CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use wayland_client::{protocol::wl_surface::WlSurface, Connection, Proxy};
use wayland_egl::WlEglSurface;

// GLES2 konštanty, ktoré `gl` (core profil) nemusí exportovať.
const LUMINANCE: u32 = 0x1909;
const LUMINANCE_ALPHA: u32 = 0x190A;

const VS: &str = "
attribute vec2 a_pos;
uniform vec4 u_rect;
varying vec2 v_tc;
void main() {
    gl_Position = vec4(a_pos * u_rect.zw + u_rect.xy, 0.0, 1.0);
    v_tc = vec2(a_pos.x * 0.5 + 0.5, 0.5 - a_pos.y * 0.5);
}
";

// Overlay: výrez z atlasu (u_uv = u0,v0,u1,v1).
const VS_OVL: &str = "
attribute vec2 a_pos;
uniform vec4 u_rect;
uniform vec4 u_uv;
varying vec2 v_tc;
void main() {
    gl_Position = vec4(a_pos * u_rect.zw + u_rect.xy, 0.0, 1.0);
    vec2 t = vec2(a_pos.x * 0.5 + 0.5, 0.5 - a_pos.y * 0.5);
    v_tc = mix(u_uv.xy, u_uv.zw, t);
}
";

const FS_SOLID: &str = "
uniform vec4 u_color; // premultiplikovaná
void main() {
    gl_FragColor = u_color;
}
";

const FS_TEXT: &str = "
uniform sampler2D u_tex;
uniform vec4 u_color; // premultiplikovaná
void main() {
    gl_FragColor = u_color * texture2D(u_tex, v_tc).r;
}
";

const FS_PRELUDE: &str = "
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
varying vec2 v_tc;
";

const FS_RGBA: &str = "
uniform sampler2D u_tex;
uniform vec2 u_texel;   // veľkosť jedného cieľového pixelu v súradniciach textúry
uniform float u_sharp;  // sila doostrenia (0 = vypnuté)
void main() {
    vec4 c = texture2D(u_tex, v_tc);
    if (u_sharp > 0.0) {
        vec3 b = texture2D(u_tex, v_tc + vec2(u_texel.x, 0.0)).rgb
               + texture2D(u_tex, v_tc - vec2(u_texel.x, 0.0)).rgb
               + texture2D(u_tex, v_tc + vec2(0.0, u_texel.y)).rgb
               + texture2D(u_tex, v_tc - vec2(0.0, u_texel.y)).rgb;
        c.rgb = clamp(c.rgb + u_sharp * (c.rgb - b * 0.25), 0.0, 1.0);
    }
    gl_FragColor = vec4(c.rgb * c.a, c.a); // premultiplikované pre Wayland
}
";

const FS_YUV: &str = "
uniform sampler2D u_y;
uniform sampler2D u_u;
uniform sampler2D u_v;
uniform float u_nv12;
uniform vec4 u_k;      // rv, gu, gv, bu
uniform vec3 u_range;  // yoff, yscale, cscale
uniform vec2 u_texel;
uniform float u_sharp;
void main() {
    float y = texture2D(u_y, v_tc).r;
    if (u_sharp > 0.0) {
        float b = texture2D(u_y, v_tc + vec2(u_texel.x, 0.0)).r
                + texture2D(u_y, v_tc - vec2(u_texel.x, 0.0)).r
                + texture2D(u_y, v_tc + vec2(0.0, u_texel.y)).r
                + texture2D(u_y, v_tc - vec2(0.0, u_texel.y)).r;
        y = clamp(y + u_sharp * (y - b * 0.25), 0.0, 1.0);
    }
    vec4 c1 = texture2D(u_u, v_tc);
    vec4 c2 = texture2D(u_v, v_tc);
    float u = c1.r;
    float v = mix(c2.r, c1.a, u_nv12);
    float yy = (y - u_range.x) * u_range.y;
    float uu = (u - 0.5) * u_range.z;
    float vv = (v - 0.5) * u_range.z;
    vec3 rgb = vec3(yy + u_k.x * vv, yy - u_k.y * uu - u_k.z * vv, yy + u_k.w * uu);
    gl_FragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
";

struct Prog {
    id: u32,
    rect: i32,
    u: [i32; 8],
}

#[derive(Clone, Copy)]
enum Gpu {
    None,
    Rgba { w: u32, h: u32 },
    Yuv { w: u32, h: u32, nv12: bool, k: [f32; 4], range: [f32; 3] },
}

pub struct Gfx {
    egl: egl::DynamicInstance<egl::EGL1_4>,
    display: egl::Display,
    surface: egl::Surface,
    _context: egl::Context,
    wl_egl: WlEglSurface,
    prog_rgba: Prog,
    prog_yuv: Prog,
    prog_solid: Prog,
    prog_text: Prog,
    tex_atlas: u32,
    vbo: u32,
    tex_rgba: u32,
    tex_y: u32,
    tex_u: u32,
    tex_v: u32,
    tex_thumb: u32,
    pub max_tex: u32,
    npot_ok: bool,
    mag: i32,
    bg: [f32; 4],
    /// Sila doostrenia pri zmenšení (0..1).
    sharpen: f32,
    width: i32,
    height: i32,
    content: Gpu,
}

unsafe fn compile(kind: u32, src: &str) -> Result<u32> {
    let sh = gl::CreateShader(kind);
    let c = CString::new(src)?;
    gl::ShaderSource(sh, 1, &c.as_ptr(), ptr::null());
    gl::CompileShader(sh);
    let mut ok = 0;
    gl::GetShaderiv(sh, gl::COMPILE_STATUS, &mut ok);
    if ok == 0 {
        let mut len = 0;
        gl::GetShaderiv(sh, gl::INFO_LOG_LENGTH, &mut len);
        let mut buf = vec![0u8; len.max(1) as usize];
        gl::GetShaderInfoLog(sh, len, ptr::null_mut(), buf.as_mut_ptr() as *mut c_char);
        return Err(anyhow!("shader: {}", String::from_utf8_lossy(&buf)));
    }
    Ok(sh)
}

unsafe fn program(vs_src: &str, fs_body: &str, uniforms: &[&str]) -> Result<Prog> {
    let vs = compile(gl::VERTEX_SHADER, vs_src)?;
    let fs = compile(gl::FRAGMENT_SHADER, &format!("{FS_PRELUDE}{fs_body}"))?;
    let id = gl::CreateProgram();
    gl::AttachShader(id, vs);
    gl::AttachShader(id, fs);
    let a = CString::new("a_pos")?;
    gl::BindAttribLocation(id, 0, a.as_ptr());
    gl::LinkProgram(id);
    let mut ok = 0;
    gl::GetProgramiv(id, gl::LINK_STATUS, &mut ok);
    if ok == 0 {
        return Err(anyhow!("linkovanie shaderov zlyhalo"));
    }
    let loc = |n: &str| {
        let c = CString::new(n).unwrap();
        gl::GetUniformLocation(id, c.as_ptr())
    };
    let mut u = [-1; 8];
    for (i, n) in uniforms.iter().enumerate() {
        u[i] = loc(n);
    }
    Ok(Prog { id, rect: loc("u_rect"), u })
}

unsafe fn new_tex(mag: i32) -> u32 {
    let mut t = 0;
    gl::GenTextures(1, &mut t);
    gl::BindTexture(gl::TEXTURE_2D, t);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, mag);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
    t
}

/// Nahrá (alebo prepíše) jednu textúru na danej jednotke.
unsafe fn put(unit: u32, tex: u32, fmt: u32, w: u32, h: u32, data: &[u8], replace: bool) {
    gl::ActiveTexture(gl::TEXTURE0 + unit);
    gl::BindTexture(gl::TEXTURE_2D, tex);
    if replace {
        gl::TexSubImage2D(gl::TEXTURE_2D, 0, 0, 0, w as i32, h as i32, fmt, gl::UNSIGNED_BYTE, data.as_ptr() as *const c_void);
    } else {
        gl::TexImage2D(gl::TEXTURE_2D, 0, fmt as i32, w as i32, h as i32, 0, fmt, gl::UNSIGNED_BYTE, data.as_ptr() as *const c_void);
    }
}

impl Gfx {
    pub fn new(conn: &Connection, surface: &WlSurface, w: i32, h: i32, cfg: &Config) -> Result<Gfx> {
        let display_ptr = conn.backend().display_ptr() as *mut c_void;
        let egl = unsafe { egl::DynamicInstance::<egl::EGL1_4>::load_required() }
            .map_err(|e| anyhow!("nepodarilo sa načítať libEGL: {e:?}"))?;
        let display = unsafe { egl.get_display(display_ptr) }.ok_or_else(|| anyhow!("žiadny EGL display"))?;
        egl.initialize(display)?;
        egl.bind_api(egl::OPENGL_ES_API)?;
        let attr = [
            egl::SURFACE_TYPE, egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE, egl::OPENGL_ES2_BIT,
            egl::RED_SIZE, 8, egl::GREEN_SIZE, 8, egl::BLUE_SIZE, 8, egl::ALPHA_SIZE, 8,
            egl::NONE,
        ];
        let config = egl.choose_first_config(display, &attr)?.ok_or_else(|| anyhow!("žiadna vhodná EGL konfigurácia"))?;
        let context = egl.create_context(display, config, None, &[egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE])?;
        let wl_egl = WlEglSurface::new(surface.id(), w, h)?;
        let esurf = unsafe { egl.create_window_surface(display, config, wl_egl.ptr() as *mut c_void, None) }?;
        egl.make_current(display, Some(esurf), Some(esurf), Some(context))?;
        // Nečakať na frame callback (neblokovať, keď je okno skryté); kreslíme len pri zmene.
        let _ = egl.swap_interval(display, 0);

        gl::load_with(|name| egl.get_proc_address(name).map_or(ptr::null(), |p| p as *const c_void));

        let mag = if cfg.filter == Filter::Nearest { gl::NEAREST } else { gl::LINEAR } as i32;
        let (prog_rgba, prog_yuv, prog_solid, prog_text, tex_atlas, vbo, tex_rgba, tex_y, tex_u, tex_v, tex_thumb, max_tex, npot_ok);
        unsafe {
            prog_rgba = program(VS, FS_RGBA, &["u_tex", "u_texel", "u_sharp"])?;
            prog_yuv = program(VS, FS_YUV, &["u_y", "u_u", "u_v", "u_nv12", "u_k", "u_range", "u_texel", "u_sharp"])?;
            prog_solid = program(VS, FS_SOLID, &["u_color"])?;
            prog_text = program(VS_OVL, FS_TEXT, &["u_tex", "u_color", "u_uv"])?;
            let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            let mut b = 0;
            gl::GenBuffers(1, &mut b);
            gl::BindBuffer(gl::ARRAY_BUFFER, b);
            gl::BufferData(gl::ARRAY_BUFFER, 32, quad.as_ptr() as *const c_void, gl::STATIC_DRAW);
            vbo = b;
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            tex_rgba = new_tex(mag);
            tex_y = new_tex(mag);
            tex_u = new_tex(mag);
            tex_v = new_tex(mag);
            tex_thumb = new_tex(gl::LINEAR as i32);
            tex_atlas = new_tex(gl::LINEAR as i32);
            let mut m = 0;
            gl::GetIntegerv(gl::MAX_TEXTURE_SIZE, &mut m);
            max_tex = m.max(1024) as u32;
            let ext = gl::GetString(gl::EXTENSIONS);
            npot_ok = !ext.is_null()
                && CStr::from_ptr(ext as *const c_char).to_string_lossy().contains("GL_OES_texture_npot");
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);
        }
        let a = cfg.background[3];
        let bg = [cfg.background[0] * a, cfg.background[1] * a, cfg.background[2] * a, a];
        Ok(Gfx {
            egl, display, surface: esurf, _context: context, wl_egl,
            prog_rgba, prog_yuv, prog_solid, prog_text, tex_atlas, vbo, tex_rgba, tex_y, tex_u, tex_v, tex_thumb,
            max_tex, npot_ok, mag, bg, sharpen: cfg.sharpen.clamp(0.0, 1.0) as f32, width: w, height: h, content: Gpu::None,
        })
    }

    pub fn resize(&mut self, w: i32, h: i32) {
        self.wl_egl.resize(w, h, 0, 0);
        self.width = w;
        self.height = h;
    }

    pub fn clear_content(&mut self) {
        self.content = Gpu::None;
    }

    /// `replace` = ďalšia snímka animácie rovnakej veľkosti.
    pub fn upload_rgba(&mut self, w: u32, h: u32, data: &[u8], mips: bool, replace: bool) {
        let replace = replace && matches!(self.content, Gpu::Rgba { w: cw, h: ch } if cw == w && ch == h);
        unsafe {
            if !replace {
                gl::ActiveTexture(gl::TEXTURE0);
                gl::BindTexture(gl::TEXTURE_2D, self.tex_rgba);
                let min = if mips && self.npot_ok { gl::LINEAR_MIPMAP_LINEAR } else { gl::LINEAR };
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, min as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, self.mag);
            }
            put(0, self.tex_rgba, gl::RGBA, w, h, data, replace);
            if mips && self.npot_ok && !replace {
                gl::GenerateMipmap(gl::TEXTURE_2D);
            }
        }
        self.content = Gpu::Rgba { w, h };
    }

    /// Náhľad snímku z lišty (RGBA), vlastná textúra.
    pub fn upload_thumb(&mut self, w: u32, h: u32, data: &[u8]) {
        if data.len() < (w * h * 4) as usize {
            return;
        }
        unsafe {
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.tex_thumb);
            gl::TexImage2D(
                gl::TEXTURE_2D, 0, gl::RGBA as i32, w as i32, h as i32, 0, gl::RGBA, gl::UNSIGNED_BYTE,
                data.as_ptr() as *const c_void,
            );
        }
    }

    /// Nakreslí náhľad do obdĺžnika (px buffera, počiatok vľavo hore).
    pub fn draw_thumb(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let r = self.ndc(x, y, w, h);
        let p = &self.prog_rgba;
        unsafe {
            gl::UseProgram(p.id);
            gl::Uniform4f(p.rect, r[0], r[1], r[2], r[3]);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.tex_thumb);
            gl::Uniform1i(p.u[0], 0);
            gl::Uniform1f(p.u[2], 0.0);
            self.bind_quad();
            gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);
        }
    }

    pub fn upload_yuv(&mut self, f: &VideoFrame) {
        let (cw, ch) = ((f.w + 1) / 2, (f.h + 1) / 2);
        let replace = matches!(self.content, Gpu::Yuv { w, h, nv12, .. } if w == f.w && h == f.h && nv12 == f.nv12);
        unsafe {
            put(0, self.tex_y, LUMINANCE, f.w, f.h, &f.planes[0], replace);
            if f.nv12 {
                put(1, self.tex_u, LUMINANCE_ALPHA, cw, ch, &f.planes[1], replace);
            } else {
                put(1, self.tex_u, LUMINANCE, cw, ch, &f.planes[1], replace);
                put(2, self.tex_v, LUMINANCE, cw, ch, &f.planes[2], replace);
            }
        }
        let k = if f.bt601 { [1.402, 0.3441, 0.7141, 1.772] } else { [1.5748, 0.1873, 0.4681, 1.8556] };
        let range = if f.full_range { [0.0, 1.0, 1.0] } else { [16.0 / 255.0, 255.0 / 219.0, 255.0 / 224.0] };
        self.content = Gpu::Yuv { w: f.w, h: f.h, nv12: f.nv12, k, range };
    }

    /// `rect` = (left, top, w, h) v px buffera, počiatok vľavo hore.
    pub fn draw(&mut self, rect: (f64, f64, f64, f64)) -> Result<()> {
        let (sw, sh) = (self.width as f64, self.height as f64);
        let (l, t, w, h) = rect;
        let r = [
            ((l + w / 2.0) / sw * 2.0 - 1.0) as f32,
            (1.0 - (t + h / 2.0) / sh * 2.0) as f32,
            (w / sw) as f32,
            (h / sh) as f32,
        ];
        unsafe {
            gl::Viewport(0, 0, self.width, self.height);
            gl::ClearColor(self.bg[0], self.bg[1], self.bg[2], self.bg[3]);
            gl::Clear(gl::COLOR_BUFFER_BIT);
            let prog = match self.content {
                Gpu::None => None,
                Gpu::Rgba { .. } => Some(&self.prog_rgba),
                Gpu::Yuv { .. } => Some(&self.prog_yuv),
            };
            if let Some(p) = prog {
                gl::UseProgram(p.id);
                gl::Uniform4f(p.rect, r[0], r[1], r[2], r[3]);
                // Doostrenie len pri zmenšení (cieľ menší ako zdroj).
                let (cw, ch) = match self.content {
                    Gpu::Rgba { w, h } | Gpu::Yuv { w, h, .. } => (w as f64, h as f64),
                    Gpu::None => (1.0, 1.0),
                };
                let sharp = if self.sharpen > 0.0 && w > 0.0 && h > 0.0 && (w < cw || h < ch) { self.sharpen } else { 0.0 };
                let (tx, ty) = ((1.0 / w.max(1.0)) as f32, (1.0 / h.max(1.0)) as f32);
                match self.content {
                    Gpu::Rgba { .. } => {
                        gl::ActiveTexture(gl::TEXTURE0);
                        gl::BindTexture(gl::TEXTURE_2D, self.tex_rgba);
                        gl::Uniform1i(p.u[0], 0);
                        gl::Uniform2f(p.u[1], tx, ty);
                        gl::Uniform1f(p.u[2], sharp);
                    }
                    Gpu::Yuv { nv12, k, range, .. } => {
                        gl::ActiveTexture(gl::TEXTURE0);
                        gl::BindTexture(gl::TEXTURE_2D, self.tex_y);
                        gl::ActiveTexture(gl::TEXTURE1);
                        gl::BindTexture(gl::TEXTURE_2D, self.tex_u);
                        gl::ActiveTexture(gl::TEXTURE2);
                        gl::BindTexture(gl::TEXTURE_2D, if nv12 { self.tex_u } else { self.tex_v });
                        gl::Uniform1i(p.u[0], 0);
                        gl::Uniform1i(p.u[1], 1);
                        gl::Uniform1i(p.u[2], 2);
                        gl::Uniform1f(p.u[3], if nv12 { 1.0 } else { 0.0 });
                        gl::Uniform4f(p.u[4], k[0], k[1], k[2], k[3]);
                        gl::Uniform3f(p.u[5], range[0], range[1], range[2]);
                        gl::Uniform2f(p.u[6], tx, ty);
                        gl::Uniform1f(p.u[7], sharp);
                    }
                    Gpu::None => {}
                }
                gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
                gl::EnableVertexAttribArray(0);
                gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, 0, ptr::null());
                gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);
            }
        }
        Ok(())
    }

    /// Zobrazí nakreslený frame.
    pub fn present(&mut self) -> Result<()> {
        self.egl.swap_buffers(self.display, self.surface)?;
        Ok(())
    }

    fn ndc(&self, x: f32, y: f32, w: f32, h: f32) -> [f32; 4] {
        let (sw, sh) = (self.width as f32, self.height as f32);
        [(x + w / 2.0) / sw * 2.0 - 1.0, 1.0 - (y + h / 2.0) / sh * 2.0, w / sw, h / sh]
    }

    unsafe fn bind_quad(&self) {
        gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
        gl::EnableVertexAttribArray(0);
        gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, 0, ptr::null());
    }

    /// Vyplnený obdĺžnik (px buffera, počiatok vľavo hore), farba nepremultiplikovaná RGBA.
    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        let r = self.ndc(x, y, w, h);
        let p = &self.prog_solid;
        unsafe {
            gl::UseProgram(p.id);
            gl::Uniform4f(p.rect, r[0], r[1], r[2], r[3]);
            gl::Uniform4f(p.u[0], c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]);
            self.bind_quad();
            gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);
        }
    }

    /// Znovu vytvorí (vynuluje) atlas glyfov `size`×`size`.
    pub fn atlas_reset(&mut self, size: u32) {
        let zeros = vec![0u8; (size * size) as usize];
        unsafe {
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.tex_atlas);
            gl::TexImage2D(
                gl::TEXTURE_2D, 0, LUMINANCE as i32, size as i32, size as i32, 0, LUMINANCE, gl::UNSIGNED_BYTE,
                zeros.as_ptr() as *const c_void,
            );
        }
    }

    pub fn atlas_put(&mut self, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        unsafe {
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.tex_atlas);
            gl::TexSubImage2D(
                gl::TEXTURE_2D, 0, x as i32, y as i32, w as i32, h as i32, LUMINANCE, gl::UNSIGNED_BYTE,
                data.as_ptr() as *const c_void,
            );
        }
    }

    /// Jeden glyf z atlasu (`uv` = u0,v0,u1,v1), farba nepremultiplikovaná RGBA.
    pub fn glyph(&mut self, x: f32, y: f32, w: f32, h: f32, uv: [f32; 4], c: [f32; 4]) {
        let r = self.ndc(x, y, w, h);
        let p = &self.prog_text;
        unsafe {
            gl::UseProgram(p.id);
            gl::Uniform4f(p.rect, r[0], r[1], r[2], r[3]);
            gl::Uniform4f(p.u[2], uv[0], uv[1], uv[2], uv[3]);
            gl::Uniform4f(p.u[1], c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.tex_atlas);
            gl::Uniform1i(p.u[0], 0);
            self.bind_quad();
            gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);
        }
    }
}
