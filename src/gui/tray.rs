//! The icon in the notification area. It stands for SSH Remote while its window is closed and
//! remote access by ID or tunnels go on (a click brings the window back), and, as a program of its
//! own (`sshremote --tray`, started with every sign-in once the service is installed), for the
//! service: its menu turns remote access on or off, copies the ID, sets who comes in and how,
//! shows the window of whoever is connected, works the service and quits.
//!
//! Its thread does all of that itself: a hidden window gets no frames, so nothing may wait for one.
//! Windows only: a Wayland session lets no program hide its window and bring it back, and on
//! Linux the systemd user service keeps remote access up with no window.

/// What the menu shows, as the window (or the service) last told.
#[derive(Clone, Default)]
pub(super) struct Status {
    /// Remote access by ID: whether it is on, and the ID.
    pub access: Option<(bool, String)>,
    /// The service: installed, running.
    pub service: Option<(bool, bool)>,
    /// Who comes in and how: asked first, straight through the routers, found on this network,
    /// straight in from it, asking without a password.
    pub settings: Option<[bool; 5]>,
    /// The language the menu speaks: the window may change it while the icon stays (elsewhere
    /// the menu is made again as it opens).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub lang: &'static str,
    /// Viewers connected by the ID now.
    #[cfg(feature = "remote")]
    pub connected: Vec<crate::access::Viewer>,
    /// The icon's own program: Quit hides the icon, and changes go through the administrator
    /// prompt (the settings are the administrators').
    #[cfg_attr(not(windows), allow(dead_code))]
    pub alone: bool,
}

pub(super) type Shared = std::sync::Arc<std::sync::Mutex<Status>>;

/// Set when the menu's Quit was chosen: the window then closes for good.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) static QUIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Set when the menu changed remote access's settings: the window reads them again.
pub(super) static CHANGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
pub(super) use win::Tray;

/// The menu's choices, by their item numbers.
#[cfg_attr(not(windows), allow(dead_code))]
mod item {
    pub const OPEN: usize = 1;
    pub const ACCESS: usize = 2;
    pub const COPY_ID: usize = 3;
    pub const ASK: usize = 4;
    pub const STRAIGHT: usize = 5;
    pub const NEARBY: usize = 6;
    pub const DIRECT: usize = 7;
    pub const SETTINGS: usize = 8;
    pub const SHOW: usize = 9;
    pub const INSTALL: usize = 10;
    pub const START: usize = 11;
    pub const STOP: usize = 12;
    pub const REMOVE: usize = 13;
    pub const REQUESTS: usize = 14;
    /// Ending the connection of the viewer listed n-th, from here.
    pub const END: usize = 200;
    pub const END_MOST: usize = 50;
    pub const QUIT: usize = 99;
}

/// A line of the menu, the same on every system: a choice (its item number; 0 for a line that
/// only says something), a separator, or a submenu.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub(super) enum Entry {
    Item { id: usize, label: String, enabled: bool, checked: Option<bool> },
    Separator,
    Sub { label: String, entries: Vec<Entry> },
}

/// The menu for `s`.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub(super) fn entries(s: &Status) -> Vec<Entry> {
    let it = |id: usize, label: &str, enabled: bool, checked: Option<bool>| Entry::Item { id, label: label.to_string(), enabled, checked };
    let mut m = vec![it(item::OPEN, t!("Open SSH Remote"), true, None), Entry::Separator];
    match &s.access {
        Some((on, id)) => {
            m.push(it(item::ACCESS, t!("Remote access by ID"), true, Some(*on)));
            m.push(it(item::COPY_ID, &t!("Copy the ID {}", crate::computers::spaced(id)), *on, None));
        }
        None => m.push(it(0, t!("Remote access by ID: not set up"), false, None)),
    }
    // Who comes in and how: the switches of the remote access settings, as worded there and in
    // their order.
    let mut who = match s.settings {
        Some([ask, straight, nearby, direct, requests]) => vec![
            it(item::ASK, t!("Ask the person at this computer before letting anyone in"), true, Some(ask)),
            it(item::REQUESTS, t!("Let people without the password ask to come in"), true, Some(requests)),
            it(item::NEARBY, t!("Let computers on this network find this one"), true, Some(nearby)),
            it(item::DIRECT, t!("Let viewers on this network come straight in"), true, Some(direct)),
            it(item::STRAIGHT, t!("Let viewers by ID go straight through the routers"), true, Some(straight)),
        ],
        None => vec![it(0, t!("Not set up"), false, None)],
    };
    who.push(Entry::Separator);
    who.push(it(item::SETTINGS, t!("All remote access settings\u{2026}"), true, None));
    m.push(Entry::Sub { label: t!("Who comes in").into(), entries: who });
    // Whoever is connected: their window brought up, or their connection ended, each.
    #[cfg(feature = "remote")]
    match s.connected.len() {
        0 => m.push(it(0, t!("Nobody is connected"), false, None)),
        n => {
            let mut sub = Vec::new();
            if cfg!(windows) {
                sub.push(it(item::SHOW, if n == 1 { t!("Show their window") } else { t!("Show their windows") }, true, None));
                sub.push(Entry::Separator);
            }
            for (i, v) in s.connected.iter().take(item::END_MOST).enumerate() {
                sub.push(it(item::END + i, &t!("End: {}", crate::lang::tr_msg(&v.describe())), true, None));
            }
            m.push(Entry::Sub { label: if n == 1 { t!("1 connected now").into() } else { t!("{} connected now", n) }, entries: sub });
        }
    }
    let svc = match s.service {
        Some((true, runs)) => vec![it(item::START, t!("Start"), !runs, None), it(item::STOP, t!("Stop"), runs, None), it(item::REMOVE, t!("Remove"), true, None)],
        Some((false, _)) => vec![it(item::INSTALL, t!("Install: start remote access with the computer"), true, None)],
        None => vec![it(0, t!("Looking\u{2026}"), false, None)],
    };
    let label = match s.service {
        Some((true, true)) => t!("Service: running"),
        Some((true, false)) => t!("Service: stopped"),
        _ => t!("Service"),
    };
    m.push(Entry::Sub { label: label.into(), entries: svc });
    m.push(Entry::Separator);
    m.push(it(item::QUIT, if s.alone { t!("Hide this icon until the next sign-in") } else { t!("Quit") }, true, None));
    m
}

