//! Text chat between the viewer and the person at the remote screen. The agent
//! has no window of its own, so it starts `sshremote --chat-window` on the remote
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

/// `sshremote --chat-window`: the small window shown on the remote desktop.
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
    let mut native = crate::gui::native_options("SSH Remote chat", [380.0, 460.0]);
    native.viewport = native.viewport.with_always_on_top();
    eframe::run_native(
        "sshremote chat",
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
        crate::gui::chrome::behaviour(&ctx);
        if crate::gui::chrome::title_bar(ui, |ui| {
            ui.label(RichText::new("SSH Remote chat").size(13.0));
        })
        .close
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let card = ui.visuals().window_fill;
        let header = egui::Frame::new().fill(card).inner_margin(egui::Margin::symmetric(16, 10));
        egui::Panel::top("chat-header").frame(header).show(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::Vec2::splat(34.0), egui::Sense::hover());
                ui.painter().rect_filled(r, crate::gui::theme::R_LG, crate::gui::ACCENT);
                let icon = egui::FontId::proportional(17.0);
                ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, crate::gui::ic::CHAT, icon, Color32::WHITE);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(RichText::new("Viewer").strong().size(15.0));
                    let state = if self.ended.is_some() {
                        "disconnected; this window closes shortly"
                    } else {
                        "viewing this screen over SSH Remote"
                    };
                    ui.label(RichText::new(state).small().weak());
                });
            });
        });
        let footer = egui::Frame::new().fill(card).inner_margin(egui::Margin::symmetric(12, 10));
        egui::Panel::bottom("chat-input").frame(footer).show(ui, |ui| {
            if let Some(text) = input_row(ui, &mut self.input, self.ended.is_none()) {
                println!("{}", escape(&text));
                use std::io::Write;
                let _ = std::io::stdout().flush();
                self.lines.push((false, text));
            }
        });
        let body = egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(14, 10));
        egui::CentralPanel::default().frame(body).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| show_lines(ui, &self.lines, "Viewer", "You"));
        });
        if self.ended.is_some_and(|t| t.elapsed() > Duration::from_secs(10)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

/// The conversation as message bubbles, as in a messaging app: the other side's on the left with
/// their name over the first of a run, one's own on the right in the accent colour.
pub fn show_lines(ui: &mut egui::Ui, lines: &[(bool, String)], them: &str, me: &str) {
    let dark = ui.visuals().dark_mode;
    if lines.is_empty() {
        ui.add_space(ui.available_height() * 0.3);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(crate::gui::ic::CHAT).size(34.0).weak());
            ui.label(RichText::new("No messages yet").strong());
            ui.label(RichText::new(format!("Messages with {them} appear here.")).small().weak());
        });
        return;
    }
    let width = ui.available_width();
    for (i, (theirs, text)) in lines.iter().enumerate() {
        let first = i == 0 || lines[i - 1].0 != *theirs;
        let last = i + 1 == lines.len() || lines[i + 1].0 != *theirs;
        if first && i > 0 {
            ui.add_space(6.0);
        }
        let (fill, fg) = if *theirs {
            (
                if dark { Color32::from_rgb(44, 47, 56) } else { Color32::from_rgb(232, 235, 241) },
                ui.visuals().text_color(),
            )
        } else {
            (crate::gui::ACCENT, Color32::WHITE)
        };
        // The corner nearest the sender stays sharp on the last bubble of a run.
        let r = crate::gui::theme::R_LG;
        let radius = match (*theirs, last) {
            (true, true) => egui::CornerRadius { nw: r, ne: r, sw: 1, se: r },
            (false, true) => egui::CornerRadius { nw: r, ne: r, sw: r, se: 1 },
            _ => egui::CornerRadius::same(r),
        };
        let align = if *theirs { egui::Align::Min } else { egui::Align::Max };
        ui.with_layout(egui::Layout::top_down(align), |ui| {
            if first {
                ui.label(RichText::new(if *theirs { them } else { me }).small().weak());
            }
            ui.add_space(-6.0);
            egui::Frame::new().fill(fill).corner_radius(radius).inner_margin(egui::Margin::symmetric(12, 8)).show(
                ui,
                |ui| {
                    ui.set_max_width(width * 0.75);
                    ui.add(egui::Label::new(RichText::new(text).color(fg).size(14.0)).wrap().selectable(true));
                },
            );
        });
        ui.add_space(-6.0);
    }
}

/// A message field with a send button inside it; Enter sends. Returns the text when sent.
pub fn input_row(ui: &mut egui::Ui, input: &mut String, enabled: bool) -> Option<String> {
    let mut sent = None;
    let field = ui.visuals().extreme_bg_color;
    let line = ui.visuals().widgets.noninteractive.bg_stroke;
    egui::Frame::new()
        .fill(field)
        .stroke(line)
        .corner_radius(crate::gui::theme::R_MD)
        .inner_margin(egui::Margin { left: 14, right: 4, top: 4, bottom: 4 })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let edit = ui.add_enabled(
                    enabled,
                    egui::TextEdit::singleline(input)
                        .hint_text("Message")
                        .frame(egui::Frame::NONE)
                        .desired_width(ui.available_width() - 40.0)
                        .min_size(egui::Vec2::new(0.0, 30.0)),
                );
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ready = enabled && !input.trim().is_empty();
                let send = egui::Button::new(RichText::new(crate::gui::ic::SEND).size(16.0).color(Color32::WHITE))
                    .fill(if ready { crate::gui::ACCENT } else { crate::gui::ACCENT.gamma_multiply(0.4) })
                    .corner_radius(crate::gui::theme::R_SM)
                    .min_size(egui::Vec2::splat(32.0));
                if (ui.add_enabled(ready, send).on_hover_text("Send (Enter)").clicked() || enter) && ready {
                    sent = Some(std::mem::take(input).trim().to_string());
                    edit.request_focus();
                }
            });
        });
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_survive_escaping() {
        for s in ["hi", "two\nlines", "back\\slash\\n literal", "\u{C548}\u{B155}\u{D558}\u{C138}\u{C694}"] {
            assert_eq!(unescape(&escape(s)), s);
            assert!(!escape(s).contains('\n'));
        }
    }
}
