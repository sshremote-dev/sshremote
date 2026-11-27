//! The file transfer window: this computer on the left, the remote host on the right,
//! with upload and download between them, folder operations and a transfer list.

use std::collections::BTreeSet;
use std::path::PathBuf;

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Key, Rect, RichText, Sense, Vec2};

use crate::files::DRIVES;
use crate::gui::{ACCENT, human, ic};
use crate::proto::{DirEntry, FileOp, ToAgent, ToClient};

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Name,
    Size,
    Modified,
}

/// One side of the window.
pub struct Pane {
    pub path: String,
    typed: String,
    entries: Vec<DirEntry>,
    error: String,
    sel: BTreeSet<usize>,
    anchor: Option<usize>,
    sort: Sort,
    ascending: bool,
    hidden: bool,
    loading: bool,
}

/// Files dragged from one pane to the other.
struct Dragged {
    remote: bool,
    paths: Vec<String>,
}

enum Dialog {
    NewFolder { remote: bool, name: String },
    Rename { remote: bool, path: String, name: String },
    Delete { remote: bool, paths: Vec<String> },
}

/// What the window asks the session to do.
pub enum Cmd {
    Send(ToAgent),
    /// Local files or folders to send into a remote folder.
    Upload {
        paths: Vec<PathBuf>,
        to: String,
    },
    /// Remote files or folders to fetch into a local folder.
    Download {
        paths: Vec<String>,
        to: PathBuf,
    },
    /// Stops a transfer: (upload?, id).
    Cancel(bool, u32),
}

/// A row of the transfer list.
pub struct Transfer {
    pub upload: bool,
    pub id: u32,
    pub name: String,
    pub done: u64,
    pub size: u64,
}

impl Pane {
    fn new() -> Self {
        Pane {
            path: String::new(),
            typed: String::new(),
            entries: Vec::new(),
            error: String::new(),
            sel: BTreeSet::new(),
            anchor: None,
            sort: Sort::Name,
            ascending: true,
            hidden: false,
            loading: true,
        }
    }

    fn child(&self, name: &str) -> String {
        crate::files::path_child(&self.path, name)
    }

    fn parent(&self) -> String {
        crate::files::path_parent(&self.path)
    }

    fn shown_path(path: &str) -> String {
        if path == DRIVES { t!("Drives").into() } else { path.to_string() }
    }

    pub fn set_list(&mut self, path: String, mut entries: Vec<DirEntry>, error: String) {
        let same = path == self.path;
        let keep: Vec<String> = if same { self.selected().map(|e| e.name.clone()).collect() } else { Vec::new() };
        sort(&mut entries, self.sort, self.ascending);
        self.typed = Self::shown_path(&path);
        (self.path, self.entries, self.error, self.loading) = (path, entries, error, false);
        self.sel = self.entries.iter().enumerate().filter(|(_, e)| keep.contains(&e.name)).map(|(i, _)| i).collect();
        self.anchor = None;
    }

    fn selected(&self) -> impl Iterator<Item = &DirEntry> {
        self.sel.iter().filter_map(|&i| self.entries.get(i))
    }

    /// The line under the list: the one selected item's name, size and date, or how many are
    /// selected and their size, or what the folder holds.
    fn summary(&self) -> String {
        if self.loading {
            return t!("Loading...").to_string();
        }
        let shown = self.visible();
        let picked: Vec<&DirEntry> = self.selected().collect();
        match picked.as_slice() {
            [] => {
                let dirs = shown.iter().filter(|&&i| self.entries[i].dir).count();
                let files = shown.len() - dirs;
                let size: u64 = shown.iter().map(|&i| &self.entries[i]).filter(|e| !e.dir).map(|e| e.size).sum();
                format!("{}, {} ({})", folders(dirs), files_count(files), human(size))
            }
            [e] => {
                let what = if e.dir { t!("folder").to_string() } else { human(e.size) };
                let when = crate::files::local_time(e.mtime);
                if when.is_empty() {
                    format!("{}  \u{b7}  {what}", e.name)
                } else {
                    format!("{}  \u{b7}  {what}  \u{b7}  {}", e.name, t!("modified {}", when))
                }
            }
            many => {
                let size: u64 = many.iter().filter(|e| !e.dir).map(|e| e.size).sum();
                let dirs = many.iter().filter(|e| e.dir).count();
                let folders = if dirs > 0 { format!(", {}", folders(dirs)) } else { String::new() };
                t!("{} of {} selected ({}{})", many.len(), shown.len(), human(size), folders)
            }
        }
    }

