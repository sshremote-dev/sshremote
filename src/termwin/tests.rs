use crate::gui::snapshot::RunTest;
use super::*;

#[test]
fn tab_titles_are_short() {
    assert_eq!(tab_title(r"Administrator: C:\WINDOWS\System32\WindowsPowerShell1.0\powershell.exe"), "powershell");
    assert_eq!(tab_title("a@server: ~"), "a@server: ~");
}

#[test]
fn keys_become_terminal_bytes() {
    let none = egui::Modifiers::NONE;
    assert_eq!(key_bytes(Key::ArrowUp, none, false).unwrap(), b"\x1b[A");
    assert_eq!(key_bytes(Key::ArrowUp, none, true).unwrap(), b"\x1bOA");
    assert_eq!(key_bytes(Key::ArrowLeft, egui::Modifiers::CTRL, false).unwrap(), b"\x1b[1;5D");
    assert_eq!(key_bytes(Key::C, egui::Modifiers::CTRL, false).unwrap(), [3]);
    assert_eq!(key_bytes(Key::F5, none, false).unwrap(), b"\x1b[15~");
    assert_eq!(key_bytes(Key::B, egui::Modifiers::ALT, false).unwrap(), b"\x1bb");
    assert!(key_bytes(Key::A, none, false).is_none());
    assert_eq!(key_bytes(Key::Enter, egui::Modifiers::ALT, false).unwrap(), b"\x1b\r", "Meta+Return");
    assert_eq!(key_bytes(Key::Space, egui::Modifiers::CTRL, false).unwrap(), [0], "Ctrl+Space is NUL");
    if !cfg!(windows) {
        assert_eq!(key_bytes(Key::X, egui::Modifiers::CTRL | egui::Modifiers::ALT, false).unwrap(), [0x1b, 0x18]);
    }
}

/// Alt+letter goes out once, as ESC and the letter: the text egui sends after the key is not
/// typed again.
#[test]
fn alt_letters_go_out_once() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    frame(&ctx, &mut w, Vec::new());
    let key = |key| Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: egui::Modifiers::ALT };
    let out = frame(&ctx, &mut w, vec![key(Key::B), Event::Text("b".into()), key(Key::Period), Event::Text(".".into())]);
    assert_eq!(typed(&out), "\x1bb\x1b.");
    // Plain text after an Alt key that sent nothing of its own is still typed.
    let out = frame(&ctx, &mut w, vec![Event::Text("x".into())]);
    assert_eq!(typed(&out), "x");
}

#[test]
fn the_emulator_answers_cursor_queries() {
    let (tx, rx) = mpsc::channel();
    let mut term = Term::new(Config::default(), &Size { cols: 20, rows: 5 }, Listener(tx));
    let mut p: Processor = Processor::new();
    p.advance(&mut term, b"ab\x1b[6n");
    let replies: Vec<String> =
        rx.try_iter().filter_map(|e| if let TermEvent::PtyWrite(s) = e { Some(s) } else { None }).collect();
    assert_eq!(replies, ["\x1b[1;3R"]);
}

/// A frame of the window with no real display, as the window would run it.
fn frame(ctx: &egui::Context, w: &mut TermWindow, events: Vec<Event>) -> Vec<Cmd> {
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
        focused: true,
        events,
        ..Default::default()
    };
    let mut out = Vec::new();
    let _ = ctx.run_test(input, |ui| out.extend(w.embedded(ui)));
    out
}

/// A pane's program that ends well closes its pane; a tool's stays, its output there to read.
#[cfg(unix)]
#[test]
fn a_tools_pane_stays_when_its_program_ends_well() {
    let ctx = egui::Context::default();
    for (launch, stays) in [(Launch::Local(vec!["true".into()]), false), (Launch::Tool(vec!["true".into()]), true)] {
        let mut w = TermWindow::single(Profile { name: "t".into(), icon: ic::TERMINAL, launch });
        let mut closed = false;
        for _ in 0..100 {
            closed |= frame(&ctx, &mut w, Vec::new()).iter().any(|c| matches!(c, Cmd::Close));
            if closed || w.tabs.first().is_some_and(|t| t.exited.is_some()) && stays {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for _ in 0..3 {
            closed |= frame(&ctx, &mut w, Vec::new()).iter().any(|c| matches!(c, Cmd::Close));
        }
        assert_eq!(!closed, stays, "stays: {stays}");
    }
}

/// Syntax-coloured code is fenced with its indentation, and a frame drawn right above the
/// prompt (as an agent's input box) stays with the prompt in the live rows.
#[test]
fn coloured_code_and_framed_prompts() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    let t = &mut w.tabs[0];
    let cols = t.term.grid().columns();
    let kw = "\x1b[38;5;170m";
    let st = "\x1b[38;5;114m";
    let r = "\x1b[0m";
    let out = format!(
        "\x1b[2J\x1b[HSome text\r\n\r\n  {kw}def{r} f(x):\r\n      {kw}return{r} {st}\"x\"{r}\r\n\r\n  after\r\n{}\r\n> \r\n{}\r\n  status\x1b[2A\r\x1b[2C",
        "\u{2500}".repeat(cols),
        "\u{2500}".repeat(cols)
    );
    t.parser.advance(&mut t.term, out.as_bytes());
    let (first, last) = reader::input_rows(t);
    let md = reader::screen_markdown(t, 0, first);
    assert!(md.contains("  ```\n  def f(x):\n      return \"x\"\n  ```"), "{md}");
    assert!(md.contains("after"), "{md}");
    assert!(!md.contains('\u{2500}'), "the frame goes with the prompt: {md}");
    assert_eq!(last - first, 3, "rule, prompt, rule, status");
}

/// Code as Claude Code colours it stays one block: rows left in the plain colour ("Empty,",
/// "Ok(x)", "}") join it, the hint it floats over its text is dropped, and a coloured prompt
/// marker or a heading does not make code.
#[test]
fn claude_code_blocks_stay_whole() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    let t = &mut w.tabs[0];
    let kw = "\x1b[38;5;170m";
    let ty = "\x1b[38;5;75m";
    let mk = "\x1b[38;5;244m";
    let r = "\x1b[0m";
    let out = format!(
        "\x1b[2J\x1b[H{mk}\u{276F}{r} {ty}explain errors{r}\r\n\r\n\x1b[1mError Handling{r}\r\n\r\n  {kw}use{r} std::fmt;\r\n\r\n  {kw}enum{r} {ty}E{r} {{\r\n      Empty,\r\n      {ty}Bad{r}({ty}String{r}),\r\n  }}\r\n\r\n  {kw}fn{r} {ty}f{r}() -> {ty}R{r} {{\r\n      {kw}let{r} x = {ty}1{r};\r\n      x\r\n      Ok(x)\r\n  }}                Jump to bottom (ctrl+End) \u{2193}\r\n\r\nSummary: a {ty}&self{r} method reads the value and another one changes\r\nit in place.\r\n\r\n{kw}\u{273B}{r} {ty}Thinking\u{2026}{r} (3s \u{B7} {mk}9 tokens{r})\r\ntmux off\r\n\r\n  {kw}use{r} std::io;\r\n"
    );
    t.parser.advance(&mut t.term, out.as_bytes());
    let md = reader::screen_markdown(t, 0, 24);
    assert!(md.contains("\n\u{273B} Thinking\u{2026} (3s \u{B7} 9 tokens)\n\ntmux off"), "a status line is text: {md}");
    assert!(md.contains("      let x = 1;\n      x\n      Ok(x)"), "{md}");
    assert!(md.ends_with("```\n  use std::io;\n  ```"), "a lone first row of code: {md}");
    assert!(!md.contains("Jump to bottom"), "{md}");
    assert_eq!(md.matches("```").count(), 4, "one block, and the lone row: {md}");
    assert!(md.contains("```\n  use std::fmt;\n\n  enum E {\n      Empty,"), "{md}");
    assert!(md.contains("      Ok(x)\n  }\n  ```"), "{md}");
    assert!(md.starts_with("\u{276F} explain errors"), "{md}");
    assert!(md.contains("# Error Handling"), "{md}");
    assert!(md.contains("```\n\nSummary: a &self method reads the value and another one changes\nit in place."), "{md}");
}

