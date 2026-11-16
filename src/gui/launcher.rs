//! Opening a program in a pane, three ways: a search over what a computer has installed (as the
//! Start menu's search finds them), a command line (as Run takes one), or a file picked from its
//! folders and opened with its program. The same for this computer and for one reached; where the
//! programs and the folders come from is the caller's.

use eframe::egui::{self, Key, Modifiers, RichText, Sense, Vec2};

use crate::gui::ic;
use crate::proto::DirEntry;

/// The drives above a drive's root (Windows), as a folder's path (`files::DRIVES`).
pub(crate) const DRIVES: &str = "::";

/// One of this computer's folders, for the File mode: its path as shown, its entries (folders
/// first), or why it could not be read.
pub(crate) fn list_here(path: &str) -> (String, Vec<DirEntry>, String) {
    #[cfg(feature = "remote")]
    if let crate::proto::ToClient::DirList { path, entries, error } = crate::files::list(path) {
        return (path, entries, error);
    }
    let home = || std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = if path.is_empty() { home() } else if path == DRIVES { "/".into() } else { std::path::PathBuf::from(path) };
    let shown = dir.display().to_string();
    match std::fs::read_dir(&dir) {
        Ok(rd) => {
            let mut entries: Vec<DirEntry> = rd
                .flatten()
                .filter_map(|e| {
                    let m = std::fs::metadata(e.path()).ok()?;
                    Some(DirEntry { name: e.file_name().to_string_lossy().into_owned(), dir: m.is_dir(), size: m.len(), mtime: 0 })
                })
                .collect();
            entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
            (shown, entries, String::new())
        }
        Err(e) => (shown, Vec::new(), e.to_string()),
    }
}

/// The three ways.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Mode {
    #[default]
    Search,
    Command,
    File,
}

impl Mode {
    pub(crate) const ALL: [Mode; 3] = [Mode::Search, Mode::Command, Mode::File];

    /// As a button that opens it.
    pub(crate) fn button(self) -> String {
        match self {
            Mode::Search => format!("{}  Search programs", ic::SEARCH),
            Mode::Command => format!("{}  Run a command", ic::TERMINAL),
            Mode::File => format!("{}  Open a file", ic::FOLDER_OPEN),
        }
    }
}

/// A program to open: how it is named (`apps::APP`, `COMMAND` or `OPEN`), that name, and what its
/// pane is called.
pub(crate) type Pick = (u8, String, String);

/// What the launcher needs of its caller this frame.
pub(crate) enum Ask {
    Open(Pick),
    /// A folder's entries, given back with `set_folder` (an empty path: the home folder).
    List(String),
}

/// One of the programs to choose from, as a computer lists them.
pub(crate) struct Prog<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub detail: &'a str,
}

/// The computer's programs as far as they are known.
pub(crate) enum Programs<'a> {
    Coming,
    Listed(Vec<Prog<'a>>),
}

struct Folder {
    path: String,
    entries: Vec<DirEntry>,
    error: String,
}

/// How tall the part under the mode's field is, the same in each mode: a menu keeps the size it
/// opened with.

#[derive(Default)]
pub(crate) struct Launcher {
    pub(crate) mode: Mode,
    find: String,
    /// The result chosen by the arrow keys.
    sel: usize,
    line: String,
    /// Command lines and programs opened from here, the last first; kept in Settings for this
    /// computer's (`keep`), for the session for another's.
    commands: Vec<String>,
    recent: Vec<String>,
    keep: bool,
    folder: Option<Folder>,
    asked: bool,
    path: String,
    /// A file of the folder, picked to open.
    file: Option<String>,
    /// A path typed and listed: opened with its program when it is no folder (a file's path).
    typed: Option<String>,
    focus: bool,
}

const RECENT: &str = "programs_recent";
const COMMANDS: &str = "programs_commands";

