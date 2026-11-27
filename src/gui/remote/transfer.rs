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
        if self.view_only || self.mirror || !self.share_clipboard {
            self.notify(t!("Files are not pasted there while the clipboard is not shared or the view is only for looking"));
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
        self.notify(&if paths.len() == 1 { t!("Pasting {} item there\u{2026}", paths.len()) } else { t!("Pasting {} items there\u{2026}", paths.len()) });
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
            self.notify(&t!("{} MB copied there: too much to bring over by itself; fetch it in Files", total >> 20));
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
                t.upload_done(tab, Err(t!("nothing could be read").into()));
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
            t.upload_done(tab, Err(t!("stopped").into()));
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
                Some(e) if e.contains('\0') || e.to_lowercase().contains("nul") => Err(t!("the host's SSH Remote is older: update it there").to_string()),
                Some(e) => Err(crate::lang::tr_msg(&e).into_owned()),
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
        let u = Upload { id, name, dir, cancelled: false, error: None, path, file: None, size, sent: 0, acked: 0, ended: false, stamp: None, from: None, asking: false };
        self.uploads.push_back(u);
        Ok(())
    }

    /// Sends the next chunks of the first unfinished upload, keeping a bounded amount unacknowledged.
    pub(in crate::gui) fn feed_uploads(&mut self) -> Result<(), String> {
        use std::io::Read;
        let Some(i) = self.uploads.iter().position(|u| !u.ended) else { return Ok(()) };
        // Each file starts once the one before it is sent, so the host has one open at a time
        // however many were dropped.
        if self.uploads[i].file.is_none() {
            let can_resume = self.resume.can;
            let u = &mut self.uploads[i];
            match std::fs::File::open(&u.path) {
                Ok(mut f) => {
                    let stamp = f.metadata().ok().map(|m| (m.len(), m.modified().ok()));
                    // Cut off by a broken connection: the host is asked where to go on from,
                    // if this is still the file that was being sent.
                    // Kept while asked, should the connection break again before the answer.
                    u.from = u.from.filter(|from| can_resume && stamp.is_some() && stamp == u.stamp && *from <= u.size);
                    if let Some(from) = u.from
                        && let Ok(tail) = crate::files::tail_sum(&mut f, from)
                    {
                        (u.file, u.asking) = (Some(f), true);
                        let ask = ToAgent::FileResume { id: u.id, size: u.size, from, tail, dir: u.dir.clone().unwrap_or_default(), name: u.name.clone() };
                        return self.send(ask);
                    }
                    (u.stamp, u.from) = (stamp, None);
                    u.size = stamp.map_or(u.size, |s| s.0);
                    u.file = Some(f);
                    let start = ToAgent::FileStart { id: u.id, size: u.size, dir: u.dir.clone().unwrap_or_default(), name: u.name.clone() };
                    self.send(start)?;
                }
                Err(e) => {
                    let (id, text) = (u.id, t!("{} failed: {}", u.name, e));
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
            if u.asking || u.sent - u.acked.min(u.sent) >= UPLOAD_WINDOW {
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
                Ok(0) if want > 0 => Err(t!("it got shorter while being sent").to_string()),
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
                    let mut tag = self.next_tag;
                    self.next_tag += 1;
                    // Followed to its end, to fetch again from where a broken connection cut it.
                    if self.resume.can {
                        tag |= crate::files::DONE_TAG;
                        self.resume.follow(tag, &path);
                    }
                    self.fetch_dirs.insert(tag, to.clone());
                    self.send(ToAgent::Fetch { tag, path })?;
                }
            }
            Cmd::Cancel(true, id) => {
                if let Some(i) = self.uploads.iter().position(|u| u.id == id) {
                    let u = &mut self.uploads[i];
                    if u.file.is_none() && !u.ended {
                        // Not started: the host never heard of it.
                        let text = t!("{}: upload cancelled", u.name);
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
                    self.resume.done(tag);
                    self.inbox.end(id, false, "cancelled");
                    self.downloads.retain(|d| d.0 != id);
                    self.notify(&t!("{}: download cancelled", name));
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
            .map(|u| t!("Sending {}  {}%", u.name, (u.acked * 100).checked_div(u.size).unwrap_or(100)))
            .collect();
        if sending.len() > 3 {
            lines.push(t!("and {} more to send", sending.len() - 3));
        }
        let receiving: Vec<String> = self
            .downloads
            .iter()
            .filter_map(|(id, name, _)| {
                let (got, size) = self.inbox.received(*id)?;
                Some(t!("Receiving {}  {}%", name, (got * 100).checked_div(size).unwrap_or(100)))
            })
            .collect();
        lines.extend(receiving.iter().take(3).cloned());
        if receiving.len() > 3 {
            lines.push(t!("and {} more being received", receiving.len() - 3));
        }
        if hovering {
            lines.push(t!("Drop to send to the Downloads folder on {}", self.host()));
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
        self.fb_dirty = true;
        let opts = self.tex_filter;
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

impl Remote {
    /// A download begins: into the folder its request chose.
    pub(super) fn fetch_started(&mut self, id: u32, size: u64, tag: u32, name: String) -> Result<(), String> {
        let dir = self.fetch_dirs.get(&tag).cloned().unwrap_or_else(crate::files::downloads);
        let started = self.sess.fetch_start(&mut self.inbox, id, size, &dir, &name).map_err(|e| t!("connection lost: {}", e))?;
        if let Some(crate::files::Note::Done { text, .. }) = started {
            self.notify(&format!("{name}: {}", crate::lang::tr_msg(&text)));
        } else {
            self.resume.came_to(tag, &name, false);
            self.downloads.push((id, name, tag));
        }
        Ok(())
    }

    /// What this agent can do. One that goes on with transfers keeps what arrives of uploads
    /// when a connection breaks, and takes up those the connection before this one cut off.
    pub(super) fn can(&mut self, bits: u32) -> Result<(), String> {
        if bits & crate::proto::CAN_RESUME == 0 {
            self.carried_end();
            return Ok(());
        }
        self.resume.can = true;
        self.send(ToAgent::Resumes)?;
        let Some(mut c) = self.resume.carried.take() else { return Ok(()) };
        let n = c.count();
        self.uploads.extend(c.uploads.drain(..));
        for mut a in std::mem::take(&mut c.fetches) {
            let mut tail = [0; 32];
            if let Some(k) = &a.kept {
                match std::fs::File::open(&k.part).and_then(|mut f| crate::files::tail_sum(&mut f, a.from)) {
                    Ok(t) => tail = t,
                    Err(_) => {
                        k.discard();
                        (a.kept, a.from) = (None, 0);
                    }
                }
            }
            self.fetch_dirs.insert(a.tag, a.dir.clone());
            let at = (!a.name.is_empty()).then(|| (a.name.clone(), a.from == u64::MAX));
            self.resume.fetches.insert(a.tag, Fetching { path: a.path.clone(), at });
            self.send(ToAgent::FetchFrom { tag: a.tag, path: a.path.clone(), name: a.name.clone(), from: a.from, tail })?;
            if let Some(k) = a.kept.take() {
                self.resume.waiting.push((a.tag, a.name.clone(), k));
            }
        }
        self.notify(&if n == 1 { t!("Going on with {} transfer the connection cut off", n) } else { t!("Going on with {} transfers the connection cut off", n) });
        Ok(())
    }

    /// The transfers carried from the connection before end: this agent cannot go on with them.
    pub(super) fn carried_end(&mut self) {
        if let Some(c) = self.resume.carried.take() {
            let n = c.count();
            self.notify(&if n == 1 {
                t!("{} transfer the connection cut off could not go on: the host's SSH Remote is older; send it again", n)
            } else {
                t!("{} transfers the connection cut off could not go on: the host's SSH Remote is older; send them again", n)
            });
        }
    }

    /// Where the host goes on with an upload the connection before cut off.
    pub(super) fn file_from(&mut self, id: u32, from: u64) -> Result<(), String> {
        use std::io::Seek;
        let Some(u) = self.uploads.iter_mut().find(|u| u.id == id && u.asking) else { return Ok(()) };
        (u.asking, u.from) = (false, None);
        match u.file.as_mut().map(|f| f.seek(std::io::SeekFrom::Start(from))) {
            Some(Ok(_)) if from <= u.size => (u.sent, u.acked) = (from, from),
            _ => {
                (u.error, u.ended, u.file) = (Some(t!("it could not go on from where it was cut off").into()), true, None);
                self.send(ToAgent::FileEnd { id })?;
            }
        }
        Ok(())
    }

    /// The file a download fetched again had come to: on from the part kept here, or anew.
    pub(super) fn fetch_at(&mut self, id: u32, size: u64, tag: u32, from: u64, name: String) -> Result<(), String> {
        let kept = self.resume.waiting.iter().position(|(t, n, _)| *t == tag && *n == name).map(|i| self.resume.waiting.remove(i).2);
        if from == 0 {
            if let Some(k) = kept {
                k.discard();
            }
            return self.fetch_started(id, size, tag, name);
        }
        // A file grown since, as it was up to there, goes on at its new size.
        let kept = kept.map(|k| crate::files::Kept { size, ..k });
        match kept {
            Some(k) if self.inbox.take_up(id, &k, from) => {
                self.resume.came_to(tag, &name, false);
                self.downloads.push((id, name, tag));
            }
            k => {
                if let Some(k) = k {
                    k.discard();
                }
                self.send(ToAgent::FetchCancel { id, tag: 0 })?;
                self.notify(&t!("{} could not go on from where it was cut off", name));
            }
        }
        Ok(())
    }

    /// The transfers under way, to go on with in the connection made after this one broke (if
    /// its agent can); what arrived of downloads is kept here for that. Uploads that are part
    /// of a paste or a terminal's drop end with the connection.
    pub(in crate::gui) fn take_carry(&mut self) -> Option<Carry> {
        // Broken again before the agent said it goes on with them: still to go on with.
        if let Some(c) = self.resume.carried.take() {
            return Some(c);
        }
        if !self.resume.can {
            return None;
        }
        let mut kept: std::collections::HashMap<u32, crate::files::Kept> = self.inbox.keep_all().into_iter().collect();
        let batch: Vec<u32> = self.pasting.iter().flat_map(|p| p.1.iter().copied()).chain(self.term_drops.iter().flat_map(|d| d.left.iter().copied())).collect();
        let uploads: Vec<Upload> = std::mem::take(&mut self.uploads)
            .into_iter()
            .filter(|u| !u.cancelled && u.error.is_none() && !batch.contains(&u.id))
            .map(|mut u| {
                if !u.asking {
                    u.from = (u.acked > 0 && u.stamp.is_some()).then_some(u.acked);
                }
                (u.file, u.sent, u.acked, u.ended, u.asking) = (None, 0, 0, false, false);
                u
            })
            .collect();
        let mut fetches = Vec::new();
        for (tag, f) in std::mem::take(&mut self.resume.fetches) {
            let dir = self.fetch_dirs.get(&tag).cloned().unwrap_or_else(crate::files::downloads);
            let (name, from, part) = match f.at {
                None => (String::new(), 0, None),
                Some((name, true)) => (name, u64::MAX, None),
                Some((name, false)) => {
                    let id = self.downloads.iter().find(|d| d.2 == tag && d.1 == name).map(|d| d.0);
                    match id.and_then(|id| kept.remove(&id)) {
                        Some(k) if k.received > 0 => (name, k.received, Some(k)),
                        k => {
                            k.inspect(crate::files::Kept::discard);
                            (name, 0, None)
                        }
                    }
                }
            };
            fetches.push(Again { tag, path: f.path, dir, name, from, kept: part });
        }
        // Parts asked to go on with that the connection broke again before they came.
        for (tag, name, k) in std::mem::take(&mut self.resume.waiting) {
            match fetches.iter_mut().find(|a| a.tag == tag && a.name == name && a.kept.is_none() && a.from != u64::MAX) {
                Some(a) => (a.from, a.kept) = (k.received, Some(k)),
                None => k.discard(),
            }
        }
        kept.into_values().for_each(|k| k.discard());
        if uploads.is_empty() && fetches.is_empty() {
            return None;
        }
        Some(Carry { uploads, fetches, next_upload: self.next_upload, next_tag: self.next_tag })
    }

    /// Takes on the transfers the connection before this one cut off; they go on once the agent
    /// says it can.
    pub(in crate::gui) fn carry_on(&mut self, c: Carry) {
        self.next_upload = self.next_upload.max(c.next_upload);
        self.next_tag = self.next_tag.max(c.next_tag);
        self.resume.carried = Some(c);
    }
}

/// Going on with the transfers a broken connection cut off.
#[derive(Default)]
pub(in crate::gui) struct Resume {
    /// The agent goes on with them (`proto::CAN_RESUME`).
    pub(super) can: bool,
    /// Download requests followed, by tag, to be fetched again from where they were.
    pub(super) fetches: std::collections::HashMap<u32, Fetching>,
    /// Those of the connection before, until the agent says whether it goes on with them.
    carried: Option<Carry>,
    /// Parts kept here of downloads fetched again, until their file comes: tag, name, part.
    waiting: Vec<(u32, String, crate::files::Kept)>,
}

impl Drop for Resume {
    fn drop(&mut self) {
        self.waiting.iter().for_each(|w| w.2.discard());
    }
}

impl Resume {
    pub(in crate::gui) fn follow(&mut self, tag: u32, path: &str) {
        self.fetches.insert(tag, Fetching { path: path.to_string(), at: None });
    }

    /// The file a followed request has come to, and whether it arrived whole (or ended).
    pub(in crate::gui) fn came_to(&mut self, tag: u32, name: &str, ended: bool) {
        if let Some(f) = self.fetches.get_mut(&tag) {
            f.at = Some((name.to_string(), ended));
        }
    }

    /// All of a request is sent.
    pub(in crate::gui) fn done(&mut self, tag: u32) {
        self.fetches.remove(&tag);
        self.waiting.retain(|(t, _, k)| {
            if *t == tag {
                k.discard();
            }
            *t != tag
        });
    }
}

/// A download request followed: the path asked for, and the file it came to (whether ended).
pub(super) struct Fetching {
    path: String,
    at: Option<(String, bool)>,
}

/// The transfers a broken connection cut off, carried into the next one.
pub(in crate::gui) struct Carry {
    uploads: Vec<Upload>,
    fetches: Vec<Again>,
    next_upload: u32,
    next_tag: u32,
}

impl Carry {
    /// How many transfers it holds.
    pub(in crate::gui) fn count(&self) -> usize {
        self.uploads.len() + self.fetches.len()
    }
}

impl Drop for Carry {
    fn drop(&mut self) {
        self.fetches.iter().filter_map(|a| a.kept.as_ref()).for_each(crate::files::Kept::discard);
    }
}

/// A download request to fetch again: its tag, path and folder here, the file it came to (none
/// yet: empty), how much of that is here (`u64::MAX`: all) and the part holding it.
struct Again {
    tag: u32,
    path: String,
    dir: std::path::PathBuf,
    name: String,
    from: u64,
    kept: Option<crate::files::Kept>,
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
