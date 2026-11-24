//! Recording a remote screen as it shows here, into an MP4 file (H.264) any player opens. Frames
//! are encoded on a thread of their own, at most 15 a second and only when the picture changed;
//! the file is written as it goes, in pieces that each play, so one cut off plays to its end.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
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
    /// A session's recording keeps one size, in one file, however the viewer's changes: its
    /// first picture's shape, as large as the screen's (up to `MOST`) where it was smaller.
    size: Option<(usize, usize)>,
    screen: (usize, usize),
}

/// The largest a session is recorded.
const MOST: (usize, usize) = (1920, 1200);

impl Recording {
    /// Starts one, its file named after `what` in the Videos folder (or Downloads).
    pub fn start(what: &str) -> Recording {
        let (jobs, rx) = mpsc::sync_channel(2);
        let base = name(what);
        std::thread::spawn(move || encode(rx, base));
        Recording { jobs, started: Instant::now(), last: None, dirty: true, size: None, screen: (0, 0) }
    }

    /// Starts one of a session by ID in `dir` (made if need be), the time in its name, of a
    /// screen of `screen` pixels.
    pub fn session(dir: &Path, screen: (usize, usize)) -> Result<Recording, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if crate::sys::free_space(dir).is_some_and(|f| f < KEEP_FREE) {
            return Err(format!("the disk of {} is nearly full", dir.display()));
        }
        let base = name("session");
        let file = base.file_name().ok_or("no name")?;
        let base = dir.join(file);
        let (jobs, rx) = mpsc::sync_channel(2);
        std::thread::spawn(move || encode(rx, base));
        Ok(Recording { jobs, started: Instant::now(), last: None, dirty: true, size: None, screen })
    }

    /// Hands a captured picture over (BGRx rows `stride` apart), when one is due.
    pub fn picture(&mut self, img: &crate::capture::Image) {
        self.dirty = true;
        if !self.due() || img.w < 16 || img.h < 16 {
            return;
        }
        let screen = (self.screen.0.min(MOST.0), self.screen.1.min(MOST.1));
        let (w, h) = *self.size.get_or_insert_with(|| {
            // A small first picture (a pane in a grid) as large as the screen allows.
            let k = (screen.0 as f64 / img.w as f64).min(screen.1 as f64 / img.h as f64).max(1.0);
            ((img.w as f64 * k) as usize & !1, (img.h as f64 * k) as usize & !1)
        });
        // Fitted in whole, nearest pixels taken; black on the sides of another shape.
        let (fw, fh) = if img.w * h <= img.h * w { ((img.w * h / img.h).max(1), h) } else { (w, (img.h * w / img.w).max(1)) };
        let (ox, oy) = ((w - fw) / 2, (h - fh) / 2);
        let mut rgb = vec![0; w * h * 3];
        for y in 0..fh {
            let row = &img.data[(y * img.h / fh) * img.stride..];
            let out = &mut rgb[((oy + y) * w + ox) * 3..][..fw * 3];
            for (x, o) in out.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let px = &row[(x * img.w / fw) * 4..][..3];
                *o = [px[2], px[1], px[0]];
            }
        }
        let now = Instant::now();
        if self.jobs.try_send(Job::Frame(rgb, w, h, now)).is_ok() {
            (self.last, self.dirty) = (Some(now), false);
        }
    }

    /// Whether a picture would be kept now, if it changed: the last one is old enough.
    pub fn wants(&self) -> bool {
        self.last.is_none_or(|t| t.elapsed() >= Duration::from_millis(1000 / FPS as u64))
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

/// How long a piece of a file holds at most: a recording cut off (its program killed, the
/// computer turned off) plays up to its last whole piece.
const PIECE_MS: u64 = 2000;
/// Room on the disk a recording leaves: it stops before taking that.
const KEEP_FREE: u64 = 1 << 30;

/// One MP4 written as it goes: after the header (`moov`, what decoding needs, written with the
/// first key frame) the frames come in pieces of a couple of seconds (`moof` and `mdat`), each
/// whole in itself. The length is filled in at the end.
struct Mp4 {
    path: PathBuf,
    out: File,
    w: usize,
    h: usize,
    enc: Encoder,
    yuv: YUVBuffer,
    crop: Vec<u8>,
    begun: Instant,
    /// The frames of the piece being gathered: data, whether a key frame, when (ms from `begun`).
    piece: Vec<(Vec<u8>, bool, u64)>,
    /// Where the header keeps the length, once written; the time the pieces have filled.
    length_at: Option<u64>,
    filled: u64,
    pieces: u32,
    /// How far the file is written, and how many frames are in it.
    at: u64,
    frames: usize,
    sps: Vec<u8>,
    pps: Vec<u8>,
}

impl Mp4 {
    /// A new file at `path`, or beside it (NAME-2.mp4 and on) where one is there already.
    fn create(path: &Path, w: usize, h: usize, begun: Instant) -> Result<Mp4, String> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(((w * h) as f64 * FPS as f64 * 0.06).clamp(300e3, 8e6) as u32))
            .max_frame_rate(FrameRate::from_hz(FPS as f32))
            .intra_frame_period(IntraFramePeriod::from_num_frames(FPS * 20))
            .skip_frames(false);
        let enc = Encoder::with_api_config(OpenH264API::from_source(), config).map_err(|e| e.to_string())?;
        let (path, mut out) = create_beside(path)?;
        let ftyp = bx(b"ftyp", &[b"isom".as_slice(), &512u32.to_be_bytes(), b"isom", b"iso6", b"avc1", b"mp41"].concat());
        out.write_all(&ftyp).map_err(|e| e.to_string())?;
        Ok(Mp4 {
            path,
            out,
            w,
            h,
            enc,
            yuv: YUVBuffer::new(w, h),
            crop: Vec::new(),
            begun,
            piece: Vec::new(),
            length_at: None,
            filled: 0,
            pieces: 0,
            at: ftyp.len() as u64,
            frames: 0,
            sps: Vec::new(),
            pps: Vec::new(),
        })
    }

    /// Encodes a picture `stride_w` pixels wide (cut to even sides) and keeps it as a frame.
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
        let ms = when.saturating_duration_since(self.begun).as_millis() as u64;
        if self.length_at.is_none() {
            // Before the first key frame nothing can be shown.
            if sample.is_empty() || !key || self.sps.len() < 4 || self.pps.is_empty() {
                return Ok(());
            }
            let (moov, length) = self.moov();
            self.out.write_all(&moov).map_err(|e| e.to_string())?;
            self.length_at = Some(self.at + length as u64);
            self.at += moov.len() as u64;
        }
        if sample.is_empty() {
            return Ok(());
        }
        // A piece ends before a key frame (where a player can start), or once long enough.
        if self.piece.first().is_some_and(|f| key || ms.saturating_sub(f.2) >= PIECE_MS) {
            self.write_piece(ms)?;
        }
        self.piece.push((sample, key, ms));
        self.frames += 1;
        Ok(())
    }

    /// Writes the frames gathered as a piece, the last lasting until `next` (ms).
    fn write_piece(&mut self, next: u64) -> Result<(), String> {
        if self.piece.is_empty() {
            return Ok(());
        }
        let piece = std::mem::take(&mut self.piece);
        let durations: Vec<u32> = (0..piece.len()).map(|i| (piece.get(i + 1).map_or(next, |f| f.2).saturating_sub(piece[i].2)).max(1) as u32).collect();
        self.pieces += 1;
        let mfhd = full(b"mfhd", 0, 0, &self.pieces.to_be_bytes());
        // Data offsets from the piece's own start.
        let tfhd = full(b"tfhd", 0, 0x02_0000, &1u32.to_be_bytes());
        let tfdt = full(b"tfdt", 1, 0, &self.filled.to_be_bytes());
        let trun_len = 12 + 8 + piece.len() * 12;
        let moof_len = 8 + mfhd.len() + 8 + tfhd.len() + tfdt.len() + trun_len;
        let mut runs = Vec::with_capacity(trun_len);
        runs.extend_from_slice(&(piece.len() as u32).to_be_bytes());
        runs.extend_from_slice(&((moof_len + 8) as u32).to_be_bytes());
        for (f, d) in piece.iter().zip(&durations) {
            // A key frame depends on no other; the others are not where a player may start.
            let flags: u32 = if f.1 { 0x0200_0000 } else { 0x0101_0000 };
            runs.extend_from_slice(&[d.to_be_bytes(), (f.0.len() as u32).to_be_bytes(), flags.to_be_bytes()].concat());
        }
        // Duration, size and flags for each frame, after the data offset.
        let trun = full(b"trun", 0, 0x0701, &runs);
        let moof = bx(b"moof", &[mfhd, bx(b"traf", &[tfhd, tfdt, trun].concat())].concat());
        debug_assert_eq!(moof.len(), moof_len);
        let data: usize = piece.iter().map(|f| f.0.len()).sum();
        let mut all = Vec::with_capacity(moof.len() + 8 + data);
        all.extend_from_slice(&moof);
        all.extend_from_slice(&((data + 8) as u32).to_be_bytes());
        all.extend_from_slice(b"mdat");
        for f in &piece {
            all.extend_from_slice(&f.0);
        }
        // Whole, at once: killed, the file keeps every piece written and no part of one.
        self.out.write_all(&all).map_err(|e| e.to_string())?;
        self.at += all.len() as u64;
        self.filled += durations.iter().map(|&d| d as u64).sum::<u64>();
        if crate::sys::free_space(&self.path).is_some_and(|f| f < KEEP_FREE) {
            return Err(format!("the disk is nearly full, so the recording stopped ({})", self.path.display()));
        }
        Ok(())
    }

    /// Writes the last piece and the length, and closes the file.
    fn finish(mut self, end: Instant) -> Result<PathBuf, String> {
        let Some(length_at) = self.length_at else {
            drop(self.out);
            let _ = std::fs::remove_file(&self.path);
            return Err("nothing was recorded".into());
        };
        let end_ms = end.saturating_duration_since(self.begun).as_millis() as u64;
        let last = self.piece.last().map_or(0, |f| f.2 + 1);
        let pieces = self.write_piece(end_ms.max(last));
        let f = &mut self.out;
        f.seek(SeekFrom::Start(length_at)).and_then(|_| f.write_all(&self.filled.to_be_bytes())).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        pieces.map(|_| self.path)
    }

    /// The header: the picture's size and decoding parameters, no frames (they come in pieces);
    /// and where in it the length goes.
    fn moov(&self) -> (Vec<u8>, usize) {
        let (w, h) = (self.w as u32, self.h as u32);
        let matrix: Vec<u8> = [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000].iter().flat_map(|v| v.to_be_bytes()).collect();
        let mvhd = full(b"mvhd", 0, 0, &[&[0u8; 8][..], &1000u32.to_be_bytes(), &[0; 4], &0x0001_0000u32.to_be_bytes(), &0x0100u16.to_be_bytes(), &[0; 10], &matrix, &[0; 24], &2u32.to_be_bytes()].concat());
        let tkhd = full(b"tkhd", 0, 3, &[&[0u8; 8][..], &1u32.to_be_bytes(), &[0; 4], &[0; 4], &[0; 8], &[0; 4], &[0; 4], &matrix, &(w << 16).to_be_bytes(), &(h << 16).to_be_bytes()].concat());
        let mdhd = full(b"mdhd", 0, 0, &[&[0u8; 8][..], &1000u32.to_be_bytes(), &[0; 4], &0x55c4u16.to_be_bytes(), &[0; 2]].concat());
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
        // No frames here: the tables are empty.
        let none = 0u32.to_be_bytes();
        let stbl = bx(b"stbl", &[stsd, full(b"stts", 0, 0, &none), full(b"stsc", 0, 0, &none), full(b"stsz", 0, 0, &[none, none].concat()), full(b"stco", 0, 0, &none)].concat());
        let minf = bx(b"minf", &[vmhd, dinf, stbl].concat());
        let mdia = bx(b"mdia", &[mdhd, hdlr, minf].concat());
        let trak = bx(b"trak", &[tkhd, mdia].concat());
        // Track 1's frames come in pieces; the length (ms) goes in mehd at the end.
        let trex = full(b"trex", 0, 0, &[1u32, 1, 0, 0, 0].iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<_>>());
        let mvex = bx(b"mvex", &[full(b"mehd", 1, 0, &[0; 8]), trex.clone()].concat());
        let moov = bx(b"moov", &[mvhd, trak, mvex].concat());
        let length = moov.len() - trex.len() - 8;
        (moov, length)
    }
}

