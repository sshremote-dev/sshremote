//! Choosing what to share with GNOME's own screen-sharing dialog (the xdg desktop
//! portal that video calls use), on the remote screen, without any helper. The
//! portal never reveals the list of windows; it hands over a stream of whatever
//! the person picked in its dialog, and with remote interaction allowed, input to it.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, RawFd};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedObjectPath, OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const RD: &str = "org.freedesktop.portal.RemoteDesktop";
const SC: &str = "org.freedesktop.portal.ScreenCast";

/// A stream the person chose in the portal dialog, with the session that sends input to it.
/// One of the screens a portal shares: its PipeWire node, and where it is and how large on the
/// desktop (as far as the portal says).
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    pub node: u32,
    pub at: (i32, i32),
    pub size: (i32, i32),
}

pub struct Portal {
    conn: Connection,
    session: OwnedObjectPath,
    /// The stream shown, which pointer positions are given in.
    pub node: u32,
    /// Every screen shared (a desktop with several monitors may share them all at once),
    /// left to right.
    pub screens: Vec<Screen>,
    /// A whole monitor was chosen, rather than a window.
    pub monitor: bool,
    fd: OwnedFd,
    /// The RemoteDesktop interface, made once: input events go through it many times a second;
    /// None where the desktop has the screen-casting portal alone (input cannot be given).
    rd: Option<Proxy<'static>>,
    /// What lets the next session start without the dialog, where the desktop gives it.
    pub restore: Option<String>,
    /// The pointer comes beside the frames, as their metadata, rather than in them.
    pub apart: bool,
}

fn err(what: &str) -> impl Fn(zbus::Error) -> String + '_ {
    move |e| format!("screen-sharing portal {what}: {e}")
}

/// Calls a portal method that answers through a Request object, and waits for the answer.
fn request<B>(
    conn: &Connection,
    iface: &str,
    method: &str,
    token: &str,
    body: &B,
) -> Result<HashMap<String, OwnedValue>, String>
where
    B: zbus::export::serde::ser::Serialize + zbus::zvariant::DynamicType,
{
    let sender = conn.unique_name().ok_or("no bus name")?.trim_start_matches(':').replace('.', "_");
    let path = format!("{PATH}/request/{sender}/{token}");
    let req = Proxy::new(conn, DEST, path.as_str(), "org.freedesktop.portal.Request").map_err(err(method))?;
    let mut answers = req.receive_signal("Response").map_err(err(method))?;
    let p = Proxy::new(conn, DEST, PATH, iface).map_err(err(method))?;
    let _: OwnedObjectPath = p.call(method, body).map_err(err(method))?;
    let msg = answers.next().ok_or("the screen-sharing portal went away")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().map_err(err(method))?;
    match code {
        0 => Ok(results),
        1 => Err("the choice was cancelled on the SSH host's screen".into()),
        _ => Err("the SSH host's screen did not allow sharing".into()),
    }
}

fn options(token: &str) -> HashMap<&'static str, Value<'_>> {
    HashMap::from([("handle_token", Value::from(token))])
}

/// Shows the dialog on the remote screen and waits until someone picks a window or a screen there.
pub fn pick() -> Result<Portal, String> {
    start(1 | 2, None, true, super::pw::usable())
}

/// The whole of one monitor, with keyboard and pointer: the way in to a Wayland desktop other
/// than GNOME (KDE Plasma, COSMIC, Hyprland, Sway...). `restore`, from an earlier session, lets
/// it start without the dialog where the desktop allows that; a desktop with no remote-desktop
/// portal (wlroots ones) shares the screen alone.
pub fn screen(restore: Option<&str>) -> Result<Portal, String> {
    // The pointer apart where reading the stream that way was tried (KDE Plasma): a failure
    // there would need the dialog again to go back.
    let kde = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|d| d.eq_ignore_ascii_case("KDE")));
    let apart = kde && super::pw::usable();
    match start(1, restore, true, apart) {
        Err(e) if e.contains("RemoteDesktop") || e.contains("UnknownMethod") || e.contains("ServiceUnknown") || e.contains("not provided") => {
            start(1, restore, false, apart).map_err(|e2| format!("{e2} (and with input: {e})"))
        }
        r => r,
    }
}

