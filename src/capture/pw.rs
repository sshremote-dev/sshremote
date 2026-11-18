//! PipeWire video streams read here rather than through GStreamer, so that the pointer can come
//! apart from the pictures: Mutter (and the portals) send its picture and place as each buffer's
//! cursor metadata, which GStreamer's reader drops. libpipewire is loaded when first needed;
//! without it GStreamer reads the stream and the pointer stays in the pictures.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

type P = *mut c_void;

const TYPE_ID: u32 = 3;
const TYPE_INT: u32 = 4;
const TYPE_RECTANGLE: u32 = 10;
const TYPE_FRACTION: u32 = 11;
const TYPE_OBJECT: u32 = 15;
const TYPE_CHOICE: u32 = 19;
const CHOICE_RANGE: u32 = 1;
const CHOICE_FLAGS: u32 = 4;
const OBJECT_FORMAT: u32 = 0x40003;
const OBJECT_BUFFERS: u32 = 0x40004;
const OBJECT_META: u32 = 0x40005;
const PARAM_ENUM_FORMAT: u32 = 3;
const PARAM_FORMAT: u32 = 4;
const PARAM_BUFFERS: u32 = 5;
const PARAM_META: u32 = 6;
const FORMAT_MEDIA_TYPE: u32 = 1;
const FORMAT_MEDIA_SUBTYPE: u32 = 2;
const MEDIA_VIDEO: u32 = 2;
const SUBTYPE_RAW: u32 = 1;
const VIDEO_FORMAT: u32 = 0x20001;
const VIDEO_SIZE: u32 = 0x20003;
const VIDEO_FRAMERATE: u32 = 0x20004;
const RGBX: u32 = 7;
const BGRX: u32 = 8;
const RGBA: u32 = 11;
const BGRA: u32 = 12;
const BUFFERS_DATA_TYPE: u32 = 6;
const META_TYPE: u32 = 1;
const META_SIZE: u32 = 2;
const META_HEADER: u32 = 1;
const META_CROP: u32 = 2;
const META_CURSOR: u32 = 5;
const DATA_MEM_PTR: u32 = 1;
const DATA_MEM_FD: u32 = 2;
const CHUNK_CORRUPTED: i32 = 1;
const DATA_READABLE: u32 = 1;
const DIRECTION_INPUT: u32 = 0;
const FLAG_AUTOCONNECT: u32 = 1;
const FLAG_MAP_BUFFERS: u32 = 1 << 2;
const FLAG_DONT_RECONNECT: u32 = 1 << 7;
/// struct spa_meta_cursor and struct spa_meta_bitmap.
const CURSOR_LEN: usize = 28;
const BITMAP_LEN: usize = 20;
/// The largest pointer picture asked for, as GStreamer's reader and Mutter have it.
const CURSOR_META_MAX: i32 = (CURSOR_LEN + BITMAP_LEN + 384 * 384 * 4) as i32;

struct Lib {
    thread_loop_new: unsafe extern "C" fn(*const c_char, *const c_void) -> P,
    thread_loop_get_loop: unsafe extern "C" fn(P) -> P,
    thread_loop_start: unsafe extern "C" fn(P) -> c_int,
    thread_loop_stop: unsafe extern "C" fn(P),
    thread_loop_lock: unsafe extern "C" fn(P),
    thread_loop_unlock: unsafe extern "C" fn(P),
    thread_loop_destroy: unsafe extern "C" fn(P),
    context_new: unsafe extern "C" fn(P, P, usize) -> P,
    context_destroy: unsafe extern "C" fn(P),
    context_connect: unsafe extern "C" fn(P, P, usize) -> P,
    context_connect_fd: unsafe extern "C" fn(P, c_int, P, usize) -> P,
    core_disconnect: unsafe extern "C" fn(P) -> c_int,
    properties_new_string: unsafe extern "C" fn(*const c_char) -> P,
    stream_new: unsafe extern "C" fn(P, *const c_char, P) -> P,
    stream_add_listener: unsafe extern "C" fn(P, P, *const Events, P),
    stream_connect: unsafe extern "C" fn(P, u32, u32, u32, *const *const u64, u32) -> c_int,
    stream_update_params: unsafe extern "C" fn(P, *const *const u64, u32) -> c_int,
    stream_dequeue_buffer: unsafe extern "C" fn(P) -> *mut PwBuffer,
    stream_queue_buffer: unsafe extern "C" fn(P, *mut PwBuffer) -> c_int,
    stream_disconnect: unsafe extern "C" fn(P) -> c_int,
    stream_destroy: unsafe extern "C" fn(P),
}

