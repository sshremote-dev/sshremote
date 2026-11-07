//! System sound: the agent captures what the host plays and sends 20 ms Opus
//! packets (48 kHz stereo, 64 kbit/s); the viewer decodes and plays them from a
//! short buffer. Off until the viewer asks for it.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const RATE: u32 = 48_000;
/// Samples per channel in one 20 ms packet.
pub const FRAME: usize = 960;
const BITRATE: i32 = 64_000;

/// Converts interleaved samples at `from` Hz with `ch` channels to 48 kHz stereo, linearly.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Resampler {
    from: u32,
    ch: usize,
    pos: f64,
    last: [f32; 2],
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Resampler {
    pub fn new(from: u32, ch: usize) -> Resampler {
        Resampler { from, ch: ch.max(1), pos: 0.0, last: [0.0; 2] }
    }

    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let frames: Vec<[f32; 2]> =
            input.chunks(self.ch).map(|f| if f.len() >= 2 { [f[0], f[1]] } else { [f[0], f[0]] }).collect();
        if self.from == RATE {
            for f in &frames {
                out.extend_from_slice(f);
            }
            return;
        }
        let step = self.from as f64 / RATE as f64;
        while self.pos < frames.len() as f64 {
            let i = self.pos.floor() as usize;
            let t = (self.pos - i as f64) as f32;
            let a = if i == 0 { self.last } else { frames[i - 1] };
            let b = frames[i];
            out.push(a[0] + (b[0] - a[0]) * t);
            out.push(a[1] + (b[1] - a[1]) * t);
            self.pos += step;
        }
        self.pos -= frames.len() as f64;
        if let Some(l) = frames.last() {
            self.last = *l;
        }
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn encoder() -> Result<opus::Encoder, String> {
    let mut e =
        opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::Audio).map_err(|e| e.to_string())?;
    e.set_bitrate(opus::Bitrate::Bits(BITRATE)).map_err(|e| e.to_string())?;
    Ok(e)
}

/// The host's sound as Opus packets; capture stops when this is dropped.
pub struct Capture {
    pub packets: Receiver<Vec<u8>>,
    _keep: Box<dyn std::any::Any>,
}

/// Windows: loopback recording of the default output device through WASAPI.
#[cfg(windows)]
pub fn capture() -> Result<Capture, String> {
    let device = cpal::default_host().default_output_device().ok_or("no audio output device")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    if config.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!("unsupported sample format {:?}", config.sample_format()));
    }
    let (tx, rx) = mpsc::channel();
    let mut enc = encoder()?;
    let mut rs = Resampler::new(config.sample_rate(), config.channels() as usize);
    let mut pcm: Vec<f32> = Vec::new();
    let stream = device
        .build_input_stream::<f32, _, _>(
            config.config(),
            move |data: &[f32], _| {
                rs.push(data, &mut pcm);
                while pcm.len() >= FRAME * 2 {
                    if let Ok(p) = enc.encode_vec_float(&pcm[..FRAME * 2], 4000) {
                        let _ = tx.send(p);
                    }
                    pcm.drain(..FRAME * 2);
                }
            },
            |e| eprintln!("sshremote: audio capture: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Capture { packets: rx, _keep: Box::new(stream) })
}

/// Linux: the default sink's monitor through PulseAudio or PipeWire, encoded by GStreamer.
#[cfg(target_os = "linux")]
pub fn capture() -> Result<Capture, String> {
    use gstreamer as gst;
    use gstreamer::prelude::*;
    crate::capture::ensure_session_env();
    gst::init().map_err(|e| e.to_string())?;
    let desc = format!(
        "pulsesrc device=@DEFAULT_MONITOR@ ! audioconvert ! audioresample \
         ! audio/x-raw,rate={RATE},channels=2 ! opusenc bitrate={BITRATE} frame-size=20 \
         ! appsink name=sink sync=false"
    );
    let p = gst::parse::launch(&desc)
        .map_err(|e| format!("audio pipeline: {e}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| "audio pipeline".to_string())?;
    let sink =
        p.by_name("sink").and_then(|s| s.downcast::<gstreamer_app::AppSink>().ok()).ok_or("audio sink missing")?;
    p.set_state(gst::State::Playing).map_err(|e| format!("audio capture did not start: {e}"))?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(sample) = sink.pull_sample() {
            let Some(buf) = sample.buffer() else { continue };
            let Ok(map) = buf.map_readable() else { continue };
            if tx.send(map.as_slice().to_vec()).is_err() {
                break;
            }
        }
    });
    struct Stop(gst::Pipeline);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = self.0.set_state(gst::State::Null);
        }
    }
    Ok(Capture { packets: rx, _keep: Box::new(Stop(p)) })
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn capture() -> Result<Capture, String> {
    Err("sound capture is not available on this system".into())
}

