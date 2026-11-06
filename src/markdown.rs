//! A small Markdown view for terminal text: headings, lists, quotes, tables,
//! fenced code with a copy button, and inline bold, italics and code. Made for
//! reading what AI coding agents (Claude Code, Codex) print in a terminal.

use eframe::egui::{self, Color32, CornerRadius, FontId, RichText, Stroke, TextFormat, text::LayoutJob};

/// Box-drawing frames, prompt markers and padding that terminal agents draw around their text.
pub fn clean_agent_output(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_end();
        let core = t.trim_matches(|c: char| c.is_whitespace() || "│┃║|".contains(c));
        // Lines that are only frame (╭───╮, ╰───╯, ────) carry nothing.
        if !core.is_empty() && core.chars().all(|c| "─━═╭╮╰╯┌┐└┘├┤┬┴┼╌╍┄┅ ▔▁".contains(c)) {
            continue;
        }
        let mut l = t.trim_start_matches(|c: char| "│┃║".contains(c)).trim_end_matches(|c: char| "│┃║ ".contains(c));
        // A frame's left border leaves one space of padding behind.
        if t.starts_with(['│', '┃', '║']) {
            l = l.strip_prefix(' ').unwrap_or(l);
        }
        // Agent bullets and tool markers start a paragraph or a list item.
        let l = if let Some(rest) = l.trim_start().strip_prefix("⏺ ") {
            rest.to_string()
        } else if let Some(rest) = l.trim_start().strip_prefix("● ") {
            rest.to_string()
        } else if let Some(rest) = l.trim_start().strip_prefix("⎿ ") {
            format!("  {}", rest.trim_start())
        } else if let Some(rest) = l.trim_start().strip_prefix("• ") {
            format!("- {rest}")
        } else {
            l.to_string()
        };
        out.push(l);
    }
    // Runs of blank lines become one.
    let mut s = String::new();
    let mut blank = 0;
    for l in out {
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        s.push_str(&l);
        s.push('\n');
    }
    s.trim().to_string()
}

/// Inline Markdown (**bold**, *italics*, `code`, [text](url)) as a layout job.
fn inline(text: &str, size: f32, color: Color32, strong: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let plain = |job: &mut LayoutJob, s: &str, bold: bool, italic: bool| {
        if s.is_empty() {
            return;
        }
        let mut f = TextFormat { font_id: FontId::proportional(size), color, italics: italic, ..Default::default() };
        if bold {
            f.color = if color == Color32::PLACEHOLDER { color } else { color.gamma_multiply(1.2) };
            f.font_id = FontId::proportional(size);
            f.underline = Stroke::NONE;
        }
        // egui has no bold face in the default fonts; strong text is drawn brighter instead.
        if bold || strong {
            f.color = Color32::WHITE.lerp_to_gamma(color, 0.15);
        }
        job.append(s, 0.0, f);
    };
    let mut rest = text;
    let mut buf = String::new();
    let (mut bold, mut italic) = (false, false);
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('`')
            && let Some(end) = r.find('`')
        {
            plain(&mut job, &buf, bold, italic);
            buf.clear();
            let f = TextFormat {
                font_id: FontId::monospace(size * 0.92),
                color: Color32::from_rgb(230, 190, 120),
                background: Color32::from_white_alpha(18),
                ..Default::default()
            };
            job.append(&r[..end], 0.0, f);
            rest = &r[end + 1..];
            continue;
        }
        if let Some(r) = rest.strip_prefix("**").or_else(|| rest.strip_prefix("__")) {
            plain(&mut job, &buf, bold, italic);
            buf.clear();
            bold = !bold;
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix('*')
            && !r.starts_with(' ')
        {
            plain(&mut job, &buf, bold, italic);
            buf.clear();
            italic = !italic;
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix('[')
            && let Some(close) = r.find("](")
            && let Some(end) = r[close..].find(')')
        {
            plain(&mut job, &buf, bold, italic);
            buf.clear();
            let f = TextFormat {
                font_id: FontId::proportional(size),
                color: Color32::from_rgb(110, 170, 250),
                underline: Stroke::new(1.0, Color32::from_rgb(110, 170, 250)),
                ..Default::default()
            };
            job.append(&r[..close], 0.0, f);
            rest = &r[close + end + 1..];
            continue;
        }
        let c = rest.chars().next().unwrap();
        buf.push(c);
        rest = &rest[c.len_utf8()..];
    }
    plain(&mut job, &buf, bold, italic);
    job.wrap.max_width = f32::INFINITY;
    job
}

fn paragraph(ui: &mut egui::Ui, text: &str, size: f32, strong: bool) {
    let color = ui.visuals().text_color();
    let mut job = inline(text, size, color, strong);
    job.wrap.max_width = ui.available_width();
    ui.add(egui::Label::new(job).wrap().selectable(true));
}

fn code_block(ui: &mut egui::Ui, lang: &str, code: &str, id: usize) {
    egui::Frame::new()
        .fill(Color32::from_rgb(14, 15, 19))
        .stroke(Stroke::new(1.0, Color32::from_white_alpha(20)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(if lang.is_empty() { "code" } else { lang }).small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let copied = ui.ctx().data(|d| d.get_temp::<f64>(egui::Id::unique(("copied", id))));
                    let now = ui.input(|i| i.time);
                    let label = if copied.is_some_and(|t| now - t < 1.5) { "Copied" } else { "Copy" };
                    if ui.small_button(label).clicked() {
                        ui.ctx().copy_text(code.to_string());
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique(("copied", id)), now));
                    }
                });
            });
            ui.add(
                egui::Label::new(RichText::new(code).monospace().size(13.0).color(Color32::from_rgb(220, 223, 228)))
                    .selectable(true)
                    .extend(),
            );
        });
}

