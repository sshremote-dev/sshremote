//! A connected host: its screen, the toolbar, input, files, terminal and chat.

use super::*;
use crate::capture::Share;

mod input;
mod panels;
mod toolbar;
mod transfer;

/// Windows or programs offered for sharing, or why there are none.
type WindowList = Result<Vec<crate::capture::WindowInfo>, String>;

pub(super) type Deferred = Box<dyn FnOnce(&mut Remote, &egui::Context) -> Result<(), String>>;

/// `p` moved to the nearest corner of a pixel of the screen: a picture drawn from there maps its
/// pixels onto the screen's, where half a pixel off would blur every one of them.
fn on_pixels(p: Pos2, ppp: f32) -> Pos2 {
    Pos2::new((p.x * ppp).round() / ppp, (p.y * ppp).round() / ppp)
}

/// The host's picture at a pane's size when that is smaller: each pixel the mean of the host's
/// pixels it covers (only rows that changed are made again), drawn pixel for pixel. The host sends
/// its own pixels, which its lossless coding and scrolls keep exact; made smaller there, every
/// scroll would be resent and text would cost far more.
struct Small {
    shrink: crate::capture::Shrink<3>,
    tex: Option<TextureHandle>,
    size: (usize, usize),
}

/// Tells the panes' pictures apart in egui's keyboard focus.
static NEXT_KEYS_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A connection shown in a workspace pane: its screen, files or terminal. The workspace draws its
/// toolbar and owns full screen; the pane takes input only while it is the active one.
pub(super) struct Remote {
    pub(super) active: bool,
    /// A web app's page (`capture::browser`): a page's controls (back, forward, reload), not a
    /// computer's.
    pub(super) web: bool,
    /// The host has no screen to show (`capture::NoScreen`): the pane opens on its terminal.
    screenless: bool,
    screen_moved: bool,
    /// The pixels to a point a web page was last told (it lays itself out by them).
    density: Option<u16>,
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
    /// The picture made smaller here for a pane smaller than the host's screen, and whether the
    /// host's picture changed since.
    small: Option<Small>,
    fb_dirty: bool,
    /// The host's pointer, drawn here (the pictures leave it out): its picture, size and the
    /// pixel that points, in the host's pixels, and where the host has it (None: hidden).
    pointer_tex: Option<(TextureHandle, Vec2, Vec2)>,
    host_pointer: Option<(i16, i16)>,
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
    /// A picture from there being put on this clipboard.
    image_in: Option<std::sync::mpsc::Receiver<bool>>,
    /// The mark of the picture on this clipboard when that computer's clipboard last changed:
    /// it is older than what is there, and Ctrl+V does not paste it over it.
    pub(super) stale_picture: std::sync::Arc<std::sync::Mutex<Option<(usize, u64)>>>,
    /// Watching only: no key, click or wheel reaches the SSH host.
    view_only: bool,
    /// This computer's own screen in its own window: what is typed or clicked here would come back
    /// to this window, so none of it is sent (its windows are used directly).
    mirror: bool,
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
    /// The resolutions the shown screen can be set to, the one it has and its own (`Modes`).
    modes: crate::proto::ScreenModes,
    /// The host said them at all (one older than 0.21.0 never does).
    modes_heard: bool,
    /// The monitor the user picked, if any, to pick again after a reconnect.
    chosen_monitor: Option<u8>,
    /// Every monitor of the host was asked for, each in a window of its own (taken by the
    /// workspace, which opens them).
    pub(super) spread_asked: bool,
    /// What the person at a computer reached by its ID lets this viewer do (`grant`).
    pub(super) allowed: u8,
    /// What can be done to the computer itself and what is on (`hostctl` bits), once asked.
    pub(super) host: Option<(u8, u8)>,
    pub(super) host_asked: bool,
    /// A paste the keys of this frame already made (text or files), so the key's release does not
    /// make another.
    pub(super) paste_seen: bool,
    /// Files copied here going over to be pasted there: the batch, its uploads, and their names.
    pub(super) pasting: Option<(String, Vec<u32>, Vec<String>)>,
    /// Files dropped or pasted on a terminal of this session, going to the host's drop folder.
    pub(super) term_drops: Vec<transfer::TermDrop>,
    /// Files copied there coming over for the clipboard here: the folder, fetches left, and the
    /// paths to put on the clipboard.
    pub(super) clip_in: Option<(Vec<u32>, Vec<std::path::PathBuf>)>,
    pub(super) batches: u32,
    /// A voice call (`chat::CALL_*`), and while talking the microphone here and a player for theirs.
    pub(super) call: u8,
    call_mic: Option<crate::audio::Capture>,
    call_player: Option<crate::audio::Player>,
    /// The screen being recorded to a video file.
    pub(super) recording: Option<crate::record::Recording>,
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
    /// The picture's place in egui's keyboard focus, held while its keys go to the host.
    keys_id: egui::Id,
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
        // The pointer is drawn here where this mouse is, never a round trip behind it.
        let mut ctl = ctl;
        ctl.s.formats |= crate::proto::FORMAT_POINTER;
        Remote {
            web: sess.backend == "browser",
            screenless: sess.backend == "none",
            screen_moved: false,
            density: None,
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
            keys_id: egui::Id::unique(("remote-keys", NEXT_KEYS_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))),
            pick_pressed: false,
            pick_at: None,
            peer: crate::chat::Peer::new(label.split("  ").next().unwrap_or(&label)),
            label,
            sess,
            rx,
            ctl,
            fb: Framebuffer::default(),
            tex: None,
            small: None,
            fb_dirty: false,
            pointer_tex: None,
            host_pointer: None,
            fit: Fit::Window,
            fill_full: true,
            pan: Vec2::ZERO,
            clip: Clip::new(),
            image_in: None,
            stale_picture: Default::default(),
            share_clipboard: true,
            view_only: false,
            mirror: false,
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
            modes: (Vec::new(), (0, 0), (0, 0)),
            modes_heard: false,
            chosen_monitor: None,
            spread_asked: false,
            allowed: crate::grant::ALL,
            host: None,
            host_asked: false,
            paste_seen: false,
            pasting: None,
            term_drops: Vec::new(),
            clip_in: None,
            batches: 0,
            call: crate::chat::CALL_ENDED,
            call_mic: None,
            call_player: None,
            recording: None,
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
        // A host with no screen to show opens on its terminal (or its files, without a terminal).
        if self.screenless && self.mode() == super::workspace::PaneMode::Screen && !std::mem::replace(&mut self.screen_moved, true) {
            let to = if self.allowed & crate::grant::TERMINAL != 0 { super::workspace::PaneMode::Terminal } else { super::workspace::PaneMode::Files };
            self.set_mode(to)?;
        }
        {
            // System keys go to the remote host while its screen is the active pane of the window
            // in front, as with RustDesk's focused window; on Linux, as there, while the pointer is
            // over the picture too, so the desktop's own come back as it leaves. The options menu
            // turns this off.
            let shown = self.last_shown;
            let over = cfg!(windows) || shown.zip(ctx.pointer_hover_pos()).is_some_and(|(r, p)| r.contains(p));
            let want = self.grab_keys && !self.view_only && !self.mirror && ctx.input(|i| i.focused) && shown.is_some() && over;
            self.grab_wanted = want && self.active && self.mode() == super::workspace::PaneMode::Screen;
        }
        // Alt, Ctrl or Shift pressed alone make no key event: they reach the host as they change
        // (Alt+Tab is Alt from here, then Tab from the hook), and are let go there likewise.
        let mods = ctx.input(|i| i.modifiers);
        let here = !self.view_only
            && !self.mirror
            && self.active
            && self.mode() == super::workspace::PaneMode::Screen
            && ctx.input(|i| i.focused)
            && !self.keys_elsewhere(&ctx)
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
            && let Some(crate::clip::Copied::Text(text)) = self.clip.poll()
        {
            self.send(ToAgent::Clipboard(text))?;
        }
        // The picture as it is now goes into the recording, at most fifteen times a second; one
        // that changed just after a frame was kept goes in a moment later.
        if let Some(r) = self.recording.as_mut() {
            if r.due() {
                r.frame(&self.fb.rgb, self.fb.w, self.fb.h);
            } else if r.dirty {
                ctx.request_repaint_after(std::time::Duration::from_millis(70));
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        // This side's voice, as it comes.
        let voice: Vec<Vec<u8>> = self.call_mic.as_ref().map(|m| m.packets.try_iter().collect()).unwrap_or_default();
        for p in voice {
            self.send(ToAgent::Voice(p))?;
        }
        self.ctl.tick(&mut self.sess).map_err(|e| e.to_string())?;
        if self.files_only {
            self.feed_uploads()?;
            return self.files_view(ui);
        }
        self.feed_uploads()?;
        self.term_drop_progress();
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
            if self.web && self.density != Some((ppp * 100.0).round() as u16) {
                let d = (ppp * 100.0).round() as u16;
                self.density = Some(d);
                let _ = self.send(ToAgent::Density(d));
            }
            if self.screenless {
                // No picture will come: what there is instead, said where it would be.
                let text = "This computer's screen cannot be shown here.\nIts terminal and files work: pick them above.";
                ui.painter().text(pic_area.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(15.0), ui.visuals().weak_text_color());
                None
            } else {
                self.resize(pic_area.size() * ppp)?;
                self.edge_pan(&ctx, pic_area);
                self.picture(ui, pic_area, ppp)
            }
        };
        if let Some(r) = term_area {
            self.terminal_panel(ui, r)?;
            // Where the keys go, outlined.
            let focused = if self.term_focus { r } else { pic_area };
            ui.painter().rect_stroke(focused.shrink(0.5), CornerRadius::ZERO, egui::Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
        }
        // Files dropped on the picture are uploaded; those dropped on the terminal were its own.
        self.take_drops(&ctx)?;
        self.last_shown = shown;
        let chat_rect = match chat_area {
            Some(r) => self.chat_panel(ui, r)?,
            None => None,
        };
        let chat_rect = chat_rect.or(term_area);
        self.upload_overlay(&ctx, screen);
        self.tell_settled();
        self.toasts.draw(&ctx, screen, &[], None);
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
        let keys = !(self.view_only || self.mirror || inline || !self.active);
        let term_keys = split && self.term_focus;
        self.hold_keys(ui, pic_area, keys && !term_keys && shown.is_some());
        if !keys {
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

    /// That computer's clipboard changed: the picture on this one now is older than it.
    fn copied_there(&mut self) {
        let stale = self.stale_picture.clone();
        std::thread::spawn(move || {
            let m = crate::clip::image_mark();
            *stale.lock().unwrap_or_else(|e| e.into_inner()) = m;
        });
    }

    pub(super) fn pump(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let start = Instant::now();
        // A picture from there, on this clipboard now (or not).
        if let Some(rx) = &self.image_in {
            match rx.try_recv() {
                Ok(ok) => {
                    self.image_in = None;
                    self.copied_there();
                    self.notify(if ok { "A picture copied there is on this computer's clipboard" } else { "A picture copied there could not be put on this computer's clipboard" });
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.image_in = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
        loop {
            // A flood of messages waits for the next frame rather than keep this one from drawing.
            if start.elapsed() > PUMP_BUDGET {
                ctx.request_repaint();
                return Ok(());
            }
            match self.rx.try_recv() {
                Ok(Ev::Net(ToClient::Frame(f))) => {
                    let resized = self.fb.apply(&f).map_err(|e| e.to_string())?;
                    if let Some(r) = self.recording.as_mut() {
                        r.dirty = true;
                    }
                    self.upload(ctx, resized, &f);
                    self.ctl.frame_done(f.covered() * 2 >= self.fb.w * self.fb.h);
                    self.send(ToAgent::Ack(f.id))?;
                    if std::mem::take(&mut self.fb.need_key) {
                        self.send(ToAgent::Refresh)?;
                    }
                }
                Ok(Ev::Net(ToClient::Pong(t))) => self.ctl.pong(t),
                Ok(Ev::Net(ToClient::PointerShape { w, h, hot_x, hot_y, rgba })) => {
                    let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    let size = Vec2::new(w as f32, h as f32);
                    let hot = Vec2::new(hot_x as f32, hot_y as f32);
                    match self.pointer_tex.as_mut() {
                        Some((t, s, o)) => {
                            t.set(img, TextureOptions::LINEAR);
                            (*s, *o) = (size, hot);
                        }
                        None => self.pointer_tex = Some((ctx.load_texture("remote-pointer", img, TextureOptions::LINEAR), size, hot)),
                    }
                    ctx.request_repaint();
                }
                Ok(Ev::Net(ToClient::PointerAt(at))) => {
                    self.host_pointer = at;
                    ctx.request_repaint();
                }
                Ok(Ev::Net(ToClient::Voice(p))) => {
                    if let Some(pl) = self.call_player.as_mut() {
                        pl.push(&p);
                    }
                }
                Ok(Ev::Net(ToClient::CallState(state))) => self.call_now(state),
                Ok(Ev::Net(ToClient::Audio(p))) => {
                    if let Some(pl) = self.sound.as_mut() {
                        pl.push(&p);
                    }
                }
                Ok(Ev::Net(ToClient::Notice(t))) => self.notify(&t),
                Ok(Ev::Net(ToClient::Allowed(bits))) => self.allowed = bits,
                Ok(Ev::Net(ToClient::HostState { can, on })) => self.host = Some((can, on)),
                Ok(Ev::Net(ToClient::ClipFiles(list))) => {
                    self.copied_there();
                    if self.share_clipboard && self.active {
                        self.files_copied_there(list)?;
                    }
                }
                Ok(Ev::Net(ToClient::FetchDone { tag })) => {
                    if let Some((left, paths)) = self.clip_in.as_mut() {
                        left.retain(|t| *t != tag);
                        if left.is_empty() {
                            let paths = std::mem::take(paths);
                            self.clip_in = None;
                            let n = paths.len();
                            match crate::clip::put_files_here(&paths) {
                                Ok(()) => self.notify(&format!("{n} {} copied there now on the clipboard here: paste them in a folder", if n == 1 { "item" } else { "items" })),
                                Err(e) => self.notify(&format!("The files copied there could not go on the clipboard here: {e}")),
                            }
                        }
                    }
                }
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
                Ok(Ev::Net(ToClient::Modes { list, current, own })) => {
                    self.modes = (list, current, own);
                    self.modes_heard = true;
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
                    self.copied_there();
                }
                // A picture copied there (a screenshot): on this clipboard, to paste here; one too
                // large to come over comes empty.
                Ok(Ev::Net(ToClient::ClipImage(png))) => {
                    if png.is_empty() || !(self.share_clipboard && self.active) {
                        self.copied_there();
                    } else {
                        self.image_in = Some(self.clip.set_image_later(png));
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
                Ok(Ev::Net(ToClient::Launched { text, .. })) => self.notify(&text),
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
                    let for_clipboard = self.clip_in.as_ref().is_some_and(|(left, _)| left.contains(&tag));
                    match self.inbox.end(id, ok, &text) {
                        Some(crate::files::Note::Done { ok: true, .. }) if for_clipboard => {}
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
                    // Part of a terminal's drop: once all of it is there, its paths are typed.
                    if self.term_drop_done(id, ok, &text) {
                        continue;
                    }
                    // Part of a paste: once all of it is there, it goes on the clipboard there and is pasted.
                    if let Some((batch, left, names)) = self.pasting.as_mut()
                        && left.contains(&id)
                    {
                        left.retain(|u| *u != id);
                        self.uploads.retain(|u| u.id != id);
                        if !ok {
                            self.pasting = None;
                            self.notify(&format!("The paste failed: {text}"));
                        } else if left.is_empty() {
                            let (batch, names) = (batch.clone(), std::mem::take(names));
                            self.pasting = None;
                            self.send(ToAgent::PasteFiles { batch, names })?;
                        }
                        continue;
                    }
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

    /// Asks the agent for a capture that fits the window, once resizing has settled.
    pub(super) fn resize(&mut self, px: Vec2) -> Result<(), String> {
        let want = match self.fit {
            // The host's own pixels, made smaller here when the pane is (`Small`); only a page,
            // laid out at the size it is shown at, gets the pane's.
            _ if !self.web => self.sess.desk,
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

    /// The host's pointer over the picture at `rect`: where this mouse is while it moves the
    /// host's (this window's own arrow hidden there), else where the host has it.
    fn draw_pointer(&self, ui: &egui::Ui, painter: &egui::Painter, rect: Rect) {
        let Some((tex, size, hot)) = &self.pointer_tex else { return };
        let ctx = ui.ctx();
        let k = rect.width() / self.sess.desk.0.max(1) as f32;
        let here = ctx.pointer_hover_pos().filter(|&p| {
            rect.contains(p) && self.active && !self.view_only() && ctx.layer_id_at(p).is_none_or(|l| l.order == egui::Order::Background) && !ctx.any_popup_open()
        });
        let at = match (here, self.host_pointer) {
            (Some(p), _) => {
                ctx.set_cursor_icon(egui::CursorIcon::None);
                p
            }
            (None, Some((x, y))) => rect.min + Vec2::new(x as f32, y as f32) * k,
            (None, None) => return,
        };
        let r = Rect::from_min_size(at - *hot * k, *size * k);
        painter.image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    }

    /// The picture at `w` x `h`, made anew where the host's changed.
    fn small_texture(&mut self, ctx: &egui::Context, w: usize, h: usize) -> Option<egui::TextureId> {
        if self.fb.w == 0 || self.fb.rgb.len() < self.fb.w * self.fb.h * 3 {
            return None;
        }
        let fresh = self.small.as_ref().is_none_or(|s| s.size != (w, h));
        if fresh {
            self.small = Some(Small { shrink: Default::default(), tex: None, size: (w, h) });
        }
        let s = self.small.as_mut()?;
        if fresh || std::mem::take(&mut self.fb_dirty) {
            s.shrink.run(&self.fb.rgb, self.fb.w, self.fb.h, self.fb.w * 3, w, h);
            let out = s.shrink.result();
            match (&mut s.tex, s.shrink.drawn) {
                (Some(t), Some((y0, y1))) if !fresh => {
                    t.set_partial([0, y0], ColorImage::from_rgb([w, y1 - y0], &out[y0 * w * 3..y1 * w * 3]), TextureOptions::LINEAR)
                }
                (Some(_), None) if !fresh => {}
                _ => s.tex = Some(ctx.load_texture("remote-small", ColorImage::from_rgb([w, h], out), TextureOptions::LINEAR)),
            }
        }
        s.tex.as_ref().map(|t| t.id())
    }

    /// Draws the remote screen; returns where it is, in points.
    pub(super) fn picture(&mut self, ui: &mut egui::Ui, area: Rect, ppp: f32) -> Option<Rect> {
        let tex = self.tex.as_ref()?;
        let size = Vec2::new(self.fb.w as f32, self.fb.h as f32) / ppp;
        let rect = match self.fit {
            Fit::Window if self.fill_full && self.filling => area,
            Fit::Window => {
                let mut k = (area.width() / size.x).min(area.height() / size.y);
                // The agent sends the pane's size: within a pixel of it, the picture is drawn at
                // its own size, pixel for pixel, never blurred by a resampling of a hair.
                if ((k - 1.0) * size.x.max(size.y) * ppp).abs() < 1.0 {
                    k = 1.0;
                }
                let s = size * k;
                Rect::from_min_size(on_pixels(area.center() - s / 2.0, ppp), s)
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
                Rect::from_min_size(on_pixels(min, ppp), full)
            }
        };
        // Smaller than the host's pixels: made smaller here, drawn pixel for pixel.
        let px = (rect.size() * ppp).round();
        let (w, h) = (px.x as usize, px.y as usize);
        let (id, rect) = if w > 0 && h > 0 && w <= self.fb.w && h <= self.fb.h && (w, h) != (self.fb.w, self.fb.h) {
            let id = tex.id();
            (self.small_texture(ui.ctx(), w, h).unwrap_or(id), Rect::from_min_size(rect.min, px / ppp))
        } else {
            (tex.id(), rect)
        };
        let painter = ui.painter().with_clip_rect(area);
        painter.image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        self.draw_pointer(ui, &painter, rect);
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
        self.view_only || self.mirror
    }

    /// It shows this computer's own screen (`mirror`): its keys stay this window's.
    pub(super) fn set_mirror(&mut self) {
        self.mirror = true;
    }

    pub(super) fn mirror(&self) -> bool {
        self.mirror
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
    pub(in crate::gui) fn preset_name(&self) -> String {
        self.ctl.preset.clone()
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
    /// Whether it shows a web app's page.
    pub(super) fn is_web(&self) -> bool {
        self.web
    }

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

    /// The host's monitors (its cameras apart), and the one shown here.
    pub(super) fn monitors(&self) -> (usize, usize) {
        (self.monitors.iter().filter(|m| !m.camera()).count(), self.monitor)
    }

    /// What the user chose here, for another connection to the host that shows its monitor `m`.
    pub(super) fn prefs_on(&self, m: u8) -> Prefs {
        let mut p = self.prefs();
        p.monitor = Some(m);
        p
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

}

/// Every toolbar action of the remote screen, clicked as a user would, against the built-in test
/// desktop: cargo build --release, then
/// SSHREMOTE_EXE=target/release/sshremote.exe cargo test --release -- --ignored --test-threads=1 toolbar_actions
#[cfg(test)]
mod actions;

impl Remote {
    /// Where the call is now, as the computer says: answered (the microphone and speakers here
    /// join), declined or ended (they let go).
    pub(super) fn call_now(&mut self, state: u8) {
        use crate::chat::{CALL_DECLINED, CALL_ENDED, CALL_TALKING};
        let was = self.call;
        self.call = state;
        if state == CALL_TALKING {
            match crate::audio::microphone() {
                Ok(m) => self.call_mic = Some(m),
                Err(e) => self.notify(&format!("The microphone here: {e}; you can still hear them")),
            }
            match crate::audio::Player::new() {
                Ok(p) => self.call_player = Some(p),
                Err(e) => self.notify(&format!("The speakers here: {e}")),
            }
            self.notify("In a call with the person at that computer");
        } else if state == CALL_DECLINED || state == CALL_ENDED {
            (self.call_mic, self.call_player) = (None, None);
            if state == CALL_DECLINED {
                self.notify("They did not answer the call");
            } else if was == CALL_TALKING {
                self.notify("The call ended");
            }
        }
    }
}

impl Remote {
    /// Starts recording the screen to a video file, or stops and says where it went.
    pub(super) fn toggle_recording(&mut self) {
        match self.recording.take() {
            None => {
                self.recording = Some(crate::record::Recording::start(self.host()));
                self.notify("Recording the screen (at most 15 frames a second, only as it changes)");
            }
            Some(r) => match r.stop() {
                Ok(files) => {
                    let list: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
                    self.notify(&format!("Recorded: {}", list.join(", ")));
                }
                Err(e) => self.notify(&format!("The recording failed: {e}")),
            },
        }
    }
}
