//! Windows: UI Automation, as screen readers and Windows-MCP use it.

use uiautomation::patterns::{
    UIExpandCollapsePattern, UIInvokePattern, UILegacyIAccessiblePattern, UISelectionItemPattern, UITogglePattern,
    UIValuePattern,
};
use uiautomation::types::{ControlType, Handle, TreeScope, UIProperty};
use uiautomation::variants::Variant;
use uiautomation::{UIAutomation, UIElement};
use windows_sys::Win32::Foundation::HWND;

use super::{Outcome, Scope, clip, short_role};
use crate::capture::winlist;
use crate::proto::{
    ToClient, UI_CHECKED, UI_DISABLED, UI_EDITABLE, UI_EXPANDED, UI_FOCUSED, UI_SELECTED, UiAction, UiElem,
};

pub struct Ui {
    auto: Option<UIAutomation>,
    /// The elements of the last listing, by number - 1, with their centre in captured pixels.
    elems: Vec<(UIElement, i32, i32)>,
    size: (usize, usize),
    /// A window or program is shared: only it is listed and operated.
    scoped: Option<Scope>,
}

fn interactive(ct: ControlType) -> bool {
    matches!(
        ct,
        ControlType::Button
            | ControlType::CheckBox
            | ControlType::ComboBox
            | ControlType::Edit
            | ControlType::Hyperlink
            | ControlType::ListItem
            | ControlType::MenuItem
            | ControlType::RadioButton
            | ControlType::Slider
            | ControlType::Spinner
            | ControlType::TabItem
            | ControlType::TreeItem
            | ControlType::SplitButton
            | ControlType::DataItem
            | ControlType::HeaderItem
            | ControlType::Document
    )
}

fn informative(ct: ControlType) -> bool {
    matches!(ct, ControlType::Text | ControlType::Header | ControlType::StatusBar | ControlType::Image)
}

fn err(e: uiautomation::Error) -> String {
    format!("UI Automation: {e}")
}

impl Ui {
    pub fn new() -> Self {
        Ui { auto: None, elems: Vec::new(), size: (0, 0), scoped: None }
    }

    /// Windows knows what is shared from the capture; nothing to remember here.
    pub fn pin(&mut self, _: u8) {}

    /// The window to list: the shared one, the shared program's front window, or the one in front.
    fn target(scope: &Scope) -> HWND {
        let front = winlist::foreground();
        if let Some(w) = scope.window {
            return w as HWND;
        }
        if let Some(exe) = &scope.app {
            if winlist::exe_of(winlist::pid_of(front)).eq_ignore_ascii_case(exe) {
                return front;
            }
            return winlist::top_window_of(exe).unwrap_or(front);
        }
        front
    }

    fn auto(&mut self) -> Result<&UIAutomation, String> {
        if self.auto.is_none() {
            // A thread already in another COM mode can still create the object.
            let a = UIAutomation::new().or_else(|_| UIAutomation::new_direct()).map_err(err)?;
            self.auto = Some(a);
        }
        Ok(self.auto.as_ref().unwrap())
    }

    pub fn state(
        &mut self,
        origin: (i32, i32),
        size: (usize, usize),
        scope: &Scope,
        text: bool,
        max: usize,
    ) -> ToClient {
        let hwnd = Self::target(scope);
        let title = winlist::title_of(hwnd);
        // While one window is shared there is nothing else to go to; a shared program offers its own windows.
        let windows: Vec<(u64, String)> = if scope.window.is_some() {
            Vec::new()
        } else {
            winlist::shareable()
                .into_iter()
                .filter(|w| w.id != hwnd as u64)
                .filter(|w| {
                    scope
                        .app
                        .as_ref()
                        .is_none_or(|exe| winlist::exe_of(winlist::pid_of(w.id as HWND)).eq_ignore_ascii_case(exe))
                })
                .take(20)
                .map(|w| (w.id, clip(&format!("{}: {}", w.app, w.title), 60)))
                .collect()
        };
        self.size = size;
        self.scoped = (scope.window.is_some() || scope.app.is_some())
            .then(|| Scope { window: scope.window, app: scope.app.clone() });
        let (elems, more, note) = match self.list(hwnd, origin, text, max) {
            Ok((e, m)) => (e, m, String::new()),
            Err(e) => (Vec::new(), 0, e),
        };
        ToClient::UiTree { title, windows, elems, more, note }
    }

