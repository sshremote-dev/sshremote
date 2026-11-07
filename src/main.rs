mod admin;
mod agent;
mod audio;
mod backup;
mod bilevel;
mod capture;
mod chat;
mod client;
mod clip;
mod color;
#[cfg(windows)]
mod conpty;
mod files;
mod filewin;
mod gui;
mod hosts;
#[cfg(windows)]
mod keygrab;
mod keys;
mod markdown;
mod mcp;
mod net;
mod proto;
mod pty;
mod quant;
#[cfg(windows)]
mod relay;
mod scroll;
#[cfg(windows)]
mod service;
mod session;
mod shot;
mod sixel;
mod sys;
mod termwin;
mod tools;
mod transport;
mod ui;
mod vault;
mod xk;
mod video;

use color::Mode;

const USAGE: &str = "\
usage: sshremote                       open the window: connect screen, recent hosts
       sshremote [options] HOST        view HOST's desktop over ssh in this terminal
       sshremote --gui [options] [HOST] the same in a window
       sshremote --term [options]      choose a recent or ~/.ssh/config host, then view it here
       sshremote mcp [options] HOST    MCP server on stdio so an AI agent can see and operate HOST
       sshremote mcp --here            the same for this computer's own screen
       sshremote terminal [HOST [PORT]] [--split HOST] [--split-down HOST] [--view V] [-- CMD...]
                                  the terminal window: a local shell, or ssh to HOST; --split adds
                                  panes beside or below (HOST local: a shell here); --view text or
                                  markdown starts in that view; -- runs CMD in the first tab
       sshremote --local [options]     view a built-in test desktop (no ssh)
       sshremote --agent [--capture B] remote side, started by the viewer
       sshremote --serve [--listen ADDR] [--via RELAY] [--password P]
                                  wait for viewers without ssh (prints a one-time password)
       sshremote --relay [--listen ADDR]  pair agents and viewers behind NAT
       sshremote sshremote://HOST[:PORT] --password P     view a --serve agent directly
       sshremote sshremote://ID@RELAY[:PORT] --password P view one through a relay

options:
  --preset P      auto (default), or 0 (black and white, 50%, 5 fps) .. 4 (full colour, 60 fps)
  --budget KB/s   traffic auto stays under; default 100
  --mode M        mono gray4 gray16 16c 256c true
  --scale P       capture scale in percent, 10-100
  --fps N         frame rate cap, 1-60
  --cell WxH      terminal cell size in pixels (default: ask the terminal, else 10x20)
  --capture B     agent capture backend: auto gnome windows synthetic
  --remote CMD    agent command on HOST (default: sshremote --agent)
  --ssh ARG       extra ssh argument, repeatable (e.g. --ssh -p --ssh 2222)
  --sound         play the host's sound (about 8 KB/s)
  --fullscreen    window mode: start in full screen (Ctrl+Alt+Enter leaves it)
  --open WHAT     window mode: start with the host's terminal in place of its screen (terminal),
                  or with its files and the screen paused (files)
  --probe SECS    run headless for SECS and print traffic (with --view WxH, --dump out.ppm)

keys: Ctrl+] opens the menu (q quit, 0-4 presets, a auto, m colour, +/- scale, f fps, z zoom, r refresh)";

