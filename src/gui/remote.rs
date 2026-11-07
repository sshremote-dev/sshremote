//! A connected host: its screen, the toolbar, input, files, terminal and chat.

use super::*;

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
    /// Where the toolbar sits along the top edge, 0 left to 1 right; dragged by its grip or handle.
    bar_x: f32,
    /// The toolbar's width last frame, to keep it on screen.
    bar_w: f32,
    /// The tucked-away toolbar stays out until then: after the handle or the toolbar was last under the pointer.
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
        Remote {
            terms: terminal.then(|| crate::termwin::TermWindow::new(ssh_cmd.clone())),
            term_inline: terminal,
            term_detached: false,
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
            chat_lines: Vec::new(),
            unread: 0,
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
        self.take_drops(&ctx)?;
        self.feed_uploads()?;
        let screen = ui.max_rect();
        let ppp = ctx.pixels_per_point();
        // A pinned toolbar gets its own strip instead of covering the top of the remote screen.
        let area = if self.pinned { screen.with_min_y(screen.top() + 42.0) } else { screen };
        ui.painter().rect_filled(screen, CornerRadius::ZERO, Color32::from_rgb(18, 19, 23));
        ui.painter().rect_filled(area, CornerRadius::ZERO, Color32::BLACK);
        let inline = self.term_inline && self.terms.is_some();
        let shown = if inline {
            // The terminal fills the area below the toolbar; the remote screen waits behind it.
            let area = screen.with_min_y(screen.top() + 42.0);
            self.inline_terminal(ui, area)?;
            None
        } else {
            self.resize(area.size() * ppp)?;
            self.edge_pan(&ctx, area);
            self.picture(ui, area, ppp)
        };
        self.last_shown = shown;
        let toolbar_rect = self.toolbar(&ctx, screen)?;
        let chat_rect = self.chat_panel(&ctx)?;
        self.upload_overlay(&ctx, screen);
        self.draw_toasts(&ctx, screen);
        self.file_window(&ctx)?;
        self.term_window(&ctx)?;
        self.share_picker(&ctx)?;
        if self.pick_at.is_some_and(|t| Instant::now() >= t) {
            self.pick_at = None;
            self.send(ToAgent::Share { kind: 3, id: 0 })?;
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
            painter.rect_filled(r, CornerRadius::same(8), Color32::from_rgba_unmultiplied(20, 22, 28, 220));
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
            painter.rect_filled(r, CornerRadius::same(10), ACCENT);
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
        if ctx.input(|i| i.key_pressed(Key::F11)) {
            self.set_fullscreen(&ctx, !self.fullscreen);
        }
        // Network messages wake the window themselves; this only keeps statistics and pings going.
        ctx.request_repaint_after(Duration::from_millis(100));
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
                Ok(Ev::Net(ToClient::Chat(t))) => match self.chat.as_mut() {
                    Some((lines, _)) => lines.push((true, t)),
                    None => {
                        self.notify(&format!("Message: {t}"));
                        self.chat_lines.push((true, t));
                        self.unread += 1;
                    }
                },
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
                Ok(Ev::Net(ToClient::FetchStart { id, size, name })) => {
                    if let Some(crate::files::Note::Done { text, .. }) = self.inbox.start(id, size, &name) {
                        self.notify(&format!("{name}: {text}"));
                    } else {
                        self.downloads.push((id, name, 0));
                    }
                }
                Ok(Ev::Net(ToClient::FetchBegin { id, size, tag, name })) => {
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
        match &dir {
            Some(d) => self.send(ToAgent::FileStartIn { id, size, dir: d.clone(), name: name.clone() })?,
            None => self.send(ToAgent::FileStart { id, size, name: name.clone() })?,
        }
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
    pub(super) fn chat_panel(&mut self, ctx: &egui::Context) -> Result<Option<Rect>, String> {
        let Some((lines, input)) = self.chat.as_mut() else { return Ok(None) };
        let mut open = true;
        let mut sent = None;
        let resp = egui::Window::new("Chat")
            .open(&mut open)
            .default_pos(ctx.content_rect().left_top() + Vec2::new(20.0, 70.0))
            .default_size([340.0, 420.0])
            .show(ctx, |ui| {
                let h = ui.available_height().max(120.0) - 64.0;
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .max_height(h)
                    .show(ui, |ui| crate::chat::show_lines(ui, lines, "Remote", "You"));
                sent = crate::chat::input_row(ui, input, true);
            });
        let rect = resp.map(|r| r.response.rect);
        if let Some(text) = sent {
            if let Some((lines, _)) = self.chat.as_mut() {
                lines.push((false, text.clone()));
            }
            self.send(ToAgent::Chat(text))?;
        }
        if !open && let Some((lines, _)) = self.chat.take() {
            self.chat_lines = lines;
        }
        Ok(rect)
    }

    /// Shows the file transfer window and carries out what it asks for.
    pub(super) fn file_window(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let Some(mut f) = self.files.take() else { return Ok(()) };
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
        let host = self.label.split("  ").next().unwrap_or(&self.label).to_string();
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
                ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
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
                                ToAgent::Share { kind: 0, id: 0 }
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
                            pick = Some(ToAgent::Share { kind: 3, id: 0 });
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
                            pick = Some(ToAgent::Share { kind: 4, id: 0 });
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
                self.send(ToAgent::Share { kind: 0, id: 0 })?;
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
        let host = self.label.split("  ").next().unwrap_or(&self.label).to_string();
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
                    self.send(ToAgent::FetchTagged { tag, path })?;
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
                CornerRadius::same(10),
                Color32::from_rgba_unmultiplied(32, 34, 41, (235.0 * fade) as u8),
            );
            let bar = Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height()));
            painter.rect_filled(bar, CornerRadius::same(2), (if *warn { WARN } else { ACCENT }).gamma_multiply(fade));
            painter.galley(rect.min + Vec2::new(18.0, 10.0), galley, Color32::WHITE.gamma_multiply(fade));
            y = rect.top() - 8.0;
        }
        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(50));
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
        painter.rect_filled(bg, CornerRadius::same(8), Color32::from_rgba_unmultiplied(28, 30, 36, 230));
        painter.galley(pos, galley, Color32::WHITE);
    }

    /// Copies changed pixels into the texture.
    pub(super) fn upload(&mut self, ctx: &egui::Context, resized: bool, f: &crate::proto::Frame) {
        let (w, h) = (self.fb.w, self.fb.h);
        let opts = TextureOptions::LINEAR;
        match self.tex.as_mut() {
            Some(t) if !resized && t.size() == [w, h] => {
                for (x, y, rw, rh) in f.damage() {
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
        // Keys go to the remote unless a toolbar widget (a slider, a menu) has focus.
        let local = ctx.memory(|m| m.focused().is_some()) || ctx.any_popup_open();
        if let Some(rect) = shown {
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
                Event::Key { key: Key::F11, .. } => {}
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
                        let sym = if (c as u32) < 0x100 { c as u32 } else { 0x0100_0000 | c as u32 };
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
                    if pressed && (over_toolbar(pos) || local) {
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

    /// The toolbar, or while it is tucked away a small tab hanging from the top edge (as in Jump
    /// Desktop) that brings it back when the pointer reaches it; returns the toolbar's rectangle.
    pub(super) fn toolbar(&mut self, ctx: &egui::Context, screen: Rect) -> Result<Option<Rect>, String> {
        let id = egui::Id::unique("toolbar");
        let handle_id = egui::Id::unique("toolbar-handle");
        let shown_a_while = Instant::now() < self.bar_until;
        if !(self.pinned || shown_a_while || menu_open(ctx) || ctx.any_popup_open()) {
            let size = Vec2::new(64.0, 18.0);
            let left = self.bar_left(screen, size.x);
            let rect = Rect::from_min_size(Pos2::new(left, screen.top()), size);
            let dot = match self.ctl.rtt {
                r if r < 80.0 => GOOD,
                r if r < 250.0 => WARN,
                _ => BAD,
            };
            let resp = egui::Area::new(handle_id).fixed_pos(rect.min).order(egui::Order::Foreground).show(ctx, |ui| {
                let (r, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
                let fill = ctx.global_style().visuals.window_fill;
                let radius = CornerRadius { nw: 0, ne: 0, sw: 9, se: 9 };
                ui.painter().rect_filled(r, radius, fill.gamma_multiply(if resp.hovered() { 1.0 } else { 0.9 }));
                ui.painter().rect_stroke(
                    r,
                    radius,
                    Stroke::new(1.0, Color32::from_white_alpha(30)),
                    egui::StrokeKind::Inside,
                );
                ui.painter().circle_filled(r.left_center() + Vec2::new(14.0, 0.0), 3.5, dot);
                let fg = ctx.global_style().visuals.text_color();
                ui.painter().text(
                    r.center() + Vec2::new(6.0, 0.0),
                    Align2::CENTER_CENTER,
                    ic::CARET_DOWN,
                    FontId::proportional(13.0),
                    fg,
                );
                resp.on_hover_text("Toolbar (not pinned). Point here to show it; drag to move it along the edge.")
            });
            let r = resp.inner;
            self.drag_bar(ctx, &r, screen);
            // Hovering opens the toolbar, which then stays while the pointer is on it.
            if r.hovered() && !r.dragged() && !r.drag_stopped() {
                self.bar_until = Instant::now() + Duration::from_millis(700);
                ctx.request_repaint();
            }
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
                    .fill(ctx.global_style().visuals.window_fill.gamma_multiply(0.96))
                    .stroke(Stroke::new(1.0, Color32::from_white_alpha(25)))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(egui::Margin::symmetric(6, 3))
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
        ui.label(RichText::new("●").size(14.0).color(dot)).on_hover_text(&details);
        let host = self.label.split("  ").next().unwrap_or(&self.label).to_string();
        ui.add_sized([96.0, 20.0], egui::Label::new(RichText::new(host).strong().size(13.0)).truncate())
            .on_hover_text(&details);
        // Fixed width, so a round trip going from 9 to 120 ms does not shift the buttons.
        let rtt = if self.ctl.rtt >= 999.5 { "999+ ms".to_string() } else { format!("{:.0} ms", self.ctl.rtt) };
        ui.add_sized([46.0, 20.0], egui::Label::new(RichText::new(rtt).small().weak().monospace()))
            .on_hover_text(&details);
        if self.sharing.0 != 0 {
            let name: String = self.sharing.1.chars().take(18).collect();
            let chip = egui::Button::new(
                RichText::new(format!("{}  {name}  {}", ic::APP_WINDOW, ic::X)).small().color(Color32::WHITE),
            )
            .fill(ACCENT)
            .corner_radius(CornerRadius::same(10));
            if ui
                .add(chip)
                .on_hover_text(format!("Sharing {}. Click to share the whole screen again.", self.sharing.1))
                .clicked()
            {
                *action = Some(Box::new(|r, _| r.send(ToAgent::Share { kind: 0, id: 0 })));
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
        let fs_tip = if self.fullscreen { "Leave full screen (F11)" } else { "Full screen (F11)" };
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
                ("Switch window", "Alt+Tab", Some(vec![ALT_L, 0xff09])),
                ("Close window", "Alt+F4", Some(vec![ALT_L, 0xffc1])),
                ("Print screen", "PrtSc", Some(vec![0xff61])),
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
            (ic::MONITOR, "Back to the remote screen (the terminal keeps running)")
        } else {
            (ic::TERMINAL, "Terminal: a shell on the SSH host, in place of the screen")
        };
        let term = icon(ui, glyph, tip, self.terms.is_some());
        if term.clicked() {
            *action = Some(Box::new(|r, _| {
                if r.terms.is_none() {
                    r.terms = Some(crate::termwin::TermWindow::new(r.ssh_cmd.clone()));
                }
                r.term_detached = false;
                r.term_inline = !r.term_inline;
                r.release_all()
            }));
        }
        term.context_menu(|ui| {
            if ui.button(format!("{}  Terminal in its own window", ic::TERMINAL)).clicked() {
                *action = Some(Box::new(|r, _| {
                    if r.terms.is_none() {
                        r.terms = Some(crate::termwin::TermWindow::new(r.ssh_cmd.clone()));
                    }
                    r.term_inline = false;
                    r.term_detached = true;
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
                    r.term_inline = false;
                    r.term_detached = false;
                    Ok(())
                }));
                ui.close();
            }
        });
        let chat_glyph = if self.unread > 0 { format!("{} {}", ic::CHAT, self.unread) } else { ic::CHAT.into() };
        if icon(ui, &chat_glyph, "Chat with the person at the SSH host's screen", self.chat.is_some()).clicked() {
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
        if icon(ui, ic::CAMERA, "Screenshot: save the SSH host's screen as a PNG in Pictures", false).clicked() {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Snapshot)));
        }
        let (glyph, tip) = if self.paused {
            (ic::PLAY, "Resume the picture")
        } else {
            (ic::PAUSE, "Pause the picture to save traffic; fetch single frames when needed")
        };
        // Paused: a filled blue play button, so the frozen state is plain to see.
        let b = if self.paused {
            egui::Button::new(RichText::new(glyph).size(16.0).color(Color32::WHITE))
                .fill(ACCENT)
                .min_size(Vec2::splat(28.0))
        } else {
            tool_button(glyph, false)
        };
        if ui.add(b).on_hover_text(tip).clicked() {
            let on = !self.paused;
            self.paused = on;
            *action = Some(Box::new(move |r, _| r.send(ToAgent::Pause(on))));
        }
        if self.paused && icon(ui, ic::ARROW_CLOCKWISE, "Fetch one frame", false).clicked() {
            *action = Some(Box::new(|r, _| r.send(ToAgent::Refresh)));
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
        });
        more.on_hover_text("Options");
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
        let (pin, tip) = if self.pinned {
            (
                RichText::new(ic::PUSH_PIN).size(16.0).family(filled()).color(Color32::WHITE),
                "Pinned: the toolbar stays. Click to unpin; it then folds into a tab at the top edge.",
            )
        } else {
            (
                RichText::new(ic::PUSH_PIN_SLASH).size(16.0).color(ui.visuals().weak_text_color()),
                "Not pinned: the toolbar folds into a tab at the top edge. Click to pin it.",
            )
        };
        let b =
            egui::Button::new(pin).min_size(Vec2::splat(28.0)).frame_when_inactive(self.pinned).selected(self.pinned);
        if ui.add(b).on_hover_text(tip).clicked() {
            self.pinned = !self.pinned;
            self.notify(if self.pinned {
                "Toolbar pinned"
            } else {
                "Toolbar unpinned: it folds into the tab at the top"
            });
        }
        ui.separator();
        let end = egui::Button::new(RichText::new(ic::X).size(15.0).color(Color32::WHITE))
            .fill(BAD)
            .min_size(Vec2::splat(28.0));
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
                    *pick = Some(ToAgent::Share { kind: 1, id: w.id });
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
                    *pick = Some(ToAgent::Share { kind: 2, id });
                }
            }
        }
    }
}
