//! Recording a remote screen as it shows here, into an MP4 file (H.264) any player opens. Frames
//! are encoded on a thread of their own, at most 15 a second and only when the picture changed;
//! the file is written as it goes and its index added at the end.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use openh264::OpenH264API;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, RateControlMode, UsageType};
use openh264::formats::{RgbSliceU8, YUVBuffer};

/// The most frames a second a recording keeps.
const FPS: u32 = 15;

enum Job {
    Frame(Vec<u8>, usize, usize, Instant),
    Stop(SyncSender<Result<Vec<PathBuf>, String>>),
}

/// A recording going on; `stop` ends it and says where it went.
pub struct Recording {
    jobs: SyncSender<Job>,
    pub started: Instant,
    last: Option<Instant>,
    /// The picture changed since the last frame was kept.
    pub dirty: bool,
}

impl Recording {
    /// Starts one, its file named after `what` in the Videos folder (or Downloads).
    pub fn start(what: &str) -> Recording {
        let (jobs, rx) = mpsc::sync_channel(2);
        let base = name(what);
        std::thread::spawn(move || encode(rx, base));
        Recording { jobs, started: Instant::now(), last: None, dirty: true }
    }

    /// Starts one of a session by ID in `dir` (made if need be), the time in its name.
    pub fn session(dir: &Path) -> Result<Recording, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let base = name("session");
        let file = base.file_name().ok_or("no name")?;
        let base = dir.join(file);
        let (jobs, rx) = mpsc::sync_channel(2);
        std::thread::spawn(move || encode(rx, base));
        Ok(Recording { jobs, started: Instant::now(), last: None, dirty: true })
    }

    /// Hands a captured picture over (BGRx rows `stride` apart), when one is due.
    pub fn picture(&mut self, img: &crate::capture::Image) {
        self.dirty = true;
        if !self.due() || img.w < 16 || img.h < 16 {
            return;
        }
        let mut rgb = Vec::with_capacity(img.w * img.h * 3);
        for row in img.data.chunks(img.stride).take(img.h) {
            for px in row[..img.w * 4].chunks_exact(4) {
                rgb.extend_from_slice(&[px[2], px[1], px[0]]);
            }
        }
        let now = Instant::now();
        if self.jobs.try_send(Job::Frame(rgb, img.w, img.h, now)).is_ok() {
            (self.last, self.dirty) = (Some(now), false);
        }
    }

    /// Whether a frame would be kept now (the picture changed, and the last one is old enough).
    pub fn due(&self) -> bool {
        self.dirty && self.last.is_none_or(|t| t.elapsed() >= Duration::from_millis(1000 / FPS as u64))
    }

    /// Hands the picture over (RGB, three bytes a pixel); dropped when the encoder is behind.
    pub fn frame(&mut self, rgb: &[u8], w: usize, h: usize) {
        if w < 16 || h < 16 {
            return;
        }
        let now = Instant::now();
        if self.jobs.try_send(Job::Frame(rgb.to_vec(), w, h, now)).is_ok() {
            (self.last, self.dirty) = (Some(now), false);
        }
    }

    /// Ends it: the files written (a new one began wherever the picture changed size).
    pub fn stop(self) -> Result<Vec<PathBuf>, String> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.jobs.send(Job::Stop(tx)).map_err(|_| "the recording ended early".to_string())?;
        rx.recv().map_err(|_| "the recording ended early".to_string())?
    }
}

/// A recording left (its pane closed) still gets its index and stays playable.
impl Drop for Recording {
    fn drop(&mut self) {
        let (tx, _) = mpsc::sync_channel(1);
        let _ = self.jobs.send(Job::Stop(tx));
    }
}

/// Where a recording goes: "sshremote-HOST-20261113-081500.mp4".
fn name(what: &str) -> PathBuf {
    let shot = crate::shot::save_path();
    let stamp = shot.file_stem().map(|s| s.to_string_lossy().trim_start_matches("sshremote-").to_string()).unwrap_or_default();
    let what: String = what.chars().map(|c| if c.is_alphanumeric() || c == '.' { c } else { '-' }).collect();
    let downloads = crate::files::downloads();
    let videos = downloads.parent().map(|h| h.join("Videos")).filter(|p| p.is_dir());
    videos.unwrap_or(downloads).join(format!("sshremote-{}-{stamp}.mp4", what.trim_matches('-')))
}

