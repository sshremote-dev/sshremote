//! Markdown for terminal text, drawn the way VS Code's preview draws it: headings with rules,
//! tables with borders, highlighted code blocks with a copy button, lists, quotes and links.
//! What AI coding agents (Claude Code, Codex) print is first split into prompts, replies and
//! tool runs, so each reply can be copied on its own.

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Sense, Stroke, TextFormat, Vec2,
    text::LayoutJob,
};

static BOLD: AtomicBool = AtomicBool::new(false);

fn bold_family() -> FontFamily {
    if BOLD.load(Ordering::Relaxed) { FontFamily::Name("bold".into()) } else { FontFamily::Proportional }
}

/// Loads a bold system face for headings and strong text; egui's own fonts have none.
pub fn install_bold(ctx: &egui::Context) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    let candidates: &[(&str, u32)] = if cfg!(windows) {
        &[(r"C:\Windows\Fonts\segoeuib.ttf", 0), (r"C:\Windows\Fonts\malgunbd.ttf", 0)]
    } else if cfg!(target_os = "macos") {
        &[("/System/Library/Fonts/Supplemental/Arial Bold.ttf", 0)]
    } else {
        &[
            ("/usr/share/fonts/truetype/ubuntu/Ubuntu-B.ttf", 0),
            ("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 0),
            ("/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc", 1),
        ]
    };
    for (i, (path, index)) in candidates.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let mut data = egui::FontData::from_owned(bytes);
        data.index = *index;
        let family = InsertFontFamily { family: FontFamily::Name("bold".into()), priority: FontPriority::Lowest };
        ctx.add_font(FontInsert { name: format!("bold-{i}"), data, families: vec![family] });
        BOLD.store(true, Ordering::Relaxed);
    }
}

/// A run of Markdown.
#[derive(Debug, Clone, PartialEq, Hash)]
pub enum Block {
    Heading(u8, String),
    Para(String),
    /// A paragraph or preformatted run that goes on in the next block: one long run is kept in
    /// parts so only the parts in view are drawn, and the parts are drawn with no space between.
    Part(Box<Block>),
    Code {
        lang: String,
        code: String,
    },
    Table {
        align: Vec<Align>,
        rows: Vec<Vec<String>>,
    },
    /// One line of a list: its depth, marker ("-", "3.", "[ ]", "[x]") and text.
    Item {
        depth: usize,
        marker: String,
        text: String,
    },
    Quote(Vec<Block>),
    Rule,
    /// Text laid out in columns (command output, listings), kept exactly as it is.
    Pre(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A document prepared for drawing.
pub struct Doc {
    pub blocks: Vec<Block>,
}

impl Doc {
    /// Any text as Markdown: a file shown with cat or less, a program's or an agent's output. What
    /// terminal programs draw with box characters and bullet glyphs is made plain Markdown first.
    pub fn new(text: &str) -> Self {
        let mut blocks = parse(&dedent(&glyphs(&tidy(text))));
        // A code block or preformatted run with nothing in it (a program's frame drawn empty
        // for a moment) would show as an empty box with a copy button.
        blocks.retain(|b| match b {
            Block::Code { code, .. } => !code.trim().is_empty(),
            Block::Pre(text) => !text.trim().is_empty(),
            _ => true,
        });
        Doc { blocks }
    }
}

/// Bullet glyphs that programs put before lines, read as Markdown: small bullets start list
/// items, and other markers (prompts, reply dots, output corners) give way to spaces so the text
/// keeps its column.
fn glyphs(text: &str) -> String {
    const ITEMS: [&str; 4] = ["• ", "◦ ", "▪ ", "‣ "];
    const MARKS: [&str; 7] = ["● ", "⏺ ", "❯ ", "› ", "⎿ ", "└ ", "→ "];
    text.lines()
        .map(|line| {
            let t = line.trim_start();
            let indent = &line[..line.len() - t.len()];
            if let Some(g) = ITEMS.iter().find(|g| t.starts_with(**g)) {
                return format!("{indent}- {}", &t[g.len()..]);
            }
            if let Some(g) = MARKS.iter().find(|g| t.starts_with(**g)) {
                return format!("{indent}{}{}", " ".repeat(g.chars().count()), &t[g.len()..]);
            }
            // Any other symbol a program sets before a line ("✻ Worked", "⎿ Read"): a marker, not
            // text. Letters of any script, Markdown's own marks, and box and block characters
            // (frames, quote bars) are kept.
            let mut chars = t.chars();
            if let (Some(c), Some(' ')) = (chars.next(), chars.next())
                && !c.is_ascii()
                && !c.is_alphanumeric()
                && !('\u{2500}'..='\u{259f}').contains(&c)
            {
                return format!("{indent}  {}", &t[c.len_utf8() + 1..]);
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Box-drawn tables become Markdown tables; frame lines and side borders go.
pub fn tidy(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if t.starts_with(['┌', '╭']) && t.contains('┬') {
            let indent = lines[i].len() - lines[i].trim_start().len();
            let (rows, used) = box_table(&lines[i + 1..]);
            if !rows.is_empty() {
                let pad = " ".repeat(indent);
                for (n, row) in rows.iter().enumerate() {
                    out.push(format!("{pad}| {} |", row.join(" | ")));
                    if n == 0 {
                        out.push(format!("{pad}|{}|", vec!["---"; row.len()].join("|")));
                    }
                }
                i += 1 + used;
                continue;
            }
        }
        let core = t.trim_matches(|c: char| c.is_whitespace() || "│┃║".contains(c));
        // Lines that are only frame (╭───╮, ╰───╯, ────) carry nothing.
        if !core.is_empty() && core.chars().all(|c| "─━═╭╮╰╯┌┐└┘├┤┬┴┼╌╍┄┅ ▔▁".contains(c))
        {
            i += 1;
            continue;
        }
        if lines[i].chars().filter(|c| ('\u{2580}'..='\u{259f}').contains(c)).count() >= 3 {
            i += 1;
            continue;
        }
        let line = lines[i].trim_end();
        if let Some(rest) = line.trim_start().strip_prefix('▎') {
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push(format!("{indent}>{rest}"));
            i += 1;
            continue;
        }
        let framed = line.trim_start().starts_with(['│', '┃', '║']);
        let l = if framed {
            let inner = line.trim_start().trim_start_matches(['│', '┃', '║']);
            let inner = inner.trim_end().trim_end_matches(['│', '┃', '║']).trim_end();
            inner.strip_prefix(' ').unwrap_or(inner).to_string()
        } else {
            line.to_string()
        };
        out.push(l);
        i += 1;
    }
    out.join("\n")
}

/// Rows of a box-drawn table whose top border was just read, and the lines it used. A row may
/// span several lines when its cells wrap; rows are split by ├──┼──┤ lines.
fn box_table(lines: &[&str]) -> (Vec<Vec<String>>, usize) {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut used = 0;
    let finish = |cur: &mut Vec<String>, rows: &mut Vec<Vec<String>>| {
        if !cur.is_empty() {
            rows.push(cur.iter().map(|c| c.trim().replace('|', "\\|")).collect());
            cur.clear();
        }
    };
    for l in lines {
        let t = l.trim();
        used += 1;
        if t.starts_with(['└', '╰']) {
            finish(&mut cur, &mut rows);
            return (rows, used);
        }
        if t.starts_with(['├', '╞', '┝']) {
            finish(&mut cur, &mut rows);
            continue;
        }
        if !t.starts_with('│') {
            used -= 1;
            break;
        }
        let cells: Vec<&str> = t.trim_matches('│').split('│').collect();
        if cur.is_empty() {
            cur = cells.iter().map(|c| c.trim().to_string()).collect();
        } else {
            for (k, c) in cells.iter().enumerate() {
                let c = c.trim();
                if let Some(slot) = cur.get_mut(k)
                    && !c.is_empty()
                {
                    if !slot.is_empty() {
                        slot.push(' ');
                    }
                    slot.push_str(c);
                }
            }
        }
    }
    // Cut off at the end of the screen: the rows so far.
    finish(&mut cur, &mut rows);
    if rows.is_empty() { (Vec::new(), 0) } else { (rows, used) }
}

/// Removes the indentation all non-blank lines share.
pub fn dedent(text: &str) -> String {
    let indent = |l: &str| l.chars().take_while(|c| c.is_whitespace()).count();
    let n = text.lines().filter(|l| !l.trim().is_empty()).map(indent).min().unwrap_or(0);
    text.lines().map(|l| l.chars().skip(n).collect::<String>().trim_end().to_string()).collect::<Vec<_>>().join("\n")
}

fn table_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    let mut cells = vec![String::new()];
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cells.last_mut().unwrap().push('|');
                chars.next();
            }
            '|' => cells.push(String::new()),
            c => cells.last_mut().unwrap().push(c),
        }
    }
    cells.into_iter().map(|c| c.trim().to_string()).collect()
}

fn is_separator(line: &str) -> bool {
    let cells = table_row(line);
    line.contains('-') && cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| "-: ".contains(ch)))
}

