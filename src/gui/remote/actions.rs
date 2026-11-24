use super::*;
use egui::{Event, PointerButton};

struct Harness {
    ctx: egui::Context,
    r: Remote,
    clock: f64,
    size: Vec2,
    shapes: Vec<egui::epaint::ClippedShape>,
    /// The pointer's look asked of the window in the last frame.
    cursor: egui::CursorIcon,
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
        let mut h = Harness { ctx, r, clock: 0.0, size, shapes: Vec::new(), cursor: egui::CursorIcon::Default, snap: Default::default() };
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
        self.cursor = out.platform_output.cursor_icon;
        result
    }

    /// Whether a picture was drawn with its top-left corner at `at` (within a pixel).
    fn drawn_image_at(&self, at: Pos2) -> bool {
        fn find(shape: &egui::Shape, at: Pos2) -> bool {
            match shape {
                egui::Shape::Mesh(m) => m.texture_id != egui::TextureId::default() && m.vertices.first().is_some_and(|v| (v.pos - at).length() < 1.0),
                egui::Shape::Vec(v) => v.iter().any(|s| find(s, at)),
                _ => false,
            }
        }
        self.shapes.iter().any(|c| find(&c.shape, at))
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
        self.tool_with(what, PointerButton::Primary)
    }

    /// Right-clicks a toolbar control, for its menu.
    fn tool_menu(&mut self, what: &str) -> Result<(), String> {
        self.tool_with(what, PointerButton::Secondary)
    }

    fn tool_with(&mut self, what: &str, button: PointerButton) -> Result<(), String> {
        // What the last state change shows, drawn before looking.
        self.frame(Vec::new())?;
        let bar = Rect::from_min_size(Pos2::ZERO, Vec2::new(self.size.x, super::super::chrome::HEIGHT));
        let mut hits = Vec::new();
        for c in self.shapes.iter().filter(|c| c.clip_rect.intersects(bar)) {
            collect(&c.shape, what, &mut hits);
        }
        let at = hits.into_iter().find(|r| bar.contains(r.center())).unwrap_or_else(|| panic!("no {what:?} in the toolbar")).center();
        self.click_with(at, button)
    }

    /// Moves to and clicks the text `what`, then lets two frames pass.
    fn click(&mut self, what: &str) -> Result<(), String> {
        let at = self.find(what).unwrap_or_else(|| panic!("nothing drawn reads {what:?}")).center();
        self.click_at(at)
    }

    fn click_at(&mut self, at: Pos2) -> Result<(), String> {
        self.click_with(at, PointerButton::Primary)
    }

    fn click_with(&mut self, at: Pos2, button: PointerButton) -> Result<(), String> {
        self.frame(vec![Event::PointerMoved(at)])?;
        let press = |pressed| Event::PointerButton { pos: at, button, pressed, modifiers: Default::default() };
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

/// This computer's own screen in its own window: a click or a key there would come back to this
/// window, so neither is sent.
#[test]
#[ignore]
fn this_computers_own_screen_sends_nothing() {
    let mut h = Harness::new(Vec2::new(1000.0, 700.0));
    h.r.set_mirror();
    h.frame(Vec::new()).unwrap();
    let at = h.r.last_shown.expect("the picture").center();
    let press = |pressed, modifiers| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers };
    h.frame(vec![Event::PointerMoved(at)]).unwrap();
    h.frame(vec![press(true, egui::Modifiers::SHIFT)]).unwrap();
    assert!(!h.r.mods.shift && h.r.held.is_empty(), "nothing is down there");
    h.frame(vec![press(false, egui::Modifiers::NONE)]).unwrap();
    assert!(h.r.view_only(), "and it says it is only for looking");
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

/// Between a Mac and another computer Ctrl and the command key trade places: Ctrl (and copying
/// with it) is the command key there, the Win key Ctrl; a Mac's command key reaches the host
/// (as Super, or as Ctrl when they trade).
#[test]
#[ignore]
fn ctrl_and_the_command_key_trade_places_between_a_mac_and_another() {
    let mut h = Harness::new(Vec2::new(1000.0, 700.0));
    let show = |h: &mut Harness, modifiers: egui::Modifiers, extra: Vec<Event>, grabbed: Vec<(u32, bool)>| {
        let mut grabbed = Some(grabbed);
        let mut events = vec![Event::ModifiersChanged(modifiers)];
        events.extend(extra);
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, h.size)), focused: true, events, ..Default::default() };
        let r = &mut h.r;
        let mut result = Ok(());
        h.snap.frame(&h.ctx, input, |ui| result = r.show(ui, grabbed.take().unwrap_or_default()));
        result.unwrap();
    };
    let none = egui::Modifiers::NONE;
    show(&mut h, none, Vec::new(), Vec::new());
    h.r.swap_cmd = true;
    show(&mut h, egui::Modifiers::CTRL | egui::Modifiers::COMMAND, vec![Event::Copy], Vec::new());
    assert!(h.r.held.contains(&SUPER_L) && !h.r.held.contains(&CONTROL_L), "Ctrl is the command key there: {:?}", h.r.held);
    assert!(!h.r.held.contains(&('c' as u32)), "C was tapped");
    show(&mut h, none, Vec::new(), Vec::new());
    assert!(h.r.held.is_empty(), "let go: {:?}", h.r.held);
    show(&mut h, none, Vec::new(), vec![(SUPER_L, true)]);
    assert!(h.r.held.contains(&CONTROL_L), "the Win key is Ctrl there");
    show(&mut h, none, Vec::new(), vec![(SUPER_L, false)]);
    assert!(h.r.held.is_empty(), "let go: {:?}", h.r.held);
    // A Mac host's keys menu: its own shortcuts, and the trade to turn off.
    h.r.sess.backend = "mac".into();
    h.tool(ic::KEYBOARD).unwrap();
    assert!(h.find("Spotlight").is_some() && h.find("Start menu").is_none(), "a Mac's shortcuts");
    // (A Mac to a Mac has nothing to trade: the menu stays open.)
    if !cfg!(target_os = "macos") {
        h.click("Ctrl here is Cmd there").unwrap();
        assert!(!h.r.swap_cmd, "turned off");
        h.tool(ic::KEYBOARD).unwrap();
    }
    h.click("Cmd + next key").unwrap();
    assert!(h.r.win_next, "Cmd with the next key");
    h.r.win_next = false;
    h.r.sess.backend = "synthetic".into();
    // A Mac's command key, to a Mac and to another computer.
    for (swap, there) in [(false, SUPER_L), (true, CONTROL_L)] {
        h.r.swap_cmd = swap;
        show(&mut h, egui::Modifiers::MAC_CMD | egui::Modifiers::COMMAND, Vec::new(), Vec::new());
        assert!(h.r.held.contains(&there), "the command key reaches the host as {there:x}: {:?}", h.r.held);
        show(&mut h, none, Vec::new(), Vec::new());
        assert!(h.r.held.is_empty(), "let go: {:?}", h.r.held);
    }
}

