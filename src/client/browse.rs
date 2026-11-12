//! The host's files in the terminal, and uploads to it.

use super::*;

impl Viewer {
    /// The remote folder: a header, the entries with the selection, keys at the foot.
    pub(super) fn draw_files(&mut self) -> io::Result<()> {
        let height = self.rows.saturating_sub(1);
        let list_h = height.saturating_sub(4).max(1);
        let shown: Vec<(String, bool, u64, u64)> =
            self.files.shown().iter().map(|e| (clean(&e.name), e.dir, e.size, e.mtime)).collect();
        if self.files.sel >= shown.len() {
            self.files.sel = shown.len().saturating_sub(1);
        }
        if self.files.sel < self.files.top {
            self.files.top = self.files.sel;
        } else if self.files.sel >= self.files.top + list_h {
            self.files.top = self.files.sel + 1 - list_h;
        }
        let place = if self.files.path == crate::files::DRIVES { "Drives".to_string() } else { clean(&self.files.path) };
        let mut rows: Vec<(String, &str)> = vec![(format!(" Files on {}:  {place}", self.host), "\x1b[48;5;236;97m")];
        let date_w = 17;
        let size_w = 10;
        let name_w = self.cols.saturating_sub(date_w + size_w + 6).max(10);
        rows.push((format!("   {:<name_w$} {:>size_w$}  {}", "Name", "Size", "Modified"), "\x1b[2m"));
        if self.files.loading {
            rows.push(("   Loading...".into(), "\x1b[2m"));
        } else if !self.files.error.is_empty() {
            rows.push((format!("   {}", clean(&self.files.error)), "\x1b[31m"));
        } else if shown.is_empty() {
            rows.push(("   (empty)".into(), "\x1b[2m"));
        }
        for (i, (name, dir, size, mtime)) in shown.iter().enumerate().skip(self.files.top).take(list_h) {
            let mark = if *dir { "/" } else { " " };
            let name: String = format!("{name}{mark}");
            let name = cut_visible(&name, name_w);
            let pad = name_w.saturating_sub(visible_len(&name));
            let size = if *dir { String::new() } else { crate::files::human(*size) };
            let when = crate::files::local_time(*mtime);
            let style = if i == self.files.sel { "\x1b[7m" } else if *dir { "\x1b[34;1m" } else { "" };
            rows.push((format!("   {name}{} {size:>size_w$}  {when}", " ".repeat(pad)), style));
        }
        while rows.len() < height - 2 {
            rows.push((String::new(), ""));
        }
        rows.push((format!(" Downloads go to {}   (o: open that folder)", crate::files::downloads().display()), "\x1b[36m"));
        rows.push((
            " Enter open folder / get file  d get file or folder  Backspace up  ~ home  u send here  n new folder  F2 rename  Del delete  . hidden  F5 reload".into(),
            "\x1b[2m",
        ));
        self.draw_rows(&rows)
    }

    /// The status line in the files view: the selection, and the transfers going on.
    pub(super) fn files_line(&self) -> String {
        let sel = self.files.shown().get(self.files.sel).map(|e| {
            let what = if e.dir { "folder".to_string() } else { crate::files::human(e.size) };
            let when = crate::files::local_time(e.mtime);
            if when.is_empty() { format!("{}  {what}", e.name) } else { format!("{}  {what}  {when}", e.name) }
        });
        let moving = if self.transfers.is_empty() {
            String::new()
        } else {
            let done: u64 = self.transfers.iter().map(|t| t.done).sum();
            let size: u64 = self.transfers.iter().map(|t| t.size).sum::<u64>().max(1);
            let pct = (done * 100 / size).min(100) as usize;
            let bar = format!("[{}{}]", "#".repeat(pct / 10), ".".repeat(10 - pct / 10));
            let first = &self.transfers[0];
            let arrow = if first.upload { "up" } else { "down" };
            format!(" | {arrow} {} {bar} {pct}% ({} files)", first.name, self.transfers.len())
        };
        format!("{}{moving}", sel.unwrap_or_else(|| format!("{} items", self.files.shown().len())))
    }

    pub(super) fn files_go(&mut self, path: String) -> io::Result<()> {
        self.files.loading = true;
        self.files.error.clear();
        self.sess.send(ToAgent::ListDir { path })?;
        if self.overlay == Overlay::None && self.prompt.is_none() {
            self.draw_files()?;
        }
        Ok(())
    }

