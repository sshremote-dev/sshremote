//! A connected host: its screen, the toolbar, input, files, terminal and chat.

use super::*;
use crate::capture::Share;

/// Windows or programs offered for sharing, or why there are none.
type WindowList = Result<Vec<crate::capture::WindowInfo>, String>;

pub(super) type Deferred = Box<dyn FnOnce(&mut Remote, &egui::Context) -> Result<(), String>>;

/// A connection shown in a workspace pane: its screen, files or terminal. The workspace draws its
/// toolbar and owns full screen; the pane takes input only while it is the active one.
pub(super) struct Remote {
    pub(super) active: bool,
    /// Ctrl+Alt+Enter was pressed in it: the workspace turns full screen on or off.
    pub(super) full_toggle: bool,
    /// The pane is the whole full-screen window (zoomed, or the window's only pane in full
    /// screen): the picture fills it.
    pub(super) filling: bool,
    /// In a pane: it wants the system keys (pointer over its picture, "Send system keys" on);
    /// the app turns the grab on for whichever window and pane asks.
    pub(super) grab_wanted: bool,
    /// In a pane: its shell ended, and with it the pane.
    pub(super) pane_closed: bool,
    sess: Session,
    rx: mpsc::Receiver<Ev>,
    ctl: Control,
    fb: Framebuffer,
    tex: Option<TextureHandle>,
    label: String,
    /// Who is at the screen, for the chat: "user (address)".
    peer: crate::chat::Peer,
    fit: Fit,
    /// In full screen, fit to the window fills it all, stretched a little where the shapes
    /// differ (a 16:9 host on a 16:10 screen), rather than leave bars.
    fill_full: bool,
    /// How far a fixed-size picture is scrolled, in points.
    pan: Vec2,
    clip: Clip,
    share_clipboard: bool,
    /// Watching only: no key, click or wheel reaches the SSH host.
    view_only: bool,
    held: BTreeSet<u32>,
    mods: Modifiers,
    pointer: (u16, u16),
    view: (u16, u16),
    view_changed: Option<Instant>,
    win_next: bool,
    /// Notifications at the bottom right.
    toasts: super::toast::Toasts,
    grab_keys: bool,
    uploads: std::collections::VecDeque<Upload>,
    next_upload: u32,
    sound: Option<crate::audio::Player>,
    /// Open chat panel: messages (from the remote side?, text) and the line being typed.
    chat: Option<(Vec<crate::chat::Line>, String)>,
    /// Messages kept while the panel is closed, and how many are unread.
    chat_lines: Vec<crate::chat::Line>,
    unread: usize,
    /// How many of the messages are from earlier sessions with this host.
    chat_earlier: usize,
    /// The chat panel's width beside the picture.
    chat_w: f32,
    /// The terminal as a panel under the picture, its height, and whether it has the keyboard.
    term_split: bool,
    term_h: f32,
    term_focus: bool,
    paused: bool,
    /// The user paused the picture (rather than the pane showing something else).
    user_paused: bool,
    /// The picture was paused because the pane is not drawn (another tab, behind a zoomed pane).
    hidden_paused: bool,
    monitors: Vec<crate::proto::MonitorInfo>,
    monitor: usize,
    /// The monitor the user picked, if any, to pick again after a reconnect.
    chosen_monitor: Option<u8>,
    /// Wheel movement not yet a whole step at the host (trackpads send many small ones).
    wheel: Vec2,
    inbox: crate::files::Inbox,
    /// Downloads in progress: id, name and the request tag (0 for the Downloads folder).
    downloads: Vec<(u32, String, u32)>,
    /// The file manager, while the pane shows the files.
    files: Option<crate::filewin::FileWindow>,
    /// The terminal window, while open.
    terms: Option<crate::termwin::TermWindow>,
    /// The terminal takes the place of the remote screen in this pane.
    term_inline: bool,
    /// The terminal has a window of its own instead.
    term_detached: bool,
    /// The pane shows the file manager alone, with no screen.
    files_only: bool,
    ssh_cmd: Option<Vec<String>>,
    /// The "what to share" chooser while open: the window list once it arrived, and the tab shown.
    picker: Option<(Option<WindowList>, u8)>,
    /// What is shared now: kind as in `capture::Share` and a description.
    sharing: (u8, String),
    /// The next click on the picture chooses the window to share.
    click_pick: bool,
    /// Where the remote picture was drawn last, to tell when the pointer is over it.
    last_shown: Option<Rect>,
    /// That click has been pressed on the picture (the release of the click that opened the mode does not count).
    pick_pressed: bool,
    /// When to share the window that the choosing click brought to the front.
    pick_at: Option<Instant>,
    /// Local folder each tagged download request writes into.
    fetch_dirs: std::collections::HashMap<u32, std::path::PathBuf>,
    next_tag: u32,
    /// Uploads and downloads finished since the last notice about them.
    sent: Settled,
    got: Settled,
}

/// A file being sent to the remote host.
struct Upload {
    id: u32,
    name: String,
    /// The remote folder chosen in the file window; `None` for the Downloads folder.
    dir: Option<String>,
    cancelled: bool,
    /// Why the local file could not be read to the end.
    error: Option<String>,
    path: std::path::PathBuf,
    /// Open while it is the one being sent: they go one after another.
    file: Option<std::fs::File>,
    size: u64,
    sent: u64,
    acked: u64,
    ended: bool,
}

/// Files that arrived whole, told as one notice once the transfers settle: a folder of a
/// thousand files is one line, not a thousand.
#[derive(Default)]
struct Settled {
    count: usize,
    /// The last file's name, and the folder they went into.
    name: String,
    folder: String,
    at: Option<Instant>,
}

impl Settled {
    fn add(&mut self, name: String, folder: String) {
        self.count += 1;
        (self.name, self.folder, self.at) = (name, folder, Some(Instant::now()));
    }

    /// Done and quiet for a moment.
    fn due(&self) -> bool {
        self.at.is_some_and(|t| t.elapsed() > Duration::from_millis(800))
    }
}

/// Unacknowledged upload bytes; more would only queue in ssh and delay the screen.
const UPLOAD_WINDOW: u64 = 1 << 20;
const UPLOAD_CHUNK: usize = 64 << 10;

/// How long one frame may spend on network messages before the window is drawn.
const PUMP_BUDGET: Duration = Duration::from_millis(12);

/// What the user chose for a connection, carried over when it is made again after a drop: a
/// host set to view only stays view only.
pub(super) struct Prefs {
    view_only: bool,
    fit: Fit,
    fill_full: bool,
    share_clipboard: bool,
    grab_keys: bool,
    sound: bool,
    quality: Quality,
    chat: bool,
    chat_w: f32,
    term_h: f32,
    monitor: Option<u8>,
    paused: bool,
}

/// The picture quality chosen: adapting to the link, a preset, or settings of the user's own.
#[derive(Clone, Copy)]
enum Quality {
    Auto,
    Preset(usize),
    Custom(Mode, u8, u8),
}

impl Remote {
    pub(super) fn new(
        sess: Session,
        rx: mpsc::Receiver<Ev>,
        ctl: Control,
        label: String,
        ssh_cmd: Option<Vec<String>>,
    ) -> Self {
        // The conversation with this host so far, from earlier sessions.
        let chat_lines = crate::chat::load_history(label.split("  ").next().unwrap_or(&label));
        Remote {
            active: true,
            full_toggle: false,
            filling: false,
            grab_wanted: false,
            pane_closed: false,
            terms: None,
            term_inline: false,
            term_detached: false,
            files_only: false,
            picker: None,
            sharing: (0, String::new()),
            click_pick: false,
            last_shown: None,
            pick_pressed: false,
            pick_at: None,
            peer: crate::chat::Peer::new(label.split("  ").next().unwrap_or(&label)),
            label,
            sess,
            rx,
            ctl,
            fb: Framebuffer::default(),
            tex: None,
            fit: Fit::Window,
            fill_full: true,
            pan: Vec2::ZERO,
            clip: Clip::new(),
            share_clipboard: true,
            view_only: false,
            held: BTreeSet::new(),
            mods: Modifiers::NONE,
            pointer: (u16::MAX, u16::MAX),
            view: (0, 0),
            view_changed: None,
            win_next: false,
            toasts: Default::default(),
            grab_keys: true,
            uploads: std::collections::VecDeque::new(),
            next_upload: 1,
            sound: None,
            chat: None,
            chat_earlier: chat_lines.len(),
            chat_lines,
            unread: 0,
            chat_w: 340.0,
            term_split: false,
            term_h: 260.0,
            term_focus: false,
            paused: false,
            user_paused: false,
            hidden_paused: false,
            monitors: Vec::new(),
            monitor: 0,
            chosen_monitor: None,
            wheel: Vec2::ZERO,
            inbox: crate::files::Inbox::default(),
            downloads: Vec::new(),
            files: None,
            fetch_dirs: Default::default(),
            next_tag: 1,
            sent: Settled::default(),
            got: Settled::default(),
            ssh_cmd,
        }
    }

    /// One frame of the pane, with the system keys taken from Windows for it; on an error the
    /// session is closed.
    pub(super) fn show(&mut self, ui: &mut egui::Ui, grabbed: Vec<(u32, bool)>) -> Result<(), String> {
        let ctx = ui.ctx().clone();
        #[cfg(windows)]
        {
            // System keys go to the remote host while its screen is the active pane of the window
            // in front, as with RustDesk's focused window; the options menu turns this off.
            let shown = self.last_shown.is_some();
            let want = self.grab_keys && !self.view_only && ctx.input(|i| i.focused) && shown;
            self.grab_wanted = want && self.active && self.mode() == super::workspace::PaneMode::Screen;
        }
        // Alt, Ctrl or Shift pressed alone make no key event: they reach the host as they change
        // (Alt+Tab is Alt from here, then Tab from the hook), and are let go there likewise.
        let mods = ctx.input(|i| i.modifiers);
        let here = !self.view_only
            && self.active
            && self.mode() == super::workspace::PaneMode::Screen
            && ctx.input(|i| i.focused)
            && !ctx.memory(|m| m.focused().is_some())
            && !ctx.any_popup_open();
        let changed = (mods.alt, mods.ctrl, mods.shift) != (self.mods.alt, self.mods.ctrl, self.mods.shift);
        let synced = if here && changed { self.sync_mods(mods) } else { Ok(()) };
        // Kept with the other keys held there, so they are let go however the pane loses them.
        let result = synced
            .and_then(|_| grabbed.into_iter().try_for_each(|(keysym, down)| self.key(keysym, down)))
            .and_then(|_| self.frame(ui));
        if result.is_err() {
            self.sess.close();
            self.grab_wanted = false;
        }
        result
    }

    /// One notice for the files that arrived whole, once their transfers have settled.
    fn tell_settled(&mut self) {
        if self.uploads.is_empty() && self.sent.due() {
            let s = std::mem::take(&mut self.sent);
            let text = if s.count == 1 {
                format!("{} saved on the SSH host in {}", s.name, s.folder)
            } else {
                format!("{} files saved on the SSH host in {}", s.count, s.folder)
            };
            self.notify(&text);
        }
        if self.downloads.is_empty() && self.got.due() {
            let s = std::mem::take(&mut self.got);
            let text = if s.count == 1 {
                format!("Downloaded {} to {}", s.name, s.folder)
            } else {
                format!("{} files downloaded to {}", s.count, s.folder)
            };
            self.notify(&text);
        }
    }

    /// The file manager filling the pane, with the notices over it.
    fn files_view(&mut self, ui: &mut egui::Ui) -> Result<(), String> {
        let host = self.host().to_string();
        let transfers = self.transfers();
        if let Some(f) = self.files.as_mut() {
            f.active = self.active;
        }
        let cmds = egui::CentralPanel::default()
            .show(ui, |ui| self.files.as_mut().map(|f| f.embedded(ui, &host, &transfers)).unwrap_or_default())
            .inner;
        for c in cmds {
            self.file_cmd(c)?;
        }
        self.tell_settled();
        let ctx = ui.ctx().clone();
        self.toasts.draw(&ctx, ui.max_rect());
        ctx.request_repaint_after(Duration::from_millis(500));
        Ok(())
    }

    /// The host's name, the first part of the label ("host  1920x1080  backend").
    fn host(&self) -> &str {
        self.label.split("  ").next().unwrap_or(&self.label)
    }

