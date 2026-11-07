//! Choosing what to share with GNOME's own screen-sharing dialog (the xdg desktop
//! portal that video calls use), on the remote screen, without any helper. The
//! portal never reveals the list of windows; it hands over a stream of whatever
//! the person picked in its dialog, and with remote interaction allowed, input to it.

use std::collections::HashMap;
use std::os::fd::AsRawFd;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedObjectPath, OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const RD: &str = "org.freedesktop.portal.RemoteDesktop";
const SC: &str = "org.freedesktop.portal.ScreenCast";

/// A stream the person chose in the portal dialog, with the session that sends input to it.
pub struct Portal {
    conn: Connection,
    session: OwnedObjectPath,
    pub node: u32,
    fd: OwnedFd,
    /// The RemoteDesktop interface, made once: input events go through it many times a second.
    rd: Proxy<'static>,
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
    let conn = Connection::session().map_err(err("connection"))?;
    let tag = std::process::id();
    let t = |n: u32| format!("sshremote{tag}_{n}");

    let (t1, s1) = (t(1), format!("sshremotesession{tag}"));
    let mut o = options(&t1);
    o.insert("session_handle_token", Value::from(s1.as_str()));
    let r = request(&conn, RD, "CreateSession", &t1, &(o,))?;
    let session: String =
        r.get("session_handle").and_then(|v| String::try_from(v.clone()).ok()).ok_or("no portal session")?;
    let session = OwnedObjectPath::try_from(session).map_err(|e| e.to_string())?;
    let sp = ObjectPath::from(&session);

    let t2 = t(2);
    let mut o = options(&t2);
    // Keyboard and pointer.
    o.insert("types", Value::U32(3));
    request(&conn, RD, "SelectDevices", &t2, &(&sp, o))?;

    let t3 = t(3);
    let mut o = options(&t3);
    // A window or a monitor, one of them, with the pointer drawn into the picture.
    o.insert("types", Value::U32(1 | 2));
    o.insert("multiple", Value::Bool(false));
    o.insert("cursor_mode", Value::U32(2));
    request(&conn, SC, "SelectSources", &t3, &(&sp, o))?;

    let t4 = t(4);
    let r = request(&conn, RD, "Start", &t4, &(&sp, "", options(&t4)))?;
    let streams: Vec<(u32, HashMap<String, OwnedValue>)> = r
        .get("streams")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| v.try_into().ok())
        .ok_or("the portal sent no stream")?;
    let node = streams.first().map(|s| s.0).ok_or("the portal sent no stream")?;

    let p = Proxy::new(&conn, DEST, PATH, SC).map_err(err("ScreenCast"))?;
    let fd: OwnedFd =
        p.call("OpenPipeWireRemote", &(&sp, HashMap::<&str, Value>::new())).map_err(err("OpenPipeWireRemote"))?;
    let rd = zbus::blocking::proxy::Builder::new(&conn)
        .destination(DEST)
        .and_then(|b| b.path(PATH))
        .and_then(|b| b.interface(RD))
        .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No))
        .and_then(|b| b.build())
        .map_err(err("RemoteDesktop"))?;
    Ok(Portal { conn, session, node, fd, rd })
}

impl Portal {
    /// Whether the portal session is still open; it closes when the shared window does.
    pub fn alive(&self) -> bool {
        Proxy::new(&self.conn, DEST, self.session.as_str(), "org.freedesktop.portal.Session")
            .and_then(|p| p.get_property::<u32>("version"))
            .is_ok()
    }

    /// What GStreamer's pipewiresrc needs to read the stream.
    pub fn source(&self) -> String {
        format!("fd={} path={}", self.fd.as_raw_fd(), self.node)
    }

    fn notify<B>(&self, method: &str, body: &B) -> Result<(), String>
    where
        B: zbus::export::serde::ser::Serialize + zbus::zvariant::DynamicType,
    {
        // Without waiting for the reply, as for Mutter's own interface.
        self.rd.call_noreply(method, body).map_err(err(method))
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
