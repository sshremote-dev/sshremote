//! The window in front as a short list of elements an AI agent can read and
//! operate by number (accessibility: UI Automation on Windows, AT-SPI on Linux),
//! so it needs a screenshot only for what has no text.

#[cfg(not(any(windows, target_os = "linux")))]
use crate::proto::UiAction;

#[cfg(target_os = "linux")]
mod atspi;
#[cfg(windows)]
mod win;

#[cfg(target_os = "linux")]
pub use atspi::Ui;
#[cfg(windows)]
pub use win::Ui;

/// What is shared, as the capture knows it (Windows); GNOME pins its own when sharing starts.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Scope {
    pub window: Option<u64>,
    pub app: Option<String>,
}

/// What the agent still has to do after the accessibility side did its part.
pub enum Outcome {
    Done(String),
    /// Click with the pointer at captured pixels (button 1 left, 3 right).
    #[cfg_attr(not(windows), allow(dead_code))]
    ClickAt {
        x: f64,
        y: f64,
        button: u8,
        double: bool,
    },
    /// Type with the keyboard into the focused element, after selecting all of it if `replace`.
    Type {
        text: String,
        replace: bool,
    },
}

/// Short role names, so a list costs few tokens.
pub fn short_role(role: &str) -> String {
    let r = role.to_lowercase().replace([' ', '_', '-'], "");
    match r.as_str() {
        "pushbutton" | "togglebutton" | "splitbutton" => "button".into(),
        "entry" | "passwordtext" | "text" | "textbox" | "edit" => "edit".into(),
        "checkbox" | "checkmenuitem" => "checkbox".into(),
        "radiobutton" | "radiomenuitem" => "radio".into(),
        "combobox" => "combo".into(),
        "hyperlink" | "link" => "link".into(),
        "menuitem" => "menuitem".into(),
        "pagetab" | "tabitem" => "tab".into(),
        "listitem" => "item".into(),
        "treeitem" => "treeitem".into(),
        "spinbutton" | "spinner" => "spin".into(),
        "label" | "statictext" => "text".into(),
        "heading" | "header" => "heading".into(),
        "documentframe" | "document" | "documentweb" => "document".into(),
        "tablecell" | "dataitem" => "cell".into(),
        other => other.to_string(),
    }
}

/// Text cut to `n` characters, on one line.
pub fn clip(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= n { one } else { format!("{}...", one.chars().take(n).collect::<String>()) }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub struct Ui;

#[cfg(not(any(windows, target_os = "linux")))]
impl Ui {
    pub fn new() -> Self {
        Ui
    }
    pub fn pin(&mut self, _: u8) {}
    pub fn state(&mut self, _: (i32, i32), _: (usize, usize), _: &Scope, _: bool, _: usize) -> crate::proto::ToClient {
        crate::proto::ToClient::UiTree {
            title: String::new(),
            windows: Vec::new(),
            elems: Vec::new(),
            more: 0,
            note: "no accessibility support on this system".into(),
        }
    }
    pub fn act(&mut self, _: u64, _: UiAction, _: &str) -> Result<Outcome, String> {
        Err("no accessibility support on this system".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_and_text_stay_short() {
        assert_eq!(short_role("push button"), "button");
        assert_eq!(short_role("Hyperlink"), "link");
        assert_eq!(clip("a  b\nc", 10), "a b c");
        assert_eq!(clip("abcdefgh", 3), "abc...");
    }
}
