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
    Close,
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

    fn sep(&self) -> char {
        if self.path.contains('\\') && !self.path.contains('/') { '\\' } else { '/' }
    }

    fn child(&self, name: &str) -> String {
        if self.path == DRIVES {
            return name.to_string();
        }
        let sep = self.sep();
        if self.path.ends_with(sep) { format!("{}{name}", self.path) } else { format!("{}{sep}{name}", self.path) }
    }

    fn parent(&self) -> String {
        let sep = self.sep();
        let trimmed = self.path.trim_end_matches(sep);
        if trimmed.len() == 2 && trimmed.ends_with(':') {
            // A drive root; above it are the drives.
            return DRIVES.into();
        }
        match trimmed.rfind(sep) {
            Some(0) => sep.to_string(),
            Some(i) if trimmed[..i].ends_with(':') => format!("{}{sep}", &trimmed[..i]),
            Some(i) => trimmed[..i].to_string(),
            None => self.path.clone(),
        }
    }

    fn shown_path(path: &str) -> String {
        if path == DRIVES { "Drives".into() } else { path.to_string() }
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

pub struct FileWindow {
    pub local: Pane,
    pub remote: Pane,
    dialog: Option<Dialog>,
    /// The pane keyboard shortcuts act on.
    focus_remote: bool,
    status: String,
}

impl FileWindow {
    /// Opens on the local home folder; the remote listing is requested by the caller.
    pub fn new() -> Self {
        let mut w = FileWindow {
            local: Pane::new(),
            remote: Pane::new(),
            dialog: None,
            focus_remote: true,
            status: String::new(),
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
        self.status = if ok { text.to_string() } else { format!("Failed: {text}") };
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
                self.status = if ok { text } else { format!("Failed: {text}") };
            }
            self.refresh_local();
        }
    }

    /// Draws the window; returns what the session should do.
    pub fn show(&mut self, ctx: &egui::Context, host: &str, transfers: &[Transfer]) -> Vec<Cmd> {
        let mut out = Vec::new();
        let mut builder = egui::ViewportBuilder::default()
            .with_title(format!("Files - {host}"))
            .with_inner_size([1000.0, 640.0])
            .with_min_inner_size([640.0, 360.0])
            .with_decorations(false);
        if let Some(icon) = crate::gui::app_icon() {
            builder = builder.with_icon(icon);
        }
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("sshremote-files"), builder, |ui, _| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                out.push(Cmd::Close);
            }
            crate::gui::chrome::behaviour(ui.ctx());
            let bar = crate::gui::chrome::title_bar(ui, |ui| {
                ui.label(RichText::new(format!("Files - {host}")).size(13.0));
            });
            if bar.close {
                out.push(Cmd::Close);
            }
            self.body(ui, host, transfers, &mut out);
        });
        out
    }

    /// The file manager drawn inside another window, as a connection made for files shows it.
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
        if !dropped.is_empty() {
            out.push(Cmd::Upload { paths: dropped, to: self.remote.path.clone() });
        }
        egui::Panel::bottom("transfers").show(ui, |ui| {
            ui.add_space(4.0);
            if transfers.is_empty() {
                let hint = "Drag files between the two sides, or drop files from the desktop on this window to upload.";
                ui.label(RichText::new(if self.status.is_empty() { hint } else { &self.status }).weak());
            } else {
                ui.label(RichText::new(format!("Transfers ({})", transfers.len())).strong());
                egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                    for t in transfers {
                        ui.horizontal(|ui| {
                            let arrow = if t.upload { ic::UPLOAD } else { ic::DOWNLOAD };
                            ui.label(RichText::new(arrow).color(ACCENT));
                            ui.add_sized([280.0, 18.0], egui::Label::new(&t.name).truncate());
                            let frac = if t.size == 0 { 1.0 } else { t.done as f32 / t.size as f32 };
                            ui.add(
                                egui::ProgressBar::new(frac).desired_width(ui.available_width() - 170.0).text(format!(
                                    "{} of {}",
                                    human(t.done),
                                    human(t.size)
                                )),
                            );
                            if ui.button(RichText::new(ic::X_CIRCLE)).on_hover_text("Cancel").clicked() {
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
            let gap = 72.0;
            let w = ((ui.available_width() - gap) / 2.0).max(200.0);
            let h = ui.available_height();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(Vec2::new(w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    self.pane_ui(ui, false, "This computer", out)
                });
                ui.allocate_ui_with_layout(Vec2::new(gap - 16.0, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(h * 0.38);
                        let up = !self.local.sel.is_empty();
                        let b = egui::Button::new(RichText::new(ic::ARROW_RIGHT).size(18.0))
                            .frame_when_inactive(false)
                            .min_size(Vec2::splat(40.0));
                        if ui.add_enabled(up, b).on_hover_text("Upload the selection to the remote folder").clicked() {
                            let paths = self.local.selected_paths();
                            self.transfer(false, paths, out);
                        }
                        let down = !self.remote.sel.is_empty();
                        let b = egui::Button::new(RichText::new(ic::ARROW_LEFT).size(18.0))
                            .frame_when_inactive(false)
                            .min_size(Vec2::splat(40.0));
                        if ui.add_enabled(down, b).on_hover_text("Download the selection to this computer").clicked() {
                            let paths = self.remote.selected_paths();
                            self.transfer(true, paths, out);
                        }
                    });
                });
                ui.allocate_ui_with_layout(Vec2::new(w, h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    self.pane_ui(ui, true, &format!("SSH host: {host}"), out)
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
                    ui.label(RichText::new(format!("{glyph}  {title}")).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let p = self.pane(remote);
                        let (g, tip) = if p.hidden {
                            (ic::EYE, "Hide hidden files")
                        } else {
                            (ic::EYE_SLASH, "Show hidden files")
                        };
                        if crate::gui::icon(ui, g, tip, p.hidden).clicked() {
                            p.hidden = !p.hidden;
                        }
                        if crate::gui::icon(ui, ic::FOLDER_PLUS, "New folder", false).clicked() {
                            self.dialog = Some(Dialog::NewFolder { remote, name: "New folder".into() });
                        }
                        if crate::gui::icon(ui, ic::ARROWS_CLOCKWISE, "Refresh", false).clicked() {
                            let path = self.pane(remote).path.clone();
                            self.go(remote, path, out);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if crate::gui::icon(ui, ic::ARROW_UP, "Up one folder (Backspace)", false).clicked() {
                        let path = self.pane(remote).parent();
                        self.go(remote, path, out);
                    }
                    if crate::gui::icon(ui, ic::HOUSE, "Home folder", false).clicked() {
                        self.go(remote, String::new(), out);
                    }
                    if cfg!(windows) || remote {
                        let tip = "Drives (Windows)";
                        if crate::gui::icon(ui, ic::HARD_DRIVES, tip, false).clicked() {
                            self.go(remote, DRIVES.into(), out);
                        }
                    }
                    let p = self.pane(remote);
                    let edit = ui.add(egui::TextEdit::singleline(&mut p.typed).desired_width(f32::INFINITY));
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        let typed = p.typed.trim().to_string();
                        let path = if typed == "Drives" { DRIVES.into() } else { typed };
                        self.go(remote, path, out);
                    }
                });
                let p = self.pane(remote);
                if !p.error.is_empty() {
                    ui.colored_label(crate::gui::bad(), &p.error);
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
                let p = self.pane(remote);
                let n = p.visible().len();
                let selected: u64 = p.selected().map(|e| if e.dir { 0 } else { e.size }).sum();
                let text = if p.loading {
                    "Loading...".to_string()
                } else if p.sel.is_empty() {
                    format!("{n} items")
                } else {
                    format!("{n} items, {} selected ({})", p.sel.len(), human(selected))
                };
                ui.label(RichText::new(text).small().weak());
            });
    }

    fn table(&mut self, ui: &mut egui::Ui, remote: bool, out: &mut Vec<Cmd>) {
        let row_h = 24.0;
        let width = ui.available_width();
        let (size_w, date_w) = (84.0, 132.0);
        // Column headers that sort.
        let (head, _) = ui.allocate_exact_size(Vec2::new(width, row_h), Sense::hover());
        let cols = [
            (Sort::Name, "Name", Rect::from_min_max(head.min, egui::pos2(head.max.x - size_w - date_w, head.max.y))),
            (
                Sort::Size,
                "Size",
                Rect::from_min_max(
                    egui::pos2(head.max.x - size_w - date_w, head.min.y),
                    egui::pos2(head.max.x - date_w, head.max.y),
                ),
            ),
            (Sort::Modified, "Modified", Rect::from_min_max(egui::pos2(head.max.x - date_w, head.min.y), head.max)),
        ];
        let weak = ui.visuals().weak_text_color();
        for (key, name, rect) in cols {
            let r = ui.interact(rect, ui.make_persistent_id(("sort", name, remote)), Sense::click());
            let p = self.pane(remote);
            let mark = if p.sort == key { if p.ascending { ic::CARET_UP } else { ic::CARET_DOWN } } else { "" };
            let color = if r.hovered() { ui.visuals().strong_text_color() } else { weak };
            let (pos, align) = if key == Sort::Size {
                (rect.right_center() - Vec2::new(8.0, 0.0), Align2::RIGHT_CENTER)
            } else {
                (rect.left_center() + Vec2::new(8.0, 0.0), Align2::LEFT_CENTER)
            };
            ui.painter().text(pos, align, format!("{name} {mark}"), FontId::proportional(12.5), color);
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
                        painter.rect_filled(rect, CornerRadius::same(crate::gui::theme::R_SM), ACCENT.gamma_multiply(0.35));
                    } else if r.hovered() {
                        painter.rect_filled(rect, CornerRadius::same(crate::gui::theme::R_SM), ui.visuals().widgets.hovered.bg_fill);
                    }
                    let text = ui.visuals().text_color();
                    let glyph = if p.path == DRIVES {
                        ic::HARD_DRIVE
                    } else if e.dir {
                        ic::FOLDER
                    } else {
                        ic::FILE
                    };
                    // Folders in amber, deeper on a light background so they still stand out.
                    let folder = if ui.visuals().dark_mode { Color32::from_rgb(230, 180, 70) } else { Color32::from_rgb(196, 134, 18) };
                    let glyph_color = if e.dir { folder } else { weak };
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
                    if !e.dir {
                        painter.text(
                            egui::pos2(rect.max.x - date_w - 8.0, rect.center().y),
                            Align2::RIGHT_CENTER,
                            human(e.size),
                            FontId::proportional(12.5),
                            weak,
                        );
                    }
                    painter.text(
                        egui::pos2(rect.max.x - date_w + 8.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        crate::files::local_time(e.mtime),
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
                        let mut item = |ui: &mut egui::Ui, g: &str, label: &'static str| {
                            if ui.button(format!("{g}  {label}")).clicked() {
                                menu_action = Some((label, i));
                                ui.close();
                            }
                        };
                        if e.dir {
                            item(ui, ic::FOLDER_OPEN, "Open");
                        }
                        item(
                            ui,
                            if remote { ic::DOWNLOAD } else { ic::UPLOAD },
                            if remote { "Download" } else { "Upload" },
                        );
                        let _ = other;
                        ui.separator();
                        item(ui, ic::PENCIL, "Rename");
                        item(ui, ic::TRASH, "Delete");
                        ui.separator();
                        item(ui, ic::FOLDER_PLUS, "New folder");
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
                _ => self.dialog = Some(Dialog::NewFolder { remote, name: "New folder".into() }),
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context, out: &mut Vec<Cmd>) {
        if self.dialog.is_some() || ctx.memory(|m| m.focused().is_some()) {
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
            let title = if matches!(d, Dialog::NewFolder { .. }) { "New folder" } else { "Rename" };
            match d {
                Dialog::NewFolder { name, .. } | Dialog::Rename { name, .. } => {
                    ui.label(RichText::new(title).strong().size(16.0));
                    let r = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
                    if !r.has_focus() {
                        r.request_focus();
                    }
                }
                Dialog::Delete { paths, .. } => {
                    ui.label(RichText::new("Delete").strong().size(16.0));
                    let what = if paths.len() == 1 { paths[0].clone() } else { format!("{} items", paths.len()) };
                    ui.label(format!(
                        "Delete {what}? Folders are deleted with everything in them. This cannot be undone."
                    ));
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if deleting { "Delete" } else { "OK" };
                let fill = if deleting { Color32::from_rgb(200, 62, 62) } else { ACCENT };
                if ui.add(egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(fill)).clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
            });
        });
        if done.is_none() && ctx.input(|i| i.key_pressed(Key::Enter)) {
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
    use super::*;

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
}
