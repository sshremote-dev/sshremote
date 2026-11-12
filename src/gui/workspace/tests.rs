use crate::gui::snapshot::RunTest;
use super::*;

use egui::{Event, PointerButton};

struct Harness {
    ctx: egui::Context,
    w: Workspace,
    snap: super::super::snapshot::Snap,
    size: Vec2,
    clock: f64,
}

impl Harness {
    fn new() -> Harness {
        static SCRATCH: std::sync::Once = std::sync::Once::new();
        SCRATCH.call_once(|| {
            let dir = std::env::temp_dir().join(format!("sshremote-ws-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            for var in ["APPDATA", "XDG_CONFIG_HOME"] {
                unsafe { std::env::set_var(var, &dir) };
            }
        });
        let ctx = egui::Context::default();
        crate::gui::theme::style(&ctx);
        let opt = crate::parse_client(&["--gui".into(), "--local".into()]).unwrap();
        Harness { ctx, w: Workspace::new(opt), snap: Default::default(), size: Vec2::new(1280.0, 800.0), clock: 0.0 }
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.clock += 0.05;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.clock),
            focused: true,
            events,
            ..Default::default()
        };
        let w = &mut self.w;
        self.snap.frame(&self.ctx, input, |ui| {
            crate::gui::look::frame(ui.ctx());
            w.ui(ui, Vec::new());
        });
    }

    /// Frames until `done`, or panics after a few seconds.
    fn until(&mut self, what: &str, done: impl Fn(&Workspace) -> bool) {
        for _ in 0..200 {
            self.frame(Vec::new());
            if done(&self.w) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        panic!("never: {what}");
    }

    fn click_at(&mut self, at: Pos2, button: PointerButton) {
        for e in super::super::snapshot::click(at, button) {
            self.frame(e);
        }
        self.frame(Vec::new());
        self.frame(Vec::new());
    }

    /// Clicks what reads `what` in the title bar.
    fn bar(&mut self, what: &str) {
        self.frame(Vec::new());
        let r = self
            .snap
            .find(what)
            .into_iter()
            .rev()
            .find(|r| r.center().y < super::super::chrome::HEIGHT)
            .unwrap_or_else(|| panic!("no {what:?} in the title bar"));
        self.click_at(r.center(), PointerButton::Primary);
    }

    /// Clicks what reads `what`, the topmost drawn (a menu's item).
    fn item(&mut self, what: &str) {
        self.frame(Vec::new());
        let r = *self.snap.find(what).first().unwrap_or_else(|| panic!("no {what:?} drawn"));
        self.click_at(r.center(), PointerButton::Primary);
    }
}

fn live(w: &Workspace, i: usize) -> bool {
    w.panes.get(i).is_some_and(|p| matches!(p.state, State::Live(_)))
}

fn mode_of(w: &Workspace, i: usize) -> Option<PaneMode> {
    match &w.panes.get(i)?.state {
        State::Live(r) => Some(r.mode()),
        _ => None,
    }
}

fn test_desktop() -> Launch {
    Launch::Agent {
        target: Target::Local { capture: "synthetic".into() },
        label: "Test desktop".into(),
        ssh_cmd: None,
        mode: PaneMode::Screen,
    }
}

/// Over a shared screen, whose keys are the host's, Ctrl+Alt+PgUp/PgDn go between the tabs and
/// Ctrl+Alt+Shift+PgUp/PgDn between the panes of one.
#[test]
#[ignore]
fn tabs_and_panes_change_by_key_over_a_shared_screen() {
    let mut h = Harness::new();
    h.w.add(test_desktop(), Place::Tab, PaneStart::default());
    h.w.add(test_desktop(), Place::Tab, PaneStart::default());
    h.w.add(test_desktop(), Place::Split { below: false }, PaneStart::default());
    h.until("the test desktops connect", |w| (0..3).all(|i| live(w, i)));
    assert!(h.w.screen_has_keys(), "a shared screen has the keys");
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    let ca = egui::Modifiers::CTRL | egui::Modifiers::ALT;
    let cas = ca | egui::Modifiers::SHIFT;
    assert_eq!(h.w.current, 1);
    let order = h.w.order(1);
    let active = h.w.tabs[1].active;
    h.frame(vec![key(egui::Key::PageDown, cas)]);
    assert_ne!(h.w.tabs[1].active, active, "Ctrl+Alt+Shift+PgDn: the next pane");
    h.frame(vec![key(egui::Key::PageUp, cas)]);
    assert_eq!(h.w.tabs[1].active, active, "Ctrl+Alt+Shift+PgUp: back");
    assert!(order.contains(&active));
    h.frame(vec![key(egui::Key::PageDown, ca)]);
    assert_eq!(h.w.current, 0, "Ctrl+Alt+PgDn: the next tab, round to the first");
    h.frame(vec![key(egui::Key::PageUp, ca)]);
    assert_eq!(h.w.current, 1, "Ctrl+Alt+PgUp: the tab before");
    h.frame(vec![key(egui::Key::PageDown, egui::Modifiers::CTRL)]);
    assert_eq!(h.w.current, 1, "other keys stay the host's");
}

/// A screen opened into a workspace takes the quality chosen for it then, not the one the
/// workspace was made with.
#[test]
#[ignore]
fn a_pane_keeps_the_quality_it_was_opened_with() {
    let mut h = Harness::new();
    h.w.set_preset(Some(0));
    h.w.add(test_desktop(), Place::Tab, PaneStart::default());
    h.until("the test desktop connects", |w| live(w, 0));
    let State::Live(r) = &h.w.panes[0].state else { unreachable!() };
    assert_eq!(r.preset_name(), crate::proto::PRESETS[0].name);
}

/// In full screen a split tab's screen fills the monitor alone, again after going to another tab
/// and back; stepping to the other pane moves that zoom along, and leaving full screen undoes it.
#[test]
#[ignore]
fn full_screen_keeps_one_screen_alone_across_tabs_and_panes() {
    let mut h = Harness::new();
    h.w.add(test_desktop(), Place::Tab, PaneStart::default());
    h.w.add(test_desktop(), Place::Tab, PaneStart::default());
    h.w.add(test_desktop(), Place::Split { below: false }, PaneStart::default());
    h.until("the test desktops connect", |w| (0..3).all(|i| live(w, i)));
    h.bar(ic::ARROWS_OUT);
    assert!(h.w.full.on);
    let alone = |w: &Workspace| w.zoom.is_some() && w.zoom == Some(w.tabs[w.current].active);
    assert!(alone(&h.w), "the split tab's screen fills the monitor");
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    let ca = egui::Modifiers::CTRL | egui::Modifiers::ALT;
    h.frame(vec![key(egui::Key::PageDown, ca)]);
    assert_eq!((h.w.current, h.w.zoom), (0, None), "the other tab has one pane");
    h.frame(vec![key(egui::Key::PageUp, ca)]);
    assert!(h.w.current == 1 && alone(&h.w), "back on the split tab, its screen alone again");
    h.frame(vec![key(egui::Key::PageDown, ca | egui::Modifiers::SHIFT)]);
    assert!(alone(&h.w) && h.w.full.zoomed == h.w.zoom, "the next pane alone, still full screen's zoom");
    h.w.set_full(&h.ctx, false);
    assert_eq!(h.w.zoom, None, "out of full screen the tab is split as before");
}