/// Does what the menu's item `n` asks; says what went wrong, if anything did.
#[cfg_attr(not(windows), allow(dead_code))]
fn act(n: usize, status: &Status) -> Result<(), String> {
    #[cfg(feature = "remote")]
    {
        use crate::access;
        // A change of the settings or the service: here when this program has the rights (a
        // window started as administrator), else by the command line through the prompt for them.
        let as_admin = |args: &[&str], here: &dyn Fn() -> Result<(), String>| -> Result<(), String> {
            // Elsewhere than Windows the settings and the service are the user's own.
            if crate::admin::elevated() || !cfg!(windows) {
                return here();
            }
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let args: Vec<String> = ["--no-console", "remote-access"].into_iter().chain(args.iter().copied()).map(String::from).collect();
            #[cfg(windows)]
            let given = crate::admin::run_elevated(&exe.to_string_lossy(), &args, true)?;
            #[cfg(not(windows))]
            let given = crate::admin::relaunch(&args)?;
            let _ = exe;
            if given { Ok(()) } else { Err(t!("administrator rights were not given").into()) }
        };
        let change = |f: &dyn Fn(&mut access::Config)| -> Result<(), String> {
            let mut c = access::stored()?.ok_or(t!("remote access is not set up"))?;
            f(&mut c);
            access::save(&c)
        };
        let set = status.settings.unwrap_or_default();
        let word = |on: bool| if on { "on" } else { "off" };
        match n {
            item::ACCESS => {
                let on = !status.access.as_ref().is_some_and(|a| a.0);
                as_admin(&[word(on)], &|| change(&|c| c.enabled = on))?;
            }
            item::ASK => as_admin(&["ask", word(!set[0])], &|| change(&|c| c.ask = !set[0]))?,
            item::STRAIGHT => as_admin(&["straight", word(!set[1])], &|| change(&|c| c.punch = !set[1]))?,
            item::NEARBY => as_admin(&["lan", word(!set[2])], &|| change(&|c| c.lan = !set[2]))?,
            item::DIRECT => as_admin(&["direct", word(!set[3])], &|| change(&|c| c.direct = !set[3]))?,
            item::REQUESTS => as_admin(&["requests", word(!set[4])], &|| change(&|c| c.requests = !set[4]))?,
            item::COPY_ID => {
                if let Some((_, id)) = &status.access {
                    arboard::Clipboard::new().and_then(|mut c| c.set_text(id.clone())).map_err(|e| e.to_string())?;
                }
            }
            n if (item::END..item::END + item::END_MOST).contains(&n) => {
                if let Some(v) = status.connected.get(n - item::END) {
                    // The icon's own program asks the service; the window's ends its own.
                    if status.alone { access::ask_to_end(v.n)? } else { access::disconnect(v.n) }
                }
            }
            item::INSTALL => as_admin(&["install"], &access::install_service)?,
            item::REMOVE => as_admin(&["uninstall"], &access::uninstall_service)?,
            item::START => as_admin(&["service", "start"], &|| access::service_control(true))?,
            item::STOP => as_admin(&["service", "stop"], &|| access::service_control(false))?,
            _ => {}
        }
        CHANGED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(not(feature = "remote"))]
    let _ = (n, status);
    Ok(())
}