/// Draws `rows` as a full-screen program's screen (one per row, the rest blank).
fn draw(t: &mut Tab, rows: &[String]) {
    let n = t.term.grid().screen_lines();
    let mut out = String::new();
    for r in 0..n {
        out.push_str(&format!("\x1b[{};1H\x1b[2K{}", r + 1, rows.get(r).map_or("", String::as_str)));
    }
    t.parser.advance(&mut t.term, out.as_bytes());
    t.rev += 1;
}

/// The record's rows as plain text.
fn record(t: &Tab) -> Vec<String> {
    t.collect.rows().into_iter().map(|k| k.plain).collect()
}

/// Ticks the record at `now` and once the screen has settled.
fn settle(t: &mut Tab, now: &mut f64) {
    collect::tick(t, *now);
    *now += collect::STABLE + 0.1;
    collect::tick(t, *now);
    *now += 0.1;
}

fn alt_tab() -> (TermWindow, usize) {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    let t = &mut w.tabs[0];
    t.parser.advance(&mut t.term, b"\x1b[?1049h");
    t.rev += 1;
    let n = t.term.grid().screen_lines();
    (w, n)
}

/// Screens of a program scrolled down and back up go into one record, in order, each row once;
/// a screen too far to place goes after it.
#[test]
fn the_record_gathers_scrolled_screens_in_order() {
    let (mut w, n) = alt_tab();
    let t = &mut w.tabs[0];
    let line = |i: usize| format!("line {i:03} of the file");
    let page = |k: usize| (k..k + n).map(line).collect::<Vec<_>>();
    let mut now = 0.0;
    for k in [0, 5, 9, 3, 0, 12] {
        draw(t, &page(k));
        settle(t, &mut now);
    }
    assert_eq!(record(t), (0..12 + n).map(line).collect::<Vec<_>>(), "in order, each once");
    // Scrolled too far at once: after the record, apart.
    draw(t, &page(200));
    settle(t, &mut now);
    let r = record(t);
    assert_eq!(r[..12 + n], (0..12 + n).map(line).collect::<Vec<_>>()[..]);
    assert_eq!(r[12 + n], "", "a gap");
    assert_eq!(r[13 + n..], page(200)[..]);
    // The program leaves: the record goes.
    t.parser.advance(&mut t.term, b"\x1b[?1049l");
    t.rev += 1;
    settle(t, &mut now);
    assert!(t.collect.is_empty(), "a new record when the program leaves");
}

/// An answer that appears at once, longer than the screen, shows only its end; scrolling up
/// to its start joins it with the question in one document, each row once.
#[test]
fn pieces_of_the_record_join_up_as_it_is_scrolled() {
    let (mut w, n) = alt_tab();
    let t = &mut w.tabs[0];
    let mut doc: Vec<String> = vec!["Banner".into(), "version 1".into(), String::new(), "\u{276F} the question".into(), String::new()];
    doc.extend((0..3 * n).map(|i| format!("answer row {i}")));
    let mut now = 0.0;
    // The question, then the whole answer at once: the screen shows its end.
    draw(t, &doc[..4]);
    settle(t, &mut now);
    let len = doc.len();
    draw(t, &doc[len - n..]);
    settle(t, &mut now);
    // Scrolled up a few rows at a time to the top.
    let mut s = len - n;
    while s > 0 {
        s = s.saturating_sub(4);
        // Scrolled back, the agent covers its last row with a hint.
        let mut page = doc[s..s + n].to_vec();
        page[n - 1] = "                Jump to bottom (ctrl+End) \u{2193}".into();
        draw(t, &page);
        settle(t, &mut now);
    }
    assert_eq!(record(t), doc, "one document, each row once");
}