    pub(super) fn send(&mut self, m: ToAgent) -> Result<(), String> {
        self.sess.send(m).map_err(|e| format!("connection lost: {e}"))
    }

    pub(super) fn notify(&mut self, text: &str) {
        self.toasts.push(text);
    }

    /// One UI pass: network, input, picture and toolbar. An error ends the session.
    pub(super) fn frame(&mut self, ui: &mut egui::Ui) -> Result<(), String> {
        let ctx = ui.ctx().clone();
        // Shown again: the picture comes back, whole.
        if std::mem::take(&mut self.hidden_paused) && !self.paused {
            self.send(ToAgent::Pause(false))?;
            self.send(ToAgent::Refresh)?;
        }
        self.pump(&ctx)?;
        // What is copied here goes to the host being worked in, as the pane becomes active if
        // it was copied meanwhile; the others do not get it.
        if self.share_clipboard
            && self.active
            && let Some(text) = self.clip.poll()
        {
            self.send(ToAgent::Clipboard(text))?;
        }
        self.ctl.tick(&mut self.sess).map_err(|e| e.to_string())?;
        if self.files_only {
            self.feed_uploads()?;
            return self.files_view(ui);
        }
        self.take_drops(&ctx)?;
        self.feed_uploads()?;
        let screen = ui.available_rect_before_wrap();
        let area = screen;
        let ppp = ctx.pixels_per_point();
        ui.painter().rect_filled(screen, CornerRadius::ZERO, Color32::from_rgb(18, 19, 23));
        // Around the picture (scaled down, or of another shape), the background layers show, as
        // behind the terminals; black when there are none.
        super::look::backdrop(ui, area, Color32::BLACK);
        let inline = self.term_inline && self.terms.is_some();
        // Side panels, as in an editor: the chat on the right, the terminal along the bottom; the
        // picture takes what is left, so both can be used while watching.
        let (rest, chat_area) = self.chat_split(ui, area);
        let split = !inline && !self.term_detached && self.term_split && self.terms.is_some();
        let (pic_area, term_area) = if split { self.term_split_at(ui, rest) } else { (rest, None) };
        // The keyboard goes to whichever of the picture and the terminal was clicked last.
        if let Some(p) = ctx.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten()) {
            if term_area.is_some_and(|r| r.contains(p)) {
                self.term_focus = true;
            } else if split && pic_area.contains(p) {
                self.term_focus = false;
            }
        }
        let shown = if inline {
            // The terminal fills the area beside the chat; the remote screen waits behind it.
            self.inline_terminal(ui, rest)?;
            None
        } else {
            self.resize(pic_area.size() * ppp)?;
            self.edge_pan(&ctx, pic_area);
            self.picture(ui, pic_area, ppp)
        };
        if let Some(r) = term_area {
            self.terminal_panel(ui, r)?;
            // Where the keys go, outlined.
            let focused = if self.term_focus { r } else { pic_area };
            ui.painter().rect_stroke(focused.shrink(0.5), CornerRadius::ZERO, egui::Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
        }
        self.last_shown = shown;
        let chat_rect = match chat_area {
            Some(r) => self.chat_panel(ui, r)?,
            None => None,
        };
        let chat_rect = chat_rect.or(term_area);
        self.upload_overlay(&ctx, screen);
        self.tell_settled();
        self.toasts.draw(&ctx, screen);
        self.term_window(&ctx)?;
        self.share_picker(&ctx)?;
        if self.pick_at.is_some_and(|t| Instant::now() >= t) {
            self.pick_at = None;
            self.send(ToAgent::Share(Share::ActiveWindow))?;
        }
        if self.click_pick {
            // A hint over the picture while the next click chooses the window.
            let text = format!("{}  Click the window to share   ·   Esc cancels", ic::CURSOR_CLICK);
            let painter =
                ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("click-pick")));
            let galley = painter.layout_no_wrap(text, FontId::proportional(15.0), Color32::WHITE);
            let r = Rect::from_center_size(
                Pos2::new(screen.center().x, screen.top() + 80.0),
                galley.size() + Vec2::new(32.0, 18.0),
            );
            painter.rect_filled(r, CornerRadius::same(crate::gui::theme::R_MD), ACCENT);
            painter.galley(r.min + Vec2::new(16.0, 9.0), galley, Color32::WHITE);
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let ui_rects: Vec<Rect> = chat_rect.into_iter().collect();
        if self.view_only || inline || !self.active {
            self.release_all()?;
        } else {
            self.input(&ctx, shown, &ui_rects)?;
        }
        // Full screen on or off (F11 is the host's here).
        if self.active && ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::ALT, Key::Enter)) {
            self.full_toggle = true;
        }
        // Network messages wake the window themselves; this only keeps statistics and pings going.
        ctx.request_repaint_after(Duration::from_secs(1));
        Ok(())
    }

    /// The connection kept going while its pane is not drawn: pictures, files and shells read,
    /// transfers fed, the link watched; keys held at the host let go.
    pub(super) fn background(&mut self, ctx: &egui::Context) -> Result<(), String> {
        // Nobody sees the picture: it stops, costing the link and the host nothing meanwhile.
        if !self.paused && !self.hidden_paused {
            self.hidden_paused = true;
            self.send(ToAgent::Pause(true))?;
        }
        self.pump(ctx)?;
        self.ctl.tick(&mut self.sess).map_err(|e| e.to_string())?;
        self.feed_uploads()?;
        if !self.held.is_empty() || self.mods != Modifiers::NONE {
            self.release_all()?;
        }
        Ok(())
    }

    pub(super) fn pump(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let start = Instant::now();
        loop {
            // A flood of messages waits for the next frame rather than keep this one from drawing.
            if start.elapsed() > PUMP_BUDGET {
                ctx.request_repaint();
                return Ok(());
            }
            match self.rx.try_recv() {
                Ok(Ev::Net(ToClient::Frame(f))) => {
                    let resized = self.fb.apply(&f).map_err(|e| e.to_string())?;
                    self.upload(ctx, resized, &f);
                    self.ctl.frame_done(f.covered() * 2 >= self.fb.w * self.fb.h);
                    self.send(ToAgent::Ack(f.id))?;
                    if std::mem::take(&mut self.fb.need_key) {
                        self.send(ToAgent::Refresh)?;
                    }
                }
                Ok(Ev::Net(ToClient::Pong(t))) => self.ctl.pong(t),
                Ok(Ev::Net(ToClient::Audio(p))) => {
                    if let Some(pl) = self.sound.as_mut() {
                        pl.push(&p);
                    }
                }
                Ok(Ev::Net(ToClient::Notice(t))) => self.notify(&t),
                Ok(Ev::Net(ToClient::Snapshot(png))) => self.notify(&crate::session::save_snapshot(&png)),
                Ok(Ev::Net(ToClient::Chat(t))) => {
                    // A message is cut where a chat stops being one.
                    let t = crate::chat::trimmed(&t);
                    crate::chat::append_history(self.host(), true, &t);
                    match self.chat.as_mut() {
                        Some((lines, _)) => lines.push((true, t, crate::chat::now())),
                        None => {
                            self.notify(&format!("Message: {t}"));
                            self.chat_lines.push((true, t, crate::chat::now()));
                            self.unread += 1;
                        }
                    }
                }
                Ok(Ev::Net(ToClient::Monitors { list, current })) => {
                    (self.monitors, self.monitor) = (list, current as usize)
                }
                Ok(Ev::Net(ToClient::Hello { w, h, backend })) => {
                    // Another monitor: new size, and the capture must be fitted again.
                    self.sess.hello(w, h, backend.clone());
                    let host = self.label.split("  ").next().unwrap_or("").to_string();
                    self.label = format!("{host}  {w}x{h}  {backend}");
                    self.view = (0, 0);
                }
                // Only the host being worked in writes the clipboard here: one in another pane or
                // tab cannot replace what was just copied.
                Ok(Ev::Net(ToClient::Clipboard(t))) => {
                    if self.share_clipboard && self.active {
                        self.clip.set(t);
                    }
                }
                Ok(Ev::Net(ToClient::FileProgress { id, received })) => {
                    if let Some(u) = self.uploads.iter_mut().find(|u| u.id == id) {
                        u.acked = received;
                    }
                }
                Ok(Ev::Net(ToClient::DirList { path, entries, error })) => {
                    if let Some(f) = self.files.as_mut() {
                        f.remote.set_list(path, entries, error);
                    }
                }
                Ok(Ev::Net(ToClient::WindowList { list, error })) => {
                    if let Some(p) = self.picker.as_mut() {
                        p.0 = Some(if error.is_empty() { Ok(list) } else { Err(error) });
                    }
                }
                // The chooser shows what happened; a helper in place may list the windows already.
                Ok(Ev::Net(ToClient::HelperInstalled { ok, text })) => {
                    if let Some(p) = self.picker.as_mut() {
                        p.0 = Some(Err(text.clone()));
                    }
                    self.notify(&text);
                    if ok {
                        let _ = self.send(ToAgent::ListWindows);
                    }
                }
                Ok(Ev::Net(ToClient::Sharing { kind, text })) => {
                    if kind != self.sharing.0 || text != self.sharing.1 {
                        self.notify(&format!("Now sharing {text}"));
                    }
                    self.sharing = (kind, text);
                }
                Ok(Ev::Net(ToClient::TermOutput { id, data })) => {
                    if let Some(t) = self.terms.as_mut() {
                        t.output(id, &data);
                    }
                }
                Ok(Ev::Net(ToClient::TermExit { id, text })) => match self.terms.as_mut() {
                    Some(t) => t.exited(id, text),
                    None => self.notify(&format!("Terminal: {text}")),
                },
                Ok(Ev::Net(ToClient::OpDone { ok, text })) => match self.files.as_mut() {
                    Some(f) => {
                        let cmd = f.op_done(ok, &text);
                        self.file_cmd(cmd)?;
                    }
                    None => self.notify(&text),
                },
                Ok(Ev::Net(ToClient::FetchStart { id, size, tag, name })) => {
                    let dir = self.fetch_dirs.get(&tag).cloned().unwrap_or_else(crate::files::downloads);
                    let started = self.sess.fetch_start(&mut self.inbox, id, size, &dir, &name).map_err(|e| format!("connection lost: {e}"))?;
                    if let Some(crate::files::Note::Done { text, .. }) = started {
                        self.notify(&format!("{name}: {text}"));
                    } else {
                        self.downloads.push((id, name, tag));
                    }
                }
                Ok(Ev::Net(ToClient::FetchData { id, data })) => match self
                    .sess
                    .fetch_data(&mut self.inbox, id, &data)
                    .map_err(|e| format!("connection lost: {e}"))?
                {
                    Some(crate::files::Note::Done { text, .. }) => {
                        let name = self.downloads.iter().find(|d| d.0 == id).map(|d| d.1.clone()).unwrap_or_default();
                        self.downloads.retain(|d| d.0 != id);
                        self.notify(&format!("{name} failed: {text}"));
                    }
                    _ => {}
                },
                Ok(Ev::Net(ToClient::FetchEnd { id, ok, text })) => {
                    if id != 0 && !self.downloads.iter().any(|d| d.0 == id) {
                        // Already cancelled or failed here; one that could not even be opened
                        // there says why.
                        if !ok && text != "cancelled" {
                            self.notify(&format!("Download failed: {text}"));
                        }
                        continue;
                    }
                    let name = self.downloads.iter().find(|d| d.0 == id).map(|d| d.1.clone()).unwrap_or_default();
                    let tag = self.downloads.iter().find(|d| d.0 == id).map_or(0, |d| d.2);
                    self.downloads.retain(|d| d.0 != id);
                    match self.inbox.end(id, ok, &text) {
                        Some(crate::files::Note::Done { ok: true, .. }) => {
                            let folder = self.fetch_dirs.get(&tag).cloned().unwrap_or_else(crate::files::downloads);
                            self.got.add(name, folder.display().to_string());
                        }
                        Some(crate::files::Note::Done { text, .. }) => self.notify(&format!("{name} failed: {text}")),
                        _ => self.notify(&format!("Download failed: {text}")),
                    }
                    // After the file is closed, so the listing shows its full size.
                    if tag != 0 && let Some(f) = self.files.as_mut() {
                        f.refresh_local();
                    }
                }
                Ok(Ev::Net(ToClient::FileDone { id, ok, text })) => {
                    let name = self.uploads.iter().find(|u| u.id == id).map(|u| u.name.clone()).unwrap_or_default();
                    let cancelled = self.uploads.iter().any(|u| u.id == id && u.cancelled);
                    let error = self.uploads.iter().find(|u| u.id == id).and_then(|u| u.error.clone());
                    let dir = self.uploads.iter().find(|u| u.id == id).and_then(|u| u.dir.clone());
                    self.uploads.retain(|u| u.id != id);
                    if dir.is_some() && let Some(f) = self.files.as_ref() {
                        let cmd = f.refresh_remote();
                        self.file_cmd(cmd)?;
                    }
                    match (error, cancelled, ok) {
                        (Some(e), ..) => self.notify(&format!("{name} failed: {e}")),
                        (None, true, _) => self.notify(&format!("{name}: upload cancelled")),
                        (None, false, true) => {
                            // The host's own path for one in its Downloads folder.
                            let folder = dir.unwrap_or_else(|| text.rsplit_once(['/', '\\']).map_or(text.clone(), |(d, _)| d.to_string()));
                            self.sent.add(name, folder);
                        }
                        (None, false, false) => self.notify(&format!("{name} failed: {text}")),
                    }
                }
                Ok(Ev::Net(ToClient::Error(e)) | Ev::Closed(e)) => {
                    let log = self.sess.log();
                    return Err(if log.is_empty() { e } else { format!("{e}\n{log}") });
                }
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => return Err("connection closed".into()),
            }
        }
    }

    /// Starts uploading files dropped on the window.
    pub(super) fn take_drops(&mut self, ctx: &egui::Context) -> Result<(), String> {
        // Beside other panes, files dropped go to the active one only.
        if !self.active {
            return Ok(());
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let path = f.path().to_path_buf();
            if path.as_os_str().is_empty() {
                continue;
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let mut files = Vec::new();
            if path.is_dir() {
                // The folder is recreated on the remote host, "name/sub/file".
                crate::files::walk(&path, &name, &mut files);
            } else {
                files.push((path, name));
            }
            for (path, name) in files {
                self.queue_upload(&path, name, None)?;
            }
        }
        Ok(())
    }

    /// Uploads files or whole folders into `dir` on the remote host.
    pub(super) fn upload_into(&mut self, paths: &[std::path::PathBuf], dir: &str) -> Result<(), String> {
        for path in paths {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let mut files = Vec::new();
            if path.is_dir() {
                crate::files::walk(path, &name, &mut files);
            } else {
                files.push((path.clone(), name));
            }
            for (path, name) in files {
                self.queue_upload(&path, name, Some(dir.to_string()))?;
            }
        }
        Ok(())
    }

    pub(super) fn queue_upload(
        &mut self,
        path: &std::path::Path,
        name: String,
        dir: Option<String>,
    ) -> Result<(), String> {
        let size = match std::fs::metadata(path) {
            Ok(m) => m.len(),
            Err(e) => {
                self.notify(&format!("{name}: {e}"));
                return Ok(());
            }
        };
        let id = self.next_upload;
        self.next_upload += 1;
        let path = path.to_path_buf();
        self.uploads.push_back(Upload { id, name, dir, cancelled: false, error: None, path, file: None, size, sent: 0, acked: 0, ended: false });
        Ok(())
    }

    /// Sends the next chunks of the first unfinished upload, keeping a bounded amount unacknowledged.
    pub(super) fn feed_uploads(&mut self) -> Result<(), String> {
        use std::io::Read;
        let Some(i) = self.uploads.iter().position(|u| !u.ended) else { return Ok(()) };
        // Each file starts once the one before it is sent, so the host has one open at a time
        // however many were dropped.
        if self.uploads[i].file.is_none() {
            let u = &mut self.uploads[i];
            match std::fs::File::open(&u.path) {
                Ok(f) => {
                    u.size = f.metadata().map_or(u.size, |m| m.len());
                    u.file = Some(f);
                    let start = ToAgent::FileStart { id: u.id, size: u.size, dir: u.dir.clone().unwrap_or_default(), name: u.name.clone() };
                    self.send(start)?;
                }
                Err(e) => {
                    let text = format!("{} failed: {e}", u.name);
                    self.uploads.remove(i);
                    self.notify(&text);
                    return Ok(());
                }
            }
        }
        let mut buf = vec![0u8; UPLOAD_CHUNK];
        loop {
            let u = &mut self.uploads[i];
            if u.sent - u.acked.min(u.sent) >= UPLOAD_WINDOW {
                return Ok(());
            }
            let id = u.id;
            // The size it had when it started: a file that grows meanwhile still arrives whole.
            let want = (u.size - u.sent).min(UPLOAD_CHUNK as u64) as usize;
            let read = match u.file.as_mut() {
                Some(f) if want > 0 => f.read(&mut buf[..want]),
                _ => Ok(0),
            };
            let n = match read {
                Ok(0) if want > 0 => Err("it got shorter while being sent".to_string()),
                Ok(n) => Ok(n),
                Err(e) => Err(e.to_string()),
            };
            match n {
                Ok(0) => {
                    (u.ended, u.file) = (true, None);
                    return self.send(ToAgent::FileEnd { id });
                }
                Ok(n) => {
                    u.sent += n as u64;
                    self.send(ToAgent::FileData { id, data: buf[..n].to_vec() })?;
                }
                // A file that cannot be read on (a network drive gone) ends short, so the host
                // drops what it has; the connection goes on.
                Err(e) => {
                    (u.error, u.ended, u.file) = (Some(e), true, None);
                    return self.send(ToAgent::FileEnd { id });
                }
            }
        }
    }

    /// The chat panel; returns its rectangle while open.
    /// Opens the terminal panel under the picture (with the keyboard) and the chat beside it.
    pub(super) fn open_panels(&mut self, terminal: bool, chat: bool) {
        if terminal {
            self.terms.get_or_insert_with(|| crate::termwin::TermWindow::new(self.ssh_cmd.clone()));
            (self.term_split, self.term_focus) = (true, true);
        }
        if chat && self.chat.is_none() {
            self.chat = Some((std::mem::take(&mut self.chat_lines), String::new()));
            self.unread = 0;
        }
    }

    /// The area beside the chat panel, and the panel's own, with the splitter between to drag.
    fn chat_split(&mut self, ui: &mut egui::Ui, area: Rect) -> (Rect, Option<Rect>) {
        if self.chat.is_none() {
            return (area, None);
        }
        // At most six tenths of the pane, so the picture keeps the rest however narrow it is.
        let most = area.width() * 0.6;
        let w = self.chat_w.clamp(260.0_f32.min(most), most.max(1.0));
        let panel = area.with_min_x(area.right() - w);
        let handle = Rect::from_min_max(Pos2::new(panel.left() - 3.0, area.top()), Pos2::new(panel.left() + 3.0, area.bottom()));
        let r = ui.interact(handle, ui.scope_id().with("chat-splitter"), egui::Sense::drag());
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        self.chat_w = w - r.drag_delta().x;
        (area.with_max_x(panel.left()), Some(panel))
    }

    /// The picture above, the terminal panel below, and the splitter between them to drag.
    fn term_split_at(&mut self, ui: &mut egui::Ui, area: Rect) -> (Rect, Option<Rect>) {
        let h = self.term_h.clamp(120.0, (area.height() - 120.0).max(120.0));
        let panel = area.with_min_y(area.bottom() - h);
        let handle = Rect::from_min_max(Pos2::new(area.left(), panel.top() - 3.0), Pos2::new(area.right(), panel.top() + 3.0));
        let r = ui.interact(handle, ui.scope_id().with("terminal-splitter"), egui::Sense::drag());
        if r.hovered() || r.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        self.term_h = h - r.drag_delta().y;
        (area.with_max_y(panel.top()), Some(panel))
    }

    /// The terminal under the picture: a header to hide it, fill the window with it or move it to a
    /// window of its own, then the shells.
    fn terminal_panel(&mut self, ui: &mut egui::Ui, area: Rect) -> Result<(), String> {
        let fill = ui.visuals().panel_fill;
        ui.painter().rect_filled(area, CornerRadius::ZERO, fill);
        let line = ui.visuals().widgets.noninteractive.bg_stroke;
        ui.painter().hline(area.x_range(), area.top(), line);
        let head = Rect::from_min_size(area.min, Vec2::new(area.width(), 28.0));
        let mut what = None;
        let mut bar = ui.new_child(egui::UiBuilder::new().max_rect(head.shrink2(Vec2::new(10.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
        bar.label(RichText::new("TERMINAL").small().strong());
        bar.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let small = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                ui.add(egui::Button::new(RichText::new(glyph).size(14.0)).frame_when_inactive(false).min_size(Vec2::splat(24.0)))
                    .on_hover_text(tip)
                    .clicked()
            };
            if small(ui, ic::CARET_DOWN, "Hide the panel (the shells keep running)") {
                what = Some('h');
            }
            if small(ui, ic::ARROW_SQUARE_OUT, "Move the terminal to a window of its own") {
                what = Some('w');
            }
            if small(ui, ic::CORNERS_OUT, "Fill the window with the terminal (the remote screen waits behind it)") {
                what = Some('f');
            }
        });
        match what {
            Some('h') => self.term_split = false,
            Some('w') => (self.term_split, self.term_detached) = (false, true),
            Some('f') => (self.term_split, self.term_inline) = (false, true),
            _ => {}
        }
        if what.is_some() {
            self.term_focus = false;
            return self.release_all();
        }
        let body = area.with_min_y(head.bottom());
        let Some(mut t) = self.terms.take() else { return Ok(()) };
        t.keys_on = self.term_focus;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("terminal-panel"));
        let cmds = t.embedded(&mut child);
        self.terms = Some(t);
        for c in cmds {
            match c {
                crate::termwin::Cmd::Send(m) => self.send(m)?,
                crate::termwin::Cmd::Close => {
                    self.terms = None;
                    self.term_split = false;
                    self.term_focus = false;
                }
            }
        }
        Ok(())
    }

    /// The chat docked beside the picture: earlier messages with this host, then this session's.
    pub(super) fn chat_panel(&mut self, ui: &mut egui::Ui, area: Rect) -> Result<Option<Rect>, String> {
        let host = self.host().to_string();
        let peer = self.peer.get();
        let earlier = self.chat_earlier;
        let Some((lines, input)) = self.chat.as_mut() else { return Ok(None) };
        let mut sent = None;
        let (mut close, mut clear) = (false, false);
        let fill = ui.visuals().panel_fill;
        ui.painter().rect_filled(area, CornerRadius::ZERO, fill);
        ui.painter().vline(area.left(), area.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
        let mut panel = ui.new_child(egui::UiBuilder::new().max_rect(area.shrink2(Vec2::new(12.0, 8.0))).id_salt("chat-panel"));
        panel.horizontal(|ui| {
            ui.label(RichText::new(format!("{}  Chat", ic::CHAT)).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let small = |ui: &mut egui::Ui, glyph: &str, tip: &str| {
                    ui.add(egui::Button::new(RichText::new(glyph).size(14.0)).frame_when_inactive(false).min_size(Vec2::splat(24.0)))
                        .on_hover_text(tip)
                        .clicked()
                };
                close = small(ui, ic::X, "Close the chat (the conversation is kept)");
                clear = earlier + lines.len() > 0 && small(ui, ic::TRASH, "Clear the history kept with this host");
            });
        });
        panel.label(RichText::new("With the person at the SSH host's screen; a window opens there.").small().weak());
        let h = (panel.available_height() - 52.0).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("chat-lines")
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .max_height(h)
            .show(&mut panel, |ui| crate::chat::show_lines_since(ui, lines, earlier, &peer, "You"));
        panel.add_space(4.0);
        sent = crate::chat::input_row(&mut panel, input, true).or(sent);
        if clear {
            crate::chat::clear_history(&host);
            lines.clear();
            self.chat_earlier = 0;
        }
        if let Some(text) = sent {
            if let Some((lines, _)) = self.chat.as_mut() {
                lines.push((false, text.clone(), crate::chat::now()));
            }
            crate::chat::append_history(&host, false, &text);
            self.send(ToAgent::Chat(text))?;
        }
        if close && let Some((lines, _)) = self.chat.take() {
            self.chat_lines = lines;
        }
        Ok(Some(area))
    }

    /// Shows the file transfer window and carries out what it asks for.
    /// Uploads and downloads in progress, for the file manager's list.
    fn transfers(&self) -> Vec<crate::filewin::Transfer> {
        let mut transfers: Vec<crate::filewin::Transfer> = self
            .uploads
            .iter()
            .filter(|u| !u.cancelled)
            .map(|u| crate::filewin::Transfer {
                upload: true,
                id: u.id,
                name: u.name.clone(),
                done: u.acked,
                size: u.size,
            })
            .collect();
        for (id, name, _) in &self.downloads {
            let (done, size) = self.inbox.received(*id).unwrap_or((0, 0));
            transfers.push(crate::filewin::Transfer { upload: false, id: *id, name: name.clone(), done, size });
        }
        transfers
    }

    /// Chooses what the remote host shares, as in a video call: a screen, a window or a program.
    pub(super) fn share_picker(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let Some((list, tab)) = self.picker.as_mut() else { return Ok(()) };
        let mut pick: Option<ToAgent> = None;
        let mut close = false;
        let mut click_pick = false;
        let monitors = self.monitors.clone();
        let current = self.monitor;
        let (modal, fit) = crate::gui::modal(ctx, "sshremote-share-picker", Vec2::new(560.0, 440.0));
        let modal = modal.show(ctx, |ui| {
            ui.set_width(fit.x);
            ui.set_max_height(fit.y);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Choose what to share").size(18.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon(ui, ic::X, "Close", false).clicked() {
                        close = true;
                    }
                });
            });
            ui.label(RichText::new("The SSH host sends only what you choose; clicks and keys reach only that.").weak());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (i, (g, name)) in
                    [(ic::MONITOR, "Entire screen"), (ic::APP_WINDOW, "Window"), (ic::SQUARES_FOUR, "Application")]
                        .into_iter()
                        .enumerate()
                {
                    if ui.selectable_label(*tab == i as u8, format!("{g}  {name}")).clicked() {
                        *tab = i as u8;
                    }
                }
            });
            ui.separator();
            let row = |ui: &mut egui::Ui, glyph: &str, title: &str, sub: &str, on: bool| -> bool {
                let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), egui::Sense::click());
                if on {
                    ui.painter().rect_stroke(rect, CornerRadius::same(crate::gui::theme::R_SM), egui::Stroke::new(1.0, super::selected_stroke()), egui::StrokeKind::Inside);
                }
                let fill = if on {
                    super::selected_fill()
                } else if r.hovered() {
                    ui.visuals().widgets.hovered.bg_fill
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, CornerRadius::same(crate::gui::theme::R_MD), fill);
                let text = ui.visuals().text_color();
                let weak = ui.visuals().weak_text_color();
                ui.painter().text(
                    rect.left_center() + Vec2::new(14.0, 0.0),
                    Align2::LEFT_CENTER,
                    glyph,
                    FontId::proportional(20.0),
                    text,
                );
                let galley =
                    ui.painter().layout(title.to_string(), FontId::proportional(14.0), text, rect.width() - 70.0);
                ui.painter().galley(rect.min + Vec2::new(46.0, 5.0), galley, text);
                ui.painter().text(
                    rect.min + Vec2::new(46.0, 25.0),
                    Align2::LEFT_TOP,
                    sub,
                    FontId::proportional(11.5),
                    weak,
                );
                r.clicked()
            };
            egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((fit.y - 110.0).clamp(80.0, 330.0)).show(ui, |ui| match *tab {
                0 => {
                    let screens: Vec<(String, String)> = if monitors.is_empty() {
                        vec![(String::new(), "The only screen".into())]
                    } else {
                        monitors
                            .iter()
                            .map(|m| {
                                (m.name.clone(), format!("{}x{}{}", m.w, m.h, if m.primary { ", primary" } else { "" }))
                            })
                            .collect()
                    };
                    for (i, (name, sub)) in screens.iter().enumerate() {
                        let on = self.sharing.0 == 0 && i == current;
                        if row(ui, ic::MONITOR, format!("Screen {}  {name}", i + 1).trim(), sub, on) {
                            pick = Some(if i == current || monitors.len() < 2 {
                                ToAgent::Share(Share::Screen)
                            } else {
                                ToAgent::SelectMonitor(i as u8)
                            });
                        }
                    }
                }
                t => {
                    // The window in front, or its program: no list needed.
                    let no_helper = matches!(list, Some(Err(e)) if e.starts_with(crate::capture::NEEDS_HELPER));
                    if t == 1 {
                        let sub = "The window in front on the SSH host; click it there first";
                        if row(ui, ic::APP_WINDOW, "Active window", sub, false) {
                            pick = Some(ToAgent::Share(Share::ActiveWindow));
                        }
                        // Pointing at the window itself: the click brings it to the front, then it is shared.
                        let sub = "Click the window you want in the picture of the SSH host";
                        if row(ui, ic::CURSOR_CLICK, "Click a window to share", sub, false) {
                            click_pick = true;
                        }
                    } else if no_helper {
                        ui.add_enabled_ui(false, |ui| {
                            row(
                                ui,
                                ic::SQUARES_FOUR,
                                "Active application",
                                "Needs the sshremote helper on GNOME",
                                false,
                            )
                        });
                    } else {
                        let sub = "Every window of the program in front on the SSH host";
                        if row(ui, ic::SQUARES_FOUR, "Active application", sub, false) {
                            pick = Some(ToAgent::Share(Share::ActiveApp));
                        }
                    }
                    ui.separator();
                    share_list(ui, list, t, &row, &mut pick);
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if click_pick {
            self.click_pick = true;
            self.pick_pressed = false;
            close = true;
            if self.sharing.0 != 0 {
                // The whole screen first, so every window can be seen and clicked.
                self.send(ToAgent::Share(Share::Screen))?;
            }
        }
        if let Some(m) = pick {
            // The chooser stays open for the install's answer.
            let installing = m == ToAgent::InstallHelper;
            self.send(m)?;
            close = !installing;
            if installing && let Some(p) = self.picker.as_mut() {
                p.0 = None;
            }
        }
        if close {
            self.picker = None;
        }
        Ok(())
    }

    /// The terminal in this window, with a bar to go back to the screen or give it its own window.
    pub(super) fn inline_terminal(&mut self, ui: &mut egui::Ui, area: Rect) -> Result<(), String> {
        let Some(mut t) = self.terms.take() else { return Ok(()) };
        t.keys_on = self.active;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).id_salt("inline-terminal"));
        let cmds = t.embedded(&mut child);
        self.terms = Some(t);
        for c in cmds {
            match c {
                crate::termwin::Cmd::Send(m) => self.send(m)?,
                crate::termwin::Cmd::Close => {
                    self.terms = None;
                    self.term_inline = false;
                    // A pane that was the shell goes with it, as a plain shell's pane does.
                    self.pane_closed = true;
                }
            }
        }
        Ok(())
    }

    pub(super) fn term_window(&mut self, ctx: &egui::Context) -> Result<(), String> {
        if !self.term_detached {
            return Ok(());
        }
        let Some(mut t) = self.terms.take() else { return Ok(()) };
        t.keys_on = true;
        let host = self.host().to_string();
        let cmds = t.show(ctx, &host);
        self.terms = Some(t);
        for c in cmds {
            match c {
                crate::termwin::Cmd::Send(m) => self.send(m)?,
                crate::termwin::Cmd::Close => {
                    self.terms = None;
                    self.term_detached = false;
                }
            }
        }
        Ok(())
    }

    pub(super) fn file_cmd(&mut self, c: crate::filewin::Cmd) -> Result<(), String> {
        use crate::filewin::Cmd;
        match c {
            Cmd::Send(m) => self.send(m)?,
            Cmd::Upload { paths, to } => self.upload_into(&paths, &to)?,
            Cmd::Download { paths, to } => {
                for path in paths {
                    let tag = self.next_tag;
                    self.next_tag += 1;
                    self.fetch_dirs.insert(tag, to.clone());
                    self.send(ToAgent::Fetch { tag, path })?;
                }
            }
            Cmd::Cancel(true, id) => {
                if let Some(i) = self.uploads.iter().position(|u| u.id == id) {
                    let u = &mut self.uploads[i];
                    if u.file.is_none() && !u.ended {
                        // Not started: the host never heard of it.
                        let text = format!("{}: upload cancelled", u.name);
                        self.uploads.remove(i);
                        self.notify(&text);
                    } else {
                        u.cancelled = true;
                        if !u.ended {
                            (u.ended, u.file) = (true, None);
                            self.send(ToAgent::FileEnd { id })?;
                        }
                    }
                }
            }
            Cmd::Cancel(false, id) => {
                if let Some(&(_, ref name, tag)) = self.downloads.iter().find(|d| d.0 == id) {
                    let name = name.clone();
                    self.send(ToAgent::FetchCancel { id, tag })?;
                    self.inbox.end(id, false, "cancelled");
                    self.downloads.retain(|d| d.0 != id);
                    self.notify(&format!("{name}: download cancelled"));
                }
            }
        }
        Ok(())
    }

    /// Progress of uploads and the drop hint, bottom left over the remote screen.
    pub(super) fn upload_overlay(&self, ctx: &egui::Context, screen: Rect) {
        // Dropped files go to the active pane: only it says so.
        let hovering = self.active && ctx.input(|i| !i.raw.hovered_files.is_empty());
        // The files under way, and how many wait after them (a dropped folder can hold thousands).
        let sending: Vec<&Upload> = self.uploads.iter().filter(|u| !u.cancelled).collect();
        let mut lines: Vec<String> = sending
            .iter()
            .take(3)
            .map(|u| format!("Sending {}  {}%", u.name, (u.acked * 100).checked_div(u.size).unwrap_or(100)))
            .collect();
        if sending.len() > 3 {
            lines.push(format!("and {} more to send", sending.len() - 3));
        }
        let receiving: Vec<String> = self
            .downloads
            .iter()
            .filter_map(|(id, name, _)| {
                let (got, size) = self.inbox.received(*id)?;
                Some(format!("Receiving {name}  {}%", (got * 100).checked_div(size).unwrap_or(100)))
            })
            .collect();
        lines.extend(receiving.iter().take(3).cloned());
        if receiving.len() > 3 {
            lines.push(format!("and {} more being received", receiving.len() - 3));
        }
        if hovering {
            lines.push(format!("Drop to send to the Downloads folder on {}", self.host()));
        }
        if lines.is_empty() {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("uploads")));
        let text = lines.join("\n");
        let galley = painter.layout_no_wrap(text, FontId::proportional(14.0), Color32::WHITE);
        let pos = Pos2::new(screen.left() + 16.0, screen.bottom() - 16.0 - galley.size().y);
        let bg = Rect::from_min_size(pos, galley.size()).expand(8.0);
        painter.rect_filled(bg, CornerRadius::same(crate::gui::theme::R_MD), Color32::from_rgba_unmultiplied(28, 30, 36, 230));
        painter.galley(pos, galley, Color32::WHITE);
    }

    /// Copies changed pixels into the texture.
    pub(super) fn upload(&mut self, ctx: &egui::Context, resized: bool, f: &crate::proto::Frame) {
        let (w, h) = (self.fb.w, self.fb.h);
        let opts = TextureOptions::LINEAR;
        match self.tex.as_mut() {
            Some(t) if !resized && t.size() == [w, h] => {
                // Tiles of one row band go up as one span, and a frame that changed most of the
                // screen as one image: a texture upload per 32 px tile costs far more than the bytes.
                let mut bands: Vec<(usize, usize, usize, usize)> = Vec::new();
                for (x, y, rw, rh) in f.damage() {
                    match bands.iter_mut().find(|b| b.1 == y && b.3 == rh) {
                        Some(b) => {
                            let right = (b.0 + b.2).max(x + rw);
                            b.0 = b.0.min(x);
                            b.2 = right - b.0;
                        }
                        None => bands.push((x, y, rw, rh)),
                    }
                }
                let area: usize = bands.iter().map(|b| b.2 * b.3).sum();
                if area * 2 > w * h {
                    t.set(ColorImage::from_rgb([w, h], &self.fb.rgb), opts);
                    return;
                }
                for (x, y, rw, rh) in bands {
                    let mut px = Vec::with_capacity(rw * rh * 3);
                    for row in y..y + rh {
                        px.extend_from_slice(&self.fb.rgb[(row * w + x) * 3..(row * w + x + rw) * 3]);
                    }
                    t.set_partial([x, y], ColorImage::from_rgb([rw, rh], &px), opts);
                }
            }
            _ => self.tex = Some(ctx.load_texture("remote", ColorImage::from_rgb([w, h], &self.fb.rgb), opts)),
        }
    }

    /// Asks the agent for a capture that fits the window, once resizing has settled.
    pub(super) fn resize(&mut self, px: Vec2) -> Result<(), String> {
        let want = match self.fit {
            Fit::Window | Fit::Stretch => (px.x.max(64.0) as u16, px.y.max(64.0) as u16),
            // Below 100% the agent sends fewer pixels; above, the full resolution is enlarged here.
            Fit::Fixed(p) => {
                let k = p.min(100) as f32 / 100.0;
                ((self.sess.desk.0 as f32 * k) as u16, (self.sess.desk.1 as f32 * k) as u16)
            }
        };
        if want != self.view {
            self.view = want;
            self.view_changed = Some(Instant::now());
        }
        if let Some(t) = self.view_changed
            && t.elapsed() > Duration::from_millis(250)
        {
            self.view_changed = None;
            (self.ctl.s.view_w, self.ctl.s.view_h) = self.view;
            let s = self.ctl.s;
            self.send(ToAgent::Settings(s))?;
        }
        Ok(())
    }

    /// Draws the remote screen; returns where it is, in points.
    pub(super) fn picture(&mut self, ui: &mut egui::Ui, area: Rect, ppp: f32) -> Option<Rect> {
        let tex = self.tex.as_ref()?;
        let size = Vec2::new(self.fb.w as f32, self.fb.h as f32) / ppp;
        let rect = match self.fit {
            Fit::Window if self.fill_full && self.filling => area,
            Fit::Window => {
                let k = (area.width() / size.x).min(area.height() / size.y);
                Rect::from_center_size(area.center(), size * k)
            }
            Fit::Stretch => area,
            Fit::Fixed(p) => {
                let full = Vec2::new(self.sess.desk.0 as f32, self.sess.desk.1 as f32) * (p as f32 / 100.0) / ppp;
                let over = (full - area.size()).max(Vec2::ZERO);
                self.pan = self.pan.clamp(Vec2::ZERO, over);
                // Centred along an axis where it fits, panned along one where it does not.
                let min = Pos2::new(
                    if over.x > 0.0 { area.min.x - self.pan.x } else { area.center().x - full.x / 2.0 },
                    if over.y > 0.0 { area.min.y - self.pan.y } else { area.center().y - full.y / 2.0 },
                );
                Rect::from_min_size(min, full)
            }
        };
        let painter = ui.painter().with_clip_rect(area);
        painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        // Where the visible part lies, when the picture is larger than the window.
        let bar = Color32::from_white_alpha(70);
        if rect.width() > area.width() + 0.5 {
            let (a, w) = ((area.min.x - rect.min.x) / rect.width(), area.width() / rect.width());
            let x0 = area.min.x + a * area.width();
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, area.max.y - 5.0), Pos2::new(x0 + w * area.width(), area.max.y - 1.0)),
                CornerRadius::same(2),
                bar,
            );
        }
        if rect.height() > area.height() + 0.5 {
            let (a, h) = ((area.min.y - rect.min.y) / rect.height(), area.height() / rect.height());
            let y0 = area.min.y + a * area.height();
            painter.rect_filled(
                Rect::from_min_max(
                    Pos2::new(area.max.x - 5.0, y0),
                    Pos2::new(area.max.x - 1.0, y0 + h * area.height()),
                ),
                CornerRadius::same(2),
                bar,
            );
        }
        Some(rect)
    }

    /// Moves a picture larger than the window when the pointer rests near an edge.
    pub(super) fn edge_pan(&mut self, ctx: &egui::Context, area: Rect) {
        if !matches!(self.fit, Fit::Fixed(_)) {
            return;
        }
        let Some(p) = ctx.pointer_hover_pos().filter(|p| area.contains(*p)) else { return };
        const EDGE: f32 = 28.0;
        let dt = ctx.input(|i| i.stable_dt).min(0.05);
        let mut d = Vec2::ZERO;
        if p.x < area.min.x + EDGE {
            d.x = -1.0;
        } else if p.x > area.max.x - EDGE {
            d.x = 1.0;
        }
        if p.y < area.min.y + EDGE {
            d.y = -1.0;
        } else if p.y > area.max.y - EDGE {
            d.y = 1.0;
        }
        if d != Vec2::ZERO {
            self.pan += d * 900.0 * dt;
            ctx.request_repaint();
        }
    }

    pub(super) fn remote_point(&self, rect: Rect, pos: Pos2) -> Option<(u16, u16)> {
        if !rect.contains(pos) || self.fb.w == 0 {
            return None;
        }
        let x = ((pos.x - rect.min.x) / rect.width() * self.fb.w as f32) as usize;
        let y = ((pos.y - rect.min.y) / rect.height() * self.fb.h as f32) as usize;
        Some((x.min(self.fb.w - 1) as u16, y.min(self.fb.h - 1) as u16))
    }

    pub(super) fn key(&mut self, keysym: u32, down: bool) -> Result<(), String> {
        if down {
            self.held.insert(keysym);
        } else if !self.held.remove(&keysym) {
            return Ok(());
        }
        self.send(ToAgent::KeyState { keysym, down })
    }

    /// Taps a key, for the Copy/Cut/Paste shortcuts the window system turns into events.
    pub(super) fn tap(&mut self, keysym: u32) -> Result<(), String> {
        self.key(keysym, true)?;
        self.key(keysym, false)
    }

    pub(super) fn sync_mods(&mut self, m: Modifiers) -> Result<(), String> {
        let pairs =
            [(self.mods.shift, m.shift, SHIFT_L), (self.mods.ctrl, m.ctrl, CONTROL_L), (self.mods.alt, m.alt, ALT_L)];
        for (was, now, sym) in pairs {
            if was != now {
                self.key(sym, now)?;
            }
        }
        self.mods = m;
        Ok(())
    }

    /// Releases everything held, as when the window loses focus mid-shortcut.
    pub(super) fn release_all(&mut self) -> Result<(), String> {
        for k in std::mem::take(&mut self.held) {
            self.send(ToAgent::KeyState { keysym: k, down: false })?;
        }
        self.mods = Modifiers::NONE;
        Ok(())
    }

    pub(super) fn input(&mut self, ctx: &egui::Context, shown: Option<Rect>, ui_rects: &[Rect]) -> Result<(), String> {
        let (events, focused) = ctx.input(|i| (i.events.clone(), i.focused));
        if !focused {
            return self.release_all();
        }
        // Pointer events over any of this window's own controls (toolbar, menus, panels) stay here.
        let over_toolbar = |p: Pos2| {
            ui_rects.iter().any(|t| t.contains(p))
                || ctx.layer_id_at(p).is_some_and(|l| l.order != egui::Order::Background)
        };
        // Keys go to the remote unless a toolbar widget (a slider, a menu) or the terminal panel has them.
        let term_keys = self.term_split && self.term_focus && self.terms.is_some() && !self.term_detached;
        let local = ctx.memory(|m| m.focused().is_some()) || ctx.any_popup_open() || term_keys;
        if let Some(rect) = shown.filter(|_| !local) {
            // Composed text (Korean, Japanese, Chinese input methods) arrives as IME commits.
            ctx.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Terminal,
                    rect,
                    cursor_rect: Rect::from_min_size(rect.min, Vec2::new(1.0, 16.0)),
                    should_interrupt_composition: false,
                })
            });
        }
        for e in events {
            if self.click_pick && matches!(e, Event::Key { key: Key::Escape, .. }) {
                if matches!(e, Event::Key { pressed: true, .. }) {
                    self.click_pick = false;
                    self.notify("Choosing a window cancelled");
                }
                continue;
            }
            match e {
                // Full screen here: the host does not see this one key.
                Event::Key { key: Key::Enter, modifiers, .. } if modifiers.ctrl && modifiers.alt => {}
                Event::Key { key, physical_key, pressed, modifiers, .. } if !local => {
                    self.sync_mods(modifiers)?;
                    let Some(sym) = keysym(physical_key.unwrap_or(key)) else { continue };
                    if pressed && std::mem::take(&mut self.win_next) {
                        let (sym, mods) = if sym == ' ' as u32 { (SUPER_L, 0) } else { (sym, MOD_SUPER) };
                        self.send(ToAgent::Key { keysym: sym, mods })?;
                        continue;
                    }
                    self.key(sym, pressed)?;
                }
                Event::ModifiersChanged(m) if !local => self.sync_mods(m)?,
                // Clipboard keys reach here as copy, cut and paste: the host gets the keys pressed,
                // Ctrl+C, X and V, or Ctrl+Insert, Shift+Delete and Shift+Insert (with the
                // modifiers already held there), never a bare letter.
                Event::Copy if !local => self.tap(if crate::sys::insert_held() { xk::INSERT } else { 'c' as u32 })?,
                Event::Cut if !local => self.tap(if self.mods.shift && !self.mods.ctrl { xk::DELETE } else { 'x' as u32 })?,
                Event::Paste(_) if !local => self.tap(if self.mods.shift && !self.mods.ctrl { xk::INSERT } else { 'v' as u32 })?,
                Event::Ime(egui::ImeEvent::Commit(text)) if !local => {
                    for c in text.chars() {
                        let sym = xk::of_char(c);
                        self.send(ToAgent::Key { keysym: sym, mods: 0 })?;
                    }
                }
                Event::PointerMoved(pos) if !over_toolbar(pos) => {
                    if let Some(p) = shown.and_then(|r| self.remote_point(r, pos))
                        && p != self.pointer
                    {
                        self.pointer = p;
                        self.send(ToAgent::Pointer { x: p.0, y: p.1 })?;
                    }
                }
                Event::PointerButton { pos, button, pressed, modifiers } => {
                    if pressed && (over_toolbar(pos) || local || super::chrome::on_edge(ctx, pos)) {
                        continue;
                    }
                    // Shift+click, Ctrl+click: the modifiers held here are held there for the click.
                    if !local {
                        self.sync_mods(modifiers)?;
                    }
                    let Some(p) = shown.and_then(|r| self.remote_point(r, pos)) else { continue };
                    if p != self.pointer {
                        self.pointer = p;
                        self.send(ToAgent::Pointer { x: p.0, y: p.1 })?;
                    }
                    let b = match button {
                        egui::PointerButton::Primary => 1,
                        egui::PointerButton::Middle => 2,
                        egui::PointerButton::Secondary => 3,
                        egui::PointerButton::Extra1 => 8,
                        egui::PointerButton::Extra2 => 9,
                    };
                    self.send(ToAgent::Button { button: b, pressed })?;
                    if self.click_pick && b == 1 && pressed {
                        self.pick_pressed = true;
                    }
                    if self.click_pick && self.pick_pressed && b == 1 && !pressed {
                        self.pick_pressed = false;
                        // Once the window has come to the front, it becomes what is shared.
                        self.click_pick = false;
                        self.pick_at = Some(Instant::now() + Duration::from_millis(400));
                    }
                }
                Event::MouseWheel { unit, delta, modifiers, .. } => {
                    let Some(pos) = ctx.pointer_hover_pos() else { continue };
                    if over_toolbar(pos) || !shown.is_some_and(|r| r.contains(pos)) {
                        continue;
                    }
                    // Ctrl+wheel zooms there, Shift+wheel scrolls sideways there.
                    if !local {
                        self.sync_mods(modifiers)?;
                    }
                    let lines = match unit {
                        egui::MouseWheelUnit::Point => delta / 40.0,
                        egui::MouseWheelUnit::Line => delta,
                        egui::MouseWheelUnit::Page => delta * 10.0,
                    };
                    // Whole steps go now, the rest adds up with what comes next.
                    self.wheel -= lines;
                    let (sy, sx) = (self.wheel.y.trunc() as i32, self.wheel.x.trunc() as i32);
                    self.wheel -= Vec2::new(sx as f32, sy as f32);
                    if sy != 0 {
                        self.send(ToAgent::Scroll { steps: sy.clamp(-10, 10) as i8 })?;
                    }
                    if sx != 0 {
                        self.send(ToAgent::HScroll { steps: sx.clamp(-10, 10) as i8 })?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The connection's state in a few lines, for a tooltip: quality, round trip, rate, frames,
    /// host and display settings.
    pub(super) fn details(&self) -> String {
        let state = match self.ctl.rtt {
            r if r < 80.0 => "good",
            r if r < 250.0 => "slow",
            _ => "poor",
        };
        format!(
            "Connection {state}\nRound trip {:.0} ms, {:.0} KB/s, {:.0} fps\n{}\n{}",
            self.ctl.rtt,
            self.ctl.kbps,
            self.ctl.fps_now,
            self.label,
            self.ctl.describe()
        )
    }

    /// Watching only: keys, clicks and the wheel do not reach the host.
    pub(super) fn view_only(&self) -> bool {
        self.view_only
    }

    /// The pane's controls in the workspace's title bar (or its full-screen dock): the screen's
    /// display, sharing, keys, terminal, chat, screenshot, pause and options.
    pub(super) fn toolbar_items(&mut self, ui: &mut egui::Ui, action: &mut Option<Deferred>) {
        let screen = self.screen_controls();
        // A pane showing its shell: the shell's own controls first.
        if self.mode() == super::workspace::PaneMode::Terminal
            && let Some(t) = self.terms.as_mut()
        {
            t.controls(ui);
            ui.separator();
        }
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.spacing_mut().button_padding = Vec2::new(6.0, 4.0);
        // Input off: said here, one click turns it back on.
        if self.view_only && ui.button(RichText::new(format!("{}  View only  {}", ic::EYE, ic::X)).small()).on_hover_text("Mouse and keyboard do not reach the host. Click to control it again").clicked() {
            self.view_only = false;
        }
        if screen {
        if self.sharing.0 != 0 {
            let name: String = self.sharing.1.chars().take(18).collect();
            let chip = egui::Button::new(RichText::new(format!("{}  {name}  {}", ic::APP_WINDOW, ic::X)).small())
                .fill(super::selected_fill())
                .stroke(egui::Stroke::new(1.0, super::selected_stroke()))
                .corner_radius(CornerRadius::same(crate::gui::theme::R_MD));
            if ui
                .add(chip)
                .on_hover_text(format!("Sharing {}. Click to share the whole screen again.", self.sharing.1))
                .clicked()
            {
                *action = Some(Box::new(|r, _| r.send(ToAgent::Share(Share::Screen))));
            }
        }
        ui.separator();

        let display = icon_menu(ui, ic::MONITOR, |ui| {
            ui.label(RichText::new("Quality").strong());
            let auto = self.ctl.auto.is_some();
            if ui.selectable_label(auto, "Auto (adapts to the connection)").clicked() {
                *action = Some(Box::new(|r, _| r.menu('a')));
                ui.close();
            }
            for (i, p) in PRESETS.iter().enumerate() {
                let on = !auto && self.ctl.preset == p.name;
                if ui.selectable_label(on, preset_text(p)).clicked() {
                    let c = char::from(b'0' + i as u8);
                    *action = Some(Box::new(move |r, _| r.menu(c)));
                    ui.close();
                }
            }
            ui.collapsing("Custom", |ui| {
                let mut mode = self.ctl.s.mode;
                ui.label(RichText::new("Colours").weak());
                ui.horizontal_wrapped(|ui| {
                    for m in Mode::ALL {
                        ui.selectable_value(&mut mode, m, m.label());
                    }
                });
                let mut scale = self.ctl.s.scale;
                ui.add(egui::Slider::new(&mut scale, 20..=100).text("Resolution %"));
                let mut fps = self.ctl.s.fps;
                ui.add(egui::Slider::new(&mut fps, 1..=60).text("Frames per second"));
                if (mode, scale, fps) != (self.ctl.s.mode, self.ctl.s.scale, self.ctl.s.fps) {
                    *action = Some(Box::new(move |r, _| r.custom(mode, scale, fps)));
                }
            });
            ui.separator();
            ui.label(RichText::new("View").strong());
            let mut pick = |ui: &mut egui::Ui, fit: Fit, text: &str| {
                if ui.selectable_label(self.fit == fit, text).clicked() {
                    *action = Some(Box::new(move |r, _| {
                        (r.fit, r.pan) = (fit, Vec2::ZERO);
                        Ok(())
                    }));
                    ui.close();
                }
            };
            pick(ui, Fit::Window, "Fit to window");
            pick(ui, Fit::Stretch, "Stretch to fill");
            ui.checkbox(&mut self.fill_full, "Fill the whole screen in full screen")
                .on_hover_text("No bars when this screen's shape differs from the host's: the picture stretches a little");
            ui.horizontal_wrapped(|ui| {
                for p in [50, 75, 100, 125, 150, 200] {
                    pick(ui, Fit::Fixed(p), &format!("{p}%"));
                }
            });
            if self.monitors.len() > 1 {
                ui.separator();
                ui.label(RichText::new("Screen").strong());
                for (i, m) in self.monitors.iter().enumerate() {
                    let primary = if m.primary { ", primary" } else { "" };
                    let text = format!("{}  {} ({}x{}{primary})", i + 1, m.name, m.w, m.h);
                    if ui.selectable_label(i == self.monitor, text).clicked() {
                        *action = Some(Box::new(move |r, _| {
                            r.chosen_monitor = Some(i as u8);
                            r.send(ToAgent::SelectMonitor(i as u8))
                        }));
                        ui.close();
                    }
                }
            }
        });
        display.on_hover_text(format!("Display: {}; view {}", self.ctl.describe(), self.fit.label()));

        if icon(ui, ic::APP_WINDOW, "Share: the entire screen, one window or one application", self.sharing.0 != 0)
            .clicked()
        {
            *action = Some(Box::new(|r, _| {
                r.picker = Some((None, if r.sharing.0 == 0 { 0 } else { 1 }));
                r.send(ToAgent::ListWindows)
            }));
        }
        let keys = icon_menu(ui, ic::KEYBOARD, |ui| {
            let combos: [(&str, &str, Option<Vec<u32>>); 9] = [
                ("Secure attention", "Ctrl+Alt+Del", None),
                ("Lock screen", "Win+L", Some(vec![SUPER_L, 'l' as u32])),
                ("Start menu", "Win", Some(vec![SUPER_L])),
                ("Run", "Win+R", Some(vec![SUPER_L, 'r' as u32])),
                ("File explorer", "Win+E", Some(vec![SUPER_L, 'e' as u32])),
                ("Show desktop", "Win+D", Some(vec![SUPER_L, 'd' as u32])),
                ("Switch window", "Alt+Tab", Some(vec![ALT_L, xk::TAB])),
                ("Close window", "Alt+F4", Some(vec![ALT_L, xk::F1 + 3])),
                ("Print screen", "PrtSc", Some(vec![xk::PRINT])),
            ];
            for (label, shortcut, keys) in combos {
                if ui.add(egui::Button::new(label).shortcut_text(shortcut)).clicked() {
                    *action = Some(Box::new(move |r, _| match keys {
                        None => r.send(ToAgent::SecureAttention),
                        Some(k) => r.combo(&k),
                    }));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Win + next key").clicked() {
                *action = Some(Box::new(|r, _| {
                    r.win_next = true;
                    r.notify("Press a key to send with Win (Space: Win alone)");
                    Ok(())
                }));
                ui.close();
            }
        });
        keys.on_hover_text("Send special keys");
        ui.separator();
        }
        // The terminal under the screen, in a pane too, as with the chat.
        let (glyph, tip) = if self.term_inline && self.terms.is_some() {
            (ic::MONITOR, "Back to the remote screen, with the terminal under it")
        } else {
            (ic::TERMINAL, "Terminal: a shell on the host in a panel under the screen (right click for more)")
        };
        let showing = self.terms.is_some() && (self.term_split || self.term_inline || self.term_detached);
        let term = icon(ui, glyph, tip, showing);
        if term.clicked() {
            *action = Some(Box::new(|r, _| {
                if r.terms.is_none() {
                    r.terms = Some(crate::termwin::TermWindow::new(r.ssh_cmd.clone()));
                    r.term_split = false;
                }
                if r.term_inline || r.term_detached {
                    // Back under the picture.
                    (r.term_inline, r.term_detached, r.term_split) = (false, false, true);
                } else {
                    r.term_split = !r.term_split;
                }
                r.term_focus = r.term_split;
                r.release_all()
            }));
        }
        term.context_menu(|ui| {
            crate::gui::menu_look(ui);
            if ui.button(format!("{}  Terminal in place of the screen", ic::CORNERS_OUT)).clicked() {
                *action = Some(Box::new(|r, _| {
                    if r.terms.is_none() {
                        r.terms = Some(crate::termwin::TermWindow::new(r.ssh_cmd.clone()));
                    }
                    (r.term_inline, r.term_detached, r.term_split) = (true, false, false);
                    r.release_all()
                }));
                ui.close();
            }
            if ui.button(format!("{}  Terminal in its own window", ic::TERMINAL)).clicked() {
                *action = Some(Box::new(|r, _| {
                    if r.terms.is_none() {
                        r.terms = Some(crate::termwin::TermWindow::new(r.ssh_cmd.clone()));
                    }
                    (r.term_inline, r.term_detached, r.term_split) = (false, true, false);
                    Ok(())
                }));
                ui.close();
            }
            if self.terms.is_some() && ui.button(format!("{}  Close all shells", ic::X)).clicked() {
                *action = Some(Box::new(|r, _| {
                    if let Some(t) = r.terms.take() {
                        for c in t.close_all() {
                            if let crate::termwin::Cmd::Send(m) = c {
                                r.send(m)?;
                            }
                        }
                    }
                    (r.term_inline, r.term_detached, r.term_split) = (false, false, false);
                    Ok(())
                }));
                ui.close();
            }
        });
        let chat_glyph = if self.unread > 0 { format!("{} {}", ic::CHAT, self.unread) } else { ic::CHAT.into() };
        let chat_tip = "Chat with the person at the SSH host's screen";
        let shot_tip = "Screenshot: save the SSH host's screen as a PNG in Pictures";
        let (pause_glyph, pause_tip) = if self.paused {
            (ic::PLAY, "Resume the picture")
        } else {
            (ic::PAUSE, "Pause the picture to save traffic; fetch single frames when needed")
        };
        // Chat is with the person at the screen: shown with the screen only.
        let chat = screen && crate::modules::on("chat") && icon(ui, &chat_glyph, chat_tip, self.chat.is_some()).clicked();
        let shot = screen && icon(ui, ic::CAMERA, shot_tip, false).clicked();
        // Paused: the play button shows as on, so the frozen state is plain to see.
        let (mut pause, mut frame, mut helper) = (false, false, false);
        if screen {
            let b = tool_button(pause_glyph, self.paused);
            pause = ui.add(b).on_hover_text(pause_tip).clicked();
            frame = self.paused && icon(ui, ic::ARROW_CLOCKWISE, "Fetch one frame", false).clicked();
        }

        let mut sound = self.sound.is_some();
        let mut sound_clicked = false;
        // The screen's options (view only, sound, system keys...): only where the screen is shown.
        if screen {
            let more = icon_menu(ui, ic::GEAR, |ui| {
                ui.checkbox(&mut self.view_only, "View only")
                    .on_hover_text("Watch without sending keys, clicks or the wheel");
                ui.checkbox(&mut self.share_clipboard, "Share clipboard").on_hover_text("Copied text goes both ways");
                if crate::modules::on("sound") {
                    sound_clicked = ui.checkbox(&mut sound, "Sound (about 8 KB/s)").clicked();
                }
                if cfg!(windows) {
                    let tip = "While this screen is the active pane, Win, Alt+Tab, Alt+Esc, Ctrl+Esc and Print Screen go to the SSH host instead of this PC";
                    ui.checkbox(&mut self.grab_keys, "Send system keys").on_hover_text(tip);
                }
                ui.separator();
                if ui.button("Repaint the whole screen").clicked() {
                    *action = Some(Box::new(|r, _| r.menu('r')));
                    ui.close();
                }
                // A GNOME host shows single windows and programs with its helper only.
                if self.label.ends_with("gnome") {
                    let tip = "Lets you share one window or program of this GNOME host; it starts at the host's next login";
                    if ui.button(format!("{}  Install the window helper on this host", ic::DOWNLOAD)).on_hover_text(tip).clicked() {
                        helper = true;
                        ui.close();
                    }
                }
            });
            more.on_hover_text("Options");
        }
        if helper {
            // The answer shows in the window chooser, opened on its windows' page.
            *action = Some(Box::new(|r, _| {
                r.picker = Some((None, 1));
                r.send(ToAgent::InstallHelper)
            }));
        }
        if chat {
            *action = Some(Box::new(|r, _| {
                match r.chat.take() {
                    Some((lines, _)) => r.chat_lines = lines,
                    None => {
                        r.chat = Some((std::mem::take(&mut r.chat_lines), String::new()));
                        r.unread = 0;
                    }
                }
                Ok(())
            }));
        }
        if shot {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Snapshot)));
        }
        if pause {
            let on = !self.paused;
            (self.paused, self.user_paused) = (on, on);
            *action = Some(Box::new(move |r, _| r.send(ToAgent::Pause(on))));
        }
        if frame {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Refresh)));
        }
        if sound_clicked {
            *action = Some(Box::new(move |r, _| {
                if sound {
                    match crate::audio::Player::new() {
                        Ok(p) => r.sound = Some(p),
                        Err(e) => {
                            r.notify(&format!("Sound: {e}"));
                            return Ok(());
                        }
                    }
                } else {
                    r.sound = None;
                }
                r.send(ToAgent::Audio(sound))
            }));
        }
    }

    /// Where the picture was drawn last frame.
    #[cfg(test)]
    pub(super) fn shown_rect(&self) -> Option<Rect> {
        self.last_shown
    }

    /// Pictures received so far.
    #[cfg(test)]
    pub(super) fn frames(&self) -> u32 {
        self.ctl.frames
    }

    #[cfg(test)]
    pub(super) fn set_view_only(&mut self, on: bool) {
        self.view_only = on;
    }

    #[cfg(test)]
    pub(super) fn toast_texts(&self) -> Vec<&str> {
        self.toasts.texts()
    }

    /// The round trip to the host in milliseconds.
    pub(super) fn rtt(&self) -> f32 {
        self.ctl.rtt as f32
    }

    /// Ends the connection.
    pub(super) fn close(&mut self) {
        self.sess.close();
    }

    /// Shells opened before the connection, or kept from a lost one, shown in its terminal.
    pub(super) fn adopt_terms(&mut self, mut t: crate::termwin::TermWindow) {
        t.bare = true;
        self.terms = Some(t);
    }

    /// The shells, kept when the connection is lost.
    pub(super) fn take_terms(&mut self) -> Option<crate::termwin::TermWindow> {
        self.terms.take()
    }

    /// Whether the screen's own controls (quality, keys, pause...) belong in the toolbar now.
    fn screen_controls(&self) -> bool {
        self.mode() == super::workspace::PaneMode::Screen
    }

    /// What a pane of this connection shows.
    pub(super) fn mode(&self) -> super::workspace::PaneMode {
        use super::workspace::PaneMode;
        if self.files_only {
            PaneMode::Files
        } else if self.term_inline && self.terms.is_some() {
            PaneMode::Terminal
        } else {
            PaneMode::Screen
        }
    }

    /// Shows the terminal, the screen or the files of this connection, the others put away: the
    /// picture stops (no traffic) unless it is shown, the file manager closes (transfers go on),
    /// the shells keep running.
    pub(super) fn set_mode(&mut self, mode: super::workspace::PaneMode) -> Result<(), String> {
        use super::workspace::PaneMode;
        if mode == self.mode() {
            return Ok(());
        }
        let was_screen = self.mode() == PaneMode::Screen;
        self.release_all()?;
        self.term_inline = false;
        self.files_only = false;
        self.files = None;
        match mode {
            PaneMode::Terminal => {
                let t = self.terms.get_or_insert_with(|| crate::termwin::TermWindow::new(self.ssh_cmd.clone()));
                t.bare = true;
                self.term_inline = true;
            }
            PaneMode::Files => {
                self.files = Some(crate::filewin::FileWindow::new());
                self.files_only = true;
                self.send(ToAgent::ListDir { path: String::new() })?;
            }
            PaneMode::Screen => {}
        }
        // The picture stops while it is not shown, and stays stopped back on the screen if the
        // user paused it.
        let show = mode == PaneMode::Screen;
        let pause = !show || self.user_paused;
        if show != was_screen && pause != self.paused {
            self.paused = pause;
            self.send(ToAgent::Pause(pause))?;
            if !pause {
                self.send(ToAgent::Refresh)?;
            }
        }
        Ok(())
    }

    /// Presses keys in order and releases them in reverse.
    pub(super) fn combo(&mut self, keys: &[u32]) -> Result<(), String> {
        for &k in keys {
            self.send(ToAgent::KeyState { keysym: k, down: true })?;
        }
        for &k in keys.iter().rev() {
            self.send(ToAgent::KeyState { keysym: k, down: false })?;
        }
        Ok(())
    }

    pub(super) fn menu(&mut self, c: char) -> Result<(), String> {
        match self.ctl.menu(c) {
            Action::Refresh => {
                self.fb = Framebuffer::default();
                self.tex = None;
                self.send(ToAgent::Refresh)
            }
            Action::Settings => {
                let s = self.ctl.s;
                self.send(ToAgent::Settings(s))
            }
            _ => Ok(()),
        }
    }

    pub(super) fn custom(&mut self, mode: Mode, scale: u8, fps: u8) -> Result<(), String> {
        (self.ctl.s.mode, self.ctl.s.scale, self.ctl.s.fps) = (mode, scale, fps);
        self.ctl.preset = "custom".into();
        self.ctl.auto = None;
        let s = self.ctl.s;
        self.send(ToAgent::Settings(s))
    }

    /// What the user chose here, to carry into the connection made after this one drops.
    pub(super) fn prefs(&self) -> Prefs {
        let quality = if self.ctl.auto.is_some() {
            Quality::Auto
        } else if let Some(i) = PRESETS.iter().position(|p| p.name == self.ctl.preset) {
            Quality::Preset(i)
        } else {
            Quality::Custom(self.ctl.s.mode, self.ctl.s.scale, self.ctl.s.fps)
        };
        Prefs {
            view_only: self.view_only,
            fit: self.fit,
            fill_full: self.fill_full,
            share_clipboard: self.share_clipboard,
            grab_keys: self.grab_keys,
            sound: self.sound.is_some(),
            quality,
            chat: self.chat.is_some(),
            chat_w: self.chat_w,
            term_h: self.term_h,
            monitor: self.chosen_monitor,
            paused: self.user_paused,
        }
    }

    /// Takes on what the user chose in the connection before this one. The quality goes out
    /// with the first picture size, as on any start.
    pub(super) fn restore(&mut self, p: Prefs) -> Result<(), String> {
        (self.view_only, self.fit, self.fill_full) = (p.view_only, p.fit, p.fill_full);
        (self.share_clipboard, self.grab_keys) = (p.share_clipboard, p.grab_keys);
        (self.chat_w, self.term_h) = (p.chat_w, p.term_h);
        match p.quality {
            Quality::Auto => {
                if self.ctl.auto.is_none() {
                    let _ = self.ctl.menu('a');
                }
            }
            Quality::Preset(i) => {
                let _ = self.ctl.menu(char::from(b'0' + i as u8));
            }
            Quality::Custom(mode, scale, fps) => {
                (self.ctl.s.mode, self.ctl.s.scale, self.ctl.s.fps) = (mode, scale, fps);
                (self.ctl.preset, self.ctl.auto) = ("custom".into(), None);
            }
        }
        self.open_panels(false, p.chat);
        if let Some(m) = p.monitor {
            self.chosen_monitor = Some(m);
            self.send(ToAgent::SelectMonitor(m))?;
        }
        if p.sound {
            match crate::audio::Player::new() {
                Ok(pl) => {
                    self.sound = Some(pl);
                    self.send(ToAgent::Audio(true))?;
                }
                Err(e) => self.notify(&format!("Sound: {e}")),
            }
        }
        self.user_paused = p.paused;
        if p.paused && self.mode() == super::workspace::PaneMode::Screen && !self.paused {
            self.paused = true;
            self.send(ToAgent::Pause(true))?;
        }
        Ok(())
    }

    /// Text pasted into the host from an extension or the command palette: a screen gets it on
    /// its clipboard and then Ctrl+V; a terminal types it.
    pub(super) fn paste_text(&mut self, ctx: &egui::Context, text: String) -> Result<(), String> {
        use super::workspace::PaneMode;
        match self.mode() {
            PaneMode::Screen if !self.view_only => {
                self.send(ToAgent::Clipboard(text))?;
                self.release_all()?;
                self.send(ToAgent::Key { keysym: 'v' as u32, mods: crate::proto::MOD_CTRL })
            }
            PaneMode::Screen => {
                self.notify("View only: nothing was pasted");
                Ok(())
            }
            // The terminal takes it as a paste from the keyboard would.
            PaneMode::Terminal => {
                ctx.input_mut(|i| i.events.push(egui::Event::Paste(text)));
                Ok(())
            }
            PaneMode::Files => {
                self.notify("Nothing to paste into here");
                Ok(())
            }
        }
    }
}

