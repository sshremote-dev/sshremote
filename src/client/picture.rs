//! The picture in the terminal: its size, frames, painting and the status line.

use super::*;

impl Viewer {
    pub(super) fn measure(&mut self) {
        if let Some((w, h)) = self.probe {
            (self.cw, self.ch) = self.cell_override.unwrap_or((10, 20));
            (self.cols, self.rows) = (w as usize / self.cw, h as usize / self.ch + 1);
            (self.ctl.s.view_w, self.ctl.s.view_h) = (w, h);
            return;
        }
        if self.tmux.is_some() {
            self.tmux = tmux_pane();
        }
        let (c, r) = terminal::size().unwrap_or((80, 24));
        self.cols = c.max(10) as usize;
        self.rows = r.max(3) as usize;
        let (mut cw, mut ch) = (10, 20);
        if let Ok(ws) = terminal::window_size()
            && ws.columns > 0
            && ws.rows > 0
        {
            let (w, h) = (ws.width as usize / ws.columns as usize, ws.height as usize / ws.rows as usize);
            // Some ssh clients (Windows OpenSSH) report a fixed 640x480 whatever the window is.
            if (4..=64).contains(&w) && (8..=128).contains(&h) && (ws.width, ws.height) != (640, 480) {
                (cw, ch) = (w, h);
            }
        }
        (self.cw, self.ch) = self.cell_override.or(self.cell_queried).unwrap_or((cw, ch));
        self.ctl.s.view_w = (self.cols * self.cw).min(u16::MAX as usize) as u16;
        self.ctl.s.view_h = ((self.rows - 1) * self.ch).min(u16::MAX as usize) as u16;
    }

    pub(super) fn display(&self) -> (usize, usize) {
        self.fb.fit((self.ctl.s.view_w as usize, self.ctl.s.view_h as usize), self.stretch)
    }

    /// Where the picture starts, in pixels: centred in the terminal on whole cells, so a screen
    /// of another shape leaves even bands on both sides, not one band at the bottom or right.
    pub(crate) fn origin(&self) -> (usize, usize) {
        let (dw, dh) = self.display();
        let (vw, vh) = (self.ctl.s.view_w as usize, self.ctl.s.view_h as usize);
        let (cw, ch) = (self.cw.max(1), self.ch.max(1));
        // Half the band, to the nearest cell: the two bands differ by a cell at most.
        let at = |band: usize, cell: usize| (band / 2 + cell / 2) / cell * cell;
        (at(vw.saturating_sub(dw), cw), at(vh.saturating_sub(dh), ch))
    }

    pub(super) fn apply(&mut self, f: Frame) -> io::Result<()> {
        let result = self.show_frame(f);
        // A video stream the decoder could not join: the agent starts it over.
        if std::mem::take(&mut self.fb.need_key) {
            self.sess.send(ToAgent::Refresh)?;
        }
        result
    }

    pub(super) fn show_frame(&mut self, f: Frame) -> io::Result<()> {
        if self.view != View::Screen || self.overlay != Overlay::None || self.prompt.is_some() {
            // Not in view (another mode, or something over it): kept, and drawn when it is again.
            if self.view == View::Screen {
                self.fb.apply(&f)?;
                self.dirty.iter_mut().for_each(|d| *d = true);
            }
            self.ctl.frame_done(false);
            return self.sess.send(ToAgent::Ack(f.id));
        }
        let started = Instant::now();
        let mut out = Vec::with_capacity(1 << 16);
        out.extend_from_slice(b"\x1b[?2026h");
        if self.fb.apply(&f)? {
            if let Some(k) = &mut self.kitty {
                let mut clear = Vec::new();
                k.clear(&mut clear);
                self.wrap(&mut out, &clear);
            }
            out.extend_from_slice(b"\x1b[0m\x1b[2J");
            self.dirty.iter_mut().for_each(|d| *d = true);
        }
        if self.lut_mode != Some(f.mode) {
            // True colour gets a palette per painted rectangle instead (see `paint`).
            let shown = if f.mode == Mode::True { Mode::Color256 } else { f.mode };
            self.pal = shown.palette();
            self.lut = Lut::new(&self.pal);
            self.lut_mode = Some(f.mode);
        }
        let (w, h) = (self.fb.w, self.fb.h);
        let (dw, dh) = self.display();
        let (ox, oy) = self.origin();
        let img_rows = self.rows - 1;
        self.dirty.resize(self.cols * img_rows, false);
        for (tx, ty, tw, th) in f.damage() {
            let x0 = ox + tx * dw / w;
            let x1 = ox + ((tx + tw) * dw).div_ceil(w);
            let y0 = oy + ty * dh / h;
            let y1 = oy + ((ty + th) * dh).div_ceil(h);
            for r in y0 / self.ch..y1.div_ceil(self.ch).min(img_rows) {
                for c in x0 / self.cw..x1.div_ceil(self.cw).min(self.cols) {
                    self.dirty[r * self.cols + c] = true;
                }
            }
        }
        self.paint(&mut out, dw, dh);
        out.extend_from_slice(b"\x1b[?2026l");
        self.paint_bytes += out.len() as u64;
        self.out.write_all(&out)?;
        self.out.flush()?;
        self.paint_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.ctl.frame_done(f.covered() * 2 >= w * h);
        self.sess.send(ToAgent::Ack(f.id))
    }

