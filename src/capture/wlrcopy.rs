//! Wayland desktops built on wlroots and its kin (Hyprland, Sway, river, niri...): the screen read
//! through wlr-screencopy, as grim and wayvnc read it, with no portal and so no dialog for someone
//! at the screen to answer; input through the virtual pointer and keyboard (`wlinput`). A copy
//! waits for the screen to change, so a still screen costs nothing; one is always waiting (a thread
//! of its own asks again as soon as one comes), so no change goes by unseen.

use std::fs::File;
use std::os::fd::{AsFd, AsRawFd};
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

use super::{Capture, Image, Input, Monitor};

/// A monitor as the compositor tells it: its name, mode (pixels), place (layout points) and scale.
struct Out {
    output: WlOutput,
    name: String,
    size: (i32, i32),
    at: (i32, i32),
    scale: i32,
}

/// What a frame being copied has said so far.
#[derive(Default)]
struct Copying {
    /// The memory form asked for: format, width, height, stride.
    form: Option<(wl_shm::Format, u32, u32, u32)>,
    /// All forms told (version 3); before it, the first one is taken.
    told: bool,
    upside_down: bool,
    ready: bool,
    failed: bool,
}

#[derive(Default)]
struct State {
    manager: Option<(ZwlrScreencopyManagerV1, u32)>,
    shm: Option<WlShm>,
    outs: Vec<Out>,
    copying: Copying,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(s: &mut State, r: &WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<State>) {
        if let wl_registry::Event::Global { name, interface, version } = e {
            match interface.as_str() {
                "zwlr_screencopy_manager_v1" => s.manager = Some((r.bind(name, version.min(3), qh, ()), version.min(3))),
                "wl_shm" => s.shm = Some(r.bind(name, 1, qh, ())),
                "wl_output" => {
                    let n = s.outs.len();
                    let output = r.bind(name, version.min(4), qh, n);
                    s.outs.push(Out { output, name: format!("output {}", n + 1), size: (0, 0), at: (0, 0), scale: 1 });
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlOutput, usize> for State {
    fn event(s: &mut State, _: &WlOutput, e: wl_output::Event, n: &usize, _: &Connection, _: &QueueHandle<State>) {
        let Some(o) = s.outs.get_mut(*n) else { return };
        match e {
            wl_output::Event::Mode { flags: WEnum::Value(f), width, height, .. } if f.contains(wl_output::Mode::Current) => o.size = (width, height),
            wl_output::Event::Geometry { x, y, .. } => o.at = (x, y),
            wl_output::Event::Scale { factor } => o.scale = factor.max(1),
            wl_output::Event::Name { name } => o.name = name,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(s: &mut State, _: &ZwlrScreencopyFrameV1, e: zwlr_screencopy_frame_v1::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {
        let c = &mut s.copying;
        match e {
            zwlr_screencopy_frame_v1::Event::Buffer { format: WEnum::Value(f), width, height, stride } => {
                // The forms read here, the compositor's own (XRGB, ARGB) first.
                let rank = |f: wl_shm::Format| match f {
                    wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => Some(0),
                    wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => Some(1),
                    _ => None,
                };
                if rank(f).is_some() && c.form.is_none_or(|(g, ..)| rank(f) < rank(g)) {
                    c.form = Some((f, width, height, stride));
                }
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => c.told = true,
            zwlr_screencopy_frame_v1::Event::Flags { flags: WEnum::Value(f) } => {
                c.upside_down = f.contains(zwlr_screencopy_frame_v1::Flags::YInvert);
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => c.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => c.failed = true,
            _ => {}
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut State, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
        }
    )*};
}
ignore!(ZwlrScreencopyManagerV1, WlShm, WlShmPool, WlBuffer);

/// Shared memory the compositor copies the screen into.
struct Memory {
    _file: File,
    at: *mut u8,
    len: usize,
    pool: WlShmPool,
    buffer: WlBuffer,
    form: (wl_shm::Format, u32, u32, u32),
}

impl Drop for Memory {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
        // SAFETY: the mapping made in `memory`, no longer used.
        unsafe { libc::munmap(self.at as *mut libc::c_void, self.len) };
    }
}

/// A monitor as the copying thread last heard of it.
#[derive(Clone, PartialEq)]
struct Told {
    name: String,
    size: (i32, i32),
    at: (i32, i32),
    scale: i32,
}

fn told(outs: &[Out]) -> Vec<Told> {
    outs.iter().map(|o| Told { name: o.name.clone(), size: o.size, at: o.at, scale: o.scale }).collect()
}

/// What the copying thread and the capture share.
struct Latest {
    /// The newest picture, and whether it was taken yet.
    img: Image,
    fresh: bool,
    /// The monitor wanted and how many times one was asked for; the ask the picture answers
    /// (a picture of the monitor shown before is not given).
    want: (usize, u64),
    answers: u64,
    outs: Vec<Told>,
    /// Why copying stopped for good.
    error: Option<String>,
    stop: bool,
}

struct Shared {
    latest: std::sync::Mutex<Latest>,
    came: std::sync::Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Latest> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// At most this many pictures a second (a viewer asks for up to 60): changes coming faster wait.
const MIN_GAP: Duration = Duration::from_millis(15);
/// A copy asked for this long after the last one came may have missed a change: Hyprland's
/// "with damage" waits for the next change, not for any since the last copy (wlroots counts them).
const LATE: Duration = Duration::from_millis(4);
/// How long the screen stays still before a late copy is taken over by a whole one.
const QUIET: Duration = Duration::from_millis(100);

/// The copy being made: the compositor's frame, whether its memory was given, whether it waits
/// for the screen to change.
struct Frame {
    proxy: ZwlrScreencopyFrameV1,
    given: bool,
    whole: bool,
}

/// The copying, on a thread of its own: the next copy is asked for as soon as one comes, so a
/// change made while the picture is not taken (the viewer's pace) still reaches the next one.
struct Copier {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    frame: Option<Frame>,
    memory: Option<Memory>,
    shared: std::sync::Arc<Shared>,
}

// SAFETY: the connection and the mapping are used only by the copying thread.
unsafe impl Send for Copier {}

impl Copier {
    /// Reads what the compositor sent, waiting up to `wait` for it.
    fn pump(&mut self, wait: Duration) -> Result<(), String> {
        let _ = self.conn.flush();
        self.queue.dispatch_pending(&mut self.state).map_err(|e| e.to_string())?;
        if let Some(guard) = self.queue.prepare_read() {
            let mut fd = libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one pollfd, for the connection's open descriptor.
            if unsafe { libc::poll(&mut fd, 1, wait.as_millis().min(1000) as i32) } > 0 {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(format!("the Wayland display went away ({e})")),
                }
            }
        }
        self.queue.dispatch_pending(&mut self.state).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn drop_frame(&mut self) {
        if let Some(f) = self.frame.take() {
            f.proxy.destroy();
        }
        self.state.copying = Copying::default();
    }

    /// Asks for a copy of monitor `out`: `whole` at once, otherwise once the screen changes.
    fn ask(&mut self, out: usize, whole: bool) -> Result<(), String> {
        let output = self.state.outs.get(out).ok_or("the monitor is gone")?.output.clone();
        self.drop_frame();
        // The pointer drawn in: this desktop does not tell it apart.
        let proxy = self.state.manager.as_ref().ok_or("no screen copying")?.0.capture_output(1, &output, &self.queue.handle(), ());
        self.frame = Some(Frame { proxy, given: false, whole });
        let _ = self.conn.flush();
        Ok(())
    }

    /// Gives the copy its memory once the compositor told what it takes; true when given.
    fn give(&mut self) -> Result<bool, String> {
        let version = self.state.manager.as_ref().map_or(1, |m| m.1);
        let Some(f) = self.frame.as_mut().filter(|f| !f.given) else { return Ok(false) };
        let c = &self.state.copying;
        if c.form.is_none() || (version >= 3 && !c.told) {
            return Ok(false);
        }
        let form = c.form.ok_or("no form of memory this program reads")?;
        if self.memory.as_ref().is_none_or(|m| m.form != form) {
            self.memory = None;
            self.memory = Some(memory(self.state.shm.as_ref().ok_or("no shared memory")?, form, &self.queue.handle())?);
        }
        let buffer = &self.memory.as_ref().expect("made above").buffer;
        if f.whole || version < 2 {
            f.proxy.copy(buffer);
        } else {
            f.proxy.copy_with_damage(buffer);
        }
        f.given = true;
        let _ = self.conn.flush();
        Ok(true)
    }

    /// The picture in the memory, the right way up and in BGRX order.
    fn picture(&self, upside_down: bool, mut data: Vec<u8>) -> Image {
        let m = self.memory.as_ref().expect("given");
        let (format, w, h, stride) = (m.form.0, m.form.1 as usize, m.form.2 as usize, m.form.3 as usize);
        // SAFETY: the mapping holds `len` bytes, the compositor done writing them (ready).
        let px = unsafe { std::slice::from_raw_parts(m.at, m.len) };
        data.clear();
        data.reserve(w * h * 4);
        for y in 0..h {
            let row = if upside_down { h - 1 - y } else { y };
            data.extend_from_slice(&px[row * stride..row * stride + w * 4]);
        }
        if matches!(format, wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888) {
            for p in data.as_chunks_mut::<4>().0 {
                p.swap(0, 2);
            }
        }
        Image { w, h, stride: w * 4, data }
    }

    fn run(mut self) {
        if let Err(e) = self.copy_on() {
            self.shared.lock().error = Some(e);
            self.shared.came.notify_all();
        }
        self.drop_frame();
        self.memory = None;
        let _ = self.conn.flush();
    }

    fn copy_on(&mut self) -> Result<(), String> {
        let version = self.state.manager.as_ref().map_or(1, |m| m.1);
        // The monitor and ask being copied, whether a picture came for it, when the last came,
        // what the next copy is and when it may be asked for, and when a late one gives way to a
        // whole one.
        let (mut asked, mut pictured) = ((usize::MAX, u64::MAX), false);
        let (mut came, mut whole, mut not_before, mut until_quiet) = (None::<Instant>, true, Instant::now(), None::<Instant>);
        // A picture's memory, used again.
        let mut spare = Vec::new();
        loop {
            let want = {
                let mut l = self.shared.lock();
                if l.stop {
                    return Ok(());
                }
                let outs = told(&self.state.outs);
                if l.outs != outs {
                    l.outs = outs;
                }
                l.want
            };
            if want != asked {
                self.drop_frame();
                (asked, pictured, whole, not_before, until_quiet) = (want, false, true, Instant::now(), None);
            }
            let now = Instant::now();
            if until_quiet.is_some_and(|t| now >= t) && self.frame.as_ref().is_some_and(|f| !f.whole) {
                self.drop_frame();
                (whole, until_quiet) = (true, None);
            }
            if self.frame.is_none() && now >= not_before {
                self.ask(asked.0, whole)?;
            }
            if self.give()? && !whole && came.is_some_and(|c| c.elapsed() > LATE) {
                until_quiet = Some(Instant::now() + QUIET);
            }
            let c = &self.state.copying;
            if c.failed {
                self.drop_frame();
                // The first copy refused (Hyprland with its permissions enforced): the portal's turn.
                if !pictured && asked.1 == 0 {
                    return Err("the screen copy failed".into());
                }
                // The monitor turned off, say: asked again in a moment.
                (whole, not_before) = (true, Instant::now() + Duration::from_millis(200));
            } else if c.ready {
                let upside_down = c.upside_down;
                let at = Instant::now();
                // Faster than pictures are wanted: the next copy waits a little.
                not_before = came.filter(|c| at.duration_since(*c) < MIN_GAP).map_or(at, |c| c + MIN_GAP);
                if version < 2 {
                    not_before = at + Duration::from_millis(33);
                }
                (came, whole, until_quiet, pictured) = (Some(at), false, None, true);
                // The next copy is asked for first, so its answer comes while this one is read.
                self.drop_frame();
                if at >= not_before {
                    self.ask(asked.0, false)?;
                }
                let mut img = self.picture(upside_down, std::mem::take(&mut spare));
                let mut l = self.shared.lock();
                if l.want == asked {
                    std::mem::swap(&mut l.img, &mut img);
                    (l.fresh, l.answers) = (true, asked.1);
                    self.shared.came.notify_all();
                }
                spare = img.data;
                continue;
            }
            // Nothing to do until the compositor answers, the next copy may go, or a late one
            // has waited long enough.
            let mut wait = Duration::from_millis(50);
            if self.frame.is_none() {
                wait = wait.min(not_before.saturating_duration_since(Instant::now()));
            }
            if let Some(t) = until_quiet {
                wait = wait.min(t.saturating_duration_since(Instant::now()));
            }
            self.pump(wait)?;
        }
    }
}

pub struct WlrCopy {
    shared: std::sync::Arc<Shared>,
    copier: Option<std::thread::JoinHandle<()>>,
    current: usize,
    desk: (usize, usize),
    target: (usize, usize),
    shrink: super::Shrink,
    /// The newest picture taken, and whether it is to be given again (the first look, a new size).
    last: Image,
    again: bool,
    input: super::wlinput::WlInput,
}

impl WlrCopy {
    pub fn open() -> Result<WlrCopy, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland display ({e})"))?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut state = State::default();
        // The globals, then what the monitors tell of themselves.
        for _ in 0..2 {
            queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        }
        if state.manager.is_none() || state.shm.is_none() || state.outs.is_empty() {
            return Err("this desktop does not copy its screen for programs (wlr-screencopy)".into());
        }
        // Without its keyboard and pointer the portal does better (it may take input).
        let input = super::wlinput::WlInput::open()?;
        if !input.has_pointer() {
            return Err("this desktop offers no virtual pointer".into());
        }
        let latest = Latest {
            img: Image { w: 0, h: 0, stride: 0, data: Vec::new() },
            fresh: false,
            want: (0, 0),
            answers: u64::MAX,
            outs: told(&state.outs),
            error: None,
            stop: false,
        };
        let shared = std::sync::Arc::new(Shared { latest: std::sync::Mutex::new(latest), came: std::sync::Condvar::new() });
        let copier = Copier { conn, queue, state, frame: None, memory: None, shared: shared.clone() };
        let copier = std::thread::Builder::new().name("wlr-screencopy".into()).spawn(move || copier.run()).map_err(|e| e.to_string())?;
        let mut w = WlrCopy {
            shared,
            copier: Some(copier),
            current: 0,
            desk: (0, 0),
            target: (0, 0),
            shrink: super::Shrink::default(),
            last: Image { w: 0, h: 0, stride: 0, data: Vec::new() },
            again: true,
            input,
        };
        // The first picture, which also tells the screen's size; a compositor that refuses
        // (Hyprland with its permissions enforced) leaves it to the portal.
        if !w.take(Some(Duration::from_secs(5)))? {
            return Err("the screen copy gave no picture".into());
        }
        (w.desk, w.target) = ((w.last.w, w.last.h), (w.last.w, w.last.h));
        Ok(w)
    }

    /// Takes the newest picture if one came since, waiting up to `wait` for it; true if taken.
    fn take(&mut self, wait: Option<Duration>) -> Result<bool, String> {
        let until = wait.map(|w| Instant::now() + w);
        let mut l = self.shared.lock();
        loop {
            if let Some(e) = &l.error {
                return Err(e.clone());
            }
            if l.fresh && l.answers == l.want.1 {
                l.fresh = false;
                std::mem::swap(&mut self.last, &mut l.img);
                return Ok(true);
            }
            let Some(left) = until.map(|u| u.saturating_duration_since(Instant::now())).filter(|d| !d.is_zero()) else {
                return Ok(false);
            };
            l = self.shared.came.wait_timeout(l, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

/// Shared memory of `form` for the compositor to copy into.
fn memory(shm: &WlShm, form: (wl_shm::Format, u32, u32, u32), qh: &QueueHandle<State>) -> Result<Memory, String> {
    let (format, w, h, stride) = form;
    let len = stride as usize * h as usize;
    let file = super::wlinput::tempfile_fd()?;
    file.set_len(len as u64).map_err(|e| e.to_string())?;
    // SAFETY: a shared mapping of the file just sized, unmapped in Drop.
    let at = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, file.as_raw_fd(), 0) };
    if at == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, format, qh, ());
    Ok(Memory { _file: file, at: at as *mut u8, len, pool, buffer, form })
}

impl Capture for WlrCopy {
    fn name(&self) -> &'static str {
        "wlroots"
    }

    fn desktop_size(&self) -> (usize, usize) {
        self.desk
    }

    fn monitors(&self) -> Vec<Monitor> {
        self.shared
            .lock()
            .outs
            .iter()
            .enumerate()
            .map(|(i, o)| Monitor { name: o.name.clone(), w: o.size.0.max(0) as usize, h: o.size.1.max(0) as usize, primary: i == 0 })
            .collect()
    }

    fn current(&self) -> usize {
        self.current
    }

    fn screen_id(&self) -> Option<String> {
        self.shared.lock().outs.get(self.current).map(|o| o.name.clone())
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        {
            let mut l = self.shared.lock();
            if index >= l.outs.len() {
                return Err("no such monitor".into());
            }
            l.want = (index, l.want.1 + 1);
            l.fresh = false;
        }
        (self.current, self.last) = (index, Image { w: 0, h: 0, stride: 0, data: Vec::new() });
        self.take(Some(Duration::from_secs(5)))?;
        (self.desk, self.target, self.again) = ((self.last.w, self.last.h), (self.last.w, self.last.h), true);
        Ok(())
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.target = (w.max(1), h.max(1));
        self.again = true;
        Ok(())
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        if self.take(None)? {
            // Its resolution changed or it turned: the viewer hears the new size.
            self.desk = (self.last.w, self.last.h);
        } else if !std::mem::take(&mut self.again) {
            return Ok(None);
        }
        let img = &self.last;
        if self.target == (img.w, img.h) {
            return Ok(Some(Image { w: img.w, h: img.h, stride: img.stride, data: img.data.clone() }));
        }
        Ok(Some(self.shrink.image(&img.data, img.w, img.h, img.stride, self.target.0, self.target.1)))
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        // The virtual pointer goes over all the monitors as one: points on this one are placed
        // where it lies among them.
        let ev = match ev {
            Input::Move { x, y } => {
                let placed = {
                    let l = self.shared.lock();
                    l.outs.get(self.current).filter(|_| l.outs.len() > 1).map(|o| {
                        let s = o.scale as f64;
                        let extent = l.outs.iter().fold((0, 0), |(w, h), o| (w.max(o.at.0 + o.size.0 / o.scale), h.max(o.at.1 + o.size.1 / o.scale)));
                        ((o.at.0 as f64 + x / s, o.at.1 as f64 + y / s), extent)
                    })
                };
                if let Some(((x, y), extent)) = placed {
                    return self.input.input(Input::Move { x, y }, (extent.0.max(1) as usize, extent.1.max(1) as usize));
                }
                Input::Move { x, y }
            }
            ev => ev,
        };
        self.input.input(ev, self.desk)
    }
}

impl Drop for WlrCopy {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        if let Some(t) = self.copier.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The screen read with no dialog, and it keeps coming: keys typed into the focused terminal
    /// show in a later picture, and come out as typed (Shift's characters, other scripts, Ctrl).
    /// With SSHREMOTE_WLR_BOUND=FILE the desktop has Super+B bound to touch FILE: the shortcut
    /// must reach it. In a wlroots desktop's session: SSHREMOTE_WLR_TEST=1 TESTS --ignored
    /// a_wlroots_screen_is_copied_without_asking
    #[test]
    #[ignore]
    fn a_wlroots_screen_is_copied_without_asking() {
        if std::env::var_os("SSHREMOTE_WLR_TEST").is_none() {
            return;
        }
        let mut w = WlrCopy::open().expect("screen copying");
        let first = w.grab().unwrap().expect("the first picture");
        assert_eq!((first.w, first.h), w.desktop_size());
        assert!(!w.monitors().is_empty());
        // Each key shows soon after it is typed.
        let mut seen = first.data;
        for c in "echo copied".chars() {
            let typed = std::time::Instant::now();
            w.input(Input::Key { keysym: crate::xk::of_char(c), mods: 0 }).unwrap();
            let shown = loop {
                if let Some(img) = w.grab().unwrap()
                    && img.data != seen
                {
                    seen = img.data;
                    break typed.elapsed();
                }
                assert!(typed.elapsed() < std::time::Duration::from_secs(2), "a picture after {c:?}");
                std::thread::sleep(std::time::Duration::from_millis(2));
            };
            eprintln!("{c:?} shown after {shown:?}");
            assert!(shown < std::time::Duration::from_millis(500), "{c:?} shown after {shown:?}");
        }
        let wait = |what: &dyn Fn() -> bool| {
            let since = std::time::Instant::now();
            while !what() && since.elapsed() < std::time::Duration::from_secs(3) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            what()
        };
        let ctrl = |w: &mut WlrCopy, c: char| w.input(Input::Key { keysym: c as u32, mods: crate::proto::MOD_CTRL }).unwrap();
        let typed = std::env::temp_dir().join("sshremote-wlr-typed");
        let _ = std::fs::remove_file(&typed);
        let line = "Hello, World! @#$%^&*()_+{}|:\"<>? ~ 안녕 üñ";
        ctrl(&mut w, 'u');
        for c in format!("cat > {}\n{line}\n", typed.display()).chars() {
            let keysym = if c == '\n' { crate::xk::RETURN } else { crate::xk::of_char(c) };
            w.input(Input::Key { keysym, mods: 0 }).unwrap();
        }
        // The viewer's own Shift held around a key's own character.
        for (sym, down) in [(0xffe1, true), ('1' as u32, true), ('1' as u32, false), (0xffe1, false), ('a' as u32, true), ('a' as u32, false), (0xff0d, true), (0xff0d, false)] {
            w.input(Input::KeyState { keysym: sym, down }).unwrap();
        }
        ctrl(&mut w, 'd');
        assert!(wait(&|| std::fs::read_to_string(&typed).is_ok_and(|t| t.ends_with('\n') && t.lines().count() == 2)), "typed into the terminal");
        assert_eq!(std::fs::read_to_string(&typed).unwrap(), format!("{line}\n!a\n"));
        if let Some(bound) = std::env::var_os("SSHREMOTE_WLR_BOUND") {
            let _ = std::fs::remove_file(&bound);
            for (sym, down) in [(crate::xk::SUPER_L, true), ('b' as u32, true), ('b' as u32, false), (crate::xk::SUPER_L, false)] {
                w.input(Input::KeyState { keysym: sym, down }).unwrap();
            }
            assert!(wait(&|| std::path::Path::new(&bound).exists()), "Super+B reached the desktop's shortcut");
        }
        // Smaller for a smaller view.
        w.set_target(first.w / 2, first.h / 2).unwrap();
        let half = w.grab().unwrap().expect("a picture at the new size");
        assert_eq!((half.w, half.h), (first.w / 2, first.h / 2));
    }

    /// Grabbed at a viewer's pace while keys come in between, the picture ends where the screen
    /// does: a change made while no copy was asked for still reaches it. In a wlroots desktop's
    /// session with a terminal focused: SSHREMOTE_WLR_TEST=1 TESTS --ignored the_last_change
    #[test]
    #[ignore]
    fn the_last_change_reaches_the_picture() {
        use std::time::{Duration, Instant};
        if std::env::var_os("SSHREMOTE_WLR_TEST").is_none() {
            return;
        }
        let mut w = WlrCopy::open().expect("screen copying");
        let mut last = w.grab().unwrap().expect("the first picture");
        for round in 0..3 {
            let (start, mut typed) = (Instant::now(), 0);
            while start.elapsed() < Duration::from_millis(1500) {
                if typed < 6 && start.elapsed() >= Duration::from_millis(45 * typed as u64) {
                    let keysym = if round == 1 { crate::xk::BACKSPACE } else { b"sshrem"[typed] as u32 };
                    w.input(Input::Key { keysym, mods: 0 }).unwrap();
                    typed += 1;
                }
                if let Some(img) = w.grab().unwrap() {
                    last = img;
                }
                std::thread::sleep(Duration::from_millis(33));
            }
            let now = WlrCopy::open().expect("another copy").grab().unwrap().expect("its picture");
            let differ = last.data.iter().zip(&now.data).filter(|(a, b)| a != b).count();
            assert_eq!(differ, 0, "round {round}: the picture is behind the screen ({differ} bytes differ)");
        }
        for _ in 0..6 {
            w.input(Input::Key { keysym: crate::xk::BACKSPACE, mods: 0 }).unwrap();
        }
    }
}
