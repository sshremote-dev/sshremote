//! Files dropped on the windows on Linux. Under Wayland winit (0.30) takes no drop at all: a data
//! device of this program's own, on the windows' own connection, hears the drag (over which
//! window, and where) and the drop (the files' paths), which go into egui's input as winit's
//! would. Under X11 winit takes the drop but not where it was let go: the pointer is asked then.
//!
//! GNOME gives a drag to one data device of a program, the one first in its lists (the clipboard
//! has another, and gets the clipboard on every one): this one is made anew each time the
//! windows take or lose the keyboard, which puts it first.

use std::path::PathBuf;
use std::sync::Mutex;

use eframe::egui;
use egui::{Context, Pos2};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};

const URIS: &str = "text/uri-list";

/// What the listening thread found, for the windows to take.
#[derive(Default)]
struct Shared {
    /// The main window's surface; the window system the windows are on.
    main: Option<u32>,
    x11: bool,
    /// A drag over a window now: its surface, and where (in its own coordinates).
    hover: Option<(u32, (f64, f64))>,
    /// Drops not taken yet: on which surface, where, what.
    drops: Vec<(u32, (f64, f64), Vec<PathBuf>)>,
    /// The other window that last had the keyboard: drops on a window other than the main one go there.
    other: Option<egui::ViewportId>,
    ctx: Option<Context>,
    /// What makes the data devices: the connection, its queue, the manager, the seats, the devices
    /// now; and whether the main window had the keyboard last frame.
    conn: Option<(Connection, QueueHandle<State>)>,
    manager: Option<WlDataDeviceManager>,
    seats: Vec<WlSeat>,
    devices: Vec<WlDataDevice>,
    focused: Option<bool>,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    main: None,
    x11: false,
    hover: None,
    drops: Vec::new(),
    other: None,
    ctx: None,
    conn: None,
    manager: None,
    seats: Vec::new(),
    devices: Vec::new(),
    focused: None,
});

/// The data devices made anew (first in GNOME's lists again), the old ones let go.
fn renew(sh: &mut Shared) {
    let (Some((conn, qh)), Some(m)) = (sh.conn.clone(), sh.manager.clone()) else { return };
    for d in sh.devices.drain(..) {
        if d.version() >= 2 {
            d.release();
        }
    }
    sh.devices = sh.seats.iter().map(|seat| m.get_data_device(seat, &qh, ())).collect();
    let _ = conn.flush();
}

fn shared() -> std::sync::MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

fn repaint() {
    if let Some(c) = &shared().ctx {
        c.request_repaint();
    }
}

/// Starts listening, once, on the window system `frame`'s window is on.
pub(crate) fn start(frame: &eframe::Frame, ctx: &Context) {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let (Ok(d), Ok(w)) = (frame.display_handle(), frame.window_handle()) else { return };
        shared().ctx = Some(ctx.clone());
        match (d.as_raw(), w.as_raw()) {
            (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(w)) => {
                let (display, surface) = (d.display.as_ptr(), w.surface.as_ptr());
                if let Err(e) = listen(display, surface) {
                    eprintln!("drops: not heard under Wayland ({e})");
                }
            }
            (RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_), _) => shared().x11 = true,
            _ => {}
        }
    });
}