fn list_marker(t: &str) -> Option<(String, &str)> {
    for b in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(b) {
            for (task, m) in [("[ ] ", "[ ]"), ("[x] ", "[x]"), ("[X] ", "[x]")] {
                if let Some(r) = rest.strip_prefix(task) {
                    return Some((m.into(), r));
                }
            }
            return Some(("-".into(), rest));
        }
    }
    let (n, rest) = t.split_once(['.', ')'])?;
    (!n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit()) && rest.starts_with(' '))
        .then(|| (format!("{n}."), &rest[1..]))
}

/// A line laid out with spaces rather than written as prose: a gap of several spaces or a tab
/// between words, as in `dir`, `ls -l` or a table of numbers. Indentation alone is not enough.
pub fn columned(line: &str) -> bool {
    line.trim().contains('\t') || gaps(line).len() >= 2
}

/// Where columns start on a line: after each run of two or more spaces past the indent.
fn gaps(line: &str) -> Vec<usize> {
    let chars: Vec<char> = line.chars().collect();
    let start = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(chars.len());
    let (mut out, mut run) = (Vec::new(), 0);
    for (i, &c) in chars.iter().enumerate().skip(start) {
        if c == ' ' {
            run += 1;
        } else {
            if run >= 2 {
                out.push(i);
            }
            run = 0;
        }
    }
    out
}

/// Lines laid out in columns (`ls`, `ps`, aligned settings): a line with several columns, or
/// columns that start at the same place on two lines. A sentence with one double space is not.
pub fn columned_block(lines: &[&str]) -> bool {
    if lines.iter().any(|l| columned(l)) {
        return true;
    }
    let mut seen = std::collections::HashSet::new();
    lines.iter().flat_map(|l| gaps(l)).any(|p| !seen.insert(p))
}