/// # Safety
/// `T` must be the function pointer type of the symbol `name`.
unsafe fn sym<T: Copy>(lib: P, name: &CStr) -> Option<T> {
    // SAFETY: dlsym on a handle from dlopen; the caller names the symbol's type.
    let p = unsafe { libc::dlsym(lib, name.as_ptr()) };
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<P, T>(&p) })
}

fn lib() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(|| unsafe {
        // SAFETY: the library is never closed; each symbol gets its C type.
        let h = libc::dlopen(c"libpipewire-0.3.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if h.is_null() {
            return None;
        }
        let init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) = sym(h, c"pw_init")?;
        init(std::ptr::null_mut(), std::ptr::null_mut());
        Some(Lib {
            thread_loop_new: sym(h, c"pw_thread_loop_new")?,
            thread_loop_get_loop: sym(h, c"pw_thread_loop_get_loop")?,
            thread_loop_start: sym(h, c"pw_thread_loop_start")?,
            thread_loop_stop: sym(h, c"pw_thread_loop_stop")?,
            thread_loop_lock: sym(h, c"pw_thread_loop_lock")?,
            thread_loop_unlock: sym(h, c"pw_thread_loop_unlock")?,
            thread_loop_destroy: sym(h, c"pw_thread_loop_destroy")?,
            context_new: sym(h, c"pw_context_new")?,
            context_destroy: sym(h, c"pw_context_destroy")?,
            context_connect: sym(h, c"pw_context_connect")?,
            context_connect_fd: sym(h, c"pw_context_connect_fd")?,
            core_disconnect: sym(h, c"pw_core_disconnect")?,
            properties_new_string: sym(h, c"pw_properties_new_string")?,
            stream_new: sym(h, c"pw_stream_new")?,
            stream_add_listener: sym(h, c"pw_stream_add_listener")?,
            stream_connect: sym(h, c"pw_stream_connect")?,
            stream_update_params: sym(h, c"pw_stream_update_params")?,
            stream_dequeue_buffer: sym(h, c"pw_stream_dequeue_buffer")?,
            stream_queue_buffer: sym(h, c"pw_stream_queue_buffer")?,
            stream_disconnect: sym(h, c"pw_stream_disconnect")?,
            stream_destroy: sym(h, c"pw_stream_destroy")?,
        })
    })
    .as_ref()
}

/// A stream once failed here: the rest of this agent's life reads through GStreamer.
static BROKEN: AtomicBool = AtomicBool::new(false);

/// Whether streams can be read here (libpipewire loads, and reading has not failed before).
pub fn usable() -> bool {
    !BROKEN.load(Ordering::Relaxed) && lib().is_some()
}

/// Reading a stream here failed in a way GStreamer's reader may not.
pub fn give_up() {
    BROKEN.store(true, Ordering::Relaxed);
}

#[repr(C)]
struct Events {
    version: u32,
    destroy: Option<unsafe extern "C" fn(P)>,
    state_changed: Option<unsafe extern "C" fn(P, c_int, c_int, *const c_char)>,
    control_info: Option<unsafe extern "C" fn()>,
    io_changed: Option<unsafe extern "C" fn()>,
    param_changed: Option<unsafe extern "C" fn(P, u32, *const u8)>,
    add_buffer: Option<unsafe extern "C" fn()>,
    remove_buffer: Option<unsafe extern "C" fn()>,
    process: Option<unsafe extern "C" fn(P)>,
    drained: Option<unsafe extern "C" fn()>,
    command: Option<unsafe extern "C" fn()>,
    trigger_done: Option<unsafe extern "C" fn()>,
}

static EVENTS: Events = Events {
    version: 2,
    destroy: None,
    state_changed: Some(on_state),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_param),
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
    command: None,
    trigger_done: None,
};

#[repr(C)]
struct PwBuffer {
    buffer: *mut SpaBuffer,
}

#[repr(C)]
struct SpaBuffer {
    n_metas: u32,
    n_datas: u32,
    metas: *mut SpaMeta,
    datas: *mut SpaData,
}

#[repr(C)]
struct SpaMeta {
    kind: u32,
    size: u32,
    data: *mut u8,
}

#[repr(C)]
struct SpaData {
    kind: u32,
    flags: u32,
    fd: i64,
    mapoffset: u32,
    maxsize: u32,
    data: *mut u8,
    chunk: *mut SpaChunk,
}

#[repr(C)]
struct SpaChunk {
    offset: u32,
    size: u32,
    stride: i32,
    flags: i32,
}

/// A frame as the stream sent it, BGRx, with the part of it that holds the picture (a window
/// stream's buffers can be larger than its window).
pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
    pub crop: Option<(usize, usize, usize, usize)>,
}