    pub(super) fn files_key(&mut self, k: &event::KeyEvent) -> io::Result<()> {
        use event::KeyCode;
        let n = self.files.shown().len();
        let page = self.rows.saturating_sub(6).max(1);
        let picked = self.files.shown().get(self.files.sel).map(|e| (e.name.clone(), e.dir));
        match k.code {
            KeyCode::Up => self.files.sel = self.files.sel.saturating_sub(1),
            KeyCode::Down => self.files.sel = (self.files.sel + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.files.sel = self.files.sel.saturating_sub(page),
            KeyCode::PageDown => self.files.sel = (self.files.sel + page).min(n.saturating_sub(1)),
            KeyCode::Home => self.files.sel = 0,
            KeyCode::End => self.files.sel = n.saturating_sub(1),
            KeyCode::Enter | KeyCode::Right => match picked {
                Some((name, true)) => {
                    let path = self.files.child(&name);
                    self.files.sel = 0;
                    return self.files_go(path);
                }
                Some((name, false)) if k.code == KeyCode::Enter => {
                    let path = self.files.child(&name);
                    self.sess.send(ToAgent::Fetch { tag: 0, path })?;
                    return self.notify(&format!("Downloading {name} to {}", crate::files::downloads().display()));
                }
                _ => {}
            },
            KeyCode::Backspace | KeyCode::Left => {
                let path = self.files.parent();
                self.files.sel = 0;
                return self.files_go(path);
            }
            KeyCode::Char('~') => {
                self.files.sel = 0;
                return self.files_go(String::new());
            }
            KeyCode::Char('d') => {
                if let Some((name, _)) = picked {
                    let path = self.files.child(&name);
                    self.sess.send(ToAgent::Fetch { tag: 0, path })?;
                    return self.notify(&format!("Downloading {name} to {}", crate::files::downloads().display()));
                }
            }
            KeyCode::Char('.') => self.files.hidden = !self.files.hidden,
            KeyCode::Char('o') => {
                let dir = crate::files::downloads();
                let _ = std::fs::create_dir_all(&dir);
                #[cfg(feature = "gui")]
                crate::tools::open_path(&dir);
                return self.notify(&format!("Opened {}", dir.display()));
            }
            KeyCode::F(5) => {
                let path = self.files.path.clone();
                return self.files_go(path);
            }
            KeyCode::Char('u') => {
                self.prompt = Some(('u', String::new()));
                return self.status();
            }
            KeyCode::Char('n') if self.files.path != crate::files::DRIVES => {
                self.prompt = Some(('n', String::new()));
                return self.status();
            }
            KeyCode::F(2) | KeyCode::Char('r') if self.files.path != crate::files::DRIVES => {
                if let Some((name, _)) = picked {
                    self.target = Some(self.files.child(&name));
                    self.prompt = Some(('r', name));
                    return self.status();
                }
            }
            KeyCode::Delete if self.files.path != crate::files::DRIVES => {
                if let Some((name, _)) = picked {
                    self.target = Some(self.files.child(&name));
                    self.overlay = Overlay::Confirm('x');
                    return self.redraw();
                }
            }
            _ => return Ok(()),
        }
        self.draw_files()
    }

    /// Sends a local file to the remote Downloads folder; the link paces the writes.
    pub(super) fn upload(&mut self, path: &str) -> io::Result<()> {
        self.upload_to(path, "")
    }

    /// Sends a local file into the remote folder `dir` (empty: Downloads), a piece at a time as
    /// the loop comes round, so the view stays live and the progress true.
    pub(super) fn upload_to(&mut self, path: &str, dir: &str) -> io::Result<()> {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => return self.notify(&format!("{path}: {e}")),
        };
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let name = std::path::Path::new(path).file_name().map_or("file".into(), |n| n.to_string_lossy().into_owned());
        let id = self.next_upload;
        self.next_upload += 1;
        self.sess.send(ToAgent::FileStart { id, size, dir: dir.to_string(), name: name.clone() })?;
        self.uploads.push((id, file, size, 0));
        self.transfers.push(Transfer { id, name: name.clone(), done: 0, size, upload: true });
        self.notify(&format!("Sending {name}..."))
    }

    /// The next pieces of the uploads under way; true while some remain.
    pub(super) fn pump_uploads(&mut self) -> io::Result<bool> {
        use std::io::Read;
        let mut budget: usize = 1 << 20;
        let mut finished = Vec::new();
        let mut failed = Vec::new();
        let mut buf = vec![0u8; 64 << 10];
        for (id, file, size, sent) in &mut self.uploads {
            while budget > 0 {
                // The size it had when it started: a file that grows meanwhile still arrives whole.
                let want = (*size - *sent).min(buf.len() as u64) as usize;
                if want == 0 {
                    finished.push(*id);
                    break;
                }
                let n = match file.read(&mut buf[..want]) {
                    Ok(0) => {
                        failed.push((*id, "it got shorter while being sent".to_string()));
                        break;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        failed.push((*id, e.to_string()));
                        break;
                    }
                };
                self.sess.send(ToAgent::FileData { id: *id, data: buf[..n].to_vec() })?;
                *sent += n as u64;
                budget = budget.saturating_sub(n);
            }
            if let Some(t) = self.transfers.iter_mut().find(|t| t.upload && t.id == *id) {
                t.done = *sent;
            }
        }
        for id in finished {
            self.sess.send(ToAgent::FileEnd { id })?;
            self.uploads.retain(|u| u.0 != id);
        }
        // A file that cannot be read ends its own upload, not the session.
        for (id, e) in failed {
            self.sess.send(ToAgent::FileEnd { id })?;
            self.uploads.retain(|u| u.0 != id);
            self.transfers.retain(|t| !(t.upload && t.id == id));
            self.notify(&format!("Upload stopped: {e}"))?;
        }
        if self.view == View::Files && self.overlay == Overlay::None && self.prompt.is_none() {
            self.status()?;
        }
        Ok(!self.uploads.is_empty())
    }
}
