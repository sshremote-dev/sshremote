//! Linux: AT-SPI, the accessibility bus that screen readers such as Orca use.
//! On Wayland it does not reveal screen positions, so elements are operated
//! through their own actions and text interfaces instead of pointer clicks.

use std::time::Duration;

use zbus::blocking::{Connection, Proxy, connection};
use zbus::zvariant::OwnedObjectPath;

use super::{Outcome, Scope, clip, short_role};
use crate::proto::{
    ToClient, UI_CHECKED, UI_DISABLED, UI_EDITABLE, UI_EXPANDED, UI_FOCUSED, UI_SELECTED, UiAction, UiElem,
};

/// An accessible object: its bus name and object path.
type Ref = (String, OwnedObjectPath);

const ACC: &str = "org.a11y.atspi.Accessible";
// AT-SPI state bits.
const ACTIVE: u64 = 1 << 1;
const CHECKED: u64 = 1 << 4;
const EDITABLE: u64 = 1 << 7;
const ENABLED: u64 = 1 << 8;
const EXPANDED: u64 = 1 << 10;
const FOCUSABLE: u64 = 1 << 11;
const FOCUSED: u64 = 1 << 12;
const SELECTED: u64 = 1 << 23;
const SENSITIVE: u64 = 1 << 24;
const SHOWING: u64 = 1 << 25;
/// Nodes looked at in one listing, so a huge web page cannot stall the agent.
const VISIT_LIMIT: usize = 4000;

pub struct Ui {
    conn: Option<Connection>,
    elems: Vec<Ref>,
    windows: Vec<Ref>,
    /// Applications were asked to expose their content (Chrome and Electron wait for that).
    enabled: bool,
    /// While a window or program is shared: that window, or that application, as it was in front then.
    pinned: Option<(Ref, Option<Ref>)>,
}

fn err(e: zbus::Error) -> String {
    format!("accessibility bus: {e}")
}

fn interactive(role: &str) -> bool {
    matches!(
        role,
        "push button"
            | "button"
            | "menu button"
            | "switch"
            | "toggle button"
            | "check box"
            | "radio button"
            | "combo box"
            | "entry"
            | "password text"
            | "link"
            | "menu item"
            | "check menu item"
            | "radio menu item"
            | "page tab"
            | "list item"
            | "tree item"
            | "spin button"
            | "slider"
            | "table cell"
            | "terminal"
            | "editbar"
            | "autocomplete"
    )
}

fn informative(role: &str) -> bool {
    matches!(role, "label" | "heading" | "paragraph" | "static" | "status bar" | "caption" | "text")
}

impl Ui {
    pub fn new() -> Self {
        Ui { conn: None, elems: Vec::new(), windows: Vec::new(), enabled: false, pinned: None }
    }

    /// Follows what is shared: 1 and 3 a window, 2 and 4 a program, 0 the whole screen.
    pub fn pin(&mut self, kind: u8) {
        self.pinned = None;
        if kind == 0 {
            return;
        }
        let Ok(conn) = self.conn().cloned() else { return };
        let root: Ref =
            ("org.a11y.atspi.Registry".into(), OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root").unwrap());
        for app in Self::children(&conn, &root) {
            for frame in Self::children(&conn, &app) {
                if Self::states(&conn, &frame) & ACTIVE != 0 {
                    let window = matches!(kind, 1 | 3).then_some(frame);
                    self.pinned = Some((app, window));
                    return;
                }
            }
        }
    }

    fn conn(&mut self) -> Result<&Connection, String> {
        if self.conn.is_none() {
            let session = Connection::session().map_err(err)?;
            let bus = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus").map_err(err)?;
            if !self.enabled {
                // Toolkits such as Chrome's only publish their content once assistive technology is on.
                if let Ok(status) = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Status") {
                    let _ = status.set_property("IsEnabled", true);
                }
                self.enabled = true;
            }
            let addr: String = bus.call("GetAddress", &()).map_err(err)?;
            let conn = connection::Builder::address(addr.as_str())
                .map_err(err)?
                .method_timeout(Duration::from_secs(2))
                .build()
                .map_err(err)?;
            self.conn = Some(conn);
        }
        Ok(self.conn.as_ref().unwrap())
    }