    fn selected_paths(&self) -> Vec<String> {
        self.selected().map(|e| self.child(&e.name)).collect()
    }

    fn visible(&self) -> Vec<usize> {
        (0..self.entries.len()).filter(|&i| self.hidden || !self.entries[i].name.starts_with('.')).collect()
    }

    fn resort(&mut self) {
        let names: Vec<String> = self.selected().map(|e| e.name.clone()).collect();
        sort(&mut self.entries, self.sort, self.ascending);
        self.sel = self.entries.iter().enumerate().filter(|(_, e)| names.contains(&e.name)).map(|(i, _)| i).collect();
    }
}

fn sort(entries: &mut [DirEntry], by: Sort, ascending: bool) {
    entries.sort_by(|a, b| {
        let o = match by {
            Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            Sort::Size => a.size.cmp(&b.size),
            Sort::Modified => a.mtime.cmp(&b.mtime),
        };
        b.dir.cmp(&a.dir).then(if ascending { o } else { o.reverse() })
    });
}

/// Files read as text (notes, logs, configuration, code), shown with lines in their icon.
fn is_text(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(
        ext.as_str(),
        "txt" | "md" | "log" | "csv" | "json" | "yaml" | "yml" | "toml" | "ini" | "conf" | "cfg" | "xml" | "html" | "css"
            | "rs" | "py" | "js" | "ts" | "go" | "c" | "h" | "cpp" | "java" | "sh" | "ps1" | "bat" | "sql"
    ) || name.starts_with('.') && !name.contains('/')
}

/// "1 folder", "3 folders".
fn folders(n: usize) -> String {
    if n == 1 { t!("1 folder").into() } else { t!("{} folders", n) }
}

/// "1 file", "3 files".
fn files_count(n: usize) -> String {
    if n == 1 { t!("1 file").into() } else { t!("{} files", n) }
}

pub struct FileWindow {
    /// Whether it takes keys and dropped files: a pane beside others only while it is active.
    pub active: bool,
    pub local: Pane,
    pub remote: Pane,
    dialog: Option<Dialog>,
    /// The pane keyboard shortcuts act on.
    focus_remote: bool,
    status: String,
    /// Bytes moved by the transfers when last measured, and how fast (bytes a second).
    moved: Option<(std::time::Instant, u64)>,
    rate: f64,
}

impl FileWindow {
    /// Opens on the local home folder; the remote listing is requested by the caller.
    pub fn new() -> Self {
        let mut w = FileWindow {
            active: true,
            local: Pane::new(),
            remote: Pane::new(),
            dialog: None,
            focus_remote: true,
            status: String::new(),
            moved: None,
            rate: 0.0,
        };
        w.list_local(String::new());
        w
    }

    fn list_local(&mut self, path: String) {
        if let ToClient::DirList { path, entries, error } = crate::files::list(&path) {
            self.local.set_list(path, entries, error);
        }
    }

    pub fn refresh_local(&mut self) {
        let p = self.local.path.clone();
        self.list_local(p);
    }

    /// Asks for the remote folder again, after a change there.
    pub fn refresh_remote(&self) -> Cmd {
        Cmd::Send(ToAgent::ListDir { path: self.remote.path.clone() })
    }

    pub fn op_done(&mut self, ok: bool, text: &str) -> Cmd {
        self.status = if ok { crate::lang::tr_msg(text).into_owned() } else { t!("Failed: {}", crate::lang::tr_msg(text)) };
        self.refresh_remote()
    }

    fn pane(&mut self, remote: bool) -> &mut Pane {
        if remote { &mut self.remote } else { &mut self.local }
    }

    /// Opens a folder on either side.
    fn go(&mut self, remote: bool, path: String, out: &mut Vec<Cmd>) {
        if remote {
            self.remote.loading = true;
            out.push(Cmd::Send(ToAgent::ListDir { path }));
        } else {
            self.list_local(path);
        }
    }

    fn transfer(&self, from_remote: bool, paths: Vec<String>, out: &mut Vec<Cmd>) {
        if paths.is_empty() {
            return;
        }
        if from_remote {
            out.push(Cmd::Download { paths, to: PathBuf::from(&self.local.path) });
        } else {
            out.push(Cmd::Upload {
                paths: paths.into_iter().map(PathBuf::from).collect(),
                to: self.remote.path.clone(),
            });
        }
    }