    fn list(&mut self, hwnd: HWND, origin: (i32, i32), text: bool, max: usize) -> Result<(Vec<UiElem>, u32), String> {
        let auto = self.auto()?;
        let root = auto.element_from_handle(Handle::from(hwnd as isize)).map_err(err)?;
        let cache = auto.create_cache_request().map_err(err)?;
        for p in [
            UIProperty::Name,
            UIProperty::ControlType,
            UIProperty::BoundingRectangle,
            UIProperty::IsEnabled,
            UIProperty::IsOffscreen,
            UIProperty::IsKeyboardFocusable,
            UIProperty::HasKeyboardFocus,
            UIProperty::ValueValue,
            UIProperty::IsPassword,
            UIProperty::ToggleToggleState,
            UIProperty::SelectionItemIsSelected,
            UIProperty::ExpandCollapseExpandCollapseState,
        ] {
            cache.add_property(p).map_err(err)?;
        }
        let visible =
            auto.create_property_condition(UIProperty::IsOffscreen, Variant::from(false), None).map_err(err)?;
        let control =
            auto.create_property_condition(UIProperty::IsControlElement, Variant::from(true), None).map_err(err)?;
        let cond = auto.create_and_condition(visible, control).map_err(err)?;
        let found = root.find_all_build_cache(TreeScope::Descendants, &cond, &cache).map_err(err)?;
        let mut out = Vec::new();
        let mut kept = Vec::new();
        let mut more = 0u32;
        for e in found {
            let Ok(ct) = e.get_cached_control_type() else { continue };
            let name = e.get_cached_name().unwrap_or_default();
            let focusable = e.is_cached_keyboard_focusable().unwrap_or(false);
            let keep = interactive(ct)
                || (focusable && !name.trim().is_empty())
                || (text && informative(ct) && !name.trim().is_empty());
            if !keep {
                continue;
            }
            let Ok(r) = e.get_cached_bounding_rectangle() else { continue };
            if r.get_width() <= 0 || r.get_height() <= 0 {
                continue;
            }
            let cx = (r.get_left() + r.get_right()) / 2 - origin.0;
            let cy = (r.get_top() + r.get_bottom()) / 2 - origin.1;
            // Sharing part of the screen: what lies outside it is not offered.
            if self.scoped.is_some() && !self.on_screen(cx, cy) {
                continue;
            }
            if out.len() >= max {
                more += 1;
                continue;
            }
            let get_bool =
                |p: UIProperty| e.get_cached_property_value(p).ok().and_then(|v| v.try_into().ok()).unwrap_or(false);
            let get_i32 =
                |p: UIProperty| e.get_cached_property_value(p).ok().and_then(|v| v.try_into().ok()).unwrap_or(-1);
            let mut flags = 0;
            if get_bool(UIProperty::HasKeyboardFocus) {
                flags |= UI_FOCUSED;
            }
            if !e.is_cached_enabled().unwrap_or(true) {
                flags |= UI_DISABLED;
            }
            if get_i32(UIProperty::ToggleToggleState) == 1 {
                flags |= UI_CHECKED;
            }
            if get_bool(UIProperty::SelectionItemIsSelected) {
                flags |= UI_SELECTED;
            }
            if get_i32(UIProperty::ExpandCollapseExpandCollapseState) == 1 {
                flags |= UI_EXPANDED;
            }
            let mut value = String::new();
            if matches!(ct, ControlType::Edit | ControlType::ComboBox | ControlType::Document | ControlType::Spinner) {
                flags |= UI_EDITABLE;
                if !get_bool(UIProperty::IsPassword) {
                    value = e
                        .get_cached_property_value(UIProperty::ValueValue)
                        .ok()
                        .and_then(|v| TryInto::<String>::try_into(v).ok())
                        .unwrap_or_default();
                }
            }
            let id = out.len() as u32 + 1;
            out.push(UiElem {
                id,
                role: match ct {
                    // UI Automation's Text is a label, not an input field.
                    ControlType::Text => "text".into(),
                    ControlType::Header => "heading".into(),
                    other => short_role(&format!("{other:?}")),
                },
                name: clip(&name, 80),
                value: clip(&value, 80),
                x: cx,
                y: cy,
                flags,
            });
            kept.push((e, cx, cy));
        }
        self.elems = kept;
        Ok((out, more))
    }

