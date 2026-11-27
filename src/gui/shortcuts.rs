//! Keys of one's own for a screen's actions (Settings, Shortcuts). Off until turned on, so every
//! key reaches the host as before; then each action has a key to begin with (Ctrl+Alt+Shift and
//! a letter, which hosts leave alone), and any can be changed or taken away. A key is matched
//! where it is on the keyboard, whatever the layout, and taken before the host sees it.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Key, Modifiers};

/// What a key can do in a screen pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    CtrlAltDel,
    FullScreen,
    NextScreen,
    PrevScreen,
    Screenshot,
    Record,
    Chat,
    Call,
    ViewOnly,
    Sound,
    Pause,
    Repaint,
    GameMouse,
    Lock,
}

pub const ALL: [Action; 14] = [
    Action::CtrlAltDel,
    Action::FullScreen,
    Action::NextScreen,
    Action::PrevScreen,
    Action::Screenshot,
    Action::Record,
    Action::Chat,
    Action::Call,
    Action::ViewOnly,
    Action::Sound,
    Action::Pause,
    Action::Repaint,
    Action::GameMouse,
    Action::Lock,
];

impl Action {
    /// Its name in the settings file.
    fn id(self) -> &'static str {
        match self {
            Action::CtrlAltDel => "ctrl_alt_del",
            Action::FullScreen => "full_screen",
            Action::NextScreen => "next_screen",
            Action::PrevScreen => "prev_screen",
            Action::Screenshot => "screenshot",
            Action::Record => "record",
            Action::Chat => "chat",
            Action::Call => "call",
            Action::ViewOnly => "view_only",
            Action::Sound => "sound",
            Action::Pause => "pause",
            Action::Repaint => "repaint",
            Action::GameMouse => "game_mouse",
            Action::Lock => "lock",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::CtrlAltDel => t!("Send Ctrl+Alt+Del"),
            Action::FullScreen => t!("Full screen on or off"),
            Action::NextScreen => t!("The host's next screen"),
            Action::PrevScreen => t!("The host's screen before"),
            Action::Screenshot => t!("Screenshot"),
            Action::Record => t!("Start or stop recording"),
            Action::Chat => t!("Chat open or closed"),
            Action::Call => t!("Call, or hang up"),
            Action::ViewOnly => t!("View only on or off"),
            Action::Sound => t!("Sound on or off"),
            Action::Pause => t!("Pause or resume the picture"),
            Action::Repaint => t!("Repaint the whole screen"),
            Action::GameMouse => t!("Mouse for games on or off"),
            Action::Lock => t!("Lock that computer"),
        }
    }

    /// Its key at first, with Ctrl+Alt+Shift.
    fn first_key(self) -> Option<Key> {
        Some(match self {
            Action::CtrlAltDel => Key::End,
            Action::FullScreen => Key::F,
            Action::NextScreen => Key::ArrowRight,
            Action::PrevScreen => Key::ArrowLeft,
            Action::Screenshot => Key::P,
            Action::Record => Key::R,
            Action::Chat => Key::C,
            Action::ViewOnly => Key::I,
            Action::Sound => Key::S,
            Action::GameMouse => Key::G,
            Action::Lock => Key::L,
            Action::Call | Action::Pause | Action::Repaint => return None,
        })
    }
}

/// Modifiers and a key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The Mac's Command key (the Windows key elsewhere is not seen by the window).
    pub cmd: bool,
    pub key: Key,
}

const PREFIX: Combo = Combo { ctrl: true, alt: true, shift: true, cmd: false, key: Key::A };

impl Combo {
    pub fn of(m: Modifiers, key: Key) -> Combo {
        Combo { ctrl: m.ctrl, alt: m.alt, shift: m.shift, cmd: m.mac_cmd, key }
    }

    /// "Ctrl+Alt+Shift+P", as kept and shown.
    pub fn text(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for (on, name) in [(self.ctrl, "Ctrl"), (self.alt, if cfg!(target_os = "macos") { "Option" } else { "Alt" }), (self.shift, "Shift"), (self.cmd, "Cmd")] {
            if on {
                parts.push(name);
            }
        }
        parts.push(self.key.name());
        parts.join("+")
    }

    pub fn parse(s: &str) -> Option<Combo> {
        let mut c = Combo { ctrl: false, alt: false, shift: false, cmd: false, key: Key::A };
        let mut key = None;
        for part in s.split('+').map(str::trim) {
            match part {
                "Ctrl" => c.ctrl = true,
                "Alt" | "Option" => c.alt = true,
                "Shift" => c.shift = true,
                "Cmd" => c.cmd = true,
                k => key = Key::from_name(k),
            }
        }
        c.key = key?;
        c.held().then_some(c)
    }

