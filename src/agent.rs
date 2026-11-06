//! Remote side: captures the desktop, reduces it to the requested colour mode,
//! sends only changed rectangles, and injects the viewer's input.

use std::io::{self, BufReader, BufWriter, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::capture::{self, Image, Input};
use crate::color::Mode;
use crate::color::{Plane, Reducer, diff, pack};
use crate::proto::{Frame, Settings, Tile, ToAgent, ToClient, read_msg, write_msg};
use crate::{scroll, video};

/// Frames sent but not yet acknowledged; more would only queue stale screens in SSH.
const IN_FLIGHT: u32 = 2;

type Wire = zstd::stream::write::Encoder<'static, BufWriter<Box<dyn Write>>>;

fn send(w: &mut Wire, m: &ToClient) -> io::Result<()> {
    let (k, p) = m.encode();
    write_msg(w, k, &p)?;
    w.flush()
}

fn target(desk: (usize, usize), s: &Settings) -> (usize, usize) {
    let (dw, dh) = (desk.0 as f64, desk.1 as f64);
    let fit = (s.view_w.max(16) as f64 / dw).min(s.view_h.max(16) as f64 / dh).min(1.0);
    let k = fit * s.scale as f64 / 100.0;
    (((dw * k) as usize).max(16), ((dh * k) as usize).max(16))
}

/// Serves on stdin/stdout, or on the named pipes a Windows relay created.
pub fn run(backend: &str, pipe: Option<&str>) -> Result<(), String> {
    #[cfg(windows)]
    if pipe.is_none()
        && matches!(backend, "auto" | "windows")
        // DESK_RELAY forces the relay path so it can be tested from a console session.
        && (crate::capture::windows::needs_relay() || std::env::var_os("DESK_RELAY").is_some())
    {
        return crate::relay::run(backend);
    }
    let (input, output): (Box<dyn Read + Send>, Box<dyn Write>) = match pipe {
        #[cfg(windows)]
        Some(base) => {
            let (down, up) = crate::relay::pipe_names(base);
            let open = |p: &str, write: bool| {
                std::fs::OpenOptions::new().read(!write).write(write).open(p).map_err(|e| format!("{p}: {e}"))
            };
            (Box::new(open(&down, false)?), Box::new(open(&up, true)?))
        }
        #[cfg(not(windows))]
        Some(_) => return Err("--pipe is only used on Windows".into()),
        None => (Box::new(io::stdin()), Box::new(io::stdout())),
    };
    let mut enc =
        zstd::stream::write::Encoder::new(BufWriter::with_capacity(1 << 16, output), 3).map_err(|e| e.to_string())?;
    // A long window lets earlier screens act as a dictionary for repeats.
    enc.window_log(24).map_err(|e| e.to_string())?;
    enc.long_distance_matching(true).map_err(|e| e.to_string())?;
    let result = serve(&mut enc, input, backend);
    if let Err(e) = &result {
        let _ = send(&mut enc, &ToClient::Error(e.clone()));
    }
    result
}

fn serve(enc: &mut Wire, input: Box<dyn Read + Send>, backend: &str) -> Result<(), String> {
    let mut cap = capture::open(backend)?;
    let desk = cap.desktop_size();
    let io_err = |e: io::Error| e.to_string();
    send(enc, &ToClient::Hello { w: desk.0 as u16, h: desk.1 as u16, backend: cap.name().into() }).map_err(io_err)?;

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut input = BufReader::new(input);
        while let Ok(Some((k, p))) = read_msg(&mut input) {
            let Ok(m) = ToAgent::decode(k, &p) else { break };
            if tx.send(m).is_err() {
                break;
            }
        }
    });

    let mut settings: Option<Settings> = None;
    let mut size = (0, 0);
    let mut reducer: Option<Reducer> = None;
    let mut last: Option<Image> = None;
    let mut fresh = false;
    let mut prev: Option<Plane> = None;
    let mut vid = video::Video::new();
    let mut clip = crate::clip::Clip::new();
    let mut inbox = crate::files::Inbox::default();
    let mut outbox = crate::files::Outbox::default();
    let mut in_flight = 0u32;
    let mut next_id = 1u32;
    let mut due = Instant::now();
    loop {
        // A download in progress keeps the loop turning; otherwise wait for input or the next frame.
        let longest = if outbox.busy() { 1 } else { 8 };
        let wait = due
            .saturating_duration_since(Instant::now())
            .clamp(Duration::from_millis(1), Duration::from_millis(longest));
        let mut msgs = Vec::new();
        match rx.recv_timeout(wait) {
            Ok(m) => msgs.push(m),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Ok(()),
        }
        msgs.extend(rx.try_iter());
        for m in msgs {
            match m {
                ToAgent::Settings(s) => {
                    let t = target(desk, &s);
                    if t != size {
                        cap.set_target(t.0, t.1)?;
                        size = t;
                        prev = None;
                        vid.reset();
                    }
                    if reducer.as_ref().is_none_or(|r| r.mode() != s.mode) {
                        reducer = Some(Reducer::new(s.mode));
                        prev = None;
                        vid.reset();
                    }
                    settings = Some(s);
                }
                ToAgent::Ack(_) => in_flight = in_flight.saturating_sub(1),
                ToAgent::Refresh => {
                    prev = None;
                    vid.restart();
                }
                ToAgent::Ping(t) => send(enc, &ToClient::Pong(t)).map_err(io_err)?,
                ToAgent::Key { keysym, mods } => report(cap.input(Input::Key { keysym, mods })),
                ToAgent::Pointer { x, y } => {
                    let img = last.as_ref().map_or(desk, |i| (i.w, i.h));
                    let sx = (x as f64 + 0.5) * desk.0 as f64 / img.0.max(1) as f64;
                    let sy = (y as f64 + 0.5) * desk.1 as f64 / img.1.max(1) as f64;
                    report(cap.input(Input::Move { x: sx, y: sy }));
                }
                ToAgent::Button { button, pressed } => report(cap.input(Input::Button { button, pressed })),
                ToAgent::Scroll { steps } => report(cap.input(Input::Scroll { steps: steps as i32 })),
                ToAgent::HScroll { steps } => report(cap.input(Input::HScroll { steps: steps as i32 })),
                ToAgent::KeyState { keysym, down } => report(cap.input(Input::KeyState { keysym, down })),
                ToAgent::SecureAttention => report(cap.input(Input::SecureAttention)),
                ToAgent::Clipboard(text) => clip.set(text),
                ToAgent::FileStart { id, size, name } => {
                    if let Some(m) = inbox.start(id, size, &name) {
                        send(enc, &m.to_client()).map_err(io_err)?;
                    }
                }
                ToAgent::FileData { id, data } => {
                    if let Some(m) = inbox.data(id, &data) {
                        send(enc, &m.to_client()).map_err(io_err)?;
                    }
                }
                ToAgent::FileEnd { id } => {
                    if let Some(m) = inbox.end(id, true, "") {
                        send(enc, &m.to_client()).map_err(io_err)?;
                    }
                }
                ToAgent::ListDir { path } => send(enc, &crate::files::list(&path)).map_err(io_err)?,
                ToAgent::Fetch { path } => {
                    if let Some(m) = outbox.fetch(&path) {
                        send(enc, &m).map_err(io_err)?;
                    }
                }
                ToAgent::FetchAck { id, received } => outbox.ack(id, received),
            }
        }
        // Downloads share the stream with the screen, a slice per turn.
        for m in outbox.pump(256 << 10) {
            send(enc, &m).map_err(io_err)?;
        }
        if let Some(text) = clip.poll() {
            send(enc, &ToClient::Clipboard(text)).map_err(io_err)?;
        }
        let (Some(s), Some(red)) = (settings, reducer.as_ref()) else {
            continue;
        };
        let now = Instant::now();
        // Grab only when a frame may go out: some backends copy the whole screen per grab.
        if in_flight >= IN_FLIGHT || now < due {
            continue;
        }
        if let Some(img) = cap.grab()? {
            last = Some(img);
            fresh = true;
        }
        let Some(img) = last.as_ref().filter(|i| (i.w, i.h) == size) else {
            continue;
        };
        if !fresh && prev.is_some() {
            continue;
        }
        fresh = false;
        let plane = red.reduce(img);
        let mut rects = diff(prev.as_ref(), &plane);
        let mut moves = Vec::new();
        if let Some(p) = prev.as_ref()
            && let Some(m) = scroll::find(p, &plane, &rects)
        {
            let mut moved = p.clone();
            scroll::apply(&mut moved.data, moved.w, moved.mode.bpp(), &m);
            rects = diff(Some(&moved), &plane);
            moves.push(m);
        }
        let mut video_part = None;
        if matches!(plane.mode, Mode::Color256 | Mode::True) {
            if let video::Event::Ended(r) = vid.observe(plane.w, plane.h, &rects) {
                rects.push(r);
            }
            if let Some(reg) = vid.region {
                if rects.iter().any(|r| video::overlaps(r, &reg)) {
                    let rate =
                        (reg.w * reg.h) as f64 * s.fps as f64 * if plane.mode == Mode::True { 0.08 } else { 0.04 };
                    let data = vid.encode(img, s.fps, rate.clamp(150e3, 6e6) as u32)?;
                    let (x, y, w, h) = (reg.x as u16, reg.y as u16, reg.w as u16, reg.h as u16);
                    video_part = Some(Tile { x, y, w, h, data });
                }
                rects = rects.iter().flat_map(|r| video::subtract(r, &reg)).collect();
            }
        }
        prev = Some(plane);
        if rects.is_empty() && moves.is_empty() && video_part.is_none() {
            due = now + Duration::from_millis(1000 / s.fps.max(1) as u64);
            continue;
        }
        let plane = prev.as_ref().unwrap();
        let tiles = rects
            .iter()
            .map(|r| Tile {
                x: r.x as u16,
                y: r.y as u16,
                w: r.w as u16,
                h: r.h as u16,
                data: pack(plane, r.x, r.y, r.w, r.h),
            })
            .collect();
        let frame = Frame {
            id: next_id,
            w: plane.w as u16,
            h: plane.h as u16,
            mode: plane.mode,
            moves,
            tiles,
            video: video_part,
        };
        next_id = next_id.wrapping_add(1);
        send(enc, &ToClient::Frame(frame)).map_err(io_err)?;
        in_flight += 1;
        due = now + Duration::from_millis(1000 / s.fps.max(1) as u64);
    }
}

fn report(r: Result<(), String>) {
    if let Err(e) = r {
        eprintln!("desk agent: {e}");
    }
}