/// In a shared screen F11 is the remote computer's; Ctrl+Alt+Enter is the full-screen key
/// there, and it leaves a full screen the window's button started.
#[test]
#[ignore]
fn workspace_full_screen_keys_in_a_shared_screen() {
    let mut h = Harness::new();
    h.w.add(test_desktop(), Place::Tab, PaneStart::default());
    h.until("the test desktop connects", |w| live(w, 0));
    let cae = egui::Modifiers::CTRL | egui::Modifiers::ALT;
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    h.frame(vec![key(egui::Key::F11, egui::Modifiers::NONE)]);
    assert!(!h.w.full.on, "F11 goes to the remote computer");
    h.bar(ic::ARROWS_OUT);
    assert!(h.w.full.on, "the button fills the monitor");
    h.frame(vec![key(egui::Key::Enter, cae)]);
    h.frame(Vec::new());
    assert!(!h.w.full.on, "Ctrl+Alt+Enter leaves it");
    h.frame(vec![key(egui::Key::Enter, cae)]);
    h.frame(Vec::new());
    assert!(h.w.full.on, "and fills the monitor with the screen again");
    h.frame(vec![key(egui::Key::Enter, cae)]);
    h.frame(Vec::new());
    assert!(!h.w.full.on && h.w.zoom.is_none(), "and leaves");
    h.w.close_all();
}

/// The look of Appearance on a workspace of two terminals: a picture across the window, then
/// one in each pane and tiled, own colours, the overlay, and the Appearance window itself.
#[test]
#[ignore]
fn workspace_look_pictures() {
    use crate::gui::look::{self, BgLayer, Fit, Look, Scope};
    let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
    let mut h = Harness::new();
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    let shell = || Launch::Shell { label: profile.name.clone(), profile: profile.clone(), target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None };
    h.w.add(shell(), Place::Tab, PaneStart::default());
    h.w.add(shell(), Place::Split { below: false }, PaneStart::default());
    let shot = |h: &mut Harness, l: &Look, name: &str| {
        look::set(l.clone());
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(30));
            h.frame(Vec::new());
        }
        h.snap.save(&h.ctx, h.size, &dir.join(format!("look_{name}.png")));
    };
    let built = |id: &str, strength: f32, fit: Fit| BgLayer { src: format!("builtin:{id}"), on: true, strength, fit, scope: Scope::Window };
    // Out of the box: the 3D title over the night lake.
    shot(&mut h, &Look::default(), "default");
    // The built-in style: its picture and its animation, across the window.
    for style in ["night"] {
        let l = Look { layers: vec![built(&format!("{style}-motion"), 0.7, Fit::Fill), built(&format!("{style}-still"), 0.7, Fit::Fill)], ..Look::default() };
        shot(&mut h, &l, style);
    }
    // The title in front of the night lake, and the same with each pane its own.
    let night = vec![built("brand", 0.8, Fit::Whole), built("night-motion", 0.7, Fit::Fill), built("night-still", 0.7, Fit::Fill)];
    shot(&mut h, &Look { layers: night.clone(), ..Look::default() }, "brand_night");
    let each: Vec<BgLayer> = night.iter().map(|b| BgLayer { scope: Scope::Pane, ..b.clone() }).collect();
    shot(&mut h, &Look { layers: each, ..Look::default() }, "brand_night_panes");
    // The layer list in Appearance.
    look::open(&h.ctx);
    shot(&mut h, &Look::default(), "editor");
    let wheel = Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -500.0), modifiers: Default::default(), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
    h.frame(vec![Event::PointerMoved(Pos2::new(640.0, 450.0))]);
    for _ in 0..3 {
        h.frame(vec![wheel.clone()]);
    }
    shot(&mut h, &Look::default(), "editor_layers");
    look::close();
    // Other fonts and a larger interface, taken at once.
    if cfg!(windows) {
        let fonts = Look { ui_font: r"C:\Windows\Fonts\georgia.ttf".into(), mono_font: r"C:\Windows\Fonts\cour.ttf".into(), ui_scale: 1.2, ..Look::default() };
        shot(&mut h, &fonts, "fonts");
    }
    look::set(Look::default());
    crate::termwin::reset_look();
    h.w.close_all();
}

/// Each terminal shows what its own computer is doing: an SSH terminal its host's figures (from
/// SSHREMOTE_SSH_HOST), a local one this computer's; then only in the active one.
#[test]
#[ignore]
fn workspace_host_figures() {
    use crate::gui::look::{self, HudScope, Look};
    let (Some(dir), Ok(host)) = (std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from), std::env::var("SSHREMOTE_SSH_HOST")) else { return };
    let mut h = Harness::new();
    look::set(Look { layers: Vec::new(), ..Look::default() });
    let ssh = Launch::Shell { label: host.clone(), profile: crate::termwin::ssh_profile(&host, ""), target: Target::Local { capture: "synthetic".into() }, ssh_cmd: Some(vec!["ssh".into(), host.clone()]) };
    h.w.add(ssh, Place::Tab, PaneStart::default());
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    h.w.add(Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Split { below: false }, PaneStart::default());
    let shown = |h: &mut Harness, what: &str| {
        for _ in 0..160 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            h.frame(Vec::new());
            if !h.snap.find(what).is_empty() {
                return true;
            }
        }
        false
    };
    assert!(shown(&mut h, &format!("{host}  \u{B7}  CPU")), "the SSH terminal shows its host's figures");
    assert!(shown(&mut h, "this computer  \u{B7}  CPU"), "the local one this computer's");
    h.snap.save(&h.ctx, h.size, &dir.join("host_figures.png"));
    look::set(Look { layers: Vec::new(), hud_active_only: true, ..Look::default() });
    for _ in 0..5 {
        h.frame(Vec::new());
    }
    assert!(h.snap.find(&format!("{host}  \u{B7}")).is_empty(), "only the active terminal (the local one) shows them");
    h.snap.save(&h.ctx, h.size, &dir.join("host_figures_active.png"));
    assert_eq!(Look::default().hud_scope, HudScope::Terminals);
    look::set(Look::default());
    h.w.close_all();
}

