// A build with only some parts leaves helpers the others use unused.
#![cfg_attr(not(all(feature = "gui", feature = "remote")), allow(dead_code, unused_imports))]
mod admin;
#[cfg(feature = "remote")]
mod agent;
#[cfg(feature = "remote")]
mod audio;
mod backup;
mod bilevel;
#[cfg(feature = "remote")]
mod capture;
#[cfg(feature = "remote")]
mod chat;
#[cfg(feature = "remote")]
mod client;
#[cfg(feature = "remote")]
mod clip;
mod color;
#[cfg(windows)]
#[cfg(feature = "remote")]
mod conpty;
mod ext;
mod exthost;
#[cfg(feature = "remote")]
mod files;
#[cfg(all(feature = "gui", feature = "remote"))]
mod filewin;
#[cfg(feature = "gui")]
mod gui;
mod hosts;
#[cfg(windows)]
#[cfg(feature = "gui")]
mod keygrab;
mod keys;
mod license;
mod modules;
mod options;
#[cfg(feature = "gui")]
mod markdown;
#[cfg(feature = "mcp")]
mod mcp;
#[cfg(feature = "remote")]
mod net;
mod proto;
mod pty;
#[cfg(feature = "remote")]
mod quant;
#[cfg(windows)]
#[cfg(feature = "remote")]
mod relay;
mod scroll;
#[cfg(all(windows, feature = "remote"))]
mod service;
#[cfg(feature = "remote")]
mod session;
mod shared;
#[cfg(feature = "remote")]
mod shot;
#[cfg(feature = "remote")]
mod sixel;
mod script;
mod sys;
#[cfg(feature = "gui")]
mod termwin;
#[cfg(feature = "gui")]
mod tools;
#[cfg(feature = "remote")]
mod transport;
#[cfg(feature = "remote")]
mod ui;
mod vault;
#[cfg(feature = "web")]
mod web;
// Some keys are only sent on to Windows.
#[cfg(feature = "remote")]
mod video;
#[cfg_attr(not(windows), allow(dead_code))]
mod xk;

use color::Mode;

const USAGE: &str = "\
usage: sshremote                       open the window: connect screen, recent hosts
       sshremote [options] HOST        view HOST's desktop over ssh in this terminal
       sshremote --gui [options] [HOST] the same in a window
       sshremote --term [options]      choose a recent or ~/.ssh/config host, then view it here
       sshremote mcp [options] HOST    MCP server on stdio so an AI agent can see and operate HOST
       sshremote mcp --here            the same for this computer's own screen
       sshremote terminal [HOST [PORT]] [--split HOST] [--split-down HOST] [--view V] [-- CMD...]
                                  the workspace with a terminal pane: a local shell, or ssh to HOST;
                                  --split adds panes beside or below (HOST local: a shell here);
                                  --view text or markdown starts the first in that view; -- runs CMD
       sshremote license [show | install FILE | remove]   the premium license
       sshremote module [list | on ID | off ID]   turn parts of the app on or off
       sshremote web [--listen ADDR] [--password-file F]
                                  terminals in a browser: this computer and its ~/.ssh/config hosts
       sshremote ext list|available|install FILE-OR-ID|update|remove ID|pack DIR OUT.sshx|dev DIR
                                  extensions: style packs from sshremote.dev or a file
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
                  its files and the screen paused (files), or beside the screen a terminal panel
                  (panel) and the chat (chat); several in one value, as 'panel chat'
  --probe SECS    run headless for SECS and print traffic (with --view WxH, --dump out.ppm)

keys: Ctrl+] opens the menu (q quit, 0-4 presets, a auto, m colour, +/- scale, f fps, z zoom, r refresh)";