/// Text in the terminal's own font with each character on the grid: wide characters (Korean,
/// Chinese, Japanese) take exactly two cells, as in the terminal and in an editor.
pub fn mono_job(text: &str, font: &FontId, color: Color32, fonts: &mut egui::epaint::FontsView<'_>) -> LayoutJob {
    use unicode_width::UnicodeWidthChar;
    let cell = fonts.glyph_width(font, 'M');
    // Every row as tall as the terminal font's, whichever font draws a character, so rows with
    // Korean keep the grid and the line numbers.
    let plain = TextFormat {
        font_id: font.clone(),
        color,
        line_height: Some(fonts.row_height(font)),
        valign: egui::Align::Center,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    let mut run_start = 0;
    for (i, c) in text.char_indices() {
        if c.is_ascii() {
            continue;
        }
        // A symbol from a fallback font is often narrower or wider than the cell; it is spaced to
        // take exactly its one or two cells, so the columns after it stay in line.
        let gap = c.width().unwrap_or(1).max(1) as f32 * cell - fonts.glyph_width(font, c);
        if gap.abs() > 0.25 {
            if run_start < i {
                job.append(&text[run_start..i], 0.0, plain.clone());
            }
            job.append(&text[i..i + c.len_utf8()], 0.0, TextFormat { extra_letter_spacing: gap, ..plain.clone() });
            run_start = i + c.len_utf8();
        }
    }
    if run_start < text.len() || text.is_empty() {
        job.append(&text[run_start..], 0.0, plain);
    }
    job.wrap.max_width = f32::INFINITY;
    job
}

/// Parses Markdown into blocks.
pub fn parse(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let flush_part = |para: &mut Vec<&str>, blocks: &mut Vec<Block>, more: bool| {
        if !para.is_empty() {
            let b = if columned_block(para) {
                Block::Pre(para.join("\n"))
            } else {
                // Lines of a paragraph join up, as in the preview: the terminal broke them only to fit.
                Block::Para(para.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" "))
            };
            blocks.push(if more { Block::Part(Box::new(b)) } else { b });
            para.clear();
        }
    };
    let flush = |para: &mut Vec<&str>, blocks: &mut Vec<Block>| flush_part(para, blocks, false);
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        let indent = line.len() - t.len();
        if t.starts_with("```") || t.starts_with("~~~") {
            flush(&mut para, &mut blocks);
            let fence = &t[..3];
            let lang = t[3..].trim().to_string();
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                code.push(lines[i].get(indent.min(lines[i].len() - lines[i].trim_start().len())..).unwrap_or(""));
                i += 1;
            }
            blocks.push(Block::Code { lang, code: code.join("\n") });
            i += 1;
            continue;
        }
        if t.starts_with('|') && i + 1 < lines.len() && is_separator(lines[i + 1]) {
            flush(&mut para, &mut blocks);
            let align = table_row(lines[i + 1])
                .iter()
                .map(|c| match (c.starts_with(':'), c.ends_with(':')) {
                    (true, true) => Align::Center,
                    (false, true) => Align::Right,
                    _ => Align::Left,
                })
                .collect();
            let mut rows = vec![table_row(line)];
            i += 2;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                rows.push(table_row(lines[i]));
                i += 1;
            }
            blocks.push(Block::Table { align, rows });
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && (t.len() == hashes || t[hashes..].starts_with(' ')) {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Heading(hashes as u8, t[hashes..].trim().trim_end_matches('#').trim().to_string()));
            i += 1;
            continue;
        }
        if t.chars().filter(|c| !c.is_whitespace()).count() >= 3
            && (t.chars().all(|c| c == '-' || c == ' ')
                || t.chars().all(|c| c == '*' || c == ' ')
                || t.chars().all(|c| c == '_' || c == ' '))
        {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Rule);
            i += 1;
            continue;
        }
        if let Some((marker, rest)) = list_marker(t) {
            flush(&mut para, &mut blocks);
            let mut text = rest.trim().to_string();
            // Lazy continuation lines belong to the item.
            while i + 1 < lines.len() {
                let next = lines[i + 1];
                let nt = next.trim_start();
                if nt.is_empty()
                    || list_marker(nt).is_some()
                    || next.len() - nt.len() <= indent
                    || nt.starts_with("```")
                {
                    break;
                }
                text.push(' ');
                text.push_str(nt.trim_end());
                i += 1;
            }
            blocks.push(Block::Item { depth: indent / 2, marker, text });
            i += 1;
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            flush(&mut para, &mut blocks);
            let mut inner = vec![q.strip_prefix(' ').unwrap_or(q).to_string()];
            i += 1;
            while i < lines.len()
                && let Some(q) = lines[i].trim_start().strip_prefix('>')
            {
                inner.push(q.strip_prefix(' ').unwrap_or(q).to_string());
                i += 1;
            }
            blocks.push(Block::Quote(parse(&inner.join("\n"))));
            continue;
        }
        if t.is_empty() {
            flush(&mut para, &mut blocks);
        } else if indent >= 4 && para.is_empty() && blocks.last().is_none_or(|b| !matches!(b, Block::Item { .. })) {
            // An indented block is code.
            let mut code = Vec::new();
            while i < lines.len() && (lines[i].trim().is_empty() || lines[i].len() - lines[i].trim_start().len() >= 4) {
                code.push(lines[i].get(4..).unwrap_or("").trim_end());
                i += 1;
            }
            while code.last().is_some_and(|l| l.is_empty()) {
                code.pop();
            }
            blocks.push(Block::Code { lang: String::new(), code: code.join("\n") });
            continue;
        } else {
            // A very long paragraph (output without blank lines) goes in parts, so a long document
            // is still drawn by the parts in view. A part is closed only when a line goes on after
            // it, so the last part is a plain paragraph with its space below.
            if para.len() >= 40 {
                flush_part(&mut para, &mut blocks, true);
            }
            para.push(line);
        }
        i += 1;
    }
    flush(&mut para, &mut blocks);
    blocks
}

/// Colours of VS Code's Dark Modern and Light Modern themes.
#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color32,
    pub fg: Color32,
    weak: Color32,
    rule: Color32,
    code_bg: Color32,
    inline_code: Color32,
    link: Color32,
    quote: Color32,
    table_head: Color32,
    table_alt: Color32,
    keyword: Color32,
    control: Color32,
    string: Color32,
    comment: Color32,
    number: Color32,
    function: Color32,
    ty: Color32,
}

impl Theme {
    pub fn of(ui: &egui::Ui) -> Self {
        Self::palette(ui.visuals().dark_mode)
    }

    /// The palette that suits the terminal's colour scheme, on its background and foreground.
    pub fn terminal(bg: Color32, fg: Color32) -> Self {
        let dark = bg.r() as u32 + bg.g() as u32 + bg.b() as u32 <= 3 * 128;
        let mut t = Self::palette(dark);
        t.bg = bg;
        t.fg = fg;
        let shade = if dark { Color32::WHITE } else { Color32::BLACK };
        t.code_bg = bg.lerp_to_gamma(shade, 0.05);
        t.table_head = bg.lerp_to_gamma(shade, 0.06);
        t.table_alt = bg.lerp_to_gamma(shade, 0.025);
        t.rule = bg.lerp_to_gamma(shade, 0.16);
        t.weak = bg.lerp_to_gamma(fg, 0.6);
        t
    }

    fn palette(dark: bool) -> Self {
        if dark {
            Theme {
                bg: Color32::from_rgb(31, 31, 31),
                fg: Color32::from_rgb(204, 204, 204),
                weak: Color32::from_rgb(140, 140, 140),
                rule: Color32::from_rgb(60, 60, 60),
                code_bg: Color32::from_rgb(24, 24, 24),
                inline_code: Color32::from_rgb(215, 186, 125),
                link: Color32::from_rgb(55, 148, 255),
                quote: Color32::from_rgb(70, 70, 70),
                table_head: Color32::from_rgb(40, 40, 40),
                table_alt: Color32::from_rgb(35, 35, 35),
                keyword: Color32::from_rgb(86, 156, 214),
                control: Color32::from_rgb(197, 134, 192),
                string: Color32::from_rgb(206, 145, 120),
                comment: Color32::from_rgb(106, 153, 85),
                number: Color32::from_rgb(181, 206, 168),
                function: Color32::from_rgb(220, 220, 170),
                ty: Color32::from_rgb(78, 201, 176),
            }
        } else {
            Theme {
                bg: Color32::WHITE,
                fg: Color32::from_rgb(31, 31, 31),
                weak: Color32::from_rgb(110, 110, 110),
                rule: Color32::from_rgb(220, 220, 220),
                code_bg: Color32::from_rgb(246, 248, 250),
                inline_code: Color32::from_rgb(163, 21, 21),
                link: Color32::from_rgb(0, 95, 184),
                quote: Color32::from_rgb(208, 215, 222),
                table_head: Color32::from_rgb(246, 248, 250),
                table_alt: Color32::from_rgb(250, 251, 252),
                keyword: Color32::from_rgb(0, 0, 255),
                control: Color32::from_rgb(175, 0, 219),
                string: Color32::from_rgb(163, 21, 21),
                comment: Color32::from_rgb(0, 128, 0),
                number: Color32::from_rgb(9, 134, 88),
                function: Color32::from_rgb(121, 94, 38),
                ty: Color32::from_rgb(38, 127, 153),
            }
        }
    }
}

