//! On KDE Plasma under Wayland: KWin scripts, loaded over D-Bus, hold the program's own window over
//! its pane, as GNOME's helper does there. One catches the next window a program opens (unseen
//! from its first frame) and names it back to this program; one per held window keeps it over the
//! pane of this window (moved with it, minimized while the pane is not shown, over it while it is
//! in use), loaded again whenever the pane changes. A script lets its window go when this window
//! goes, should this program end without saying so.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, FontId, Rect};
use zbus::blocking::{Connection, Proxy};

const KWIN: &str = "org.kde.KWin";
/// This program's name on the bus, where scripts say which window they caught.
const INBOX: &str = "io.github.thinkbigx.SshRemote.Held";

fn conn() -> Option<&'static Connection> {
    static CONN: std::sync::OnceLock<Option<Connection>> = std::sync::OnceLock::new();
    CONN.get_or_init(|| zbus::blocking::connection::Builder::session().ok()?.method_timeout(Duration::from_millis(800)).build().ok()).as_ref()
}

/// Windows caught by token, as the catching scripts call `Caught`.
static CAUGHT: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// This program's own name on the bus, answering the scripts' calls on a thread of its own.
fn inbox() -> Option<String> {
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let conn = Connection::session().ok()?;
        let name = format!("{INBOX}.p{}", std::process::id());
        conn.request_name(name.as_str()).ok()?;
        std::thread::Builder::new()
            .name("held-inbox".into())
            .spawn(move || {
                for msg in zbus::blocking::MessageIterator::from(&conn).flatten() {
                    if msg.message_type() != zbus::message::Type::MethodCall {
                        continue;
                    }
                    let header = msg.header();
                    if header.member().is_some_and(|m| m.as_str() == "Caught")
                        && let Ok((token, id)) = msg.body().deserialize::<(String, String)>()
                    {
                        CAUGHT.lock().unwrap_or_else(|e| e.into_inner()).push((token, id));
                    }
                    let _ = conn.reply(&header, &());
                }
            })
            .ok()?;
        Some(name)
    })
    .clone()
}

fn scripting() -> Option<Proxy<'static>> {
    Proxy::new(conn()?, KWIN, "/Scripting", "org.kde.kwin.Scripting").ok()
}

/// A KDE session on Wayland whose KWin runs scripts.
pub(in crate::gui) fn here() -> bool {
    static HERE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HERE.get_or_init(|| std::env::var_os("WAYLAND_DISPLAY").is_some() && scripting().is_some_and(|p| p.call::<_, _, bool>("isScriptLoaded", &("sshremote-none",)).is_ok()))
}

/// Runs `body` in KWin as script `name` (one of that name loaded before is unloaded first).
fn run(name: &str, body: &str) -> bool {
    let Some(p) = scripting() else { return false };
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let file = dir.join(format!("{name}.js"));
    if std::fs::write(&file, body).is_err() {
        return false;
    }
    let _: Result<bool, _> = p.call("unloadScript", &(name,));
    let loaded: Result<i32, _> = p.call("loadScript", &(file.display().to_string(), name));
    let started: Result<(), _> = p.call("start", &());
    let _ = std::fs::remove_file(&file);
    loaded.is_ok_and(|n| n >= 0) && started.is_ok()
}

fn unload(name: &str) {
    if let Some(p) = scripting() {
        let _: Result<bool, _> = p.call("unloadScript", &(name,));
    }
}

/// Whether KWin still has the window, and where its frame is (x, y, width, height).
fn info(id: &str) -> Option<[i32; 4]> {
    let p = Proxy::new(conn()?, KWIN, "/KWin", "org.kde.KWin").ok()?;
    let map: HashMap<String, zbus::zvariant::OwnedValue> = p.call("getWindowInfo", &(id,)).ok()?;
    let num = |k: &str| map.get(k).and_then(|v| f64::try_from(v).ok().or_else(|| i32::try_from(v).ok().map(f64::from))).map(|f| f.round() as i32);
    Some([num("x")?, num("y")?, num("width")?, num("height")?])
}