impl Frame {
    pub fn pixels(&self) -> super::Pixels<'_> {
        super::Pixels { w: self.w, h: self.h, stride: self.w * 4, data: &self.data }
    }
}

/// The pointer's picture, RGBA not premultiplied; empty while the pointer is hidden.
pub struct Shape {
    pub w: usize,
    pub h: usize,
    pub hot: (usize, usize),
    pub rgba: Vec<u8>,
}

impl Shape {
    pub fn to_pointer(&self) -> super::Pointer {
        super::Pointer { w: self.w, h: self.h, hot: self.hot, rgba: self.rgba.clone() }
    }
}

/// Where the pointer is on the stream's frames (None while it is hidden or elsewhere), and its
/// newest picture with a number that changes with it.
#[derive(Clone, Default)]
pub struct PointerNow {
    pub at: Option<(i32, i32)>,
    pub shape: Option<Arc<Shape>>,
    pub serial: u64,
}

#[derive(Default)]
struct Shared {
    format: Option<(u32, usize, usize)>,
    frame: Option<Frame>,
    spare: Vec<Vec<u8>>,
    pointer: PointerNow,
    moved: bool,
    streaming: bool,
    failed: Option<String>,
    ended: bool,
}

/// What the stream's thread reaches through its callbacks' data.
struct Inner {
    lib: &'static Lib,
    stream: AtomicPtr<c_void>,
    shared: Mutex<Shared>,
    changed: Condvar,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Stream {
    lib: &'static Lib,
    tl: P,
    ctx: P,
    core: P,
    stream: P,
    inner: Arc<Inner>,
    /// struct spa_hook, which the stream keeps a pointer to.
    hook: Box<[usize; 8]>,
}

impl Stream {
    /// Reads PipeWire node `node`, on the portal's PipeWire connection `fd` (kept open by its
    /// owner) or the session's own, and waits up to `wait` for its first frame. A failure says
    /// whether it was this reader's (GStreamer's may do), or the stream sent nothing.
    pub fn open(fd: Option<RawFd>, node: u32, wait: Duration) -> Result<Stream, (String, bool)> {
        let lib = lib().ok_or(("libpipewire is not installed".to_string(), true))?;
        // SAFETY: PipeWire's thread loop, context, core and stream are made, used under the
        // loop's lock, and destroyed in Drop in reverse order.
        unsafe {
            let tl = (lib.thread_loop_new)(c"sshremote-capture".as_ptr(), std::ptr::null());
            if tl.is_null() {
                return Err(("PipeWire: no thread loop".into(), true));
            }
            let ctx = (lib.context_new)((lib.thread_loop_get_loop)(tl), std::ptr::null_mut(), 0);
            if ctx.is_null() {
                (lib.thread_loop_destroy)(tl);
                return Err(("PipeWire: no context".into(), true));
            }
            if (lib.thread_loop_start)(tl) < 0 {
                (lib.context_destroy)(ctx);
                (lib.thread_loop_destroy)(tl);
                return Err(("PipeWire: the thread loop did not start".into(), true));
            }
            let inner = Arc::new(Inner { lib, stream: AtomicPtr::new(std::ptr::null_mut()), shared: Mutex::default(), changed: Condvar::new() });
            let mut me = Stream { lib, tl, ctx, core: std::ptr::null_mut(), stream: std::ptr::null_mut(), inner, hook: Box::new([0; 8]) };
            (lib.thread_loop_lock)(tl);
            let made = me.connect(fd, node);
            (lib.thread_loop_unlock)(tl);
            made.map_err(|e| (e, true))?;
            me.first(wait)?;
            Ok(me)
        }
    }

    /// # Safety
    /// Called once, under the thread loop's lock.
    unsafe fn connect(&mut self, fd: Option<RawFd>, node: u32) -> Result<(), String> {
        let lib = self.lib;
        unsafe {
            self.core = match fd {
                Some(fd) => {
                    // The core closes the descriptor it is given; the portal keeps its own.
                    let own = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3);
                    if own < 0 {
                        return Err("PipeWire: the portal's connection could not be shared".into());
                    }
                    (lib.context_connect_fd)(self.ctx, own, std::ptr::null_mut(), 0)
                }
                None => (lib.context_connect)(self.ctx, std::ptr::null_mut(), 0),
            };
            if self.core.is_null() {
                return Err("PipeWire: no connection".into());
            }
            let props = (lib.properties_new_string)(c"media.type=Video media.category=Capture media.role=Screen".as_ptr());
            self.stream = (lib.stream_new)(self.core, c"sshremote".as_ptr(), props);
            if self.stream.is_null() {
                return Err("PipeWire: no stream".into());
            }
            self.inner.stream.store(self.stream, Ordering::Release);
            (lib.stream_add_listener)(self.stream, self.hook.as_mut_ptr().cast(), &EVENTS, Arc::as_ptr(&self.inner) as P);
            let formats: Vec<Vec<u64>> = [BGRX, BGRA, RGBX, RGBA].into_iter().map(format_param).collect();
            let params: Vec<*const u64> = formats.iter().map(|p| p.as_ptr()).collect();
            let flags = FLAG_AUTOCONNECT | FLAG_MAP_BUFFERS | FLAG_DONT_RECONNECT;
            let r = (lib.stream_connect)(self.stream, DIRECTION_INPUT, node, flags, params.as_ptr(), params.len() as u32);
            if r < 0 {
                return Err(format!("PipeWire: the stream did not connect ({r})"));
            }
        }
        Ok(())
    }

