//! On GNOME under Wayland, where no program may place another's window: the sshremote helper
//! extension, inside GNOME Shell, holds the program's own window over its pane. It catches the
//! window as it is made (unseen from its first frame), puts it over the pane of this window, moves
//! it with this window, minimizes it while the pane is not shown, and lets it go when the pane
//! closes or this program ends. This side tells it where the pane is, in this window's logical
//! pixels.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, FontId, Rect};
use zbus::blocking::{Connection, Proxy};

const DEST: &str = "io.github.thinkbigx.SshRemote.Windows";
const PATH: &str = "/io/github/thinkbigx/SshRemote/Windows";

fn proxy() -> Option<Proxy<'static>> {
    static CONN: std::sync::OnceLock<Option<Connection>> = std::sync::OnceLock::new();
    let conn = CONN
        .get_or_init(|| zbus::blocking::connection::Builder::session().ok()?.method_timeout(Duration::from_millis(500)).build().ok())
        .as_ref()?;
    Proxy::new(conn, DEST, PATH, DEST).ok()
}

fn call<B, R>(method: &str, body: &B) -> Option<R>
where
    B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    proxy()?.call(method, body).ok()
}

/// A GNOME session on Wayland whose helper holds windows (one from before this version is brought
/// up to date, which GNOME loads at the next login).
pub(in crate::gui) fn here() -> bool {
    static HERE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HERE.get_or_init(|| {
        // The helper answers only inside GNOME Shell.
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return false;
        }
        let Some(p) = proxy() else { return false };
        let introspect = zbus::blocking::fdo::IntrospectableProxy::builder(p.connection()).destination(DEST).and_then(|b| b.path(PATH)).and_then(|b| b.build());
        let Some(xml) = introspect.ok().and_then(|i| i.introspect().ok()) else { return false };
        let holds = xml.contains("\"Catch\"");
        if !holds {
            crate::capture::gnome_helper_update();
        }
        holds
    })
}

const LOOK: Duration = Duration::from_millis(150);
const POLL: Duration = Duration::from_millis(250);
const GIVE_UP: Duration = Duration::from_secs(20);

pub(in crate::gui) struct Held {
    /// The window looked out for (`Catch`), until one is held.
    token: Option<u32>,
    id: u64,
    /// Opened for the pane (closed with it), rather than one already open.
    new: bool,
    what: String,
    name: String,
    since: Instant,
    looked: Instant,
    said: Option<String>,
    ended: bool,
    /// What the helper was last told, and when it was last asked how the window is.
    told: Option<([i32; 6], bool)>,
    polled: Instant,
}

impl Held {
    pub(in crate::gui) fn start(kind: u8, text: &str) -> Held {
        let token: Option<u32> = call("Catch", &(std::process::id(),));
        let (name, mut said) = match crate::apps::launch(kind, text) {
            Ok(n) => (n, None),
            Err(e) => (String::new(), Some(format!("It could not be started: {e}."))),
        };
        if token.is_none() && said.is_none() {
            said = Some("GNOME's sshremote helper does not answer, so this program stays a window of its own.".into());
        }
        let what = if name.is_empty() { text.trim().trim_end_matches('/').rsplit('/').next().unwrap_or(text).to_string() } else { name.clone() };
        let now = Instant::now();
        Held { token, id: 0, new: false, what, name, since: now, looked: now, said, ended: false, told: None, polled: now - POLL }
    }

    pub(in crate::gui) fn ended(&self) -> bool {
        self.ended
    }

    /// Its window: the one the helper caught, or (after a few seconds) one of the program started,
    /// already open (a program that opens one window only and shows that again).
    fn find(&mut self) {
        if let Some(t) = self.token {
            let id: u64 = call("Caught", &(t,)).unwrap_or(0);
            if id != 0 {
                (self.id, self.new) = (id, true);
                return;
            }
        }
        let first = self.name.to_lowercase().split_whitespace().next().unwrap_or("").to_string();
        if self.since.elapsed() < Duration::from_secs(3) || first.is_empty() {
            return;
        }
        type Row = (u64, String, String, i32, i32, i32, i32, bool, bool, bool, u64);
        let list: Vec<Row> = call("List", &()).unwrap_or_default();
        if let Some(w) = list.iter().find(|w| w.8 && w.2.to_lowercase().contains(&first) && !w.2.to_lowercase().contains("ssh remote")) {
            self.id = w.0;
        }
    }

    pub(in crate::gui) fn show(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        if self.id == 0 && self.said.is_none() && self.looked.elapsed() >= LOOK {
            self.looked = Instant::now();
            self.find();
            if self.id == 0 && self.since.elapsed() > GIVE_UP {
                self.said = Some(format!("{} opened no window of its own to show here.", self.what));
            }
        }
        let text = match (self.id, &self.said) {
            (0, Some(s)) => s.clone(),
            (0, None) => format!("Starting {}\u{2026}", self.what),
            _ => format!("{} is here", self.what),
        };
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), ui.visuals().weak_text_color());
        ctx.request_repaint_after(POLL);
        if self.id == 0 {
            return;
        }
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        let shown = !minimized && !super::covered(&ctx, rect);
        self.tell(&ctx, rect, shown);
        // Held now: its wait is over.
        if let Some(t) = self.token.take() {
            let _: Option<()> = call("Forget", &(t,));
        }
    }

    /// Where the pane is in this window, in logical pixels, and whether the window shows: told when
    /// it changes, and asked again every so often (whether the window is still there).
    fn tell(&mut self, ctx: &egui::Context, rect: Rect, shown: bool) {
        let z = ctx.zoom_factor();
        let px = |v: f32| (v * z).round() as i32;
        let whole = ctx.content_rect();
        let at = [px(whole.width()), px(whole.height()), px(rect.min.x - whole.min.x), px(rect.min.y - whole.min.y), px(rect.width()), px(rect.height())];
        if self.told == Some((at, shown)) && self.polled.elapsed() < POLL {
            return;
        }
        self.told = Some((at, shown));
        self.polled = Instant::now();
        let state: Option<String> = call("Hold", &(self.id, std::process::id(), at[0], at[1], at[2], at[3], at[4], at[5], shown));
        if state.as_deref() == Some("gone") {
            self.id = 0;
            self.ended = true;
        }
    }

    pub(in crate::gui) fn hide(&mut self) {
        if self.id == 0 {
            return;
        }
        if let Some((at, true)) = self.told {
            self.told = Some((at, false));
            let _: Option<String> = call("Hold", &(self.id, std::process::id(), at[0], at[1], at[2], at[3], at[4], at[5], false));
        }
    }

    /// The pane closes: its program with it (it may ask to save first), or a window that was
    /// already open put back where it was.
    pub(in crate::gui) fn close(&mut self) {
        if let Some(t) = self.token.take() {
            let caught: u64 = call("Caught", &(t,)).unwrap_or(0);
            if caught != 0 && self.id == 0 {
                (self.id, self.new) = (caught, true);
            }
            let _: Option<()> = call("Forget", &(t,));
        }
        if self.id != 0 {
            let _: Option<()> = call("Release", &(self.id, self.new));
            self.id = 0;
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.close();
    }
}