fn parse_client(args: &[String]) -> Result<session::Options, String> {
    let mut o = session::Options {
        host: None,
        local: false,
        gui: false,
        remote: session::REMOTE.into(),
        capture: String::new(),
        ssh_args: Vec::new(),
        password: None,
        preset: None,
        budget: 100.0,
        mode: None,
        scale: None,
        fps: None,
        cell: None,
        probe: None,
        probe_type: String::new(),
        probe_scroll: 0,
        probe_send: None,
        probe_fetch: None,
        probe_monitor: None,
        sound: false,
        open: String::new(),
    };
    let mut view = (1900u16, 1000u16);
    let mut dump = None;
    let mut probe = None;
    let mut here = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--local" => o.local = true,
            // This computer's own screen, through a local agent: for MCP on the machine it runs on.
            "--here" => {
                o.local = true;
                here = true;
            }
            "--gui" => o.gui = true,
            "--term" => {}
            "--preset" => {
                let v = val()?;
                o.preset = if v == "auto" { None } else { Some(v.parse().map_err(|_| "bad --preset")?) };
            }
            "--budget" => o.budget = val()?.parse().map_err(|_| "bad --budget")?,
            "--mode" => o.mode = Some(Mode::parse(&val()?).ok_or("bad --mode")?),
            "--scale" => o.scale = Some(val()?.parse().map_err(|_| "bad --scale")?),
            "--fps" => o.fps = Some(val()?.parse().map_err(|_| "bad --fps")?),
            "--cell" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("bad --cell")?;
                o.cell = Some((w.parse().map_err(|_| "bad --cell")?, h.parse().map_err(|_| "bad --cell")?));
            }
            "--capture" => o.capture = val()?,
            "--remote" => o.remote = val()?,
            "--ssh" => o.ssh_args.push(val()?),
            "--password" => o.password = Some(val()?),
            "--sound" => o.sound = true,
            "--fullscreen" => o.open.push_str(" fullscreen"),
            "--open" => o.open = val()?,
            "--probe" => probe = Some(val()?.parse::<f64>().map_err(|_| "bad --probe")?),
            "--view" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("bad --view")?;
                view = (w.parse().map_err(|_| "bad --view")?, h.parse().map_err(|_| "bad --view")?);
            }
            "--dump" => dump = Some(val()?),
            "--type" => o.probe_type = val()?,
            "--send" => o.probe_send = Some(val()?),
            "--fetch" => o.probe_fetch = Some(val()?),
            "--monitor" => o.probe_monitor = Some(val()?.parse().map_err(|_| "bad --monitor")?),
            "--scroll" => o.probe_scroll = val()?.parse().map_err(|_| "bad --scroll")?,
            "-h" | "--help" => return Err(USAGE.into()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}\n\n{USAGE}")),
            s => o.host = Some(s.into()),
        }
    }
    o.probe = probe.map(|secs| (secs, view, dump));
    if o.capture.is_empty() {
        o.capture = if o.local && !here { "synthetic" } else { "auto" }.into();
    }
    if !o.local && !o.gui && o.host.is_none() {
        // `sshremote --term` without a host: choose one in the terminal.
        if !args.iter().any(|a| a == "--term") {
            return Err(USAGE.into());
        }
        o.host = Some(hosts::pick().ok_or("no host chosen")?);
    }
    if let Some(h) = o.host.as_deref().filter(|_| !o.local && o.probe.is_none()) {
        hosts::remember(h);
    }
    Ok(o)
}

/// Gives command-line use a console: the parent's, or a new one, with the standard handles on it.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetConsoleWindow, GetStdHandle, STD_ERROR_HANDLE,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        // Redirected (ssh, a pipe, a file) or already on a console: nothing to do.
        if !GetConsoleWindow().is_null() || (!out.is_null() && out != INVALID_HANDLE_VALUE) {
            return;
        }
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 && AllocConsole() == 0 {
            return;
        }
        let open = |name: &str, access: u32| {
            let w: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            CreateFileW(
                w.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        let conout = open("CONOUT$", GENERIC_READ | GENERIC_WRITE);
        let conin = open("CONIN$", GENERIC_READ | GENERIC_WRITE);
        if conout != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, conout);
            SetStdHandle(STD_ERROR_HANDLE, conout);
        }
        if conin != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_INPUT_HANDLE, conin);
        }
    }
}