impl Launcher {
    /// For this computer: what was opened and run is remembered across sessions.
    pub(crate) fn here() -> Launcher {
        let read = |k: &str| crate::hosts::setting(k).map(|v| v.split('\u{1f}').filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default();
        Launcher { keep: true, recent: read(RECENT), commands: read(COMMANDS), ..Launcher::default() }
    }

    /// Opened in `mode`, its field taking the keyboard; a search starts afresh, as the Start
    /// menu's does.
    pub(crate) fn start(&mut self, mode: Mode) {
        self.mode = mode;
        self.focus = true;
        self.sel = 0;
        self.find.clear();
    }

    /// A folder listed for `Ask::List`.
    pub(crate) fn set_folder(&mut self, path: String, entries: Vec<DirEntry>, error: String) {
        if error.is_empty() {
            self.typed = None;
        }
        self.path = if path == DRIVES { String::new() } else { path.clone() };
        self.file = None;
        self.folder = Some(Folder { path, entries, error });
    }

    /// The mode chosen and its part, as wide as the page it is laid in (the page's buttons choose
    /// the mode); what it asks for, if anything.
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, programs: Programs<'_>) -> Option<Ask> {
        ui.set_width(ui.available_width());
        let ask = match self.mode {
            Mode::Search => self.search(ui, programs),
            Mode::Command => self.command(ui),
            Mode::File => self.files(ui),
        };
        if let Some(Ask::Open((kind, text, _))) = &ask {
            self.remember(*kind, text);
        }
        ask
    }

    /// How tall the part under the field is: all the room left in the page.
    fn room(&self, ui: &egui::Ui) -> f32 {
        (ui.available_height() - 1.0).max(140.0)
    }

    fn remember(&mut self, kind: u8, text: &str) {
        let (list, key) = if kind == crate::apps::APP { (&mut self.recent, RECENT) } else { (&mut self.commands, COMMANDS) };
        list.retain(|t| t != text);
        list.insert(0, text.to_string());
        list.truncate(12);
        if self.keep {
            crate::hosts::set_setting(key, &list.join("\u{1f}"));
        }
    }