/// The icon's own program's view: the service's state file and the service manager; on Linux
/// the settings themselves too, which are the user's.
/// The program's icon averaged down to `size` by `size`, RGBA (all clear for an icon of none).
#[cfg(any(windows, all(target_os = "linux", feature = "remote")))]
fn icon_at(icon: &eframe::egui::IconData, size: usize) -> Vec<[u8; 4]> {
    let (w, h) = (icon.width as usize, icon.height as usize);
    let mut out = vec![[0u8; 4]; size * size];
    if w == 0 || h == 0 {
        return out;
    }
    for y in 0..size {
        for x in 0..size {
            let (x0, x1) = (x * w / size, ((x + 1) * w / size).max(x * w / size + 1).min(w));
            let (y0, y1) = (y * h / size, ((y + 1) * h / size).max(y * h / size + 1).min(h));
            let (mut sum, mut n) = ([0u32; 4], 0u32);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let o = (sy * w + sx) * 4;
                    for (s, &v) in sum.iter_mut().zip(&icon.rgba[o..o + 4]) {
                        *s += v as u32;
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            out[y * size + x] = sum.map(|s| (s / n) as u8);
        }
    }
    out
}

#[cfg(all(any(windows, target_os = "linux"), feature = "remote"))]
fn read_status() -> Status {
    crate::lang::set(&crate::hosts::setting_now("language").unwrap_or_default());
    let mut st = Status { alone: true, lang: crate::lang::current(), ..Default::default() };
    st.service = Some((crate::access::service_installed(), crate::access::service_runs()));
    let v: Option<serde_json::Value> =
        crate::access::state_file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str(&t).ok());
    if let Some(v) = &v {
        let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if !id.is_empty() {
            st.access = Some((b("enabled"), id));
            st.settings = Some([b("ask"), b("punch"), b("lan"), b("direct"), b("requests")]);
        }
        // Who is connected counts only while the service runs.
        if st.service.is_some_and(|s| s.1) {
            st.connected = crate::access::service_viewers();
        }
    }
    #[cfg(not(windows))]
    if let Ok(Some(c)) = crate::access::stored() {
        st.access = Some((c.enabled, c.id.clone()));
        st.settings = Some([c.ask, c.punch, c.lan, c.direct, c.requests]);
    }
    st
}

/// `sshremote --tray`: the icon alone, for the service, until it is removed or the icon hidden.
/// One for each signed-in user.
#[cfg(all(windows, feature = "remote"))]
pub fn run_alone() -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name = crate::sys::wide(win::ALONE);
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(());
    }
    let status: Shared = std::sync::Arc::new(std::sync::Mutex::new(read_status()));
    let tray = Tray::start(0, status.clone(), super::app_icon().as_ref()).ok_or(t!("the notification area is not there"))?;
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if QUIT.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let now = read_status();
        // The service was removed: its program file is let go with the icon.
        if now.service.is_some_and(|s| !s.0) {
            break;
        }
        *status.lock().unwrap_or_else(|e| e.into_inner()) = now;
        tray.refresh();
    }
    drop(tray);
    unsafe { CloseHandle(mutex) };
    Ok(())
}

/// `sshremote --tray` on Linux: the icon in the top bar (a StatusNotifierItem, which GNOME shows
/// with its AppIndicator extension, as Ubuntu has on), with the same menu as on Windows.
#[cfg(all(target_os = "linux", feature = "remote"))]
pub fn run_alone() -> Result<(), String> {
    sni::run()
}

#[cfg(all(target_os = "linux", feature = "remote"))]
mod sni {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use zbus::object_server::SignalEmitter;
    use zbus::{OwnedObjectPath, OwnedVariant, Structure, Variant};

    use super::{Entry, QUIT, Shared, Status, act, entries, item, read_status};

    type Pixmap = Vec<(i32, i32, Vec<u8>)>;

    fn tip(s: &Status) -> String {
        let mut t = match &s.access {
            Some((true, id)) => t!("Reachable by ID {}", crate::computers::spaced(id)),
            Some((false, _)) => t!("Remote access is off").to_string(),
            None => t!("Remote access is not set up").to_string(),
        };
        if !s.connected.is_empty() {
            t.push_str(&format!("; {}", t!("{} connected now", s.connected.len())));
        }
        t
    }