#[test]
#[ignore]
fn workspace_modes_splits_and_reconnect() {
    let mut h = Harness::new();
    h.w.add(test_desktop(), Place::Tab, PaneStart::default());
    h.until("the test desktop connects", |w| live(w, 0));

    // The mode drop-down turns the pane into a terminal and into files, without connecting again.
    h.bar("Screen sharing");
    h.item("Terminal");
    h.until("terminal mode", |w| mode_of(w, 0) == Some(PaneMode::Terminal));
    h.bar("Terminal");
    h.item("Files");
    h.until("files mode", |w| mode_of(w, 0) == Some(PaneMode::Files));
    h.bar("Files");
    h.item("Screen sharing");
    h.until("screen mode", |w| mode_of(w, 0) == Some(PaneMode::Screen));

    // Split beside: Quick open is asked for; what it opens goes beside, and is active.
    h.bar(ic::SPLIT_SIDE);
    assert_eq!(h.w.picker, Some(Place::Split { below: false }));
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    let shell = Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None };
    h.w.picker = None;
    h.w.add(shell, Place::Split { below: false }, PaneStart::default());
    h.frame(Vec::new());
    let order = h.w.order(0);
    assert_eq!(order.len(), 2, "two panes side by side");
    assert_eq!(h.w.tabs[0].active, order[1], "the new pane is active");

    // A click in the left pane makes it active.
    h.click_at(Pos2::new(200.0, 400.0), PointerButton::Primary);
    assert_eq!(h.w.tabs[0].active, order[0], "a click activates the pane");
    // Move it right: the two swap places.
    h.bar(ic::CARET_RIGHT);
    assert_eq!(h.w.order(0), vec![order[1], order[0]], "moved right");
    h.bar(ic::CARET_LEFT);
    assert_eq!(h.w.order(0), order, "and back");

    // The pane's own full screen fills the window with it alone.
    h.click_at(Pos2::new(200.0, 400.0), PointerButton::Primary);
    h.bar(ic::CORNERS_OUT);
    assert_eq!(h.w.zoom, Some(order[0]), "pane full screen");
    h.w.set_zoom(None);
    // Full screen from a screen shows it alone; leaving brings the split back.
    h.bar(ic::ARROWS_OUT);
    assert!(h.w.full.on && h.w.zoom == Some(order[0]), "the screen alone on the monitor");
    let ctx = h.ctx.clone();
    h.w.set_full(&ctx, false);
    assert!(!h.w.full.on && h.w.zoom.is_none(), "the split again");

    // A lost connection is made again by itself, in the mode it had.
    h.w.set_mode(order[0], PaneMode::Terminal);
    h.until("terminal again", |w| mode_of(w, 0) == Some(PaneMode::Terminal));
    if let State::Live(r) = &mut h.w.panes[0].state {
        r.close();
    }
    h.until("the pane notices", |w| matches!(w.panes[0].state, State::Down(_)));
    h.until("it connects again", |w| live(w, 0));
    assert_eq!(mode_of(&h.w, 0), Some(PaneMode::Terminal), "in the mode it had");

    // What the user chose stays chosen: a screen set to view only is view only again.
    h.w.set_mode(order[0], PaneMode::Screen);
    h.until("screen again", |w| mode_of(w, 0) == Some(PaneMode::Screen));
    if let State::Live(r) = &mut h.w.panes[0].state {
        r.set_view_only(true);
        r.close();
    }
    h.until("the pane notices", |w| matches!(w.panes[0].state, State::Down(_)));
    h.until("it connects again", |w| live(w, 0));
    let State::Live(r) = &h.w.panes[0].state else { unreachable!() };
    assert!(r.view_only(), "still view only after the reconnect");
    assert!(r.toast_texts().contains(&"Connected again"), "and it says it is back");

    // Closing the tab closes both panes; nothing is left.
    h.w.close_tab(0);
    h.frame(Vec::new());
    assert!(h.w.tabs.is_empty() && h.w.panes.is_empty());
}

/// What could not be done while tabs are open shows over the panes, not on the open window
/// the user is no longer looking at.
#[test]
fn a_failure_in_the_workspace_shows_there() {
    let mut h = Harness::new();
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    let shell = Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None };
    h.w.add(shell, Place::Tab, PaneStart::default());
    h.frame(Vec::new());
    h.w.failed = Some("The script failed: no such file".into());
    h.frame(Vec::new());
    assert!(h.w.failed.is_none(), "taken by the workspace");
    assert_eq!(h.w.toasts.texts(), ["The script failed: no such file"]);
    h.w.close_all();
}

/// Pictures of the workspace to look at: split, narrow, a pane in full screen, connecting.
#[test]
#[ignore]
fn workspace_pictures() {
    let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
    for dark in [true, false] {
        let mut h = Harness::new();
        crate::gui::theme::apply_theme(&h.ctx, dark);
        let name = if dark { "dark" } else { "light" };
        let save = |h: &mut Harness, what: &str| {
            for _ in 0..8 {
                h.frame(Vec::new());
            }
            h.snap.save(&h.ctx, h.size, &dir.join(format!("ws_{name}_{what}.png")));
        };
        h.w.add(test_desktop(), Place::Tab, PaneStart::default());
        h.until("connected", |w| live(w, 0));
        // One pane: zoom and layout show, greyed.
        save(&mut h, "one_pane");
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        h.w.add(
            Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
            Place::Split { below: false },
            PaneStart::default(),
        );
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            h.frame(Vec::new());
        }
        save(&mut h, "split");
        h.bar(ic::LAYOUT);
        save(&mut h, "layout_menu");
        h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        let first = h.w.order(0)[0];
        h.w.confirm = Some(Confirm::Pane(first));
        save(&mut h, "confirm");
        h.w.confirm = None;
        h.w.tabs[0].active = first;
        h.w.set_mode(first, PaneMode::Files);
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(30));
            h.frame(Vec::new());
        }
        save(&mut h, "files");
        h.w.set_mode(first, PaneMode::Screen);
        h.w.tabs[0].active = h.w.order(0)[1];
        h.bar("Terminal");
        save(&mut h, "mode_menu");
        h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        h.size = Vec2::new(760.0, 520.0);
        h.click_at(Pos2::new(150.0, 300.0), PointerButton::Primary);
        save(&mut h, "narrow");
        h.bar(ic::DOTS);
        save(&mut h, "narrow_more");
        h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        h.size = Vec2::new(1280.0, 800.0);
        let first = h.w.order(0)[0];
        h.w.tabs[0].active = first;
        h.w.set_zoom(Some(first));
        h.frame(vec![Event::PointerMoved(Pos2::new(640.0, 2.0))]);
        save(&mut h, "zoom_dock");
        h.w.set_zoom(None);
        if let State::Live(r) = &mut h.w.panes[0].state {
            r.close();
        }
        h.until("down", |w| matches!(w.panes[0].state, State::Down(_)));
        h.frame(Vec::new());
        h.snap.save(&h.ctx, h.size, &dir.join(format!("ws_{name}_reconnecting.png")));
        h.w.close_tab(0);
        h.frame(Vec::new());
    }
}

