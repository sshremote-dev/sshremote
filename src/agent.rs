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
    run_on(backend, input, output)
}

/// Serves one viewer over any pair of streams (stdio, relay pipes, a sealed network link).
pub fn run_on(backend: &str, input: Box<dyn Read + Send>, output: Box<dyn Write>) -> Result<(), String> {
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
    let mut desk = cap.desktop_size();
    let io_err = |e: io::Error| e.to_string();
    send(enc, &ToClient::Hello { w: desk.0 as u16, h: desk.1 as u16, backend: cap.name().into() }).map_err(io_err)?;
    send(enc, &monitors(cap.as_ref())).map_err(io_err)?;

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
    // Paused: no frames unless the viewer asks for one with Refresh.
    let (mut paused, mut one_shot) = (false, false);
    let mut prev: Option<Plane> = None;
    let mut vid = video::Video::new();
    let mut clip = crate::clip::Clip::new();
    let mut inbox = crate::files::Inbox::default();
    let mut terms = crate::pty::Terms::default();
    let mut outbox = crate::files::Outbox::default();
    let mut sound: Option<crate::audio::Capture> = None;
    let mut chat = crate::chat::Window::default();
    let mut pending = String::new();
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
            // Text waiting to be pasted goes in before any later input, so the order is kept.
            let typed_text = matches!(m, ToAgent::Key { keysym, mods: 0 } if keysym & 0xff00_0000 == 0x0100_0000);
            if !typed_text && matches!(m, ToAgent::Key { .. } | ToAgent::KeyState { .. } | ToAgent::Button { .. }) {
                paste(&mut pending, &mut clip, cap.as_mut());
            }
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
                    one_shot = true;
                    vid.restart();
                }
                ToAgent::Pause(on) => {
                    paused = on;
                    if !on {
                        prev = None;
                    }
                }
                ToAgent::Snapshot => {
                    // Some backends only deliver a picture when the screen changes; wait a little for one.
                    for _ in 0..40 {
                        if let Some(img) = cap.grab()? {
                            last = Some(img);
                            fresh = true;
                        }
                        if last.as_ref().is_some_and(|i| (i.w, i.h) == size) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                    let m = match last.as_ref().map(crate::shot::png_from_bgrx) {
                        Some(Ok(png)) => ToClient::Snapshot(png),
                        Some(Err(e)) => ToClient::Notice(format!("Screenshot: {e}")),
                        None => ToClient::Notice("Screenshot: no picture yet".into()),
                    };
                    send(enc, &m).map_err(io_err)?;
                }
                ToAgent::Ping(t) => send(enc, &ToClient::Pong(t)).map_err(io_err)?,
                // GNOME types only what its keyboard layout has; other text (Korean, emoji...) is pasted.
                ToAgent::Key { keysym, mods: 0 } if keysym & 0xff00_0000 == 0x0100_0000 && cap.name() == "gnome" => {
                    if let Some(c) = char::from_u32(keysym & 0x00ff_ffff) {
                        pending.push(c);
                    }
                }
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
                ToAgent::FileStartIn { id, size, dir, name } => {
                    if let Some(m) = inbox.start_in(id, size, std::path::Path::new(&dir), &name) {
                        send(enc, &m.to_client()).map_err(io_err)?;
                    }
                }
                ToAgent::FileOp { op, path, arg } => send(enc, &crate::files::op(op, &path, &arg)).map_err(io_err)?,
                ToAgent::FetchTagged { tag, path } => {
                    if let Some(m) = outbox.fetch_tagged(&path, tag) {
                        send(enc, &m).map_err(io_err)?;
                    }
                }
                ToAgent::TermOpen { id, cols, rows } => {
                    if let Some(m) = terms.open(id, cols, rows) {
                        send(enc, &m).map_err(io_err)?;
                    }
                }
                ToAgent::TermInput { id, data } => terms.input(id, &data),
                ToAgent::TermResize { id, cols, rows } => terms.resize(id, cols, rows),
                ToAgent::TermClose { id } => terms.close(id),
                ToAgent::FetchCancel { id, tag } => {
                    for m in outbox.cancel(id, tag) {
                        send(enc, &m).map_err(io_err)?;
                    }
                }
                ToAgent::Chat(text) => {
                    if let Err(e) = chat.show(&text) {
                        send(enc, &ToClient::Notice(format!("Chat: {e}"))).map_err(io_err)?;
                    }
                }
                ToAgent::Audio(on) => {
                    sound = None;
                    if on {
                        match crate::audio::capture() {
                            Ok(c) => sound = Some(c),
                            Err(e) => send(enc, &ToClient::Notice(format!("Sound: {e}"))).map_err(io_err)?,
                        }
                    }
                }
                ToAgent::SelectMonitor(i) => {
                    if let Err(e) = cap.select(i as usize) {
                        report(Err(e));
                        continue;
                    }
                    if let Ok(text) = cap.share(crate::capture::Share::Screen) {
                        send(enc, &ToClient::Sharing { kind: 0, text }).map_err(io_err)?;
                    }
                    desk = cap.desktop_size();
                    announce(enc, cap.as_mut(), desk, settings, &mut size)?;
                    (prev, last) = (None, None);
                    vid.reset();
                }
                ToAgent::ListWindows => {
                    let m = match cap.windows() {
                        Ok(list) => ToClient::WindowList { list, error: String::new() },
                        Err(error) => ToClient::WindowList { list: Vec::new(), error },
                    };
                    send(enc, &m).map_err(io_err)?;
                }
                ToAgent::InstallHelper => {
                    let error = match cap.install_helper() {
                        Ok(text) => text,
                        Err(e) => format!("Could not install the helper: {e}"),
                    };
                    send(enc, &ToClient::WindowList { list: Vec::new(), error }).map_err(io_err)?;
                }
                ToAgent::Share { kind, id } => {
                    let Some(scope) = crate::capture::Share::from_wire(kind, id) else { continue };
                    match cap.share(scope) {
                        Ok(text) => {
                            send(enc, &ToClient::Sharing { kind, text }).map_err(io_err)?;
                            desk = cap.desktop_size();
                            announce(enc, cap.as_mut(), desk, settings, &mut size)?;
                            (prev, last) = (None, None);
                            vid.reset();
                        }
                        Err(e) => send(enc, &ToClient::Notice(format!("Sharing: {e}"))).map_err(io_err)?,
                    }
                }
            }
        }
        paste(&mut pending, &mut clip, cap.as_mut());
        for text in chat.replies() {
            send(enc, &ToClient::Chat(text)).map_err(io_err)?;
        }
        if let Some(c) = &sound {
            for p in c.packets.try_iter() {
                send(enc, &ToClient::Audio(p)).map_err(io_err)?;
            }
        }
        for m in terms.poll() {
            send(enc, &m).map_err(io_err)?;
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
        if in_flight >= IN_FLIGHT || now < due || (paused && !one_shot) {
            continue;
        }
        if let Some(img) = cap.grab()? {
            last = Some(img);
            fresh = true;
        }
        // A shared window was resized or moved to where its size changed.
        if cap.desktop_size() != desk {
            desk = cap.desktop_size();
            announce(enc, cap.as_mut(), desk, settings, &mut size)?;
            (prev, last) = (None, None);
            vid.reset();
            continue;
        }
        let Some(img) = last.as_ref().filter(|i| (i.w, i.h) == size) else {
            continue;
        };
        if !fresh && prev.is_some() {
            continue;
        }
        fresh = false;
        one_shot = false;
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

/// Pastes text the backend cannot type, through the clipboard and Ctrl+V.
fn paste(pending: &mut String, clip: &mut crate::clip::Clip, cap: &mut dyn capture::Capture) {
    if pending.is_empty() {
        return;
    }
    if !clip.stage_paste(std::mem::take(pending)) {
        report(Err("text outside the keyboard layout needs the clipboard, which is unavailable".into()));
        return;
    }
    for (k, down) in [(0xffe3, true), ('v' as u32, true), ('v' as u32, false), (0xffe3, false)] {
        report(cap.input(Input::KeyState { keysym: k, down }));
    }
    // Applications fetch pasted text asynchronously; let it land before the next keys.
    thread::sleep(Duration::from_millis(150));
}

fn monitors(cap: &dyn capture::Capture) -> ToClient {
    let list = cap
        .monitors()
        .into_iter()
        .map(|m| crate::proto::MonitorInfo { name: m.name, w: m.w as u16, h: m.h as u16, primary: m.primary })
        .collect();
    ToClient::Monitors { list, current: cap.current() as u8 }
}

/// Tells the viewer the new picture size and fits the capture to it.
fn announce(
    enc: &mut Wire,
    cap: &mut dyn capture::Capture,
    desk: (usize, usize),
    settings: Option<Settings>,
    size: &mut (usize, usize),
) -> Result<(), String> {
    let io_err = |e: io::Error| e.to_string();
    let hello = ToClient::Hello { w: desk.0 as u16, h: desk.1 as u16, backend: cap.name().into() };
    send(enc, &hello).map_err(io_err)?;
    send(enc, &monitors(cap)).map_err(io_err)?;
    if let Some(s) = settings {
        *size = target(desk, &s);
        cap.set_target(size.0, size.1)?;
    }
    Ok(())
}

fn report(r: Result<(), String>) {
    if let Err(e) = r {
        eprintln!("desk agent: {e}");
    }
}
