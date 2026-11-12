#[test]
fn tmux_pane_and_passthrough() {
    assert_eq!(super::parse_tmux_pane("3 40 on top\n"), Some((4, 40)));
    assert_eq!(super::parse_tmux_pane("0 0 2 bottom\n"), Some((0, 0)));
    let mut out = Vec::new();
    super::tmux_wrap(&mut out, b"\x1b[1;1H\x1bPq#0\x1b\\");
    assert_eq!(out, b"\x1bPtmux;\x1b\x1b[1;1H\x1b\x1bPq#0\x1b\x1b\\\x1b\\".to_vec());
}

/// What the viewer writes, kept for the test to read.
#[derive(Clone, Default)]
struct Screen(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Screen {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Screen {
    fn take(&self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut *self.0.lock().unwrap())).into_owned()
    }
}

/// Keys held at the host are let go: a modifier let go here (also with the menu open), a
/// release that matches no key held (Shift let go first), and all of them when this terminal
/// goes to the back.
#[test]
#[ignore]
fn keys_held_at_the_host_are_let_go() {
    use crate::proto::{MOD_CTRL, MOD_SHIFT};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind as K, KeyModifiers as Mods, ModifierKeyCode as M};
    use super::*;
    static SCRATCH: std::sync::Once = std::sync::Once::new();
    SCRATCH.call_once(|| {
        let dir = std::env::temp_dir().join(format!("sshremote-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for var in ["APPDATA", "XDG_CONFIG_HOME"] {
            unsafe { std::env::set_var(var, &dir) };
        }
    });
    let opt = crate::parse_client(&["--local".into()]).unwrap();
    let (sess, _tx, _rx) = Session::connect(&opt).expect("a local agent (SSHREMOTE_EXE)");
    let mut v = Viewer::new(&opt, sess, Box::new(std::io::sink()));
    (v.cols, v.rows) = (100, 30);
    v.key_states = true;
    let ev = |code: KeyCode, mods: Mods, kind: K| Event::Key(KeyEvent::new_with_kind(code, mods, kind));
    let ctrl = KeyCode::Modifier(M::LeftControl);
    v.on_term(ev(ctrl, Mods::CONTROL, K::Press)).unwrap();
    assert_eq!(v.held_mods, MOD_CTRL, "Ctrl goes down with its press");
    v.on_term(ev(ctrl, Mods::NONE, K::Release)).unwrap();
    assert_eq!(v.held_mods, 0, "and up with its release");
    v.on_term(ev(ctrl, Mods::CONTROL, K::Press)).unwrap();
    v.on_term(ev(KeyCode::Char(']'), Mods::CONTROL, K::Press)).unwrap();
    assert!(v.overlay != Overlay::None, "the menu is open");
    v.on_term(ev(ctrl, Mods::NONE, K::Release)).unwrap();
    assert_eq!(v.held_mods, 0, "Ctrl let go with the menu open is let go at the host");
    v.overlay = Overlay::None;
    v.on_term(ev(KeyCode::Modifier(M::LeftShift), Mods::SHIFT, K::Press)).unwrap();
    v.on_term(ev(KeyCode::Char('!'), Mods::SHIFT, K::Press)).unwrap();
    v.on_term(ev(KeyCode::Modifier(M::LeftShift), Mods::NONE, K::Release)).unwrap();
    v.on_term(ev(KeyCode::Char('a'), Mods::NONE, K::Press)).unwrap();
    assert_eq!((v.held.len(), v.held_mods & MOD_SHIFT), (2, 0));
    v.on_term(ev(KeyCode::Char('1'), Mods::NONE, K::Release)).unwrap();
    assert!(v.held.is_empty(), "a release that matches no key lets every key go: {:?}", v.held);
    v.on_term(ev(KeyCode::Char('x'), Mods::NONE, K::Press)).unwrap();
    v.on_term(ev(ctrl, Mods::CONTROL, K::Press)).unwrap();
    v.on_term(Event::FocusLost).unwrap();
    assert!(v.held.is_empty() && v.held_mods == 0, "all let go when the terminal goes to the back");
}