/// A portal session sharing `types` (1 a monitor, 2 a window), with input unless `screen_only`,
/// and the pointer as metadata when `apart` and the desktop sends it so.
fn start(types: u32, restore: Option<&str>, input: bool, apart: bool) -> Result<Portal, String> {
    let conn = Connection::session().map_err(err("connection"))?;
    let tag = std::process::id();
    let t = |n: u32| format!("sshremote{tag}_{n}{}", if input { "" } else { "v" });
    let iface = if input { RD } else { SC };

    let (t1, s1) = (t(1), format!("sshremotesession{tag}{}", if input { "" } else { "v" }));
    let mut o = options(&t1);
    o.insert("session_handle_token", Value::from(s1.as_str()));
    let r = request(&conn, iface, "CreateSession", &t1, &(o,))?;
    let session: String =
        r.get("session_handle").and_then(|v| String::try_from(v.clone()).ok()).ok_or("no portal session")?;
    let session = OwnedObjectPath::try_from(session).map_err(|e| e.to_string())?;
    let sp = ObjectPath::from(&session);
    // Kept until revoked (2), so later sessions need no one at the screen; where the desktop
    // keeps it at all.
    let keep = |o: &mut HashMap<&'static str, Value<'_>>| {
        if types == 1 {
            o.insert("persist_mode", Value::U32(2));
            if let Some(r) = restore {
                o.insert("restore_token", Value::from(r.to_string()));
            }
        }
    };

    if input {
        let t2 = t(2);
        let mut o = options(&t2);
        // Keyboard and pointer.
        o.insert("types", Value::U32(3));
        keep(&mut o);
        request(&conn, RD, "SelectDevices", &t2, &(&sp, o))?;
    }

    let t3 = t(3);
    let mut o = options(&t3);
    // What to share, one of it, with the pointer drawn into the picture (2) or beside it (4).
    let apart = apart
        && Proxy::new(&conn, DEST, PATH, SC).ok().and_then(|p| p.get_property::<u32>("AvailableCursorModes").ok()).is_some_and(|m| m & 4 != 0);
    o.insert("types", Value::U32(types));
    // Every monitor, where the desktop offers them together: the viewer picks one as it would
    // elsewhere. A window alone.
    o.insert("multiple", Value::Bool(types == 1));
    o.insert("cursor_mode", Value::U32(if apart { 4 } else { 2 }));
    if !input {
        keep(&mut o);
    }
    request(&conn, SC, "SelectSources", &t3, &(&sp, o))?;

    let t4 = t(4);
    let r = request(&conn, iface, "Start", &t4, &(&sp, "", options(&t4)))?;
    let restore = r.get("restore_token").and_then(|v| String::try_from(v.clone()).ok());
    let streams: Vec<(u32, HashMap<String, OwnedValue>)> = r
        .get("streams")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| v.try_into().ok())
        .ok_or("the portal sent no stream")?;
    let (node, props) = streams.first().ok_or("the portal sent no stream")?;
    let node = *node;
    // source_type 1 is a monitor, 2 a window.
    let monitor = props.get("source_type").and_then(|v| u32::try_from(v).ok()) == Some(1);
    let pair = |props: &HashMap<String, OwnedValue>, k: &str| props.get(k).and_then(|v| <(i32, i32)>::try_from(v.clone()).ok()).unwrap_or((0, 0));
    let mut screens: Vec<Screen> = streams.iter().map(|(node, p)| Screen { node: *node, at: pair(p, "position"), size: pair(p, "size") }).collect();
    screens.sort_by_key(|s| (s.at.0, s.at.1));

    let p = Proxy::new(&conn, DEST, PATH, SC).map_err(err("ScreenCast"))?;
    let fd: OwnedFd =
        p.call("OpenPipeWireRemote", &(&sp, HashMap::<&str, Value>::new())).map_err(err("OpenPipeWireRemote"))?;
    let rd = if input {
        Some(
            zbus::blocking::proxy::Builder::new(&conn)
                .destination(DEST)
                .and_then(|b| b.path(PATH))
                .and_then(|b| b.interface(RD))
                .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No))
                .and_then(|b| b.build())
                .map_err(err("RemoteDesktop"))?,
        )
    } else {
        None
    };
    // The one at the left first, as monitors are listed.
    let node = screens.first().map_or(node, |s| s.node);
    Ok(Portal { conn, session, node, screens, monitor, fd, rd, restore, apart })
}

impl Portal {
    /// Whether input can be given (the desktop has the remote-desktop portal).
    pub fn controls(&self) -> bool {
        self.rd.is_some()
    }

