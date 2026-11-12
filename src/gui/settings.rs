//! Settings, over the home window: the pages on the left, the one chosen on the right.

use super::*;
use crate::tools::{BackupTool, KeyTool, VaultTool};

/// A page of the settings.
enum Page {
    Appearance,
    Connections,
    Keys(KeyTool),
    Passwords(VaultTool),
    Backup(BackupTool),
    Extensions,
    Parts,
    About,
}

impl Page {
    fn key(&self) -> u8 {
        match self {
            Page::Appearance => 0,
            Page::Connections => 1,
            Page::Keys(_) => 2,
            Page::Passwords(_) => 3,
            Page::Backup(_) => 4,
            Page::Extensions => 5,
            Page::Parts => 6,
            Page::About => 7,
        }
    }

    /// Its title and what it is for; Connections and About write their own.
    fn title(&self) -> Option<(&'static str, &'static str)> {
        Some(match self {
            Page::Appearance => ("Appearance", "Theme, fonts, colours, background pictures and the load overlay."),
            Page::Keys(_) => ("SSH keys", "Your key pairs: copy one, install it on a computer, or create one."),
            Page::Passwords(_) => ("Saved passwords", "Passwords for SSH and for IDs, in the system's password store."),
            Page::Backup(_) => ("Backup and restore", "Computers, settings and ~/.ssh, by hand or each day."),
            Page::Extensions => ("Extensions", "Looks, commands, status bar items, panels and features. Each one can do only what its card says."),
            Page::Parts => ("Parts", "What this app is made of."),
            Page::Connections | Page::About => return None,
        })
    }

    /// A page's own form is open (Back and Esc return to its list).
    fn in_form(&self) -> bool {
        match self {
            Page::Keys(t) => t.sub_page(),
            Page::Passwords(t) => t.sub_page(),
            Page::Backup(t) => t.sub_page(),
            _ => false,
        }
    }

    /// From a page's own form back to its list; `false` when already there.
    fn back(&mut self) -> bool {
        let t: Option<&mut dyn SubPage> = match self {
            Page::Keys(t) => Some(t),
            Page::Passwords(t) => Some(t),
            Page::Backup(t) => Some(t),
            _ => None,
        };
        t.is_some_and(|t| t.sub_page() && {
            t.back();
            true
        })
    }
}

/// A tool page with forms of its own inside it.
trait SubPage {
    fn sub_page(&self) -> bool;
    fn back(&mut self);
}

macro_rules! sub_page {
    ($($t:ty),*) => {$(
        impl SubPage for $t {
            fn sub_page(&self) -> bool { <$t>::sub_page(self) }
            fn back(&mut self) { <$t>::back(self) }
        }
    )*};
}
sub_page!(KeyTool, VaultTool, BackupTool);

pub(super) struct Settings {
    page: Page,
}

impl Settings {
    pub(super) fn new() -> Settings {
        Settings { page: Page::Appearance }
    }

    /// Opened on the SSH keys, for the site's pictures.
    #[cfg(test)]
    pub(super) fn keys() -> Settings {
        Settings { page: Page::Keys(KeyTool::new()) }
    }
}