const BODY: f32 = 14.0;

/// Where links sit in a laid-out paragraph, by character.
type Links = Vec<(std::ops::Range<usize>, String)>;

/// Inline Markdown (**bold**, *italics*, ~~struck~~, `code`, [text](url), bare URLs) as a layout job.
fn inline(text: &str, size: f32, th: &Theme, strong: bool) -> (LayoutJob, Links) {
    let mut job = LayoutJob::default();
    let mut links = Links::new();
    let mut chars = 0usize;
    let (mut bold, mut italic, mut strike) = (false, false, false);
    let fmt = |bold: bool, italic: bool, strike: bool| TextFormat {
        font_id: FontId::new(size, if bold || strong { bold_family() } else { FontFamily::Proportional }),
        color: th.fg,
        italics: italic,
        strikethrough: if strike { Stroke::new(1.0, th.fg) } else { Stroke::NONE },
        line_height: Some(size * 1.55),
        ..Default::default()
    };
    let mut buf = String::new();
    let push = |job: &mut LayoutJob, s: &str, f: TextFormat, chars: &mut usize| {
        if !s.is_empty() {
            job.append(s, 0.0, f);
            *chars += s.chars().count();
        }
    };
    let mut rest = text;
    while !rest.is_empty() {
        let flush_buf = |job: &mut LayoutJob, buf: &mut String, chars: &mut usize, b, it, st| {
            let s = std::mem::take(buf);
            push(job, &s, fmt(b, it, st), chars);
        };
        if let Some(r) = rest.strip_prefix('`')
            && let Some(end) = r.find('`')
        {
            flush_buf(&mut job, &mut buf, &mut chars, bold, italic, strike);
            let f = TextFormat {
                font_id: FontId::monospace(size * 0.9),
                color: th.inline_code,
                background: th.code_bg,
                line_height: Some(size * 1.55),
                ..Default::default()
            };
            push(&mut job, &r[..end], f, &mut chars);
            rest = &r[end + 1..];
            continue;
        }
        if let Some(r) = rest.strip_prefix("**").or_else(|| rest.strip_prefix("__")) {
            flush_buf(&mut job, &mut buf, &mut chars, bold, italic, strike);
            bold = !bold;
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix("~~") {
            flush_buf(&mut job, &mut buf, &mut chars, bold, italic, strike);
            strike = !strike;
            rest = r;
            continue;
        }
        // A star closes italics wherever it is, and opens them only before a word with a closing star to come.
        if let Some(r) = rest.strip_prefix('*').filter(|r| italic || (!r.starts_with(' ') && r.contains('*'))) {
            flush_buf(&mut job, &mut buf, &mut chars, bold, italic, strike);
            italic = !italic;
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix('[')
            && let Some(close) = r.find("](")
            && let Some(end) = r[close..].find(')')
        {
            flush_buf(&mut job, &mut buf, &mut chars, bold, italic, strike);
            let label = &r[..close];
            let url = &r[close + 2..close + end];
            let start = chars;
            let mut f = fmt(bold, italic, strike);
            f.color = th.link;
            push(&mut job, label, f, &mut chars);
            links.push((start..chars, url.to_string()));
            rest = &r[close + end + 1..];
            continue;
        }
        if (rest.starts_with("https://") || rest.starts_with("http://"))
            && buf.chars().last().is_none_or(|c| c.is_whitespace() || c == '(')
        {
            flush_buf(&mut job, &mut buf, &mut chars, bold, italic, strike);
            let end = rest.find(|c: char| c.is_whitespace() || c == ')' || c == '>').unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':']);
            let start = chars;
            let mut f = fmt(bold, italic, strike);
            f.color = th.link;
            push(&mut job, url, f, &mut chars);
            links.push((start..chars, url.to_string()));
            rest = &rest[url.len()..];
            continue;
        }
        if let Some(r) = rest.strip_prefix('\\')
            && let Some(c) = r.chars().next().filter(|c| c.is_ascii_punctuation())
        {
            buf.push(c);
            rest = &r[1..];
            continue;
        }
        let c = rest.chars().next().unwrap();
        buf.push(c);
        rest = &rest[c.len_utf8()..];
    }
    push(&mut job, &buf.clone(), fmt(bold, italic, strike), &mut chars);
    (job, links)
}

/// Lays out and draws inline text that wraps to the width, opens links on click and can be selected.
fn rich(ui: &mut egui::Ui, text: &str, size: f32, th: &Theme, strong: bool) {
    let (mut job, links) = inline(text, size, th, strong);
    job.wrap.max_width = ui.available_width();
    let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
    let r = ui.add(egui::Label::new(galley.clone()).selectable(true).sense(Sense::click()));
    if links.is_empty() {
        return;
    }
    let at = |p: Pos2| galley.cursor_from_pos(p - r.rect.min).index.0;
    let over = r.hover_pos().and_then(|p| links.iter().find(|(range, _)| range.contains(&at(p))));
    if let Some((_, url)) = over {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if r.clicked() {
            crate::termwin::open_url(url);
        }
        r.on_hover_text(url.as_str());
    }
}

/// A small copy button at the top right of `rect`, shown while the pointer is over it, as on
/// code blocks in VS Code and on GitHub.
fn hover_copy(ui: &mut egui::Ui, th: &Theme, rect: Rect, id: egui::Id, text: &str) {
    let now = ui.input(|i| i.time);
    let copied = ui.ctx().data(|d| d.get_temp::<f64>(id)).is_some_and(|t| now - t < 1.5);
    if !copied && !ui.rect_contains_pointer(rect) {
        return;
    }
    let glyph = if copied { crate::gui::ic::CHECK } else { crate::gui::ic::COPY };
    let at = Rect::from_min_size(Pos2::new(rect.right() - 34.0, rect.top() + 6.0), Vec2::splat(28.0));
    let b = egui::Button::new(egui::RichText::new(glyph).size(14.0).color(th.fg))
        .fill(th.code_bg)
        .stroke(Stroke::new(1.0, th.rule))
        .corner_radius(crate::gui::theme::R_SM);
    let tip = if copied { "Copied" } else { "Copy" };
    if ui.put(at, b).on_hover_text(tip).clicked() {
        ui.ctx().copy_text(text.to_string());
        ui.ctx().data_mut(|d| d.insert_temp(id, now));
    }
}

