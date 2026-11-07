//! A connected host: its screen, the toolbar, input, files, terminal and chat.

use super::*;
use crate::capture::Share;

/// Windows or programs offered for sharing, or why there are none.
type WindowList = Result<Vec<crate::capture::WindowInfo>, String>;

pub(super) type Deferred = Box<dyn FnOnce(&mut Remote, &egui::Context) -> Result<(), String>>;

pub(super) struct Remote {
    sess: Session,
    rx: mpsc::Receiver<Ev>,
    ctl: Control,
    fb: Framebuffer,
    tex: Option<TextureHandle>,
    label: String,
    fit: Fit,
    /// How far a fixed-size picture is scrolled, in points.
    pan: Vec2,
    fullscreen: bool,
    pinned: bool,
    /// Where the toolbar sits along the top edge, 0 left to 1 right; dragged by its grip.
    bar_x: f32,
    /// The toolbar's width last frame, to keep it on screen.
    bar_w: f32,
    /// The hidden toolbar stays out until then: after the top edge or the toolbar was last under the pointer.
    bar_until: Instant,
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
    /// Notifications at the bottom right: text, whether it is a warning, when it came.
    toasts: Vec<(String, bool, Instant)>,
    grab_keys: bool,
    uploads: std::collections::VecDeque<Upload>,
    next_upload: u32,
    sound: Option<crate::audio::Player>,
    /// Open chat panel: messages (from the remote side?, text) and the line being typed.
    chat: Option<(Vec<(bool, String)>, String)>,
    /// Messages kept while the panel is closed, and how many are unread.
    chat_lines: Vec<(bool, String)>,
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
    monitors: Vec<crate::proto::MonitorInfo>,
    monitor: usize,
    inbox: crate::files::Inbox,
    /// Downloads in progress: id, name and the request tag (0 for the Downloads folder).
    downloads: Vec<(u32, String, u32)>,
    /// The file transfer window, while open.
    files: Option<crate::filewin::FileWindow>,
    /// The terminal window, while open.
    terms: Option<crate::termwin::TermWindow>,
    /// The terminal takes the place of the remote screen in this window.
    term_inline: bool,
    /// The terminal has a window of its own instead.
    term_detached: bool,
    /// A connection made for its files: the window is the file manager alone, with no screen.
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
}

/// A file being sent to the remote host.
struct Upload {
    id: u32,
    name: String,
    /// The remote folder chosen in the file window; `None` for the Downloads folder.
    dir: Option<String>,
    cancelled: bool,
    file: std::fs::File,
    size: u64,
    sent: u64,
    acked: u64,
    ended: bool,
}

/// Unacknowledged upload bytes; more would only queue in ssh and delay the screen.
const UPLOAD_WINDOW: u64 = 1 << 20;
const UPLOAD_CHUNK: usize = 64 << 10;

impl Remote {
    pub(super) fn new(
        sess: Session,
        rx: mpsc::Receiver<Ev>,
        ctl: Control,
        label: String,
        ssh_cmd: Option<Vec<String>>,
        terminal: bool,
    ) -> Self {
        // The conversation with this host so far, from earlier sessions.
        let chat_lines = crate::chat::load_history(label.split("  ").next().unwrap_or(&label));
        Remote {
            terms: terminal.then(|| crate::termwin::TermWindow::new(ssh_cmd.clone())),
            term_inline: terminal,
            term_detached: false,
            files_only: false,
            picker: None,
            sharing: (0, String::new()),
            click_pick: false,
            last_shown: None,
            pick_pressed: false,
            pick_at: None,
            label,
            sess,
            rx,
            ctl,
            fb: Framebuffer::default(),
            tex: None,
            fit: Fit::Window,
            pan: Vec2::ZERO,
            fullscreen: false,
            pinned: true,
            bar_x: crate::hosts::setting("toolbar_x").and_then(|v| v.parse().ok()).unwrap_or(0.5),
            bar_w: 600.0,
            bar_until: Instant::now(),
            clip: Clip::new(),
            share_clipboard: true,
            view_only: false,
            held: BTreeSet::new(),
            mods: Modifiers::NONE,
            pointer: (u16::MAX, u16::MAX),
            view: (0, 0),
            view_changed: None,
            win_next: false,
            toasts: Vec::new(),
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
            monitors: Vec::new(),
            monitor: 0,
            inbox: crate::files::Inbox::default(),
            downloads: Vec::new(),
            files: None,
            fetch_dirs: Default::default(),
            next_tag: 1,
            ssh_cmd,
        }
    }