    /// Repaints dirty cells, merged into rectangles of whole cells.
    pub(super) fn paint(&mut self, out: &mut Vec<u8>, dw: usize, dh: usize) {
        let (cols, img_rows) = (self.cols, self.rows - 1);
        let (fw, fh) = (self.fb.w, self.fb.h);
        let (ox, oy) = self.origin();
        let adaptive = self.fb.mode == Some(Mode::True);
        for r in 0..img_rows {
            let mut c = 0;
            while c < cols {
                if !self.dirty[r * cols + c] {
                    c += 1;
                    continue;
                }
                let mut c1 = c;
                while c1 < cols && self.dirty[r * cols + c1] {
                    c1 += 1;
                }
                let mut r1 = r + 1;
                while r1 < img_rows && (c..c1).all(|k| self.dirty[r1 * cols + k]) {
                    r1 += 1;
                }
                for rr in r..r1 {
                    self.dirty[rr * cols + c..rr * cols + c1].fill(false);
                }
                let (x0, y0) = (c * self.cw, r * self.ch);
                let (w, h) = ((c1 - c) * self.cw, (r1 - r) * self.ch);
                self.px.clear();
                self.px.resize(w * h, [0; 3]);
                if dw > 0 {
                    for py in 0..h {
                        // Outside the picture (the bands beside it) stays black.
                        let Some(dy) = (y0 + py).checked_sub(oy).filter(|&d| d < dh) else { continue };
                        let frow = (dy * fh / dh) * fw;
                        let line = &mut self.px[py * w..(py + 1) * w];
                        for (px, slot) in line.iter_mut().enumerate() {
                            let Some(dx) = (x0 + px).checked_sub(ox) else { continue };
                            if dx >= dw {
                                break;
                            }
                            let o = (frow + dx * fw / dw) * 3;
                            slot.copy_from_slice(&self.fb.rgb[o..o + 3]);
                        }
                    }
                }
                let mut img = Vec::new();
                let (top, left) = self.tmux.unwrap_or((0, 0));
                // Through tmux kitty images go unplaced, placed by text tmux keeps in the pane.
                let mut placeholders = Vec::new();
                if self.kitty.is_none() || self.tmux.is_none() {
                    let _ = write!(img, "\x1b[{};{}H", top + r + 1, left + c + 1);
                }
                if let Some(k) = &mut self.kitty {
                    let text = self.tmux.is_some().then_some(&mut placeholders);
                    k.place(&mut img, self.px.as_flattened(), w, h, (cols, img_rows), (c, c1, r, r1), text);
                } else if adaptive {
                    quant::quantize(&self.px, &mut self.qpal, &mut self.idx);
                    self.sixel.encode(&mut img, w, h, &self.idx, &self.qpal);
                } else {
                    self.idx.clear();
                    self.idx.extend(self.px.iter().map(|p| self.lut.get(p[0], p[1], p[2])));
                    self.sixel.encode(&mut img, w, h, &self.idx, &self.pal);
                }
                if self.tmux.is_some() && self.kitty.is_some() {
                    // Through tmux one sequence at a time: it keeps no passthrough of a whole picture.
                    let mut cuts: Vec<usize> = img.windows(3).enumerate().filter(|(_, w)| *w == b"\x1b_G").map(|(i, _)| i).skip(1).collect();
                    cuts.insert(0, 0);
                    cuts.push(img.len());
                    for w in cuts.windows(2) {
                        tmux_wrap(out, &img[w[0]..w[1]]);
                    }
                    out.extend_from_slice(&placeholders);
                } else if self.tmux.is_some() {
                    tmux_wrap(out, &img);
                } else {
                    out.extend_from_slice(&img);
                }
                c = c1;
            }
        }
    }