/// Colours code by a small tokenizer that knows comments, strings, numbers and the keywords of
/// common languages, close to what VS Code shows.
pub fn highlight(code: &str, lang: &str, th: &Theme, size: f32) -> LayoutJob {
    const KEYWORDS: &[&str] = &[
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "def",
        "default",
        "do",
        "elif",
        "else",
        "enum",
        "except",
        "export",
        "extends",
        "false",
        "finally",
        "fn",
        "for",
        "from",
        "func",
        "function",
        "if",
        "impl",
        "import",
        "in",
        "interface",
        "is",
        "lambda",
        "let",
        "loop",
        "match",
        "mod",
        "mut",
        "new",
        "nil",
        "None",
        "not",
        "null",
        "or",
        "and",
        "package",
        "pass",
        "pub",
        "raise",
        "return",
        "self",
        "Self",
        "static",
        "struct",
        "super",
        "switch",
        "then",
        "this",
        "throw",
        "trait",
        "true",
        "True",
        "False",
        "try",
        "type",
        "typeof",
        "use",
        "var",
        "void",
        "where",
        "while",
        "with",
        "yield",
        "fi",
        "done",
        "esac",
        "local",
        "echo",
        "export",
        "unsafe",
        "dyn",
        "ref",
        "crate",
        "extern",
        "public",
        "private",
        "protected",
        "readonly",
        "int",
        "char",
        "bool",
        "string",
        "float",
        "double",
        "long",
        "unsigned",
    ];
    const CONTROL: &[&str] = &[
        "if", "else", "elif", "for", "while", "loop", "match", "return", "break", "continue", "switch", "case", "try",
        "catch", "except", "finally", "throw", "raise", "await", "yield", "do", "then", "fi", "done", "esac", "import",
        "from", "use", "export",
    ];
    let lang = lang.to_ascii_lowercase();
    if matches!(lang.as_str(), "" | "text" | "txt" | "plain" | "output" | "log") {
        let mut job = LayoutJob::default();
        job.append(
            code,
            0.0,
            TextFormat {
                font_id: FontId::monospace(size),
                color: th.fg,
                line_height: Some(size * 1.5),
                ..Default::default()
            },
        );
        return job;
    }
    let hash_comment = matches!(
        lang.as_str(),
        "sh" | "bash"
            | "ssh-config"
            | "zsh"
            | "shell"
            | "console"
            | "python"
            | "py"
            | "ruby"
            | "rb"
            | "yaml"
            | "yml"
            | "toml"
            | "ini"
            | "conf"
            | "powershell"
            | "ps1"
            | "pwsh"
            | "perl"
            | "r"
            | "dockerfile"
            | "makefile"
            | "nix"
    );
    let dash_comment = matches!(lang.as_str(), "sql" | "lua" | "haskell" | "hs");
    let mono = FontId::monospace(size);
    let fmt = |c: Color32| TextFormat {
        font_id: mono.clone(),
        color: c,
        line_height: Some(size * 1.5),
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    let mut block_comment = false;
    for (n, line) in code.split('\n').enumerate() {
        if n > 0 {
            job.append("\n", 0.0, fmt(th.fg));
        }
        let b = line.as_bytes();
        let mut i = 0;
        while i < line.len() {
            let rest = &line[i..];
            if block_comment {
                let end = rest.find("*/").map_or(rest.len(), |e| e + 2);
                job.append(&rest[..end], 0.0, fmt(th.comment));
                block_comment = end == rest.len() && !rest.ends_with("*/");
                i += end;
                continue;
            }
            let comment_start = rest.starts_with("//") && !hash_comment
                || (hash_comment && rest.starts_with('#'))
                || (dash_comment && rest.starts_with("--"));
            if comment_start {
                job.append(rest, 0.0, fmt(th.comment));
                break;
            }
            if rest.starts_with("/*") && !hash_comment {
                let end = rest[2..].find("*/").map_or(rest.len(), |e| e + 4);
                block_comment = end == rest.len() && !rest.ends_with("*/");
                job.append(&rest[..end], 0.0, fmt(th.comment));
                i += end;
                continue;
            }
            let c = b[i];
            if c == b'"' || c == b'\'' || c == b'`' {
                // A Rust lifetime ('a) is not a string.
                let mut j = i + 1;
                while j < b.len() && b[j] != c {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                let end = (j + 1).min(line.len());
                let s = line.get(i..end).unwrap_or(rest);
                let lifetime = c == b'\'' && lang == "rust" && !s.ends_with('\'');
                job.append(s, 0.0, fmt(if lifetime { th.ty } else { th.string }));
                i += s.len();
                continue;
            }
            if c.is_ascii_digit() {
                let end =
                    rest.find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_')).unwrap_or(rest.len());
                job.append(&rest[..end], 0.0, fmt(th.number));
                i += end;
                continue;
            }
            if c.is_ascii_alphabetic() || c == b'_' || (c == b'$' && hash_comment) {
                let end =
                    1 + rest[1..].find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')).unwrap_or(rest.len() - 1);
                let word = &rest[..end];
                let next = rest[end..].trim_start();
                let color = if c == b'$' {
                    th.number
                } else if CONTROL.contains(&word) {
                    th.control
                } else if KEYWORDS.contains(&word) {
                    th.keyword
                } else if next.starts_with('(') || next.starts_with("!(") {
                    th.function
                } else if word.starts_with(|ch: char| ch.is_ascii_uppercase()) && word.len() > 1 {
                    th.ty
                } else {
                    th.fg
                };
                job.append(word, 0.0, fmt(color));
                i += end;
                continue;
            }
            let ch = rest.chars().next().unwrap();
            job.append(&rest[..ch.len_utf8()], 0.0, fmt(th.fg));
            i += ch.len_utf8();
        }
    }
    job
}

fn code_block(ui: &mut egui::Ui, th: &Theme, lang: &str, code: &str, id: egui::Id) {
    let rect = egui::Frame::new()
        .fill(th.code_bg)
        .stroke(Stroke::new(1.0, th.rule))
        .corner_radius(CornerRadius::same(crate::gui::theme::R_SM))
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::horizontal().id_salt(id).auto_shrink([false, true]).show(ui, |ui| {
                let mut job = highlight(code, lang, th, 13.0);
                job.wrap.max_width = f32::INFINITY;
                let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
                ui.add(egui::Label::new(galley).selectable(true).extend());
            });
        })
        .response
        .rect;
    hover_copy(ui, th, rect, id, code);
}