/// The windows or programs to pick from, or why there is no list.
fn share_list(
    ui: &mut egui::Ui,
    list: &Option<WindowList>,
    t: u8,
    row: &dyn Fn(&mut egui::Ui, &str, &str, &str, bool) -> bool,
    pick: &mut Option<ToAgent>,
) {
    match list {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Asking the SSH host for its windows...");
            });
        }
        Some(Err(e)) => {
            ui.add_space(8.0);
            ui.label(e.as_str());
            if e.starts_with(crate::capture::NEEDS_HELPER) && e.contains("Install it") {
                ui.add_space(6.0);
                let b = crate::gui::theme::primary("Install the helper on the SSH host");
                if ui.add(b).clicked() {
                    *pick = Some(ToAgent::InstallHelper);
                }
            }
        }
        Some(Ok(wins)) if wins.is_empty() => {
            ui.label(RichText::new("No windows are open on the SSH host.").weak());
        }
        Some(Ok(wins)) if t == 1 => {
            for w in wins {
                let sub = format!("{}  {}x{}", w.app, w.w, w.h);
                if row(ui, ic::APP_WINDOW, &w.title, &sub, false) {
                    *pick = Some(ToAgent::Share(Share::Window(w.id)));
                }
            }
        }
        Some(Ok(wins)) => {
            let mut apps: Vec<(String, u64, usize)> = Vec::new();
            for w in wins {
                match apps.iter_mut().find(|a| a.0 == w.app) {
                    Some(a) => a.2 += 1,
                    None => apps.push((w.app.clone(), w.id, 1)),
                }
            }
            for (app, id, n) in apps {
                let sub = if n == 1 { "1 window".to_string() } else { format!("{n} windows") };
                if row(ui, ic::SQUARES_FOUR, &app, &sub, false) {
                    *pick = Some(ToAgent::Share(Share::App(id)));
                }
            }
        }
    }
}

