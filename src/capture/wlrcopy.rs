//! Wayland desktops built on wlroots and its kin (Hyprland, Sway, river, niri...): the screen read
//! through wlr-screencopy, as grim and wayvnc read it, with no portal and so no dialog for someone
//! at the screen to answer; input through the virtual pointer and keyboard (`wlinput`). A copy
//! waits for the screen to change, so a still screen costs nothing.

use std::fs::File;
use std::os::fd::{AsFd, AsRawFd};

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

pub struct WlrCopy {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    current: usize,
    /// The copy asked for, and whether its memory was given yet.
    frame: Option<(ZwlrScreencopyFrameV1, bool)>,
    memory: Option<Memory>,
    desk: (usize, usize),
    target: (usize, usize),
    shrink: super::Shrink,
    /// The newest picture, and whether it is to be given again (the first look, a new size).
    last: Image,
    again: bool,
    input: super::wlinput::WlInput,
}

// SAFETY: the connection and the mapping are used from the thread that holds them.
unsafe impl Send for WlrCopy {}

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
        let mut w = WlrCopy {
            conn,
            queue,
            state,
            current: 0,
            frame: None,
            memory: None,
            desk: (0, 0),
            target: (0, 0),
            shrink: super::Shrink::default(),
            last: Image { w: 0, h: 0, stride: 0, data: Vec::new() },
            again: true,
            input,
        };
        // The first picture, which also tells the screen's size; a compositor that refuses
        // (Hyprland with its permissions enforced) leaves it to the portal.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while w.last.w == 0 {
            w.step()?;
            if std::time::Instant::now() > until {
                return Err("the screen copy gave no picture".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        (w.desk, w.target) = ((w.last.w, w.last.h), (w.last.w, w.last.h));
        Ok(w)
    }

    /// Reads what the compositor sent, without waiting.
    fn pump(&mut self) -> Result<(), String> {
        let _ = self.conn.flush();
        if let Some(guard) = self.queue.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(format!("the Wayland display went away ({e})")),
            }
        }
        self.queue.dispatch_pending(&mut self.state).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Moves the copy on: asks for one, gives it memory, takes the picture when it is ready.
    /// True when a new picture came.
    fn step(&mut self) -> Result<bool, String> {
        self.pump()?;
        let qh = self.queue.handle();
        let Some((manager, version)) = self.state.manager.clone() else { return Err("no screen copying".into()) };
        let out = self.state.outs.get(self.current).ok_or("the monitor is gone")?.output.clone();
        let Some((frame, given)) = &mut self.frame else {
            self.state.copying = Copying::default();
            // The pointer drawn in: this desktop does not tell it apart.
            self.frame = Some((manager.capture_output(1, &out, &qh, ()), false));
            let _ = self.conn.flush();
            return Ok(false);
        };
        let c = &self.state.copying;
        if c.failed {
            frame.destroy();
            self.frame = None;
            return Err("the screen copy failed".into());
        }
        if !*given {
            if c.form.is_none() || (version >= 3 && !c.told) {
                return Ok(false);
            }
            let form = c.form.ok_or("no form of memory this program reads")?;
            if self.memory.as_ref().is_none_or(|m| m.form != form) {
                self.memory = None;
                self.memory = Some(memory(self.state.shm.as_ref().ok_or("no shared memory")?, form, &qh)?);
            }
            let buffer = &self.memory.as_ref().expect("made above").buffer;
            // After the first, a copy waits for the screen to change.
            if version >= 2 && self.last.w > 0 {
                frame.copy_with_damage(buffer);
            } else {
                frame.copy(buffer);
            }
            *given = true;
            let _ = self.conn.flush();
            return Ok(false);
        }
        if !c.ready {
            return Ok(false);
        }
        let upside_down = c.upside_down;
        frame.destroy();
        self.frame = None;
        let m = self.memory.as_ref().expect("given above");
        let (format, w, h, stride) = (m.form.0, m.form.1 as usize, m.form.2 as usize, m.form.3 as usize);
        // SAFETY: the mapping holds `len` bytes, the compositor done writing them (ready).
        let px = unsafe { std::slice::from_raw_parts(m.at, m.len) };
        let mut data = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let row = if upside_down { h - 1 - y } else { y };
            data.extend_from_slice(&px[row * stride..row * stride + w * 4]);
        }
        if matches!(format, wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888) {
            for p in data.as_chunks_mut::<4>().0 {
                p.swap(0, 2);
            }
        }
        self.last = Image { w, h, stride: w * 4, data };
        Ok(true)
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
        self.state
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
        self.state.outs.get(self.current).map(|o| o.name.clone())
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        if index >= self.state.outs.len() {
            return Err("no such monitor".into());
        }
        if let Some((f, _)) = self.frame.take() {
            f.destroy();
        }
        (self.current, self.last) = (index, Image { w: 0, h: 0, stride: 0, data: Vec::new() });
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.last.w == 0 && std::time::Instant::now() < until {
            self.step()?;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        (self.desk, self.target, self.again) = ((self.last.w, self.last.h), (self.last.w, self.last.h), true);
        Ok(())
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.target = (w.max(1), h.max(1));
        self.again = true;
        Ok(())
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        // A copy that failed (the monitor turned off, say) is asked for again.
        let fresh = match self.step() {
            Ok(f) => f,
            Err(e) if e == "the screen copy failed" => false,
            Err(e) => return Err(e),
        };
        if fresh {
            // Its resolution changed or it turned: the viewer hears the new size.
            self.desk = (self.last.w, self.last.h);
            // The next copy waits for a change meanwhile.
            self.step()?;
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
            Input::Move { x, y } if self.state.outs.len() > 1 => {
                let o = &self.state.outs[self.current];
                let s = o.scale as f64;
                let (x, y) = (o.at.0 as f64 + x / s, o.at.1 as f64 + y / s);
                let extent = self.state.outs.iter().fold((0, 0), |(w, h), o| (w.max(o.at.0 + o.size.0 / o.scale), h.max(o.at.1 + o.size.1 / o.scale)));
                return self.input.input(Input::Move { x, y }, (extent.0.max(1) as usize, extent.1.max(1) as usize));
            }
            ev => ev,
        };
        self.input.input(ev, self.desk)
    }
}

impl Drop for WlrCopy {
    fn drop(&mut self) {
        if let Some((f, _)) = self.frame.take() {
            f.destroy();
        }
        self.memory = None;
        let _ = self.conn.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The screen read with no dialog, and it keeps coming: keys typed into the focused terminal
    /// show in a later picture. In a wlroots desktop's session: SSHREMOTE_WLR_TEST=1 TESTS
    /// --ignored a_wlroots_screen_is_copied_without_asking
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
        // Smaller for a smaller view.
        w.set_target(first.w / 2, first.h / 2).unwrap();
        let half = w.grab().unwrap().expect("a picture at the new size");
        assert_eq!((half.w, half.h), (first.w / 2, first.h / 2));
    }
}