/// Command output and other aligned text: the terminal's font, every column where it was.
fn pre(ui: &mut egui::Ui, th: &Theme, text: &str) {
    let font = FontId::monospace(13.0);
    egui::ScrollArea::horizontal().id_salt(ui.next_auto_id()).auto_shrink([false, true]).show(ui, |ui| {
        let job = ui.ctx().fonts_mut(|f| mono_job(text, &font, th.fg, f));
        let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
        ui.add(egui::Label::new(galley).selectable(true).extend());
    });
}

fn table(ui: &mut egui::Ui, th: &Theme, align: &[Align], rows: &[Vec<String>], id: egui::Id) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return;
    }
    let pad = Vec2::new(13.0, 6.0);
    // Column widths from the cells' natural widths, each capped so long text wraps.
    let lay = |ui: &egui::Ui, text: &str, head: bool, width: f32| {
        let (mut job, _) = inline(text, BODY, th, head);
        job.wrap.max_width = width;
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    };
    let mut widths = vec![30.0f32; cols];
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            widths[c] = widths[c].max(lay(ui, cell, r == 0, f32::INFINITY).size().x.min(380.0));
        }
    }
    let shown = egui::ScrollArea::horizontal().id_salt(id).auto_shrink([false, true]).show(ui, |ui| {
        let total: f32 = widths.iter().map(|w| w + 2.0 * pad.x).sum();
        let galleys: Vec<Vec<_>> = rows
            .iter()
            .enumerate()
            .map(|(r, row)| {
                (0..cols).map(|c| lay(ui, row.get(c).map_or("", |s| s.as_str()), r == 0, widths[c])).collect()
            })
            .collect();
        let heights: Vec<f32> =
            galleys.iter().map(|row| row.iter().map(|g| g.size().y).fold(0.0, f32::max) + 2.0 * pad.y).collect();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(total, heights.iter().sum()), Sense::hover());
        let p = ui.painter();
        let mut y = rect.top();
        for (r, row) in galleys.iter().enumerate() {
            let h = heights[r];
            let fill = if r == 0 {
                th.table_head
            } else if r % 2 == 0 {
                th.table_alt
            } else {
                th.bg
            };
            p.rect_filled(
                Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(total, h)),
                CornerRadius::ZERO,
                fill,
            );
            let mut x = rect.left();
            for (c, g) in row.iter().enumerate() {
                let w = widths[c] + 2.0 * pad.x;
                let free = widths[c] - g.size().x;
                let dx = match align.get(c).copied().unwrap_or(Align::Left) {
                    Align::Left => 0.0,
                    Align::Center => free / 2.0,
                    Align::Right => free,
                };
                p.galley(Pos2::new(x + pad.x + dx, y + pad.y), g.clone(), th.fg);
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h)),
                    CornerRadius::ZERO,
                    Stroke::new(1.0, th.rule),
                    egui::StrokeKind::Inside,
                );
                x += w;
            }
            y += h;
        }
    });
    // Copied as tab-separated text, which spreadsheets take as cells.
    let tsv: String = rows.iter().map(|r| r.join("	")).collect::<Vec<_>>().join(
        "
",
    );
    let rect = shown.inner_rect.intersect(Rect::from_min_size(shown.inner_rect.min, shown.content_size));
    hover_copy(ui, th, rect, id.with("copy"), &tsv);
}

fn heading(ui: &mut egui::Ui, th: &Theme, level: u8, text: &str) {
    let size = [26.0, 21.0, 17.5, 15.0, 14.0, 13.0][(level as usize).clamp(1, 6) - 1];
    ui.add_space(if level <= 2 { 14.0 } else { 8.0 });
    rich(ui, text, size, th, true);
    if level <= 2 {
        let r = ui.available_rect_before_wrap();
        ui.painter().hline(r.x_range(), r.top() + 2.0, Stroke::new(1.0, th.rule));
        ui.add_space(8.0);
    }
}

/// Draws blocks; `id` keeps the copy buttons and scroll areas of separate documents apart.
pub fn blocks(ui: &mut egui::Ui, th: &Theme, blocks: &[Block], id: egui::Id) {
    for n in 0..blocks.len() {
        block(ui, th, blocks, n, id);
    }
}

/// Draws `blocks[n]`; the others decide spacing (a list item followed by another stays close).
fn block(ui: &mut egui::Ui, th: &Theme, blocks: &[Block], n: usize, id: egui::Id) {
    let b = &blocks[n];
    let bid = id.with(n);
    match b {
        Block::Heading(level, text) => heading(ui, th, *level, text),
        Block::Pre(text) => {
            pre(ui, th, text);
            ui.add_space(6.0);
        }
        Block::Para(text) => {
            rich(ui, text, BODY, th, false);
            ui.add_space(6.0);
        }
        Block::Part(inner) => match &**inner {
            Block::Para(text) => rich(ui, text, BODY, th, false),
            Block::Pre(text) => pre(ui, th, text),
            _ => {}
        },
        Block::Code { lang, code } => {
            code_block(ui, th, lang, code, bid);
            ui.add_space(8.0);
        }
        Block::Table { align, rows } => {
            table(ui, th, align, rows, bid);
            ui.add_space(8.0);
        }
        Block::Item { depth, marker, text } => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_space(6.0 + *depth as f32 * 22.0);
                let glyph = match marker.as_str() {
                    "-" => ["•", "◦", "▪"][depth % 3].to_string(),
                    "[ ]" => "☐".into(),
                    "[x]" => "☑".into(),
                    m => m.to_string(),
                };
                let (r, _) = ui.allocate_exact_size(Vec2::new(18.0, BODY * 1.55), Sense::hover());
                ui.painter().text(r.right_center(), Align2::RIGHT_CENTER, glyph, FontId::proportional(BODY), th.weak);
                ui.vertical(|ui| rich(ui, text, BODY, th, false));
            });
            if !matches!(blocks.get(n + 1), Some(Block::Item { .. })) {
                ui.add_space(6.0);
            }
        }
        Block::Quote(inner) => {
            let r = egui::Frame::new()
                .inner_margin(egui::Margin { left: 16, right: 4, top: 2, bottom: 2 })
                .show(ui, |ui| {
                    let mut weak = *th;
                    weak.fg = th.weak;
                    self::blocks(ui, &weak, inner, bid);
                })
                .response
                .rect;
            ui.painter().vline(r.left() + 2.0, r.y_range(), Stroke::new(4.0, th.quote));
            ui.add_space(6.0);
        }
        Block::Rule => {
            ui.add_space(6.0);
            let r = ui.available_rect_before_wrap();
            ui.painter().hline(r.x_range(), r.top(), Stroke::new(2.0, th.rule));
            ui.add_space(10.0);
        }
    }
}

/// A document's key and width, each block's top and height, and where it ends.
type Layout = (u64, u32, Vec<(f32, f32)>, f32);