    /// A point inside the captured picture, where the pointer can click.
    fn on_screen(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as usize) < self.size.0 && (y as usize) < self.size.1
    }

    pub fn act(&mut self, target: u64, act: UiAction, text: &str) -> Result<Outcome, String> {
        if act == UiAction::FocusWindow {
            let hwnd = target as HWND;
            if !winlist::exists(hwnd) {
                return Err("no such window; call state again".into());
            }
            if let Some(s) = &self.scoped {
                let same_app =
                    s.app.as_ref().is_some_and(|exe| winlist::exe_of(winlist::pid_of(hwnd)).eq_ignore_ascii_case(exe));
                if !same_app {
                    return Err("only the shared window or program can be used; share the whole screen first".into());
                }
            }
            winlist::raise(hwnd);
            return Ok(Outcome::Done(format!("brought \"{}\" to the front", winlist::title_of(hwnd))));
        }
        let (e, x, y) =
            self.elems.get((target as usize).wrapping_sub(1)).cloned().ok_or("no such element; call state again")?;
        let here = self.on_screen(x, y);
        if self.scoped.is_some() && !here {
            return Err("that control is outside the shared window; share the whole screen first".into());
        }
        match act {
            UiAction::Click | UiAction::DoubleClick | UiAction::RightClick if here => Ok(Outcome::ClickAt {
                x: x as f64,
                y: y as f64,
                button: if act == UiAction::RightClick { 3 } else { 1 },
                double: act == UiAction::DoubleClick,
            }),
            UiAction::Click => {
                // Not in the picture (scrolled away or another monitor): operate it directly.
                if let Ok(p) = e.get_pattern::<UIInvokePattern>() {
                    p.invoke().map_err(err)?;
                } else if let Ok(p) = e.get_pattern::<UITogglePattern>() {
                    p.toggle().map_err(err)?;
                } else if let Ok(p) = e.get_pattern::<UISelectionItemPattern>() {
                    p.select().map_err(err)?;
                } else if let Ok(p) = e.get_pattern::<UIExpandCollapsePattern>() {
                    p.expand().map_err(err)?;
                } else if let Ok(p) = e.get_pattern::<UILegacyIAccessiblePattern>() {
                    p.do_default_action().map_err(err)?;
                } else {
                    return Err("the element is off screen and has no action".into());
                }
                Ok(Outcome::Done("activated".into()))
            }
            UiAction::DoubleClick | UiAction::RightClick => Err("the element is not in the picture".into()),
            UiAction::Focus => {
                e.set_focus().map_err(err)?;
                Ok(Outcome::Done("focused".into()))
            }
            UiAction::SetText => {
                let _ = e.set_focus();
                if let Ok(p) = e.get_pattern::<UIValuePattern>()
                    && p.set_value(text).is_ok()
                {
                    return Ok(Outcome::Done("text set".into()));
                }
                Ok(Outcome::Type { text: text.to_string(), replace: true })
            }
            UiAction::Type => {
                e.set_focus().map_err(err)?;
                Ok(Outcome::Type { text: text.to_string(), replace: false })
            }
            UiAction::FocusWindow => unreachable!(),
        }
    }
}