fn encode(rx: Receiver<Job>, base: PathBuf) {
    let mut done: Vec<PathBuf> = Vec::new();
    let mut file: Option<Mp4> = None;
    let mut failed: Option<String> = None;
    while let Ok(job) = rx.recv() {
        match job {
            Job::Frame(rgb, w, h, at) => {
                if failed.is_some() {
                    continue;
                }
                let (ew, eh) = (w & !1, h & !1);
                // A picture of another size starts a file of its own.
                if file.as_ref().is_some_and(|f| (f.w, f.h) != (ew, eh)) {
                    match file.take().map(|f| f.finish(at)) {
                        Some(Ok(p)) => done.push(p),
                        Some(Err(e)) => failed = Some(e),
                        None => {}
                    }
                }
                if file.is_none() {
                    let path = if done.is_empty() { base.clone() } else { base.with_extension(format!("{}.mp4", done.len() + 1)) };
                    match Mp4::create(&path, ew, eh, at) {
                        Ok(f) => file = Some(f),
                        Err(e) => {
                            failed = Some(e);
                            continue;
                        }
                    }
                }
                let f = file.as_mut().expect("made above");
                if let Err(e) = f.add(&rgb, w, at) {
                    failed = Some(e);
                }
            }
            Job::Stop(reply) => {
                if let Some(f) = file.take() {
                    match f.finish(Instant::now()) {
                        Ok(p) => done.push(p),
                        Err(e) => failed = failed.or(Some(e)),
                    }
                }
                let _ = reply.send(match failed {
                    Some(e) => Err(e),
                    None if done.is_empty() => Err("nothing was recorded".into()),
                    None => Ok(done),
                });
                return;
            }
        }
    }
}

/// One MP4 being written: its frames' data in `mdat` as they come, their sizes, places, times
/// and which are key frames for the index at the end.
struct Mp4 {
    path: PathBuf,
    out: BufWriter<File>,
    w: usize,
    h: usize,
    enc: Encoder,
    yuv: YUVBuffer,
    crop: Vec<u8>,
    begun: Instant,
    /// Where the mdat box starts, and where the next sample goes.
    mdat: u64,
    at: u64,
    samples: Vec<(u64, u32, bool, u64)>,
    sps: Vec<u8>,
    pps: Vec<u8>,
}