    /// A field that takes the keyboard when the mode opens.
    fn field(&mut self, ui: &mut egui::Ui, which: &'static str, hint: String) -> (egui::Response, bool) {
        let id = ui.make_persistent_id(which);
        let text = match which {
            "launcher-find" => &mut self.find,
            "launcher-line" => &mut self.line,
            _ => &mut self.path,
        };
        // What is there already is chosen, so typing replaces it (as Run keeps the last command);
        // focused first, or the field lets go of a choice it does not have the keyboard for.
        // Not in the frame of the click that opened it, which would take the keyboard back.
        let clicking = ui.input(|i| i.pointer.any_pressed() || i.pointer.any_released());
        if self.focus && !clicking {
            self.focus = false;
            ui.memory_mut(|m| m.request_focus(id));
            let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()));
            state.cursor.set_char_range(Some(all));
            state.store(ui.ctx(), id);
        }
        let r = ui.add(egui::TextEdit::singleline(text).id(id).hint_text(hint).desired_width(f32::INFINITY));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        // Enter that opens nothing (a name not found, a folder listed) leaves the keyboard here.
        if enter {
            r.request_focus();
        }
        (r, enter)
    }

    /// As the Start menu's search: the best match first, then the rest, each found by the start
    /// of its name or of a word in it, its initials, or its program file; before anything is
    /// typed, the last opened and then every program.
    fn search(&mut self, ui: &mut egui::Ui, programs: Programs<'_>) -> Option<Ask> {
        let id = ui.make_persistent_id("launcher-find");
        let keys = ui.memory(|m| m.has_focus(id));
        let (down, up) = if keys { ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::ArrowUp))) } else { (false, false) };
        let before = self.find.clone();
        let (_, enter) = self.field(ui, "launcher-find", format!("{}  Type a program's name", ic::SEARCH));
        if self.find != before {
            self.sel = 0;
        }
        let h = self.room(ui);
        let list = match programs {
            Programs::Coming => return body(ui, h, |ui| note(ui, "Looking at what is installed\u{2026}")),
            Programs::Listed(l) => l,
        };
        let q = self.find.trim();
        // What shows, in order, with a heading before some.
        let mut rows: Vec<(Option<&str>, &Prog)> = Vec::new();
        if q.is_empty() {
            for (i, p) in self.recent.iter().filter_map(|r| list.iter().find(|p| p.id == r.as_str())).take(5).enumerate() {
                rows.push(((i == 0).then_some("Recent"), p));
            }
            let mut all: Vec<&Prog> = list.iter().collect();
            all.sort_by_cached_key(|p| p.name.to_lowercase());
            for (i, p) in all.into_iter().enumerate() {
                rows.push(((i == 0).then_some("All programs"), p));
            }
        } else {
            let mut found: Vec<(u8, usize, String, &Prog)> = list
                .iter()
                .filter_map(|p| {
                    let used = self.recent.iter().position(|r| r == p.id).unwrap_or(usize::MAX);
                    rank(q, p.name, p.id, p.detail).map(|r| (r, used, p.name.to_lowercase(), p))
                })
                .collect();
            found.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
            for (i, (_, _, _, p)) in found.into_iter().enumerate() {
                rows.push((match i {
                    0 => Some("Best match"),
                    1 => Some("Programs"),
                    _ => None,
                }, p));
            }
        }
        if rows.is_empty() {
            return body(ui, h, |ui| note(ui, if list.is_empty() { "No programs were found there" } else { "No program by that name: try Command to run it by its file" }));
        }
        let last = rows.len() - 1;
        if down {
            self.sel = (self.sel + 1).min(last);
        }
        if up {
            self.sel = self.sel.saturating_sub(1);
        }
        self.sel = self.sel.min(last);
        let mut pick = enter.then(|| rows[self.sel].1);
        let moved = down || up;
        body(ui, h, |ui| {
            egui::ScrollArea::vertical().max_height(h).auto_shrink([false, false]).show(ui, |ui| {
                for (i, (heading, p)) in rows.iter().enumerate() {
                    if let Some(h) = heading {
                        ui.add_space(if i == 0 { 0.0 } else { 6.0 });
                        ui.label(RichText::new(*h).small().weak());
                    }
                    let best = !q.is_empty() && i == 0;
                    let r = row(ui, ic::APP_WINDOW, p.name, if best { if p.detail.is_empty() { "Program" } else { p.detail } } else { "" }, i == self.sel, best);
                    let r = if p.detail.is_empty() || best { r } else { r.on_hover_text(p.detail) };
                    if moved && i == self.sel {
                        r.scroll_to_me(None);
                    }
                    if r.clicked() {
                        pick = Some(*p);
                    }
                }
            });
        });
        pick.map(|p| Ask::Open((crate::apps::APP, p.id.to_string(), p.name.to_string())))
    }

    /// As Run: a program with what it takes, or a folder, a file or an address to open with its
    /// program; the last ones run again with a click.
    fn command(&mut self, ui: &mut egui::Ui) -> Option<Ask> {
        let mut pick = None;
        ui.horizontal(|ui| {
            let run = egui::Button::new(format!("{}  Run", ic::PLAY));
            let w = ui.available_width() - 74.0;
            let (_, enter) = ui.allocate_ui(Vec2::new(w, ui.spacing().interact_size.y), |ui| self.field(ui, "launcher-line", "A program, a folder, a file or an address".into())).inner;
            let typed = self.line.trim().to_string();
            if (ui.add_enabled(!typed.is_empty(), run).clicked() || enter) && !typed.is_empty() {
                pick = Some(command_pick(&typed));
            }
        });
        let commands = self.commands.clone();
        let h = self.room(ui);
        body(ui, h, |ui| {
            note(ui, "It opens in a pane of its own, its window alone: \"notepad\", \"gedit notes.txt\", a folder, ~/report.pdf, https://\u{2026}");
            if !commands.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("Run before").small().weak());
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for c in &commands {
                        let r = row(ui, ic::CLOCK_COUNTER_CLOCKWISE, c, "", false, false).on_hover_text("Run it again");
                        if r.clicked() {
                            pick = Some(command_pick(c));
                        }
                    }
                });
            }
        });
        pick.map(Ask::Open)
    }

    /// A folder at a time, from the home folder: a folder opens with a click, a file with a double
    /// click (or picked, then Open); the folder itself opens in the computer's file manager.
    fn files(&mut self, ui: &mut egui::Ui) -> Option<Ask> {
        // The home folder first; the part is laid out meanwhile (a menu takes the size it has).
        let mut ask = None;
        if !self.asked {
            self.asked = true;
            ask = Some(Ask::List(String::new()));
        }
        let at = self.folder.as_ref().map(|f| f.path.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            let up = parent(&at);
            if ui.add_enabled(!at.is_empty() && up != at, egui::Button::new(ic::ARROW_UP)).on_hover_text("The folder it is in").clicked() {
                ask = Some(Ask::List(up));
            }
            if ui.button(ic::HOUSE).on_hover_text("The home folder").clicked() {
                ask = Some(Ask::List(String::new()));
            }
            let hint = if at == DRIVES { "The drives".to_string() } else { "A folder, or a file to open".to_string() };
            // The keyboard once the folder's path is in the field, to be replaced by typing.
            let wait = self.folder.is_none() && std::mem::take(&mut self.focus);
            let (_, enter) = self.field(ui, "launcher-path", hint);
            self.focus |= wait;
            if enter && !self.path.trim().is_empty() {
                self.typed = Some(self.path.trim().to_string());
                ask = Some(Ask::List(self.path.trim().to_string()));
            }
        });
        if ask.is_none()
            && self.folder.as_ref().is_some_and(|f| !f.error.is_empty())
            && let Some(path) = self.typed.take()
        {
            let name = path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or(&path).to_string();
            return Some(Ask::Open((crate::apps::OPEN, path, name)));
        }
        let mut open: Option<String> = None;
        let h = self.room(ui);
        body(ui, h, |ui| {
            let Some(f) = &self.folder else {
                note(ui, "Reading the folder\u{2026}");
                return;
            };
            if !f.error.is_empty() {
                note(ui, &format!("It could not be read: {}", f.error));
                return;
            }
            let shown: Vec<&DirEntry> = f.entries.iter().filter(|e| !e.name.starts_with('.') && !(e.dir && LEGACY.contains(&e.name.as_str()))).collect();
            if shown.is_empty() {
                note(ui, "Nothing in this folder");
            }
            egui::ScrollArea::vertical().max_height(h - 40.0).auto_shrink([false, false]).show(ui, |ui| {
                for e in shown {
                    let glyph = if f.path == DRIVES { ic::HARD_DRIVE } else if e.dir { ic::FOLDER } else { ic::FILE };
                    let size = if e.dir { String::new() } else { crate::sys::human(e.size) };
                    let on = self.file.as_deref() == Some(e.name.as_str());
                    let r = row(ui, glyph, &e.name, &size, on, false);
                    if e.dir && r.clicked() {
                        ask = Some(Ask::List(join(&f.path, &e.name)));
                    } else if !e.dir && r.double_clicked() {
                        open = Some(join(&f.path, &e.name));
                    } else if !e.dir && r.clicked() {
                        self.file = Some(e.name.clone());
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if f.path != DRIVES && ui.button(format!("{}  Open this folder", ic::FOLDER_OPEN)).on_hover_text("In the computer's file manager, in a pane").clicked() {
                    open = Some(f.path.clone());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let picked = self.file.clone();
                    if ui.add_enabled(picked.is_some(), egui::Button::new("Open")).on_hover_text("With the program that opens it").clicked()
                        && let Some(name) = picked
                    {
                        open = Some(join(&f.path, &name));
                    }
                });
            });
            if self.file.is_some() && ui.input(|i| i.key_pressed(Key::Enter)) && !ui.memory(|m| m.focused().is_some()) {
                open = self.file.as_ref().map(|n| join(&f.path, n));
            }
        });
        if let Some(path) = open {
            let name = path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or(&path).to_string();
            return Some(Ask::Open((crate::apps::OPEN, path, name)));
        }
        ask
    }
}

