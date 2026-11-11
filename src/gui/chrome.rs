//! Window frames drawn by the program, as Windows Terminal and browsers draw theirs: the title bar
//! holds the window's own content (tabs, a toolbar) beside the minimize, maximize and close
//! buttons, and keeps what the system's title bar does: dragging moves the window (with snapping),
//! a double click maximizes or restores it, the icon, a right click or Alt+Space open the system
//! menu, and the edges resize it.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2, ViewportCommand};

/// Height of a title bar, as in Windows 11 apps with tabs.
pub const HEIGHT: f32 = 40.0;
/// Width of a caption button.
const BUTTON: f32 = 46.0;
/// Width of the edge that resizes the window.
const EDGE: f32 = 6.0;

/// A window of one size (the open window): no resizing edges, no maximize, no full screen.
pub fn set_fixed(ctx: &egui::Context, fixed: bool) {
    let key = egui::Id::unique(("chrome-fixed", ctx.viewport_id()));
    ctx.data_mut(|d| d.insert_temp(key, fixed));
}

fn fixed(ctx: &egui::Context) -> bool {
    let key = egui::Id::unique(("chrome-fixed", ctx.viewport_id()));
    ctx.data(|d| d.get_temp::<bool>(key)).unwrap_or(false)
}

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false))
}

/// The largest window side in pixels the GPU draws, as the renderer found it.
pub static MAX_SIDE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(8192);

/// The colour of title bars and tab strips, a shade darker than the panels.
pub fn bar_fill(v: &egui::Visuals) -> Color32 {
    v.panel_fill.lerp_to_gamma(Color32::BLACK, if v.dark_mode { 0.25 } else { 0.04 })
}

/// Edges that resize the window and Alt+Space for the system menu; call once per frame of every
/// window that draws its own frame, before its content.
pub fn behaviour(ctx: &egui::Context) {
    // A new window is set up in its first frames, not looked at on every frame after.
    let key = egui::Id::unique(("chrome-new", ctx.viewport_id()));
    let seen = ctx.data(|d| d.get_temp::<u8>(key)).unwrap_or(0);
    if seen < 3 {
        #[cfg(windows)]
        win::round_corners();
        if seen == 0 {
            // No larger than the GPU can draw: past it the renderer gives up.
            let most = MAX_SIDE.load(std::sync::atomic::Ordering::Relaxed) as f32 / ctx.pixels_per_point();
            ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(egui::Vec2::splat(most)));
        }
        ctx.data_mut(|d| d.insert_temp(key, seen + 1));
    }
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::Space)) {
        system_menu(ctx, Pos2::new(8.0, HEIGHT));
    }
    super::look::frame(ctx);
    if maximized(ctx) || fixed(ctx) {
        return;
    }
    let screen = ctx.content_rect();
    let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    use egui::viewport::ResizeDirection as D;
    let (l, r) = (p.x - screen.left() < EDGE, screen.right() - p.x < EDGE);
    let (t, b) = (p.y - screen.top() < EDGE, screen.bottom() - p.y < EDGE);
    let dir = match (l, r, t, b) {
        (true, _, true, _) => Some(D::NorthWest),
        (_, true, true, _) => Some(D::NorthEast),
        (true, _, _, true) => Some(D::SouthWest),
        (_, true, _, true) => Some(D::SouthEast),
        (true, ..) => Some(D::West),
        (_, true, ..) => Some(D::East),
        (_, _, true, _) => Some(D::North),
        (.., true) => Some(D::South),
        _ => None,
    };
    let Some(dir) = dir else { return };
    let icon = match dir {
        D::West | D::East => egui::CursorIcon::ResizeHorizontal,
        D::North | D::South => egui::CursorIcon::ResizeVertical,
        D::NorthWest | D::SouthEast => egui::CursorIcon::ResizeNwSe,
        D::NorthEast | D::SouthWest => egui::CursorIcon::ResizeNeSw,
    };
    ctx.set_cursor_icon(icon);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}

/// Whether `p` is on the edge that resizes the window (not while it is maximized).
pub fn on_edge(ctx: &egui::Context, p: Pos2) -> bool {
    let r = ctx.content_rect();
    !maximized(ctx)
        && !fixed(ctx)
        && (p.x - r.left() < EDGE || r.right() - p.x < EDGE || p.y - r.top() < EDGE || r.bottom() - p.y < EDGE)
}