    /// One frame of the connected window, with the system keys taken from Windows; on an error the
    /// session is closed and the window returns to the home screen.
    pub(super) fn show(&mut self, ui: &mut egui::Ui, grabbed: Vec<(u32, bool)>) -> Result<(), String> {
        let ctx = ui.ctx().clone();
        #[cfg(windows)]
        {
            // As in RustDesk: system keys go to the remote host while the pointer is over its picture.
            let over = ctx.pointer_hover_pos().is_some_and(|p| self.last_shown.is_some_and(|s| s.contains(p)));
            crate::keygrab::set_active(self.grab_keys && !self.view_only && ctx.input(|i| i.focused) && over);
        }
        let result = grabbed
            .into_iter()
            .try_for_each(|(keysym, down)| self.send(ToAgent::KeyState { keysym, down }))
            .and_then(|_| self.frame(ui));
        if result.is_err() {
            self.sess.close();
            if self.fullscreen {
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
            }
            ctx.send_viewport_cmd(ViewportCommand::Title("SSH Remote".into()));
        }
        result
    }

    /// For a connection made for its files: the file window open and the screen paused, so
    /// almost no picture is sent until it is resumed.
    /// For a connection made for its files: the window shows the file manager alone, and the
    /// remote screen is paused, so no picture is sent unless "Show the screen" is chosen.
    pub(super) fn open_files_only(&mut self) -> Result<(), String> {
        self.files = Some(crate::filewin::FileWindow::new());
        self.send(ToAgent::ListDir { path: String::new() })?;
        self.files_only = true;
        self.paused = true;
        self.send(ToAgent::Pause(true))
    }

