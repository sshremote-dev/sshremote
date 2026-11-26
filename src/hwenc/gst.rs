//! GStreamer's hardware H.264 encoders on Linux, as the system has them: VA-API (`vah264enc`,
//! `vaapih264enc`), NVIDIA's (`nvh264enc`) and Intel's (`qsvh264enc`), in a pipeline from NV12
//! pictures to constrained-baseline Annex B units, one out for each picture in.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use super::Encoder;

/// The encoders tried, best first, with their low-latency settings (each names them its own
/// way); `{kbps}` and `{gop}` filled in.
const ENCODERS: &[(&str, &str)] = &[
    ("vah264enc", "rate-control=cbr bitrate={kbps} key-int-max={gop} b-frames=0 target-usage=7"),
    ("vaapih264enc", "rate-control=cbr bitrate={kbps} keyframe-period={gop} max-bframes=0"),
    ("nvh264enc", "rc-mode=cbr bitrate={kbps} gop-size={gop} bframes=0 zerolatency=true"),
    ("qsvh264enc", "rate-control=cbr bitrate={kbps} gop-size={gop} b-frames=0"),
];

pub struct Gst {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    sink: gst_app::AppSink,
    name: String,
    size: (usize, usize),
    nv12: Vec<u8>,
    frame: u64,
    step: gst::ClockTime,
}

pub fn open(w: usize, h: usize, fps: u8, bitrate: u32) -> Result<Box<dyn Encoder>, String> {
    crate::capture::ensure_session_env();
    gst::init().map_err(|e| format!("GStreamer: {e}"))?;
    let fps = fps.max(1) as u32;
    let (kbps, gop) = ((bitrate / 1000).max(100).to_string(), (fps * 600).to_string());
    // Tests take a software encoder on a computer without the hardware.
    #[cfg(test)]
    let test = std::env::var("SSHREMOTE_TEST_H264").ok().and_then(|t| t.split_once(' ').map(|(n, s)| (n.to_string(), s.to_string())));
    #[cfg(not(test))]
    let test: Option<(String, String)> = None;
    let tried: Vec<(String, String)> = test.into_iter().chain(ENCODERS.iter().map(|&(n, s)| (n.into(), s.into()))).collect();
    let mut why = Vec::new();
    for (name, settings) in tried {
        if gst::ElementFactory::find(&name).is_none() {
            continue;
        }
        let settings = settings.replace("{kbps}", &kbps).replace("{gop}", &gop);
        // Its settings as asked, else as it comes (a version that names them otherwise).
        for s in [settings.as_str(), ""] {
            match start(&name, s, w, h, fps) {
                Ok(g) => return Ok(Box::new(g)),
                Err(e) => why.push(format!("{name}: {e}")),
            }
        }
    }
    Err(if why.is_empty() { "no video hardware here encodes H.264 through GStreamer".into() } else { why.join("; ") })
}

fn start(name: &str, settings: &str, w: usize, h: usize, fps: u32) -> Result<Gst, String> {
    let desc = format!(
        "appsrc name=src is-live=true format=time caps=video/x-raw,format=NV12,width={w},height={h},framerate={fps}/1 \
         ! {name} {settings} ! video/x-h264,profile=constrained-baseline \
         ! h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au \
         ! appsink name=sink sync=false max-buffers=8"
    );
    let pipeline = gst::parse::launch(&desc).map_err(|e| e.to_string())?.downcast::<gst::Pipeline>().map_err(|_| "no pipeline".to_string())?;
    let src = pipeline.by_name("src").and_then(|s| s.downcast::<gst_app::AppSrc>().ok()).ok_or("no source")?;
    let sink = pipeline.by_name("sink").and_then(|s| s.downcast::<gst_app::AppSink>().ok()).ok_or("no sink")?;
    pipeline.set_state(gst::State::Playing).map_err(|e| format!("did not start: {e}"))?;
    Ok(Gst {
        pipeline,
        src,
        sink,
        name: format!("GStreamer's {name}"),
        size: (w, h),
        nv12: vec![0; w * h * 3 / 2],
        frame: 0,
        step: gst::ClockTime::from_nseconds(1_000_000_000 / fps as u64),
    })
}

impl Encoder for Gst {
    fn encode(&mut self, y: &[u8], u: &[u8], v: &[u8], strides: (usize, usize, usize)) -> Result<Vec<u8>, String> {
        let (w, h) = self.size;
        for r in 0..h {
            self.nv12[r * w..(r + 1) * w].copy_from_slice(&y[r * strides.0..r * strides.0 + w]);
        }
        let uv = &mut self.nv12[w * h..];
        for r in 0..h / 2 {
            for c in 0..w / 2 {
                uv[r * w + 2 * c] = u[r * strides.1 + c];
                uv[r * w + 2 * c + 1] = v[r * strides.2 + c];
            }
        }
        let mut buf = gst::Buffer::from_slice(self.nv12.clone());
        if let Some(b) = buf.get_mut() {
            b.set_pts(self.step * self.frame);
            b.set_duration(self.step);
        }
        let first = self.frame == 0;
        self.frame += 1;
        self.src.push_buffer(buf).map_err(|e| format!("{}: {e}", self.name))?;
        // A first picture may take the hardware a while to set up.
        let wait = gst::ClockTime::from_mseconds(if first { 3000 } else { 250 });
        let sample = self.sink.try_pull_sample(wait).ok_or_else(|| format!("{}: no picture in time", self.name))?;
        let data = sample.buffer().ok_or("no picture")?.map_readable().map_err(|e| e.to_string())?.as_slice().to_vec();
        Ok(data)
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

impl Drop for Gst {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}