/// A line that keeps changing (an agent's spinner) and the agent's input box stay out; a
/// redraw from the top with room below (as after /clear) starts the record again.
#[test]
fn the_record_leaves_out_live_lines_and_starts_again_when_cleared() {
    let (mut w, n) = alt_tab();
    let t = &mut w.tabs[0];
    let cols = t.term.grid().columns();
    let rule = "\u{2500}".repeat(cols);
    let mut now = 0.0;
    let screen = |answer: usize, k: usize| {
        let mut rows: Vec<String> = vec!["Banner of the agent".into(), "version 1".into(), String::new()];
        rows.extend((0..answer).map(|i| format!("answer line {i}")));
        rows.push(format!("\u{273B} Thinking\u{2026} ({k}s \u{B7} {} tokens)", k * 7));
        rows.resize(n - 4, String::new());
        rows.extend([rule.clone(), "\u{276F} typing here".into(), rule.clone(), "  status".into()]);
        rows
    };
    for k in 0..6 {
        draw(t, &screen(k, k));
        settle(t, &mut now);
    }
    let r = record(t);
    assert_eq!(r.first().map(String::as_str), Some("Banner of the agent"));
    assert!(r.iter().any(|l| l == "answer line 4"), "{r:?}");
    assert!(!r.iter().any(|l| l.contains("Thinking") || l.contains("typing") || l.contains('\u{2500}')), "{r:?}");
    // /clear: the banner and the command, nothing below.
    let mut cleared: Vec<String> = vec!["Banner of the agent".into(), "version 1".into(), String::new(), "\u{276F} /clear".into()];
    cleared.resize(n - 4, String::new());
    cleared.extend([rule.clone(), "\u{276F} ".into(), rule.clone(), "  status".into()]);
    // The command's row comes a moment after the rest: it is still taken once it settles.
    let mut first = cleared.clone();
    first[3] = String::new();
    draw(t, &first);
    collect::tick(t, now);
    now += 0.3;
    draw(t, &cleared);
    collect::tick(t, now);
    now += 0.3;
    collect::tick(t, now);
    settle(t, &mut now);
    assert_eq!(record(t), vec!["Banner of the agent", "version 1", "", "\u{276F} /clear"]);
}

/// The Markdown view shows the record, its menu copies it, and clearing empties it.
#[test]
fn the_markdown_view_shows_copies_and_clears_the_record() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    let mut clock = 0.0;
    let mut tick = |w: &mut TermWindow, events: Vec<Event>| {
        clock += 0.2;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            focused: true,
            time: Some(clock),
            events,
            ..Default::default()
        };
        ctx.run_test(input, |ui| {
            w.embedded(ui);
        })
    };
    tick(&mut w, Vec::new());
    let t = &mut w.tabs[0];
    t.parser.advance(&mut t.term, b"\x1b[?1049h");
    draw(t, &["# Title".into(), "Some words.".into()]);
    w.tabs[0].view = View::Markdown;
    for _ in 0..6 {
        tick(&mut w, Vec::new());
    }
    let text = w.tabs[0].reading.as_ref().map(|r| r.1.0.clone()).unwrap_or_default();
    assert!(text.contains("Some words."), "{text:?}");
    // Right click: the menu; "Copy all as Markdown" copies the record.
    let at = Pos2::new(400.0, 300.0);
    let click = |button, pressed| Event::PointerButton { pos: at, button, pressed, modifiers: Default::default() };
    tick(&mut w, vec![Event::PointerMoved(at), click(egui::PointerButton::Secondary, true)]);
    tick(&mut w, vec![click(egui::PointerButton::Secondary, false)]);
    let item = tick(&mut w, Vec::new());
    let find = |out: &egui::FullOutput, what: &str| {
        let mut found = None;
        for c in &out.shapes {
            if let egui::Shape::Text(s) = &c.shape
                && s.galley.job.text.contains(what)
            {
                found = Some(s.galley.rect.translate(s.pos.to_vec2()).center());
            }
        }
        found
    };
    let copy_at = find(&item, "Copy all as Markdown").expect("the menu shows");
    let press = |pressed| Event::PointerButton { pos: copy_at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    tick(&mut w, vec![Event::PointerMoved(copy_at), press(true)]);
    let out = tick(&mut w, vec![press(false)]);
    let copied = out.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(s) if s.contains("Some words.")));
    assert!(copied, "the record is copied");
    // The clear button empties it, and what is on the screen does not come back by itself.
    let shown = tick(&mut w, Vec::new());
    let clear_at = find(&shown, ic::ERASER).expect("the clear button");
    tick(&mut w, vec![Event::PointerMoved(clear_at), Event::PointerButton { pos: clear_at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
    tick(&mut w, vec![Event::PointerButton { pos: clear_at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
    for _ in 0..6 {
        tick(&mut w, Vec::new());
    }
    assert!(w.tabs[0].collect.is_empty(), "cleared");
    // Ctrl+F opens the search, which finds the words.
    let t = &mut w.tabs[0];
    draw(t, &["# Title".into(), "Other words here.".into()]);
    for _ in 0..6 {
        tick(&mut w, Vec::new());
    }
    tick(&mut w, vec![Event::Key { key: Key::F, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }]);
    tick(&mut w, vec![Event::Text("other".into())]);
    tick(&mut w, Vec::new());
    let out = tick(&mut w, Vec::new());
    assert!(find(&out, "1 of 1").is_some(), "the search finds it");
}

/// Python as Claude Code colours it: comments, imports with one colour and functions two
/// blank lines apart stay one block; a bold heading and a numbered list stay text.
#[test]
fn python_blocks_stay_whole() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    let t = &mut w.tabs[0];
    let (kw, st, cm, r) = ("\x1b[38;5;170m", "\x1b[38;5;114m", "\x1b[38;5;244m", "\x1b[0m");
    let out = format!(
        "\x1b[2J\x1b[H\x1b[1mWord Count{r}\r\n\r\n  1. {kw}Read{r} the file.\r\n\r\n  {st}\"\"\"Count words.\"\"\"{r}\r\n\r\n  {kw}import{r} sys\r\n  {kw}from{r} pathlib {kw}import{r} Path\r\n\r\n  {cm}# Words shorter than this are skipped.{r}\r\n  MIN = {st}3{r}\r\n\r\n\r\n  {kw}def{r} {st}read{r}(path):\r\n      {cm}# Lowercase it.{r}\r\n      {kw}return{r} path\r\n\r\nThat is all.\r\n"
    );
    t.parser.advance(&mut t.term, out.as_bytes());
    let md = reader::screen_markdown(t, 0, 20);
    assert_eq!(md.matches("```").count(), 2, "one block: {md}");
    assert!(md.starts_with("## Word Count\n\n  1. Read the file.\n\n  ```\n  \"\"\"Count words.\"\"\""), "{md}");
    assert!(md.contains("  # Words shorter than this are skipped.\n  MIN = 3\n\n\n  def read(path):\n      # Lowercase it.\n      return path\n  ```\n\nThat is all."), "{md}");
}

/// The reading views follow the window: the terminal takes their size, the program is told,
/// and the document is made again from the rows as they wrap at the new width.
#[test]
fn reading_views_follow_the_window_size() {
    // The Markdown view is read, not typed into: the terminal keeps the size the others give it.
    {
        let view = View::Text;
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        let mut clock = 0.0;
        let mut at = |w: &mut TermWindow, width: f32| {
            clock += 0.2;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 500.0))),
                time: Some(clock),
                ..Default::default()
            };
            let mut cmds = Vec::new();
            let _ = ctx.run_test(input, |ui| cmds.extend(w.embedded(ui)));
            cmds
        };
        at(&mut w, 900.0);
        w.tabs[0].view = view;
        let t = &mut w.tabs[0];
        t.parser.advance(&mut t.term, "word ".repeat(60).as_bytes());
        t.rev += 1;
        at(&mut w, 900.0);
        at(&mut w, 900.0);
        let wide = w.tabs[0].size.0;
        let cmds = at(&mut w, 500.0);
        let narrow = w.tabs[0].size.0;
        assert!(narrow < wide, "{view:?}: the terminal narrows with the view ({wide} to {narrow})");
        let told = cmds.iter().any(|c| matches!(c, Cmd::Send(ToAgent::TermResize { cols, .. }) if *cols == narrow));
        assert!(told, "{view:?}: the program is told the new size");
        at(&mut w, 500.0);
        // The rows wrap at the new width and the document is made from them.
        let rows_now = w.tabs[0].term.grid().columns();
        assert_eq!(rows_now, narrow as usize, "{view:?}");
        let longest = match view {
            View::Text => w.tabs[0].editor.as_ref().unwrap().1.0.lines().map(|l| l.chars().count()).max().unwrap(),
            _ => reader::screen_markdown(&w.tabs[0], 0, 10).lines().map(|l| l.chars().count()).max().unwrap(),
        };
        assert!(longest <= narrow as usize + 1 || view == View::Markdown, "{view:?}: lines fit {narrow}: {longest}");
    }
}