/// Many tabs scroll as whole tabs, the current one always in view, with the + and the
/// pane's controls kept clear; the list at the end goes to any tab, middle click closes one.
#[test]
fn many_tabs_scroll_and_keep_the_bar_whole() {
    let mut h = Harness::new();
    for i in 0..30 {
        let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        profile.name = format!("host{i:02}");
        h.w.add(
            Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
            Place::Tab,
            PaneStart::default(),
        );
    }
    let in_bar = |h: &Harness, what: &str| h.snap.find(what).into_iter().filter(|r| r.center().y < super::super::chrome::HEIGHT).collect::<Vec<_>>();
    for current in [29, 0, 17] {
        h.w.current = current;
        h.frame(Vec::new());
        h.frame(Vec::new());
        assert!(!in_bar(&h, &format!("host{current:02}")).is_empty(), "tab {current} is in view");
        let plus = in_bar(&h, ic::PLUS)[0];
        let mode = in_bar(&h, "Terminal")[0];
        assert!(plus.right() < mode.left(), "+ clear of the controls");
    }
    // The list is the caret before the + (the mode drop-down has one too).
    h.frame(Vec::new());
    let plus = in_bar(&h, ic::PLUS)[0];
    let carets = in_bar(&h, ic::CARET_DOWN);
    let list = carets.iter().find(|r| r.right() < plus.left()).unwrap_or_else(|| panic!("no list in {carets:?}, + at {plus:?}"));
    h.click_at(list.center(), PointerButton::Primary);
    h.item("host03");
    assert_eq!(h.w.current, 3, "the list goes to a tab");
    let at = in_bar(&h, "host04")[0].center();
    h.click_at(at, PointerButton::Middle);
    assert!(matches!(h.w.confirm, Some(Confirm::Tab(_))), "middle click asks to close the tab");
    h.frame(vec![Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    assert_eq!(h.w.tabs.len(), 29, "and closes it");
    h.frame(Vec::new());
    assert!(in_bar(&h, "host04").is_empty());
    h.w.close_all();
}

#[test]
fn tab_pictures() {
    let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR").map(std::path::PathBuf::from) else { return };
    for n in [1usize, 3] {
        let mut h = Harness::new();
        h.size = Vec2::new(480.0, 320.0);
        for i in 0..n {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = format!("server-{i}");
            h.w.add(Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
        }
        for _ in 0..8 {
            h.frame(Vec::new());
        }
        h.snap.save(&h.ctx, h.size, &dir.join(format!("tabs_min_{n}.png")));
        h.w.close_all();
    }
    for n in [2usize, 8, 16, 30] {
        let mut h = Harness::new();
        for i in 0..n {
            let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
            profile.name = format!("server-{i}.example.com");
            h.w.add(
                Launch::Shell { label: profile.name.clone(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
                Place::Tab,
                PaneStart::default(),
            );
        }
        h.w.current = n / 2;
        for _ in 0..8 {
            h.frame(vec![Event::PointerMoved(Pos2::new(160.0, 20.0))]);
        }
        h.snap.save(&h.ctx, h.size, &dir.join(format!("tabs_{n}.png")));
        crate::gui::theme::apply_theme(&h.ctx, false);
        h.size = Vec2::new(760.0, 500.0);
        for _ in 0..8 {
            h.frame(Vec::new());
        }
        h.snap.save(&h.ctx, h.size, &dir.join(format!("tabs_{n}_light_narrow.png")));
        h.w.close_all();
    }
    // A tab dragged along the bar, one coming in from another window, and one out.
    let mut h = Harness::new();
    for name in ["alpha", "beta", "gamma"] {
        let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        profile.name = name.into();
        h.w.add(Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
    }
    h.frame(Vec::new());
    let a = h.snap.find("alpha")[0].center();
    let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
    h.frame(vec![Event::PointerMoved(a)]);
    h.frame(vec![press(a, true)]);
    for k in 1..=6 {
        h.frame(vec![Event::PointerMoved(a + Vec2::new(22.0 * k as f32, 0.0))]);
    }
    for _ in 0..6 {
        h.frame(Vec::new());
    }
    h.snap.save(&h.ctx, h.size, &dir.join("tab_drag_float.png"));
    h.frame(vec![press(a + Vec2::new(132.0, 0.0), false)]);
    h.w.drop_hint = Some(2);
    h.frame(Vec::new());
    h.snap.save(&h.ctx, h.size, &dir.join("tab_drop_hint.png"));
    h.w.drop_hint = None;
    let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, h.size)), ..Default::default() };
    let w = &mut h.w;
    h.snap.frame(&h.ctx, input, |ui| {
        w.ui(ui, Vec::new());
        super::super::tab_ghost(ui.ctx(), ic::TERMINAL, "alpha", 1, Pos2::new(600.0, 400.0));
    });
    h.snap.save(&h.ctx, h.size, &dir.join("tab_ghost.png"));
    h.w.close_all();
}

/// Tabs dragged along the bar change places; one dragged away from the bar leaves for a
/// window of its own, with its panes.
#[test]
fn tabs_drag_to_reorder_and_tear_off() {
    let mut h = Harness::new();
    let shell = |name: &str| {
        let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        profile.name = name.into();
        Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
    };
    h.w.add(shell("first"), Place::Tab, PaneStart::default());
    h.w.add(shell("second"), Place::Tab, PaneStart::default());
    h.frame(Vec::new());
    h.frame(Vec::new());
    let at = |h: &Harness, name: &str| {
        h.snap.find(name).into_iter().find(|r| r.center().y < super::super::chrome::HEIGHT).unwrap().center()
    };
    let drag = |h: &mut Harness, from: Pos2, to: Pos2| {
        let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        h.frame(vec![Event::PointerMoved(from)]);
        h.frame(vec![press(from, true)]);
        for k in 1..=8 {
            h.frame(vec![Event::PointerMoved(from + (to - from) * (k as f32 / 8.0))]);
        }
        h.frame(vec![press(to, false)]);
        h.frame(Vec::new());
    };
    let names = |w: &Workspace| -> Vec<String> { (0..w.tabs.len()).map(|t| w.name(w.tabs[t].active).0).collect() };
    assert_eq!(names(&h.w), ["first", "second"]);
    let (a, b) = (at(&h, "first"), at(&h, "second"));
    drag(&mut h, a, b);
    assert_eq!(names(&h.w), ["second", "first"], "dragged along the bar");
    // Away from the bar it is out while held, and handed to the app when let go there.
    let a = at(&h, "first");
    let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
    h.frame(vec![Event::PointerMoved(a)]);
    h.frame(vec![press(a, true)]);
    for k in 1..=8 {
        h.frame(vec![Event::PointerMoved(a + Vec2::new(0.0, 40.0 * k as f32))]);
    }
    assert_eq!(h.w.drag_out, Some(a + Vec2::new(0.0, 320.0)), "out while held");
    h.frame(vec![press(a + Vec2::new(0.0, 320.0), false)]);
    assert_eq!(h.w.dropped, Some((1, a + Vec2::new(0.0, 320.0))), "let go away from the bar");
    assert_eq!(h.w.drag_out, None);
    assert_eq!(names(&h.w), ["second", "first"], "the app takes it from here");
    h.w.close_all();
}

/// A tab from another window goes in where it was dropped, its panes numbered after these.
#[test]
fn a_tab_from_another_window_goes_in_where_dropped() {
    let mut h = Harness::new();
    let shell = |name: &str| {
        let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        profile.name = name.into();
        Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
    };
    h.w.add(shell("a"), Place::Tab, PaneStart::default());
    h.w.add(shell("b"), Place::Tab, PaneStart::default());
    let mut other = Workspace::new(h.w.opt.clone());
    other.add(shell("c"), Place::Tab, PaneStart::default());
    other.add(shell("d"), Place::Split { below: false }, PaneStart::default());
    let moved = other.take_tab(0).expect("a tab");
    h.w.put_tab(moved, 1);
    h.frame(Vec::new());
    let names: Vec<String> = (0..h.w.tabs.len()).map(|t| h.w.name(h.w.tabs[t].active).0).collect();
    assert_eq!(names, ["a", "d", "b"], "in between, its active pane named");
    assert_eq!(h.w.current, 1, "and shown");
    let mut ids: Vec<u64> = h.w.panes.iter().map(|p| p.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4, "every pane its own number");
    assert_eq!(h.w.order(1).len(), 2, "with both its panes");
    h.w.close_all();
    other.close_all();
}

/// Windows Terminal's keys: new tab, next tab, split, between panes, close pane.
#[test]
fn terminal_keys_for_tabs_and_panes() {
    let mut h = Harness::new();
    let shell = |name: &str| {
        let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        profile.name = name.into();
        Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
    };
    h.w.add(shell("a"), Place::Tab, PaneStart::default());
    h.w.add(shell("b"), Place::Tab, PaneStart::default());
    h.w.add(shell("c"), Place::Split { below: false }, PaneStart::default());
    h.frame(Vec::new());
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    let cs = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
    let alt = egui::Modifiers::ALT;
    h.frame(vec![key(egui::Key::T, cs)]);
    assert_eq!(h.w.picker, Some(Place::Tab), "Ctrl+Shift+T opens the dialog");
    h.w.picker = None;
    h.frame(vec![key(egui::Key::Tab, egui::Modifiers::CTRL)]);
    assert_eq!(h.w.current, 0, "Ctrl+Tab: the next tab, round to the first");
    h.frame(vec![key(egui::Key::Tab, egui::Modifiers::CTRL)]);
    assert_eq!(h.w.current, 1);
    let order = h.w.order(1);
    assert_eq!(h.w.tabs[1].active, order[1]);
    h.frame(vec![key(egui::Key::ArrowLeft, alt)]);
    assert_eq!(h.w.tabs[1].active, order[0], "Alt+Left: the pane before");
    h.frame(vec![key(egui::Key::Minus, egui::Modifiers::ALT | egui::Modifiers::SHIFT)]);
    assert_eq!(h.w.picker, Some(Place::Split { below: true }), "Alt+Shift+-: split below");
    h.w.picker = None;
    h.frame(vec![key(egui::Key::W, cs)]);
    assert_eq!(h.w.confirm, Some(Confirm::Pane(order[0])), "Ctrl+Shift+W asks first");
    h.frame(vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
    assert_eq!(h.w.order(1), vec![order[1]], "and closes the pane once told yes");
    h.w.close_all();
}

/// A screen in a tab not shown stops its picture, so it costs the link nothing, and has it
/// again when shown.
#[test]
#[ignore]
fn a_screen_in_a_hidden_tab_stops_its_picture() {
    let mut h = Harness::new();
    h.w.add(test_desktop(), Place::Tab, PaneStart::default());
    h.until("the test desktop connects", |w| live(w, 0));
    let frames = |w: &Workspace| match &w.panes[0].state {
        State::Live(r) => r.frames(),
        _ => 0,
    };
    let settle = |h: &mut Harness, n: usize| {
        for _ in 0..n {
            h.frame(Vec::new());
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    };
    settle(&mut h, 20);
    // How many come in a while shown, then in as long hidden.
    let start = frames(&h.w);
    settle(&mut h, 70);
    let shown = frames(&h.w) - start;
    assert!(shown >= 4, "pictures while shown: {shown}");
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    h.w.add(Launch::Shell { label: "shell".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
    settle(&mut h, 15);
    let hidden = frames(&h.w);
    settle(&mut h, 70);
    assert!(frames(&h.w) <= hidden + 1, "none while hidden ({shown} when shown): {hidden} then {}", frames(&h.w));
    h.w.select_tab(0);
    h.until("pictures again when shown", |w| frames(w) > hidden + 2);
    h.w.close_all();
}

/// Panes in different windows never share an id, so extensions told of them never see one
/// twice, or a "closed" for a pane still open elsewhere.
#[test]
fn pane_ids_are_unique_across_windows() {
    let profile = || crate::termwin::local_profiles().into_iter().next().unwrap();
    let shell = || Launch::Shell { label: "s".into(), profile: profile(), target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None };
    let opt = crate::parse_client(&["--gui".into(), "--local".into()]).unwrap();
    let mut a = Workspace::new(opt.clone());
    a.add(shell(), Place::Tab, PaneStart::default());
    a.add(shell(), Place::Tab, PaneStart::default());
    let mut b = a.take_tab(1).expect("a tab");
    a.add(shell(), Place::Tab, PaneStart::default());
    b.add(shell(), Place::Tab, PaneStart::default());
    let mut ids: Vec<u64> = a.panes.iter().chain(&b.panes).map(|p| p.id).collect();
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n, "all different: {ids:?}");
    a.put_tab(b.take_tab(0).unwrap(), 0);
    let mut back: Vec<u64> = a.panes.iter().map(|p| p.id).collect();
    back.sort();
    back.dedup();
    assert_eq!(back.len(), a.panes.len());
    a.close_all();
    b.close_all();
}

/// A shell in a tab not shown keeps taking its output, so it does not stall.
#[test]
fn shells_in_hidden_tabs_keep_running() {
    let mut h = Harness::new();
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    let shell = |profile: crate::termwin::Profile| Launch::Shell { label: "s".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None };
    h.w.add(shell(profile.clone()), Place::Tab, PaneStart::default());
    h.w.add(shell(profile), Place::Tab, PaneStart::default());
    h.w.current = 1;
    let hidden = h.w.tabs[0].active;
    for _ in 0..60 {
        h.frame(Vec::new());
        std::thread::sleep(std::time::Duration::from_millis(50));
        let i = h.w.index(hidden).unwrap();
        if let State::Shell(t) = &h.w.panes[i].state
            && t.output_seen() > 0
        {
            break;
        }
    }
    let i = h.w.index(hidden).unwrap();
    let State::Shell(t) = &h.w.panes[i].state else { panic!("a shell") };
    assert!(t.output_seen() > 0, "the hidden shell's prompt was read");
    h.w.close_all();
}

/// While a dialog asks something, the panes behind it get no keys; closing the window asks
/// too; F11 and Ctrl+Shift+Enter fill the screen with the window or the pane.
#[test]
fn dialogs_keep_the_keys_and_the_window_asks_before_closing() {
    let mut h = Harness::new();
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    h.w.add(Launch::Shell { label: "s".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
    h.frame(Vec::new());
    let id = h.w.tabs[0].active;
    let keys_on = |h: &Harness| match &h.w.panes[0].state {
        State::Shell(t) => t.keys_on,
        _ => false,
    };
    assert!(keys_on(&h));
    h.w.confirm = Some(Confirm::Pane(id));
    h.frame(Vec::new());
    assert!(!keys_on(&h), "the dialog has the keys");
    h.w.confirm = None;
    h.frame(Vec::new());
    assert!(keys_on(&h));
    // The system's close (Alt+F4): asked, not done.
    h.clock += 0.05;
    let mut input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, h.size)),
        time: Some(h.clock),
        focused: true,
        ..Default::default()
    };
    input.viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    let w = &mut h.w;
    h.snap.frame(&h.ctx, input, |ui| {
        w.ui(ui, Vec::new());
    });
    assert_eq!(h.w.confirm, Some(Confirm::Window), "closing the window asks");
    assert_eq!(h.w.panes.len(), 1);
    h.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    assert_eq!(h.w.confirm, None);
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    h.frame(vec![key(egui::Key::F11, egui::Modifiers::NONE)]);
    assert!(h.w.full.on, "F11: the window's full screen");
    h.frame(vec![key(egui::Key::F11, egui::Modifiers::NONE)]);
    assert!(!h.w.full.on);
    h.frame(vec![key(egui::Key::Enter, egui::Modifiers::CTRL | egui::Modifiers::SHIFT)]);
    assert_eq!(h.w.zoom, Some(id), "Ctrl+Shift+Enter: the pane's");
    h.w.close_all();
}

/// Points of windows on monitors of different scales meet in screen pixels.
#[test]
fn screen_points_are_pixels() {
    let ctx = egui::Context::default();
    let mut input = egui::RawInput::default();
    let info = input.viewports.entry(egui::ViewportId::ROOT).or_default();
    info.inner_rect = Some(Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 600.0)));
    info.native_pixels_per_point = Some(1.5);
    let mut got = Pos2::ZERO;
    let _ = ctx.run_test(input.clone(), |ui| got = screen_point(ui.ctx(), Pos2::new(10.0, 20.0)));
    assert_eq!(got, Pos2::new(165.0, 105.0));
    // With the page zoomed, a point in it is that much further from the window's corner, and
    // a place on the screen comes back to the same point.
    ctx.set_zoom_factor(1.25);
    let mut back = Pos2::ZERO;
    let _ = ctx.run_test(input, |ui| {
        got = screen_point(ui.ctx(), Pos2::new(10.0, 20.0));
        back = window_point(ui.ctx(), got);
    });
    assert_eq!(got, Pos2::new(150.0 + 18.75, 75.0 + 37.5));
    assert!((back - Pos2::new(10.0, 20.0)).length() < 0.01, "{back:?}");
}

/// Let go outside the window, a tab drag ends even when the pointer is gone in that frame.
#[test]
fn a_drag_let_go_outside_ends() {
    let mut h = Harness::new();
    for name in ["a", "b"] {
        let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        profile.name = name.into();
        h.w.add(Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
    }
    h.frame(Vec::new());
    let a = h.snap.find("a").into_iter().find(|r| r.center().y < super::super::chrome::HEIGHT).unwrap().center();
    let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
    h.frame(vec![Event::PointerMoved(a)]);
    h.frame(vec![press(a, true)]);
    for k in 1..=6 {
        h.frame(vec![Event::PointerMoved(a + Vec2::new(0.0, 60.0 * k as f32))]);
    }
    let out = a + Vec2::new(0.0, 360.0);
    h.frame(vec![press(out, false), Event::PointerGone]);
    assert_eq!(h.w.dragging_tab(), None, "the drag ended");
    assert!(h.w.dropped.is_some(), "and the tab was let go away from the bar");
    h.w.close_all();
}

/// Closing a tab or a pane asks first: Escape keeps it, Enter closes it.
#[test]
fn closing_asks_first() {
    let mut h = Harness::new();
    let shell = |name: &str| {
        let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        profile.name = name.into();
        Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
    };
    h.w.add(shell("one"), Place::Tab, PaneStart::default());
    h.w.add(shell("two"), Place::Tab, PaneStart::default());
    h.w.add(shell("three"), Place::Split { below: false }, PaneStart::default());
    h.frame(Vec::new());
    let key = |key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
    // The pane's own close, last of its controls (the tab's is on the left).
    for _ in 0..4 {
        h.frame(Vec::new());
    }
    let x = h.snap.find(ic::X).into_iter().filter(|r| r.center().y < super::super::chrome::HEIGHT).max_by(|a, b| a.left().total_cmp(&b.left())).unwrap();
    h.click_at(x.center(), PointerButton::Primary);
    assert!(matches!(h.w.confirm, Some(Confirm::Pane(_))), "a pane's close asks");
    assert!(!h.snap.find("Close three?").is_empty(), "naming the pane");
    h.frame(vec![key(egui::Key::Escape)]);
    assert_eq!(h.w.confirm, None);
    assert_eq!(h.w.order(1).len(), 2, "Escape keeps it");
    // A tab's close: the whole tab, its panes too.
    let id = h.w.tabs[1].active;
    h.w.confirm = Some(Confirm::Tab(id));
    h.frame(Vec::new());
    assert!(!h.snap.find("Its 2 panes close").is_empty());
    h.frame(vec![key(egui::Key::Enter)]);
    assert_eq!(h.w.tabs.len(), 1, "the tab went, with both panes");
    assert_eq!(h.w.panes.len(), 1);
    h.w.close_all();
}

/// Every pane can fill the window: a terminal gets the button among its controls.
/// What extensions add, driven as a user would: a panel's link goes back to its extension as
/// an action, the palette runs what is chosen, and a terminal asked for opens.
#[test]
fn extensions_show_panels_take_commands_and_open_terminals() {
    let mut h = Harness::new();
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    h.w.add(Launch::Shell { label: "t".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
    let md = "Tunnels

| Host | |
|---|---|
| db | [Start](sshremote:start?host=db) |
";
    crate::exthost::test_message("t.panel", serde_json::json!({ "method": "panel.show", "params": { "id": "p", "title": "Tunnels here", "markdown": md } }));
    crate::exthost::test_message("t.panel", serde_json::json!({ "method": "status.set", "params": { "id": "n", "text": "2 tunnels" } }));
    for _ in 0..4 {
        h.frame(Vec::new());
    }
    assert!(!h.snap.find("Tunnels here").is_empty() && !h.snap.find("2 tunnels").is_empty(), "the panel and the status item show");
    if let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR") {
        h.snap.save(&h.ctx, h.size, &std::path::Path::new(&dir).join("ext_panel.png"));
    }
    let start = h.snap.find("Start")[0].center();
    crate::exthost::test_sent();
    h.frame(vec![Event::PointerMoved(start)]);
    let press = |pressed| Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    h.frame(vec![press(true)]);
    h.frame(vec![press(false)]);
    let sent = crate::exthost::test_sent();
    assert!(
        sent.iter().any(|(e, m, p)| e == "t.panel" && m == "action" && p["action"] == "start" && p["args"]["host"] == "db"),
        "the link came back as an action: {sent:?}"
    );
    // The palette: Ctrl+Shift+P, a filter, Enter.
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    h.frame(vec![Event::PointerMoved(Pos2::new(300.0, 300.0))]);
    h.frame(Vec::new());
    h.frame(vec![key(egui::Key::P, egui::Modifiers::CTRL | egui::Modifiers::SHIFT)]);
    h.frame(Vec::new());
    h.frame(Vec::new());
    assert!(!h.snap.find("Split down").is_empty(), "the palette lists the workspace's commands");
    h.frame(vec![Event::Text("split down".into())]);
    h.frame(vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
    assert_eq!(h.w.picker, Some(Place::Split { below: true }), "Enter ran the command");
    h.w.picker = None;
    // A terminal an extension asks for opens beside the active pane.
    crate::exthost::test_request(crate::exthost::Request::OpenTerminal { argv: vec!["ssh".into(), "db".into()], label: "db".into(), split: Some(false) });
    h.frame(Vec::new());
    assert!(h.w.panes.iter().any(|p| p.label == "db"), "the terminal opened");
    // An extension's question: a dialog, an answer typed, Enter; the answer goes back.
    crate::exthost::test_sent();
    crate::exthost::test_message("t.panel", serde_json::json!({ "id": 5, "method": "input.ask", "params": { "title": "Port", "prompt": "Which port to forward?" } }));
    h.frame(Vec::new());
    h.frame(Vec::new());
    assert!(!h.snap.find("Which port to forward?").is_empty(), "the question shows");
    h.frame(vec![Event::Text("8080".into())]);
    h.frame(vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
    let sent = crate::exthost::test_sent();
    assert!(sent.iter().any(|(e, m, p)| e == "t.panel" && m == "reply" && p["id"] == 5 && p["result"]["value"] == "8080"), "{sent:?}");
    // A form: the first field typed in, the box ticked, Enter; the values come back by id.
    crate::exthost::test_message("t.panel", serde_json::json!({ "id": 6, "method": "input.form", "params": { "title": "Forward", "submit": "Forward", "fields": [
        { "id": "dest", "label": "Leads to", "type": "text" },
        { "id": "tls", "label": "Use TLS", "type": "bool" },
        { "id": "host", "label": "Host", "type": "choice", "choices": ["db", "web"] }
    ] } }));
    h.frame(Vec::new());
    h.frame(Vec::new());
    assert!(!h.snap.find("Leads to").is_empty() && !h.snap.find("Use TLS").is_empty(), "the form's fields show");
    h.frame(vec![Event::Text("localhost:80".into())]);
    let tick = h.snap.find("Use TLS")[0];
    let at = Pos2::new(tick.right() + 20.0, tick.center().y);
    h.frame(vec![Event::PointerMoved(at)]);
    let press = |pressed| Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    h.frame(vec![press(true)]);
    h.frame(vec![press(false)]);
    h.frame(vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
    let sent = crate::exthost::test_sent();
    assert!(
        sent.iter().any(|(_, m, p)| m == "reply" && p["id"] == 6 && p["result"]["values"] == serde_json::json!({ "dest": "localhost:80", "tls": true, "host": "db" })),
        "{sent:?}"
    );
    crate::exthost::test_message("t.panel", serde_json::json!({ "method": "exit" }));
}

#[test]
fn a_terminal_pane_fills_the_window() {
    let mut h = Harness::new();
    let shell = || {
        let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        Launch::Shell { label: "t".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
    };
    // One pane: nothing to zoom; the button shows, greyed, as the layout menu does.
    h.w.add(shell(), Place::Tab, PaneStart::default());
    for _ in 0..4 {
        h.frame(Vec::new());
    }
    assert!(!h.snap.find(ic::CORNERS_OUT).is_empty(), "the zoom button shows with one pane");
    h.bar(ic::CORNERS_OUT);
    assert_eq!(h.w.zoom, None, "and does nothing");
    h.w.add(shell(), Place::Split { below: false }, PaneStart::default());
    for _ in 0..4 {
        h.frame(Vec::new());
    }
    h.bar(ic::CORNERS_OUT);
    assert_eq!(h.w.zoom, Some(h.w.tabs[0].active), "the pane alone, in the tab");
    assert!(!h.w.full.on, "the window stays as it is");
    for _ in 0..3 {
        h.frame(Vec::new());
    }
    assert!(h.snap.find(ic::PLUS).iter().any(|r| r.center().y < super::super::chrome::HEIGHT), "the tabs stay");
    h.bar(ic::CORNERS_IN);
    assert_eq!(h.w.zoom, None, "back to the grid");
    // The window's full screen is its own button, at the right, and keeps a grid of shells.
    h.bar(ic::ARROWS_OUT);
    assert!(h.w.full.on && h.w.zoom.is_none());
    h.w.close_all();
}

/// Splits share the room evenly; the layouts arrange a tab as tmux's do.
#[test]
fn splits_are_even_and_layouts_arrange() {
    let rects = |node: &Node| {
        fn walk(n: &Node, r: Rect, out: &mut Vec<(u64, Rect)>) {
            match n {
                Node::Leaf(id) => out.push((*id, r)),
                Node::Split { below, ratio, a, b } => {
                    let (ra, rb) = if *below {
                        let y = r.top() + r.height() * ratio;
                        (r.with_max_y(y), r.with_min_y(y))
                    } else {
                        let x = r.left() + r.width() * ratio;
                        (r.with_max_x(x), r.with_min_x(x))
                    };
                    walk(a, ra, out);
                    walk(b, rb, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(node, Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 600.0)), &mut out);
        out
    };
    // Three side by side, the third split off the second: a third each.
    let mut root = Node::Leaf(1);
    root.split(1, 2, false);
    root.even_out(2, false);
    root.split(2, 3, false);
    root.even_out(3, false);
    for (_, r) in rects(&root) {
        assert!((r.width() - 400.0).abs() < 0.5, "{r:?}");
    }
    // A size set by hand elsewhere stays: one wide on the left, a stack on the right split.
    let mut root = Node::Leaf(1);
    root.split(1, 2, false);
    root.split(2, 3, true);
    if let Node::Split { ratio, .. } = &mut root {
        *ratio = 0.7;
    }
    root.split(3, 4, false);
    root.even_out(4, false);
    let r = rects(&root);
    assert!((r[0].1.width() - 840.0).abs() < 0.5, "the hand-set width stays: {r:?}");
    assert!((r[2].1.width() - r[3].1.width()).abs() < 0.5, "the new pair is even: {r:?}");
    // Mixed: one on the left, two stacked on the right; the stack's panes share its height.
    let mut root = Node::Leaf(1);
    root.split(1, 2, false);
    root.split(2, 3, true);
    root.balance();
    let r = rects(&root);
    assert!((r[0].1.width() - 600.0).abs() < 0.5 && (r[1].1.height() - 300.0).abs() < 0.5, "{r:?}");
    let ids = [1, 2, 3, 4, 5];
    let side = rects(&Node::arranged(&ids, 3, Layout::Side));
    assert!(side.iter().all(|(_, r)| (r.width() - 240.0).abs() < 0.5 && r.height() == 600.0));
    let stacked = rects(&Node::arranged(&ids, 3, Layout::Stacked));
    assert!(stacked.iter().all(|(_, r)| (r.height() - 120.0).abs() < 0.5));
    let main = rects(&Node::arranged(&ids, 3, Layout::MainLeft));
    assert_eq!(main[0].0, 3, "the active pane is the main one");
    assert!((main[0].1.width() - 720.0).abs() < 0.5 && (main[1].1.height() - 150.0).abs() < 0.5);
    let top = rects(&Node::arranged(&ids, 3, Layout::MainTop));
    assert_eq!(top[0].0, 3);
    assert!((top[0].1.height() - 360.0).abs() < 0.5 && (top[1].1.width() - 300.0).abs() < 0.5);
    let grid = rects(&Node::arranged(&[1, 2, 3, 4], 1, Layout::Grid));
    assert!(grid.iter().all(|(_, r)| (r.width() - 600.0).abs() < 0.5 && (r.height() - 300.0).abs() < 0.5), "{grid:?}");
    let grid = rects(&Node::arranged(&ids, 1, Layout::Grid));
    assert_eq!(grid.len(), 5);
    assert!((grid[0].1.height() - 300.0).abs() < 0.5, "two rows: {grid:?}");
}

/// Closing panes and tabs keeps the rest as it was: the current tab, full screen off once
/// the pane that filled the screen is gone, and Alt+arrows left to a lone shell.
#[test]
fn closing_keeps_the_rest_in_order() {
    let mut h = Harness::new();
    let shell = |name: &str| {
        let mut profile = crate::termwin::local_profiles().into_iter().next().unwrap();
        profile.name = name.into();
        Launch::Shell { label: name.into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None }
    };
    for n in ["a", "b", "c"] {
        h.w.add(shell(n), Place::Tab, PaneStart::default());
    }
    h.w.current = 1;
    h.frame(Vec::new());
    h.w.close_tab(0);
    assert_eq!(h.w.name(h.w.tabs[h.w.current].active).0, "b", "the current tab stays");
    // A zoomed pane, then closed: the tab is a grid again.
    h.w.add(shell("d"), Place::Split { below: false }, PaneStart::default());
    let d = h.w.tabs[h.w.current].active;
    h.w.set_zoom(Some(d));
    h.frame(Vec::new());
    assert_eq!(h.w.zoom, Some(d));
    h.w.close_pane(d);
    h.frame(Vec::new());
    assert_eq!(h.w.zoom, None, "the zoom ends with its pane");
    // Alt+Left in a tab of one pane is the shell's.
    h.frame(vec![Event::Key { key: egui::Key::ArrowLeft, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::ALT }]);
    let kept = h.ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Key { key: egui::Key::ArrowLeft, .. })));
    assert!(kept, "Alt+Left reaches the shell");
    h.w.close_all();
}

/// A shell turned to the screen of a host without the agent, then back to Terminal, is the
/// same shell again.
#[test]
fn a_shell_that_cannot_reach_the_agent_stays_a_shell() {
    let mut h = Harness::new();
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    let target = Target::Ssh {
        host: "nobody@127.0.0.1".into(),
        port: Some(1),
        args: vec!["-o".into(), "ConnectTimeout=2".into()],
        remote: "sshremote --agent".into(),
        capture: "auto".into(),
        password: None,
        unattended: true,
    };
    h.w.add(Launch::Shell { label: "s".into(), profile, target, ssh_cmd: None }, Place::Tab, PaneStart::default());
    h.frame(Vec::new());
    let id = h.w.tabs[0].active;
    h.w.set_mode(id, PaneMode::Screen);
    assert!(matches!(h.w.panes[0].state, State::Down(_)));
    h.w.set_mode(id, PaneMode::Terminal);
    assert!(matches!(h.w.panes[0].state, State::Shell(_)), "back to the shell it was");
    h.w.close_all();
}

/// In full screen the title bar gives way to a dock at the top edge: folded, the pane's state and
/// a caret; unfolded, the bar's tabs and controls.
#[test]
fn full_screen_dock_folds_and_unfolds() {
    // Folded at first, as for someone who never unfolded it.
    crate::hosts::set_setting("toolbar_expanded", "no");
    let mut h = Harness::new();
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    h.w.add(
        Launch::Shell { label: "shell".into(), profile, target: Target::Local { capture: "synthetic".into() }, ssh_cmd: None },
        Place::Tab,
        PaneStart::default(),
    );
    h.frame(Vec::new());
    h.bar(ic::ARROWS_OUT);
    assert!(h.w.full.on);
    let away = Pos2::new(600.0, 500.0);
    let tabs_shown = |h: &Harness| !h.snap.find(ic::PLUS).is_empty();
    h.frame(vec![Event::PointerMoved(away)]);
    assert!(!tabs_shown(&h), "folded: no title bar");
    assert!(h.w.panes_area.top() < 1.0, "the panes start at the top: {:?}", h.w.panes_area);
    assert!(!h.snap.find(ic::CARET_DOWN).is_empty(), "the folded dock greets at first");
    for _ in 0..80 {
        h.frame(vec![Event::PointerMoved(away)]);
    }
    assert!(h.snap.find(ic::CARET_DOWN).is_empty(), "then steps aside: the picture is whole");
    h.frame(vec![Event::PointerMoved(Pos2::new(900.0, 1.0))]);
    h.frame(Vec::new());
    let caret = h.snap.find(ic::CARET_DOWN).into_iter().next().expect("back at the top edge");
    assert!(caret.top() < 20.0 && caret.height() < 16.0, "a small tab at the top edge: {caret:?}");
    let name = h.snap.find("shell").into_iter().next().expect("the pane's name in the tab");
    assert!(name.height() < 16.0, "in small type: {name:?}");
    // A click on the tab unfolds it: the band takes its place at the top edge.
    h.click_at(name.center(), PointerButton::Primary);
    assert!(h.w.dock.open && tabs_shown(&h), "unfolded");
    let plus = h.snap.find(ic::PLUS).into_iter().next().unwrap();
    assert!(plus.top() < 20.0, "the band hangs from the top edge, not below the tab: {plus:?}");
    assert!(h.snap.find("shell").iter().all(|r| r.height() > 12.0), "the tab gave way to the band");
    // Away from it, it folds by itself after a moment.
    for _ in 0..60 {
        h.frame(vec![Event::PointerMoved(away)]);
    }
    assert!(!h.w.dock.open && !tabs_shown(&h), "gone again");
    assert!(h.snap.find(ic::CARET_DOWN).is_empty(), "whole, not into the tab: it was left unfolded");
    // Unfolded last, the band itself comes back at the top edge.
    h.frame(vec![Event::PointerMoved(Pos2::new(900.0, 1.0))]);
    h.frame(Vec::new());
    assert!(h.w.dock.open && tabs_shown(&h), "remembered unfolded");
    let tab_h = h.snap.find(ic::CARET_UP).into_iter().next().expect("the band's caret").height();
    assert!(tab_h < 20.0, "a compact band: {tab_h}");
    for _ in 0..60 {
        h.frame(vec![Event::PointerMoved(away)]);
    }
    assert!(!h.w.dock.open, "gone after a moment");
    // Ctrl+Alt+Home unfolds and folds it.
    let home = |pressed| Event::Key { key: egui::Key::Home, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::CTRL | egui::Modifiers::ALT };
    h.frame(vec![home(true)]);
    h.frame(vec![home(false)]);
    assert!(h.w.dock.open, "Ctrl+Alt+Home unfolds it");
    // The band's own caret folds it back into the tab.
    let fold = h.snap.find(ic::CARET_UP).into_iter().next().expect("the band's fold caret");
    h.click_at(fold.center(), PointerButton::Primary);
    assert!(!h.w.dock.open && !tabs_shown(&h), "folded into the tab");
    assert!(!h.snap.find(ic::CARET_DOWN).is_empty(), "the tab is back");
    // Leaving full screen from the band.
    h.frame(vec![Event::PointerMoved(Pos2::new(900.0, 1.0))]);
    let name = h.snap.find("shell").into_iter().next().expect("the tab");
    h.click_at(name.center(), PointerButton::Primary);
    let leave = h.snap.find(ic::ARROWS_IN).into_iter().next().expect("leave full screen in the band");
    h.click_at(leave.center(), PointerButton::Primary);
    assert!(!h.w.full.on, "left full screen");
    h.frame(Vec::new());
    assert!(tabs_shown(&h), "the title bar is back");
    h.w.close_all();
}

/// In full screen a screen pane that has the window to itself fills it to the edges, zoomed or
/// as the window's only pane.
#[test]
#[ignore]
fn full_screen_pictures_fill_the_window() {
    let mut h = Harness::new();
    h.w.add(test_desktop(), Place::Tab, PaneStart::default());
    h.until("connected", |w| live(w, 0));
    let ctx = h.ctx.clone();
    h.w.set_full(&ctx, true);
    let shown_of = |w: &Workspace| match &w.panes[0].state {
        State::Live(r) => r.shown_rect(),
        _ => None,
    };
    h.until("a picture", |w| shown_of(w).is_some());
    for _ in 0..5 {
        h.frame(Vec::new());
    }
    let shown = shown_of(&h.w).expect("a picture");
    assert_eq!(shown, h.w.panes_area, "edge to edge, no bars");
    h.w.close_all();
}

/// "This computer": its terminal turned to the screen goes on to the local agent's picture.
#[test]
#[ignore]
fn a_local_terminal_turns_to_this_computers_screen() {
    let mut h = Harness::new();
    let profile = crate::termwin::local_profiles().into_iter().next().unwrap();
    h.w.add(Launch::Shell { label: "PowerShell".into(), profile, target: Target::Local { capture: "auto".into() }, ssh_cmd: None }, Place::Tab, PaneStart::default());
    h.frame(Vec::new());
    let id = h.w.tabs[0].active;
    h.w.set_mode(id, PaneMode::Screen);
    h.until("this computer's screen", |w| live(w, 0));
    for _ in 0..20 {
        h.frame(Vec::new());
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    assert!(live(&h.w, 0), "still there");
    assert_eq!(mode_of(&h.w, 0), Some(PaneMode::Screen));
    h.w.close_all();
}

/// A first connection that fails asks at once: Reconnect or Close.
#[test]
#[ignore]
fn a_failed_connection_offers_reconnect_or_close() {
    let mut h = Harness::new();
    let target = Target::Ssh {
        host: "nobody@127.0.0.1".into(),
        port: Some(1),
        args: vec!["-o".into(), "ConnectTimeout=2".into()],
        remote: "sshremote --agent".into(),
        capture: "auto".into(),
        password: None,
        unattended: true,
    };
    h.w.add(Launch::Agent { target, label: "nowhere".into(), ssh_cmd: None, mode: PaneMode::Screen }, Place::Tab, PaneStart::default());
    h.until("it gives up", |w| matches!(&w.panes[0].state, State::Down(d) if d.given_up));
    h.frame(Vec::new());
    assert!(!h.snap.find("Reconnect").is_empty(), "Reconnect is offered");
    assert!(!h.snap.find("Close").is_empty(), "Close is offered");
    assert!(h.snap.find("Cancel").is_empty(), "no Cancel once given up");
    h.item("Close");
    assert!(h.w.panes.is_empty(), "Close closes the pane");
}