    /// With a modifier that typing leaves alone: Shift and a letter types, it is no shortcut.
    pub fn held(&self) -> bool {
        self.ctrl || self.alt || self.cmd
    }

    /// Keys the app keeps for itself in a screen pane, whatever is set here: what is said of one.
    pub fn reserved(&self) -> Option<&'static str> {
        let ca = self.ctrl && self.alt && !self.cmd;
        match self.key {
            Key::Enter if ca && !self.shift => Some(n!("{} is kept for full screen, always")),
            Key::PageUp | Key::PageDown if ca => Some(n!("{} is kept for the tabs and panes")),
            Key::Num1 | Key::Num2 | Key::Num3 | Key::Num4 | Key::Num5 | Key::Num6 | Key::Num7 | Key::Num8 | Key::Num9 if ca && self.shift => Some(n!("{} is kept for the host's screens by number")),
            Key::Delete if ca && !self.shift => Some(n!("{} is kept for this computer's own Ctrl+Alt+Del")),
            _ => None,
        }
    }
}

/// Whether the keys are on, and each action's key (none: taken away).
#[derive(Clone, PartialEq, Debug)]
pub struct Keys {
    pub on: bool,
    pub bound: Vec<(Action, Option<Combo>)>,
}

impl Keys {
    /// As they are to begin with: off, each action with its first key.
    pub fn first() -> Keys {
        Keys { on: false, bound: ALL.iter().map(|&a| (a, a.first_key().map(|key| Combo { key, ..PREFIX }))).collect() }
    }

    pub fn key_of(&self, a: Action) -> Option<Combo> {
        self.bound.iter().find(|(b, _)| *b == a).and_then(|(_, c)| *c)
    }

    /// The action of these modifiers and this key, when the keys are on.
    pub fn find(&self, m: Modifiers, key: Key) -> Option<Action> {
        let c = Combo::of(m, key);
        self.on.then(|| self.bound.iter().find(|(_, b)| *b == Some(c)).map(|(a, _)| *a)).flatten()
    }

    /// `a` on `c` from now on (none: on no key); an action that had `c` before loses it, and is
    /// said.
    pub fn set(&mut self, a: Action, c: Option<Combo>) -> Option<Action> {
        let mut lost = None;
        for (b, k) in self.bound.iter_mut() {
            if *b == a {
                *k = c;
            } else if c.is_some() && *k == c {
                *k = None;
                lost = Some(*b);
            }
        }
        lost
    }
}

/// The keys as kept, read once a process and after each change made here.
pub fn current() -> Arc<Keys> {
    let mut k = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    k.get_or_insert_with(|| Arc::new(read())).clone()
}

static KEYS: Mutex<Option<Arc<Keys>>> = Mutex::new(None);

fn read() -> Keys {
    let mut k = Keys::first();
    k.on = crate::hosts::setting("shortcuts").is_some_and(|v| v == "on");
    for (a, c) in k.bound.iter_mut() {
        if let Some(v) = crate::hosts::setting(&format!("shortcut.{}", a.id())) {
            *c = Combo::parse(&v);
        }
    }
    k
}

/// Keeps `k`, kept only where it differs from the first keys (they may change in a later version).
pub fn save(k: &Keys) {
    let first = Keys::first();
    crate::hosts::set_setting("shortcuts", if k.on { "on" } else { "off" });
    for (a, c) in &k.bound {
        let name = format!("shortcut.{}", a.id());
        if *c == first.key_of(*a) {
            crate::hosts::remove_settings(&format!("{name}="));
        } else {
            crate::hosts::set_setting(&name, &c.map_or_else(|| "none".to_string(), |c| c.text()));
        }
    }
    *KEYS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(k.clone()));
}

/// The settings page: the keys on or off, each action's key, a key taken from the next one pressed.
#[derive(Default)]
pub struct Page {
    /// The action whose new key the next one pressed is.
    taking: Option<Action>,
    said: Option<String>,
}