/// Rows the terminal wraps again at a new width still read as the sentence they were, with
/// every space between words wherever the break falls.
#[test]
fn rewrapped_rows_keep_their_spaces() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    let t = &mut w.tabs[0];
    t.term.resize(Size { cols: 120, rows: 20 });
    let line = "(or `mcp --here` for this computer) lets an AI agent read the window in front as text";
    t.parser.advance(&mut t.term, format!("{line}\r\n").as_bytes());
    for cols in (20..=110).rev() {
        t.term.resize(Size { cols, rows: 20 });
        let history = (t.term.grid().total_lines() - t.term.grid().screen_lines()) as i32;
        let md = reader::screen_markdown(t, -history, 20);
        let words: Vec<&str> = md.split_whitespace().collect();
        assert_eq!(words.join(" "), line, "at {cols} columns: {md:?}");
    }
}

/// The text view shows output that arrives after it opens, as it streams in.
#[test]
fn text_view_follows_new_output() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    // Frames closer together than the rebuild interval, as when output streams in.
    let mut clock = 0.0;
    let mut tick = |w: &mut TermWindow| {
        clock += 0.04;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            focused: true,
            time: Some(clock),
            ..Default::default()
        };
        let _ = ctx.run_test(input, |ui| {
            w.embedded(ui);
        });
    };
    tick(&mut w);
    w.tabs[0].view = View::Text;
    tick(&mut w);
    for k in 0..5 {
        let t = &mut w.tabs[0];
        t.parser.advance(&mut t.term, format!("streamed line {k}\r\n").as_bytes());
        t.rev += 1;
        tick(&mut w);
    }
    for _ in 0..5 {
        tick(&mut w);
    }
    let doc = &w.tabs[0].editor.as_ref().expect("built").1.0;
    assert!(doc.contains("streamed line 4"), "{doc:?}");
    // What is drawn: the laid-out text, not only the document behind it.
    let shown = &w.tabs[0].editor_galley.as_ref().expect("laid out").3.text;
    assert!(shown.contains("streamed line 4"), "{shown:?}");
}