/// Windows' old names for folders, kept as links that cannot be opened: hidden as Explorer hides them.
const LEGACY: [&str; 13] = [
    "Application Data", "Cookies", "Local Settings", "My Documents", "NetHood", "PrintHood", "Recent", "SendTo", "Start Menu", "Templates", "My Music", "My Pictures", "My Videos",
];

/// A command line as Run takes it: a path or an address opens with its program, the rest runs.
fn command_pick(typed: &str) -> Pick {
    if looks_like_file(typed) {
        let path = unquoted(typed);
        let name = path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(path).to_string();
        (crate::apps::OPEN, path.to_string(), name)
    } else {
        (crate::apps::COMMAND, typed.to_string(), crate::apps::program_of(typed))
    }
}

/// A file or an address rather than a command: a path from the root or home, a drive, or a scheme.
pub(crate) fn looks_like_file(s: &str) -> bool {
    let p = unquoted(s);
    (p.len() < s.len() || !p.contains(char::is_whitespace)) && (p.starts_with('/') || p.starts_with('~') || p.contains("://") || p.get(1..3) == Some(":\\") || p.get(1..3) == Some(":/"))
}

/// A path typed in quotes (it has spaces), without them.
fn unquoted(s: &str) -> &str {
    s.strip_prefix('"').and_then(|r| r.strip_suffix('"')).filter(|r| !r.contains('"')).unwrap_or(s)
}

/// The part under the field, `h` tall in every mode.
fn body<R: Default>(ui: &mut egui::Ui, h: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let mut inner = ui.new_child(egui::UiBuilder::new().layout(egui::Layout::top_down(egui::Align::Min)));
    inner.spacing_mut().item_spacing.y = 2.0;
    inner.set_min_height(h);
    inner.set_max_height(h);
    let out = add(&mut inner);
    let used = inner.min_rect();
    ui.advance_cursor_after_rect(used);
    out
}

