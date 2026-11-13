//! Settings, over the home window: the pages on the left, the one chosen on the right.

use super::*;
use crate::gui::kit;
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
            Page::Connections => return None,
            Page::About => ("About", ""),
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

    /// Opened on the page named, for pictures.
    #[cfg(test)]
    pub(super) fn at(name: &str) -> Settings {
        let page = match name {
            "connections" => Page::Connections,
            "keys" => Page::Keys(KeyTool::new()),
            "passwords" => Page::Passwords(VaultTool::new("")),
            "backup" => Page::Backup(BackupTool::new()),
            "extensions" => Page::Extensions,
            "parts" => Page::Parts,
            "about" => Page::About,
            _ => Page::Appearance,
        };
        Settings { page }
    }
}

impl App {
    /// The settings window, while it is open.
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.settings.take() else { return };
        let (modal, fit) = modal(ctx, "settings", Vec2::new(940.0, 640.0));
        let mut close = false;
        let mut actions = Vec::new();
        let mut changed = false;
        let r = modal.show(ctx, |ui| {
            ui.set_width(fit.x);
            ui.set_height(fit.y);
            let full = ui.available_rect_before_wrap();
            // The pages on a pane of their own at the left, the one chosen beside it.
            let nav = Rect::from_min_size(full.min, Vec2::new(196.0, full.height()));
            let body = full.with_min_x(nav.right() + 24.0);
            let v = ui.visuals().clone();
            ui.painter().rect_filled(nav, CornerRadius::same(theme::R_MD), if v.dark_mode { v.extreme_bg_color } else { v.panel_fill });
            let mut n = ui.new_child(egui::UiBuilder::new().max_rect(nav.shrink(8.0)).layout(egui::Layout::top_down(egui::Align::Min)));
            n.label(RichText::new(format!("{}  Settings", ic::GEAR)).size(15.0).strong());
            n.add_space(10.0);
            self.settings_nav(&mut n, &mut s);
            let mut b = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
            // Close, at the top right corner of the page.
            let x = Rect::from_min_size(body.right_top() - Vec2::new(30.0, 0.0), Vec2::splat(30.0));
            close = b.put(x, tool_button(ic::X, false)).on_hover_text("Close (Esc)").clicked();
            egui::ScrollArea::vertical().id_salt(("settings-page", s.page.key())).auto_shrink([false, false]).show(&mut b, |ui| {
                ui.set_max_width(ui.available_width() - 14.0);
                if s.page.in_form() {
                    if ui.add(kit::ghost(format!("{}  Back", ic::ARROW_LEFT))).clicked() {
                        s.page.back();
                    }
                    ui.add_space(4.0);
                }
                if let Some((title, about)) = s.page.title() {
                    kit::title(ui, title, about);
                }
                match &mut s.page {
                    Page::Appearance => super::look::page(ui),
                    Page::Connections => self.connections_page(ui),
                    Page::Keys(k) => k.ui(ui, &mut actions),
                    Page::Passwords(v) => match crate::vault::backend() {
                        None => kit::notice(
                            ui,
                            kit::Tone::Warn,
                            "No password store on this computer",
                            if cfg!(windows) {
                                "Windows' Credential Manager could not be reached."
                            } else {
                                "Passwords are kept in the system's password store through secret-tool. Install it (the package libsecret-tools on Debian and Ubuntu) and come back: until then, passwords are asked each time."
                            },
                        ),
                        Some(_) => {
                            let known: Vec<String> = self.computers.iter().map(|c| c.address.clone()).collect();
                            v.ui(ui, &known);
                        }
                    },
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
        let width = ui.available_width();
        let mut pages: Vec<(u8, &str, &str)> =
            vec![(0, ic::PALETTE, "Appearance"), (1, ic::PLUGS, "Connections"), (2, ic::KEY, "SSH keys"), (3, ic::LOCK_KEY, "Passwords")];
        if crate::modules::on("backup") {
            pages.push((4, ic::ARCHIVE, "Backup"));
        }
        pages.extend([(5, ic::PUZZLE_PIECE, "Extensions"), (6, ic::SQUARES_FOUR, "Parts"), (7, ic::INFO, "About")]);
        for (key, glyph, name) in pages {
            let on = s.page.key() == key;
            let (rect, r) = ui.allocate_exact_size(Vec2::new(width, 34.0), egui::Sense::click());
            if on {
                ui.painter().rect_filled(rect, theme::R_SM, selected_fill());
                // The page shown is marked with the app's blue along its left edge.
                let mark = Rect::from_min_size(rect.left_top() + Vec2::new(0.0, 9.0), Vec2::new(3.0, rect.height() - 18.0));
                ui.painter().rect_filled(mark, 1.5, brand());
            } else if r.hovered() {
                ui.painter().rect_filled(rect, theme::R_SM, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            let fg = if on { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
            ui.painter().text(rect.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, glyph, FontId::proportional(15.0), fg);
            ui.painter().text(rect.left_center() + Vec2::new(36.0, 0.0), Align2::LEFT_CENTER, name, FontId::proportional(13.5), fg);
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
        kit::title(ui, "Connections", "How screens and files reach a computer over SSH. A terminal is plain ssh and needs none of this.");
        kit::card(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            kit::row(ui, "Screen quality", |ui| {
                let current = self.form.preset.map_or("Auto (adapts to the connection)".into(), |i| preset_text(&PRESETS[i]));
                let before = self.form.preset;
                combo("quality", current).width(300.0).show_ui(ui, |ui| {
                    crate::gui::menu_look(ui);
                    ui.selectable_value(&mut self.form.preset, None, "Auto (adapts to the connection)");
                    for (i, p) in PRESETS.iter().enumerate() {
                        ui.selectable_value(&mut self.form.preset, Some(i), preset_text(p));
                    }
                });
                if self.form.preset != before {
                    crate::hosts::set_setting("preset", &self.form.preset.map(|i| i.to_string()).unwrap_or_default());
                }
            });
            // auto picks the capture for the computer's desktop.
            kit::row(ui, "Screen capture", |ui| {
                let test = cfg!(feature = "test-desktop").then_some("synthetic");
                let options: Vec<&str> = ["auto", "gnome", "windows"].into_iter().chain(test).collect();
                let mut i = options.iter().position(|o| *o == self.form.capture).unwrap_or(0);
                if kit::segmented(ui, &mut i, &options) {
                    self.form.capture = options[i].to_string();
                    crate::hosts::set_setting("capture", &self.form.capture);
                }
            });
            // How sshremote is started on the computer, over SSH.
            kit::row(ui, "Agent command", |ui| {
                if ui.add(kit::field(&mut self.form.remote, crate::options::REMOTE)).changed() {
                    crate::hosts::set_setting("agent_command", self.form.remote.trim());
                }
            });
            // Computers by ID: past the relay when a path through both routers is found.
            #[cfg(feature = "remote")]
            kit::row(ui, "By ID", |ui| {
                let mut on = crate::net::straight_paths();
                if ui.checkbox(&mut on, "Go straight to the computer when a path through the routers is found").on_hover_text("The relay carries the session until then, and when there is none. The computer learns this one's address.").changed() {
                    crate::hosts::set_setting("straight_paths", if on { "yes" } else { "no" });
                }
            });
            kit::row(ui, "Passwords", |ui| {
                let r = ui.add_enabled(crate::vault::backend().is_some(), egui::Checkbox::new(&mut self.form.remember, "Remember the passwords typed, in the system's password store"));
                if r.changed() {
                    crate::hosts::set_setting("remember_password", if self.form.remember { "yes" } else { "no" });
                }
            });
        });
    }

    /// The version, and on Windows the administrator rights.
    fn about_page(&mut self, ui: &mut egui::Ui) {
        kit::card(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(48.0), egui::Sense::hover());
                super::paint_logo(ui.painter(), r, brand());
                ui.vertical(|ui| {
                    ui.label(RichText::new("SSH Remote").size(18.0).strong());
                    ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).weak());
                });
            });
            ui.add_space(8.0);
            ui.label("Terminals, screens and files of your computers, over SSH or by a remote access ID.");
            ui.horizontal(|ui| {
                ui.hyperlink_to("sshremote.dev", "https://sshremote.dev");
                ui.hyperlink_to("Documentation", "https://sshremote.dev/docs.html");
            });
        });
        if cfg!(windows) {
            ui.add_space(kit::GAP);
            kit::heading(ui, "Administrator rights");
            kit::card(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(crate::admin::ADDS).size(12.5)).wrap());
                ui.add_space(6.0);
                if crate::admin::elevated() {
                    ui.label(RichText::new(format!("{}  Running as administrator", ic::SHIELD_CHECK)).color(good()));
                } else if ui.add(kit::secondary(format!("{}  Restart as administrator", ic::SHIELD_CHECK))).clicked() {
                    self.restart_elevated(ui.ctx());
                }
                #[cfg(windows)]
                {
                    let mut ask = crate::admin::ask_at_start();
                    if ui.checkbox(&mut ask, "Ask for administrator rights at start").changed() {
                        crate::admin::set_ask_at_start(ask);
                    }
                }
            });
        }
    }
}