/// What the title bar asks for besides moving the window.
#[derive(Default)]
pub struct BarResponse {
    /// The close button was pressed; the window decides (it may ask first).
    pub close: bool,
    /// In full screen the middle button leaves it; the window decides how (its own state).
    pub leave_full: bool,
}

/// A title bar across the top of `ui`: the app's icon, then `content` (tabs, a title, a toolbar)
/// laid out left to right, then the caption buttons. Empty space in it moves the window.
pub fn title_bar(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) -> BarResponse {
    let ctx = ui.ctx().clone();
    let mut out = BarResponse::default();
    let fill = bar_fill(ui.visuals());
    let frame = egui::Frame::new().fill(fill).inner_margin(egui::Margin::ZERO);
    egui::Panel::top("chrome-title-bar").exact_size(HEIGHT).frame(frame).show_separator_line(false).show(ui, |ui| {
        let bar = ui.max_rect();
        // The bar itself first, so the widgets on it take their own clicks.
        let back = ui.interact(bar, ui.scope_id().with("chrome-drag"), Sense::click_and_drag());
        if back.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if back.double_clicked() && !fixed(&ctx) {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized(&ctx)));
        }
        if back.secondary_clicked()
            && let Some(p) = back.interact_pointer_pos()
        {
            system_menu(&ctx, p);
        }
        let buttons = Rect::from_min_max(Pos2::new(bar.right() - 3.0 * BUTTON, bar.top()), bar.max);
        // The icon opens the system menu, as the system's own title bar does.
        let icon_rect = Rect::from_min_size(bar.min, Vec2::new(40.0, HEIGHT));
        let icon = ui.interact(icon_rect, ui.scope_id().with("chrome-icon"), Sense::click());
        super::paint_logo(ui.painter(), Rect::from_center_size(icon_rect.center(), Vec2::splat(18.0)), ui.visuals().text_color());
        if icon.clicked() {
            system_menu(&ctx, Pos2::new(icon_rect.left(), icon_rect.bottom()));
        }
        let middle =
            Rect::from_min_max(Pos2::new(icon_rect.right(), bar.top()), Pos2::new(buttons.left() - 8.0, bar.bottom()));
        let mut child = ui.new_child(
            egui::UiBuilder::new().max_rect(middle).layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        child.set_clip_rect(middle);
        content(&mut child);
        let (close, leave) = caption_buttons(ui, buttons);
        out.close |= close;
        out.leave_full |= leave;
    });
    out
}

/// Minimize, maximize or restore, and close, drawn as Windows 11 draws them. True for close.
fn caption_buttons(ui: &mut egui::Ui, rect: Rect) -> (bool, bool) {
    let ctx = ui.ctx().clone();
    let fg = ui.visuals().text_color();
    let max = maximized(&ctx);
    // In full screen the middle button leaves full screen (maximizing means nothing there).
    let full = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
    let mut leave = false;
    let one_size = fixed(&ctx);
    let mut close = false;
    let middle = if full { "Leave full screen (F11; Ctrl+Alt+Enter in a shared screen)" } else if max { "Restore Down" } else { "Maximize" };
    for (k, tip) in ["Minimize", middle, "Close"].into_iter().enumerate() {
        // A window of one size has no maximize: minimize moves over beside close.
        if one_size && k == 1 {
            continue;
        }
        let slot = if one_size && k == 0 { 1 } else { k };
        let r = Rect::from_min_size(Pos2::new(rect.left() + slot as f32 * BUTTON, rect.top()), Vec2::new(BUTTON, HEIGHT));
        let resp = ui.interact(r, ui.scope_id().with(("caption", k)), Sense::click()).on_hover_text(tip);
        let hot = resp.hovered();
        let (bg, color) = match (k, hot, resp.is_pointer_button_down_on()) {
            (2, true, _) => (Color32::from_rgb(196, 43, 28), Color32::WHITE),
            (_, true, true) => (fg.gamma_multiply(0.14), fg),
            (_, true, false) => (fg.gamma_multiply(0.08), fg),
            _ => (Color32::TRANSPARENT, fg),
        };
        let p = ui.painter();
        p.rect_filled(r, CornerRadius::ZERO, bg);
        let c = r.center();
        let s = Stroke::new(1.0, color);
        match k {
            0 => {
                p.hline((c.x - 5.0)..=(c.x + 5.0), c.y, s);
            }
            1 if max => {
                p.rect_stroke(
                    Rect::from_min_size(Pos2::new(c.x - 5.0, c.y - 3.0), Vec2::splat(8.0)),
                    1,
                    s,
                    egui::StrokeKind::Inside,
                );
                p.line_segment([Pos2::new(c.x - 3.0, c.y - 5.0), Pos2::new(c.x + 5.0, c.y - 5.0)], s);
                p.line_segment([Pos2::new(c.x + 5.0, c.y - 5.0), Pos2::new(c.x + 5.0, c.y + 3.0)], s);
            }
            1 => {
                p.rect_stroke(Rect::from_center_size(c, Vec2::splat(10.0)), 1, s, egui::StrokeKind::Inside);
            }
            _ => {
                p.line_segment([c + Vec2::new(-5.0, -5.0), c + Vec2::new(5.0, 5.0)], s);
                p.line_segment([c + Vec2::new(5.0, -5.0), c + Vec2::new(-5.0, 5.0)], s);
            }
        }
        if resp.clicked() {
            match k {
                0 => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                1 if full => leave = true,
                1 => ctx.send_viewport_cmd(ViewportCommand::Maximized(!max)),
                _ => close = true,
            }
        }
    }
    (close, leave)
}