/// Runs ssh so the saved password of `host` answers its password prompt (once, so a stale password
/// fails fast), and a new host key is accepted as `yes` would.
fn saved_login(host: &str, argv: &[String]) -> i32 {
    let mut cmd = std::process::Command::new(&argv[0]);
    if let (Some(secret), Ok(exe)) = (vault::get(host), std::env::current_exe()) {
        cmd.args(["-o", "NumberOfPasswordPrompts=1", "-o", "StrictHostKeyChecking=accept-new"]);
        cmd.env("SSH_ASKPASS", exe).env("SSH_ASKPASS_REQUIRE", "force").env(transport::ASKPASS_SECRET, secret);
    }
    cmd.args(&argv[1..]);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = cmd.exec();
        eprintln!("sshremote: {}: {e}", argv[0]);
        1
    }
    #[cfg(windows)]
    match cmd.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("sshremote: {}: {e}", argv[0]);
            1
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // ssh runs this program as its askpass helper with the prompt as the only argument.
    if let Ok(secret) = std::env::var(transport::ASKPASS_SECRET)
        && args.len() == 1
        && !args[0].starts_with("--")
    {
        // Only a password or passphrase prompt gets the secret; anything else is declined.
        let prompt = args[0].to_lowercase();
        if !prompt.contains("password") && !prompt.contains("passphrase") {
            std::process::exit(1);
        }
        println!("{secret}");
        return;
    }
    // `--saved-login HOST -- ssh ...`: ssh with the password saved for HOST answering its prompt.
    if args.first().is_some_and(|a| a == "--saved-login") && args.len() > 3 && args[2] == "--" {
        std::process::exit(saved_login(&args[1], &args[3..]));
    }
    if args.is_empty() {
        // Started from a file manager or a shortcut: open the window.
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
            // A console that holds only this process was made for it; drop it rather than show an empty window.
            let mut pids = [0u32; 2];
            if GetConsoleProcessList(pids.as_mut_ptr(), 2) == 1 {
                FreeConsole();
            }
        }
        // Administrator rights are asked for at start; without them the window works and shows
        // what needs them disabled.
        if admin::ask_at_start() && !admin::elevated() && admin::relaunch(&[]).unwrap_or(false) {
            return;
        }
        let opt = parse_client(&["--gui".to_string()]).expect("default options");
        if let Err(e) = client::run(opt) {
            eprintln!("sshremote: {e}");
            std::process::exit(1);
        }
        return;
    }
    // Without a console of its own (a double click under the manifest's detached policy), command-line
    // use borrows the terminal it was started from, or opens one; window modes and helpers need none.
    #[cfg(windows)]
    if !args.iter().any(|a| ["--gui", "--service", "--chat-window", "--agent"].contains(&a.as_str()))
        && args[0] != "terminal"
    {
        attach_console();
    }
    if args[0] == "terminal" {
        // HOST [PORT], then --split HOST / --split-down HOST for panes beside or below; "local" is a shell here.
        let profile = |host: &str, port: &str| {
            if host == "local" {
                termwin::local_profiles().into_iter().next().expect("a local shell")
            } else {
                termwin::ssh_profile(host, port)
            }
        };
        let mut first = None;
        let mut panes = Vec::new();
        let mut view = None;
        let mut it = args[1..].iter().peekable();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--view" => view = it.next().cloned(),
                // Everything after -- is a command to run in the first tab.
                "--" => {
                    let argv: Vec<String> = it.by_ref().cloned().collect();
                    let name = argv.first().cloned().unwrap_or_default();
                    first =
                        Some(termwin::Profile { name, icon: gui::ic::TERMINAL, launch: termwin::Launch::Local(argv) });
                }
                "--split" | "--split-down" => {
                    let host = it.next().map_or("local", |s| s.as_str());
                    panes.push((profile(host, ""), a == "--split"));
                }
                host if first.is_none() => {
                    let port = it.next_if(|p| p.parse::<u16>().is_ok()).map_or("", |s| s.as_str());
                    first = Some(profile(host, port));
                }
                _ => {}
            }
        }
        let first = first.unwrap_or_else(|| profile("local", ""));
        if let Err(e) = termwin::run_window(first, panes, view) {
            eprintln!("sshremote: {e}");
            std::process::exit(1);
        }
        return;
    }
    #[cfg(windows)]
    for (flag, f) in [
        ("--install-service", service::install as fn() -> Result<(), String>),
        ("--uninstall-service", service::uninstall),
        ("--service", service::dispatch),
    ] {
        if args.first().is_some_and(|a| a == flag) {
            if let Err(e) = f() {
                eprintln!("sshremote: {e}");
                std::process::exit(1);
            }
            return;
        }
    }
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str());
    if args.first().is_some_and(|a| a == "--serve") {
        let r = net::serve(opt("--listen"), opt("--via"), opt("--password"), opt("--capture").unwrap_or("auto"));
        if let Err(e) = r {
            eprintln!("sshremote: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.first().is_some_and(|a| a == "mcp") {
        // stdout carries JSON-RPC only; everything else goes to stderr.
        let r = parse_client(&args[1..]).and_then(|o| mcp::run(&o));
        if let Err(e) = r {
            eprintln!("sshremote mcp: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.first().is_some_and(|a| a == "--chat-window") {
        if let Err(e) = chat::run_window() {
            eprintln!("sshremote: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.first().is_some_and(|a| a == "--relay") {
        if let Err(e) = net::relay(opt("--listen")) {
            eprintln!("sshremote: {e}");
            std::process::exit(1);
        }
        return;
    }
    let result = if args.iter().any(|a| a == "--agent") {
        let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str());
        let r = agent::run(opt("--capture").unwrap_or("auto"), opt("--pipe"));
        // A relay helper has no visible stderr; keep its failure where it can be found.
        if let (Err(e), Some(_)) = (&r, opt("--pipe")) {
            let _ = std::fs::write(std::env::temp_dir().join("sshremote-agent.log"), e);
        }
        r
    } else {
        parse_client(&args).and_then(client::run)
    };
    if let Err(e) = result {
        eprintln!("sshremote: {e}");
        std::process::exit(1);
    }
}