/// Viewer side: decodes packets into a buffer the output device drains.
pub struct Player {
    dec: opus::Decoder,
    queue: Arc<Mutex<VecDeque<f32>>>,
    rs: Resampler,
    device_rate: u32,
    device_ch: usize,
    _stream: cpal::Stream,
}

impl Player {
    pub fn new() -> Result<Player, String> {
        let device = cpal::default_host().default_output_device().ok_or("no audio output device")?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        if config.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!("unsupported sample format {:?}", config.sample_format()));
        }
        let (device_rate, device_ch) = (config.sample_rate(), config.channels() as usize);
        let queue: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let q = queue.clone();
        let stream = device
            .build_output_stream::<f32, _, _>(
                config.config(),
                move |out: &mut [f32], _| {
                    let mut q = q.lock().unwrap();
                    for s in out.iter_mut() {
                        *s = q.pop_front().unwrap_or(0.0);
                    }
                },
                |e| eprintln!("sshremote: audio playback: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        let dec = opus::Decoder::new(RATE, opus::Channels::Stereo).map_err(|e| e.to_string())?;
        // The decoder's 48 kHz output is converted to the device rate by running the resampler backwards.
        let rs = Resampler { from: RATE, ch: 2, pos: 0.0, last: [0.0; 2] };
        Ok(Player { dec, queue, rs, device_rate, device_ch, _stream: stream })
    }

    pub fn push(&mut self, packet: &[u8]) {
        let mut pcm = vec![0f32; FRAME * 2 * 3];
        let Ok(n) = self.dec.decode_float(packet, &mut pcm, false) else { return };
        pcm.truncate(n * 2);
        let stereo = if self.device_rate == RATE {
            pcm
        } else {
            let mut out = Vec::with_capacity(pcm.len());
            self.rs.from = RATE;
            let step = RATE as f64 / self.device_rate as f64;
            let frames: Vec<[f32; 2]> = pcm.chunks(2).map(|f| [f[0], f[1]]).collect();
            while self.rs.pos < frames.len() as f64 {
                let i = self.rs.pos.floor() as usize;
                let t = (self.rs.pos - i as f64) as f32;
                let a = if i == 0 { self.rs.last } else { frames[i - 1] };
                let b = frames[i];
                out.push(a[0] + (b[0] - a[0]) * t);
                out.push(a[1] + (b[1] - a[1]) * t);
                self.rs.pos += step;
            }
            self.rs.pos -= frames.len() as f64;
            if let Some(l) = frames.last() {
                self.rs.last = *l;
            }
            out
        };
        let mut q = self.queue.lock().unwrap();
        for f in stereo.chunks(2) {
            // Left and right, then silence for any further channels of the device.
            q.extend(f.iter().copied().chain(std::iter::repeat(0.0)).take(self.device_ch));
        }
        // More than 300 ms queued means the link stalled; drop the oldest to stay live.
        let max = self.device_rate as usize * self.device_ch * 3 / 10;
        while q.len() > max {
            q.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_duration() {
        let mut rs = Resampler::new(44_100, 2);
        let mut out = Vec::new();
        for _ in 0..10 {
            rs.push(&vec![0.5f32; 4410 * 2], &mut out);
        }
        // One second at 44.1 kHz becomes about one second at 48 kHz.
        assert!((out.len() as i64 / 2 - 48_000).abs() < 10, "{}", out.len() / 2);
    }

    #[test]
    fn opus_round_trip() {
        let mut enc = encoder().unwrap();
        let tone: Vec<f32> = (0..FRAME * 2).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.3).collect();
        let p = enc.encode_vec_float(&tone, 4000).unwrap();
        assert!(p.len() < 400);
        let mut dec = opus::Decoder::new(RATE, opus::Channels::Stereo).unwrap();
        let mut out = vec![0f32; FRAME * 2];
        assert_eq!(dec.decode_float(&p, &mut out, false).unwrap(), FRAME);
    }
}
