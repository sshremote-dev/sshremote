//! What is drawn over the picture: the menu, confirmations, the chat and prompts.

use super::*;

impl Viewer {
    /// Draws rows of text from the top, each cut or padded to the width; `style` per row.
    pub(super) fn draw_rows(&mut self, rows: &[(String, &str)]) -> io::Result<()> {
        self.unpicture()?;
        let mut out = String::from("\x1b[?2026h\x1b[0m");
        let height = self.rows.saturating_sub(1);
        for i in 0..height {
            let (text, style) = rows.get(i).map_or((String::new(), ""), |(t, s)| (t.clone(), *s));
            let shown = visible_len(&text);
            let text = cut_visible(&text, self.cols);
            let pad = self.cols.saturating_sub(shown.min(self.cols));
            out.push_str(&format!("\x1b[{};1H\x1b[0m{style}{text}{}\x1b[0m", i + 1, " ".repeat(pad)));
        }
        out.push_str("\x1b[?2026l");
        self.out.write_all(out.as_bytes())?;
        self.status()
    }

    /// The Ctrl+] menu: every action by its key, grouped as the window's toolbar and menus are.
    pub(super) fn draw_menu(&mut self) -> io::Result<()> {
        let mark = |v: View| if self.view == v { "*" } else { " " };
        let quality = if self.ctl.auto.is_some() { "auto".to_string() } else { self.ctl.preset_label() };
        let chat = if self.unread > 0 { format!("  ({} new)", self.unread) } else { String::new() };
        let head = "\x1b[48;5;236;97m";
        let key = "\x1b[0m";
        let rows: Vec<(String, &str)> = vec![
            (format!(" sshremote  {}   (a key below, Esc: back)", self.label), head),
            (String::new(), key),
            (" Mode".into(), "\x1b[1m"),
            (format!("   s{} Screen sharing     t{} Terminal     f{} Files", mark(View::Screen), mark(View::Terminal), mark(View::Files)), key),
            (String::new(), key),
            (" Talk".into(), "\x1b[1m"),
            (format!("   c  Chat with the person at the screen{chat}"), key),
            (String::new(), key),
            (" Screen".into(), "\x1b[1m"),
            (format!("   0-4 quality, a automatic (now {quality})    m colours    + - scale    F frames per second"), key),
            ("   z  zoom to the terminal    n  next monitor    v  share a window or application".into(), key),
            ("   p  pause or resume    u  one frame    r  repaint    x  screenshot    o  sound".into(), key),
            ("   w  Win with the next key    k  Ctrl+Alt+Del    ]  send Ctrl+] to the host".into(), key),
            (String::new(), key),
            (" Files".into(), "\x1b[1m"),
            ("   g  get a remote file by its path    l  send a local file to Downloads    (f: browse)".into(), key),
            (String::new(), key),
            (" Leave".into(), "\x1b[1m"),
            ("   q  close (asks first)".into(), key),
        ];
        self.draw_rows(&rows)
    }

    /// Covers the view with the menu, the chat or a question; in the terminal, on the alternate
    /// screen, so the shell's own screen comes back as it was.
    pub(super) fn open_overlay(&mut self, o: Overlay) -> io::Result<()> {
        if self.view == View::Screen && self.overlay == Overlay::None {
            self.release_all()?;
        }
        if self.view == View::Terminal && self.overlay == Overlay::None && self.probe.is_none() {
            // A program on its own screen keeps it: the menu is drawn over, and it repaints after.
            if self.shell_modes.alt {
                let _ = execute!(self.out, cursor::Hide);
            } else {
                let _ = execute!(self.out, terminal::EnterAlternateScreen, cursor::Hide);
            }
        }
        self.overlay = o;
        self.redraw()
    }

    pub(super) fn close_overlay(&mut self) -> io::Result<()> {
        self.overlay = Overlay::None;
        if self.view == View::Terminal {
            return self.show_shell();
        }
        self.redraw()
    }