    /// The window: started (a second one is harmless; the first stays as it is).
    fn open_app() {
        if let Ok(exe) = std::env::current_exe() {
            use std::process::Stdio;
            let _ = std::process::Command::new(exe).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }

    /// A line of the menu as dbusmenu has it: its number, properties and lines under it.
    struct Node {
        id: i32,
        props: Vec<(&'static str, Variant<'static>)>,
        children: Vec<Node>,
    }

    /// The menu as nodes: choices keep their item numbers, other lines get their own above 1000.
    fn nodes(list: &[Entry], next: &mut i32) -> Vec<Node> {
        list.iter()
            .map(|e| match e {
                Entry::Separator => {
                    *next += 1;
                    Node { id: *next, props: vec![("type", Variant::from("separator"))], children: Vec::new() }
                }
                Entry::Item { id, label, enabled, checked } => {
                    let id = if *id == 0 {
                        *next += 1;
                        *next
                    } else {
                        *id as i32
                    };
                    let mut props = vec![("label", Variant::from(label.replace('_', "__"))), ("enabled", Variant::from(*enabled))];
                    if let Some(on) = checked {
                        props.push(("toggle-type", Variant::from("checkmark")));
                        props.push(("toggle-state", Variant::from(if *on { 1i32 } else { 0 })));
                    }
                    Node { id, props, children: Vec::new() }
                }
                Entry::Sub { label, entries } => {
                    *next += 1;
                    let id = *next;
                    let children = nodes(entries, next);
                    Node { id, props: vec![("label", Variant::from(label.clone())), ("children-display", Variant::from("submenu"))], children }
                }
            })
            .collect()
    }

    fn props(n: &Node, wanted: &[String]) -> HashMap<String, OwnedVariant> {
        n.props
            .iter()
            .filter(|(k, _)| wanted.is_empty() || wanted.iter().any(|w| w == k))
            .filter_map(|(k, v)| v.try_to_owned().ok().map(|v| (k.to_string(), v)))
            .collect()
    }

    fn as_value(n: &Node, wanted: &[String], depth: i32) -> Option<OwnedVariant> {
        let children: Vec<OwnedVariant> =
            if depth == 0 { Vec::new() } else { n.children.iter().filter_map(|c| as_value(c, wanted, depth - 1)).collect() };
        let s = Structure::builder()
            .append_field(Variant::I32(n.id))
            .append_field(Variant::from(props(n, wanted)))
            .append_field(Variant::from(children))
            .build()
            .ok()?;
        Variant::Structure(s).try_to_owned().ok()
    }

    fn find(list: &[Node], id: i32) -> Option<&Node> {
        list.iter().find_map(|n| if n.id == id { Some(n) } else { find(&n.children, id) })
    }

    fn root(s: &Status) -> Node {
        let mut next = 1000;
        Node { id: 0, props: vec![("children-display", Variant::from("submenu"))], children: nodes(&entries(s), &mut next) }
    }

    struct Item {
        status: Shared,
        icon: Pixmap,
    }

    #[zbus::interface(name = "org.kde.StatusNotifierItem")]
    impl Item {
        #[zbus(property)]
        fn category(&self) -> String {
            "ApplicationStatus".into()
        }
        #[zbus(property)]
        fn id(&self) -> String {
            "sshremote".into()
        }
        #[zbus(property)]
        fn title(&self) -> String {
            "SSH Remote".into()
        }
        #[zbus(property)]
        fn status(&self) -> String {
            "Active".into()
        }
        #[zbus(property)]
        fn window_id(&self) -> i32 {
            0
        }
        #[zbus(property)]
        fn icon_name(&self) -> String {
            String::new()
        }
        #[zbus(property)]
        fn icon_pixmap(&self) -> Pixmap {
            self.icon.clone()
        }
        #[zbus(property)]
        fn overlay_icon_name(&self) -> String {
            String::new()
        }
        #[zbus(property)]
        fn attention_icon_name(&self) -> String {
            String::new()
        }
        #[zbus(property)]
        fn tool_tip(&self) -> (String, Pixmap, String, String) {
            let s = self.status.lock().map(|s| s.clone()).unwrap_or_default();
            (String::new(), Vec::new(), "SSH Remote".into(), tip(&s))
        }
        /// A click opens the window; the menu comes with the other button.
        #[zbus(property)]
        fn item_is_menu(&self) -> bool {
            false
        }
        #[zbus(property)]
        fn menu(&self) -> OwnedObjectPath {
            OwnedObjectPath::try_from("/MenuBar").expect("a path")
        }
        fn activate(&self, _x: i32, _y: i32) {
            open_app();
        }
        fn secondary_activate(&self, _x: i32, _y: i32) {}
        fn context_menu(&self, _x: i32, _y: i32) {}
        fn scroll(&self, _delta: i32, _orientation: String) {}
        #[zbus(signal)]
        async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    }

    struct Menu {
        status: Shared,
        revision: Arc<AtomicU32>,
    }

    #[zbus::interface(name = "com.canonical.dbusmenu")]
    impl Menu {
        #[zbus(property)]
        fn version(&self) -> u32 {
            3
        }
        #[zbus(property)]
        fn text_direction(&self) -> String {
            "ltr".into()
        }
        #[zbus(property)]
        fn status(&self) -> String {
            "normal".into()
        }
        #[zbus(property)]
        fn icon_theme_path(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_layout(&self, parent_id: i32, recursion_depth: i32, property_names: Vec<String>) -> (u32, (i32, HashMap<String, OwnedVariant>, Vec<OwnedVariant>)) {
            let s = self.status.lock().map(|s| s.clone()).unwrap_or_default();
            let r = root(&s);
            let n = if parent_id == 0 { Some(&r) } else { find(&r.children, parent_id) };
            let depth = if recursion_depth < 0 { i32::MAX } else { recursion_depth };
            let (id, p, children) = match n {
                Some(n) => (n.id, props(n, &property_names), if depth == 0 { Vec::new() } else { n.children.iter().filter_map(|c| as_value(c, &property_names, depth - 1)).collect() }),
                None => (parent_id, HashMap::new(), Vec::new()),
            };
            (self.revision.load(Ordering::Relaxed), (id, p, children))
        }
        fn get_group_properties(&self, ids: Vec<i32>, property_names: Vec<String>) -> Vec<(i32, HashMap<String, OwnedVariant>)> {
            let s = self.status.lock().map(|s| s.clone()).unwrap_or_default();
            let r = root(&s);
            ids.iter().filter_map(|id| if *id == 0 { Some(&r) } else { find(&r.children, *id) }).map(|n| (n.id, props(n, &property_names))).collect()
        }
        fn get_property(&self, id: i32, name: String) -> OwnedVariant {
            let s = self.status.lock().map(|s| s.clone()).unwrap_or_default();
            let r = root(&s);
            find(&r.children, id).and_then(|n| props(n, &[name]).into_values().next()).unwrap_or_else(|| OwnedVariant::from(0i32))
        }
        fn event(&self, id: i32, event_id: String, _data: OwnedVariant, _timestamp: u32) {
            if event_id == "clicked" {
                self.chosen(id);
            }
        }
        fn event_group(&self, events: Vec<(i32, String, OwnedVariant, u32)>) -> Vec<i32> {
            for (id, event_id, ..) in events {
                if event_id == "clicked" {
                    self.chosen(id);
                }
            }
            Vec::new()
        }
        fn about_to_show(&self, _id: i32) -> bool {
            false
        }
        fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
            (Vec::new(), Vec::new())
        }
        #[zbus(signal)]
        async fn layout_updated(emitter: &SignalEmitter<'_>, revision: u32, parent: i32) -> zbus::Result<()>;
    }

    impl Menu {
        fn chosen(&self, id: i32) {
            let s = self.status.lock().map(|s| s.clone()).unwrap_or_default();
            match id as usize {
                item::OPEN | item::SETTINGS | item::SHOW => open_app(),
                item::QUIT => QUIT.store(true, Ordering::Relaxed),
                n if n < 1000 => {
                    let _ = act(n, &s);
                    // Read again at once, and shown so.
                    *self.status.lock().unwrap_or_else(|e| e.into_inner()) = read_status();
                    self.revision.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
        }
    }

    /// The program's icon at `size`, as ARGB in network order.
    fn pixmap(icon: &eframe::egui::IconData, size: usize) -> (i32, i32, Vec<u8>) {
        let argb = super::icon_at(icon, size).into_iter().flat_map(|[r, g, b, a]| [a, r, g, b]).collect();
        (size as i32, size as i32, argb)
    }

    pub(super) fn run() -> Result<(), String> {
        let e = |e: zbus::Error| e.to_string();
        let status: Shared = Arc::new(Mutex::new(read_status()));
        let revision = Arc::new(AtomicU32::new(1));
        let icon = crate::gui::app_icon().map(|i| vec![pixmap(&i, 32), pixmap(&i, 64)]).unwrap_or_default();
        let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        // The connection answers the menu's calls between these, on a thread of zbus's own.
        let conn = zbus::block_on(async {
            let conn = zbus::connection::Builder::session()
                .serve_at("/StatusNotifierItem", Item { status: status.clone(), icon })
                .serve_at("/MenuBar", Menu { status: status.clone(), revision: revision.clone() })
                .build()
                .await
                .map_err(e)?;
            // One icon for each signed-in user.
            match conn.request_name_with_flags("dev.sshremote.Tray", zbus::fdo::RequestNameFlags::DO_NOT_QUEUE).await {
                Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {}
                _ => return Ok(None),
            }
            conn.request_name(name.as_str()).await.map_err(e)?;
            let watcher = zbus::Proxy::new(&conn, "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher", "org.kde.StatusNotifierWatcher").await.map_err(e)?;
            watcher
                .call_method("RegisterStatusNotifierItem", &(name.as_str(),))
                .await
                .map_err(|_| t!("the top bar shows no icons here (GNOME needs its AppIndicator extension)").to_string())?;
            Ok::<_, String>(Some(conn))
        })?;
        let Some(conn) = conn else { return Ok(()) };
        let mut last = (String::new(), 0u32);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if QUIT.load(Ordering::Relaxed) {
                return Ok(());
            }
            let now = read_status();
            if now.service.is_some_and(|s| !s.0) {
                return Ok(());
            }
            *status.lock().unwrap_or_else(|e| e.into_inner()) = now.clone();
            // A change of what the menu says: told, so an open one is made again.
            let said = (format!("{:?}{:?}{:?}{:?}{}", now.access, now.service, now.settings, now.connected, now.lang), revision.load(Ordering::Relaxed));
            if said.0 != last.0 || said.1 != last.1 {
                if said.0 != last.0 {
                    revision.fetch_add(1, Ordering::Relaxed);
                }
                let rev = revision.load(Ordering::Relaxed);
                zbus::block_on(async {
                    if let Ok(menu) = conn.object_server().interface::<_, Menu>("/MenuBar").await {
                        let _ = Menu::layout_updated(menu.signal_emitter(), rev, 0).await;
                    }
                    if let Ok(item) = conn.object_server().interface::<_, Item>("/StatusNotifierItem").await {
                        let _ = Item::new_tool_tip(item.signal_emitter()).await;
                    }
                });
                last = (said.0, rev);
            }
        }
    }
}

/// Whether the icon's own program runs in this sign-in (the window then makes no second icon).
#[cfg(windows)]
pub(super) fn alone_runs() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let name = crate::sys::wide(win::ALONE);
    let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if h.is_null() {
        return false;
    }
    unsafe { CloseHandle(h) };
    true
}

#[cfg(windows)]
mod win {
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
        DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, HICON, ICONINFO, MF_CHECKED,
        MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
        RegisterWindowMessageW, SW_HIDE, SW_RESTORE, SW_SHOW, SendMessageW, SetForegroundWindow, ShowWindow, TPM_NONOTIFY,
        TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_CONTEXTMENU,
        WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
    };

    use super::{QUIT, Shared, act, item};
    use crate::sys::wide;

    const CALLBACK: u32 = WM_APP + 1;
    const SAY: u32 = WM_APP + 2;
    /// The tip read again (the icon's own program, every few seconds).
    const REFRESH: u32 = WM_APP + 3;
    /// The icon's own program, one in each sign-in.
    pub(in crate::gui) const ALONE: &str = "Local\\SshRemoteTray";
    /// The windows of the person connected by the ID: what "show" brings to the front.
    const CONNECTED_WINDOWS: &str = "SSH Remote: someone";

    /// What the window procedure works with: the window shown again, and the menu's status.
    struct Inner {
        main: isize,
        status: Shared,
        icon: isize,
        /// The message Explorer broadcasts when it starts again (its icons are gone then).
        taskbar: u32,
        said: Option<String>,
    }

    static INNER: Mutex<Option<Inner>> = Mutex::new(None);

    /// The icon in the notification area, on a thread of its own; it goes when dropped.
    pub(in crate::gui) struct Tray {
        hwnd: isize,
    }

    impl Tray {
        /// Shows the icon for the window `main` (a top-level window of this program), hidden now.
        pub(in crate::gui) fn start(main: isize, status: Shared, icon: Option<&eframe::egui::IconData>) -> Option<Tray> {
            let hicon = icon.map_or(0, make_icon);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let class = wide("SshRemoteTray");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(proc_),
                    hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
                    lpszClassName: class.as_ptr(),
                    ..unsafe { std::mem::zeroed() }
                };
                unsafe { RegisterClassW(&wc) };
                // A hidden top-level window: Explorer's broadcast reaches it, unlike a message-only one.
                let title = wide("SSH Remote tray");
                let hwnd = unsafe {
                    CreateWindowExW(0, class.as_ptr(), title.as_ptr(), 0, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), wc.hInstance, std::ptr::null())
                };
                if hwnd.is_null() {
                    let _ = tx.send(0);
                    return;
                }
                let taskbar = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
                *INNER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Inner { main, status, icon: hicon, taskbar, said: None });
                add(hwnd as isize, NIM_ADD);
                let _ = tx.send(hwnd as isize);
                let mut msg: MSG = unsafe { std::mem::zeroed() };
                while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
                    unsafe {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                if let Some(i) = INNER.lock().unwrap_or_else(|e| e.into_inner()).take()
                    && i.icon != 0
                {
                    unsafe { DestroyIcon(i.icon as HICON) };
                }
            });
            let hwnd = rx.recv().ok().filter(|h| *h != 0)?;
            Some(Tray { hwnd })
        }

        /// The tip and menu read the status again.
        #[cfg_attr(not(feature = "remote"), allow(dead_code))]
        pub(in crate::gui) fn refresh(&self) {
            unsafe { PostMessageW(self.hwnd as HWND, REFRESH, 0, 0) };
        }

        /// A balloon from the icon, once: where the program went.
        pub(in crate::gui) fn say(&self, text: &str) {
            if let Some(i) = INNER.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                i.said = Some(text.to_string());
            }
            unsafe { PostMessageW(self.hwnd as HWND, SAY, 0, 0) };
        }

    }

    impl Drop for Tray {
        fn drop(&mut self) {
            // Waited for, so the icon is gone before the program is.
            unsafe { SendMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
        }
    }

    /// The window of this program in front now (the one whose close button was just used).
    pub(in crate::gui) fn front_window() -> Option<isize> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
        let h = unsafe { GetForegroundWindow() };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, &mut pid) };
        (!h.is_null() && pid == std::process::id()).then_some(h as isize)
    }

    /// Adds, changes or removes the icon.
    fn add(hwnd: isize, what: u32) {
        let inner = INNER.lock().unwrap_or_else(|e| e.into_inner());
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd as HWND;
        nid.uID = 1;
        nid.uFlags = NIF_MESSAGE | NIF_TIP | NIF_ICON;
        nid.uCallbackMessage = CALLBACK;
        nid.hIcon = inner.as_ref().map_or(std::ptr::null_mut(), |i| i.icon as HICON);
        let status = inner.as_ref().and_then(|i| i.status.lock().ok().map(|s| s.clone())).unwrap_or_default();
        let tip = match &status.access {
            Some((true, id)) => t!("SSH Remote: reachable by ID {}", crate::computers::spaced(id)),
            Some((false, _)) => t!("SSH Remote: remote access is off").to_string(),
            None => "SSH Remote".to_string(),
        };
        #[cfg(feature = "remote")]
        let tip = if status.connected.is_empty() { tip } else { format!("{tip}; {}", t!("{} connected now", status.connected.len())) };
        nid.szTip = wide_n(&tip);
        let mut inner = inner;
        if what == NIM_MODIFY
            && let Some(text) = inner.as_mut().and_then(|i| i.said.take())
        {
            nid.uFlags |= NIF_INFO;
            nid.szInfoTitle = wide_n("SSH Remote");
            nid.szInfo = wide_n(&text);
        }
        drop(inner);
        unsafe { Shell_NotifyIconW(what, &nid) };
    }

    /// `text` as a fixed field of UTF-16, cut to fit with its NUL (by value: on 32-bit Windows
    /// the structure is packed, and its fields cannot be borrowed).
    fn wide_n<const N: usize>(text: &str) -> [u16; N] {
        let mut a = [0u16; N];
        let w: Vec<u16> = text.encode_utf16().take(N - 1).collect();
        a[..w.len()].copy_from_slice(&w);
        a
    }

    /// A balloon from the icon now.
    fn tell(hwnd: HWND, text: &str) {
        if let Some(i) = INNER.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            i.said = Some(text.to_string());
        }
        add(hwnd as isize, NIM_MODIFY);
    }

    /// The window: the one hidden (the window's icon), else SSH Remote's open window brought to the
    /// front or, with none, started (the icon's own program).
    fn open(main: isize) {
        if main != 0 {
            return show(main);
        }
        use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
        let title = wide("SSH Remote");
        let w = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
        if !w.is_null() {
            return show(w as isize);
        }
        if let Ok(exe) = std::env::current_exe() {
            let mut c = std::process::Command::new(exe);
            use std::os::windows::process::CommandExt;
            // DETACHED_PROCESS: the window lives on its own, with no console.
            c.creation_flags(0x0000_0008);
            let _ = c.spawn();
        }
    }

    /// Brings the windows of whoever is connected by the ID to the front; how many there were.
    fn show_connected() -> usize {
        use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW};
        unsafe extern "system" fn each(h: HWND, found: LPARAM) -> windows_sys::core::BOOL {
            let mut buf = [0u16; 128];
            let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
            let title = String::from_utf16_lossy(&buf[..n]);
            if title.starts_with(CONNECTED_WINDOWS) || [t!("SSH Remote: someone is connected"), t!("SSH Remote: someone wants to connect")].contains(&title.as_str()) {
                show(h as isize);
                unsafe { *(found as *mut usize) += 1 };
            }
            1
        }
        let mut found = 0usize;
        unsafe { EnumWindows(Some(each), &mut found as *mut usize as LPARAM) };
        found
    }

    /// Shows the window again, in front.
    fn show(main: isize) {
        unsafe {
            ShowWindow(main as HWND, SW_SHOW);
            ShowWindow(main as HWND, SW_RESTORE);
            SetForegroundWindow(main as HWND);
        }
    }

    /// Hides `main`, leaving the icon to stand for it.
    pub(in crate::gui) fn hide(main: isize) {
        unsafe { ShowWindow(main as HWND, SW_HIDE) };
    }

    unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let (main, taskbar, status) = match INNER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(i) => (i.main, i.taskbar, i.status.lock().map(|s| s.clone()).unwrap_or_default()),
            None => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        };
        match msg {
            CALLBACK => match lp as u32 {
                WM_LBUTTONUP => open(main),
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    let chosen = menu(hwnd, &status);
                    match chosen {
                        0 => {}
                        item::OPEN | item::SETTINGS => open(main),
                        item::SHOW => {
                            if show_connected() == 0 {
                                tell(hwnd, t!("Nobody connected by this computer's ID has a window open here now."));
                            }
                        }
                        // The icon's own program only goes; the window's quits with it.
                        item::QUIT if main == 0 => QUIT.store(true, Ordering::Relaxed),
                        item::QUIT => {
                            QUIT.store(true, Ordering::Relaxed);
                            show(main);
                            unsafe { PostMessageW(main as HWND, WM_CLOSE, 0, 0) };
                        }
                        n => {
                            if let Err(e) = act(n, &status) {
                                tell(hwnd, &crate::lang::tr_msg(&e));
                            }
                        }
                    }
                }
                _ => {}
            },
            SAY | REFRESH => add(hwnd as isize, NIM_MODIFY),
            WM_CLOSE => {
                add(hwnd as isize, NIM_DELETE);
                unsafe {
                    DestroyWindow(hwnd);
                    PostQuitMessage(0);
                }
            }
            m if m == taskbar => add(hwnd as isize, NIM_ADD),
            _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        }
        0
    }

    /// The menu at the pointer; the item chosen, 0 for none.
    fn menu(hwnd: HWND, s: &super::Status) -> usize {
        fn fill(m: windows_sys::Win32::UI::WindowsAndMessaging::HMENU, entries: &[super::Entry]) {
            for e in entries {
                unsafe {
                    match e {
                        super::Entry::Separator => {
                            AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
                        }
                        super::Entry::Item { id, label, enabled, checked } => {
                            let flags = MF_STRING | if *enabled { 0 } else { MF_GRAYED } | if *checked == Some(true) { MF_CHECKED } else { 0 };
                            let w = wide(label);
                            AppendMenuW(m, flags, *id, w.as_ptr());
                        }
                        super::Entry::Sub { label, entries } => {
                            let sub = CreatePopupMenu();
                            fill(sub, entries);
                            let w = wide(label);
                            AppendMenuW(m, MF_STRING | MF_POPUP, sub as usize, w.as_ptr());
                        }
                    }
                }
            }
        }
        unsafe {
            let m = CreatePopupMenu();
            fill(m, &super::entries(s));
            let mut p = POINT { x: 0, y: 0 };
            GetCursorPos(&mut p);
            // The menu closes when clicked away only if its window is in front.
            SetForegroundWindow(hwnd);
            let chosen = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, p.x, p.y, 0, hwnd, std::ptr::null());
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(m);
            chosen.max(0) as usize
        }
    }

    /// The program's icon at the notification area's size.
    fn make_icon(icon: &eframe::egui::IconData) -> isize {
        const S: usize = 32;
        let (w, h) = (icon.width as usize, icon.height as usize);
        if w == 0 || h == 0 {
            return 0;
        }
        // Down to 32x32, as BGRA.
        let bgra: Vec<u8> = super::icon_at(icon, S).into_iter().flat_map(|[r, g, b, a]| [b, g, r, a]).collect();
        unsafe {
            let color = CreateBitmap(S as i32, S as i32, 1, 32, bgra.as_ptr().cast());
            let mask_bits = [0u8; S * S / 8];
            let mask = CreateBitmap(S as i32, S as i32, 1, 1, mask_bits.as_ptr().cast());
            let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
            let icon = CreateIconIndirect(&info);
            DeleteObject(color);
            DeleteObject(mask);
            icon as isize
        }
    }
}

#[cfg(windows)]
pub(super) use win::{front_window, hide};

#[cfg(all(test, target_os = "linux", feature = "remote"))]
mod tests {
    #[test]
    fn the_icon_is_averaged_down() {
        let rgba = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 255, 255]].concat();
        let icon = eframe::egui::IconData { rgba, width: 2, height: 2 };
        assert_eq!(super::icon_at(&icon, 1), vec![[127, 127, 127, 255]]);
        assert_eq!(super::icon_at(&icon, 2)[1], [0, 255, 0, 255]);
        assert_eq!(super::icon_at(&eframe::egui::IconData { rgba: Vec::new(), width: 0, height: 0 }, 2), vec![[0; 4]; 4]);
    }
}