    fn proxy(conn: &Connection, r: &Ref, iface: &'static str) -> Result<Proxy<'static>, String> {
        Proxy::new(conn, r.0.clone(), r.1.clone(), iface).map_err(err)
    }

    fn children(conn: &Connection, r: &Ref) -> Vec<Ref> {
        Self::proxy(conn, r, ACC).and_then(|p| p.call("GetChildren", &()).map_err(err)).unwrap_or_default()
    }

    fn role(conn: &Connection, r: &Ref) -> String {
        Self::proxy(conn, r, ACC).and_then(|p| p.call("GetRoleName", &()).map_err(err)).unwrap_or_default()
    }

    fn states(conn: &Connection, r: &Ref) -> u64 {
        let s: Vec<u32> =
            Self::proxy(conn, r, ACC).and_then(|p| p.call("GetState", &()).map_err(err)).unwrap_or_default();
        s.first().copied().unwrap_or(0) as u64 | (s.get(1).copied().unwrap_or(0) as u64) << 32
    }

    fn name(conn: &Connection, r: &Ref) -> String {
        Self::proxy(conn, r, ACC).and_then(|p| p.get_property::<String>("Name").map_err(err)).unwrap_or_default()
    }

    fn text(conn: &Connection, r: &Ref) -> String {
        Self::proxy(conn, r, "org.a11y.atspi.Text")
            .and_then(|p| p.call::<_, _, String>("GetText", &(0i32, 200i32)).map_err(err))
            .unwrap_or_default()
    }

    pub fn state(&mut self, _: (i32, i32), _: (usize, usize), _: &Scope, text: bool, max: usize) -> ToClient {
        match self.list(text, max) {
            Ok(t) => t,
            Err(note) => {
                ToClient::UiTree { title: String::new(), windows: Vec::new(), elems: Vec::new(), more: 0, note }
            }
        }
    }

    fn list(&mut self, text: bool, max: usize) -> Result<ToClient, String> {
        let conn = self.conn()?.clone();
        let root: Ref =
            ("org.a11y.atspi.Registry".into(), OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root").unwrap());
        // Every application's top-level windows; the active one is listed in full.
        let mut active: Option<Ref> = None;
        let mut windows = Vec::new();
        let pinned = self.pinned.clone();
        for app in Self::children(&conn, &root) {
            // The shell's own panels and overlays are not application windows.
            if Self::name(&conn, &app) == "gnome-shell" {
                continue;
            }
            // Sharing a program: only its windows.
            if pinned.as_ref().is_some_and(|p| p.0 != app) {
                continue;
            }
            for frame in Self::children(&conn, &app) {
                let st = Self::states(&conn, &frame);
                if st & ACTIVE != 0 && active.is_none() {
                    active = Some(frame.clone());
                } else if st & SHOWING != 0 {
                    windows.push(frame);
                }
            }
        }
        // Nothing marked active (no focus yet): the most recently opened window on screen.
        if active.is_none() {
            active = windows.pop();
        }
        // Sharing one window: that window, and nothing else to go to.
        if let Some((_, Some(w))) = &pinned {
            active = Some(w.clone());
            windows.clear();
        }
        let names: Vec<(u64, String)> = windows
            .iter()
            .take(20)
            .enumerate()
            .map(|(i, w)| (i as u64 + 1, clip(&Self::name(&conn, w), 60)))
            .filter(|(_, n)| !n.is_empty())
            .collect();
        self.windows = windows;
        let Some(frame) = active else {
            return Ok(ToClient::UiTree {
                title: String::new(),
                windows: names,
                elems: Vec::new(),
                more: 0,
                note: "no window in front publishes accessibility information".into(),
            });
        };
        let title = Self::name(&conn, &frame);
        let mut elems = Vec::new();
        let mut kept = Vec::new();
        let mut more = 0u32;
        let mut visited = 0;
        let mut stack = vec![frame];
        while let Some(node) = stack.pop() {
            visited += 1;
            if visited > VISIT_LIMIT {
                break;
            }
            let st = Self::states(&conn, &node);
            if st & SHOWING == 0 && visited > 1 {
                // Hidden (a closed menu, another tab): nothing below it shows either.
                continue;
            }
            let role = Self::role(&conn, &node);
            let editable = st & EDITABLE != 0;
            let take = interactive(&role) || (editable && role != "document web") || (text && informative(&role));
            if take {
                let name = Self::name(&conn, &node);
                let value = if editable || role == "entry" || role == "terminal" {
                    Self::text(&conn, &node)
                } else {
                    String::new()
                };
                let label = role != "table cell" || !name.is_empty() || st & FOCUSABLE != 0;
                // A button wrapping the real, focusable one is listed once, through the inner one.
                let wrapper =
                    role.contains("button") && st & FOCUSABLE == 0 && !Self::children(&conn, &node).is_empty();
                if label && !wrapper && (!name.trim().is_empty() || !value.is_empty() || interactive(&role) || editable)
                {
                    if elems.len() >= max {
                        more += 1;
                    } else {
                        let mut flags = 0;
                        for (bit, f) in [
                            (FOCUSED, UI_FOCUSED),
                            (CHECKED, UI_CHECKED),
                            (SELECTED, UI_SELECTED),
                            (EXPANDED, UI_EXPANDED),
                        ] {
                            if st & bit != 0 {
                                flags |= f;
                            }
                        }
                        if st & (ENABLED | SENSITIVE) == 0 {
                            flags |= UI_DISABLED;
                        }
                        if editable {
                            flags |= UI_EDITABLE;
                        }
                        let value = if role == "password text" { String::new() } else { value };
                        elems.push(UiElem {
                            id: elems.len() as u32 + 1,
                            role: short_role(&role),
                            name: clip(&name, 80),
                            value: clip(&value, 80),
                            x: -1,
                            y: -1,
                            flags,
                        });
                        kept.push(node.clone());
                    }
                }
            }
            // Children in reverse, so the list keeps the visual order.
            let mut kids = Self::children(&conn, &node);
            kids.reverse();
            stack.extend(kids);
        }
        self.elems = kept;
        let note = if visited > VISIT_LIMIT {
            "only the first part of a very large window was read".into()
        } else {
            String::new()
        };
        Ok(ToClient::UiTree { title, windows: names, elems, more, note })
    }

    pub fn act(&mut self, target: u64, act: UiAction, text: &str) -> Result<Outcome, String> {
        let conn = self.conn()?.clone();
        if act == UiAction::FocusWindow {
            let w = self
                .windows
                .get((target as usize).wrapping_sub(1))
                .cloned()
                .ok_or("no such window; call state again")?;
            let ok: bool = Self::proxy(&conn, &w, "org.a11y.atspi.Component")
                .and_then(|p| p.call("GrabFocus", &()).map_err(err))
                .unwrap_or(false);
            return if ok {
                Ok(Outcome::Done("asked the window to come forward".into()))
            } else {
                Err("GNOME does not let programs raise windows; click the window instead".into())
            };
        }
        let e =
            self.elems.get((target as usize).wrapping_sub(1)).cloned().ok_or("no such element; call state again")?;
        let focus = || -> bool {
            Self::proxy(&conn, &e, "org.a11y.atspi.Component")
                .and_then(|p| p.call("GrabFocus", &()).map_err(err))
                .unwrap_or(false)
        };
        match act {
            UiAction::Click => {
                let action = Self::proxy(&conn, &e, "org.a11y.atspi.Action")?;
                let n: i32 = action.get_property("NActions").unwrap_or(0);
                if n > 0 {
                    let _: bool = action.call("DoAction", &(0i32,)).map_err(err)?;
                    return Ok(Outcome::Done("activated".into()));
                }
                if focus() {
                    return Ok(Outcome::Done("focused (it has no click action)".into()));
                }
                Err("the element has no action; take a screenshot and click it by position".into())
            }
            UiAction::DoubleClick | UiAction::RightClick => {
                Err("GNOME does not tell where elements are; take a screenshot and click by position".into())
            }
            UiAction::Focus => {
                if focus() {
                    Ok(Outcome::Done("focused".into()))
                } else {
                    Err("the element cannot take the focus".into())
                }
            }
            UiAction::SetText => {
                let set: bool = Self::proxy(&conn, &e, "org.a11y.atspi.EditableText")
                    .and_then(|p| p.call("SetTextContents", &(text,)).map_err(err))
                    .unwrap_or(false);
                if set {
                    return Ok(Outcome::Done("text set".into()));
                }
                focus();
                Ok(Outcome::Type { text: text.to_string(), replace: true })
            }
            UiAction::Type => {
                focus();
                Ok(Outcome::Type { text: text.to_string(), replace: false })
            }
            UiAction::FocusWindow => unreachable!(),
        }
    }
}