/// Where a document's blocks go: heights measured once per block content and width (kept across
/// documents, since a growing output repeats most blocks), and for the current document and width
/// each block's top, so the blocks in view are found by binary search.
#[derive(Default)]
pub struct Heights {
    measured: std::collections::HashMap<(u64, bool, u32), f32>,
    /// The document (its key) and width the tops are for; each block's top and height.
    layout: Option<Layout>,
}

impl Heights {
    /// Each block's top and height in the document last laid out.
    pub fn tops(&self) -> &[(f32, f32)] {
        self.layout.as_ref().map_or(&[], |l| l.2.as_slice())
    }

    fn key(blocks: &[Block], n: usize, width: f32) -> (u64, bool, u32) {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        blocks[n].hash(&mut h);
        (h.finish(), matches!(blocks.get(n + 1), Some(Block::Item { .. })), width.round() as u32)
    }
}

/// Draws the blocks of `doc` that meet `view` (in the coordinates of the scroll area's content,
/// whose top left is at `origin`), in a column at `x` (from the content's left) `width` wide,
/// starting `top` below the content's top. `doc_key` names the document: its layout is worked out
/// when it or the width changes, and each frame only looks up and draws the blocks in view.
/// Returns where the document ends, in content coordinates.
#[allow(clippy::too_many_arguments)]
pub fn show_in_view(
    ui: &mut egui::Ui,
    doc: &Doc,
    doc_key: u64,
    id: egui::Id,
    th: &Theme,
    heights: &mut Heights,
    view: Rect,
    x: f32,
    width: f32,
    top: f32,
) -> f32 {
    let end = lay_out(ui, doc, doc_key, id, th, heights, x, width, top);
    let origin = ui.max_rect().min;
    let (_, _, tops, _) = heights.layout.as_ref().expect("just laid out");
    // In view, with a screen's margin either side so scrolling does not show gaps.
    let (from, to) = (view.top() - view.height(), view.bottom() + view.height());
    let first = tops.partition_point(|&(y, h)| y + h < from);
    for (n, &(y, h)) in tops.iter().enumerate().skip(first) {
        if y > to {
            break;
        }
        let rect = Rect::from_min_size(origin + Vec2::new(x, y), Vec2::new(width, h.max(1.0)));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .id_salt(("block", n))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        block(&mut child, th, &doc.blocks, n, id);
    }
    end
}

/// Where the document ends (see [`show_in_view`]), worked out when it or the width changes, so it
/// is known before it is drawn.
#[allow(clippy::too_many_arguments)]
pub fn lay_out(ui: &mut egui::Ui, doc: &Doc, doc_key: u64, id: egui::Id, th: &Theme, heights: &mut Heights, x: f32, width: f32, top: f32) -> f32 {
    let origin = ui.max_rect().min;
    let gap = ui.spacing().item_spacing.y;
    let w = width.round() as u32;
    if heights.layout.as_ref().is_none_or(|l| l.0 != doc_key || l.1 != w) {
        if heights.measured.len() > 20_000 {
            heights.measured.clear();
        }
        // Blocks not seen before at this width are measured without being drawn.
        let mut tops = Vec::with_capacity(doc.blocks.len());
        let mut y = top;
        for n in 0..doc.blocks.len() {
            let key = Heights::key(&doc.blocks, n, width);
            let h = *heights.measured.entry(key).or_insert_with(|| {
                let rect = Rect::from_min_size(Pos2::new(origin.x + x, -1.0e6), Vec2::new(width, f32::INFINITY));
                let mut probe = ui
                    .new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("measure", n)).sizing_pass().invisible());
                block(&mut probe, th, &doc.blocks, n, id.with("measure"));
                probe.min_rect().height()
            });
            tops.push((y, h));
            y += h + if matches!(doc.blocks[n], Block::Part(_)) { 0.0 } else { gap };
        }
        heights.layout = Some((doc_key, w, tops, y));
    }
    heights.layout.as_ref().expect("just laid out").3
}

/// A block's words, to search.
pub fn block_text(b: &Block) -> String {
    match b {
        Block::Heading(_, t) | Block::Para(t) | Block::Pre(t) => t.clone(),
        Block::Part(b) => block_text(b),
        Block::Code { code, .. } => code.clone(),
        Block::Table { rows, .. } => rows.iter().map(|r| r.join(" ")).collect::<Vec<_>>().join("\n"),
        Block::Item { marker, text, .. } => format!("{marker} {text}"),
        Block::Quote(bs) => bs.iter().map(block_text).collect::<Vec<_>>().join("\n"),
        Block::Rule => String::new(),
    }
}