/// A click puts the cursor where it landed: arrow keys in a full-screen program (down first,
/// then across, as the program moves it, not past the text), Left and Right along the shell's
/// own line only, the click itself to a program that asked for the mouse.
#[test]
fn a_click_moves_the_cursor_there() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    let t = &mut w.tabs[0];
    let feed = |t: &mut Tab, s: &str| t.parser.advance(&mut t.term, s.as_bytes());
    feed(t, "\x1b[?1049h\x1b[2J\x1b[Hline one\r\nsecond line here\r\nthird\x1b[H");
    let mut out = Vec::new();
    input::click_cursor(t, 1, 7, &mut out);
    assert_eq!(typed(&out), "\x1b[B", "down first");
    out.clear();
    assert!(input::seek_step(t, &mut out), "waits for the program");
    assert!(out.is_empty());
    feed(t, "\x1b[2;1H");
    assert!(input::seek_step(t, &mut out));
    assert_eq!(typed(&out), "\x1b[C".repeat(7), "then across");
    out.clear();
    feed(t, "\x1b[2;8H");
    assert!(!input::seek_step(t, &mut out), "there");
    assert!(out.is_empty());
    // Past the end of a row: to its end, not on into the next line.
    input::click_cursor(t, 1, 40, &mut out);
    assert_eq!(typed(&out), "\x1b[C".repeat(9));
    out.clear();
    // The shell: along its line only.
    feed(t, "\x1b[?1049l\x1b[2J\x1b[Hold output\r\n$ echo hello");
    t.seek = None;
    input::click_cursor(t, 1, 3, &mut out);
    assert_eq!(typed(&out), "\x1b[D".repeat(9), "left along the line");
    out.clear();
    t.seek = None;
    input::click_cursor(t, 0, 3, &mut out);
    assert!(out.is_empty() && t.seek.is_none(), "not into the output above");
    // A program that asked for the mouse gets the click.
    feed(t, "\x1b[?1000h\x1b[?1006h");
    input::click_cursor(t, 1, 7, &mut out);
    assert_eq!(typed(&out), "\x1b[<0;8;2M\x1b[<0;8;2m");
    w.close_all();
}