/// Tab with the toolbar in sight goes to the host, and so does every key after it: egui would
/// give Tab to the toolbar's first button and keep the keys in this window. Esc and the arrows
/// likewise stay the host's.
#[test]
#[ignore]
fn tab_and_the_keys_after_it_reach_the_host() {
    let mut h = Harness::new(Vec2::new(1000.0, 700.0));
    h.frame(Vec::new()).unwrap();
    let key = |key, pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: egui::Modifiers::NONE };
    for (k, sym) in [(Key::Tab, xk::TAB), (Key::A, 'a' as u32), (Key::Tab, xk::TAB), (Key::ArrowDown, xk::DOWN), (Key::Escape, xk::ESCAPE), (Key::B, 'b' as u32)] {
        h.frame(vec![key(k, true)]).unwrap();
        assert!(h.r.held.contains(&sym), "{k:?} is down at the host");
        h.frame(vec![key(k, false)]).unwrap();
        h.frame(Vec::new()).unwrap();
        assert!(!h.r.held.contains(&sym), "{k:?} is let go there");
        assert!(!h.r.keys_elsewhere(&h.ctx), "no button took the keyboard after {k:?}");
    }
    // A field of this window that is clicked has the keys, until the picture is clicked again.
    let field = egui::Id::unique("a field");
    h.ctx.memory_mut(|m| m.request_focus(field));
    assert!(h.r.keys_elsewhere(&h.ctx), "the field has them");
    let at = h.r.last_shown.expect("the picture").center();
    h.click_at(at).unwrap();
    assert!(!h.r.keys_elsewhere(&h.ctx), "the picture has them back");
}