/// Listens on the windows' Wayland connection with an event queue of its own.
fn listen(display: *mut std::ffi::c_void, surface: *mut std::ffi::c_void) -> Result<(), String> {
    // SAFETY: winit's display lives as long as the program's windows, and so this connection.
    let backend = unsafe { wayland_backend::client::Backend::from_foreign_display(display.cast()) };
    let conn = Connection::from_backend(backend);
    // SAFETY: the main window's surface, alive while the program runs.
    let main = unsafe { wayland_backend::client::ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }.map_err(|e| e.to_string())?;
    shared().main = Some(main.protocol_id());
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    shared().conn = Some((conn.clone(), qh.clone()));
    conn.display().get_registry(&qh, ());
    let mut st = State { conn: conn.clone(), offer: None, at: (0.0, 0.0), surface: 0 };
    std::thread::Builder::new()
        .name("wayland-drops".into())
        .spawn(move || {
            while queue.blocking_dispatch(&mut st).is_ok() {}
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

struct State {
    conn: Connection,
    /// The drag's offer over a window now (and whether it holds files), where, and on which surface.
    offer: Option<(WlDataOffer, bool)>,
    at: (f64, f64),
    surface: u32,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(st: &mut State, registry: &WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<State>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            if std::env::var_os("SSHREMOTE_DND_DEBUG").is_some() && interface.contains("seat") | interface.contains("data_device") {
                eprintln!("dnd: global {interface} v{version}");
            }
            let _ = st;
            let mut sh = shared();
            match interface.as_str() {
                // Version 2 lets a device go (`release`).
                "wl_seat" => sh.seats.push(registry.bind::<WlSeat, _, _>(name, version.min(2), qh, ())),
                "wl_data_device_manager" => sh.manager = Some(registry.bind::<WlDataDeviceManager, _, _>(name, version.min(3), qh, ())),
                _ => return,
            }
            renew(&mut sh);
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(_: &mut State, _: &WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(_: &mut State, _: &WlDataDeviceManager, _: <WlDataDeviceManager as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
}

/// An offer's kinds, as they are announced.
type Kinds = Mutex<Vec<String>>;

impl Dispatch<WlDataOffer, Kinds> for State {
    fn event(_: &mut State, _: &WlDataOffer, event: wl_data_offer::Event, kinds: &Kinds, _: &Connection, _: &QueueHandle<State>) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            kinds.lock().unwrap_or_else(|e| e.into_inner()).push(mime_type);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(st: &mut State, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {
        if std::env::var_os("SSHREMOTE_DND_DEBUG").is_some() {
            eprintln!("dnd: {event:?}");
        }
        match event {
            wl_data_device::Event::Enter { serial, surface, x, y, id } => {
                if let Some((old, _)) = st.offer.take() {
                    old.destroy();
                }
                let Some(offer) = id else { return };
                let files = offer.data::<Kinds>().is_some_and(|k| k.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|m| m == URIS));
                if std::env::var_os("SSHREMOTE_DND_DEBUG").is_some() {
                    eprintln!("dnd: kinds {:?}, files {files}", offer.data::<Kinds>().map(|k| k.lock().unwrap_or_else(|e| e.into_inner()).clone()));
                }
                // Files are taken (copied), anything else is not.
                offer.accept(serial, files.then(|| URIS.to_string()));
                if offer.version() >= 3 {
                    let action = if files { DndAction::Copy } else { DndAction::None };
                    offer.set_actions(action, action);
                }
                (st.surface, st.at) = (surface.id().protocol_id(), (x, y));
                if files {
                    shared().hover = Some((st.surface, st.at));
                }
                st.offer = Some((offer, files));
                repaint();
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                st.at = (x, y);
                if st.offer.as_ref().is_some_and(|o| o.1) {
                    shared().hover = Some((st.surface, st.at));
                    repaint();
                }
            }
            wl_data_device::Event::Leave => {
                if let Some((o, _)) = st.offer.take() {
                    o.destroy();
                }
                shared().hover = None;
                repaint();
            }
            wl_data_device::Event::Drop => {
                shared().hover = None;
                let Some((offer, files)) = st.offer.take() else { return };
                if !files {
                    offer.destroy();
                    return;
                }
                let mut fds = [0; 2];
                if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
                    offer.destroy();
                    return;
                }
                use std::os::fd::{AsFd, FromRawFd, OwnedFd};
                let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
                offer.receive(URIS.to_string(), w.as_fd());
                let _ = st.conn.flush();
                drop(w);
                let (conn, surface, at) = (st.conn.clone(), st.surface, st.at);
                // The paths come from the dragging program as it writes them, apart from this queue.
                std::thread::spawn(move || {
                    use std::io::Read;
                    let mut text = String::new();
                    let _ = std::fs::File::from(r).take(1 << 20).read_to_string(&mut text);
                    if offer.version() >= 3 {
                        offer.finish();
                    }
                    offer.destroy();
                    let _ = conn.flush();
                    let paths = paths_of(&text);
                    if !paths.is_empty() {
                        shared().drops.push((surface, at, paths));
                    }
                    repaint();
                });
            }
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, Kinds::default())]);
}

/// The local paths of a text/uri-list: file: URIs, one a line, # lines left out.
fn paths_of(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        // file://host/path: the path from its first slash (the host here is this computer).
        .filter_map(|rest| rest.find('/').map(|at| &rest[at..]))
        .map(|p| {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(unescape(p)))
        })
        .collect()
}

/// %XX escapes as the bytes they stand for.
fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Gives this window (`root`: the main one) the drag over it and the files dropped on it, as
/// egui's input carries winit's; called at the start of each window's frame.
pub(crate) fn feed(ctx: &Context, root: bool) {
    let id = egui::Id::unique("wl-drop-point");
    let mut sh = shared();
    let Some(main) = sh.main else { return };
    let focused = ctx.input(|i| i.focused);
    if !root && focused {
        sh.other = Some(ctx.viewport_id());
    }
    // The keyboard came or went: this program's data devices moved in GNOME's lists.
    if root && sh.focused.replace(focused) != Some(focused) {
        renew(&mut sh);
    }
    let other = sh.other;
    let mine = |surface: u32| if root { surface == main } else { surface != main && other == Some(ctx.viewport_id()) };
    let zoom = ctx.zoom_factor().max(0.1);
    let at = |p: (f64, f64)| Pos2::new(p.0 as f32 / zoom, p.1 as f32 / zoom);
    let mut point = None;
    if let Some((s, p)) = sh.hover.filter(|(s, _)| mine(*s)) {
        let _ = s;
        point = Some(at(p));
        ctx.input_mut(|i| {
            if i.raw.hovered_files.is_empty() {
                i.raw.hovered_files.push(egui::HoveredFile::default());
            }
        });
    }
    let (taken, left): (Vec<_>, Vec<_>) = std::mem::take(&mut sh.drops).into_iter().partition(|(s, _, _)| mine(*s));
    sh.drops = left;
    drop(sh);
    for (_, p, paths) in taken {
        point = Some(at(p));
        ctx.input_mut(|i| i.raw.dropped_files.extend(paths.into_iter().map(|path| std::sync::Arc::new(Dropped(path)) as std::sync::Arc<dyn egui::DroppedFile + Send + Sync>)));
    }
    ctx.data_mut(|d| d.insert_temp(id, point));
}

/// A file dropped under Wayland, as egui carries winit's.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Where files are being dragged over or were just dropped on this window, in its points: as
/// Wayland says, or the pointer under X11; None where the window system does not say.
pub(crate) fn point(ctx: &Context) -> Option<Pos2> {
    if let Some(p) = ctx.data(|d| d.get_temp::<Option<Pos2>>(egui::Id::unique("wl-drop-point"))).flatten() {
        return Some(p);
    }
    if !shared().x11 {
        return None;
    }
    let (x, y) = x11_pointer()?;
    Some(super::workspace::window_point(ctx, Pos2::new(x as f32, y as f32)))
}

/// The pointer on the X screen, in pixels.
fn x11_pointer() -> Option<(i32, i32)> {
    use x11_dl::xlib::{Display, Xlib};
    struct X(Xlib, *mut Display);
    // SAFETY: used under the lock below only.
    unsafe impl Send for X {}
    static X11: Mutex<Option<Option<X>>> = Mutex::new(None);
    let mut x = X11.lock().unwrap_or_else(|e| e.into_inner());
    let x = x
        .get_or_insert_with(|| {
            let xlib = Xlib::open().ok()?;
            let d = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
            (!d.is_null()).then_some(X(xlib, d))
        })
        .as_ref()?;
    let (mut root, mut child) = (0, 0);
    let (mut rx, mut ry, mut wx, mut wy, mut mask) = (0, 0, 0, 0, 0);
    let ok = unsafe {
        let r = (x.0.XDefaultRootWindow)(x.1);
        (x.0.XQueryPointer)(x.1, r, &mut root, &mut child, &mut rx, &mut ry, &mut wx, &mut wy, &mut mask)
    };
    (ok != 0).then_some((rx, ry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_uri_list_is_read_as_paths() {
        let text = "# from the file manager\r\nfile:///home/a/My%20shot.png\r\nfile://host/tmp/%ED%95%9C.txt\r\nhttps://example.com/x\r\n";
        assert_eq!(paths_of(text), [PathBuf::from("/home/a/My shot.png"), PathBuf::from("/tmp/한.txt")]);
    }
}