    /// A key chosen in the menu.
    pub(super) fn menu_key(&mut self, c: char) -> io::Result<()> {
        match c {
            's' => self.set_view(View::Screen),
            't' => self.set_view(View::Terminal),
            'f' => self.set_view(View::Files),
            'c' => {
                self.unread = 0;
                self.overlay = Overlay::Chat(String::new());
                self.redraw()
            }
            'q' => {
                self.overlay = Overlay::Confirm('q');
                self.redraw()
            }
            'k' => {
                self.close_overlay()?;
                self.sess.send(ToAgent::SecureAttention)?;
                self.notify("Sent Ctrl+Alt+Del")
            }
            'x' => {
                self.close_overlay()?;
                self.sess.send(ToAgent::Snapshot)?;
                self.notify("Taking a screenshot...")
            }
            'p' => {
                self.paused = !self.paused;
                self.close_overlay()?;
                if self.view == View::Screen {
                    self.sess.send(ToAgent::Pause(self.paused))?;
                }
                self.status()
            }
            'u' => {
                self.close_overlay()?;
                self.sess.send(ToAgent::Refresh)?;
                self.status()
            }
            'o' => {
                self.close_overlay()?;
                if self.player.take().is_some() {
                    self.sess.send(ToAgent::Audio(false))?;
                    return self.notify("Sound off");
                }
                match crate::audio::Player::new() {
                    Ok(p) => {
                        self.player = Some(p);
                        self.sess.send(ToAgent::Audio(true))?;
                        self.notify("Sound on")
                    }
                    Err(e) => self.notify(&format!("Sound: {e}")),
                }
            }
            'v' => {
                self.set_view(View::Screen)?;
                // Sharing part of the screen: the same key leads straight back to all of it.
                if self.sharing.0 != 0 {
                    self.sess.send(ToAgent::Share(Share::Screen))?;
                    return self.notify("Sharing the whole screen...");
                }
                self.share_list = None;
                self.sess.send(ToAgent::ListWindows)?;
                self.prompt = Some(('v', String::new()));
                self.draw_share_list()
            }
            'g' | 'l' => {
                self.close_overlay()?;
                if self.view == View::Terminal {
                    self.set_view(View::Files)?;
                }
                self.prompt = Some((c, String::new()));
                self.status()
            }
            // The screen's own keys: next monitor is n here (s is the screen mode), frames per
            // second F (f is the files).
            'n' => {
                self.close_overlay()?;
                let a = self.ctl.menu('s');
                self.run_action(a)
            }
            'F' => {
                self.close_overlay()?;
                let a = self.ctl.menu('f');
                self.run_action(a)
            }
            // In the terminal: Ctrl+] for the shell, and a repaint of its program.
            ']' if self.view == View::Terminal => {
                self.close_overlay()?;
                match self.shell {
                    Some(id) => self.sess.send(ToAgent::TermInput { id, data: vec![0x1d] }),
                    None => Ok(()),
                }
            }
            'r' if self.view == View::Terminal => {
                self.close_overlay()?;
                self.nudge_shell()
            }
            '0'..='4' | 'a' | 'm' | '+' | '=' | '-' | 'z' | 'r' | 'w' | ']' => {
                if matches!(c, 'z' | 'w') && self.view != View::Screen {
                    self.set_view(View::Screen)?;
                } else {
                    self.close_overlay()?;
                }
                let a = self.ctl.menu(c);
                self.run_action(a)
            }
            _ => self.close_overlay(),
        }
    }

    /// A question over the view: y says yes.
    pub(super) fn draw_confirm(&mut self, what: char) -> io::Result<()> {
        let (title, body) = match what {
            'q' => (format!("Close {}?", self.host), "The screen, the shell and transfers end.".to_string()),
            _ => {
                let name = clean(self.target.as_deref().unwrap_or(""));
                (format!("Delete {name} on {}?", self.host), "It cannot be brought back; a folder goes with everything in it.".to_string())
            }
        };
        let rows: Vec<(String, &str)> = vec![
            (String::new(), ""),
            (format!("   {title}"), "\x1b[1m"),
            (format!("   {body}"), ""),
            (String::new(), ""),
            ("   y: yes      any other key: no".into(), "\x1b[33m"),
        ];
        self.draw_rows(&rows)
    }