/// The host's pointer comes apart from the pictures and is drawn here where this mouse is, this
/// window's own arrow hidden over the picture; away from it, it shows where the host has it.
#[test]
#[ignore]
fn the_host_pointer_is_drawn_where_this_mouse_is() {
    let mut h = Harness::new(Vec2::new(1000.0, 700.0));
    for _ in 0..40 {
        h.frame(Vec::new()).unwrap();
        if h.r.pointer_tex.is_some() && h.r.host_pointer.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(h.r.pointer_tex.is_some(), "its shape came");
    assert_eq!(h.r.host_pointer, Some((640, 360)), "and where the host has it");
    let pic = h.r.last_shown.expect("the picture");
    // Not over the picture: drawn where the host has it, and the arrow is this window's.
    h.frame(vec![Event::PointerMoved(Pos2::new(pic.right() + 5.0, pic.top() + 5.0))]).unwrap();
    let k = pic.width() / 1280.0;
    let at_host = pic.min + Vec2::new(640.0, 360.0) * k - Vec2::splat(k);
    assert!(h.drawn_image_at(at_host), "drawn at the host's place");
    assert_ne!(h.cursor, egui::CursorIcon::None);
    // Over it: drawn right where this mouse is, the arrow hidden.
    let p = pic.min + Vec2::new(100.0, 80.0);
    h.frame(vec![Event::PointerMoved(p)]).unwrap();
    assert!(h.drawn_image_at(p - Vec2::splat(k)), "drawn where this mouse is");
    assert_eq!(h.cursor, egui::CursorIcon::None);
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
    // In place of the screen the picture pauses; the toolbar's way back starts it again.
    h.tool_menu(ic::TERMINAL).unwrap();
    h.click("Terminal in place of the screen").unwrap();
    assert!(h.r.term_inline && h.r.paused, "the terminal in place: the picture pauses");
    h.tool(ic::MONITOR).unwrap();
    assert!(!h.r.term_inline && h.r.term_split && !h.r.paused, "back on the screen, running, the terminal under it");
    h.tool(ic::TERMINAL).unwrap();

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

/// An upload and a folder's download cut off by a broken connection go on in the next one from
/// where they were, as the workspace carries them over, and arrive whole.
#[test]
#[ignore]
fn transfers_go_on_after_the_connection_breaks() {
    let mut h = Harness::new(Vec2::new(800.0, 600.0));
    let dir = std::env::temp_dir().join(format!("sshremote-goon-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (src, there, here) = (dir.join("src"), dir.join("there"), dir.join("here"));
    std::fs::create_dir_all(src.join("folder")).unwrap();
    std::fs::create_dir_all(&there).unwrap();
    std::fs::create_dir_all(&here).unwrap();
    // Bytes that do not compress, so what crosses the link is what is counted.
    let pattern = |n: usize, seed: u64| -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 32) as u8
            })
            .collect()
    };
    let big = pattern(24 << 20, 1);
    std::fs::write(src.join("big.bin"), &big).unwrap();
    let files = [("a.bin", pattern(3 << 20, 2)), ("b.bin", pattern(20 << 20, 3)), ("c.bin", pattern(1 << 20, 4))];
    for (name, data) in &files {
        std::fs::write(src.join("folder").join(name), data).unwrap();
    }
    let until = |h: &mut Harness, what: &str, done: &dyn Fn(&Remote) -> bool| {
        for _ in 0..4000 {
            h.frame(Vec::new()).unwrap();
            if done(&h.r) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("{what} never happened");
    };
    // The connection breaks as the workspace sees it break, and is made again.
    let again = |h: &mut Harness| {
        let carry = h.r.take_carry().expect("transfers carried over");
        h.r.close();
        let opt = crate::parse_client(&["--gui".into(), "--local".into()]).unwrap();
        let (sess, _tx, rx) = Session::connect(&opt).expect("a local agent");
        h.r = Remote::new(sess, rx, Control::new(&opt, Mode::True), "test desktop".into(), None);
        h.r.carry_on(carry);
    };
    until(&mut h, "the agent saying it goes on with transfers", &|r| r.resume.can);

    h.r.upload_into(&[src.join("big.bin")], there.to_str().unwrap()).unwrap();
    until(&mut h, "6 MB uploaded", &|r| r.uploads.front().is_some_and(|u| u.acked >= 6 << 20));
    let had = h.r.uploads.front().unwrap().acked;
    assert!(had < big.len() as u64, "still under way when it breaks");
    again(&mut h);
    // And again before the agent of the new connection said a word: still carried over.
    again(&mut h);
    until(&mut h, "the upload done", &|r| r.uploads.is_empty());
    assert!(std::fs::read(there.join("big.bin")).unwrap() == big, "arrived whole and right");
    assert!(!there.join("big.bin.part").exists());
    let sent = h.r.sess.tx_bytes;
    assert!(sent < big.len() as u64 - had + (1 << 20), "only the rest was sent again: {sent} bytes after {had}");

    let cmd = crate::filewin::Cmd::Download { paths: vec![src.join("folder").to_string_lossy().into_owned()], to: here.clone() };
    h.r.file_cmd(cmd).unwrap();
    until(&mut h, "5 MB of b.bin downloaded", &|r| r.downloads.iter().any(|d| d.1.ends_with("b.bin") && r.inbox.received(d.0).is_some_and(|(got, _)| got >= 5 << 20)));
    let rx_before = h.r.sess.rx();
    again(&mut h);
    until(&mut h, "the download done", &|r| r.downloads.is_empty() && r.resume.fetches.is_empty());
    for (name, data) in &files {
        assert!(std::fs::read(here.join("folder").join(name)).unwrap() == *data, "{name} arrived whole and right");
        assert!(!here.join("folder").join(format!("{name}.part")).exists());
    }
    assert!(!here.join("folder").join("a (2).bin").exists(), "what had arrived is not fetched again");
    let got = h.r.sess.rx();
    assert!(got < (20 << 20) - (5 << 20) + (1 << 20) + (2 << 20), "only the rest came again: {got} bytes (first connection {rx_before})");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The mouse for games: the window holds the pointer, the mouse's movement moves the host's by
/// as much (whole pixels, the rest kept for the next), Ctrl+Alt lets it go, and a click on the
/// picture holds it again without reaching the host.
#[test]
#[ignore]
fn the_mouse_for_games_moves_the_host_pointer_by_its_movement() {
    let mut h = Harness::new(Vec2::new(1000.0, 700.0));
    h.frame(Vec::new()).unwrap();
    let pic = h.r.last_shown.expect("the picture");
    let wait = |h: &mut Harness, what: &str, ok: &dyn Fn(&Remote) -> bool| {
        for _ in 0..200 {
            h.frame(Vec::new()).unwrap();
            if ok(&h.r) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("{what}: host pointer {:?}", h.r.host_pointer);
    };
    h.frame(vec![Event::PointerMoved(pic.center())]).unwrap();
    let start = h.r.host_pointer.expect("placed");
    h.r.games = true;
    h.r.games_held = true;
    h.frame(Vec::new()).unwrap();
    assert!(h.r.grab_set, "held");
    // Moves here are no place there any more; the movement is.
    h.frame(vec![Event::PointerMoved(pic.min + Vec2::splat(5.0)), Event::MouseMoved(Vec2::new(30.4, -20.0)), Event::MouseMoved(Vec2::new(0.7, 0.0))]).unwrap();
    let want = (start.0 + 31, start.1 - 20);
    wait(&mut h, "moved by 31, -20", &|r| r.host_pointer == Some(want));
    assert!((h.r.moved_by.x - 0.1).abs() < 0.01, "the rest kept");
    // Ctrl+Alt with a key between is the host's (Ctrl+Alt+Delete, a game's own keys).
    let both = egui::Modifiers { ctrl: true, alt: true, ..Default::default() };
    let delete = |pressed| Event::Key { key: egui::Key::Delete, physical_key: None, pressed, repeat: false, modifiers: both };
    h.frame(vec![Event::ModifiersChanged(both), delete(true), delete(false), Event::ModifiersChanged(egui::Modifiers::NONE)]).unwrap();
    assert!(h.r.games_held && h.r.grab_set, "still held");
    // Ctrl+Alt alone, let go, lets it go; a click on the picture holds it again, and neither
    // its press nor its release goes further.
    h.frame(vec![Event::ModifiersChanged(both)]).unwrap();
    assert!(h.r.games_held, "not before it is let go");
    h.frame(vec![Event::ModifiersChanged(egui::Modifiers::NONE)]).unwrap();
    assert!(!h.r.games_held && !h.r.grab_set, "let go");
    let press = |pressed| Event::PointerButton { pos: pic.center(), button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    h.frame(vec![Event::PointerMoved(pic.center()), press(true)]).unwrap();
    assert!(h.r.games_held && h.r.hold_click == Some(PointerButton::Primary), "held again by the click");
    h.frame(vec![press(false)]).unwrap();
    assert!(h.r.grab_set && h.r.hold_click.is_none());
    // Turned off: the window lets it go.
    h.r.games = false;
    h.frame(Vec::new()).unwrap();
    assert!(!h.r.grab_set && !h.r.games_held);
}
