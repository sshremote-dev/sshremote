//! The parts of the app beyond the terminal workspace, each of which can be turned off: a part
//! that is off leaves the menus and its commands, and the terminal, text and Markdown views stay.

/// Its id in the settings (`module.ID=off`), name and what it does.
pub const ALL: &[(&str, &str, &str)] = &[
    ("screen", "Screen sharing", "See and operate a host's screen"),
    ("files", "Files", "The file manager on both sides"),
    ("chat", "Chat", "Talk with the person at a shared screen"),
    ("sound", "Sound", "Hear a shared screen's sound"),
    ("backup", "Backup and restore", "Hosts, settings and ~/.ssh, by hand or each day"),
    ("mcp", "MCP server", "`sshremote mcp`: hosts' screens for AI agents"),
    ("web", "Web gateway", "`sshremote web`: terminals and screens in a browser"),
];

pub fn on(id: &str) -> bool {
    #[cfg(test)]
    if let Some(off) = TEST_OFF.with(|o| o.borrow().clone()) {
        return !off.iter().any(|x| x == id);
    }
    crate::hosts::setting(&format!("module.{id}")).as_deref() != Some("off")
}

pub fn set(id: &str, on: bool) {
    crate::hosts::set_setting(&format!("module.{id}"), if on { "on" } else { "off" });
}

/// An error for a command whose part is off.
pub fn need(id: &str) -> Result<(), String> {
    if on(id) {
        return Ok(());
    }
    let name = ALL.iter().find(|m| m.0 == id).map_or(id, |m| m.1);
    Err(format!("{name} is turned off: `sshremote module on {id}`, or Tools > Extensions > Modules"))
}

#[cfg(test)]
thread_local!(pub static TEST_OFF: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) });

/// `sshremote module [list | on ID | off ID]`
pub fn cli(args: &[String]) -> Result<(), String> {
    let known = |id: &str| ALL.iter().any(|m| m.0 == id).then_some(()).ok_or(format!("no module {id}"));
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("list") | None, _) => {
            for (id, name, what) in ALL {
                println!("{id:8} {:3}  {name}: {what}", if on(id) { "on" } else { "off" });
            }
            Ok(())
        }
        (Some(v @ ("on" | "off")), Some(id)) => {
            known(id)?;
            set(id, v == "on");
            Ok(())
        }
        _ => Err("usage: sshremote module [list | on ID | off ID]".into()),
    }
}

/// The modules part of the Extensions page.
pub fn ui(ui: &mut eframe::egui::Ui) {
    for (id, name, what) in ALL {
        let mut v = on(id);
        if ui.checkbox(&mut v, *name).on_hover_text(*what).changed() {
            set(id, v);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn off_parts_refuse() {
        super::TEST_OFF.with(|o| *o.borrow_mut() = Some(vec!["web".into()]));
        assert!(super::need("web").unwrap_err().contains("Web gateway is turned off"));
        assert!(super::need("mcp").is_ok() && super::on("screen"));
        super::TEST_OFF.with(|o| *o.borrow_mut() = None);
    }
}