fn parse_client(args: &[String]) -> Result<options::Options, String> {
    let mut o = options::Options {
        host: None,
        local: false,
        gui: false,
        remote: options::REMOTE.into(),
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
        project: None,
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
            "--project" => {
                o.project = Some(val()?);
                o.gui = true;
            }
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
        cmd.env("SSH_ASKPASS", exe).env("SSH_ASKPASS_REQUIRE", "force").env(vault::ASKPASS_SECRET, secret);
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

/// A crash leaves its reason in the settings folder: a window started without a console has
/// nowhere else to say it.
fn note_crashes() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = hosts::config_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let when = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
            let text = format!(
                "sshremote {} crashed at {} (unix time {when}), thread {thread}:
{info}
{}

",
                env!("CARGO_PKG_VERSION"),
                sys::local_time(when),
                std::backtrace::Backtrace::force_capture()
            );
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("crash.txt")) {
                let _ = f.write_all(text.as_bytes());
            }
        }
        default(info);
    }));
}

fn main() {
    note_crashes();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // ssh runs this program as its askpass helper with the prompt as the only argument.
    if let Ok(secret) = std::env::var(vault::ASKPASS_SECRET)
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
        if let Err(e) = open_window(opt) {
            // No console here: the reason goes beside the settings too.
            if let Some(dir) = hosts::config_dir() {
                let _ = std::fs::write(
                    dir.join("crash.txt"),
                    format!(
                        "sshremote stopped: {e}
"
                    ),
                );
            }
            eprintln!("sshremote: {e}");
            std::process::exit(1);
        }
        return;
    }
    // Without a console of its own (a double click under the manifest's detached policy), command-line
    // use borrows the terminal it was started from, or opens one; window modes and helpers need none.
    #[cfg(windows)]
    if !args.iter().any(|a| ["--gui", "--service", "--chat-window", "--agent", "--stats"].contains(&a.as_str()))
        && args[0] != "terminal"
    {
        attach_console();
    }
    if let Err(e) = run(&args) {
        let who = if args[0] == "mcp" { "sshremote mcp" } else { "sshremote" };
        eprintln!("{who}: {e}");
        std::process::exit(1);
    }
}

/// Options that take a value, from `args`: only those in `known` are accepted, and each needs its value.
fn options(args: &[String], known: &[&str]) -> Result<std::collections::HashMap<String, String>, String> {
    let mut out = std::collections::HashMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if !known.contains(&a.as_str()) {
            return Err(format!("unknown option {a}\n\n{USAGE}"));
        }
        out.insert(a.clone(), it.next().cloned().ok_or(format!("{a} needs a value"))?);
    }
    Ok(out)
}