/// Opens a new file at `path`, or NAME-2.mp4 and on beside it when that is taken (two sessions
/// recorded in the same second): never one written over another.
fn create_beside(path: &Path) -> Result<(PathBuf, File), String> {
    // Its folder made first: a computer without a desktop may have no Downloads.
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut last = String::new();
    for n in 1..100 {
        let p = if n == 1 { path.to_path_buf() } else { path.with_file_name(format!("{stem}-{n}.mp4")) };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(f) => return Ok((p, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = e.to_string(),
            Err(e) => return Err(format!("{}: {e}", p.display())),
        }
    }
    Err(format!("{}: {last}", path.display()))
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

    /// The top-level boxes of a file (kind, start, size), which must cover it exactly.
    fn boxes(data: &[u8]) -> Vec<(String, usize, usize)> {
        let mut at = 0;
        let mut out = Vec::new();
        while at < data.len() {
            let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
            assert!(size >= 8 && at + size <= data.len(), "a box past the end at {at}");
            out.push((String::from_utf8_lossy(&data[at + 4..at + 8]).into_owned(), at, size));
            at += size;
        }
        out
    }

    /// The frames of the pieces: where each one's data is, and whether a key frame.
    fn frames(data: &[u8]) -> Vec<(usize, usize, bool)> {
        let mut out = Vec::new();
        for (kind, at, size) in boxes(data) {
            if kind != "moof" {
                continue;
            }
            let t = at + data[at..at + size].windows(4).position(|w| w == b"trun").unwrap() + 4;
            let n = u32::from_be_bytes(data[t + 4..t + 8].try_into().unwrap()) as usize;
            let mut place = at + u32::from_be_bytes(data[t + 8..t + 12].try_into().unwrap()) as usize;
            for i in 0..n {
                let e = t + 12 + i * 12;
                let len = u32::from_be_bytes(data[e + 4..e + 8].try_into().unwrap()) as usize;
                let flags = u32::from_be_bytes(data[e + 8..e + 12].try_into().unwrap());
                out.push((place, len, flags == 0x0200_0000));
                place += len;
            }
        }
        out
    }

    /// How wide and high a file's picture is (its tkhd).
    fn picture_size(data: &[u8]) -> (u32, u32) {
        let t = data.windows(4).position(|w| w == b"tkhd").unwrap() - 4;
        let at = |o: usize| u32::from_be_bytes(data[t + o..t + o + 4].try_into().unwrap()) >> 16;
        (at(84), at(88))
    }

    /// Every frame decodes, from its place in the file with the parameters of the header.
    fn decoded(data: &[u8], sps: &[u8], pps: &[u8]) -> usize {
        let mut dec = openh264::decoder::Decoder::new().unwrap();
        let mut shown = 0;
        for (i, (at, size, key)) in frames(data).into_iter().enumerate() {
            assert!(key || i > 0, "the first frame is a key frame");
            let mut annexb = Vec::new();
            if i == 0 {
                for p in [sps, pps] {
                    annexb.extend_from_slice(&[0, 0, 0, 1]);
                    annexb.extend_from_slice(p);
                }
            }
            let mut k = at;
            while k < at + size {
                let len = u32::from_be_bytes(data[k..k + 4].try_into().unwrap()) as usize;
                annexb.extend_from_slice(&[0, 0, 0, 1]);
                annexb.extend_from_slice(&data[k + 4..k + 4 + len]);
                k += 4 + len;
            }
            if dec.decode(&annexb).unwrap().is_some() {
                shown += 1;
            }
        }
        shown
    }

    /// A session recorded on the computer from its captured pictures (BGRx, rows with room to
    /// spare) is a whole MP4 in the folder asked for, at the screen's size whatever the viewer's,
    /// and two begun in the same second are two files.
    #[test]
    fn a_session_is_recorded_from_captured_pictures() {
        let dir = std::env::temp_dir().join(format!("sshremote-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = Recording::session(&dir, (160, 120)).unwrap();
        let mut other = Recording::session(&dir, (0, 0)).unwrap();
        for i in 0..12u8 {
            // The viewer's size changes halfway, to another shape: still one file.
            let (w, h) = if i < 6 { (80, 60) } else { (120, 50) };
            let stride = w * 4 + 16;
            let data: Vec<u8> = (0..stride * h).map(|b| (b as u8).wrapping_mul(i + 1)).collect();
            r.picture(&crate::capture::Image { w, h, stride, data: data.clone() });
            other.picture(&crate::capture::Image { w, h, stride, data });
            std::thread::sleep(Duration::from_millis(70));
        }
        let files = r.stop().unwrap();
        let others = other.stop().unwrap();
        assert_eq!((files.len(), others.len()), (1, 1));
        assert_ne!(files[0], others[0]);
        assert!(files[0].starts_with(&dir) && files[0].file_name().unwrap().to_string_lossy().starts_with("sshremote-session-"));
        let data = std::fs::read(&files[0]).unwrap();
        let kinds: Vec<String> = boxes(&data).into_iter().map(|b| b.0).collect();
        assert_eq!(kinds[..2], ["ftyp", "moov"]);
        assert!(kinds[2..].chunks(2).all(|p| p == ["moof", "mdat"]), "{kinds:?}");
        assert_eq!(picture_size(&data), (160, 120), "as large as the screen");
        assert_eq!(picture_size(&std::fs::read(&others[0]).unwrap()), (80, 60), "the first picture's size");
        // The length, filled in at the end.
        let length = u64::from_be_bytes(data[data.windows(4).position(|w| w == b"mehd").unwrap() + 8..][..8].try_into().unwrap());
        assert!((300..2000).contains(&length), "{length} ms");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A recording of a changing picture is an MP4 in pieces whose frames are all in it and
    /// decode, the first a key frame; one cut off plays up to its last piece.
    #[test]
    fn a_recording_is_a_whole_mp4_even_cut_off() {
        let dir = std::env::temp_dir().join(format!("sshremote-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.mp4");
        let _ = std::fs::remove_file(&path);
        let t0 = Instant::now();
        let mut m = Mp4::create(&path, 64, 48, t0).unwrap();
        // A screen where a little changes: a bar moving over a still picture.
        let picture = |i: u8| -> Vec<u8> { (0..65 * 48).flat_map(|p| if (p % 65) / 4 == i as usize % 16 { [255, 255, 255] } else { [p as u8, (p / 65) as u8 * 5, 90] }).collect() };
        // Five seconds at 10 a second: pieces of two seconds.
        for i in 0..50u8 {
            m.add(&picture(i), 65, t0 + Duration::from_millis(100 * i as u64)).unwrap();
        }
        let n = m.frames;
        assert!(n >= 49);
        let (sps, pps) = (m.sps.clone(), m.pps.clone());
        // Cut off here, as by a kill: the pieces written so far play.
        let cut = std::fs::read(&path).unwrap();
        let pieces = boxes(&cut).iter().filter(|b| b.0 == "moof").count();
        assert!((2..=3).contains(&pieces), "{pieces} pieces");
        assert_eq!(decoded(&cut, &sps, &pps), frames(&cut).len());
        assert!(frames(&cut).len() >= 40);
        let out = m.finish(t0 + Duration::from_millis(5100)).unwrap();
        let data = std::fs::read(&out).unwrap();
        assert_eq!(decoded(&data, &sps, &pps), n);
        let length = u64::from_be_bytes(data[data.windows(4).position(|w| w == b"mehd").unwrap() + 8..][..8].try_into().unwrap());
        assert!((5000..=5100).contains(&length), "{length} ms");
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