    fn first(&self, wait: Duration) -> Result<(), (String, bool)> {
        let until = Instant::now() + wait;
        let mut s = self.inner.lock();
        loop {
            if let Some(e) = &s.failed {
                return Err((format!("PipeWire: {e}"), true));
            }
            if s.frame.is_some() {
                return Ok(());
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // Agreed on a format and still nothing: the desktop sends nothing (locked, say).
                return Err(("no frame came".into(), s.format.is_none()));
            }
            s = self.inner.changed.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// The newest frame since the last look, and whether the pointer moved or changed meanwhile.
    pub fn take(&self) -> (Option<Frame>, bool) {
        let mut s = self.inner.lock();
        (s.frame.take(), std::mem::take(&mut s.moved))
    }

    /// A taken frame's memory, for a later one.
    pub fn recycle(&self, data: Vec<u8>) {
        let mut s = self.inner.lock();
        if s.spare.len() < 2 {
            s.spare.push(data);
        }
    }

    pub fn pointer(&self) -> PointerNow {
        self.inner.lock().pointer.clone()
    }

    /// Why the stream stopped, once it has: it failed, or its source went away.
    pub fn stopped(&self) -> Option<String> {
        let s = self.inner.lock();
        s.failed.clone().or_else(|| s.ended.then(|| "the stream ended".into()))
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let lib = self.lib;
        // SAFETY: as in `open`; after the stream is destroyed no callback runs.
        unsafe {
            (lib.thread_loop_lock)(self.tl);
            if !self.stream.is_null() {
                (lib.stream_disconnect)(self.stream);
                (lib.stream_destroy)(self.stream);
            }
            if !self.core.is_null() {
                (lib.core_disconnect)(self.core);
            }
            (lib.thread_loop_unlock)(self.tl);
            (lib.thread_loop_stop)(self.tl);
            (lib.context_destroy)(self.ctx);
            (lib.thread_loop_destroy)(self.tl);
        }
    }
}

unsafe extern "C" fn on_state(data: P, _old: c_int, state: c_int, error: *const c_char) {
    // SAFETY: `data` is the Inner the stream was given, alive until the stream is destroyed.
    let inner = unsafe { &*(data as *const Inner) };
    let mut s = inner.lock();
    match state {
        -1 => {
            let why = if error.is_null() { "the stream failed".into() } else { unsafe { CStr::from_ptr(error) }.to_string_lossy().into_owned() };
            s.failed = Some(why);
        }
        0 if s.streaming => s.ended = true,
        3 => s.streaming = true,
        _ => {}
    }
    inner.changed.notify_all();
}

unsafe extern "C" fn on_param(data: P, id: u32, param: *const u8) {
    // SAFETY: as in on_state; `param` is a whole pod, its size in its first word.
    let inner = unsafe { &*(data as *const Inner) };
    if id != PARAM_FORMAT || param.is_null() {
        return;
    }
    let pod = unsafe { std::slice::from_raw_parts(param, 8 + std::ptr::read_unaligned(param as *const u32) as usize) };
    let Some(format) = parse_format(pod) else {
        inner.lock().failed = Some("PipeWire offered no picture format read here".into());
        inner.changed.notify_all();
        return;
    };
    inner.lock().format = Some(format);
    let params = [
        object(OBJECT_BUFFERS, PARAM_BUFFERS, |p| {
            p.prop(BUFFERS_DATA_TYPE);
            p.choice(CHOICE_FLAGS, TYPE_INT, 4, &[(1 << DATA_MEM_PTR) | (1 << DATA_MEM_FD)]);
        }),
        meta_param(META_HEADER, 32),
        meta_param(META_CROP, 16),
        object(OBJECT_META, PARAM_META, |p| {
            p.prop(META_TYPE);
            p.id(META_CURSOR);
            p.prop(META_SIZE);
            p.choice(CHOICE_RANGE, TYPE_INT, 4, &[CURSOR_META_MAX as u32, CURSOR_LEN as u32, CURSOR_META_MAX as u32]);
        }),
    ];
    let ptrs: Vec<*const u64> = params.iter().map(|p| p.as_ptr()).collect();
    unsafe { (inner.lib.stream_update_params)(inner.stream.load(Ordering::Acquire), ptrs.as_ptr(), ptrs.len() as u32) };
}

unsafe extern "C" fn on_process(data: P) {
    // SAFETY: as in on_state; buffers are dequeued, read and queued back on this thread.
    let inner = unsafe { &*(data as *const Inner) };
    let (lib, stream) = (inner.lib, inner.stream.load(Ordering::Acquire));
    // Every buffer's pointer is read (its picture comes once, in whichever buffer); only the
    // newest frame is copied.
    let mut newest: *mut PwBuffer = std::ptr::null_mut();
    loop {
        let b = unsafe { (lib.stream_dequeue_buffer)(stream) };
        if b.is_null() {
            break;
        }
        let sb = unsafe { &*(*b).buffer };
        unsafe { read_pointer(inner, sb) };
        if unsafe { has_picture(sb) } {
            if !newest.is_null() {
                unsafe { (lib.stream_queue_buffer)(stream, newest) };
            }
            newest = b;
        } else {
            unsafe { (lib.stream_queue_buffer)(stream, b) };
        }
    }
    if !newest.is_null() {
        unsafe {
            copy_frame(inner, &*(*newest).buffer);
            (lib.stream_queue_buffer)(stream, newest);
        }
    }
}

/// # Safety
/// `sb` is a buffer dequeued from the stream.
unsafe fn meta(sb: &SpaBuffer, kind: u32) -> Option<&[u8]> {
    (0..sb.n_metas as usize).find_map(|i| {
        let m = unsafe { &*sb.metas.add(i) };
        (m.kind == kind && !m.data.is_null()).then(|| unsafe { std::slice::from_raw_parts(m.data, m.size as usize) })
    })
}

/// # Safety
/// As `meta`.
unsafe fn has_picture(sb: &SpaBuffer) -> bool {
    if sb.n_datas == 0 || sb.datas.is_null() {
        return false;
    }
    let d = unsafe { &*sb.datas };
    (!d.data.is_null() || (d.kind == DATA_MEM_FD && d.fd >= 0)) && !d.chunk.is_null() && {
        let c = unsafe { &*d.chunk };
        c.size > 0 && c.flags & CHUNK_CORRUPTED == 0
    }
}

/// # Safety
/// As `meta`.
unsafe fn copy_frame(inner: &Inner, sb: &SpaBuffer) {
    let (spare, format) = {
        let mut s = inner.lock();
        (s.spare.pop(), s.format)
    };
    let Some((format, w, h)) = format else { return };
    let d = unsafe { &*sb.datas };
    let c = unsafe { &*d.chunk };
    let stride = if c.stride > 0 { c.stride as usize } else { w * 4 };
    let offset = (c.offset % d.maxsize.max(1)) as usize;
    if w == 0 || h == 0 || stride < w * 4 || offset + stride * (h - 1) + w * 4 > d.maxsize as usize {
        return;
    }
    // A buffer its maker did not mark readable (xdg-desktop-portal-wlr's) is mapped by PipeWire
    // without the right to read it: mapped here for reading, for this copy.
    let mut own = None;
    let base = if d.flags & DATA_READABLE == 0 && d.kind == DATA_MEM_FD && d.fd >= 0 {
        let len = d.mapoffset as usize + d.maxsize as usize;
        // SAFETY: a shared read-only mapping of the buffer's file, unmapped below.
        let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, d.fd as c_int, 0) };
        if p == libc::MAP_FAILED {
            return;
        }
        own = Some((p, len));
        unsafe { (p as *const u8).add(d.mapoffset as usize) }
    } else if d.data.is_null() {
        return;
    } else {
        d.data as *const u8
    };
    let src = unsafe { std::slice::from_raw_parts(base.add(offset), stride * (h - 1) + w * 4) };
    let mut out = spare.unwrap_or_default();
    out.clear();
    out.reserve(w * h * 4);
    for y in 0..h {
        out.extend_from_slice(&src[y * stride..y * stride + w * 4]);
    }
    if let Some((p, len)) = own {
        // SAFETY: the mapping made above, no longer read.
        unsafe { libc::munmap(p, len) };
    }
    if matches!(format, RGBX | RGBA) {
        for px in out.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }
    let crop = unsafe { meta(sb, META_CROP) }.filter(|m| m.len() >= 16).and_then(|m| {
        let (x, y, cw, ch) = (i32_at(m, 0), i32_at(m, 4), u32_at(m, 8) as usize, u32_at(m, 12) as usize);
        (x >= 0 && y >= 0 && cw > 0 && ch > 0).then_some((x as usize, y as usize, cw, ch))
    });
    let mut s = inner.lock();
    if let Some(old) = s.frame.replace(Frame { w, h, data: out, crop }) {
        s.spare.push(old.data);
    }
    inner.changed.notify_all();
}