    pub(super) fn confirm_key(&mut self, what: char, k: &event::KeyEvent) -> io::Result<()> {
        let yes = matches!(k.code, event::KeyCode::Char('y' | 'Y'));
        self.overlay = Overlay::None;
        match (what, yes) {
            ('q', true) => {
                self.quit = true;
                Ok(())
            }
            ('x', true) => {
                let Some(path) = self.target.take() else { return self.redraw() };
                self.sess.send(ToAgent::FileOp { op: crate::proto::FileOp::Delete, path, arg: String::new() })?;
                self.redraw()
            }
            _ => {
                if self.view == View::Terminal {
                    return self.close_overlay();
                }
                self.redraw()
            }
        }
    }

    /// The conversation, newest at the bottom, as Slack shows it: who and when, then the text.
    pub(super) fn draw_chat(&mut self) -> io::Result<()> {
        let Overlay::Chat(buf) = &self.overlay else { return Ok(()) };
        let buf = buf.clone();
        let height = self.rows.saturating_sub(1);
        let width = self.cols.saturating_sub(4).max(10);
        let mut body: Vec<(String, &str)> = Vec::new();
        for (theirs, text, at) in crate::chat::load_history(&self.host) {
            let peer = clean(&self.peer.get());
            let who = if theirs { peer.as_str() } else { "You" };
            let when = if at == 0 { String::new() } else { format!("  {}", crate::chat::when(at)) };
            body.push((format!("  {who}\x1b[22;2m{when}"), "\x1b[1m"));
            for line in wrap(&clean(&text), width) {
                body.push((format!("  {line}"), ""));
            }
            body.push((String::new(), ""));
        }
        let room = height.saturating_sub(3);
        let start = body.len().saturating_sub(room);
        let mut rows: Vec<(String, &str)> =
            vec![(format!(" Chat with the person at {}'s screen   (Enter: send, Esc: back)", self.host), "\x1b[48;5;236;97m")];
        rows.extend(body.drain(start..));
        while rows.len() < height - 1 {
            rows.push((String::new(), ""));
        }
        rows.push((format!(" > {buf}_"), "\x1b[48;5;238;97m"));
        self.draw_rows(&rows)
    }

    pub(super) fn chat_key(&mut self, k: &event::KeyEvent) -> io::Result<()> {
        use event::KeyCode;
        let Overlay::Chat(buf) = &mut self.overlay else { return Ok(()) };
        match k.code {
            KeyCode::Esc => return self.close_overlay(),
            KeyCode::Enter => {
                let text = buf.trim().to_string();
                buf.clear();
                if !text.is_empty() {
                    self.sess.send(ToAgent::Chat(text.clone()))?;
                    crate::chat::append_history(&self.host, false, &text);
                }
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) if !k.modifiers.contains(event::KeyModifiers::CONTROL) => buf.push(c),
            _ => return Ok(()),
        }
        self.draw_chat()
    }

    pub(super) fn prompt_key(&mut self, k: &event::KeyEvent) -> io::Result<()> {
        use event::KeyCode;
        let Some((kind, buf)) = self.prompt.as_mut() else { return Ok(()) };
        match k.code {
            KeyCode::Char(c) if !k.modifiers.contains(event::KeyModifiers::CONTROL) => buf.push(c),
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Esc => {
                if matches!(self.prompt, Some(('v', _))) {
                    self.prompt = None;
                    return self.close_share_list().and_then(|_| self.status());
                }
                self.prompt = None;
                return self.redraw();
            }
            KeyCode::Enter => {
                let (kind, text) = (*kind, buf.trim().to_string());
                self.prompt = None;
                if !text.is_empty() || kind == 'v' {
                    return self.run_prompt(kind, text);
                }
                return self.redraw();
            }
            _ => {}
        }
        self.status()
    }