    /// The file manager filling the window, under a bar with the host, the screen and disconnect.
    fn files_view(&mut self, ui: &mut egui::Ui) -> Result<(), String> {
        let host = self.host().to_string();
        let mut show_screen = false;
        let mut disconnect = false;
        let bar = super::chrome::title_bar(ui, |ui| {
            let dot = match self.ctl.rtt {
                r if r < 80.0 => GOOD,
                r if r < 250.0 => WARN,
                _ => BAD,
            };
            let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, dot);
            ui.label(RichText::new(format!("{}  Files on {host}", ic::FOLDER)).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let flat = |text: String| egui::Button::new(text).frame_when_inactive(false).min_size(Vec2::new(0.0, 28.0));
                if ui.add(flat(format!("{}  Disconnect", ic::LINK_BREAK))).clicked() {
                    disconnect = true;
                }
                if ui
                    .add(flat(format!("{}  Show the screen", ic::MONITOR)))
                    .on_hover_text("Open the remote screen; the file manager keeps a window of its own")
                    .clicked()
                {
                    show_screen = true;
                }
            });
        });
        if bar.close {
            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
        }
        let transfers = self.transfers();
        let cmds = egui::CentralPanel::default()
            .show(ui, |ui| self.files.as_mut().map(|f| f.embedded(ui, &host, &transfers)).unwrap_or_default())
            .inner;
        for c in cmds {
            self.file_cmd(c)?;
        }
        if show_screen {
            self.files_only = false;
            self.paused = false;
            self.send(ToAgent::Pause(false))?;
        }
        if disconnect {
            return Err("disconnected".into());
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
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
        let l = text.to_lowercase();
        let warn =
            ["fail", "error", "unavailable", "cannot", "could not", "denied", "sound:"].iter().any(|w| l.contains(w));
        self.toasts.push((text.to_string(), warn, Instant::now()));
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    /// One UI pass: network, input, picture and toolbar. An error ends the session.
    pub(super) fn frame(&mut self, ui: &mut egui::Ui) -> Result<(), String> {
        let ctx = ui.ctx().clone();
        self.pump(&ctx)?;
        if self.share_clipboard
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
        // In a window the toolbar sits in the title bar, beside the window's buttons; in full screen
        // it floats over the top edge as a dock.
        let docked = !self.fullscreen;
        let mut bar_action: Option<Deferred> = None;
        if docked {
            let bar = super::chrome::title_bar(ui, |ui| self.toolbar_items(ui, &mut bar_action));
            if bar.close {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
        let screen = if docked { ui.available_rect_before_wrap() } else { ui.max_rect() };
        let ppp = ctx.pixels_per_point();
        // A pinned dock gets its own strip instead of covering the top of the remote screen.
        let strip = if docked { 0.0 } else { 42.0 };
        let area = if self.pinned { screen.with_min_y(screen.top() + strip) } else { screen };
        ui.painter().rect_filled(screen, CornerRadius::ZERO, Color32::from_rgb(18, 19, 23));
        ui.painter().rect_filled(area, CornerRadius::ZERO, Color32::BLACK);
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
        let toolbar_rect = if docked { None } else { self.toolbar(&ctx, screen)? };
        if let Some(f) = bar_action {
            f(self, &ctx)?;
        }
        let chat_rect = match chat_area {
            Some(r) => self.chat_panel(ui, r)?,
            None => None,
        };
        let chat_rect = chat_rect.or(term_area);
        self.upload_overlay(&ctx, screen);
        self.draw_toasts(&ctx, screen);
        self.file_window(&ctx)?;
        self.term_window(&ctx)?;
        self.share_picker(&ctx)?;
        if self.pick_at.is_some_and(|t| Instant::now() >= t) {
            self.pick_at = None;
            self.send(ToAgent::Share(Share::ActiveWindow))?;
        }
        if self.paused || self.view_only {
            // A badge on the picture: it is a still image until resumed, or input is off.
            let text = if self.paused {
                format!("{}  Paused   ·   {} resumes, {} fetches one frame", ic::PAUSE, ic::PLAY, ic::ARROW_CLOCKWISE)
            } else {
                format!("{}  View only   ·   turn it off in Options {}", ic::EYE, ic::GEAR)
            };
            let painter =
                ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("paused-badge")));
            let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), Color32::WHITE);
            let r = Rect::from_min_size(
                Pos2::new(screen.left() + 12.0, screen.bottom() - galley.size().y - 30.0),
                galley.size() + Vec2::new(24.0, 14.0),
            );
            painter.rect_filled(r, CornerRadius::same(crate::gui::theme::R_MD), Color32::from_rgba_unmultiplied(20, 22, 28, 220));
            painter.galley(r.min + Vec2::new(12.0, 7.0), galley, Color32::WHITE);
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
        let ui_rects: Vec<Rect> = toolbar_rect.into_iter().chain(chat_rect).collect();
        if self.view_only || inline {
            self.release_all()?;
        } else {
            self.input(&ctx, shown, &ui_rects)?;
        }
        let toggle = ctx.input_mut(|i| i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::ALT, Key::Enter));
        if toggle {
            self.set_fullscreen(&ctx, !self.fullscreen);
        }
        // Network messages wake the window themselves; this only keeps statistics and pings going.
        ctx.request_repaint_after(Duration::from_secs(1));
        Ok(())
    }

    pub(super) fn pump(&mut self, ctx: &egui::Context) -> Result<(), String> {
        loop {
            match self.rx.try_recv() {
                Ok(Ev::Net(ToClient::Frame(f))) => {
                    let resized = self.fb.apply(&f).map_err(|e| e.to_string())?;
                    self.upload(ctx, resized, &f);
                    self.ctl.frame_done(f.covered() * 2 >= self.fb.w * self.fb.h);
                    self.send(ToAgent::Ack(f.id))?;
                }
                Ok(Ev::Net(ToClient::Pong(t))) => self.ctl.pong(t),
                Ok(Ev::Net(ToClient::Audio(p))) => {
                    if let Some(pl) = self.sound.as_mut() {
                        pl.push(&p);
                    }
                }
                Ok(Ev::Net(ToClient::Notice(t))) => self.notify(&t),
                Ok(Ev::Net(ToClient::Snapshot(png))) => {
                    let path = crate::shot::save_path();
                    let msg = match std::fs::write(&path, &png) {
                        Ok(()) => format!("Screenshot saved to {}", path.display()),
                        Err(e) => format!("Screenshot: {e}"),
                    };
                    self.notify(&msg);
                }
                Ok(Ev::Net(ToClient::Chat(t))) => {
                    crate::chat::append_history(self.host(), true, &t);
                    match self.chat.as_mut() {
                        Some((lines, _)) => lines.push((true, t)),
                        None => {
                            self.notify(&format!("Message: {t}"));
                            self.chat_lines.push((true, t));
                            self.unread += 1;
                        }
                    }
                }
                Ok(Ev::Net(ToClient::Monitors { list, current })) => {
                    (self.monitors, self.monitor) = (list, current as usize)
                }
                Ok(Ev::Net(ToClient::Hello { w, h, backend })) => {
                    // Another monitor: new size, and the capture must be fitted again.
                    self.sess.desk = (w, h);
                    let host = self.label.split("  ").next().unwrap_or("").to_string();
                    self.label = format!("{host}  {w}x{h}  {backend}");
                    self.view = (0, 0);
                }
                Ok(Ev::Net(ToClient::Clipboard(t))) => {
                    if self.share_clipboard {
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
                    if let Some(crate::files::Note::Done { text, .. }) = self.inbox.start_in(id, size, &dir, &name) {
                        self.notify(&format!("{name}: {text}"));
                    } else {
                        self.downloads.push((id, name, tag));
                    }
                }
                Ok(Ev::Net(ToClient::FetchData { id, data })) => match self.inbox.data(id, &data) {
                    Some(crate::files::Note::Progress { id, received }) => {
                        self.send(ToAgent::FetchAck { id, received })?
                    }
                    Some(crate::files::Note::Done { text, .. }) => {
                        self.downloads.retain(|d| d.0 != id);
                        self.notify(&text);
                    }
                    None => {}
                },
                Ok(Ev::Net(ToClient::FetchEnd { id, ok, text })) => {
                    if id != 0 && !self.downloads.iter().any(|d| d.0 == id) {
                        // Already cancelled here.
                        continue;
                    }
                    let name = self.downloads.iter().find(|d| d.0 == id).map(|d| d.1.clone()).unwrap_or_default();
                    let tagged = self.downloads.iter().any(|d| d.0 == id && d.2 != 0);
                    self.downloads.retain(|d| d.0 != id);
                    let msg = match self.inbox.end(id, ok, &text) {
                        Some(crate::files::Note::Done { ok: true, text, .. }) => format!("Downloaded {name} to {text}"),
                        Some(crate::files::Note::Done { text, .. }) => format!("{name} failed: {text}"),
                        _ => format!("Download failed: {text}"),
                    };
                    // After the file is closed, so the listing shows its full size.
                    if tagged && let Some(f) = self.files.as_mut() {
                        f.refresh_local();
                    }
                    self.notify(&msg);
                }
                Ok(Ev::Net(ToClient::FileDone { id, ok, text })) => {
                    let name = self.uploads.iter().find(|u| u.id == id).map(|u| u.name.clone()).unwrap_or_default();
                    let cancelled = self.uploads.iter().any(|u| u.id == id && u.cancelled);
                    let into_folder = self.uploads.iter().any(|u| u.id == id && u.dir.is_some());
                    self.uploads.retain(|u| u.id != id);
                    if into_folder && let Some(f) = self.files.as_ref() {
                        let cmd = f.refresh_remote();
                        self.file_cmd(cmd)?;
                    }
                    let msg = if cancelled {
                        format!("{name}: upload cancelled")
                    } else if ok {
                        format!("{name} saved on the SSH host: {text}")
                    } else {
                        format!("{name} failed: {text}")
                    };
                    self.notify(&msg);
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
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                self.notify(&format!("{name}: {e}"));
                return Ok(());
            }
        };
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let id = self.next_upload;
        self.next_upload += 1;
        self.send(ToAgent::FileStart { id, size, dir: dir.clone().unwrap_or_default(), name: name.clone() })?;
        self.uploads.push_back(Upload { id, name, dir, cancelled: false, file, size, sent: 0, acked: 0, ended: false });
        Ok(())
    }

    /// Sends the next chunks of the first unfinished upload, keeping a bounded amount unacknowledged.
    pub(super) fn feed_uploads(&mut self) -> Result<(), String> {
        use std::io::Read;
        let Some(i) = self.uploads.iter().position(|u| !u.ended) else { return Ok(()) };
        let mut buf = vec![0u8; UPLOAD_CHUNK];
        loop {
            let u = &mut self.uploads[i];
            if u.sent - u.acked.min(u.sent) >= UPLOAD_WINDOW {
                return Ok(());
            }
            let n = u.file.read(&mut buf).map_err(|e| format!("{}: {e}", u.name))?;
            let id = u.id;
            if n == 0 {
                u.ended = true;
                return self.send(ToAgent::FileEnd { id });
            }
            u.sent += n as u64;
            self.send(ToAgent::FileData { id, data: buf[..n].to_vec() })?;
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
        let w = self.chat_w.clamp(260.0, (area.width() * 0.6).max(260.0));
        let panel = area.with_min_x(area.right() - w);
        let handle = Rect::from_min_max(Pos2::new(panel.left() - 3.0, area.top()), Pos2::new(panel.left() + 3.0, area.bottom()));
        let r = ui.interact(handle, egui::Id::unique("chat-splitter"), egui::Sense::drag());
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
        let r = ui.interact(handle, egui::Id::unique("terminal-splitter"), egui::Sense::drag());
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
            .show(&mut panel, |ui| crate::chat::show_lines_since(ui, lines, earlier, "Remote", "You"));
        panel.add_space(4.0);
        sent = crate::chat::input_row(&mut panel, input, true).or(sent);
        if clear {
            crate::chat::clear_history(&host);
            lines.clear();
            self.chat_earlier = 0;
        }
        if let Some(text) = sent {
            if let Some((lines, _)) = self.chat.as_mut() {
                lines.push((false, text.clone()));
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

    pub(super) fn file_window(&mut self, ctx: &egui::Context) -> Result<(), String> {
        if self.files.is_none() {
            return Ok(());
        }
        let transfers = self.transfers();
        let Some(mut f) = self.files.take() else { return Ok(()) };
        let host = self.host().to_string();
        let cmds = f.show(ctx, &host, &transfers);
        self.files = Some(f);
        for c in cmds {
            self.file_cmd(c)?;
        }
        Ok(())
    }

    /// Chooses what the remote host shares, as in a video call: a screen, a window or a program.
    pub(super) fn share_picker(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let Some((list, tab)) = self.picker.as_mut() else { return Ok(()) };
        let mut pick: Option<ToAgent> = None;
        let mut close = false;
        let mut click_pick = false;
        let monitors = self.monitors.clone();
        let current = self.monitor;
        let modal = egui::Modal::new(egui::Id::unique("sshremote-share-picker")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.set_min_height(440.0);
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
                let fill = if on {
                    ACCENT.gamma_multiply(0.3)
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
            egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(330.0).show(ui, |ui| match *tab {
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
            // The install answer comes back as the list's message, so the chooser stays open for it.
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
        t.keys_on = true;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area).id_salt("inline-terminal"));
        let cmds = t.embedded(&mut child);
        self.terms = Some(t);
        for c in cmds {
            match c {
                crate::termwin::Cmd::Send(m) => self.send(m)?,
                crate::termwin::Cmd::Close => {
                    self.terms = None;
                    self.term_inline = false;
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
                if let Some(u) = self.uploads.iter_mut().find(|u| u.id == id) {
                    u.cancelled = true;
                    if !u.ended {
                        u.ended = true;
                        self.send(ToAgent::FileEnd { id })?;
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
            Cmd::Close => self.files = None,
        }
        Ok(())
    }

    /// Stacked notifications at the bottom right that fade out after a few seconds.
    pub(super) fn draw_toasts(&mut self, ctx: &egui::Context, screen: Rect) {
        self.toasts.retain(|t| t.2.elapsed() < Duration::from_secs(6));
        let mut y = screen.bottom() - 16.0;
        for (i, (text, warn, at)) in self.toasts.iter().enumerate().rev() {
            let fade = (1.0 - (at.elapsed().as_secs_f32() - 5.0).max(0.0)).clamp(0.0, 1.0);
            let id = egui::Id::unique(("toast", i));
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, id));
            let galley = painter.layout(text.clone(), FontId::proportional(14.0), Color32::WHITE, 380.0);
            let size = galley.size() + Vec2::new(30.0, 20.0);
            let rect = Rect::from_min_size(Pos2::new(screen.right() - 16.0 - size.x, y - size.y), size);
            painter.rect_filled(
                rect,
                CornerRadius::same(crate::gui::theme::R_MD),
                Color32::from_rgba_unmultiplied(32, 34, 41, (235.0 * fade) as u8),
            );
            let bar = Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height()));
            painter.rect_filled(bar, CornerRadius::same(2), (if *warn { WARN } else { ACCENT }).gamma_multiply(fade));
            painter.galley(rect.min + Vec2::new(18.0, 10.0), galley, Color32::WHITE.gamma_multiply(fade));
            y = rect.top() - 8.0;
        }
        if let Some(next) = self.toasts.iter().map(|t| t.2).min() {
            // Steady until the last second, which fades.
            let until_fade = Duration::from_secs(5).saturating_sub(next.elapsed());
            ctx.request_repaint_after(if until_fade.is_zero() { Duration::from_millis(50) } else { until_fade });
        }
    }

    /// Progress of uploads and the drop hint, bottom left over the remote screen.
    pub(super) fn upload_overlay(&self, ctx: &egui::Context, screen: Rect) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        let mut lines: Vec<String> = self
            .uploads
            .iter()
            .map(|u| format!("Sending {}  {}%", u.name, (u.acked * 100).checked_div(u.size).unwrap_or(100)))
            .collect();
        for (id, name, _) in &self.downloads {
            if let Some((got, size)) = self.inbox.received(*id) {
                lines.push(format!("Receiving {name}  {}%", (got * 100).checked_div(size).unwrap_or(100)));
            }
        }
        if hovering {
            lines.push("Drop to send to the remote Downloads folder".into());
        }
        if lines.is_empty() {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("uploads")));
        let text = lines.join(
            "
",
        );
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
                Event::Copy if !local => self.tap('c' as u32)?,
                Event::Cut if !local => self.tap('x' as u32)?,
                Event::Paste(_) if !local => self.tap('v' as u32)?,
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
                Event::PointerButton { pos, button, pressed, .. } => {
                    if pressed && (over_toolbar(pos) || local || (!self.fullscreen && super::chrome::on_edge(ctx, pos)))
                    {
                        continue;
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
                Event::MouseWheel { unit, delta, .. } => {
                    let Some(pos) = ctx.pointer_hover_pos() else { continue };
                    if over_toolbar(pos) || !shown.is_some_and(|r| r.contains(pos)) {
                        continue;
                    }
                    let lines = match unit {
                        egui::MouseWheelUnit::Point => delta / 40.0,
                        egui::MouseWheelUnit::Line => delta,
                        egui::MouseWheelUnit::Page => delta * 10.0,
                    };
                    let (sy, sx) = (-lines.y.round() as i32, -lines.x.round() as i32);
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

    pub(super) fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        self.pinned = !on;
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(on));
    }

    /// The floating toolbar; returns its rectangle while shown.
    /// Moves the toolbar along the top edge by `dx` points; remembered when the drag ends.
    pub(super) fn drag_bar(&mut self, ctx: &egui::Context, r: &egui::Response, screen: Rect) {
        if r.dragged() {
            self.bar_x = (self.bar_x + r.drag_delta().x / screen.width().max(1.0)).clamp(0.0, 1.0);
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if r.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
        if r.drag_stopped() {
            crate::hosts::set_setting("toolbar_x", &format!("{:.3}", self.bar_x));
        }
    }

    /// The toolbar's left edge for its remembered place, kept inside the window.
    pub(super) fn bar_left(&self, screen: Rect, width: f32) -> f32 {
        let centre = screen.left() + self.bar_x * screen.width();
        (centre - width / 2.0).clamp(screen.left() + 4.0, (screen.right() - width - 4.0).max(screen.left() + 4.0))
    }

    /// The toolbar; unpinned it is hidden entirely and comes back while the pointer is at the top
    /// edge of the window, as in Remote Desktop's full screen. Returns the toolbar's rectangle.
    pub(super) fn toolbar(&mut self, ctx: &egui::Context, screen: Rect) -> Result<Option<Rect>, String> {
        let id = egui::Id::unique("toolbar");
        if !self.pinned
            && ctx.pointer_hover_pos().is_some_and(|p| p.y <= screen.top() + 6.0 && screen.x_range().contains(p.x))
        {
            self.bar_until = Instant::now() + Duration::from_millis(700);
        }
        let shown_a_while = Instant::now() < self.bar_until;
        if !(self.pinned || shown_a_while || menu_open(ctx) || ctx.any_popup_open()) {
            ctx.request_repaint_after(Duration::from_millis(100));
            return Ok(None);
        }
        let mut action: Option<Deferred> = None;
        let left = self.bar_left(screen, self.bar_w);
        let mut grip: Option<egui::Response> = None;
        let resp = egui::Area::new(id)
            .fixed_pos(Pos2::new(left, screen.top() + 4.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(ctx.global_style().visuals.window_fill)
                    .stroke(ctx.global_style().visuals.window_stroke)
                    .corner_radius(CornerRadius::same(crate::gui::theme::R_MD))
                    .shadow(egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(90) })
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let g = ui.add(
                                egui::Label::new(RichText::new(ic::GRIP).size(16.0).weak())
                                    .sense(egui::Sense::click_and_drag())
                                    .selectable(false),
                            );
                            grip = Some(g.on_hover_text("Drag to move the toolbar"));
                            self.toolbar_items(ui, &mut action)
                        })
                    });
            });
        if let Some(g) = grip {
            self.drag_bar(ctx, &g, screen);
        }
        let rect = resp.response.rect;
        self.bar_w = rect.width();
        if resp.response.contains_pointer() {
            self.bar_until = Instant::now() + Duration::from_millis(500);
        }
        if !self.pinned {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some(f) = action {
            f(self, ctx)?;
        }
        Ok(Some(rect))
    }

    pub(super) fn toolbar_items(&mut self, ui: &mut egui::Ui, action: &mut Option<Deferred>) {
        // A narrow window keeps every button within reach: the least used go into the options
        // menu and Disconnect becomes an icon.
        let compact = ui.available_width() < 640.0;
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.spacing_mut().button_padding = Vec2::new(6.0, 4.0);
        // Connection quality at a glance; details on hover.
        let (dot, state) = match self.ctl.rtt {
            r if r < 80.0 => (GOOD, "good"),
            r if r < 250.0 => (WARN, "slow"),
            _ => (BAD, "poor"),
        };
        let details = format!(
            "Connection {state}\nRound trip {:.0} ms, {:.0} KB/s, {:.0} fps\n{}\n{}",
            self.ctl.rtt,
            self.ctl.kbps,
            self.ctl.fps_now,
            self.label,
            self.ctl.describe()
        );
        // One chip: the state's colour, the host and the round trip, the details on hover.
        let host = self.host().to_string();
        let rtt = if self.ctl.rtt >= 999.5 { "999+ ms".to_string() } else { format!("{:.0} ms", self.ctl.rtt) };
        egui::Frame::new()
            .fill(dot.gamma_multiply(0.12))
            .corner_radius(CornerRadius::same(crate::gui::theme::R_LG))
            .inner_margin(egui::Margin::symmetric(9, 3))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("●").size(10.0).color(dot));
                let width = ui.fonts_mut(|f| f.layout_no_wrap(host.clone(), FontId::proportional(13.0), Color32::WHITE).size().x);
                let room = if compact { 90.0 } else { 160.0 };
                ui.add_sized([width.min(room), 18.0], egui::Label::new(RichText::new(&host).strong().size(13.0)).truncate());
                // Fixed width, so a round trip going from 9 to 120 ms does not shift the buttons.
                ui.add_sized([44.0, 18.0], egui::Label::new(RichText::new(rtt).small().color(dot)));
            })
            .response
            .on_hover_text(&details);
        if self.sharing.0 != 0 {
            let name: String = self.sharing.1.chars().take(18).collect();
            let chip = egui::Button::new(
                RichText::new(format!("{}  {name}  {}", ic::APP_WINDOW, ic::X)).small().color(Color32::WHITE),
            )
            .fill(ACCENT)
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
                        *action = Some(Box::new(move |r, _| r.send(ToAgent::SelectMonitor(i as u8))));
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
        let fs_tip =
            if self.fullscreen { "Leave full screen (Ctrl+Alt+Enter)" } else { "Full screen (Ctrl+Alt+Enter)" };
        let fs_glyph = if self.fullscreen { ic::CORNERS_IN } else { ic::CORNERS_OUT };
        if icon(ui, fs_glyph, fs_tip, self.fullscreen).clicked() {
            *action = Some(Box::new(|r, c| {
                r.set_fullscreen(c, !r.fullscreen);
                Ok(())
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

        let files_tip = "File transfer: this computer and the SSH host side by side";
        if icon(ui, ic::FOLDER, files_tip, self.files.is_some()).clicked() {
            *action = Some(Box::new(|r, _| {
                if r.files.take().is_none() {
                    r.files = Some(crate::filewin::FileWindow::new());
                    r.send(ToAgent::ListDir { path: String::new() })?;
                }
                Ok(())
            }));
        }
        let (glyph, tip) = if self.term_inline && self.terms.is_some() {
            (ic::MONITOR, "Back to the remote screen, with the terminal under it")
        } else {
            (ic::TERMINAL, "Terminal: a shell on the SSH host in a panel under the screen (right click for more)")
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
        // Which of chat, screenshot, pause and one frame were asked for, from the bar or the menu.
        let (mut chat, mut shot, mut pause, mut frame) = (false, false, false, false);
        if !compact {
            chat = icon(ui, &chat_glyph, chat_tip, self.chat.is_some()).clicked();
            shot = icon(ui, ic::CAMERA, shot_tip, false).clicked();
            // Paused: a filled blue play button, so the frozen state is plain to see.
            let b = if self.paused {
                egui::Button::new(RichText::new(pause_glyph).size(16.0).color(Color32::WHITE))
                    .fill(ACCENT)
                    .min_size(Vec2::splat(28.0))
            } else {
                tool_button(pause_glyph, false)
            };
            pause = ui.add(b).on_hover_text(pause_tip).clicked();
            frame = self.paused && icon(ui, ic::ARROW_CLOCKWISE, "Fetch one frame", false).clicked();
        }

        let mut sound = self.sound.is_some();
        let mut sound_clicked = false;
        let mut dark = ui.visuals().dark_mode;
        let mut theme_clicked = false;
        let more = icon_menu(ui, ic::GEAR, |ui| {
            ui.checkbox(&mut self.view_only, "View only")
                .on_hover_text("Watch without sending keys, clicks or the wheel");
            ui.checkbox(&mut self.share_clipboard, "Share clipboard").on_hover_text("Copied text goes both ways");
            sound_clicked = ui.checkbox(&mut sound, "Sound (about 8 KB/s)").clicked();
            if cfg!(windows) {
                let tip = "While the pointer is over the picture, Win, Alt+Tab, Alt+Esc, Ctrl+Esc and Print Screen go to the SSH host instead of this PC";
                ui.checkbox(&mut self.grab_keys, "Send system keys").on_hover_text(tip);
            }
            theme_clicked = ui.checkbox(&mut dark, "Dark theme").clicked();
            ui.separator();
            if ui.button("Repaint the whole screen").clicked() {
                *action = Some(Box::new(|r, _| r.menu('r')));
                ui.close();
            }
            if compact {
                ui.separator();
                let item = |ui: &mut egui::Ui, glyph: &str, text: &str, tip: &str| {
                    let clicked = ui.button(format!("{glyph}  {text}")).on_hover_text(tip).clicked();
                    if clicked {
                        ui.close();
                    }
                    clicked
                };
                chat = item(ui, &chat_glyph, "Chat", chat_tip);
                shot = item(ui, ic::CAMERA, "Screenshot", shot_tip);
                pause = item(ui, pause_glyph, if self.paused { "Resume" } else { "Pause" }, pause_tip);
                frame = self.paused && item(ui, ic::ARROW_CLOCKWISE, "Fetch one frame", "Fetch one frame");
            }
        });
        // A paused picture shows on the menu button too when its own button is folded away.
        let more = if compact && self.paused { more.highlight() } else { more };
        more.on_hover_text("Options");
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
            self.paused = on;
            *action = Some(Box::new(move |r, _| r.send(ToAgent::Pause(on))));
        }
        if frame {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Refresh)));
        }
        if theme_clicked {
            set_theme(ui.ctx(), dark);
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
        // Pinned: a filled, coloured pin on a highlighted button; not pinned: a struck-through pin.
        // Only the full-screen dock can hide; in a window the toolbar is the title bar.
        let (pin, tip) = if !self.fullscreen {
            (RichText::new(""), "")
        } else if self.pinned {
            (
                RichText::new(ic::PUSH_PIN).size(16.0).family(filled()).color(Color32::WHITE),
                "Pinned: the toolbar stays. Click to unpin; it then hides and comes back when the pointer reaches the top edge.",
            )
        } else {
            (
                RichText::new(ic::PUSH_PIN_SLASH).size(16.0).color(ui.visuals().weak_text_color()),
                "Not pinned: the toolbar hides and comes back when the pointer reaches the top edge. Click to pin it.",
            )
        };
        let b =
            egui::Button::new(pin).min_size(Vec2::splat(28.0)).frame_when_inactive(self.pinned).selected(self.pinned);
        if self.fullscreen && ui.add(b).on_hover_text(tip).clicked() {
            self.pinned = !self.pinned;
            self.notify(if self.pinned {
                "Toolbar pinned"
            } else {
                "Toolbar unpinned: point at the top edge to bring it back"
            });
        }
        ui.separator();
        // In the title bar a red X would sit beside the window's own close button, so there it
        // reads as text; the full-screen dock keeps the compact red button.
        let end = if self.fullscreen {
            egui::Button::new(RichText::new(ic::X).size(15.0).color(Color32::WHITE))
                .fill(BAD)
                .min_size(Vec2::splat(28.0))
        } else if compact {
            tool_button(ic::LINK_BREAK, false)
        } else {
            egui::Button::new(RichText::new(format!("{}  Disconnect", ic::LINK_BREAK)).size(13.0))
                .frame_when_inactive(false)
                .min_size(Vec2::new(0.0, 28.0))
        };
        if ui.add(end).on_hover_text("Disconnect").clicked() {
            *action = Some(Box::new(|_, _| Err("disconnected".into())));
        }
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
                let b = egui::Button::new(RichText::new("Install the helper on the SSH host").color(Color32::WHITE))
                    .fill(ACCENT);
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
            let r = Remote::new(sess, rx, ctl, "test desktop".into(), None, false);
            let ctx = egui::Context::default();
            crate::gui::style(&ctx);
            let mut h = Harness { ctx, r, clock: 0.0, size, shapes: Vec::new() };
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
            let out = self.ctx.run_ui(input, |ui| result = self.r.frame(ui));
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

    #[test]
    #[ignore]
    fn toolbar_actions() {
        let mut h = Harness::new(Vec2::new(1280.0, 800.0));
        assert!(h.r.tex.is_some(), "the picture arrives");

        h.tool(ic::PAUSE).unwrap();
        assert!(h.r.paused, "pause");
        h.tool(ic::PLAY).unwrap();
        assert!(!h.r.paused, "resume");

        h.tool(ic::FOLDER).unwrap();
        assert!(h.r.files.is_some(), "files open");
        // Headless, the file manager is drawn inside this window, over the toolbar; in the app it
        // is a window of its own. It closes by its own command here.
        h.r.file_cmd(crate::filewin::Cmd::Close).unwrap();
        assert!(h.r.files.is_none(), "files close");

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

        h.tool(ic::CORNERS_OUT).unwrap();
        assert!(h.r.fullscreen, "full screen");
        // Ctrl+Alt+Enter leaves it; F11 is the host's.
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        h.frame(vec![key(egui::Key::F11, Default::default())]).unwrap();
        assert!(h.r.fullscreen, "F11 goes to the host");
        h.frame(vec![key(egui::Key::Enter, egui::Modifiers::CTRL | egui::Modifiers::ALT)]).unwrap();
        assert!(!h.r.fullscreen, "Ctrl+Alt+Enter leaves full screen");

        let ended = h.tool("Disconnect").is_err() || (0..3).any(|_| h.frame(Vec::new()).is_err());
        assert!(ended, "disconnect ends the session");
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

    /// In a narrow window the folded actions are in the options menu and still work.
    #[test]
    #[ignore]
    fn narrow_toolbar_actions() {
        let mut h = Harness::new(Vec2::new(620.0, 420.0));
        assert!(h.find(ic::CAMERA).is_none(), "screenshot folds away");
        h.tool(ic::GEAR).unwrap();
        h.click("Pause").unwrap();
        assert!(h.r.paused, "pause from the menu");
        h.tool(ic::GEAR).unwrap();
        h.click("Resume").unwrap();
        assert!(!h.r.paused, "resume from the menu");
        h.tool(ic::GEAR).unwrap();
        h.click("Chat").unwrap();
        assert!(h.r.chat.is_some(), "chat from the menu");
        let ended = h.tool(ic::LINK_BREAK).is_err() || (0..3).any(|_| h.frame(Vec::new()).is_err());
        assert!(ended, "disconnect as an icon");
    }
}
