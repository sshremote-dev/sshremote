use super::*;
use egui::{Event, PointerButton};

struct Harness {
    ctx: egui::Context,
    r: Remote,
    clock: f64,
    size: Vec2,
    shapes: Vec<egui::epaint::ClippedShape>,
    snap: super::super::snapshot::Snap,
}

impl Harness {
    fn new(size: Vec2) -> Harness {
        // Settings go to a scratch folder, never the user's; set once, before any agent starts
        // (these tests run one at a time: cargo test -- --test-threads=1).
        static SCRATCH: std::sync::Once = std::sync::Once::new();
        SCRATCH.call_once(|| {
            let scratch = std::env::temp_dir().join(format!("sshremote-ui-test-{}", std::process::id()));
            std::fs::create_dir_all(&scratch).unwrap();
            for var in ["APPDATA", "XDG_CONFIG_HOME"] {
                unsafe { std::env::set_var(var, &scratch) };
            }
        });
        let opt = crate::parse_client(&["--gui".into(), "--local".into()]).unwrap();
        let (sess, _tx, rx) = Session::connect(&opt).expect("a local agent (SSHREMOTE_EXE)");
        let ctl = Control::new(&opt, Mode::True);
        let r = Remote::new(sess, rx, ctl, "test desktop".into(), None);
        let ctx = egui::Context::default();
        crate::gui::style(&ctx);
        let mut h = Harness { ctx, r, clock: 0.0, size, shapes: Vec::new(), snap: Default::default() };
        // The first picture arrives.
        for _ in 0..40 {
            h.frame(Vec::new()).unwrap();
            if h.r.tex.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        h
    }

    fn frame(&mut self, events: Vec<Event>) -> Result<(), String> {
        self.clock += 0.05;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.clock),
            focused: true,
            events,
            ..Default::default()
        };
        let mut result = Ok(());
        let r = &mut self.r;
        let out = self.snap.frame(&self.ctx, input, |ui| {
            // The pane's controls in a bar across the top, as the workspace draws them, and the
            // pane under it.
            let whole = ui.max_rect();
            let bar = Rect::from_min_size(whole.min, Vec2::new(whole.width(), super::super::chrome::HEIGHT));
            let mut action = None;
            let mut top = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::left_to_right(egui::Align::Center)));
            r.toolbar_items(&mut top, &mut action);
            let mut pane = ui.new_child(egui::UiBuilder::new().max_rect(whole.with_min_y(bar.bottom())));
            result = r.frame(&mut pane);
            if let (Ok(()), Some(f)) = (&result, action) {
                result = f(r, ui.ctx());
            }
        });
        self.shapes = out.shapes;
        result
    }

    /// Where text containing `what` was drawn, the topmost (popups are drawn last).
    fn find(&self, what: &str) -> Option<Rect> {
        self.find_in(what, Rect::EVERYTHING)
    }

    fn find_in(&self, what: &str, within: Rect) -> Option<Rect> {
        let mut hits = Vec::new();
        for c in &self.shapes {
            collect(&c.shape, what, &mut hits);
        }
        hits.into_iter().rev().find(|r| within.contains(r.center()))
    }

    /// Clicks a toolbar button: `what` in the title bar, drawn first (a window shown inside this
    /// test's single viewport, such as the file manager, may lie over it).
    fn tool(&mut self, what: &str) -> Result<(), String> {
        // What the last state change shows, drawn before looking.
        self.frame(Vec::new())?;
        let bar = Rect::from_min_size(Pos2::ZERO, Vec2::new(self.size.x, super::super::chrome::HEIGHT));
        let mut hits = Vec::new();
        for c in self.shapes.iter().filter(|c| c.clip_rect.intersects(bar)) {
            collect(&c.shape, what, &mut hits);
        }
        let at = hits.into_iter().find(|r| bar.contains(r.center())).unwrap_or_else(|| panic!("no {what:?} in the toolbar")).center();
        self.click_at(at)
    }

    /// Moves to and clicks the text `what`, then lets two frames pass.
    fn click(&mut self, what: &str) -> Result<(), String> {
        let at = self.find(what).unwrap_or_else(|| panic!("nothing drawn reads {what:?}")).center();
        self.click_at(at)
    }

    fn click_at(&mut self, at: Pos2) -> Result<(), String> {
        self.frame(vec![Event::PointerMoved(at)])?;
        let press = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        self.frame(vec![press(true)])?;
        self.frame(vec![press(false)])?;
        self.frame(Vec::new())?;
        self.frame(Vec::new())
    }
}