/// The terminal viewer goes between the screen, a shell kept open, the files and the chat on
/// one connection, as the window does, and asks before disconnecting.
#[test]
#[ignore]
fn the_terminal_viewer_switches_modes_on_one_connection() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use super::*;
    static SCRATCH: std::sync::Once = std::sync::Once::new();
    SCRATCH.call_once(|| {
        let dir = std::env::temp_dir().join(format!("sshremote-cli-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for var in ["APPDATA", "XDG_CONFIG_HOME"] {
            unsafe { std::env::set_var(var, &dir) };
        }
    });
    let opt = crate::parse_client(&["--local".into()]).unwrap();
    let (sess, _tx, rx) = Session::connect(&opt).expect("a local agent (SSHREMOTE_EXE)");
    let screen = Screen::default();
    let mut v = Viewer::new(&opt, sess, Box::new(screen.clone()));
    (v.cols, v.rows) = (100, 30);
    let key = |c: KeyCode| Event::Key(KeyEvent::new(c, KeyModifiers::NONE));
    let menu = Event::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL));
    let pump = |v: &mut Viewer, until: &dyn Fn(&Viewer) -> bool| {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end && !until(v) {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ev::Net(ToClient::Frame(f))) => v.apply(f).unwrap(),
                Ok(Ev::Net(m)) => v.on_message(m).unwrap(),
                _ => {}
            }
        }
        assert!(until(v), "waited too long");
    };
    v.push_settings().unwrap();
    pump(&mut v, &|v| v.fb.w > 0);
    // The menu lists the modes; f goes to the files, listed from the host.
    v.on_term(menu.clone()).unwrap();
    assert_eq!(v.overlay, Overlay::Menu);
    assert!(screen.take().contains("Screen sharing"));
    v.on_term(key(KeyCode::Char('f'))).unwrap();
    assert_eq!(v.view, View::Files);
    pump(&mut v, &|v| !v.files.loading && !v.files.entries.is_empty());
    v.draw_files().unwrap();
    assert!(screen.take().contains("Files on"), "the folder is drawn");
    // The terminal: a shell opens and writes its prompt.
    v.on_term(menu.clone()).unwrap();
    v.on_term(key(KeyCode::Char('t'))).unwrap();
    assert_eq!(v.view, View::Terminal);
    let shell = v.shell.expect("a shell");
    pump(&mut v, &|_| !screen.0.lock().unwrap().is_empty());
    // Back to the screen: the shell stays, and what it writes waits for its return.
    v.on_term(menu.clone()).unwrap();
    v.on_term(key(KeyCode::Char('s'))).unwrap();
    assert_eq!(v.view, View::Screen);
    assert_eq!(v.shell, Some(shell), "the shell is kept");
    v.sess.send(ToAgent::TermInput { id: shell, data: b"echo kept-while-away\r".to_vec() }).unwrap();
    pump(&mut v, &|v| String::from_utf8_lossy(&v.backlog).contains("kept-while-away"));
    screen.take();
    v.on_term(menu.clone()).unwrap();
    v.on_term(key(KeyCode::Char('t'))).unwrap();
    assert!(screen.take().contains("kept-while-away"), "written out on return");
    assert!(v.backlog.is_empty());
    // The chat, with its history.
    v.on_term(menu.clone()).unwrap();
    v.on_term(key(KeyCode::Char('c'))).unwrap();
    for c in "hello".chars() {
        v.on_term(key(KeyCode::Char(c))).unwrap();
    }
    v.on_term(key(KeyCode::Enter)).unwrap();
    assert!(crate::chat::load_history(&v.host).iter().any(|l| !l.0 && l.1 == "hello"), "kept in the history");
    assert!(screen.take().contains("hello"), "and shown");
    v.on_term(key(KeyCode::Esc)).unwrap();
    assert_eq!(v.overlay, Overlay::None);
    // Closing asks; no keeps the connection, y ends it.
    v.on_term(menu.clone()).unwrap();
    v.on_term(key(KeyCode::Char('q'))).unwrap();
    assert_eq!(v.overlay, Overlay::Confirm('q'));
    v.on_term(key(KeyCode::Char('n'))).unwrap();
    assert!(!v.quit);
    v.on_term(menu).unwrap();
    v.on_term(key(KeyCode::Char('q'))).unwrap();
    v.on_term(key(KeyCode::Char('y'))).unwrap();
    assert!(v.quit, "y closes");
    v.sess.close();
}

/// The shell's program's modes are followed, so they can be set again after the menu.
#[test]
fn shell_modes_are_followed() {
    let mut m = super::ShellModes::default();
    m.follow(b"[?1049h[?25l[?1000;1006h text");
    assert!(m.alt && m.cursor_hidden);
    assert_eq!(m.restore(), "[?1000h[?1006h[?25l");
    m.follow(b"[?1000l[?1049l[?25h");
    assert!(!m.alt);
    assert_eq!(m.restore(), "[?1006h[?25h");
}

/// Names and messages from the other side cannot drive this terminal.
#[test]
fn text_from_the_host_is_made_harmless() {
    let evil = "file]52;c;aGk=.txt";
    let shown = super::clean(evil);
    assert!(!shown.contains('') && !shown.contains(''), "{shown:?}");
    assert_eq!(super::visible_len(&shown), shown.chars().count());
}

#[test]
fn cell_report() {
    assert_eq!(super::parse_cell_report(b"[6;20;10t"), Some((10, 20)));
    assert_eq!(super::parse_cell_report(b"junk[6;17;8t"), Some((8, 17)));
    assert_eq!(super::parse_cell_report(b"[4;600;1200t"), None);
}