/// Every toolbar action of the remote screen, clicked as a user would, against the built-in test
/// desktop: cargo build --release, then
/// SSHREMOTE_EXE=target/release/sshremote.exe cargo test --release -- --ignored --test-threads=1 toolbar_actions
#[cfg(test)]
mod actions {
    use super::*;
    use egui::{Event, PointerButton};

    struct Harness {
        ctx: egui::Context,
        r: Remote,
        clock: f64,
        size: Vec2,
        shapes: Vec<egui::epaint::ClippedShape>,
        snap: super::super::snapshot::Snap,
    }

    impl Harness {
        fn new(size: Vec2) -> Harness {
            // Settings go to a scratch folder, never the user's; set once, before any agent starts
            // (these tests run one at a time: cargo test -- --test-threads=1).
            static SCRATCH: std::sync::Once = std::sync::Once::new();
            SCRATCH.call_once(|| {
                let scratch = std::env::temp_dir().join(format!("sshremote-ui-test-{}", std::process::id()));
                std::fs::create_dir_all(&scratch).unwrap();
                for var in ["APPDATA", "XDG_CONFIG_HOME"] {
                    unsafe { std::env::set_var(var, &scratch) };
                }
            });
            let opt = crate::parse_client(&["--gui".into(), "--local".into()]).unwrap();
            let (sess, _tx, rx) = Session::connect(&opt).expect("a local agent (SSHREMOTE_EXE)");
            let ctl = Control::new(&opt, Mode::True);
            let r = Remote::new(sess, rx, ctl, "test desktop".into(), None);
            let ctx = egui::Context::default();
            crate::gui::style(&ctx);
            let mut h = Harness { ctx, r, clock: 0.0, size, shapes: Vec::new(), snap: Default::default() };
            // The first picture arrives.
            for _ in 0..40 {
                h.frame(Vec::new()).unwrap();
                if h.r.tex.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            h
        }

        fn frame(&mut self, events: Vec<Event>) -> Result<(), String> {
            self.clock += 0.05;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
                time: Some(self.clock),
                focused: true,
                events,
                ..Default::default()
            };
            let mut result = Ok(());
            let r = &mut self.r;
            let out = self.snap.frame(&self.ctx, input, |ui| {
                // The pane's controls in a bar across the top, as the workspace draws them, and the
                // pane under it.
                let whole = ui.max_rect();
                let bar = Rect::from_min_size(whole.min, Vec2::new(whole.width(), super::super::chrome::HEIGHT));
                let mut action = None;
                let mut top = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::left_to_right(egui::Align::Center)));
                r.toolbar_items(&mut top, &mut action);
                let mut pane = ui.new_child(egui::UiBuilder::new().max_rect(whole.with_min_y(bar.bottom())));
                result = r.frame(&mut pane);
                if let (Ok(()), Some(f)) = (&result, action) {
                    result = f(r, ui.ctx());
                }
            });
            self.shapes = out.shapes;
            result
        }

        /// Where text containing `what` was drawn, the topmost (popups are drawn last).
        fn find(&self, what: &str) -> Option<Rect> {
            self.find_in(what, Rect::EVERYTHING)
        }

        fn find_in(&self, what: &str, within: Rect) -> Option<Rect> {
            let mut hits = Vec::new();
            for c in &self.shapes {
                collect(&c.shape, what, &mut hits);
            }
            hits.into_iter().rev().find(|r| within.contains(r.center()))
        }

        /// Clicks a toolbar button: `what` in the title bar, drawn first (a window shown inside this
        /// test's single viewport, such as the file manager, may lie over it).
        fn tool(&mut self, what: &str) -> Result<(), String> {
            // What the last state change shows, drawn before looking.
            self.frame(Vec::new())?;
            let bar = Rect::from_min_size(Pos2::ZERO, Vec2::new(self.size.x, super::super::chrome::HEIGHT));
            let mut hits = Vec::new();
            for c in self.shapes.iter().filter(|c| c.clip_rect.intersects(bar)) {
                collect(&c.shape, what, &mut hits);
            }
            let at = hits.into_iter().find(|r| bar.contains(r.center())).unwrap_or_else(|| panic!("no {what:?} in the toolbar")).center();
            self.click_at(at)
        }

        /// Moves to and clicks the text `what`, then lets two frames pass.
        fn click(&mut self, what: &str) -> Result<(), String> {
            let at = self.find(what).unwrap_or_else(|| panic!("nothing drawn reads {what:?}")).center();
            self.click_at(at)
        }

        fn click_at(&mut self, at: Pos2) -> Result<(), String> {
            self.frame(vec![Event::PointerMoved(at)])?;
            let press = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Default::default() };
            self.frame(vec![press(true)])?;
            self.frame(vec![press(false)])?;
            self.frame(Vec::new())?;
            self.frame(Vec::new())
        }
    }

    fn collect(shape: &egui::Shape, what: &str, hits: &mut Vec<Rect>) {
        match shape {
            egui::Shape::Text(t) if t.galley.job.text.contains(what) => hits.push(t.galley.rect.translate(t.pos.to_vec2())),
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, what, hits)),
            _ => {}
        }
    }

    /// A click with Shift held is a Shift+click at the host.
    #[test]
    #[ignore]
    fn modifiers_go_with_clicks() {
        let mut h = Harness::new(Vec2::new(1000.0, 700.0));
        h.frame(Vec::new()).unwrap();
        let at = h.r.last_shown.expect("the picture").center();
        let press = |pressed, modifiers| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers };
        h.frame(vec![Event::PointerMoved(at)]).unwrap();
        h.frame(vec![press(true, egui::Modifiers::SHIFT)]).unwrap();
        assert!(h.r.mods.shift && h.r.held.contains(&SHIFT_L), "Shift is down at the host for the click");
        h.frame(vec![press(false, egui::Modifiers::NONE)]).unwrap();
        assert!(!h.r.mods.shift, "and let go with it");
    }

    /// Alt held alone reaches the host before the Tab the hook takes, and is let go there too.
    #[test]
    #[ignore]
    fn alt_tab_reaches_the_host_with_alt() {
        let mut h = Harness::new(Vec2::new(1000.0, 700.0));
        let show = |h: &mut Harness, modifiers: egui::Modifiers, grabbed: Vec<(u32, bool)>| {
            let mut grabbed = Some(grabbed);
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, h.size)),
                focused: true,
                events: vec![Event::ModifiersChanged(modifiers)],
                ..Default::default()
            };
            let r = &mut h.r;
            let mut result = Ok(());
            h.snap.frame(&h.ctx, input, |ui| result = r.show(ui, grabbed.take().unwrap_or_default()));
            result.unwrap();
        };
        show(&mut h, egui::Modifiers::NONE, Vec::new());
        show(&mut h, egui::Modifiers::ALT, vec![(xk::TAB, true), (xk::TAB, false)]);
        assert!(h.r.mods.alt, "Alt is down at the host");
        assert!(h.r.held.contains(&ALT_L), "held there");
        show(&mut h, egui::Modifiers::NONE, Vec::new());
        assert!(!h.r.mods.alt && !h.r.held.contains(&ALT_L), "and let go");
    }

    #[test]
    #[ignore]
    fn toolbar_actions() {
        let mut h = Harness::new(Vec2::new(1280.0, 800.0));
        assert!(h.r.tex.is_some(), "the picture arrives");

        h.tool(ic::PAUSE).unwrap();
        assert!(h.r.paused, "pause");
        h.tool(ic::PLAY).unwrap();
        assert!(!h.r.paused, "resume");

        // The terminal opens in a panel under the picture, with the keyboard.
        h.tool(ic::TERMINAL).unwrap();
        assert!(h.r.terms.is_some() && h.r.term_split && h.r.term_focus, "terminal panel under the screen");
        assert!(h.r.last_shown.is_some(), "the picture stays in view");
        // A click on the picture gives it the keyboard back; one in the panel takes it again.
        let pic = h.r.last_shown.unwrap();
        h.click_at(pic.center()).unwrap();
        assert!(!h.r.term_focus, "the picture has the keys");
        h.click_at(Pos2::new(pic.center().x, h.size.y - 40.0)).unwrap();
        assert!(h.r.term_focus, "the terminal has the keys");
        // Typing goes to the shell, not to the remote screen.
        let typed = Event::Text("x".into());
        h.frame(vec![typed]).unwrap();
        h.tool(ic::TERMINAL).unwrap();
        assert!(!h.r.term_split && h.r.terms.is_some(), "hidden, the shell still running");

        h.tool(ic::CHAT).unwrap();
        assert!(h.r.chat.is_some(), "chat opens");
        let pic = h.r.last_shown.unwrap();
        assert!(pic.right() <= h.size.x - 260.0, "the picture makes room for the chat: {pic:?}");
        h.tool(ic::CHAT).unwrap();
        assert!(h.r.chat.is_none(), "chat closes");

        h.tool(ic::GEAR).unwrap();
        h.click("View only").unwrap();
        assert!(h.r.view_only, "view only from the options");
        h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]).unwrap();

        // Ctrl+Alt+Enter asks the workspace for full screen; F11 is the host's.
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        h.frame(vec![key(egui::Key::F11, Default::default())]).unwrap();
        assert!(!h.r.full_toggle, "F11 goes to the host");
        h.frame(vec![key(egui::Key::Enter, egui::Modifiers::CTRL | egui::Modifiers::ALT)]).unwrap();
        assert!(h.r.full_toggle, "Ctrl+Alt+Enter asks for full screen");
    }

    /// The quality, view and special-key menus.
    #[test]
    #[ignore]
    fn menu_actions() {
        let mut h = Harness::new(Vec2::new(1280.0, 800.0));
        let first = &crate::proto::PRESETS[0];
        h.tool(ic::MONITOR).unwrap();
        h.click(&crate::gui::preset_text(first)).unwrap();
        assert!(h.r.ctl.auto.is_none() && h.r.ctl.preset == first.name, "a fixed preset");
        h.tool(ic::MONITOR).unwrap();
        h.click("Auto (adapts").unwrap();
        assert!(h.r.ctl.auto.is_some(), "auto again");
        h.tool(ic::MONITOR).unwrap();
        h.click("Stretch to fill").unwrap();
        assert!(h.r.fit == Fit::Stretch, "stretch");
        h.tool(ic::MONITOR).unwrap();
        h.click("Fit to window").unwrap();
        assert!(h.r.fit == Fit::Window, "fit");

        // Keys go to the built-in test desktop, never to this computer.
        h.tool(ic::KEYBOARD).unwrap();
        h.click("Start menu").unwrap();
        h.tool(ic::KEYBOARD).unwrap();
        h.click("Win + next key").unwrap();
        assert!(h.r.win_next, "Win with the next key");
        for _ in 0..5 {
            h.frame(Vec::new()).unwrap();
        }
    }

    /// Pictures of the remote screen's menus in both themes, wide and narrow:
    /// SSHREMOTE_EXE=... SSHREMOTE_SNAP_DIR=folder cargo test --release -- --ignored --test-threads=1 menus_
    #[test]
    #[ignore]
    fn menus_remote() {
        // Pictures only on request: there is nothing to check without a folder to look at.
        let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else {
            eprintln!("menus_remote: set SSHREMOTE_SNAP_DIR to get the pictures");
            return;
        };
        for (dark, size) in [(true, Vec2::new(1280.0, 800.0)), (false, Vec2::new(1280.0, 800.0)), (true, Vec2::new(900.0, 520.0))] {
            let mut h = Harness::new(size);
            crate::gui::theme::apply_theme(&h.ctx, dark);
            h.frame(Vec::new()).unwrap();
            let name = format!("remote_{}_{}", if dark { "dark" } else { "light" }, size.x as u32);
            let save = |h: &mut Harness, what: &str| {
                // Popups fade in: the picture once they are fully there.
                for _ in 0..8 {
                    h.frame(Vec::new()).unwrap();
                }
                h.snap.save(&h.ctx, h.size, &dir.join(format!("{name}_{what}.png")));
            };
            let escape = |h: &mut Harness| {
                h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]).unwrap();
                h.frame(Vec::new()).unwrap();
            };
            for (glyph, what) in [(ic::MONITOR, "display"), (ic::KEYBOARD, "keys"), (ic::GEAR, "options")] {
                if h.find_in(glyph, Rect::from_min_size(Pos2::ZERO, Vec2::new(h.size.x, 40.0))).is_none() {
                    continue;
                }
                h.tool(glyph).unwrap();
                save(&mut h, what);
                escape(&mut h);
            }
            // The terminal button's own menu, by a right click.
            let at = h.find_in(ic::TERMINAL, Rect::from_min_size(Pos2::ZERO, Vec2::new(h.size.x, 40.0))).unwrap().center();
            h.frame(vec![Event::PointerMoved(at)]).unwrap();
            let press = |pressed| Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed, modifiers: Default::default() };
            h.frame(vec![press(true)]).unwrap();
            h.frame(vec![press(false)]).unwrap();
            h.frame(Vec::new()).unwrap();
            save(&mut h, "terminal_menu");
            escape(&mut h);
            h.r.open_panels(true, true);
            for _ in 0..4 {
                h.frame(Vec::new()).unwrap();
            }
            save(&mut h, "panels");
        }
    }
}