fn note(ui: &mut egui::Ui, text: &str) -> Option<Ask> {
    ui.add(egui::Label::new(RichText::new(text).weak()).wrap());
    None
}

/// A line to click: a glyph, the name, and a word on the right (a size) or under it (the best match).
fn row(ui: &mut egui::Ui, glyph: &str, name: &str, aside: &str, on: bool, big: bool) -> egui::Response {
    let h = if big { 48.0 } else { 28.0 };
    let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
    let v = ui.visuals();
    if on || r.hovered() || big {
        // A tint near the background, as menus show the row chosen.
        let tint = |a: u8| if v.dark_mode { egui::Color32::from_white_alpha(a) } else { egui::Color32::from_black_alpha(a) };
        let fill = if on { tint(26) } else if r.hovered() { tint(16) } else { tint(8) };
        ui.painter().rect_filled(rect, 6.0, fill);
    }
    let text = v.text_color();
    let weak = v.weak_text_color();
    let font = egui::FontId::proportional(if big { 15.0 } else { 13.5 });
    let left = rect.left() + 8.0;
    ui.painter().text(egui::pos2(left, rect.center().y), egui::Align2::LEFT_CENTER, glyph, egui::FontId::proportional(if big { 20.0 } else { 15.0 }), weak);
    let x = left + if big { 30.0 } else { 24.0 };
    let room = rect.right() - x - 8.0;
    let clip = rect.with_min_x(x).with_max_x(rect.right() - if big || aside.is_empty() { 8.0 } else { 70.0 });
    if big {
        ui.painter().with_clip_rect(clip).text(egui::pos2(x, rect.center().y - 9.0), egui::Align2::LEFT_CENTER, name, font, text);
        ui.painter().with_clip_rect(clip).text(egui::pos2(x, rect.center().y + 10.0), egui::Align2::LEFT_CENTER, aside, egui::FontId::proportional(12.0), weak);
    } else {
        ui.painter().with_clip_rect(clip).text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, name, font, text);
        if !aside.is_empty() && room > 80.0 {
            ui.painter().text(egui::pos2(rect.right() - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, aside, egui::FontId::proportional(12.0), weak);
        }
    }
    r
}

/// How well a program answers what is typed, as the Start menu's search ranks it (lower first):
/// its name, the start of its name, the start of a word of it for each word typed, its initials,
/// its program file, anywhere in the name, the line about it. None: not found.
pub(crate) fn rank(q: &str, name: &str, id: &str, detail: &str) -> Option<u8> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return Some(9);
    }
    let n = name.to_lowercase();
    let words: Vec<&str> = n.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if n == q {
        return Some(0);
    }
    if n.starts_with(&q) {
        return Some(1);
    }
    if q.split_whitespace().all(|t| words.iter().any(|w| w.starts_with(t))) {
        return Some(2);
    }
    let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
    if q.chars().count() >= 2 && !q.contains(' ') && initials.starts_with(&q) {
        return Some(3);
    }
    // "cmd" for Command Prompt, "calculator" for org.gnome.Calculator.desktop.
    let file = id.rsplit(['\\', '/']).next().unwrap_or(id).to_lowercase();
    let file = file.split('!').next().unwrap_or(&file);
    let file = file.trim_end_matches(".exe").trim_end_matches(".desktop").trim_end_matches(".lnk");
    if file.starts_with(&q) || file.split(['.', '-', '_', ' ']).any(|p| p.starts_with(&q)) {
        return Some(4);
    }
    if n.contains(&q) {
        return Some(5);
    }
    if detail.to_lowercase().contains(&q) {
        return Some(6);
    }
    None
}

/// The folder a path is in; the drives above a drive's root (Windows), the root above itself.
pub(crate) fn parent(p: &str) -> String {
    if p == DRIVES || p.is_empty() {
        return p.to_string();
    }
    let t = p.trim_end_matches(['/', '\\']);
    if t.is_empty() {
        return "/".into();
    }
    if t.len() == 2 && t.ends_with(':') {
        return DRIVES.into();
    }
    match t.rfind(['/', '\\']) {
        Some(0) => "/".into(),
        Some(i) if i == 2 && t.as_bytes()[1] == b':' => t[..=i].to_string(),
        Some(i) => t[..i].to_string(),
        None => DRIVES.into(),
    }
}