    /// The share chooser drawn over the picture: what can be shared, by number.
    pub(super) fn draw_share_list(&mut self) -> io::Result<()> {
        let mut lines = vec![
            " Share: type a number, a, p or nothing, then Enter (Esc cancels) ".to_string(),
            "   Enter  the whole screen".to_string(),
            "   a      the active window".to_string(),
            "   p      the active application (every window of it)".to_string(),
        ];
        match &self.share_list {
            None => lines.push("   ...asking the SSH host for its windows".into()),
            Some(Err(e)) => lines.push(format!("   ({e})")),
            Some(Ok(list)) => {
                for (i, w) in list.iter().enumerate() {
                    lines.push(format!("   {:<6} {}: {}  {}x{}", i + 1, clean(&w.app), clean(&w.title), w.w, w.h));
                }
                if !list.is_empty() {
                    lines.push(
                        "   A number shares that window; A and a number (A3) shares its whole application.".into(),
                    );
                }
            }
        }
        self.unpicture()?;
        let width = self.cols.min(100);
        let mut out = String::new();
        for (i, l) in lines.iter().take(self.rows.saturating_sub(2)).enumerate() {
            let l: String = l.chars().take(width).collect();
            let pad = width.saturating_sub(l.chars().count());
            out.push_str(&format!("\x1b[{};1H\x1b[0;48;5;236;97m{l}{}\x1b[0m", i + 1, " ".repeat(pad)));
        }
        self.out.write_all(out.as_bytes())?;
        self.status()
    }

    /// Ends the chooser: the picture under the list is painted again.
    pub(super) fn close_share_list(&mut self) -> io::Result<()> {
        self.share_list = None;
        self.dirty.clear();
        self.full_redraw()?;
        self.sess.send(ToAgent::Refresh)
    }

    pub(super) fn run_prompt(&mut self, kind: char, text: String) -> io::Result<()> {
        match kind {
            'v' => {
                let t = text.trim().to_string();
                let list = match &self.share_list {
                    Some(Ok(l)) => l.clone(),
                    _ => Vec::new(),
                };
                let pick = |n: &str| n.parse::<usize>().ok().and_then(|i| list.get(i.wrapping_sub(1))).map(|w| w.id);
                let share = match t.as_str() {
                    "" => Share::Screen,
                    "a" => Share::ActiveWindow,
                    "p" => Share::ActiveApp,
                    n if n.starts_with(['A', 'P']) => match pick(&n[1..]) {
                        Some(id) => Share::App(id),
                        None => return self.close_share_list().and_then(|_| self.notify(&format!("No window {n}"))),
                    },
                    n => match pick(n) {
                        Some(id) => Share::Window(id),
                        None => return self.close_share_list().and_then(|_| self.notify(&format!("No window {n}"))),
                    },
                };
                self.close_share_list()?;
                self.sess.send(ToAgent::Share(share))
            }
            'u' => {
                let dir = self.files.path.clone();
                self.upload_to(&text, &dir)
            }
            'n' => {
                let path = self.files.child(&text);
                self.sess.send(ToAgent::FileOp { op: crate::proto::FileOp::Mkdir, path, arg: String::new() })?;
                self.redraw()
            }
            'r' => {
                let Some(path) = self.target.take() else { return self.redraw() };
                self.sess.send(ToAgent::FileOp { op: crate::proto::FileOp::Rename, path, arg: text })?;
                self.redraw()
            }
            'g' => {
                self.sess.send(ToAgent::Fetch { tag: 0, path: text.clone() })?;
                self.notify(&format!("Downloading {text}..."))
            }
            _ => self.upload(&text),
        }
    }
}