/// Runs what the command line asks for.
fn run(args: &[String]) -> Result<(), String> {
    match args[0].as_str() {
        #[cfg(feature = "gui")]
        "terminal" => terminal(&args[1..]),
        #[cfg(all(windows, feature = "remote"))]
        "--install-service" => service::install(),
        #[cfg(all(windows, feature = "remote"))]
        "--uninstall-service" => service::uninstall(),
        #[cfg(all(windows, feature = "remote"))]
        "--service" => service::dispatch(),
        #[cfg(feature = "remote")]
        "--serve" => {
            let o = options(&args[1..], &["--listen", "--via", "--password", "--capture"])?;
            let get = |k: &str| o.get(k).map(String::as_str);
            net::serve(get("--listen"), get("--via"), get("--password"), get("--capture").unwrap_or("auto"))
        }
        #[cfg(feature = "remote")]
        "--relay" => net::relay(options(&args[1..], &["--listen"])?.get("--listen").map(String::as_str)),
        #[cfg(all(feature = "gui", feature = "remote"))]
        "--chat-window" => chat::run_window(args.get(1).cloned().unwrap_or_else(|| "Viewer".into())),
        // The load and the time every 2 s, for a viewer to show beside this host's terminals.
        #[cfg(feature = "gui")]
        "--stats" => gui::look::print_stats(),
        #[cfg(not(feature = "gui"))]
        "--stats" | "terminal" => Err(NO_WINDOWS.into()),
        // Extensions: list, install from a file or the registry, update, remove, pack.
        "ext" => ext::cli(&args[1..]),
        "license" => license::cli(&args[1..]),
        "module" => modules::cli(&args[1..]),
        // The gateway browsers reach terminals through (sshremote.dev/web).
        #[cfg(feature = "web")]
        "web" => modules::need("web").and_then(|()| web::run(&args[1..])),
        // Built without them, the registry's extension brings the gateway and the MCP server.
        #[cfg(not(feature = "web"))]
        "web" => modules::need("web").and_then(|()| ext::run_provided("web", &args[1..])),
        // stdout carries JSON-RPC only; everything else goes to stderr.
        #[cfg(feature = "mcp")]
        "mcp" => modules::need("mcp").and_then(|()| parse_client(&args[1..])).and_then(|o| mcp::run(&o)),
        #[cfg(not(feature = "mcp"))]
        "mcp" => modules::need("mcp").and_then(|()| ext::run_provided("mcp", &args[1..])),
        // The viewer starts the agent as `REMOTE` with options after it; options a newer viewer
        // adds are passed over, so the two versions still meet.
        #[cfg(feature = "remote")]
        _ if args.iter().any(|a| a == "--agent") => {
            let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).map(String::as_str);
            let pipe = get("--pipe");
            let r = agent::run(get("--capture").unwrap_or("auto"), pipe);
            // A relay helper has no visible stderr; keep its failure where it can be found.
            if let (Err(e), Some(_)) = (&r, pipe) {
                let _ = std::fs::write(std::env::temp_dir().join("sshremote-agent.log"), e);
            }
            r
        }
        #[cfg(feature = "remote")]
        _ => parse_client(args).and_then(client::run),
        // Without them here, the Remote desktop extension brings screens, files, the agent and
        // the terminal viewer; the window itself opens here.
        #[cfg(not(feature = "remote"))]
        _ => {
            // The agent, the service and --serve/--relay go to the extension as they are.
            if let Some(opt) = parse_client(args).ok().filter(|o| o.gui && o.host.is_none() && !o.local) {
                return open_window(opt);
            }
            ext::run_provided("remote", args)
        }
    }
}

/// What a build without windows says when one is asked for.
#[cfg(not(feature = "gui"))]
pub const NO_WINDOWS: &str = "this sshremote has no windows (a build for the command line and services)";

/// The open window: the workspace and its tools.
fn open_window(opt: options::Options) -> Result<(), String> {
    #[cfg(feature = "remote")]
    return client::run(opt);
    #[cfg(all(feature = "gui", not(feature = "remote")))]
    return gui::run(&opt);
    #[cfg(not(any(feature = "gui", feature = "remote")))]
    {
        let _ = opt;
        Err(NO_WINDOWS.into())
    }
}

#[cfg(feature = "gui")]
/// `terminal [HOST [PORT]] [--split HOST] [--split-down HOST] [--view V] [-- CMD...]`.
fn terminal(args: &[String]) -> Result<(), String> {
    // "local" is a shell here.
    let shell = |host: &str, port: &str| {
        if host == "local" { gui::TermStart::Local } else { gui::TermStart::Host(host.to_string(), port.to_string()) }
    };
    let mut first = None;
    let mut panes = Vec::new();
    let mut view = None;
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--view" => {
                let v = it.next().ok_or("--view needs a value")?;
                if !["terminal", "text", "markdown"].contains(&v.as_str()) {
                    return Err(format!("--view is terminal, text or markdown, not {v}"));
                }
                view = Some(v.clone());
            }
            // Everything after -- is a command to run in the first pane.
            "--" => first = Some(gui::TermStart::Command(it.by_ref().cloned().collect())),
            "--split" | "--split-down" => {
                let host = it.next().ok_or(format!("{a} needs a host (local for a shell here)"))?;
                panes.push((shell(host, ""), a == "--split"));
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s}\n\n{USAGE}")),
            host if first.is_none() => {
                let port = it.next_if(|p| p.parse::<u16>().is_ok()).map_or("", |s| s.as_str());
                first = Some(shell(host, port));
            }
            extra => return Err(format!("one host only, then --split for more panes (not {extra})")),
        }
    }
    gui::run_terminal(first.unwrap_or(gui::TermStart::Local), panes, view)
}