/// # Safety
/// As `meta`.
unsafe fn read_pointer(inner: &Inner, sb: &SpaBuffer) {
    let Some(m) = (unsafe { meta(sb, META_CURSOR) }) else { return };
    let Some(update) = pointer_update(m) else { return };
    let mut s = inner.lock();
    let p = &mut s.pointer;
    let mut moved = p.at != update.at;
    p.at = update.at;
    if let Some(shape) = update.shape {
        p.shape = Some(Arc::new(shape));
        p.serial += 1;
        moved = true;
    }
    s.moved |= moved;
}

struct Update {
    at: Option<(i32, i32)>,
    shape: Option<Shape>,
}

/// A buffer's cursor metadata: where the pointer is (an invalid id: hidden, or off this
/// stream), and its picture when it changed.
fn pointer_update(m: &[u8]) -> Option<Update> {
    if m.len() < CURSOR_LEN {
        return None;
    }
    if u32_at(m, 0) == 0 {
        return Some(Update { at: None, shape: None });
    }
    let at = (i32_at(m, 8), i32_at(m, 12));
    let hot = (i32_at(m, 16).max(0) as usize, i32_at(m, 20).max(0) as usize);
    let b = u32_at(m, 24) as usize;
    let shape = (b >= CURSOR_LEN && b + BITMAP_LEN <= m.len()).then(|| bitmap(&m[b..], hot)).flatten();
    Some(Update { at: Some(at), shape })
}