/// A string as JavaScript reads it.
fn js(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The script that catches the next window a program opens: unseen, and named back.
fn catcher(token: u32, inbox: &str) -> String {
    format!(
        r#"// sshremote: the next window a program opens, unseen until it is in its pane.
const before = new Set(workspace.windowList().map(w => w.internalId.toString()));
let done = false;
workspace.windowAdded.connect(w => {{
    if (done || before.has(w.internalId.toString()) || w.pid === {pid} || !w.normalWindow || w.transient)
        return;
    done = true;
    w.opacity = 0;
    callDBus({inbox}, "/held", {iface}, "Caught", "{token}", w.internalId.toString());
}});
"#,
        pid = std::process::id(),
        inbox = js(inbox),
        iface = js(INBOX),
    )
}

/// The script that keeps window `id` over the pane at `at` (this window's width and height, the
/// pane's x, y, width and height in it), or minimized while it is not `shown`.
fn holder(id: &str, at: [i32; 6], shown: bool) -> String {
    format!(
        r#"// sshremote: a program's window held over a pane of the sshremote app.
const id = {id}, pid = {pid}, size = {{w: {sw}, h: {sh}}}, rel = {{x: {x}, y: {y}, w: {w}, h: {h}}}, shown = {shown};
const held = () => workspace.windowList().find(w => w.internalId.toString() === id);
const ours = () => {{
    const near = (a, b) => Math.abs(a - b) <= 3;
    const mine = workspace.windowList().filter(w => w.pid === pid && w.normalWindow);
    return mine.find(w => near(w.clientGeometry.width, size.w) && near(w.clientGeometry.height, size.h)) || (mine.length === 1 ? mine[0] : null);
}};
const me = ours();
let gone = false;
function place() {{
    const h = held(), m = ours();
    if (gone || !h || !m)
        return;
    if (!shown || m.minimized) {{
        if (workspace.activeWindow === h)
            workspace.activeWindow = m;
        h.opacity = 0;
        h.minimized = true;
        return;
    }}
    h.minimized = false;
    if (h.fullScreen)
        h.fullScreen = false;
    h.setMaximize(false, false);
    const g = m.clientGeometry;
    const to = {{x: g.x + rel.x, y: g.y + rel.y, width: rel.w, height: rel.h}};
    const f = h.frameGeometry;
    if (f.x !== to.x || f.y !== to.y || f.width !== to.width || f.height !== to.height)
        h.frameGeometry = to;
    if (workspace.activeWindow === m)
        workspace.raiseWindow(h);
    // Seen once it is in place.
    const now = h.frameGeometry;
    if (h.opacity < 1 && Math.abs(now.x - to.x) <= 1 && Math.abs(now.y - to.y) <= 1)
        h.opacity = 1;
}}
place();
const h0 = held();
if (me) {{
    me.frameGeometryChanged.connect(place);
    me.minimizedChanged.connect(place);
}}
if (h0)
    h0.frameGeometryChanged.connect(place);
workspace.windowActivated.connect(w => {{
    const h = held();
    if (!gone && shown && h && !h.minimized && w === ours())
        workspace.raiseWindow(h);
}});
// The app ended without letting go: the window is its own again.
workspace.windowRemoved.connect(w => {{
    if (w !== me)
        return;
    gone = true;
    const h = held();
    if (h) {{
        h.opacity = 1;
        h.minimized = false;
    }}
}});
"#,
        id = js(id),
        pid = std::process::id(),
        sw = at[0],
        sh = at[1],
        x = at[2],
        y = at[3],
        w = at[4],
        h = at[5],
    )
}

/// The script that lets window `id` go: seen, shown, and closed (it may ask to save first) or put
/// back at `was`.
fn letgo(id: &str, close: bool, was: Option<[i32; 4]>) -> String {
    let back = match was {
        Some([x, y, w, h]) if !close => format!("h.frameGeometry = {{x: {x}, y: {y}, width: {w}, height: {h}}};"),
        _ => String::new(),
    };
    format!(
        r#"// sshremote: a held window let go.
const h = workspace.windowList().find(w => w.internalId.toString() === {id});
if (h) {{
    h.opacity = 1;
    h.minimized = false;
    {back}
    if ({close})
        h.closeWindow();
}}
"#,
        id = js(id),
    )
}

const LOOK: Duration = Duration::from_millis(150);
const POLL: Duration = Duration::from_millis(500);
const GIVE_UP: Duration = Duration::from_secs(20);

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

pub(in crate::gui) struct Held {
    token: u32,
    /// The catching script, until a window is caught.
    catching: Option<String>,
    /// The window, by KWin's id for it, and its script.
    id: Option<String>,
    script: String,
    new: bool,
    was: Option<[i32; 4]>,
    what: String,
    since: Instant,
    looked: Instant,
    said: Option<String>,
    ended: bool,
    told: Option<([i32; 6], bool)>,
    polled: Instant,
}

impl Held {
    pub(in crate::gui) fn start(kind: u8, text: &str) -> Held {
        let token = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let catching = inbox().map(|name| (format!("sshremote-catch-{pid}-{token}"), name)).filter(|(script, name)| run(script, &catcher(token, name))).map(|(s, _)| s);
        let (name, mut said) = match crate::apps::launch(kind, text) {
            Ok(n) => (n, None),
            Err(e) => (String::new(), Some(format!("It could not be started: {e}."))),
        };
        if catching.is_none() && said.is_none() {
            said = Some("KWin does not run this app's scripts, so this program stays a window of its own.".into());
        }
        let what = if name.is_empty() { text.trim().trim_end_matches('/').rsplit('/').next().unwrap_or(text).to_string() } else { name };
        let now = Instant::now();
        Held { token, catching, id: None, script: format!("sshremote-held-{pid}-{token}"), new: false, was: None, what, since: now, looked: now, said, ended: false, told: None, polled: now }
    }

    pub(in crate::gui) fn ended(&self) -> bool {
        self.ended
    }

    fn take_caught(&mut self) -> Option<String> {
        let mut caught = CAUGHT.lock().unwrap_or_else(|e| e.into_inner());
        let i = caught.iter().position(|(t, _)| t == &self.token.to_string())?;
        Some(caught.remove(i).1)
    }

    pub(in crate::gui) fn show(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        if self.id.is_none() && self.said.is_none() && self.looked.elapsed() >= LOOK {
            self.looked = Instant::now();
            if let Some(id) = self.take_caught() {
                if let Some(s) = self.catching.take() {
                    unload(&s);
                }
                self.id = Some(id);
                self.new = true;
            } else if self.since.elapsed() > GIVE_UP {
                if let Some(s) = self.catching.take() {
                    unload(&s);
                }
                self.said = Some(format!("{} opened no window of its own to show here.", self.what));
            }
        }
        let text = match (&self.id, &self.said) {
            (None, Some(s)) => s.clone(),
            (None, None) => format!("Starting {}\u{2026}", self.what),
            _ => format!("{} is here", self.what),
        };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), ui.visuals().weak_text_color());
        ctx.request_repaint_after(LOOK.max(Duration::from_millis(200)));
        let Some(id) = self.id.clone() else { return };
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let shown = !minimized && !super::covered(&ctx, rect);
        let z = ctx.zoom_factor();
        let px = |v: f32| (v * z).round() as i32;
        let whole = ctx.content_rect();
        let at = [px(whole.width()), px(whole.height()), px(rect.min.x - whole.min.x), px(rect.min.y - whole.min.y), px(rect.width()), px(rect.height())];
        if self.told != Some((at, shown)) {
            self.told = Some((at, shown));
            run(&self.script, &holder(&id, at, shown));
        }
        if self.polled.elapsed() >= POLL {
            self.polled = Instant::now();
            if info(&id).is_none() {
                unload(&self.script);
                self.id = None;
                self.ended = true;
            }
        }
    }

    pub(in crate::gui) fn hide(&mut self) {
        let Some(id) = self.id.clone() else { return };
        if let Some((at, true)) = self.told {
            self.told = Some((at, false));
            run(&self.script, &holder(&id, at, false));
        }
    }

    pub(in crate::gui) fn close(&mut self) {
        if let Some(s) = self.catching.take() {
            unload(&s);
            // Caught meanwhile: it was opened for the pane, so it closes with it.
            if let Some(id) = self.take_caught() {
                run(&format!("{}-go", self.script), &letgo(&id, true, None));
                unload(&format!("{}-go", self.script));
            }
        }
        let Some(id) = self.id.take() else { return };
        unload(&self.script);
        run(&format!("{}-go", self.script), &letgo(&id, self.new, self.was));
        unload(&format!("{}-go", self.script));
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.close();
    }
}