fn table(ui: &mut egui::Ui, rows: &[Vec<String>], id: usize) {
    egui::Frame::new().stroke(Stroke::new(1.0, Color32::from_white_alpha(25))).corner_radius(CornerRadius::same(6)).inner_margin(6.0).show(
        ui,
        |ui| {
            egui::Grid::new(("md-table", id)).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
                for (r, row) in rows.iter().enumerate() {
                    for cell in row {
                        paragraph(ui, cell.trim(), 14.0, r == 0);
                    }
                    ui.end_row();
                }
            });
        },
    );
}

/// Draws Markdown text.
pub fn show(ui: &mut egui::Ui, text: &str) {
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut block = 0;
    let mut para = String::new();
    let flush = |ui: &mut egui::Ui, para: &mut String| {
        if !para.trim().is_empty() {
            paragraph(ui, para.trim(), 14.5, false);
            ui.add_space(4.0);
        }
        para.clear();
    };
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        if let Some(lang) = t.strip_prefix("```") {
            flush(ui, &mut para);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push(lines[i]);
                i += 1;
            }
            block += 1;
            code_block(ui, lang.trim(), &code.join("\n"), block);
            ui.add_space(6.0);
            i += 1;
            continue;
        }
        if t.starts_with('|') && t.ends_with('|') && t.len() > 1 {
            flush(ui, &mut para);
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('|') {
                let r = lines[i].trim().trim_matches('|');
                let cells: Vec<String> = r.split('|').map(|c| c.trim().to_string()).collect();
                // The --- separator row is only for alignment.
                if !cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| "-: ".contains(ch))) {
                    rows.push(cells);
                }
                i += 1;
            }
            block += 1;
            table(ui, &rows, block);
            ui.add_space(6.0);
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            flush(ui, &mut para);
            let size = [22.0, 19.0, 17.0, 15.5, 15.0, 14.5][hashes - 1];
            ui.add_space(6.0);
            paragraph(ui, t[hashes..].trim(), size, true);
            ui.add_space(2.0);
            i += 1;
            continue;
        }
        if t.chars().filter(|c| !c.is_whitespace()).count() >= 3 && t.chars().all(|c| "-*_ ".contains(c)) {
            flush(ui, &mut para);
            ui.separator();
            i += 1;
            continue;
        }
        let bullet = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ "));
        let number = t.split_once(". ").filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if bullet.is_some() || number.is_some() {
            flush(ui, &mut para);
            let indent = (line.len() - t.len()) as f32 * 6.0;
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0 + indent);
                match number {
                    Some((n, rest)) if bullet.is_none() => {
                        ui.label(RichText::new(format!("{n}.")).weak());
                        paragraph(ui, rest, 14.5, false);
                    }
                    _ => {
                        ui.label(RichText::new("•").weak());
                        paragraph(ui, bullet.unwrap_or(t), 14.5, false);
                    }
                }
            });
            i += 1;
            continue;
        }
        if let Some(q) = t.strip_prefix("> ").or_else(|| (t == ">").then_some("")) {
            flush(ui, &mut para);
            egui::Frame::new().inner_margin(egui::Margin { left: 10, right: 0, top: 2, bottom: 2 }).show(ui, |ui| {
                let r = ui.available_rect_before_wrap();
                ui.painter().vline(r.left() - 6.0, r.y_range(), Stroke::new(3.0, Color32::from_white_alpha(40)));
                paragraph(ui, q, 14.5, false);
            });
            i += 1;
            continue;
        }
        if t.is_empty() {
            flush(ui, &mut para);
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(t);
        }
        i += 1;
    }
    flush(ui, &mut para);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_frames_are_removed() {
        let raw = "╭──────────╮\n│ hello    │\n╰──────────╯\n⏺ Done. Run:\n  ⎿  cargo test\n• item";
        assert_eq!(clean_agent_output(raw), "hello\nDone. Run:\n  cargo test\n- item");
    }
}