/// A struct spa_meta_bitmap and the pixels after it; None when it tells nothing new.
fn bitmap(m: &[u8], hot: (usize, usize)) -> Option<Shape> {
    let (format, w, h, stride, off) = (u32_at(m, 0), u32_at(m, 4) as usize, u32_at(m, 8) as usize, i32_at(m, 12), u32_at(m, 16) as usize);
    if format == 0 {
        return None;
    }
    let none = Shape { w: 0, h: 0, hot: (0, 0), rgba: Vec::new() };
    if off < BITMAP_LEN || w == 0 || h == 0 || !matches!(format, RGBA | BGRA | RGBX | BGRX) {
        return Some(none);
    }
    let stride = if stride > 0 { stride as usize } else { w * 4 };
    if stride < w * 4 || off + stride * (h - 1) + w * 4 > m.len() {
        return Some(none);
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for p in m[off + y * stride..off + y * stride + w * 4].as_chunks::<4>().0 {
            let [r, g, b, a] = match format {
                BGRA => [p[2], p[1], p[0], p[3]],
                BGRX => [p[2], p[1], p[0], 255],
                RGBX => [p[0], p[1], p[2], 255],
                _ => *p,
            };
            // Premultiplied, as Mutter and KWin send it.
            let un = |v: u8| (v as u32 * 255).checked_div(a as u32).map_or(0, |q| q.min(255) as u8);
            rgba.extend_from_slice(&[un(r), un(g), un(b), a]);
        }
    }
    Some(Shape { w, h, hot: (hot.0.min(w - 1), hot.1.min(h - 1)), rgba })
}