/// Claude Code run in a pane here, shown in the Markdown view while it answers and while
/// the answer is scrolled, as pictures to look at (it costs a request):
/// SSHREMOTE_SNAP_DIR=folder SSHREMOTE_CLAUDE=1 cargo test --release markdown_view_with_claude -- --ignored
#[test]
#[ignore]
fn markdown_view_with_claude_code() {
    let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
    if std::env::var_os("SSHREMOTE_CLAUDE").is_none() {
        return;
    }
    let prompt = std::env::var("SSHREMOTE_CLAUDE_PROMPT").unwrap_or_else(|_| {
        "Without using any tools, answer in Markdown: a heading, two paragraphs, a bulleted list of five items, a table of four rows, and a 40-line Rust example in a code block, then a short summary."
            .into()
    });
    // Claude Code starts in SSHREMOTE_CLAUDE_DIR (the current folder when unset), the prompt one argument.
    let dir_arg = std::env::var("SSHREMOTE_CLAUDE_DIR").unwrap_or_else(|_| ".".into());
    let profile = Profile { name: "claude".into(), icon: ic::TERMINAL, launch: Launch::Local(vec!["cmd".into(), "/c".into(), "cd".into(), "/d".into(), dir_arg, "&".into(), "claude".into(), prompt]) };
    let mut w = TermWindow::single(profile);
    let ctx = egui::Context::default();
    crate::gui::style(&ctx);
    let size = Vec2::new(1100.0, 760.0);
    let mut snap = crate::gui::snapshot::Snap::default();
    let mut clock = 0.0;
    let mut frame = |w: &mut TermWindow, snap: &mut crate::gui::snapshot::Snap, events: Vec<Event>| {
        clock += 0.05;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            focused: true,
            time: Some(clock),
            events,
            ..Default::default()
        };
        snap.frame(&ctx, input, |ui| {
            w.embedded(ui);
        });
    };
    let secs: usize = std::env::var("SSHREMOTE_CLAUDE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(90);
    for k in 0..secs * 20 {
        if k == 60 {
            w.tabs[0].view = View::Markdown;
        }
        frame(&mut w, &mut snap, Vec::new());
        std::thread::sleep(std::time::Duration::from_millis(50));
        if k % 40 == 39 {
            snap.save(&ctx, size, &dir.join(format!("claude_md_{:03}.png", k / 20 + 1)));
        }
    }
    // Scrolled up a little at a time, as a wheel does, then back down.
    let wheel = |dy: f32| Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, dy), modifiers: Default::default(), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
    frame(&mut w, &mut snap, vec![Event::PointerMoved(Pos2::new(500.0, 300.0))]);
    // In real time, as a hand on the wheel: a notch every 100 ms, every frame recorded with
    // the document it showed.
    let mut log = String::new();
    let mut n = 0;
    for (dir_dy, notches) in [(60.0, 30), (-60.0, 20)] {
        for k in 0..notches * 2 {
            let events = if k % 2 == 0 { vec![wheel(dir_dy)] } else { Vec::new() };
            frame(&mut w, &mut snap, events);
            std::thread::sleep(std::time::Duration::from_millis(50));
            let text = w.tabs[0].reading.as_ref().map(|r| r.1.0.clone()).unwrap_or_default();
            log.push_str(&format!("===== frame {n} wheel {}\n{text}\n", if k % 2 == 0 { dir_dy } else { 0.0 }));
            snap.save(&ctx, size, &dir.join(format!("claude_md_scroll_{n:03}.png")));
            n += 1;
        }
    }
    std::fs::write(dir.join("scroll_frames.txt"), log).unwrap();
    // /clear typed in the text view: the screen is cleared.
    w.tabs[0].view = View::Text;
    frame(&mut w, &mut snap, vec![Event::Text("/clear".into())]);
    std::thread::sleep(std::time::Duration::from_millis(300));
    frame(&mut w, &mut snap, vec![Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
    for _ in 0..60 {
        frame(&mut w, &mut snap, Vec::new());
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    snap.save(&ctx, size, &dir.join("claude_cleared.png"));
    w.tabs[0].view = View::Markdown;
    for _ in 0..30 {
        frame(&mut w, &mut snap, Vec::new());
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    snap.save(&ctx, size, &dir.join("claude_md_cleared.png"));
    let text = w.tabs[0].reading.as_ref().map(|r| r.1.0.clone()).unwrap_or_default();
    let screen = reader::screen_text(&w.tabs[0], 0, w.tabs[0].term.grid().screen_lines() as i32 - 1).0;
    std::fs::write(dir.join("cleared.md"), format!("{text}\n----\n{}\n----\n{screen}", collect::debug(&w.tabs[0]))).unwrap();
    w.tabs[0].view = View::Text;
    for _ in 0..10 {
        frame(&mut w, &mut snap, Vec::new());
    }
    snap.save(&ctx, size, &dir.join("claude_text.png"));
    let _ = w.close_all();
}

/// nano on the VM in the Markdown view: scrolling through a file with the wheel gathers it, in
/// order and once each line; leaving nano starts the record again. Saves pictures and the
/// record to SSHREMOTE_SNAP_DIR; needs SSHREMOTE_NANO=1 and `ssh ubuntu` working.
#[test]
#[ignore]
fn markdown_view_with_nano() {
    let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
    if std::env::var_os("SSHREMOTE_NANO").is_none() {
        return;
    }
    let file = std::env::var("SSHREMOTE_NANO_FILE").unwrap_or_else(|_| "/home/a/sshremote-src/README.md".into());
    let launch = vec!["ssh".into(), "-t".into(), "ubuntu".into(), "nano".into(), "-v".into(), file];
    let profile = Profile { name: "nano".into(), icon: ic::TERMINAL, launch: Launch::Local(launch) };
    let mut w = TermWindow::single(profile);
    let ctx = egui::Context::default();
    crate::gui::style(&ctx);
    let size = Vec2::new(1100.0, 760.0);
    let mut snap = crate::gui::snapshot::Snap::default();
    let mut clock = 0.0;
    let mut frame = |w: &mut TermWindow, snap: &mut crate::gui::snapshot::Snap, events: Vec<Event>| {
        clock += 0.05;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            focused: true,
            time: Some(clock),
            events,
            ..Default::default()
        };
        snap.frame(&ctx, input, |ui| {
            w.embedded(ui);
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    for _ in 0..80 {
        frame(&mut w, &mut snap, Vec::new());
    }
    snap.save(&ctx, size, &dir.join("nano_text.png"));
    w.tabs[0].view = View::Markdown;
    for _ in 0..30 {
        frame(&mut w, &mut snap, Vec::new());
    }
    snap.save(&ctx, size, &dir.join("nano_md_start.png"));
    let wheel = |dy: f32| Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, dy), modifiers: Default::default(), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
    frame(&mut w, &mut snap, vec![Event::PointerMoved(Pos2::new(500.0, 300.0))]);
    // Down through the file a notch at a time, as a hand does.
    for k in 0..240 {
        frame(&mut w, &mut snap, if k % 2 == 0 { vec![wheel(-60.0)] } else { Vec::new() });
        if k % 60 == 59 {
            snap.save(&ctx, size, &dir.join(format!("nano_md_scroll_{:02}.png", k / 60)));
        }
    }
    for _ in 0..20 {
        frame(&mut w, &mut snap, Vec::new());
    }
    snap.save(&ctx, size, &dir.join("nano_md_end.png"));
    let text = w.tabs[0].reading.as_ref().map(|r| r.1.0.clone()).unwrap_or_default();
    let plain = w.tabs[0].collect.rows().into_iter().map(|k| k.plain).collect::<Vec<_>>().join("\n");
    std::fs::write(dir.join("nano_record.md"), &text).unwrap();
    std::fs::write(dir.join("nano_record.txt"), &plain).unwrap();
    // Ctrl+X leaves nano: the record starts again.
    if let Backend::Local(l) = &mut w.tabs[0].backend {
        l.write(b"\x18");
    }
    for _ in 0..40 {
        frame(&mut w, &mut snap, Vec::new());
    }
    snap.save(&ctx, size, &dir.join("nano_md_left.png"));
    assert!(w.tabs[0].collect.is_empty(), "the record goes when nano leaves");
    let _ = w.close_all();
}

/// Output that never pauses (a spinner, a reply streaming in) does not rebuild the Markdown
/// view every frame: it settles first, or catches up now and then; empty code blocks do not
/// show as boxes.
#[test]
fn the_markdown_view_waits_for_output_to_settle() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    let mut clock = 0.0;
    let mut tick = |w: &mut TermWindow| {
        clock += 0.05;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            focused: true,
            time: Some(clock),
            ..Default::default()
        };
        let _ = ctx.run_test(input, |ui| {
            w.embedded(ui);
        });
    };
    tick(&mut w);
    w.tabs[0].view = View::Markdown;
    tick(&mut w);
    let built = |w: &TermWindow| w.tabs[0].reading.as_ref().map(|r| std::rc::Rc::as_ptr(&r.1) as usize);
    let mut builds = std::collections::BTreeSet::new();
    // One second of output every frame.
    for k in 0..20 {
        let t = &mut w.tabs[0];
        t.parser.advance(&mut t.term, format!("\r\x1b[2K working {k}").as_bytes());
        t.rev += 1;
        tick(&mut w);
        builds.extend(built(&w));
    }
    assert!(builds.len() <= 3, "rebuilt {} times in a second of streaming", builds.len());
    // Then it stops: the last text shows once settled.
    let t = &mut w.tabs[0];
    t.parser.advance(&mut t.term, b"\r\n```\n```\ndone streaming\r\n");
    t.rev += 1;
    for _ in 0..12 {
        tick(&mut w);
    }
    let doc = &w.tabs[0].reading.as_ref().expect("built").1;
    assert!(doc.0.contains("done streaming"), "{:?}", doc.0);
    assert!(!doc.1.blocks.iter().any(|b| matches!(b, crate::markdown::Block::Code { code, .. } if code.trim().is_empty())));
}

/// Shift+Delete reaches the windows as a cut (as Ctrl+X does): the shell gets the key that was
/// pressed, not Ctrl+X.
#[test]
fn shift_delete_is_shift_delete() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    frame(&ctx, &mut w, Vec::new());
    let cut = |ctx: &egui::Context, w: &mut TermWindow, modifiers: egui::Modifiers| {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            focused: true,
            events: vec![Event::ModifiersChanged(modifiers), Event::Cut],
            ..Default::default()
        };
        let mut out = Vec::new();
        let _ = ctx.run_test(input, |ui| out.extend(w.embedded(ui)));
        typed(&out)
    };
    assert_eq!(cut(&ctx, &mut w, egui::Modifiers::SHIFT), "\u{1b}[3;2~");
    assert_eq!(cut(&ctx, &mut w, egui::Modifiers::CTRL), "\u{18}");
}