    /// The last row: the modes as tabs, the connection, and what is going on; or the line
    /// being typed.
    pub(super) fn status(&mut self) -> io::Result<()> {
        if self.view == View::Terminal && self.overlay == Overlay::None {
            return Ok(());
        }
        let text = if let Some((kind, buf)) = &self.prompt {
            let what = match kind {
                'g' => "remote path to download",
                'v' => "share (number, A+number, a, p, or Enter for the whole screen)",
                'u' => "local file to send into this folder",
                'n' => "name of the new folder",
                'r' => "new name",
                _ => "local file to send to Downloads",
            };
            format!(" {what} (Enter: go, Esc: cancel)> {buf}_ ")
        } else {
            let tab = |v: View, name: &str| if self.view == v { format!("\x1b[1;4m{name}\x1b[22;24m") } else { name.to_string() };
            let tabs = format!(" {}  {}  {} ", tab(View::Screen, "Screen"), tab(View::Terminal, "Terminal"), tab(View::Files, "Files"));
            let what = if let Some((n, _)) = self.note.as_ref().filter(|(_, at)| at.elapsed() < Duration::from_secs(6)) {
                n.clone()
            } else if self.view == View::Files {
                self.files_line()
            } else if self.paused {
                format!("PAUSED (Ctrl+] u: one frame, p: resume) | {}", self.ctl.status(&self.label, self.fb.w, self.fb.h))
            } else if self.win_next {
                "Win+: press a key (Space: Win alone)".to_string()
            } else if self.sharing.0 != 0 {
                format!("SHARING {} (Ctrl+] v: whole screen) | {}", self.sharing.1, self.ctl.status(&self.label, self.fb.w, self.fb.h))
            } else {
                format!("{} | paint {:.1} ms", self.ctl.status(&self.label, self.fb.w, self.fb.h), self.paint_ms)
            };
            let chat = if self.unread > 0 { format!(" | chat: {} new (Ctrl+] c)", self.unread) } else { String::new() };
            format!("{tabs}| {what}{chat} | Ctrl+] menu ")
        };
        let shown = visible_len(&text);
        let text = cut_visible(&text, self.cols);
        let pad = self.cols.saturating_sub(shown.min(self.cols));
        write!(self.out, "\x1b[{};1H\x1b[0;7m{text}{}\x1b[0m", self.rows, " ".repeat(pad))?;
        self.out.flush()
    }

    pub(super) fn push_settings(&mut self) -> io::Result<()> {
        self.sess.send(ToAgent::Settings(self.ctl.s))?;
        self.status()
    }

    pub(super) fn full_redraw(&mut self) -> io::Result<()> {
        self.fb = Framebuffer::default();
        self.unpicture()?;
        write!(self.out, "\x1b[0m\x1b[2J")?;
        self.out.flush()
    }

    /// Deletes the kitty images before text is drawn where they are: they would show over it.
    /// (Text replaces sixel pixels by itself.)
    pub(super) fn unpicture(&mut self) -> io::Result<()> {
        let Some(k) = self.kitty.as_mut().filter(|k| k.any()) else { return Ok(()) };
        let mut clear = Vec::new();
        k.clear(&mut clear);
        let mut out = Vec::new();
        self.wrap(&mut out, &clear);
        self.dirty.iter_mut().for_each(|d| *d = true);
        self.out.write_all(&out)
    }

    /// `seq` for the terminal, through tmux when inside it.
    fn wrap(&self, out: &mut Vec<u8>, seq: &[u8]) {
        if self.tmux.is_some() {
            tmux_wrap(out, seq);
        } else {
            out.extend_from_slice(seq);
        }
    }
}