/// Draws `shape` with its point at `at` onto BGRx pixels.
pub fn draw(data: &mut [u8], w: usize, h: usize, stride: usize, shape: &Shape, at: (i32, i32)) {
    let (left, top) = (at.0 - shape.hot.0 as i32, at.1 - shape.hot.1 as i32);
    for row in 0..shape.h {
        let y = top + row as i32;
        if y < 0 || y >= h as i32 {
            continue;
        }
        for col in 0..shape.w {
            let x = left + col as i32;
            if x < 0 || x >= w as i32 {
                continue;
            }
            let s = &shape.rgba[(row * shape.w + col) * 4..][..4];
            let a = s[3] as u32;
            if a == 0 {
                continue;
            }
            let o = y as usize * stride + x as usize * 4;
            for (i, v) in [s[2], s[1], s[0]].into_iter().enumerate() {
                data[o + i] = ((v as u32 * a + data[o + i] as u32 * (255 - a)) / 255) as u8;
            }
        }
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map_or(0, |s| u32::from_ne_bytes([s[0], s[1], s[2], s[3]]))
}

fn i32_at(b: &[u8], at: usize) -> i32 {
    u32_at(b, at) as i32
}

/// SPA pods as PipeWire takes them: 8-byte aligned, each a size and a type word before its body.
#[derive(Default)]
struct Pod(Vec<u8>);

impl Pod {
    fn word(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_ne_bytes());
    }
    fn pad(&mut self) {
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
    }
    fn prop(&mut self, key: u32) {
        self.word(key);
        self.word(0);
    }
    fn id(&mut self, v: u32) {
        self.word(4);
        self.word(TYPE_ID);
        self.word(v);
        self.pad();
    }
    /// A choice of `kind` among values of `child` type, each `size` bytes, given as words.
    fn choice(&mut self, kind: u32, child: u32, size: u32, words: &[u32]) {
        self.word(16 + words.len() as u32 * 4);
        self.word(TYPE_CHOICE);
        self.word(kind);
        self.word(0);
        self.word(size);
        self.word(child);
        for &w in words {
            self.word(w);
        }
        self.pad();
    }
}

/// An object pod of `kind` and `id`, its properties written by `props`, in 8-byte words.
fn object(kind: u32, id: u32, props: impl FnOnce(&mut Pod)) -> Vec<u64> {
    let mut p = Pod::default();
    p.word(0);
    p.word(TYPE_OBJECT);
    p.word(kind);
    p.word(id);
    props(&mut p);
    let size = (p.0.len() - 8) as u32;
    p.0[..4].copy_from_slice(&size.to_ne_bytes());
    p.0.as_chunks::<8>().0.iter().map(|c| u64::from_ne_bytes(*c)).collect()
}

fn meta_param(kind: u32, size: i32) -> Vec<u64> {
    object(OBJECT_META, PARAM_META, |p| {
        p.prop(META_TYPE);
        p.id(kind);
        p.prop(META_SIZE);
        p.word(4);
        p.word(TYPE_INT);
        p.word(size as u32);
        p.pad();
    })
}

/// Raw video in `format`, at any size and rate.
fn format_param(format: u32) -> Vec<u64> {
    object(OBJECT_FORMAT, PARAM_ENUM_FORMAT, |p| {
        p.prop(FORMAT_MEDIA_TYPE);
        p.id(MEDIA_VIDEO);
        p.prop(FORMAT_MEDIA_SUBTYPE);
        p.id(SUBTYPE_RAW);
        p.prop(VIDEO_FORMAT);
        p.id(format);
        p.prop(VIDEO_SIZE);
        p.choice(CHOICE_RANGE, TYPE_RECTANGLE, 8, &[1920, 1080, 1, 1, 16384, 16384]);
        p.prop(VIDEO_FRAMERATE);
        p.choice(CHOICE_RANGE, TYPE_FRACTION, 8, &[0, 1, 0, 1, 1000, 1]);
    })
}