impl Mp4 {
    fn create(path: &Path, w: usize, h: usize, begun: Instant) -> Result<Mp4, String> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(((w * h) as f64 * FPS as f64 * 0.06).clamp(300e3, 8e6) as u32))
            .max_frame_rate(FrameRate::from_hz(FPS as f32))
            .intra_frame_period(IntraFramePeriod::from_num_frames(FPS * 20))
            .skip_frames(false);
        let enc = Encoder::with_api_config(OpenH264API::from_source(), config).map_err(|e| e.to_string())?;
        let mut out = BufWriter::new(File::create(path).map_err(|e| format!("{}: {e}", path.display()))?);
        let ftyp = bx(b"ftyp", &[b"isom".as_slice(), &512u32.to_be_bytes(), b"isom", b"iso2", b"avc1", b"mp41"].concat());
        out.write_all(&ftyp).map_err(|e| e.to_string())?;
        // mdat with a 64-bit size, set at the end.
        out.write_all(&[0, 0, 0, 1]).and_then(|_| out.write_all(b"mdat")).and_then(|_| out.write_all(&[0; 8])).map_err(|e| e.to_string())?;
        let mdat = ftyp.len() as u64;
        Ok(Mp4 { path: path.to_path_buf(), out, w, h, enc, yuv: YUVBuffer::new(w, h), crop: Vec::new(), begun, mdat, at: mdat + 16, samples: Vec::new(), sps: Vec::new(), pps: Vec::new() })
    }

    /// Encodes a picture `stride_w` pixels wide (cut to even sides) and writes it as a sample.
    fn add(&mut self, rgb: &[u8], stride_w: usize, when: Instant) -> Result<(), String> {
        self.crop.clear();
        for y in 0..self.h {
            self.crop.extend_from_slice(&rgb[y * stride_w * 3..(y * stride_w + self.w) * 3]);
        }
        self.yuv.read_rgb8(RgbSliceU8::new(&self.crop, (self.w, self.h)));
        let au = self.enc.encode(&self.yuv).map_err(|e| e.to_string())?.to_vec();
        let mut sample = Vec::with_capacity(au.len() + 16);
        let mut key = false;
        for nal in nal_units(&au) {
            match nal.first().map(|b| b & 0x1f) {
                Some(7) => self.sps = nal.to_vec(),
                Some(8) => self.pps = nal.to_vec(),
                Some(9) | None => {}
                Some(t) => {
                    key |= t == 5;
                    sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    sample.extend_from_slice(nal);
                }
            }
        }
        // Before the first key frame nothing can be shown.
        if sample.is_empty() || (self.samples.is_empty() && !key) {
            return Ok(());
        }
        self.out.write_all(&sample).map_err(|e| e.to_string())?;
        let ms = when.saturating_duration_since(self.begun).as_millis() as u64;
        self.samples.push((self.at, sample.len() as u32, key, ms));
        self.at += sample.len() as u64;
        Ok(())
    }

    /// Writes the index and closes the file.
    fn finish(mut self, end: Instant) -> Result<PathBuf, String> {
        if self.samples.is_empty() || self.sps.is_empty() || self.pps.is_empty() {
            drop(self.out);
            let _ = std::fs::remove_file(&self.path);
            return Err("nothing was recorded".into());
        }
        let first = self.samples[0].3;
        let end_ms = end.saturating_duration_since(self.begun).as_millis() as u64;
        let durations: Vec<u32> = (0..self.samples.len())
            .map(|i| {
                let next = self.samples.get(i + 1).map_or(end_ms.max(self.samples[i].3 + 1), |s| s.3);
                (next.saturating_sub(self.samples[i].3)).max(1) as u32
            })
            .collect();
        let duration = (end_ms.max(self.samples.last().map_or(0, |s| s.3) + 1) - first) as u32;
        let moov = self.moov(&durations, duration);
        // The mdat holds everything written after its header.
        let mdat_size = self.at - self.mdat;
        self.out.write_all(&moov).map_err(|e| e.to_string())?;
        let mut f = self.out.into_inner().map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(self.mdat + 8)).and_then(|_| f.write_all(&mdat_size.to_be_bytes())).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        Ok(self.path)
    }

    fn moov(&self, durations: &[u32], duration: u32) -> Vec<u8> {
        let (w, h) = (self.w as u32, self.h as u32);
        let matrix: Vec<u8> = [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000].iter().flat_map(|v| v.to_be_bytes()).collect();
        let mvhd = full(b"mvhd", 0, 0, &[&[0u8; 8][..], &1000u32.to_be_bytes(), &duration.to_be_bytes(), &0x0001_0000u32.to_be_bytes(), &0x0100u16.to_be_bytes(), &[0; 10], &matrix, &[0; 24], &2u32.to_be_bytes()].concat());
        let tkhd = full(
            b"tkhd",
            0,
            3,
            &[&[0u8; 8][..], &1u32.to_be_bytes(), &[0; 4], &duration.to_be_bytes(), &[0; 8], &[0; 4], &[0; 4], &matrix, &(w << 16).to_be_bytes(), &(h << 16).to_be_bytes()].concat(),
        );
        let mdhd = full(b"mdhd", 0, 0, &[&[0u8; 8][..], &1000u32.to_be_bytes(), &duration.to_be_bytes(), &0x55c4u16.to_be_bytes(), &[0; 2]].concat());
        let hdlr = full(b"hdlr", 0, 0, &[&[0u8; 4][..], b"vide", &[0; 12], b"Video\0"].concat());
        let vmhd = full(b"vmhd", 0, 1, &[0; 8]);
        let dinf = bx(b"dinf", &full(b"dref", 0, 0, &[&1u32.to_be_bytes()[..], &full(b"url ", 0, 1, &[])].concat()));
        let avcc = bx(
            b"avcC",
            &[
                &[1, self.sps[1], self.sps[2], self.sps[3], 0xff, 0xe1][..],
                &(self.sps.len() as u16).to_be_bytes(),
                &self.sps,
                &[1],
                &(self.pps.len() as u16).to_be_bytes(),
                &self.pps,
            ]
            .concat(),
        );
        let mut name = [0u8; 32];
        name[0] = 4;
        name[1..5].copy_from_slice(b"H264");
        let avc1 = bx(
            b"avc1",
            &[
                &[0u8; 6][..],
                &1u16.to_be_bytes(),
                &[0; 16],
                &(w as u16).to_be_bytes(),
                &(h as u16).to_be_bytes(),
                &0x0048_0000u32.to_be_bytes(),
                &0x0048_0000u32.to_be_bytes(),
                &[0; 4],
                &1u16.to_be_bytes(),
                &name,
                &0x0018u16.to_be_bytes(),
                &0xffffu16.to_be_bytes(),
                &avcc,
            ]
            .concat(),
        );
        let stsd = full(b"stsd", 0, 0, &[&1u32.to_be_bytes()[..], &avc1].concat());
        // Times as runs of equal durations.
        let mut runs: Vec<(u32, u32)> = Vec::new();
        for &d in durations {
            match runs.last_mut() {
                Some((n, last)) if *last == d => *n += 1,
                _ => runs.push((1, d)),
            }
        }
        let stts = full(b"stts", 0, 0, &[(runs.len() as u32).to_be_bytes().to_vec(), runs.iter().flat_map(|(n, d)| [n.to_be_bytes(), d.to_be_bytes()].concat()).collect()].concat());
        let keys: Vec<u32> = self.samples.iter().enumerate().filter(|(_, s)| s.2).map(|(i, _)| i as u32 + 1).collect();
        let stss = full(b"stss", 0, 0, &[(keys.len() as u32).to_be_bytes().to_vec(), keys.iter().flat_map(|k| k.to_be_bytes()).collect()].concat());
        let stsc = full(b"stsc", 0, 0, &[1u32, 1, 1, 1].iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<_>>());
        let n = self.samples.len() as u32;
        let stsz = full(b"stsz", 0, 0, &[[0u32.to_be_bytes(), n.to_be_bytes()].concat(), self.samples.iter().flat_map(|s| s.1.to_be_bytes()).collect()].concat());
        let co64 = full(b"co64", 0, 0, &[n.to_be_bytes().to_vec(), self.samples.iter().flat_map(|s| s.0.to_be_bytes()).collect()].concat());
        let stbl = bx(b"stbl", &[stsd, stts, stss, stsc, stsz, co64].concat());
        let minf = bx(b"minf", &[vmhd, dinf, stbl].concat());
        let mdia = bx(b"mdia", &[mdhd, hdlr, minf].concat());
        let trak = bx(b"trak", &[tkhd, mdia].concat());
        bx(b"moov", &[mvhd, trak].concat())
    }
}

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat()
}

fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    bx(kind, &[&[version][..], &flags.to_be_bytes()[1..], body].concat())
}

/// The NAL units of an Annex B stream, without their start codes.
fn nal_units(b: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let mut end = starts.get(k + 1).map_or(b.len(), |n| n - 3);
            // A four-byte start code leaves a zero before the next three.
            while end > s && b[end - 1] == 0 && k + 1 < starts.len() {
                end -= 1;
            }
            &b[s..end]
        })
        .filter(|n| !n.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_splits_into_its_units() {
        let b = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9];
        let n = nal_units(&b);
        assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 9, 9]]);
    }

    /// A session recorded on the computer from its captured pictures (BGRx, rows with room to
    /// spare) is a whole MP4 in the folder asked for, at most 15 pictures a second.
    #[test]
    fn a_session_is_recorded_from_captured_pictures() {
        let dir = std::env::temp_dir().join(format!("sshremote-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = Recording::session(&dir).unwrap();
        let (w, h, stride) = (80, 60, 80 * 4 + 16);
        for i in 0..12u8 {
            let data: Vec<u8> = (0..stride * h).map(|b| (b as u8).wrapping_mul(i + 1)).collect();
            r.picture(&crate::capture::Image { w, h, stride, data });
            std::thread::sleep(Duration::from_millis(35));
        }
        let files = r.stop().unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].starts_with(&dir) && files[0].file_name().unwrap().to_string_lossy().starts_with("sshremote-session-"));
        let data = std::fs::read(&files[0]).unwrap();
        assert_eq!(&data[4..8], b"ftyp");
        assert!(data.windows(4).any(|b| b == b"moov"), "its index written");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A recording of a changing picture is an MP4 whose boxes add up and whose frames are all
    /// in it, the first a key frame.
    #[test]
    fn a_recording_is_a_whole_mp4() {
        let dir = std::env::temp_dir().join(format!("sshremote-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.mp4");
        let t0 = Instant::now();
        let mut m = Mp4::create(&path, 64, 48, t0).unwrap();
        for i in 0..10u8 {
            let rgb: Vec<u8> = (0..65 * 48).flat_map(|p| [(p as u8).wrapping_add(i * 20), i * 10, 90]).collect();
            m.add(&rgb, 65, t0 + Duration::from_millis(100 * i as u64)).unwrap();
        }
        let n = m.samples.len();
        assert!(n >= 9 && m.samples[0].2);
        let (samples, sps, pps) = (m.samples.clone(), m.sps.clone(), m.pps.clone());
        let out = m.finish(t0 + Duration::from_millis(1100)).unwrap();
        let data = std::fs::read(&out).unwrap();
        // Every frame decodes, from its place in the file with the parameters of its avcC.
        let mut dec = openh264::decoder::Decoder::new().unwrap();
        let mut shown = 0;
        for (i, (at, size, ..)) in samples.iter().enumerate() {
            let mut annexb = Vec::new();
            if i == 0 {
                for p in [&sps, &pps] {
                    annexb.extend_from_slice(&[0, 0, 0, 1]);
                    annexb.extend_from_slice(p);
                }
            }
            let mut k = *at as usize;
            while k < (*at + *size as u64) as usize {
                let len = u32::from_be_bytes(data[k..k + 4].try_into().unwrap()) as usize;
                annexb.extend_from_slice(&[0, 0, 0, 1]);
                annexb.extend_from_slice(&data[k + 4..k + 4 + len]);
                k += 4 + len;
            }
            if dec.decode(&annexb).unwrap().is_some() {
                shown += 1;
            }
        }
        assert_eq!(shown, n);
        // The top-level boxes cover the file exactly.
        let mut at = 0usize;
        let mut kinds = Vec::new();
        while at < data.len() {
            let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as u64;
            let size = if size == 1 { u64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap()) } else { size };
            kinds.push(String::from_utf8_lossy(&data[at + 4..at + 8]).into_owned());
            at += size as usize;
        }
        assert_eq!(at, data.len());
        assert_eq!(kinds, ["ftyp", "mdat", "moov"]);
        // Kept for a player to try: SSHREMOTE_KEEP_MP4 names where.
        if let Some(keep) = std::env::var_os("SSHREMOTE_KEEP_MP4") {
            std::fs::copy(&out, keep).unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A recording started, fed and stopped leaves a file, and one of another size a second.
    #[test]
    fn a_recording_stops_into_its_files() {
        let mut r = Recording::start("test host");
        let picture = |w: usize, h: usize, k: u8| -> Vec<u8> { (0..w * h).flat_map(|p| [(p as u8) ^ k, k, 40]).collect() };
        for k in 0..4u8 {
            while !r.due() {
                std::thread::sleep(Duration::from_millis(10));
            }
            r.frame(&picture(96, 64, k), 96, 64);
            r.dirty = true;
        }
        std::thread::sleep(Duration::from_millis(100));
        r.frame(&picture(64, 64, 9), 64, 64);
        let files = r.stop().unwrap();
        assert_eq!(files.len(), 2, "{files:?}");
        for f in &files {
            assert!(std::fs::metadata(f).unwrap().len() > 100);
            assert!(f.file_name().unwrap().to_string_lossy().starts_with("sshremote-test-host-"));
            let _ = std::fs::remove_file(f);
        }
    }
}