/// The system menu (Restore, Move, Size, Minimize, Maximize, Close) at `at`, in window points.
pub fn system_menu(ctx: &egui::Context, at: Pos2) {
    #[cfg(windows)]
    win::system_menu(at * ctx.pixels_per_point());
    #[cfg(not(windows))]
    let _ = (ctx, at);
}

#[cfg(windows)]
mod win {
    use std::collections::HashSet;
    use std::sync::Mutex;

    use eframe::egui::Pos2;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT};
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnableMenuItem, EnumThreadWindows, GetSystemMenu, IsZoomed, MF_BYCOMMAND, MF_ENABLED, MF_GRAYED, PostMessageW,
        SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE, SC_MOVE, SC_RESTORE, SC_SIZE, SetMenuDefaultItem, TPM_RETURNCMD,
        TPM_RIGHTBUTTON, TrackPopupMenu, WM_SYSCOMMAND,
    };

    pub fn system_menu(at: Pos2) {
        unsafe {
            let hwnd = GetActiveWindow();
            if hwnd.is_null() {
                return;
            }
            let mut p = POINT { x: at.x as i32, y: at.y as i32 };
            ClientToScreen(hwnd, &mut p);
            let menu = GetSystemMenu(hwnd, 0);
            if menu.is_null() {
                return;
            }
            let zoomed = IsZoomed(hwnd) != 0;
            let on = |b: bool| MF_BYCOMMAND | if b { MF_ENABLED } else { MF_GRAYED };
            EnableMenuItem(menu, SC_RESTORE, on(zoomed));
            EnableMenuItem(menu, SC_MOVE, on(!zoomed));
            EnableMenuItem(menu, SC_SIZE, on(!zoomed));
            EnableMenuItem(menu, SC_MINIMIZE, on(true));
            EnableMenuItem(menu, SC_MAXIMIZE, on(!zoomed));
            EnableMenuItem(menu, SC_CLOSE, on(true));
            SetMenuDefaultItem(menu, SC_CLOSE, 0);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, 0, hwnd, std::ptr::null());
            if cmd != 0 {
                PostMessageW(hwnd, WM_SYSCOMMAND, cmd as usize, 0);
            }
        }
    }

    static DONE: Mutex<Option<HashSet<usize>>> = Mutex::new(None);

    /// Windows 11 rounds and shadows the corners of windows without a system frame when asked.
    pub fn round_corners() {
        unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> i32 {
            let mut done = DONE.lock().unwrap();
            if done.get_or_insert_with(HashSet::new).insert(hwnd as usize) {
                // DWMWA_WINDOW_CORNER_PREFERENCE = 33, DWMWCP_ROUNDSMALL = 3.
                let round: u32 = 3;
                unsafe { DwmSetWindowAttribute(hwnd, 33, &round as *const u32 as _, 4) };
            }
            1
        }
        unsafe { EnumThreadWindows(GetCurrentThreadId(), Some(each), 0) };
    }
}