fn collect(shape: &egui::Shape, what: &str, hits: &mut Vec<Rect>) {
    match shape {
        egui::Shape::Text(t) if t.galley.job.text.contains(what) => hits.push(t.galley.rect.translate(t.pos.to_vec2())),
        egui::Shape::Vec(v) => v.iter().for_each(|s| collect(s, what, hits)),
        _ => {}
    }
}

/// A click with Shift held is a Shift+click at the host.
#[test]
#[ignore]
fn modifiers_go_with_clicks() {
    let mut h = Harness::new(Vec2::new(1000.0, 700.0));
    h.frame(Vec::new()).unwrap();
    let at = h.r.last_shown.expect("the picture").center();
    let press = |pressed, modifiers| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers };
    h.frame(vec![Event::PointerMoved(at)]).unwrap();
    h.frame(vec![press(true, egui::Modifiers::SHIFT)]).unwrap();
    assert!(h.r.mods.shift && h.r.held.contains(&SHIFT_L), "Shift is down at the host for the click");
    h.frame(vec![press(false, egui::Modifiers::NONE)]).unwrap();
    assert!(!h.r.mods.shift, "and let go with it");
}

/// Alt held alone reaches the host before the Tab the hook takes, and is let go there too.
#[test]
#[ignore]
fn alt_tab_reaches_the_host_with_alt() {
    let mut h = Harness::new(Vec2::new(1000.0, 700.0));
    let show = |h: &mut Harness, modifiers: egui::Modifiers, grabbed: Vec<(u32, bool)>| {
        let mut grabbed = Some(grabbed);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, h.size)),
            focused: true,
            events: vec![Event::ModifiersChanged(modifiers)],
            ..Default::default()
        };
        let r = &mut h.r;
        let mut result = Ok(());
        h.snap.frame(&h.ctx, input, |ui| result = r.show(ui, grabbed.take().unwrap_or_default()));
        result.unwrap();
    };
    show(&mut h, egui::Modifiers::NONE, Vec::new());
    show(&mut h, egui::Modifiers::ALT, vec![(xk::TAB, true), (xk::TAB, false)]);
    assert!(h.r.mods.alt, "Alt is down at the host");
    assert!(h.r.held.contains(&ALT_L), "held there");
    show(&mut h, egui::Modifiers::NONE, Vec::new());
    assert!(!h.r.mods.alt && !h.r.held.contains(&ALT_L), "and let go");
}

#[test]
#[ignore]
fn toolbar_actions() {
    let mut h = Harness::new(Vec2::new(1280.0, 800.0));
    assert!(h.r.tex.is_some(), "the picture arrives");

    h.tool(ic::PAUSE).unwrap();
    assert!(h.r.paused, "pause");
    h.tool(ic::PLAY).unwrap();
    assert!(!h.r.paused, "resume");

    // The terminal opens in a panel under the picture, with the keyboard.
    h.tool(ic::TERMINAL).unwrap();
    assert!(h.r.terms.is_some() && h.r.term_split && h.r.term_focus, "terminal panel under the screen");
    assert!(h.r.last_shown.is_some(), "the picture stays in view");
    // A click on the picture gives it the keyboard back; one in the panel takes it again.
    let pic = h.r.last_shown.unwrap();
    h.click_at(pic.center()).unwrap();
    assert!(!h.r.term_focus, "the picture has the keys");
    h.click_at(Pos2::new(pic.center().x, h.size.y - 40.0)).unwrap();
    assert!(h.r.term_focus, "the terminal has the keys");
    // Typing goes to the shell, not to the remote screen.
    let typed = Event::Text("x".into());
    h.frame(vec![typed]).unwrap();
    h.tool(ic::TERMINAL).unwrap();
    assert!(!h.r.term_split && h.r.terms.is_some(), "hidden, the shell still running");

    h.tool(ic::CHAT).unwrap();
    assert!(h.r.chat.is_some(), "chat opens");
    let pic = h.r.last_shown.unwrap();
    assert!(pic.right() <= h.size.x - 260.0, "the picture makes room for the chat: {pic:?}");
    h.tool(ic::CHAT).unwrap();
    assert!(h.r.chat.is_none(), "chat closes");

    h.tool(ic::GEAR).unwrap();
    h.click("View only").unwrap();
    assert!(h.r.view_only, "view only from the options");
    h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]).unwrap();

    // Ctrl+Alt+Enter asks the workspace for full screen; F11 is the host's.
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    h.frame(vec![key(egui::Key::F11, Default::default())]).unwrap();
    assert!(!h.r.full_toggle, "F11 goes to the host");
    h.frame(vec![key(egui::Key::Enter, egui::Modifiers::CTRL | egui::Modifiers::ALT)]).unwrap();
    assert!(h.r.full_toggle, "Ctrl+Alt+Enter asks for full screen");
}