/// The format a stream settled on: one of the four read here, and its size.
fn parse_format(pod: &[u8]) -> Option<(u32, usize, usize)> {
    if u32_at(pod, 4) != TYPE_OBJECT {
        return None;
    }
    let end = (8 + u32_at(pod, 0) as usize).min(pod.len());
    let (mut format, mut size) = (None, None);
    let mut at = 16;
    while at + 16 <= end {
        let (key, len, kind) = (u32_at(pod, at), u32_at(pod, at + 8) as usize, u32_at(pod, at + 12));
        let body = at + 16;
        if body + len > end {
            break;
        }
        // A value, or the first (the chosen one) of a choice of them.
        let value = |want: u32| match kind {
            k if k == want => Some(body),
            TYPE_CHOICE if len >= 16 && u32_at(pod, body + 12) == want => Some(body + 16).filter(|v| v + u32_at(pod, body + 8) as usize <= body + len),
            _ => None,
        };
        match key {
            VIDEO_FORMAT => format = value(TYPE_ID).map(|v| u32_at(pod, v)),
            VIDEO_SIZE => size = value(TYPE_RECTANGLE).map(|v| (u32_at(pod, v) as usize, u32_at(pod, v + 4) as usize)),
            _ => {}
        }
        at = body + len.next_multiple_of(8);
    }
    let format = format.filter(|f| matches!(*f, RGBX | BGRX | RGBA | BGRA))?;
    let (w, h) = size.filter(|&(w, h)| w > 0 && h > 0)?;
    Some((format, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(words: &[u64]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_ne_bytes()).collect()
    }

    #[test]
    fn a_format_offer_reads_back() {
        let pod = bytes(&format_param(BGRA));
        assert_eq!(u32_at(&pod, 0) as usize + 8, pod.len());
        // The size offered is a range whose first value is its default.
        assert_eq!(parse_format(&pod), Some((BGRA, 1920, 1080)));
        // A settled format holds plain values.
        let settled = object(OBJECT_FORMAT, PARAM_FORMAT, |p| {
            p.prop(FORMAT_MEDIA_TYPE);
            p.id(MEDIA_VIDEO);
            p.prop(VIDEO_FORMAT);
            p.id(RGBX);
            p.prop(VIDEO_SIZE);
            p.word(8);
            p.word(TYPE_RECTANGLE);
            p.word(1280);
            p.word(800);
        });
        assert_eq!(parse_format(&bytes(&settled)), Some((RGBX, 1280, 800)));
        let other = object(OBJECT_FORMAT, PARAM_FORMAT, |p| {
            p.prop(VIDEO_FORMAT);
            p.id(2);
        });
        assert_eq!(parse_format(&bytes(&other)), None, "I420 is not read here");
    }

    fn cursor_meta(id: u32, at: (i32, i32), hot: (i32, i32), bitmap: Option<(u32, usize, usize, &[u8])>) -> Vec<u8> {
        let mut m = Vec::new();
        for v in [id, 0, at.0 as u32, at.1 as u32, hot.0 as u32, hot.1 as u32] {
            m.extend_from_slice(&v.to_ne_bytes());
        }
        match bitmap {
            None => m.extend_from_slice(&0u32.to_ne_bytes()),
            Some((format, w, h, px)) => {
                m.extend_from_slice(&(CURSOR_LEN as u32).to_ne_bytes());
                for v in [format, w as u32, h as u32, (w * 4) as u32, BITMAP_LEN as u32] {
                    m.extend_from_slice(&v.to_ne_bytes());
                }
                m.extend_from_slice(px);
            }
        }
        m
    }

    #[test]
    fn the_pointer_comes_from_cursor_metadata() {
        // A 2x1 picture, premultiplied: opaque red, then half-transparent white.
        let px = [255, 0, 0, 255, 128, 128, 128, 128];
        let u = pointer_update(&cursor_meta(1, (10, 20), (1, 0), Some((RGBA, 2, 1, &px)))).unwrap();
        assert_eq!(u.at, Some((10, 20)));
        let s = u.shape.expect("a new picture");
        assert_eq!((s.w, s.h, s.hot), (2, 1, (1, 0)));
        assert_eq!(s.rgba, vec![255, 0, 0, 255, 255, 255, 255, 128]);
        // A move alone tells no picture.
        let u = pointer_update(&cursor_meta(1, (11, 20), (0, 0), None)).unwrap();
        assert!(u.shape.is_none() && u.at == Some((11, 20)));
        // Hidden, or off this stream.
        assert_eq!(pointer_update(&cursor_meta(0, (0, 0), (0, 0), None)).unwrap().at, None);
        // A picture with no pixels: the pointer is invisible.
        let mut empty = cursor_meta(1, (5, 5), (0, 0), Some((RGBA, 0, 0, &[])));
        empty[CURSOR_LEN + 16..CURSOR_LEN + 20].copy_from_slice(&0u32.to_ne_bytes());
        assert_eq!(pointer_update(&empty).unwrap().shape.map(|s| s.w), Some(0));
        // A picture larger than the metadata holds is not read past its end.
        let cut = cursor_meta(1, (5, 5), (0, 0), Some((BGRA, 4, 4, &[0; 8])));
        assert_eq!(pointer_update(&cut).unwrap().shape.map(|s| s.w), Some(0));
    }

    #[test]
    fn a_pointer_is_drawn_over_its_point() {
        let shape = Shape { w: 2, h: 2, hot: (1, 1), rgba: vec![255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 128] };
        let mut px = vec![0u8; 4 * 3 * 4];
        draw(&mut px, 4, 3, 16, &shape, (2, 1));
        // Its top-left pixel lands one up and one left of the point; BGRx order.
        assert_eq!(&px[4..8], &[0, 0, 255, 0]);
        assert_eq!(&px[16 + 4..16 + 8], &[255, 0, 0, 0]);
        assert_eq!(&px[16 + 8..16 + 12], &[128, 128, 128, 0]);
        assert!(px[8..16].iter().all(|&v| v == 0), "a clear pixel leaves the screen as it was");
        // Partly off the frame: what is on it is drawn.
        draw(&mut px, 4, 3, 16, &shape, (0, 0));
        assert_eq!(&px[..4], &[128, 128, 128, 0]);
    }
}