    /// Whether the portal session is still open; it closes when the shared window does.
    pub fn alive(&self) -> bool {
        Proxy::new(&self.conn, DEST, self.session.as_str(), "org.freedesktop.portal.Session")
            .and_then(|p| p.get_property::<u32>("version"))
            .is_ok()
    }

    /// What GStreamer's pipewiresrc needs to read the stream shown.
    pub fn source(&self) -> String {
        self.source_of(self.node)
    }

    /// The same for stream `node`.
    pub fn source_of(&self, node: u32) -> String {
        format!("fd={} path={node}", self.fd.as_raw_fd())
    }

    /// The PipeWire connection the stream is read on.
    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    fn notify<B>(&self, method: &str, body: &B) -> Result<(), String>
    where
        B: zbus::export::serde::ser::Serialize + zbus::zvariant::DynamicType,
    {
        // Each waits for its reply: the portal service handles calls on a pool of threads, so
        // calls sent ahead of their replies reach the desktop out of order (keys swapped or lost).
        let rd = self.rd.as_ref().ok_or("this desktop lets its screen be seen, not controlled (it has no remote-desktop portal)")?;
        rd.call_method(method, body).map(drop).map_err(err(method))
    }

    fn none() -> HashMap<&'static str, Value<'static>> {
        HashMap::new()
    }

    /// Pointer position in the shared stream's pixels.
    pub fn motion(&self, x: f64, y: f64) -> Result<(), String> {
        self.notify("NotifyPointerMotionAbsolute", &(ObjectPath::from(&self.session), Self::none(), self.node, x, y))
    }

    pub fn button(&self, code: i32, pressed: bool) -> Result<(), String> {
        self.notify("NotifyPointerButton", &(ObjectPath::from(&self.session), Self::none(), code, pressed as u32))
    }

    pub fn axis(&self, axis: u32, steps: i32) -> Result<(), String> {
        self.notify("NotifyPointerAxisDiscrete", &(ObjectPath::from(&self.session), Self::none(), axis, steps))
    }

    pub fn keysym(&self, keysym: u32, down: bool) -> Result<(), String> {
        self.notify(
            "NotifyKeyboardKeysym",
            &(ObjectPath::from(&self.session), Self::none(), keysym as i32, down as u32),
        )
    }

    /// The viewer's input, on a stream of `desk` pixels.
    pub fn input(&self, ev: super::Input, desk: (usize, usize)) -> Result<(), String> {
        use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};
        use super::Input;
        match ev {
            Input::Move { x, y } => self.motion(x.clamp(0.0, desk.0 as f64 - 1.0), y.clamp(0.0, desk.1 as f64 - 1.0)),
            Input::Button { button, pressed } => {
                let code = match button {
                    1 => 0x110,
                    2 => 0x112,
                    3 => 0x111,
                    8 => 0x113,
                    9 => 0x114,
                    _ => return Ok(()),
                };
                self.button(code, pressed)
            }
            Input::Scroll { steps } => self.axis(0, steps),
            Input::HScroll { steps } => self.axis(1, steps),
            Input::KeyState { keysym, down } => self.keysym(keysym, down),
            Input::SecureAttention => Ok(()),
            Input::MoveBy { dx, dy } => self.notify("NotifyPointerMotion", &(ObjectPath::from(&self.session), Self::none(), dx as f64, dy as f64)),
            Input::Key { keysym, mods } => {
                let held: Vec<u32> = [(MOD_CTRL, 29), (MOD_SHIFT, 42), (MOD_ALT, 56), (MOD_SUPER, 125)]
                    .into_iter()
                    .filter(|(m, _)| mods & m != 0)
                    .map(|(_, c)| c)
                    .collect();
                for &c in &held {
                    self.keycode(c, true)?;
                }
                let r = self.keysym(keysym, true).and_then(|_| self.keysym(keysym, false));
                for &c in held.iter().rev() {
                    self.keycode(c, false)?;
                }
                r
            }
        }
    }

    pub fn keycode(&self, code: u32, down: bool) -> Result<(), String> {
        self.notify("NotifyKeyboardKeycode", &(ObjectPath::from(&self.session), Self::none(), code as i32, down as u32))
    }
}

impl Drop for Portal {
    fn drop(&mut self) {
        // Ends the sharing, and GNOME's "screen is shared" indicator with it.
        if let Ok(p) = Proxy::new(&self.conn, DEST, self.session.as_str(), "org.freedesktop.portal.Session") {
            let _ = p.call_noreply("Close", &());
        }
    }
}
