mod agent;
mod audio;
mod capture;
mod client;
mod clip;
mod color;
#[cfg(windows)]
mod conpty;
mod files;
mod gui;
#[cfg(windows)]
mod keygrab;
mod keys;
mod net;
mod proto;
mod quant;
#[cfg(windows)]
mod relay;
mod scroll;
#[cfg(windows)]
mod service;
mod session;
mod sixel;
mod transport;
mod video;

use color::Mode;

const USAGE: &str = "\
usage: desk                       open the window: connect screen, recent hosts
       desk [options] HOST        view HOST's desktop over ssh in this terminal
       desk --gui [options] [HOST] the same in a window
       desk --local [options]     view a built-in test desktop (no ssh)
       desk --agent [--capture B] remote side, started by the viewer
       desk --serve [--listen ADDR] [--via RELAY] [--password P]
                                  wait for viewers without ssh (prints a one-time password)
       desk --relay [--listen ADDR]  pair agents and viewers behind NAT
       desk desk://HOST[:PORT] --password P     view a --serve agent directly
       desk desk://ID@RELAY[:PORT] --password P view one through a relay

options:
  --preset P      auto (default), or 0 glance (mono) .. 4 exact
  --budget KB/s   traffic auto stays under; default 100
  --mode M        mono gray4 gray16 16c 256c true
  --scale P       capture scale in percent, 10-100
  --fps N         frame rate cap, 1-60
  --cell WxH      terminal cell size in pixels (default: ask the terminal, else 10x20)
  --capture B     agent capture backend: auto gnome windows synthetic
  --remote CMD    agent command on HOST (default: desk --agent)
  --ssh ARG       extra ssh argument, repeatable (e.g. --ssh -p --ssh 2222)
  --sound         play the host's sound (about 8 KB/s)
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
    };
    let mut view = (1900u16, 1000u16);
    let mut dump = None;
    let mut probe = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--local" => o.local = true,
            "--gui" => o.gui = true,
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
        o.capture = if o.local { "synthetic" } else { "auto" }.into();
    }
    if !o.local && !o.gui && o.host.is_none() {
        return Err(USAGE.into());
    }
    Ok(o)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // ssh runs this program as its askpass helper with the prompt as the only argument.
    if let Ok(secret) = std::env::var(transport::ASKPASS_SECRET)
        && args.len() == 1
        && !args[0].starts_with("--")
    {
        println!("{secret}");
        return;
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
        let opt = parse_client(&["--gui".to_string()]).expect("default options");
        if let Err(e) = client::run(opt) {
            eprintln!("desk: {e}");
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
                eprintln!("desk: {e}");
                std::process::exit(1);
            }
            return;
        }
    }
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str());
    if args.first().is_some_and(|a| a == "--serve") {
        let r = net::serve(opt("--listen"), opt("--via"), opt("--password"), opt("--capture").unwrap_or("auto"));
        if let Err(e) = r {
            eprintln!("desk: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.first().is_some_and(|a| a == "--relay") {
        if let Err(e) = net::relay(opt("--listen")) {
            eprintln!("desk: {e}");
            std::process::exit(1);
        }
        return;
    }
    let result = if args.iter().any(|a| a == "--agent") {
        let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str());
        let r = agent::run(opt("--capture").unwrap_or("auto"), opt("--pipe"));
        // A relay helper has no visible stderr; keep its failure where it can be found.
        if let (Err(e), Some(_)) = (&r, opt("--pipe")) {
            let _ = std::fs::write(std::env::temp_dir().join("desk-agent.log"), e);
        }
        r
    } else {
        parse_client(&args).and_then(client::run)
    };
    if let Err(e) = result {
        eprintln!("desk: {e}");
        std::process::exit(1);
    }
}