/// `name` in the folder `dir`, with the separator the folder's path has.
pub(crate) fn join(dir: &str, name: &str) -> String {
    if dir == DRIVES {
        return name.to_string();
    }
    let windows = dir.contains('\\') || dir.as_bytes().get(1) == Some(&b':');
    if dir.ends_with(['/', '\\']) {
        format!("{dir}{name}")
    } else {
        format!("{dir}{}{name}", if windows { '\\' } else { '/' })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_are_found_as_the_start_menu_finds_them() {
        assert_eq!(rank("calculator", "Calculator", "", ""), Some(0));
        assert_eq!(rank("calc", "Calculator", "", ""), Some(1));
        assert_eq!(rank("code", "Visual Studio Code", "", ""), Some(2));
        assert_eq!(rank("vis co", "Visual Studio Code", "", ""), Some(2));
        assert_eq!(rank("vsc", "Visual Studio Code", "", ""), Some(3));
        assert_eq!(rank("cmd", "Command Prompt", r"shell:AppsFolder\{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\cmd.exe", ""), Some(4));
        assert_eq!(rank("calculator", "Rechner", "org.gnome.Calculator.desktop", ""), Some(4));
        assert_eq!(rank("ulat", "Calculator", "", ""), Some(5));
        assert_eq!(rank("sums", "Calculator", "", "Sums beside your work"), Some(6));
        assert_eq!(rank("zzz", "Calculator", "x.desktop", "Sums"), None);
        // Ranked: the name's start before a word's start before anywhere.
        let mut names = ["Notepad++", "Sticky Notes", "Notepad", "Keynote"];
        names.sort_by_key(|n| (rank("note", n, "", ""), n.to_lowercase()));
        assert_eq!(names, ["Notepad", "Notepad++", "Sticky Notes", "Keynote"]);
    }

    #[test]
    fn folders_go_up_and_in_on_either_system() {
        assert_eq!(parent("/home/a/Documents"), "/home/a");
        assert_eq!(parent("/home"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(parent(r"C:\Users\a"), r"C:\Users");
        assert_eq!(parent(r"C:\Users"), r"C:\");
        assert_eq!(parent(r"C:\"), DRIVES);
        assert_eq!(parent(DRIVES), DRIVES);
        assert_eq!(join("/home/a", "x.txt"), "/home/a/x.txt");
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join(r"C:\Users", "a"), r"C:\Users\a");
        assert_eq!(join(r"C:\", "Users"), r"C:\Users");
        assert_eq!(join(DRIVES, r"D:\"), r"D:\");
    }

    #[test]
    fn a_path_or_an_address_is_opened_and_the_rest_is_run() {
        assert_eq!(command_pick("gedit notes.txt"), (crate::apps::COMMAND, "gedit notes.txt".into(), "gedit".into()));
        assert_eq!(command_pick("~/report.pdf").0, crate::apps::OPEN);
        assert_eq!(command_pick(r"C:\Users").2, "Users");
        assert_eq!(command_pick("https://example.org").0, crate::apps::OPEN);
        assert_eq!(command_pick("/usr/bin/gedit /tmp/a").0, crate::apps::COMMAND);
        assert_eq!(command_pick(r#""C:\Program Files""#), (crate::apps::OPEN, r"C:\Program Files".into(), "Program Files".into()));
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;

    fn frame(ctx: &egui::Context, l: &mut Launcher, events: Vec<egui::Event>) -> Option<Ask> {
        let mut ask = None;
        ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| ask = l.ui(ui, Programs::Coming)).textures_delta.clear();
        ask
    }

    #[test]
    fn a_field_opened_again_has_its_text_chosen_so_typing_replaces_it() {
        let ctx = egui::Context::default();
        let mut l = Launcher { line: "notepad".into(), ..Launcher::default() };
        l.start(Mode::Command);
        for _ in 0..2 {
            frame(&ctx, &mut l, vec![]);
        }
        frame(&ctx, &mut l, vec![egui::Event::Text("calc".into())]);
        assert_eq!(l.line, "calc");
        // A folder's path, the first time the field shows.
        let mut l = Launcher::default();
        l.start(Mode::File);
        assert!(matches!(frame(&ctx, &mut l, vec![]), Some(Ask::List(p)) if p.is_empty()));
        l.set_folder("/home/a".into(), Vec::new(), String::new());
        for _ in 0..2 {
            frame(&ctx, &mut l, vec![]);
        }
        frame(&ctx, &mut l, vec![egui::Event::Text("/tmp".into())]);
        assert_eq!(l.path, "/tmp");
    }
}
