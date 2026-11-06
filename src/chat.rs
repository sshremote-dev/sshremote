//! Text chat between the viewer and the person at the remote screen. The agent
//! has no window of its own, so it starts `desk --chat-window` on the remote
//! desktop and talks to it over stdin/stdout, one escaped line per message.
//! Nothing is stored.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};

/// One message per line: backslashes and newlines escaped.
pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match (c, c == '\\') {
            (_, true) => match it.next() {
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            },
            (c, false) => out.push(c),
        }
    }
    out
}

/// Agent side: the chat window on the remote desktop, started on the first message.
#[derive(Default)]
pub struct Window {
    child: Option<(Child, ChildStdin)>,
    replies: Option<Receiver<String>>,
}

impl Window {
    fn alive(&mut self) -> bool {
        matches!(self.child.as_mut().map(|(c, _)| c.try_wait()), Some(Ok(None)))
    }

    /// Shows a message from the viewer, opening the window if the user closed it.
    pub fn show(&mut self, text: &str) -> Result<(), String> {
        if !self.alive() {
            self.open()?;
        }
        let (_, stdin) = self.child.as_mut().ok_or("no chat window")?;
        writeln!(stdin, "{}", escape(text)).and_then(|_| stdin.flush()).map_err(|e| e.to_string())
    }

    fn open(&mut self) -> Result<(), String> {
        let mut cmd = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        cmd.arg("--chat-window").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(target_os = "linux")]
        {
            // An agent started over ssh has no display variables; the desktop session uses the defaults.
            crate::capture::ensure_session_env();
            if std::env::var_os("WAYLAND_DISPLAY").is_none() {
                cmd.env("WAYLAND_DISPLAY", "wayland-0");
            }
            if std::env::var_os("DISPLAY").is_none() {
                cmd.env("DISPLAY", ":0");
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: the chat window itself, without a console behind it.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().map_err(|e| format!("chat window: {e}"))?;
        let stdin = child.stdin.take().ok_or("chat window stdin")?;
        let stdout = child.stdout.take().ok_or("chat window stdout")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(unescape(&line)).is_err() {
                    break;
                }
            }
        });
        self.child = Some((child, stdin));
        self.replies = Some(rx);
        Ok(())
    }

    /// Replies typed at the remote screen since the last call.
    pub fn replies(&mut self) -> Vec<String> {
        self.replies.as_ref().map(|r| r.try_iter().collect()).unwrap_or_default()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if let Some((mut c, _)) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// `desk --chat-window`: the small window shown on the remote desktop.
pub fn run_window() -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(unescape(&line)).is_err() {
                break;
            }
        }
        // The agent went away with the session.
        let _ = tx.send(String::from("\u{0}"));
    });
    let mut native = crate::gui::native_options("desk chat", [380.0, 460.0]);
    native.viewport = native.viewport.with_always_on_top();
    eframe::run_native(
        "desk chat",
        native,
        Box::new(move |cc| {
            crate::gui::style(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            let (wake_tx, wake_rx) = mpsc::channel::<String>();
            std::thread::spawn(move || {
                while let Ok(m) = rx.recv() {
                    if wake_tx.send(m).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
            Ok(Box::new(ChatApp { incoming: wake_rx, lines: Vec::new(), input: String::new(), ended: None }))
        }),
    )
    .map_err(|e| e.to_string())
}

struct ChatApp {
    incoming: Receiver<String>,
    /// (from the viewer, text)
    lines: Vec<(bool, String)>,
    input: String,
    /// When the viewer went away; the window closes itself a little later.
    ended: Option<std::time::Instant>,
}

impl eframe::App for ChatApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        for m in self.incoming.try_iter() {
            if m == "\u{0}" {
                self.ended = Some(std::time::Instant::now());
            } else {
                self.lines.push((true, m));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.label(RichText::new("Message from the person viewing this screen").color(Color32::GRAY));
            ui.separator();
            let input_h = 64.0;
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .max_height(ui.available_height() - input_h)
                .show(ui, |ui| show_lines(ui, &self.lines, "Viewer", "You"));
            if self.ended.is_some() {
                ui.colored_label(Color32::GRAY, "The viewer has disconnected; this window closes shortly.");
            }
            if let Some(text) = input_row(ui, &mut self.input, self.ended.is_none()) {
                println!("{}", escape(&text));
                use std::io::Write;
                let _ = std::io::stdout().flush();
                self.lines.push((false, text));
            }
        });
        if self.ended.is_some_and(|t| t.elapsed() > Duration::from_secs(10)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

/// Message bubbles; `them` and `me` label the two sides.
pub fn show_lines(ui: &mut egui::Ui, lines: &[(bool, String)], them: &str, me: &str) {
    for (theirs, text) in lines {
        let (who, fill, align) = if *theirs {
            (them, Color32::from_rgb(44, 48, 58), egui::Align::Min)
        } else {
            (me, Color32::from_rgb(40, 84, 160), egui::Align::Max)
        };
        ui.with_layout(egui::Layout::top_down(align), |ui| {
            ui.label(RichText::new(who).small().color(Color32::GRAY));
            egui::Frame::new().fill(fill).corner_radius(egui::CornerRadius::same(10)).inner_margin(8.0).show(
                ui,
                |ui| {
                    ui.set_max_width(260.0);
                    ui.label(RichText::new(text).color(Color32::WHITE));
                },
            );
        });
    }
}

/// A text field with a Send button; returns the text when sent.
pub fn input_row(ui: &mut egui::Ui, input: &mut String, enabled: bool) -> Option<String> {
    let mut sent = None;
    ui.horizontal(|ui| {
        let edit = ui.add_enabled(
            enabled,
            egui::TextEdit::singleline(input).hint_text("Type a message").desired_width(ui.available_width() - 70.0),
        );
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.add_enabled(enabled, egui::Button::new("Send")).clicked() || enter) && !input.trim().is_empty() {
            sent = Some(std::mem::take(input).trim().to_string());
            edit.request_focus();
        }
    });
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_survive_escaping() {
        for s in ["hi", "two\nlines", "back\\slash\\n literal", "안녕하세요"] {
            assert_eq!(unescape(&escape(s)), s);
            assert!(!escape(s).contains('\n'));
        }
    }
}