fn typed(cmds: &[Cmd]) -> String {
    let bytes: Vec<u8> = cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Send(ToAgent::TermInput { data, .. }) => Some(data.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn input_methods_type_into_the_shell() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    // The first frames open the shell and lay the terminal out.
    frame(&ctx, &mut w, Vec::new());
    frame(&ctx, &mut w, Vec::new());
    let preedit = Event::Ime(egui::ImeEvent::Preedit { text: "\u{D558}".into(), active_range_chars: None });
    let composing = frame(&ctx, &mut w, vec![preedit]);
    assert_eq!(typed(&composing), "", "nothing is sent while composing");
    assert_eq!(w.tabs[0].preedit, "\u{D558}");
    let enter = Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    let commit = Event::Ime(egui::ImeEvent::Commit("\u{D55C}\u{AE00}".into()));
    let done = frame(&ctx, &mut w, vec![commit, Event::Text(" ok".into()), enter]);
    assert_eq!(typed(&done), "\u{D55C}\u{AE00} ok\r");
    assert!(w.tabs[0].preedit.is_empty());
    // The window asks the system for an input method at the cursor.
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
        focused: true,
        ..Default::default()
    };
    let out = ctx.run_test(input, |ui| {
        w.embedded(ui);
    });
    let ime = out.platform_output.ime.expect("an input method at the cursor");
    // Asking for the focus again every frame would end the composition each time.
    assert!(!ime.should_interrupt_composition, "the composition is left alone between frames");
}

#[test]
fn reading_views_still_type_into_the_shell() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    frame(&ctx, &mut w, Vec::new());
    for view in [View::Text, View::Markdown] {
        w.tabs[0].view = view;
        frame(&ctx, &mut w, Vec::new());
        let done = frame(
            &ctx,
            &mut w,
            vec![Event::Ime(egui::ImeEvent::Commit("\u{D55C}".into())), Event::Text("a".into())],
        );
        // The Markdown view is a record to read and copy: nothing typed there goes to the shell.
        let want = if view == View::Text { "\u{D55C}a" } else { "" };
        assert_eq!(typed(&done), want, "{view:?}");
    }
}

/// Where the input method's cursor is after a frame with `events`.
fn ime_y(ctx: &egui::Context, w: &mut TermWindow, events: Vec<Event>) -> f32 {
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
        focused: true,
        events,
        ..Default::default()
    };
    let out = ctx.run_test(input, |ui| {
        w.embedded(ui);
    });
    out.platform_output.ime.map_or(f32::NAN, |i| i.cursor_rect.top())
}

#[test]
fn reading_views_scroll_with_the_wheel() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    frame(&ctx, &mut w, Vec::new());
    let t = &mut w.tabs[0];
    for n in 0..300 {
        t.parser.advance(&mut t.term, format!("line {n}\r\n").as_bytes());
    }
    t.rev += 1;
    {
        let view = View::Text;
        w.tabs[0].view = view;
        for _ in 0..3 {
            ime_y(&ctx, &mut w, Vec::new());
        }
        let bottom = ime_y(&ctx, &mut w, Vec::new());
        assert!(bottom < 500.0, "{view:?}: the live rows start in view at the bottom ({bottom})");
        let wheel = Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0.0, 10.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
            source: egui::MouseWheelSource::Wheel,
        };
        ime_y(&ctx, &mut w, vec![Event::PointerMoved(Pos2::new(400.0, 250.0)), wheel]);
        let mut after = bottom;
        for _ in 0..30 {
            after = ime_y(&ctx, &mut w, vec![Event::PointerMoved(Pos2::new(400.0, 250.0))]);
        }
        assert!(after > bottom + 50.0, "{view:?}: the wheel scrolls back ({bottom} -> {after})");
    }
}

#[test]
fn the_text_view_selects_and_copies_like_an_editor() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    frame(&ctx, &mut w, Vec::new());
    let t = &mut w.tabs[0];
    t.parser.advance(&mut t.term, b"hello world\r\n$ ");
    t.rev += 1;
    w.tabs[0].view = View::Text;
    frame(&ctx, &mut w, Vec::new());
    frame(&ctx, &mut w, Vec::new());
    let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
    let select_all = Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false, modifiers: ctrl };
    assert_eq!(typed(&frame(&ctx, &mut w, vec![select_all])), "", "Ctrl+A selects the text, not the shell's line");
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
        focused: true,
        events: vec![Event::Copy],
        ..Default::default()
    };
    let mut cmds = Vec::new();
    let out = ctx.run_test(input, |ui| cmds.extend(w.embedded(ui)));
    assert_eq!(typed(&cmds), "", "Ctrl+C with a selection copies instead of interrupting");
    let copied = out
        .platform_output
        .commands
        .iter()
        .any(|c| matches!(c, egui::OutputCommand::CopyText(s) if s.contains("hello world")));
    assert!(copied, "the selection is copied");
    // Typing still reaches the shell, and the selection gives way to the caret.
    assert_eq!(typed(&frame(&ctx, &mut w, vec![Event::Text("ls".into())])), "ls");
}

