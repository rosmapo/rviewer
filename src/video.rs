//! Video cez GStreamer: playbin -> appsink (NV12/I420), farby sa prevádzajú v shaderi.

use anyhow::{anyhow, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use std::os::fd::RawFd;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub struct VideoFrame {
    pub w: u32,
    pub h: u32,
    /// Pixel aspect ratio.
    pub par: (i32, i32),
    pub nv12: bool,
    pub full_range: bool,
    pub bt601: bool,
    /// Planes bez paddingu: Y, U, V (pri NV12 je [1] interleaved UV a [2] prázdne).
    pub planes: [Vec<u8>; 3],
}

type Slot = Arc<Mutex<Option<VideoFrame>>>;

/// Nastavenia titulkov pre prehrávač.
pub struct SubOpts {
    pub enabled: bool,
    /// URI externého súboru titulkov (suburi).
    pub ext_uri: Option<String>,
    pub encoding: String,
    pub font: String,
}

pub struct Player {
    pipeline: gst::Element,
    slot: Slot,
    pub paused: bool,
    pub eos: bool,
    /// Prišiel už aspoň jeden snímok? (chyba pred prvým snímkom = súbor sa nedá prehrať)
    pub got_frame: bool,
    sub_on: bool,
    sub_delay_ms: i64,
}

fn extract(sample: &gst::Sample) -> Option<VideoFrame> {
    let caps = sample.caps()?;
    let info = gst_video::VideoInfo::from_caps(caps).ok()?;
    let buffer = sample.buffer()?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info).ok()?;
    let (w, h) = (info.width() as usize, info.height() as usize);
    let nv12 = info.format() == gst_video::VideoFormat::Nv12;
    let (cw, ch) = ((w + 1) / 2, (h + 1) / 2);
    let dims: [(usize, usize); 3] = if nv12 { [(w, h), (cw * 2, ch), (0, 0)] } else { [(w, h), (cw, ch), (cw, ch)] };
    let nplanes = if nv12 { 2 } else { 3 };
    let mut planes: [Vec<u8>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for p in 0..nplanes {
        let data = frame.plane_data(p as u32).ok()?;
        let stride = *frame.plane_stride().get(p)? as usize;
        let (rw, rh) = dims[p];
        let mut out = Vec::with_capacity(rw * rh);
        for row in 0..rh {
            let s = row * stride;
            out.extend_from_slice(data.get(s..s + rw)?);
        }
        planes[p] = out;
    }
    let cm = info.colorimetry();
    let full_range = cm.range() == gst_video::VideoColorRange::Range0_255;
    let bt601 = match cm.matrix() {
        gst_video::VideoColorMatrix::Bt601 => true,
        gst_video::VideoColorMatrix::Bt709 | gst_video::VideoColorMatrix::Bt2020 => false,
        _ => h < 720,
    };
    let par = info.par();
    Some(VideoFrame {
        w: w as u32,
        h: h as u32,
        par: (par.numer().max(1), par.denom().max(1)),
        nv12,
        full_range,
        bt601,
        planes,
    })
}

fn store(slot: &Slot, sample: &gst::Sample, wake_fd: RawFd) {
    if let Some(f) = extract(sample) {
        *slot.lock().unwrap() = Some(f);
        crate::wake(wake_fd);
    }
}

impl Player {
    pub fn new(path: &Path, volume: f64, mute: bool, wake_fd: RawFd, subs: &SubOpts) -> Result<Player> {
        let uri = gst::glib::filename_to_uri(path, None)?;
        let caps = gst::Caps::builder("video/x-raw").field("format", gst::List::new(["NV12", "I420"])).build();
        let sink = gst_app::AppSink::builder().caps(&caps).build();
        let slot: Slot = Arc::new(Mutex::new(None));
        let (s1, s2) = (slot.clone(), slot.clone());
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    store(&s1, &sample, wake_fd);
                    Ok(gst::FlowSuccess::Ok)
                })
                .new_preroll(move |sink| {
                    let sample = sink.pull_preroll().map_err(|_| gst::FlowError::Eos)?;
                    store(&s2, &sample, wake_fd);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );

        let pipeline = gst::ElementFactory::make("playbin")
            .property("uri", uri.as_str())
            .property("video-sink", sink.to_value())
            .property("volume", volume / 100.0)
            .property("mute", mute)
            .build()?;

        // Rotácia podľa metadát (mobilné videá).
        if let Ok(flip) = gst::ElementFactory::make("videoflip").property_from_str("video-direction", "auto").build() {
            pipeline.set_property("video-filter", flip.to_value());
        }

        let bus = pipeline.bus().ok_or_else(|| anyhow!("pipeline nemá bus"))?;
        bus.set_sync_handler(move |_, _| {
            crate::wake(wake_fd);
            gst::BusSyncReply::Pass
        });
        // Titulky vykresľuje playbin priamo do snímkov (textoverlay / assrender), appsink dostane hotový obraz.
        if let Some(u) = &subs.ext_uri {
            pipeline.set_property("suburi", u.as_str());
        }
        if !subs.encoding.is_empty() {
            pipeline.set_property("subtitle-encoding", subs.encoding.as_str());
        }
        if !subs.font.is_empty() {
            pipeline.set_property("subtitle-font-desc", subs.font.as_str());
        }
        let p = Player { pipeline, slot, paused: false, eos: false, got_frame: false, sub_on: subs.enabled, sub_delay_ms: 0 };
        p.set_text_flag(subs.enabled);
        p.pipeline.set_state(gst::State::Playing)?;
        Ok(p)
    }

    /// Zapne/vypne vykresľovanie titulkov (príznak `text` v playbin `flags`).
    fn set_text_flag(&self, on: bool) {
        let flags = self.pipeline.property_value("flags");
        let Some(class) = gst::glib::FlagsClass::with_type(flags.type_()) else { return };
        let Some(builder) = class.builder_with_value(flags) else { return };
        let builder = if on { builder.set_by_nick("text") } else { builder.unset_by_nick("text") };
        if let Some(value) = builder.build() {
            self.pipeline.set_property_from_value("flags", &value);
        }
    }

    fn text_lang(&self, i: i32) -> Option<String> {
        let tags = self.pipeline.emit_by_name::<Option<gst::TagList>>("get-text-tags", &[&i])?;
        for key in ["language-name", "language-code"] {
            if let Some(v) = tags.generic(key) {
                if let Ok(s) = v.get::<&str>() {
                    return Some(s.to_string());
                }
            }
        }
        None
    }

    /// Popis stavu titulkov: "vypnuté", "žiadne" alebo "2/3 (English)".
    pub fn sub_label(&self) -> String {
        if !self.sub_on {
            return "vypnuté".into();
        }
        let n: i32 = self.pipeline.property("n-text");
        if n <= 0 {
            return "žiadne".into();
        }
        let cur: i32 = self.pipeline.property::<i32>("current-text").max(0);
        match self.text_lang(cur) {
            Some(l) => format!("{}/{} ({l})", cur + 1, n),
            None => format!("{}/{}", cur + 1, n),
        }
    }

    /// Ďalšia stopa titulkov; po poslednej sa vypnú.
    pub fn cycle_subtitle(&mut self) -> String {
        let n: i32 = self.pipeline.property("n-text");
        if n <= 0 {
            return "Titulky: žiadne".into();
        }
        if !self.sub_on {
            self.sub_on = true;
            self.set_text_flag(true);
            self.pipeline.set_property("current-text", 0i32);
        } else {
            let cur: i32 = self.pipeline.property("current-text");
            if cur + 1 < n {
                self.pipeline.set_property("current-text", cur + 1);
            } else {
                self.sub_on = false;
                self.set_text_flag(false);
            }
        }
        format!("Titulky: {}", self.sub_label())
    }

    /// Posun titulkov o `delta` sekúnd (kladné = neskôr).
    pub fn change_sub_delay(&mut self, delta: f64) -> String {
        self.sub_delay_ms += (delta * 1000.0).round() as i64;
        self.pipeline.set_property("text-offset", self.sub_delay_ms * 1_000_000);
        format!("Posun titulkov: {:+.1} s", self.sub_delay_ms as f64 / 1000.0)
    }

    pub fn take_frame(&mut self) -> Option<VideoFrame> {
        let f = self.slot.lock().unwrap().take();
        if f.is_some() {
            self.got_frame = true;
        }
        f
    }

    /// Spracuje správy zbernice; vráti chybové hlásenia.
    pub fn poll_bus(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(bus) = self.pipeline.bus() {
            while let Some(msg) = bus.pop() {
                match msg.view() {
                    gst::MessageView::Eos(_) => self.eos = true,
                    gst::MessageView::Error(e) => {
                        errors.push(format!("{} ({})", e.error(), e.debug().map(|d| d.to_string()).unwrap_or_default()))
                    }
                    _ => {}
                }
            }
        }
        errors
    }

    pub fn toggle_pause(&mut self) {
        if self.eos {
            // keep-open: po konci videa play začne odznova
            let _ = self.pipeline.seek_simple(gst::SeekFlags::FLUSH, gst::ClockTime::ZERO);
            self.eos = false;
            self.paused = false;
            let _ = self.pipeline.set_state(gst::State::Playing);
            return;
        }
        self.paused = !self.paused;
        let _ = self.pipeline.set_state(if self.paused { gst::State::Paused } else { gst::State::Playing });
    }

    pub fn seek_rel(&mut self, secs: f64) {
        let Some(pos) = self.pipeline.query_position::<gst::ClockTime>() else { return };
        let ns = (pos.nseconds() as f64 + secs * 1e9).max(0.0) as u64;
        if self
            .pipeline
            .seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT, gst::ClockTime::from_nseconds(ns))
            .is_ok()
        {
            self.eos = false;
        }
    }

    pub fn duration_secs(&self) -> Option<f64> {
        self.pipeline
            .query_duration::<gst::ClockTime>()
            .map(|d| d.nseconds() as f64 / 1e9)
            .filter(|d| *d > 0.0)
    }

    /// Posun na absolútnu pozíciu; `accurate` = presne (inak na najbližší keyframe, rýchle pri ťahaní).
    pub fn seek_abs(&mut self, secs: f64, accurate: bool) {
        let ns = (secs.max(0.0) * 1e9) as u64;
        let flags = gst::SeekFlags::FLUSH | if accurate { gst::SeekFlags::ACCURATE } else { gst::SeekFlags::KEY_UNIT };
        if self.pipeline.seek_simple(flags, gst::ClockTime::from_nseconds(ns)).is_ok() {
            self.eos = false;
        }
    }

    pub fn set_volume(&self, percent: f64) {
        self.pipeline.set_property("volume", percent / 100.0);
    }

    pub fn set_mute(&self, mute: bool) {
        self.pipeline.set_property("mute", mute);
    }

    pub fn position_secs(&self) -> Option<f64> {
        self.pipeline.query_position::<gst::ClockTime>().map(|p| p.nseconds() as f64 / 1e9)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