impl Page {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        use crate::gui::kit;
        let mut keys = (*current()).clone();
        let before = keys.clone();
        // The next key pressed, with a modifier, is the action's (Esc: none taken).
        if let Some(a) = self.taking {
            let pressed = ui.input_mut(|i| {
                let at = i.events.iter().position(|e| matches!(e, egui::Event::Key { pressed: true, repeat: false, .. }))?;
                match i.events.remove(at) {
                    egui::Event::Key { key, physical_key, modifiers, .. } => Some((physical_key.unwrap_or(key), modifiers)),
                    _ => None,
                }
            });
            match pressed {
                Some((Key::Escape, m)) if !Combo::of(m, Key::Escape).held() => self.taking = None,
                Some((key, m)) => {
                    let c = Combo::of(m, key);
                    if !c.held() {
                        self.said = Some(t!("Hold Ctrl, Alt (Option) or Cmd with the key: alone or with Shift it types").into());
                    } else if let Some(what) = c.reserved() {
                        self.said = Some(crate::lang::fill(crate::lang::tr(what), &[&c.text()]));
                    } else {
                        let lost = keys.set(a, Some(c));
                        self.said = lost.map(|l| t!("{} was {}'s: that one has no key now", c.text(), l.label()));
                        self.taking = None;
                    }
                }
                None => {}
            }
        }
        kit::card(ui, |ui| {
            ui.checkbox(&mut keys.on, t!("Use these keys in screens")).on_hover_text(t!("Off: every key goes to the host, as it is pressed"));
            ui.add_space(4.0);
            ui.label(egui::RichText::new(t!("Taken before the host sees them, in the screen being worked in. Click a key to press another for it.")).small().weak());
            ui.add_space(8.0);
            egui::Grid::new("shortcut-keys").num_columns(3).spacing([16.0, 6.0]).show(ui, |ui| {
                for a in ALL {
                    ui.label(a.label());
                    let taking = self.taking == Some(a);
                    let text = if taking { t!("Press the keys\u{2026}").to_string() } else { keys.key_of(a).map_or_else(|| t!("None").to_string(), |c| c.text()) };
                    if ui.add_enabled(keys.on, egui::Button::new(egui::RichText::new(text).monospace()).selected(taking).min_size(egui::vec2(200.0, 0.0))).clicked() {
                        self.taking = if taking { None } else { Some(a) };
                        self.said = None;
                    }
                    if ui.add_enabled(keys.on && keys.key_of(a).is_some(), egui::Button::new(t!("Clear"))).clicked() {
                        keys.set(a, None);
                    }
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            if let Some(s) = &self.said {
                ui.label(egui::RichText::new(s).small().color(crate::gui::warn()));
                ui.add_space(4.0);
            }
            if ui.add_enabled(keys.on && keys.bound != Keys::first().bound, egui::Button::new(t!("Back to the first keys"))).clicked() {
                keys.bound = Keys::first().bound;
                self.taking = None;
            }
        });
        if keys != before {
            save(&keys);
        }
    }

    /// Whether a key pressed now is this page's (it is waiting for one).
    pub fn taking(&self) -> bool {
        self.taking.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_kept_as_text_and_read_back() {
        for a in ALL {
            if let Some(c) = Keys::first().key_of(a) {
                assert_eq!(Combo::parse(&c.text()), Some(c), "{a:?}");
                assert!(c.reserved().is_none(), "{a:?}: {}", c.text());
            }
        }
        let c = Combo { ctrl: true, alt: false, shift: true, cmd: false, key: Key::F5 };
        assert_eq!(Combo::parse("Ctrl+Shift+F5"), Some(c));
        assert_eq!(Combo::parse("Shift+P"), None, "a key that types is no shortcut");
        assert_eq!(Combo::parse("Ctrl+Nothing"), None);
        assert!(Combo::parse("Ctrl+Alt+Enter").unwrap().reserved().is_some());
    }

    #[test]
    fn a_key_is_one_actions_and_only_when_on() {
        let mut k = Keys::first();
        let shot = k.key_of(Action::Screenshot).unwrap();
        let m = Modifiers { ctrl: true, alt: true, shift: true, ..Modifiers::NONE };
        assert_eq!(k.find(m, shot.key), None, "off at first");
        k.on = true;
        assert_eq!(k.find(m, shot.key), Some(Action::Screenshot));
        assert_eq!(k.find(Modifiers { ctrl: true, alt: true, ..Modifiers::NONE }, shot.key), None, "these modifiers exactly");
        // The screenshot's key given to recording: the screenshot has none then.
        assert_eq!(k.set(Action::Record, Some(shot)), Some(Action::Screenshot));
        assert_eq!(k.find(m, shot.key), Some(Action::Record));
        assert_eq!(k.key_of(Action::Screenshot), None);
    }
}