    fn run_op(&mut self, remote: bool, op: FileOp, path: String, arg: String, out: &mut Vec<Cmd>) {
        if remote {
            out.push(Cmd::Send(ToAgent::FileOp { op, path, arg }));
        } else {
            if let ToClient::OpDone { ok, text } = crate::files::op(op, &path, &arg) {
                self.status = if ok { crate::lang::tr_msg(&text).into_owned() } else { t!("Failed: {}", crate::lang::tr_msg(&text)) };
            }
            self.refresh_local();
        }
    }

    /// The file manager drawn in a pane; returns what the session should do.
    pub fn embedded(&mut self, ui: &mut egui::Ui, host: &str, transfers: &[Transfer]) -> Vec<Cmd> {
        let mut out = Vec::new();
        self.body(ui, host, transfers, &mut out);
        out
    }

    fn body(&mut self, ui: &mut egui::Ui, host: &str, transfers: &[Transfer], out: &mut Vec<Cmd>) {
        // Files dropped from the desktop go to the remote folder shown.
        let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        if !dropped.is_empty() && self.active {
            out.push(Cmd::Upload { paths: dropped, to: self.remote.path.clone() });
        }
        egui::Panel::bottom("transfers").show(ui, |ui| {
            ui.add_space(4.0);
            if transfers.is_empty() {
                (self.moved, self.rate) = (None, 0.0);
                let hint = t!("Drag files between the two sides, or drop files from the desktop on this window to upload.");
                ui.label(RichText::new(if self.status.is_empty() { hint } else { &self.status }).weak());
            } else {
                // All of them at once: how far, how fast, how long still.
                let done: u64 = transfers.iter().map(|t| t.done).sum();
                let size: u64 = transfers.iter().map(|t| t.size).sum();
                let now = std::time::Instant::now();
                match self.moved {
                    Some((at, before)) if now.duration_since(at).as_secs_f64() >= 0.5 => {
                        let r = done.saturating_sub(before) as f64 / now.duration_since(at).as_secs_f64();
                        self.rate = if self.rate == 0.0 { r } else { self.rate * 0.6 + r * 0.4 };
                        self.moved = Some((now, done));
                    }
                    Some(_) => {}
                    None => self.moved = Some((now, done)),
                }
                let left = size.saturating_sub(done);
                let eta = if self.rate > 1.0 {
                    let s = (left as f64 / self.rate).round() as u64;
                    if s >= 60 { t!(", about {} min {} s left", s / 60, s % 60) } else { t!(", about {} s left", s) }
                } else {
                    String::new()
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(t!("Transfers ({})", transfers.len())).strong());
                    let frac = if size == 0 { 1.0 } else { done as f32 / size as f32 };
                    let speed = if self.rate > 1.0 { format!("  \u{b7}  {}/s{eta}", human(self.rate as u64)) } else { String::new() };
                    ui.add(egui::ProgressBar::new(frac).desired_width(ui.available_width()).text(format!(
                        "{}{speed}",
                        t!("{} of {}  ({}%)", human(done), human(size), format!("{:.0}", frac * 100.0))
                    )));
                });
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                    for t in transfers {
                        ui.horizontal(|ui| {
                            let arrow = if t.upload { ic::UPLOAD } else { ic::DOWNLOAD };
                            ui.label(RichText::new(arrow).color(ACCENT));
                            ui.add_sized([280.0, 18.0], egui::Label::new(&t.name).truncate());
                            let frac = if t.size == 0 { 1.0 } else { t.done as f32 / t.size as f32 };
                            ui.add(
                                egui::ProgressBar::new(frac).desired_width(ui.available_width() - 170.0).text(t!(
                                    "{} of {}",
                                    human(t.done),
                                    human(t.size)
                                )),
                            );
                            if ui.button(RichText::new(ic::X_CIRCLE)).on_hover_text(t!("Cancel")).clicked() {
                                out.push(Cmd::Cancel(t.upload, t.id));
                            }
                        });
                    }
                });
                if !self.status.is_empty() {
                    ui.label(RichText::new(&self.status).weak());
                }
            }
            ui.add_space(2.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            // The two sides share the room down to a narrow pane; the arrows between them too.
            let gap = if ui.available_width() < 560.0 { 44.0 } else { 72.0 };
            let w = ((ui.available_width() - gap) / 2.0).max(110.0);
            let h = ui.available_height();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(Vec2::new(w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    self.pane_ui(ui, false, t!("This computer"), out)
                });
                ui.allocate_ui_with_layout(Vec2::new(gap - 16.0, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(h * 0.38);
                        let up = !self.local.sel.is_empty();
                        let b = egui::Button::new(RichText::new(ic::ARROW_RIGHT).size(18.0))
                            .frame_when_inactive(false)
                            .min_size(Vec2::splat(40.0));
                        if ui.add_enabled(up, b).on_hover_text(t!("Upload the selection to the remote folder")).clicked() {
                            let paths = self.local.selected_paths();
                            self.transfer(false, paths, out);
                        }
                        let down = !self.remote.sel.is_empty();
                        let b = egui::Button::new(RichText::new(ic::ARROW_LEFT).size(18.0))
                            .frame_when_inactive(false)
                            .min_size(Vec2::splat(40.0));
                        if ui.add_enabled(down, b).on_hover_text(t!("Download the selection to this computer")).clicked() {
                            let paths = self.remote.selected_paths();
                            self.transfer(true, paths, out);
                        }
                    });
                });
                ui.allocate_ui_with_layout(Vec2::new(w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    self.pane_ui(ui, true, &t!("SSH host: {}", host), out)
                });
            });
        });
        self.dialog_ui(ui.ctx(), out);
        self.shortcuts(ui.ctx(), out);
    }

    fn pane_ui(&mut self, ui: &mut egui::Ui, remote: bool, title: &str, out: &mut Vec<Cmd>) {
        let focus = self.focus_remote == remote;
        let stroke =
            if focus { ACCENT.gamma_multiply(0.7) } else { ui.visuals().widgets.noninteractive.bg_stroke.color };
        egui::Frame::new()
            .fill(ui.visuals().window_fill)
            .stroke(egui::Stroke::new(1.0, stroke))
            .corner_radius(CornerRadius::same(crate::gui::theme::R_MD))
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.horizontal(|ui| {
                    let glyph = if remote { ic::MONITOR } else { ic::HARD_DRIVE };
                    // The buttons first, from the right; the title takes what is left, cut short.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let p = self.pane(remote);
                        let (g, tip) = if p.hidden {
                            (ic::EYE, t!("Hide hidden files"))
                        } else {
                            (ic::EYE_SLASH, t!("Show hidden files"))
                        };
                        if crate::gui::icon(ui, g, tip, p.hidden).clicked() {
                            p.hidden = !p.hidden;
                        }
                        if crate::gui::icon(ui, ic::FOLDER_PLUS, t!("New folder"), false).clicked() {
                            self.dialog = Some(Dialog::NewFolder { remote, name: t!("New folder").into() });
                        }
                        if crate::gui::icon(ui, ic::ARROWS_CLOCKWISE, t!("Refresh"), false).clicked() {
                            let path = self.pane(remote).path.clone();
                            self.go(remote, path, out);
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(format!("{glyph}  {title}")).strong()).truncate())
                                .on_hover_text(title);
                        });
                    });
                });
                ui.horizontal(|ui| {
                    if crate::gui::icon(ui, ic::ARROW_UP, t!("Up one folder (Backspace)"), false).clicked() {
                        let path = self.pane(remote).parent();
                        self.go(remote, path, out);
                    }
                    if crate::gui::icon(ui, ic::HOUSE, t!("Home folder"), false).clicked() {
                        self.go(remote, String::new(), out);
                    }
                    if cfg!(windows) || remote {
                        let tip = t!("Drives (Windows)");
                        if crate::gui::icon(ui, ic::HARD_DRIVES, tip, false).clicked() {
                            self.go(remote, DRIVES.into(), out);
                        }
                    }
                    // As an address bar: a click selects the whole path, Enter goes there, and
                    // leaving it any other way (Esc) puts back where the pane is.
                    let p = self.pane(remote);
                    let edit = crate::gui::field_selecting(ui, &mut p.typed, |t| 0..t.chars().count());
                    if edit.lost_focus() {
                        // Taken, so the list does not also open its selection with it.
                        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter)) {
                            let typed = p.typed.trim().to_string();
                            let path = if typed == "Drives" || typed == t!("Drives") { DRIVES.into() } else { typed };
                            self.go(remote, path, out);
                        } else {
                            p.typed = p.path.clone();
                        }
                    }
                });
                let p = self.pane(remote);
                if !p.error.is_empty() {
                    ui.colored_label(crate::gui::bad(), crate::lang::tr_msg(&p.error));
                }
                let footer = 26.0;
                let table_frame = egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .corner_radius(CornerRadius::same(crate::gui::theme::R_SM))
                    .inner_margin(2.0);
                let table_h = ui.available_height() - footer;
                let (drop, payload) = ui.dnd_drop_zone::<Dragged, _>(table_frame, |ui| {
                    ui.set_min_size(Vec2::new(ui.available_width(), table_h));
                    ui.set_max_height(table_h);
                    self.table(ui, remote, out);
                });
                if let Some(d) = payload
                    && d.remote != remote
                {
                    self.transfer(d.remote, d.paths.clone(), out);
                }
                let _ = drop;
                let text = self.pane(remote).summary();
                ui.add(egui::Label::new(RichText::new(text).small().weak()).truncate());
            });
    }

    fn table(&mut self, ui: &mut egui::Ui, remote: bool, out: &mut Vec<Cmd>) {
        let row_h = 24.0;
        // The rows below lose what the scroll bar takes; the headings line up with them.
        let width = ui.available_width() - ui.spacing().scroll.allocated_width();
        // The name keeps most of the room: the date goes first as the list narrows, then the size
        // (the line under the list still tells them for the selection).
        let (size_w, date_w) = if width >= 420.0 {
            (84.0, 132.0)
        } else if width >= 260.0 {
            (76.0, 0.0)
        } else {
            (0.0, 0.0)
        };
        // Column headers that sort.
        let (head, _) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::hover());
        let cols = [
            (Sort::Name, n!("Name"), Rect::from_min_max(head.min, egui::pos2(head.max.x - size_w - date_w, head.max.y))),
            (
                Sort::Size,
                n!("Size"),
                Rect::from_min_max(
                    egui::pos2(head.max.x - size_w - date_w, head.min.y),
                    egui::pos2(head.max.x - date_w, head.max.y),
                ),
            ),
            (Sort::Modified, n!("Modified"), Rect::from_min_max(egui::pos2(head.max.x - date_w, head.min.y), head.max)),
        ];
        let weak = ui.visuals().weak_text_color();
        for (key, name, rect) in cols {
            if rect.width() < 1.0 {
                continue;
            }
            let r = ui.interact(rect, ui.make_persistent_id(("sort", name, remote)), Sense::click());
            let p = self.pane(remote);
            let mark = if p.sort == key { if p.ascending { ic::CARET_UP } else { ic::CARET_DOWN } } else { "" };
            let color = if r.hovered() { ui.visuals().strong_text_color() } else { weak };
            let (pos, align) = if key == Sort::Size {
                (rect.right_center() - Vec2::new(8.0, 0.0), Align2::RIGHT_CENTER)
            } else {
                (rect.left_center() + Vec2::new(8.0, 0.0), Align2::LEFT_CENTER)
            };
            ui.painter().text(pos, align, format!("{} {mark}", crate::lang::tr(name)), FontId::proportional(12.5), color);
            if r.clicked() {
                if p.sort == key {
                    p.ascending = !p.ascending;
                } else {
                    (p.sort, p.ascending) = (key, true);
                }
                p.resort();
            }
        }
        ui.painter().hline(head.x_range(), head.bottom(), ui.visuals().widgets.noninteractive.bg_stroke);

        let visible = self.pane(remote).visible();
        let mut open: Option<usize> = None;
        let mut menu_action: Option<(&'static str, usize)> = None;
        // Rows touch, and show_rows counts the spacing in each row's height when it works out the rows in view.
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(("rows", remote)).show_rows(
            ui,
            row_h,
            visible.len(),
            |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for &i in &visible[range] {
                    let p = self.pane(remote);
                    let e = p.entries[i].clone();
                    let (rect, r) =
                        ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click_and_drag());
                    let on = p.sel.contains(&i);
                    let painter = ui.painter_at(rect);
                    if on {
                        painter.rect_filled(rect, CornerRadius::same(crate::gui::theme::R_SM), crate::gui::selected_fill());
                        painter.rect_stroke(rect, CornerRadius::same(crate::gui::theme::R_SM), egui::Stroke::new(1.0, crate::gui::selected_stroke()), egui::StrokeKind::Inside);
                    } else if r.hovered() {
                        painter.rect_filled(rect, CornerRadius::same(crate::gui::theme::R_SM), ui.visuals().widgets.hovered.bg_fill);
                    }
                    let text = ui.visuals().text_color();
                    let glyph = if p.path == DRIVES {
                        ic::HARD_DRIVE
                    } else if e.dir {
                        ic::FOLDER
                    } else if is_text(&e.name) {
                        ic::FILE_TEXT
                    } else {
                        ic::FILE
                    };
                    // Folders and drives in the app's blue for the theme; files quieter.
                    let glyph_color = if e.dir || p.path == DRIVES { crate::gui::brand() } else { weak };
                    painter.text(
                        rect.left_center() + Vec2::new(8.0, 0.0),
                        Align2::LEFT_CENTER,
                        glyph,
                        FontId::proportional(15.0),
                        glyph_color,
                    );
                    let name_rect = Rect::from_min_max(
                        rect.min + Vec2::new(30.0, 0.0),
                        egui::pos2(rect.max.x - size_w - date_w, rect.max.y),
                    );
                    let galley = painter.layout(e.name.clone(), FontId::proportional(13.5), text, f32::INFINITY);
                    let clip = ui.painter_at(name_rect);
                    clip.galley(egui::pos2(name_rect.min.x, rect.center().y - galley.size().y / 2.0), galley, text);
                    if !e.dir && size_w > 0.0 {
                        painter.text(
                            egui::pos2(rect.max.x - date_w - 8.0, rect.center().y),
                            Align2::RIGHT_CENTER,
                            human(e.size),
                            FontId::proportional(12.5),
                            weak,
                        );
                    }
                    let date = if date_w > 0.0 { crate::files::local_time(e.mtime) } else { String::new() };
                    painter.text(
                        egui::pos2(rect.max.x - date_w + 8.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        date,
                        FontId::proportional(12.5),
                        weak,
                    );
                    if r.clicked() || r.secondary_clicked() || r.drag_started() {
                        self.focus_remote = remote;
                        let p = self.pane(remote);
                        let m = ui.input(|i| i.modifiers);
                        if r.secondary_clicked() || r.drag_started() {
                            if !p.sel.contains(&i) {
                                p.sel = BTreeSet::from([i]);
                                p.anchor = Some(i);
                            }
                        } else if m.shift && p.anchor.is_some() {
                            let a = visible.iter().position(|&v| Some(v) == p.anchor).unwrap_or(0);
                            let b = visible.iter().position(|&v| v == i).unwrap_or(0);
                            p.sel = visible[a.min(b)..=a.max(b)].iter().copied().collect();
                        } else if m.command {
                            if !p.sel.remove(&i) {
                                p.sel.insert(i);
                            }
                            p.anchor = Some(i);
                        } else {
                            p.sel = BTreeSet::from([i]);
                            p.anchor = Some(i);
                        }
                    }
                    if r.double_clicked() {
                        open = Some(i);
                    }
                    let p = self.pane(remote);
                    if r.dragged() {
                        r.dnd_set_drag_payload(Dragged { remote, paths: p.selected_paths() });
                    }
                    let other = if remote { "Download" } else { "Upload" };
                    r.context_menu(|ui| {
                        crate::gui::menu_look(ui);
                        let mut item = |ui: &mut egui::Ui, g: &str, label: &'static str| {
                            if ui.button(format!("{g}  {}", crate::lang::tr(label))).clicked() {
                                menu_action = Some((label, i));
                                ui.close();
                            }
                        };
                        if e.dir {
                            item(ui, ic::FOLDER_OPEN, n!("Open"));
                        }
                        item(
                            ui,
                            if remote { ic::DOWNLOAD } else { ic::UPLOAD },
                            if remote { n!("Download") } else { n!("Upload") },
                        );
                        let _ = other;
                        ui.separator();
                        item(ui, ic::PENCIL, n!("Rename"));
                        item(ui, ic::TRASH, n!("Delete"));
                        ui.separator();
                        item(ui, ic::FOLDER_PLUS, n!("New folder"));
                    });
                }
            },
        );
        if let Some(i) = open {
            let p = self.pane(remote);
            let e = &p.entries[i];
            let path = p.child(&e.name);
            if e.dir {
                self.go(remote, path, out);
            } else {
                self.transfer(remote, vec![path], out);
            }
        }
        if let Some((what, i)) = menu_action {
            let p = self.pane(remote);
            let Some(e) = p.entries.get(i).cloned() else { return };
            match what {
                "Open" => {
                    let path = p.child(&e.name);
                    self.go(remote, path, out);
                }
                "Download" | "Upload" => {
                    let paths = p.selected_paths();
                    self.transfer(remote, paths, out);
                }
                "Rename" => self.dialog = Some(Dialog::Rename { remote, path: p.child(&e.name), name: e.name }),
                "Delete" => self.dialog = Some(Dialog::Delete { remote, paths: p.selected_paths() }),
                _ => self.dialog = Some(Dialog::NewFolder { remote, name: t!("New folder").into() }),
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context, out: &mut Vec<Cmd>) {
        if !self.active || self.dialog.is_some() || ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let remote = self.focus_remote;
        let (del, f2, enter, back, all) = ctx.input(|i| {
            (
                i.key_pressed(Key::Delete),
                i.key_pressed(Key::F2),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Backspace),
                i.modifiers.command && i.key_pressed(Key::A),
            )
        });
        let p = self.pane(remote);
        if del && !p.sel.is_empty() {
            self.dialog = Some(Dialog::Delete { remote, paths: p.selected_paths() });
        } else if f2 && p.sel.len() == 1 {
            let e = p.selected().next().cloned().unwrap();
            self.dialog = Some(Dialog::Rename { remote, path: p.child(&e.name), name: e.name });
        } else if enter && p.sel.len() == 1 {
            let e = p.selected().next().cloned().unwrap();
            let path = p.child(&e.name);
            if e.dir {
                self.go(remote, path, out);
            } else {
                self.transfer(remote, vec![path], out);
            }
        } else if back {
            let path = p.parent();
            self.go(remote, path, out);
        } else if all {
            p.sel = p.visible().into_iter().collect();
        }
    }

    fn dialog_ui(&mut self, ctx: &egui::Context, out: &mut Vec<Cmd>) {
        let Some(d) = self.dialog.as_mut() else { return };
        let mut done: Option<bool> = None;
        let modal = egui::Modal::new(egui::Id::unique("sshremote-file-dialog")).show(ctx, |ui| {
            ui.set_width(360.0);
            let deleting = matches!(d, Dialog::Delete { .. });
            let title = if matches!(d, Dialog::NewFolder { .. }) { t!("New folder") } else { t!("Rename") };
            match d {
                Dialog::NewFolder { name, .. } | Dialog::Rename { name, .. } => {
                    ui.label(RichText::new(title).strong().size(16.0));
                    // The name without its extension is selected, ready to be typed over.
                    let r = crate::gui::field_selecting(ui, name, |t| {
                        let stem = t.rfind('.').filter(|&i| i > 0).map_or(t, |i| &t[..i]);
                        0..stem.chars().count()
                    });
                    if !r.has_focus() {
                        r.request_focus();
                    }
                }
                Dialog::Delete { paths, .. } => {
                    ui.label(RichText::new(t!("Delete")).strong().size(16.0));
                    let what = if paths.len() == 1 { paths[0].clone() } else { t!("{} items", paths.len()) };
                    ui.label(t!(
                        "Delete {}? Folders are deleted with everything in them. This cannot be undone.",
                        what
                    ));
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if deleting { t!("Delete") } else { t!("OK") };
                let fill = if deleting { Color32::from_rgb(200, 62, 62) } else { ACCENT };
                if ui.add(egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(fill)).clicked() {
                    done = Some(true);
                }
                if ui.button(t!("Cancel")).clicked() {
                    done = Some(false);
                }
            });
        });
        if done.is_none() && self.active && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter)) {
            done = Some(true);
        }
        if modal.should_close() && done.is_none() {
            done = Some(false);
        }
        let Some(ok) = done else { return };
        let d = self.dialog.take().unwrap();
        if !ok {
            return;
        }
        match d {
            Dialog::NewFolder { remote, name } => {
                let path = self.pane(remote).child(name.trim());
                self.run_op(remote, FileOp::Mkdir, path, String::new(), out);
            }
            Dialog::Rename { remote, path, name } => {
                self.run_op(remote, FileOp::Rename, path, name.trim().to_string(), out)
            }
            Dialog::Delete { remote, paths } => {
                for path in paths {
                    self.run_op(remote, FileOp::Delete, path, String::new(), out);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::gui::snapshot::RunTest;
    /// Keys typed into another pane do not reach a file manager that is not the active one:
    /// Delete would otherwise ask to delete its selection, and Enter would confirm.
    #[test]
    fn an_inactive_file_manager_ignores_keys() {
        for active in [false, true] {
            let ctx = egui::Context::default();
            let mut w = FileWindow::new();
            w.active = active;
            w.remote.sel.insert(0);
            w.focus_remote = true;
            let input = |events| egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(900.0, 600.0))),
                events,
                ..Default::default()
            };
            let delete = egui::Event::Key { key: Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
            let _ = ctx.run_test(input(Vec::new()), |ui| {
                w.embedded(ui, "host", &[]);
            });
            let _ = ctx.run_test(input(vec![delete]), |ui| {
                w.embedded(ui, "host", &[]);
            });
            assert_eq!(w.dialog.is_some(), active, "active {active}");
        }
    }

    use super::*;

    /// Enter that confirms a dialog only does what the dialog says: the list behind it does not
    /// also open (download) its selection with the same key.
    #[test]
    fn enter_in_a_dialog_does_not_also_open_the_selection() {
        let ctx = egui::Context::default();
        let mut w = FileWindow::new();
        w.focus_remote = true;
        w.remote.path = "/home/a".into();
        w.remote.entries = vec![DirEntry { name: "notes.txt".into(), dir: false, size: 5, mtime: 0 }];
        w.remote.sel.insert(0);
        let input = |events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(900.0, 600.0))),
            events,
            ..Default::default()
        };
        let key = |key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        let _ = ctx.run_test(input(Vec::new()), |ui| {
            w.embedded(ui, "host", &[]);
        });
        // A new folder, its name typed; the file stays selected behind the dialog.
        w.dialog = Some(Dialog::NewFolder { remote: true, name: "made".into() });
        let mut cmds = Vec::new();
        let _ = ctx.run_test(input(vec![key(Key::Enter)]), |ui| {
            cmds = w.embedded(ui, "host", &[]);
        });
        let fetches = cmds.iter().filter(|c| matches!(c, Cmd::Send(ToAgent::Fetch { .. }))).count();
        let made = cmds.iter().filter(|c| matches!(c, Cmd::Send(ToAgent::FileOp { .. }))).count();
        assert_eq!((made, fetches), (1, 0), "the folder made, the selected file not downloaded as well");
    }

    #[test]
    fn paths_climb_to_the_drives() {
        let mut p = Pane::new();
        p.path = r"C:\Users\me".into();
        assert_eq!(p.parent(), r"C:\Users");
        assert_eq!(p.child("x"), r"C:\Users\me\x");
        p.path = r"C:\".into();
        assert_eq!(p.parent(), DRIVES);
        p.path = DRIVES.into();
        assert_eq!(p.child(r"D:\"), r"D:\");
        p.path = "/home/a".into();
        assert_eq!(p.parent(), "/home");
        p.path = "/".into();
        assert_eq!(p.parent(), "/");
    }

    #[test]
    fn folders_sort_first() {
        let e = |n: &str, dir, size| DirEntry { name: n.into(), dir, size, mtime: 0 };
        let mut v = vec![e("b", false, 5), e("a", false, 9), e("z", true, 0)];
        sort(&mut v, Sort::Size, false);
        assert_eq!(v.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["z", "a", "b"]);
    }

    /// Under each list: what the folder holds, the one selected item's name, size and date, or
    /// how many are selected.
    #[test]
    fn the_line_under_the_list_tells_the_selection() {
        let e = |n: &str, dir, size| DirEntry { name: n.into(), dir, size, mtime: 0 };
        let mut p = Pane::new();
        p.set_list("/x".into(), vec![e("docs", true, 0), e("a.txt", false, 2048), e("b.bin", false, 1024)], String::new());
        assert_eq!(p.summary(), "1 folder, 2 files (3.0 KB)");
        let a = p.entries.iter().position(|x| x.name == "a.txt").unwrap();
        p.sel.insert(a);
        assert!(p.summary().starts_with("a.txt") && p.summary().contains("2.0 KB"), "{}", p.summary());
        p.sel.extend(0..3);
        assert_eq!(p.summary(), "3 of 3 selected (3.0 KB, 1 folder)");
    }
}
