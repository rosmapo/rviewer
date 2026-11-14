//! Dekódovanie obrázkov (v pracovnom vlákne). GIF animácie ako zoznam snímok.

use image::{imageops::FilterType, AnimationDecoder, DynamicImage, ImageDecoder, ImageReader};
use std::{fs::File, io::BufReader, path::Path};

pub struct LoadedImage {
    /// Veľkosť na zobrazenie (originál, po EXIF otočení).
    pub disp_w: u32,
    pub disp_h: u32,
    /// Skutočná veľkosť textúry (môže byť menšia, ak originál presahuje GL limit).
    pub tex_w: u32,
    pub tex_h: u32,
    /// (RGBA dáta, trvanie snímky v ms)
    pub frames: Vec<(Vec<u8>, u32)>,
}

pub fn load(path: &Path, max_tex: u32) -> Result<LoadedImage, String> {
    let is_gif = path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("gif")).unwrap_or(false);
    if is_gif {
        if let Ok(img) = load_gif(path, max_tex) {
            return Ok(img);
        }
    }
    load_static(path, max_tex)
}

fn load_static(path: &Path, max_tex: u32) -> Result<LoadedImage, String> {
    let reader = ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if let Ok(o) = orientation {
        img.apply_orientation(o);
    }
    let (dw, dh) = (img.width(), img.height());
    if dw > max_tex || dh > max_tex {
        img = img.resize(max_tex, max_tex, FilterType::Lanczos3);
    }
    let rgba = img.into_rgba8();
    let (tw, th) = rgba.dimensions();
    Ok(LoadedImage { disp_w: dw, disp_h: dh, tex_w: tw, tex_h: th, frames: vec![(rgba.into_raw(), 0)] })
}

fn load_gif(path: &Path, max_tex: u32) -> Result<LoadedImage, String> {
    let file = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let dec = image::codecs::gif::GifDecoder::new(file).map_err(|e| e.to_string())?;
    let mut frames = Vec::new();
    let (mut w, mut h) = (0, 0);
    let mut bytes = 0usize;
    for fr in dec.into_frames() {
        let fr = fr.map_err(|e| e.to_string())?;
        let (n, d) = fr.delay().numer_denom_ms();
        let mut ms = if d == 0 { 100 } else { n / d };
        if ms < 20 {
            ms = 100; // ako prehliadače
        }
        let buf = fr.into_buffer();
        w = buf.width();
        h = buf.height();
        if w > max_tex || h > max_tex {
            return Err("príliš veľký".into());
        }
        bytes += buf.len();
        if bytes > 768 << 20 {
            break; // limit pamäte
        }
        frames.push((buf.into_raw(), ms));
    }
    if frames.is_empty() {
        return Err("prázdny gif".into());
    }
    Ok(LoadedImage { disp_w: w, disp_h: h, tex_w: w, tex_h: h, frames })
}