impl App {
    /// The settings window, while it is open.
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.settings.take() else { return };
        let (modal, fit) = modal(ctx, "settings", Vec2::new(900.0, 620.0));
        let mut close = false;
        let mut actions = Vec::new();
        let mut changed = false;
        let r = modal.show(ctx, |ui| {
            ui.set_width(fit.x);
            ui.set_height(fit.y);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}  Settings", ic::GEAR)).size(16.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.add(tool_button(ic::X, false)).on_hover_text("Close (Esc)").clicked();
                });
            });
            ui.separator();
            let full = ui.available_rect_before_wrap();
            let nav = Rect::from_min_size(full.min, Vec2::new(176.0, full.height()));
            let body = full.with_min_x(nav.right() + 18.0);
            ui.painter().vline(nav.right() + 8.0, full.y_range(), Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color));
            let mut n = ui.new_child(egui::UiBuilder::new().max_rect(nav).layout(egui::Layout::top_down(egui::Align::Min)));
            self.settings_nav(&mut n, &mut s);
            let mut b = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
            egui::ScrollArea::vertical().id_salt(("settings-page", s.page.key())).auto_shrink([false, false]).show(&mut b, |ui| {
                ui.set_max_width(ui.available_width() - 12.0);
                if s.page.in_form() && ui.add(egui::Button::new(format!("{}  Back", ic::ARROW_LEFT)).frame_when_inactive(false)).clicked() {
                    s.page.back();
                }
                if let Some((title, about)) = s.page.title() {
                    page_title(ui, title, about);
                }
                match &mut s.page {
                    Page::Appearance => super::look::page(ui),
                    Page::Connections => self.connections_page(ui),
                    Page::Keys(k) => k.ui(ui, &mut actions),
                    Page::Passwords(v) => {
                        let known: Vec<String> = self.computers.iter().map(|c| c.address.clone()).collect();
                        v.ui(ui, &known);
                    }
                    Page::Backup(t) => t.ui(ui, &mut changed),
                    Page::Extensions => self.exts.get_or_insert_with(crate::ext::ExtTool::new).ui(ui),
                    Page::Parts => crate::ext::parts_page(ui),
                    Page::About => self.about_page(ui),
                }
                ui.add_space(12.0);
            });
        });
        for a in actions {
            match a {
                crate::tools::ToolAction::Terminal { title, argv } => {
                    self.open_profile(crate::termwin::Profile { name: title, icon: ic::KEY, launch: crate::termwin::Launch::Tool(argv) });
                    close = true;
                }
            }
        }
        if changed {
            self.computers_at = None;
        }
        // Esc goes back from a page's form first; the window closes from the page itself.
        if r.should_close() && !s.page.back() {
            close = true;
        }
        if !close {
            self.settings = Some(s);
        }
    }

    fn settings_nav(&mut self, ui: &mut egui::Ui, s: &mut Settings) {
        ui.spacing_mut().item_spacing.y = 2.0;
        let updates = crate::ext::updates_waiting().filter(|n| *n > 0);
        let mut pages: Vec<(u8, &str, &str)> =
            vec![(0, ic::PALETTE, "Appearance"), (1, ic::PLUGS, "Connections"), (2, ic::KEY, "SSH keys"), (3, ic::LOCK_KEY, "Passwords")];
        if crate::modules::on("backup") {
            pages.push((4, ic::ARCHIVE, "Backup"));
        }
        pages.extend([(5, ic::PUZZLE_PIECE, "Extensions"), (6, ic::SQUARES_FOUR, "Parts"), (7, ic::INFO, "About")]);
        for (key, glyph, name) in pages {
            let on = s.page.key() == key;
            let (rect, r) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), egui::Sense::click());
            if on {
                ui.painter().rect_filled(rect, theme::R_SM, selected_fill());
            } else if r.hovered() {
                ui.painter().rect_filled(rect, theme::R_SM, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            let fg = if on { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
            ui.painter().text(rect.left_center() + Vec2::new(10.0, 0.0), Align2::LEFT_CENTER, glyph, FontId::proportional(15.0), fg);
            ui.painter().text(rect.left_center() + Vec2::new(34.0, 0.0), Align2::LEFT_CENTER, name, FontId::proportional(13.5), fg);
            if key == 5
                && let Some(n) = updates
            {
                let at = rect.right_center() - Vec2::new(14.0, 0.0);
                ui.painter().circle_filled(at, 8.0, brand());
                ui.painter().text(at, Align2::CENTER_CENTER, n.to_string(), FontId::proportional(10.5), Color32::WHITE);
            }
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
                s.page = match key {
                    0 => Page::Appearance,
                    1 => Page::Connections,
                    2 => Page::Keys(KeyTool::new()),
                    3 => Page::Passwords(VaultTool::new("")),
                    4 => Page::Backup(BackupTool::new()),
                    5 => Page::Extensions,
                    6 => Page::Parts,
                    _ => Page::About,
                };
            }
        }
    }

    /// How the screen and files reach a computer: the picture's quality, the agent started there,
    /// how its screen is read, and passwords.
    fn connections_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "Connections", "How screens and files reach a computer. A terminal is plain ssh and needs none of this.");
        egui::Grid::new("connections-grid").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
            ui.label("Screen quality");
            let current = self.form.preset.map_or("Auto (adapts to the connection)".into(), |i| preset_text(&PRESETS[i]));
            let before = self.form.preset;
            combo("quality", current).width(280.0).show_ui(ui, |ui| {
                crate::gui::menu_look(ui);
                ui.selectable_value(&mut self.form.preset, None, "Auto (adapts to the connection)");
                for (i, p) in PRESETS.iter().enumerate() {
                    ui.selectable_value(&mut self.form.preset, Some(i), preset_text(p));
                }
            });
            if self.form.preset != before {
                crate::hosts::set_setting("preset", &self.form.preset.map(|i| i.to_string()).unwrap_or_default());
            }
            ui.end_row();
            ui.label("Agent command").on_hover_text("How sshremote is started on the computer, over SSH");
            if ui.add(egui::TextEdit::singleline(&mut self.form.remote).desired_width(320.0)).changed() {
                crate::hosts::set_setting("agent_command", self.form.remote.trim());
            }
            ui.end_row();
            ui.label("Screen capture").on_hover_text("How the computer's screen is read: auto picks the one for its desktop");
            ui.horizontal(|ui| {
                crate::gui::menu_look(ui);
                let test = cfg!(feature = "test-desktop").then_some("synthetic");
                let before = self.form.capture.clone();
                for c in ["auto", "gnome", "windows"].into_iter().chain(test) {
                    ui.selectable_value(&mut self.form.capture, c.to_string(), c);
                }
                if self.form.capture != before {
                    crate::hosts::set_setting("capture", &self.form.capture);
                }
            });
            ui.end_row();
            ui.label("Passwords");
            let r = ui.add_enabled(crate::vault::backend().is_some(), egui::Checkbox::new(&mut self.form.remember, "Remember the passwords typed, in the system's password store"));
            if r.changed() {
                crate::hosts::set_setting("remember_password", if self.form.remember { "yes" } else { "no" });
            }
            ui.end_row();
        });
    }

    /// The version, and on Windows the administrator rights.
    fn about_page(&mut self, ui: &mut egui::Ui) {
        page_title(ui, "SSH Remote", &format!("Version {}", env!("CARGO_PKG_VERSION")));
        ui.label("Terminals, screens and files of your computers, over SSH or by a remote access ID.");
        ui.hyperlink_to("sshremote.dev", "https://sshremote.dev");
        if cfg!(windows) {
            ui.add_space(14.0);
            ui.label(RichText::new("Administrator rights").size(15.0).strong());
            ui.add(egui::Label::new(RichText::new(crate::admin::ADDS).weak().size(12.0)).wrap());
            ui.add_space(4.0);
            if !crate::admin::elevated() && ui.add(theme::primary(format!("{}  Restart as administrator", ic::SHIELD_CHECK))).clicked() {
                self.restart_elevated(ui.ctx());
            }
            #[cfg(windows)]
            {
                let mut ask = crate::admin::ask_at_start();
                if ui.checkbox(&mut ask, "Ask for administrator rights at start").changed() {
                    crate::admin::set_ask_at_start(ask);
                }
            }
        }
    }
}

/// A page's title and a line about it.
fn page_title(ui: &mut egui::Ui, title: &str, about: &str) {
    ui.label(RichText::new(title).size(18.0).strong());
    ui.add(egui::Label::new(RichText::new(about).weak().size(12.5)).wrap());
    ui.add_space(10.0);
}
