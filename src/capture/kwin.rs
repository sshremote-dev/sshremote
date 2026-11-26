//! KDE Plasma: screens added for a session as KWin's virtual outputs (its screen-cast protocol,
//! which also streams each one through PipeWire), and the pointer put on them by its fake input,
//! as the portal's session that shows the other screens does not reach them. KWin lets a program
//! use these only where a desktop file of it lists them (as Plasma's own programs and its portal
//! do): this program writes one of its own, hidden from menus.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;
use wayland_protocols_plasma::screencast::v1::client::zkde_screencast_stream_unstable_v1::{self, ZkdeScreencastStreamUnstableV1};
use wayland_protocols_plasma::screencast::v1::client::zkde_screencast_unstable_v1::ZkdeScreencastUnstableV1;

/// A screen added, as the capture shows it: KWin's name for it, its stream's PipeWire node, and
/// its size and place on the desktop (logical points).
#[derive(Clone, Debug, PartialEq)]
pub struct Extra {
    pub name: String,
    pub node: u32,
    pub size: (i32, i32),
    pub at: (i32, i32),
}

static EXTRA: Mutex<Vec<Extra>> = Mutex::new(Vec::new());

/// The screens added now, in the order they came.
pub fn extras() -> Vec<Extra> {
    EXTRA.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[derive(Default)]
struct State {
    cast: Option<(ZkdeScreencastUnstableV1, u32)>,
    fake: Option<OrgKdeKwinFakeInput>,
    node: Option<Result<u32, String>>,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(s: &mut State, r: &WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<State>) {
        if let wl_registry::Event::Global { name, interface, version } = e {
            match interface.as_str() {
                // Virtual outputs came with its version 2.
                "zkde_screencast_unstable_v1" if version >= 2 => s.cast = Some((r.bind(name, version.min(4), qh, ()), version.min(4))),
                // Absolute moves came with its version 3.
                "org_kde_kwin_fake_input" if version >= 3 => s.fake = Some(r.bind(name, version.min(4), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<ZkdeScreencastStreamUnstableV1, ()> for State {
    fn event(s: &mut State, _: &ZkdeScreencastStreamUnstableV1, e: zkde_screencast_stream_unstable_v1::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {
        match e {
            zkde_screencast_stream_unstable_v1::Event::Created { node } => s.node = Some(Ok(node)),
            zkde_screencast_stream_unstable_v1::Event::Failed { error } => s.node = Some(Err(error)),
            zkde_screencast_stream_unstable_v1::Event::Closed => {
                s.node.get_or_insert_with(|| Err("KWin closed the new screen's stream".into()));
            }
            _ => {}
        }
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut State, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
        }
    )*};
}
ignore!(ZkdeScreencastUnstableV1, OrgKdeKwinFakeInput);

/// The desktop file that lets KWin give this program its screen-cast and fake input protocols
/// (one for each place the program is, so two copies do not take turns), written where it is not
/// as it should be. KWin reads it once Plasma's list of programs has it, which KDE's libraries
/// build again by themselves, in the session's own environment (building it here, without the
/// session's menu prefix, would leave the programs out of it), when the folder changes: so it is
/// put in place by a rename, which changes the folder, never written over.
fn trust() -> Result<(), String> {
    use std::hash::{Hash, Hasher};
    let exe = std::env::current_exe().and_then(|e| e.canonicalize()).map_err(|e| e.to_string())?;
    let data = std::env::var_os("XDG_DATA_HOME").map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))).ok_or("no home folder")?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    exe.hash(&mut h);
    let file = data.join("applications").join(format!("dev.sshremote.screens-{:08x}.desktop", h.finish() as u32));
    let exec = exe.display().to_string();
    let exec = if exec.contains(' ') { format!("\"{exec}\"") } else { exec };
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=SSH Remote\nComment=Screens added for remote sessions\nExec={exec}\nNoDisplay=true\nX-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1,org_kde_kwin_fake_input\n"
    );
    if std::fs::read_to_string(&file).ok().as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    std::fs::create_dir_all(file.parent().ok_or("bad path")?).map_err(|e| e.to_string())?;
    let new = file.with_extension("desktop.new");
    std::fs::write(&new, text).and_then(|()| std::fs::rename(&new, &file)).map_err(|e| format!("{}: {e}", file.display()))
}

/// A connection to KWin with its screen-cast protocol (and fake input, where given): asked again
/// for a few seconds after the desktop file was just written, until KWin has read it.
fn connect() -> Result<(Connection, EventQueue<State>, State), String> {
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        let conn = Connection::connect_to_env().map_err(|e| format!("Wayland: {e}"))?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut st = State::default();
        queue.roundtrip(&mut st).map_err(|e| format!("Wayland: {e}"))?;
        if st.cast.is_some() || Instant::now() > until {
            return Ok((conn, queue, st));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// KWin's outputs as its own tool lists them: name, place and size (logical points).
fn outputs() -> Vec<(String, (i32, i32), (i32, i32))> {
    let mut cmd = std::process::Command::new("kscreen-doctor");
    cmd.arg("-j").env("QT_QPA_PLATFORM", "wayland").stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let Ok(out) = cmd.output() else { return Vec::new() };
    let Ok(all) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    let num = |v: &serde_json::Value| v.as_f64().unwrap_or(0.0).round() as i32;
    all["outputs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["enabled"].as_bool() == Some(true))
        .map(|o| {
            let scale = o["scale"].as_f64().filter(|s| *s > 0.0).unwrap_or(1.0);
            let mode = o["modes"].as_array().into_iter().flatten().find(|m| m["id"] == o["currentModeId"]);
            let (w, h) = mode.map_or((0, 0), |m| (num(&m["size"]["width"]), num(&m["size"]["height"])));
            let size = if o["rotation"].as_u64().is_some_and(|r| r == 2 || r == 8) { (h, w) } else { (w, h) };
            (o["name"].as_str().unwrap_or("").to_string(), (num(&o["pos"]["x"]), num(&o["pos"]["y"])), ((size.0 as f64 / scale).round() as i32, (size.1 as f64 / scale).round() as i32))
        })
        .collect()
}

/// A screen KWin added for this session: it goes when this does.
pub struct Added {
    name: String,
    stream: ZkdeScreencastStreamUnstableV1,
    conn: Connection,
    _queue: EventQueue<State>,
}

impl Drop for Added {
    fn drop(&mut self) {
        EXTRA.lock().unwrap_or_else(|e| e.into_inner()).retain(|e| e.name != self.name);
        self.stream.close();
        let _ = self.conn.flush();
    }
}

const NOT_ALLOWED: &str = "KWin did not let this program add screens (its screen-cast protocol); KDE Plasma 5.27 or later lets programs that ask for it in a desktop file of their own";

/// Adds a screen of `w`x`h` (logical points): its name, and what keeps it.
pub fn add(w: u16, h: u16) -> Result<(String, Added), String> {
    trust()?;
    let (conn, mut queue, mut st) = connect()?;
    let qh = queue.handle();
    let (cast, version) = st.cast.clone().ok_or(NOT_ALLOWED)?;
    let before = outputs();
    let name = (1..).map(|n| format!("SSHRemote-{n}")).find(|n| !before.iter().any(|o| o.0.contains(n.as_str()))).unwrap_or_default();
    // The pointer in the pictures (2: embedded), as the other screens' are.
    let stream = if version >= 4 {
        cast.stream_virtual_output_with_description(name.clone(), "SSH Remote".into(), w as i32, h as i32, 1.0, 2, &qh, ())
    } else {
        cast.stream_virtual_output(name.clone(), w as i32, h as i32, 1.0, 2, &qh, ())
    };
    let until = Instant::now() + Duration::from_secs(5);
    let node = loop {
        queue.roundtrip(&mut st).map_err(|e| format!("Wayland: {e}"))?;
        if let Some(r) = st.node.take() {
            break r;
        }
        if Instant::now() > until {
            break Err("KWin did not make the new screen's stream".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let node = match node {
        Ok(n) => n,
        Err(e) => {
            stream.close();
            let _ = conn.flush();
            return Err(e);
        }
    };
    // Its name and place as KWin lists them.
    let had: Vec<String> = before.into_iter().map(|o| o.0).collect();
    let until = Instant::now() + Duration::from_secs(3);
    let found = loop {
        if let Some(o) = outputs().into_iter().find(|o| !had.contains(&o.0)) {
            break Some(o);
        }
        if Instant::now() > until {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let (oname, at, size) = found.unwrap_or((name, (0, 0), (w as i32, h as i32)));
    EXTRA.lock().unwrap_or_else(|e| e.into_inner()).push(Extra { name: oname.clone(), node, size, at });
    Ok((oname.clone(), Added { name: oname, stream, conn, _queue: queue }))
}

/// KWin's fake input, kept for the session: the pointer put on an added screen.
static FAKE: Mutex<Option<(Connection, OrgKdeKwinFakeInput, EventQueue<State>)>> = Mutex::new(None);

/// Puts the pointer at (`x`, `y`) of the desktop (logical points).
pub fn pointer_at(x: f64, y: f64) -> Result<(), String> {
    let mut fake = FAKE.lock().unwrap_or_else(|e| e.into_inner());
    if fake.is_none() {
        let (conn, queue, mut st) = connect()?;
        let input = st.fake.take().ok_or("KWin did not let this program move the pointer onto the added screen")?;
        input.authenticate("SSH Remote".into(), "Remote control of a screen added for the session".into());
        *fake = Some((conn, input, queue));
    }
    let (conn, input, _) = fake.as_ref().expect("made above");
    input.pointer_motion_absolute(x, y);
    conn.flush().map_err(|e| format!("Wayland: {e}"))
}

#[cfg(test)]
mod tests {
    /// KWin with its permission checks on (as in a real Plasma session) lets this program add a
    /// screen and put the pointer on it once its desktop file asks for that, and the screen goes
    /// with what keeps it. In a KDE Plasma session: SSHREMOTE_KWIN_TEST=1 TESTS --ignored
    /// kwin_lets_this_program_add_a_screen
    #[test]
    #[ignore]
    fn kwin_lets_this_program_add_a_screen() {
        if std::env::var_os("SSHREMOTE_KWIN_TEST").is_none() {
            return;
        }
        let (name, keep) = super::add(1280, 720).expect("a screen added");
        let o = super::outputs().into_iter().find(|o| o.0 == name).expect("KWin lists it");
        println!("added {o:?}");
        assert_eq!(o.2, (1280, 720));
        super::pointer_at(o.1.0 as f64 + 100.0, o.1.1 as f64 + 50.0).expect("the pointer put on it");
        drop(keep);
        std::thread::sleep(std::time::Duration::from_millis(800));
        assert!(!super::outputs().iter().any(|o| o.0 == name), "taken away");
    }
}
