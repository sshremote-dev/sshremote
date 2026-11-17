//! Uploads, dropped files and file commands of a remote pane.

use super::*;

impl Remote {
    /// The file manager filling the pane, with the notices over it.
    pub(super) fn files_view(&mut self, ui: &mut egui::Ui) -> Result<(), String> {
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
        self.toasts.draw(&ctx, ui.max_rect(), &[], None);
        ctx.request_repaint_after(Duration::from_millis(500));
        Ok(())
    }

    /// Starts uploading files dropped on the window.
    pub(in crate::gui) fn take_drops(&mut self, ctx: &egui::Context) -> Result<(), String> {
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

    /// Files copied here pasted there: they go over into a folder of their own on the host, then
    /// onto its clipboard, and are pasted where its keyboard is.
    pub(in crate::gui) fn paste_files(&mut self, paths: Vec<std::path::PathBuf>) -> Result<(), String> {
        if self.view_only || !self.share_clipboard {
            self.notify("Files are not pasted there while the clipboard is not shared or the view is only for looking");
            return Ok(());
        }
        self.batches += 1;
        let batch = format!("{}-{}", std::process::id(), self.batches);
        let dir = format!("{}{batch}", crate::files::CLIPBOARD);
        let first = self.next_upload;
        self.upload_into(&paths, &dir)?;
        let ids: Vec<u32> = (first..self.next_upload).collect();
        if ids.is_empty() {
            return Ok(());
        }
        let names = paths.iter().filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        self.notify(&format!("Pasting {} {} there\u{2026}", paths.len(), if paths.len() == 1 { "item" } else { "items" }));
        self.pasting = Some((batch, ids, names));
        Ok(())
    }

    /// Files copied there: fetched into a folder of their own here, then put on this clipboard,
    /// unless they are too large to bring over unasked.
    pub(in crate::gui) fn files_copied_there(&mut self, list: Vec<crate::proto::ClipFile>) -> Result<(), String> {
        const MOST: u64 = 200 << 20;
        let total: u64 = list.iter().map(|f| f.size).sum();
        if list.is_empty() {
            return Ok(());
        }
        if total > MOST {
            self.notify(&format!("{} MB copied there: too much to bring over by itself; fetch it in Files", total >> 20));
            return Ok(());
        }
        self.batches += 1;
        let dir = crate::files::clipboard_dir(&format!("{}-in-{}", std::process::id(), self.batches));
        let mut tags = Vec::new();
        let mut paths = Vec::new();
        for f in list {
            let name = f.path.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or("file").to_string();
            let tag = self.next_tag | crate::files::DONE_TAG;
            self.next_tag += 1;
            self.fetch_dirs.insert(tag, dir.clone());
            tags.push(tag);
            paths.push(dir.join(&name));
            self.send(ToAgent::Fetch { tag, path: f.path })?;
        }
        self.clip_in = Some((tags, paths));
        Ok(())
    }

    /// Files dropped or pasted on this session's terminal: into a folder of their own in the
    /// host's drop folder, their paths then typed at that terminal's prompt.
    pub(in crate::gui) fn term_upload(&mut self, tab: u32, paths: Vec<std::path::PathBuf>) -> Result<(), String> {
        self.batches += 1;
        let batch = format!("{}-{}", std::process::id(), self.batches);
        let dir = format!("{}{batch}", crate::files::DROP);
        let first = self.next_upload;
        self.upload_into(&paths, &dir)?;
        let ids: Vec<u32> = (first..self.next_upload).collect();
        if ids.is_empty() {
            if let Some(t) = self.terms.as_mut() {
                t.upload_done(tab, Err("nothing could be read".into()));
            }
            return Ok(());
        }
        let total = self.uploads.iter().filter(|u| ids.contains(&u.id)).map(|u| u.size).sum();
        self.term_drops.push(TermDrop { tab, batch, left: ids, total, finished: 0, tops: Vec::new(), failed: None });
        Ok(())
    }

    /// Stops a terminal's drop: what is under way is cancelled, and the terminal told.
    pub(in crate::gui) fn term_upload_cancel(&mut self, tab: u32) -> Result<(), String> {
        let Some(k) = self.term_drops.iter().position(|d| d.tab == tab) else { return Ok(()) };
        let d = self.term_drops.remove(k);
        for id in d.left {
            if let Some(i) = self.uploads.iter().position(|u| u.id == id) {
                let u = &mut self.uploads[i];
                if u.file.is_none() && !u.ended {
                    self.uploads.remove(i);
                } else if !u.ended {
                    u.cancelled = true;
                    (u.ended, u.file) = (true, None);
                    self.send(ToAgent::FileEnd { id })?;
                }
            }
        }
        if let Some(t) = self.terms.as_mut() {
            t.upload_done(tab, Err("stopped".into()));
        }
        Ok(())
    }

    /// Tells the terminals how far their drops have come.
    pub(in crate::gui) fn term_drop_progress(&mut self) {
        let Some(t) = self.terms.as_mut() else { return };
        for d in &self.term_drops {
            let going: u64 = self.uploads.iter().filter(|u| d.left.contains(&u.id)).map(|u| u.acked.min(u.size)).sum();
            t.upload_progress(d.tab, d.finished + going, d.total);
        }
    }

    /// An upload that was part of a terminal's drop has ended: whether it was one. Once the whole
    /// drop is there, the terminal types the paths of what was dropped (a folder's, not each file's).
    pub(in crate::gui) fn term_drop_done(&mut self, id: u32, ok: bool, text: &str) -> bool {
        let Some(k) = self.term_drops.iter().position(|d| d.left.contains(&id)) else { return false };
        let size = self.uploads.iter().find(|u| u.id == id).map_or(0, |u| u.size);
        self.uploads.retain(|u| u.id != id);
        let d = &mut self.term_drops[k];
        d.left.retain(|u| *u != id);
        d.finished += size;
        if !ok {
            d.failed.get_or_insert_with(|| text.to_string());
        } else if let Some(at) = text.find(&d.batch) {
            // DROPDIR/BATCH/NAME[/...]: the path of what was dropped is DROPDIR/BATCH/NAME.
            let after = at + d.batch.len();
            let sep = text[after..].chars().next().unwrap_or('/');
            let rest = &text[after + sep.len_utf8()..];
            let name = rest.split(['/', '\\']).next().unwrap_or(rest);
            let top = format!("{}{sep}{name}", &text[..after]);
            if !d.tops.contains(&top) {
                d.tops.push(top);
            }
        } else {
            d.tops.push(text.to_string());
        }
        if d.left.is_empty() {
            let d = self.term_drops.remove(k);
            let r = match d.failed {
                // An agent older than drops takes the folder's name for a path, and says so.
                Some(e) if e.contains('\0') || e.to_lowercase().contains("nul") => Err("the host's SSH Remote is older: update it there".to_string()),
                Some(e) => Err(e),
                None => Ok(d.tops),
            };
            if let Some(t) = self.terms.as_mut() {
                t.upload_done(d.tab, r);
            }
        }
        true
    }

    /// Uploads files or whole folders into `dir` on the remote host.
    pub(in crate::gui) fn upload_into(&mut self, paths: &[std::path::PathBuf], dir: &str) -> Result<(), String> {
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

    pub(in crate::gui) fn queue_upload(
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
    pub(in crate::gui) fn feed_uploads(&mut self) -> Result<(), String> {
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
                    let (id, text) = (u.id, format!("{} failed: {e}", u.name));
                    // A terminal's copy hears it too, and goes on with the rest.
                    if !self.term_drop_done(id, false, &text) {
                        self.uploads.remove(i);
                    }
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

    /// Shows the file transfer window and carries out what it asks for.
    /// Uploads and downloads in progress, for the file manager's list.
    pub(super) fn transfers(&self) -> Vec<crate::filewin::Transfer> {
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

    pub(in crate::gui) fn file_cmd(&mut self, c: crate::filewin::Cmd) -> Result<(), String> {
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
    pub(in crate::gui) fn upload_overlay(&self, ctx: &egui::Context, screen: Rect) {
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
    pub(in crate::gui) fn upload(&mut self, ctx: &egui::Context, resized: bool, f: &crate::proto::Frame) {
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
}

/// A drop on one of the session's terminals: its tab, the folder of this copy, the uploads still
/// going, the bytes of all and of those done, the paths of what was dropped, and the first error.
pub(in crate::gui) struct TermDrop {
    tab: u32,
    batch: String,
    left: Vec<u32>,
    total: u64,
    finished: u64,
    tops: Vec<String>,
    failed: Option<String>,
}
