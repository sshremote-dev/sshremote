//! GStreamer's H.265 decoders on Linux, as the system has them: VA-API, NVIDIA's, else libav's
//! or libde265's in software, behind h265parse and turned to RGB.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::Decoder;

/// The decoders tried, the video hardware's first, with settings for a picture out per access
/// unit in.
const DECODERS: &[(&str, &str)] = &[("vah265dec", ""), ("nvh265dec", ""), ("vaapih265dec", ""), ("avdec_h265", "max-threads=1"), ("libde265dec", "")];

fn found() -> Option<(&'static str, &'static str)> {
    crate::capture::ensure_session_env();
    gst::init().ok()?;
    DECODERS.iter().copied().find(|(n, _)| gst::ElementFactory::find(n).is_some())
}

pub fn has() -> bool {
    found().is_some()
}

pub struct Gst {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    sink: gst_app::AppSink,
    first: bool,
}

pub fn open() -> Result<Box<dyn Decoder>, String> {
    let (name, settings) = found().ok_or("this computer has no H.265 decoder for GStreamer")?;
    let desc = format!(
        "appsrc name=src is-live=true format=time caps=video/x-h265,stream-format=byte-stream,alignment=au \
         ! h265parse ! {name} {settings} ! videoconvert ! video/x-raw,format=RGB ! appsink name=sink sync=false max-buffers=8"
    );
    let pipeline = gst::parse::launch(&desc).map_err(|e| format!("{name}: {e}"))?.downcast::<gst::Pipeline>().map_err(|_| "no pipeline".to_string())?;
    let src = pipeline.by_name("src").and_then(|s| s.downcast::<gst_app::AppSrc>().ok()).ok_or("no source")?;
    let sink = pipeline.by_name("sink").and_then(|s| s.downcast::<gst_app::AppSink>().ok()).ok_or("no sink")?;
    pipeline.set_state(gst::State::Playing).map_err(|e| format!("{name} did not start: {e}"))?;
    Ok(Box::new(Gst { pipeline, src, sink, first: true }))
}

impl Decoder for Gst {
    fn decode(&mut self, au: &[u8], w: usize, h: usize, rgb: &mut Vec<u8>) -> Result<bool, String> {
        self.src.push_buffer(gst::Buffer::from_slice(au.to_vec())).map_err(|e| format!("H.265 decoder: {e}"))?;
        // The picture of this unit; a first one may take the decoder a while to set up.
        let wait = gst::ClockTime::from_mseconds(if std::mem::take(&mut self.first) { 2000 } else { 200 });
        let Some(sample) = self.sink.try_pull_sample(wait) else { return Ok(false) };
        let info = sample.caps().and_then(|c| gstreamer_video::VideoInfo::from_caps(c).ok()).ok_or("H.265 decoder: no picture size")?;
        let (pw, ph, stride) = (info.width() as usize, info.height() as usize, info.stride()[0] as usize);
        if (pw, ph) != (w, h) {
            return Err(format!("H.265 picture is {pw}x{ph}, expected {w}x{h}"));
        }
        let map = sample.buffer().ok_or("no picture")?.map_readable().map_err(|e| e.to_string())?;
        rgb.resize(w * h * 3, 0);
        for r in 0..h {
            rgb[r * w * 3..(r + 1) * w * 3].copy_from_slice(&map.as_slice()[r * stride..r * stride + w * 3]);
        }
        Ok(true)
    }
}

impl Drop for Gst {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