/// Draws a whole document in the colours of `th` (the background it is drawn on).
#[cfg(test)]
pub fn show(ui: &mut egui::Ui, doc: &Doc, id: egui::Id, th: &Theme) {
    blocks(ui, th, &doc.blocks, id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a program prints around its answer reads as Markdown in general, whichever program:
    /// a symbol before a line is a marker, a sentence with one double space is prose.
    #[test]
    fn program_output_reads_as_markdown() {
        let raw = "\u{273b} Worked for 9s\n  - first\n  - second\n\n  Created in 2004.[^1]  [^1]: by Gruber.";
        let b = Doc::new(raw).blocks;
        assert_eq!(b[0], Block::Para("Worked for 9s".into()));
        assert!(matches!(&b[1], Block::Item { depth: 0, .. }), "{:?}", b[1]);
        assert!(matches!(&b[3], Block::Para(t) if t.contains("[^1]: by Gruber")), "{:?}", b[3]);
        // Real columns stay preformatted.
        let b = Doc::new("NAME    SIZE  DATE\nfile.txt  12  May 3\nlib.rs  4096  May 4").blocks;
        assert!(matches!(&b[0], Block::Pre(_)), "{:?}", b[0]);
    }

    #[test]
    fn split_paragraphs_end_and_go_on_as_paragraphs() {
        let lines = |n: usize| (0..n).map(|k| format!("line {k}")).collect::<Vec<_>>().join("\n");
        // Exactly 40 lines, then something else: one plain paragraph, with its space below.
        let b = parse(&format!("{}\n\nother", lines(40)));
        assert!(matches!(&b[0], Block::Para(_)), "{:?}", b[0]);
        assert_eq!(b[1], Block::Para("other".into()));
        // An indented line going on after 40 is still the paragraph, not a code block.
        let b = parse(&format!("{}\n    at Foo.bar()\n    at Baz.qux()\nend", lines(40)));
        assert!(b.iter().all(|b| !matches!(b, Block::Code { .. })), "{b:?}");
        assert!(matches!(&b[1], Block::Para(t) if t.starts_with("at Foo.bar()")), "{:?}", b[1]);
    }

    #[test]
    fn long_paragraphs_go_in_joined_parts() {
        let text: Vec<String> = (0..100).map(|k| format!("line {k}")).collect();
        let b = parse(&text.join("\n"));
        assert_eq!(b.len(), 3);
        assert!(matches!(&b[0], Block::Part(p) if matches!(&**p, Block::Para(t) if t.starts_with("line 0 ") && t.ends_with("line 39"))));
        assert!(matches!(&b[1], Block::Part(_)));
        assert_eq!(b[2], Block::Para((80..100).map(|k| format!("line {k}")).collect::<Vec<_>>().join(" ")));
    }

    #[test]
    fn box_tables_become_markdown() {
        let raw = "┌──────┬───────┐\n│ Name │ Value │\n├──────┼───────┤\n│ a    │ long  │\n│      │ text  │\n├──────┼───────┤\n│ b    │ 2     │\n└──────┴───────┘";
        assert_eq!(tidy(raw), "| Name | Value |\n|---|---|\n| a | long text |\n| b | 2 |");
        match &parse(&tidy(raw))[0] {
            Block::Table { rows, .. } => assert_eq!(rows[1], ["a", "long text"]),
            b => panic!("{b:?}"),
        }
    }

    #[test]
    fn drawn_markdown_reads_as_markdown() {
        let raw = " ▐▛███▛█   Tool v2\n\n❯ give me a story\n\n● # The Log\n\n  ▎ Three short.\n\n  • first\n\n  ---\n\n  ┌──────┬──────┐\n  │ Time │ Note │\n  ├──────┼──────┤\n  │ 21:04│ SOS  │";
        let b = Doc::new(raw).blocks;
        assert_eq!(b[0], Block::Para("give me a story".into()));
        assert_eq!(b[1], Block::Heading(1, "The Log".into()));
        assert_eq!(b[2], Block::Quote(vec![Block::Para("Three short.".into())]));
        assert_eq!(b[3], Block::Item { depth: 0, marker: "-".into(), text: "first".into() });
        assert_eq!(b[4], Block::Rule);
        assert!(matches!(&b[5], Block::Table { rows, .. } if rows.len() == 2), "{b:?}");
        assert_eq!(b.len(), 6, "{b:?}");
    }

    #[test]
    fn markdown_blocks_parse() {
        let b = parse(
            "# Title\n\nSome *text*\nmore.\n\n- one\n  - two\n1. three\n- [x] done\n\n```rust\nfn main() {}\n```\n\n| a | b |\n|:-|-:|\n| 1 | 2 |\n\n> quoted\n\n---",
        );
        assert_eq!(b[0], Block::Heading(1, "Title".into()));
        assert_eq!(b[1], Block::Para("Some *text* more.".into()));
        assert_eq!(b[2], Block::Item { depth: 0, marker: "-".into(), text: "one".into() });
        assert_eq!(b[3], Block::Item { depth: 1, marker: "-".into(), text: "two".into() });
        assert_eq!(b[4], Block::Item { depth: 0, marker: "1.".into(), text: "three".into() });
        assert_eq!(b[5], Block::Item { depth: 0, marker: "[x]".into(), text: "done".into() });
        assert_eq!(b[6], Block::Code { lang: "rust".into(), code: "fn main() {}".into() });
        assert_eq!(
            b[7],
            Block::Table {
                align: vec![Align::Left, Align::Right],
                rows: vec![vec!["a".into(), "b".into()], vec!["1".into(), "2".into()]]
            }
        );
        assert_eq!(b[8], Block::Quote(vec![Block::Para("quoted".into())]));
        assert_eq!(b[9], Block::Rule);
    }

    #[test]
    fn inline_links_are_found() {
        let th = Theme {
            bg: Color32::BLACK,
            fg: Color32::WHITE,
            weak: Color32::GRAY,
            rule: Color32::GRAY,
            code_bg: Color32::BLACK,
            inline_code: Color32::WHITE,
            link: Color32::BLUE,
            quote: Color32::GRAY,
            table_head: Color32::BLACK,
            table_alt: Color32::BLACK,
            keyword: Color32::BLUE,
            control: Color32::BLUE,
            string: Color32::RED,
            comment: Color32::GREEN,
            number: Color32::GREEN,
            function: Color32::YELLOW,
            ty: Color32::GREEN,
        };
        let (job, _) = inline("*one,* *\"two.\"*", 14.0, &th, false);
        assert_eq!(job.text, "one, \"two.\"");
        let (job, links) = inline("see [docs](https://a.b) or https://c.d.", 14.0, &th, false);
        assert_eq!(job.text, "see docs or https://c.d.");
        assert_eq!(links, vec![(4..8, "https://a.b".to_string()), (12..23, "https://c.d".to_string())]);
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    /// Frame times for a long document drawn whole and drawn by what is in view:
    /// cargo test --release -- --ignored --nocapture draws_only_what_is_in_view
    #[test]
    #[ignore]
    fn draws_only_what_is_in_view() {
        let mut text = String::new();
        for k in 0..400 {
            text.push_str(&format!(
                "## Part {k}\n\nSome **bold** and *italic* text with `code` and a [link](https://a.b) in it, long enough to wrap once or twice in the column.\n\n- one\n- two\n\n```rust\nfn main() {{ let x = {k}; }}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n"
            ));
        }
        let doc = Doc::new(&text);
        let ctx = egui::Context::default();
        let th = Theme::palette(true);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0))),
            ..Default::default()
        };
        let time = |f: &mut dyn FnMut(&mut egui::Ui)| {
            let _ = ctx.run_ui(input(), |ui| f(ui));
            let start = std::time::Instant::now();
            for _ in 0..10 {
                let _ = ctx.run_ui(input(), |ui| f(ui));
            }
            start.elapsed().as_secs_f64() * 100.0
        };
        let whole = time(&mut |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| show(ui, &doc, egui::Id::unique("whole"), &th));
        });
        let mut heights = Heights::default();
        let lazy = time(&mut |ui| {
            egui::ScrollArea::vertical().id_salt("lazy").show_viewport(ui, |ui, view| {
                let end =
                    show_in_view(ui, &doc, 1, egui::Id::unique("lazy"), &th, &mut heights, view, 20.0, 900.0, 0.0);
                let o = ui.max_rect().min;
                ui.allocate_rect(Rect::from_min_size(o, Vec2::new(940.0, end)), Sense::hover());
            });
        });
        eprintln!("{} blocks: whole {whole:.2} ms a frame, in view {lazy:.2} ms a frame", doc.blocks.len());
        assert!(lazy * 5.0 < whole, "drawing what is in view is much quicker");
    }
}