/// The quality, view and special-key menus.
#[test]
#[ignore]
fn menu_actions() {
    let mut h = Harness::new(Vec2::new(1280.0, 800.0));
    let first = &crate::proto::PRESETS[0];
    h.tool(ic::MONITOR).unwrap();
    h.click(&crate::gui::preset_text(first)).unwrap();
    assert!(h.r.ctl.auto.is_none() && h.r.ctl.preset == first.name, "a fixed preset");
    h.tool(ic::MONITOR).unwrap();
    h.click("Auto (adapts").unwrap();
    assert!(h.r.ctl.auto.is_some(), "auto again");
    h.tool(ic::MONITOR).unwrap();
    h.click("Stretch to fill").unwrap();
    assert!(h.r.fit == Fit::Stretch, "stretch");
    h.tool(ic::MONITOR).unwrap();
    h.click("Fit to window").unwrap();
    assert!(h.r.fit == Fit::Window, "fit");

    // Keys go to the built-in test desktop, never to this computer.
    h.tool(ic::KEYBOARD).unwrap();
    h.click("Start menu").unwrap();
    h.tool(ic::KEYBOARD).unwrap();
    h.click("Win + next key").unwrap();
    assert!(h.r.win_next, "Win with the next key");
    for _ in 0..5 {
        h.frame(Vec::new()).unwrap();
    }
}

/// Pictures of the remote screen's menus in both themes, wide and narrow:
/// SSHREMOTE_EXE=... SSHREMOTE_SNAP_DIR=folder cargo test --release -- --ignored --test-threads=1 menus_
#[test]
#[ignore]
fn menus_remote() {
    // Pictures only on request: there is nothing to check without a folder to look at.
    let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else {
        eprintln!("menus_remote: set SSHREMOTE_SNAP_DIR to get the pictures");
        return;
    };
    for (dark, size) in [(true, Vec2::new(1280.0, 800.0)), (false, Vec2::new(1280.0, 800.0)), (true, Vec2::new(900.0, 520.0))] {
        let mut h = Harness::new(size);
        crate::gui::theme::apply_theme(&h.ctx, dark);
        h.frame(Vec::new()).unwrap();
        let name = format!("remote_{}_{}", if dark { "dark" } else { "light" }, size.x as u32);
        let save = |h: &mut Harness, what: &str| {
            // Popups fade in: the picture once they are fully there.
            for _ in 0..8 {
                h.frame(Vec::new()).unwrap();
            }
            h.snap.save(&h.ctx, h.size, &dir.join(format!("{name}_{what}.png")));
        };
        let escape = |h: &mut Harness| {
            h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]).unwrap();
            h.frame(Vec::new()).unwrap();
        };
        for (glyph, what) in [(ic::MONITOR, "display"), (ic::KEYBOARD, "keys"), (ic::GEAR, "options")] {
            if h.find_in(glyph, Rect::from_min_size(Pos2::ZERO, Vec2::new(h.size.x, 40.0))).is_none() {
                continue;
            }
            h.tool(glyph).unwrap();
            save(&mut h, what);
            escape(&mut h);
        }
        // The terminal button's own menu, by a right click.
        let at = h.find_in(ic::TERMINAL, Rect::from_min_size(Pos2::ZERO, Vec2::new(h.size.x, 40.0))).unwrap().center();
        h.frame(vec![Event::PointerMoved(at)]).unwrap();
        let press = |pressed| Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed, modifiers: Default::default() };
        h.frame(vec![press(true)]).unwrap();
        h.frame(vec![press(false)]).unwrap();
        h.frame(Vec::new()).unwrap();
        save(&mut h, "terminal_menu");
        escape(&mut h);
        h.r.open_panels(true, true);
        for _ in 0..4 {
            h.frame(Vec::new()).unwrap();
        }
        save(&mut h, "panels");
    }
}