/// Ctrl+Insert copies the selection without interrupting, and Shift+Insert pastes, as in
/// GNOME Terminal.
#[test]
fn insert_keys_copy_and_paste() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    let t = &mut w.tabs[0];
    t.parser.advance(&mut t.term, b"\x1b[2J\x1b[Hcopy me\r\n");
    let mut sel = Selection::new(SelectionType::Simple, Point::new(Line(0), Column(0)), Side::Left);
    sel.update(Point::new(Line(0), Column(6)), Side::Right);
    t.term.selection = Some(sel);
    let key = |modifiers| Event::Key { key: Key::Insert, physical_key: None, pressed: true, repeat: false, modifiers };
    let input = |events| egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
        focused: true,
        events,
        ..Default::default()
    };
    let mut cmds = Vec::new();
    let out = ctx.run_test(input(vec![key(egui::Modifiers::CTRL)]), |ui| cmds.extend(w.embedded(ui)));
    assert_eq!(typed(&cmds), "", "nothing typed");
    assert!(out.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(s) if s == "copy me")));
    cmds.clear();
    let out = ctx.run_test(input(vec![key(egui::Modifiers::SHIFT)]), |ui| cmds.extend(w.embedded(ui)));
    assert_eq!(typed(&cmds), "", "nothing typed");
    let pasting = out.viewport_output.values().any(|v| v.commands.contains(&egui::ViewportCommand::RequestPaste));
    assert!(pasting, "Shift+Insert asks for the clipboard");
    let _ = w.close_all();
}

/// In the text and Markdown views, Ctrl+C copies text selected with the mouse and sends
/// nothing to the shell; with nothing selected it interrupts, as in the terminal.
#[test]
fn ctrl_c_copies_a_mouse_selection_in_the_reading_views() {
    for view in [View::Text, View::Markdown] {
        let ctx = egui::Context::default();
        let mut w = TermWindow::new(None);
        frame(&ctx, &mut w, Vec::new());
        let t = &mut w.tabs[0];
        t.parser.advance(&mut t.term, b"alpha beta gamma\r\n\r\n$ ");
        t.rev += 1;
        w.tabs[0].view = view;
        let mut shapes = Vec::new();
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
                focused: true,
                ..Default::default()
            };
            shapes = ctx.run_test(input, |ui| { w.embedded(ui); }).shapes;
        }
        // Where "alpha beta gamma" was drawn.
        fn find(s: &egui::Shape, out: &mut Option<Rect>) {
            match s {
                egui::Shape::Text(t) if t.galley.job.text.contains("alpha beta gamma") => {
                    let row = t.galley.rows.iter().find(|r| r.text().contains("alpha")).map(|r| r.rect()).unwrap_or(t.galley.rect);
                    *out = Some(row.translate(t.pos.to_vec2()));
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| find(s, out)),
                _ => {}
            }
        }
        let mut at = None;
        shapes.iter().for_each(|c| find(&c.shape, &mut at));
        let r = at.unwrap_or_else(|| panic!("{view:?}: the text is drawn"));
        let (from, to) = (Pos2::new(r.left() + 1.0, r.center().y), Pos2::new(r.left() + 60.0, r.center().y));
        let button = |pos, pressed| Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(&ctx, &mut w, vec![Event::PointerMoved(from)]);
        frame(&ctx, &mut w, vec![button(from, true)]);
        frame(&ctx, &mut w, vec![Event::PointerMoved(to)]);
        frame(&ctx, &mut w, vec![button(to, false)]);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            focused: true,
            events: vec![Event::Copy],
            ..Default::default()
        };
        let mut cmds = Vec::new();
        let out = ctx.run_test(input, |ui| cmds.extend(w.embedded(ui)));
        assert_eq!(typed(&cmds), "", "{view:?}: Ctrl+C with a selection does not interrupt");
        let copied: Vec<&String> = out
            .platform_output
            .commands
            .iter()
            .filter_map(|c| match c {
                egui::OutputCommand::CopyText(s) => Some(s),
                _ => None,
            })
            .collect();
        assert!(copied.iter().any(|s| s.contains("alpha")), "{view:?}: the selection is copied: {copied:?}");
        // Copying ends the selection, so the next Ctrl+C interrupts, as in Windows Terminal; the
        // Markdown view takes no keys, so there it does nothing.
        frame(&ctx, &mut w, Vec::new());
        let want = if view == View::Text { "\u{3}" } else { "" };
        assert_eq!(typed(&frame(&ctx, &mut w, vec![Event::Copy])), want, "{view:?}: the next Ctrl+C");
    }
}

#[test]
fn styles_on_screen_come_back_as_markdown() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    frame(&ctx, &mut w, Vec::new());
    let t = &mut w.tabs[0];
    let screen = "\x1b[2J\x1b[H\x1b[38;5;231m\u{25CF}\x1b[39m \x1b[1;3;4mThe Log\x1b[0m\r\n\r\n  \x1b[3mA short story\x1b[0m\r\n\r\n  \x1b[1mChapter 1\x1b[0m\r\n  plain \x1b[1mbold\x1b[0m word\r\n";
    t.parser.advance(&mut t.term, screen.as_bytes());
    let md = screen_markdown(t, 0, 6);
    assert_eq!(md, "\u{25CF} # The Log\n\n  *A short story*\n\n  ## Chapter 1\n  plain **bold** word");
}

#[test]
fn tabs_move_along_the_strip_and_session_shells_stay() {
    let ctx = egui::Context::default();
    let mut w = TermWindow::new(None);
    frame(&ctx, &mut w, Vec::new());
    w.open_tab();
    w.open_tab();
    frame(&ctx, &mut w, Vec::new());
    let order = |w: &TermWindow| w.groups().iter().map(|g| g.0).collect::<Vec<_>>();
    let before = order(&w);
    assert_eq!(before.len(), 3);
    w.move_group(before[0], before[2]);
    assert_eq!(order(&w), [before[1], before[2], before[0]]);
    w.move_group(before[0], before[1]);
    assert_eq!(order(&w), before);
}
