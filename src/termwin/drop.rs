//! Files dropped on a terminal, or files and pictures pasted into it: copied into a folder of
//! their own on the computer its shell runs on (beside the user's cache, cleared of what is a day
//! old as new ones come), then their paths typed at the prompt as a terminal types a dropped
//! file's, so a program there (an AI agent reading a screenshot, an editor) can open them.
//!
//! A local shell needs no copy: its paths are typed as they are. A shell over `ssh HOST` gets
//! them over a second ssh connection (its keys, or the password saved for it); one of a remote
//! session's agent through the session's own file transfer (`Cmd::Upload`).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::*;

/// Where a pane's shell runs, for what is dropped on it.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Place {
    Here,
    /// `opts`: the options of the pane's own ssh command (its user, key, jump host, config).
    Ssh { host: String, port: Option<String>, opts: Vec<String> },
    Agent,
}

pub(super) fn place(t: &Tab) -> Place {
    match (&t.backend, &t.profile.launch) {
        (Backend::Agent, _) => Place::Agent,
        (_, Launch::Local(argv)) => ssh_of(argv).map_or(Place::Here, |(host, port, opts)| Place::Ssh { host, port, opts }),
        _ => Place::Here,
    }
}

/// The host, port and options of an `ssh` command line (ssh itself, or after the saved-login
/// wrapper): the options that choose who and how (-l, -i, -J, -F, -o...) go with the copy too;
/// those that would forward ports, ask for a terminal or go to the background do not.
pub(super) fn ssh_of(argv: &[String]) -> Option<(String, Option<String>, Vec<String>)> {
    let is_ssh = |a: &String| {
        let name = a.rsplit(['/', '\\']).next().unwrap_or(a).to_ascii_lowercase();
        name == "ssh" || name == "ssh.exe"
    };
    let at = if argv.first().is_some_and(is_ssh) {
        0
    } else {
        // sshremote --saved-login HOST -- ssh ...
        let dash = argv.iter().position(|a| a == "--")?;
        (argv.get(1).is_some_and(|a| a == "--saved-login") && argv.get(dash + 1).is_some_and(is_ssh)).then_some(dash + 1)?
    };
    // Options of ssh that take a value, and those left out of a copy.
    const VALUED: &[&str] = &["-B", "-b", "-c", "-D", "-E", "-e", "-F", "-I", "-i", "-J", "-L", "-l", "-m", "-O", "-o", "-P", "-Q", "-R", "-S", "-W", "-w"];
    const LEFT_OUT: &[&str] = &["-D", "-L", "-R", "-W", "-O", "-S", "-E", "-w", "-e", "-Q"];
    let mut port = None;
    let mut opts = Vec::new();
    let mut it = argv[at + 1..].iter();
    while let Some(a) = it.next() {
        if a == "-p" {
            port = it.next().cloned();
        } else if let Some(p) = a.strip_prefix("-p").filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
            port = Some(p.to_string());
        } else if VALUED.contains(&a.as_str()) {
            let v = it.next()?;
            if !LEFT_OUT.contains(&a.as_str()) {
                opts.extend([a.clone(), v.clone()]);
            }
        } else if a.len() > 2 && a.starts_with('-') && VALUED.contains(&&a[..2]) {
            // A value written on: -lalice, -oUser=alice.
            if !LEFT_OUT.contains(&&a[..2]) {
                opts.push(a.clone());
            }
        } else if let Some(flags) = a.strip_prefix('-') {
            // Flags: those that change only how it goes (-4, -6, -A, -C, -q, -v...) stay.
            let kept: String = flags.chars().filter(|c| !"tTNfnMsGVxXyY".contains(*c)).collect();
            if !kept.is_empty() {
                opts.push(format!("-{kept}"));
            }
        } else {
            return Some((a.clone(), port, opts));
        }
    }
    None
}

/// What has come of a copy.
pub(super) type Outcome = Result<Vec<String>, String>;

/// A copy under way, or just ended, on a pane.
pub(super) struct Job {
    /// What is copied, as it is shown: "screenshot.png", "3 files".
    pub what: String,
    /// Where it goes, as it is shown: the host's name.
    pub to: String,
    pub total: Arc<AtomicU64>,
    pub done: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
    /// Set once it ends: the paths there, or why not.
    pub end: Arc<Mutex<Option<Outcome>>>,
    /// Paths typed at the prompt already, and since when it ended (in egui's time), for the note.
    pub typed: bool,
    pub ended_at: Option<f64>,
    /// The paths are for a Windows computer (quoted as its shells read them).
    pub windows: bool,
}

impl Job {
    fn new(what: String, to: String, total: u64) -> Job {
        Job {
            what,
            to,
            total: Arc::new(AtomicU64::new(total)),
            done: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            end: Arc::new(Mutex::new(None)),
            typed: false,
            ended_at: None,
            windows: cfg!(windows),
        }
    }

    fn running(&self) -> bool {
        self.end.lock().unwrap_or_else(|e| e.into_inner()).is_none()
    }
}

fn label(paths: &[PathBuf]) -> String {
    match paths {
        [one] => one.file_name().map_or_else(|| one.display().to_string(), |n| n.to_string_lossy().into_owned()),
        many => format!("{} files", many.len()),
    }
}

fn size_of(paths: &[PathBuf]) -> u64 {
    let mut total = 0;
    for p in paths {
        if p.is_dir() {
            let mut files = Vec::new();
            crate::files::walk(p, "", &mut files);
            total += files.iter().filter_map(|(f, _)| std::fs::metadata(f).ok()).map(|m| m.len()).sum::<u64>();
        } else {
            total += std::fs::metadata(p).map_or(0, |m| m.len());
        }
    }
    total
}

/// Starts copying `paths` for the shell of pane `t`; an agent's goes out as `Cmd::Upload`.
pub(super) fn start(t: &mut Tab, paths: Vec<PathBuf>, out: &mut Vec<Cmd>) {
    let paths: Vec<PathBuf> = paths.into_iter().filter(|p| !p.as_os_str().is_empty() && p.exists()).collect();
    // A shell that ended (or never started) takes nothing.
    if paths.is_empty() || t.exited.is_some() || t.drop.as_ref().is_some_and(Job::running) {
        return;
    }
    let what = label(&paths);
    match place(t) {
        Place::Here => {
            // A name the shell would read otherwise ($ or % in it, a control character) is
            // copied under one it reads whole; the others are typed as they are.
            let odd: Vec<bool> = paths.iter().map(|p| p.to_string_lossy().chars().any(|c| c.is_control() || (cfg!(windows) && "\"$`%!".contains(c)))).collect();
            let size = paths.iter().zip(&odd).filter(|(_, o)| **o).map(|(p, _)| size_of(std::slice::from_ref(p))).sum();
            let job = Job::new(what, "here".into(), size);
            let (done, end) = (job.done.clone(), job.end.clone());
            if odd.iter().any(|o| *o) {
                std::thread::spawn(move || {
                    let r = copy_here(&paths, &odd, &done);
                    *end.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                });
            } else {
                *end.lock().unwrap_or_else(|e| e.into_inner()) = Some(Ok(paths.iter().map(|p| p.display().to_string()).collect()));
            }
            t.drop = Some(job);
        }
        Place::Agent => {
            t.drop = Some(Job::new(what, "the host".into(), size_of(&paths)));
            out.push(Cmd::Upload { tab: t.id, paths });
        }
        Place::Ssh { host, port, opts } => {
            let job = Job::new(what, host.clone(), size_of(&paths));
            let (done, cancel, end) = (job.done.clone(), job.cancel.clone(), job.end.clone());
            let windows = Arc::new(AtomicBool::new(false));
            let w = windows.clone();
            std::thread::spawn(move || {
                let r = ssh_copy(&host, port.as_deref(), &opts, &paths, &done, &cancel, &w);
                *end.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            });
            t.drop = Some(job);
            t.drop_windows = Some(windows);
        }
    }
}

/// Copies the `odd` ones of `paths` into a drop folder here under names a shell reads whole.
fn copy_here(paths: &[PathBuf], odd: &[bool], done: &AtomicU64) -> Outcome {
    let folder = crate::files::drop_dir(&crate::files::batch_name());
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut taken = std::collections::HashSet::new();
    let mut there = Vec::new();
    for (p, odd) in paths.iter().zip(odd) {
        if !odd {
            there.push(p.display().to_string());
            continue;
        }
        let name = crate::files::unique(crate::files::typable(&p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), &mut taken);
        let mut files = Vec::new();
        if p.is_dir() {
            crate::files::walk(p, &name, &mut files);
        } else {
            files.push((p.clone(), name.clone()));
        }
        for (f, rel) in files {
            let to = folder.join(rel.split(['/', '\\']).map(crate::files::typable).collect::<Vec<_>>().join(std::path::MAIN_SEPARATOR_STR));
            if let Some(d) = to.parent() {
                std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
            }
            let n = std::fs::copy(&f, &to).map_err(|e| format!("{}: {e}", f.display()))?;
            done.fetch_add(n, Ordering::AcqRel);
        }
        there.push(folder.join(&name).display().to_string());
    }
    Ok(there)
}

/// What the clipboard holds that is pasted as files: files copied in a file manager, or a
/// picture (a screenshot), kept as a PNG file here first. None when it holds neither.
pub(super) fn clipboard_files() -> Option<Vec<PathBuf>> {
    // Tests never read this computer's clipboard: whatever another program, or another session's
    // desktop the process found, holds there would decide what a key does.
    if cfg!(test) {
        return None;
    }
    let files = crate::clip::files_here();
    if !files.is_empty() {
        return Some(files);
    }
    crate::clip::image_here().map(|p| vec![p])
}

/// The paths as a shell reads them, one after another, with a space after the last.
pub(super) fn typed(paths: &[String], windows: bool) -> String {
    let mut s = String::new();
    for p in paths {
        // Never a control character (a paste's end among them), whatever a name holds.
        let p: String = p.chars().filter(|c| !c.is_control()).collect();
        let p = &p;
        let plain = |c: char| c.is_ascii_alphanumeric() || "-_./:@%+,=~\\".contains(c);
        if windows {
            // cmd splits names at , ; = and reads %: those are quoted.
            if p.chars().all(|c| plain(c) && !"~%,=".contains(c)) {
                s.push_str(p);
            } else {
                s.push('"');
                s.push_str(&p.replace('"', ""));
                s.push('"');
            }
        } else if p.chars().all(|c| plain(c) && c != '\\') {
            s.push_str(p);
        } else {
            s.push('\'');
            s.push_str(&p.replace('\'', "'\\''"));
            s.push('\'');
        }
        s.push(' ');
    }
    s
}

/// Types the paths of a copy that ended, once; says how it went over the pane for a while.
pub(super) fn tick(t: &mut Tab, ui: &egui::Ui, area: Rect, out: &mut Vec<Cmd>) {
    let now = ui.input(|i| i.time);
    if let (Some(w), Some(job)) = (&t.drop_windows, t.drop.as_mut()) {
        job.windows = w.load(Ordering::Acquire);
    }
    let Some(job) = t.drop.as_mut() else { return };
    let end = job.end.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut to_type = None;
    if let Some(r) = &end {
        if job.ended_at.is_none() {
            job.ended_at = Some(now);
        }
        if let Ok(paths) = r
            && !std::mem::replace(&mut job.typed, true)
            && !paths.is_empty()
        {
            to_type = Some(typed(paths, job.windows));
        }
    }
    let shown_for = if matches!(end, Some(Err(_))) { 8.0 } else { 4.0 };
    let gone = job.ended_at.is_some_and(|at| now - at > shown_for) || (matches!(end, Some(Ok(_))) && job.to == "here");
    if let Some(text) = to_type {
        let mut data = Vec::new();
        // As a dropped file's path is pasted by other terminals: a program that reads pastes
        // (an AI agent's prompt) takes it as one.
        if t.term.mode().contains(TermMode::BRACKETED_PASTE) {
            data.extend_from_slice(b"\x1b[200~");
            data.extend_from_slice(text.as_bytes());
            data.extend_from_slice(b"\x1b[201~");
        } else {
            data.extend_from_slice(text.as_bytes());
        }
        out.push(Cmd::Send(ToAgent::TermInput { id: t.id, data }));
    }
    if gone {
        t.drop = None;
        t.drop_windows = None;
        return;
    }
    note(t, ui, area, end, out);
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
}

/// The note in the pane's lower right: what is copied where and how far, or how it ended.
fn note(t: &mut Tab, ui: &egui::Ui, area: Rect, end: Option<Outcome>, out: &mut Vec<Cmd>) {
    let Some(job) = t.drop.as_ref() else { return };
    let (done, total) = (job.done.load(Ordering::Acquire), job.total.load(Ordering::Acquire).max(1));
    let pct = (done.min(total) * 100 / total) as u32;
    let (text, bad) = match &end {
        None => (format!("{}  Copying {} to {}  {pct}%  {} of {}", ic::UPLOAD, job.what, job.to, crate::sys::human(done), crate::sys::human(total)), false),
        Some(Ok(p)) => (format!("{}  {} there, typed at the prompt", ic::CHECK, if p.len() == 1 { "Copied".to_string() } else { format!("{} copied", p.len()) }), false),
        Some(Err(e)) => (format!("{}  {} not copied: {e}", ic::WARNING, job.what), true),
    };
    let running = end.is_none();
    let id = egui::Id::unique(("term-drop", t.id));
    let mut cancel = false;
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(Pos2::new(area.right() - 12.0, area.bottom() - 12.0)).pivot(Align2::RIGHT_BOTTOM).show(ui.ctx(), |ui| {
        let v = ui.visuals().clone();
        egui::Frame::new()
            .fill(v.window_fill)
            .stroke(egui::Stroke::new(1.0, if bad { crate::gui::bad() } else { v.window_stroke.color }))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.set_max_width((area.width() - 40.0).clamp(160.0, 520.0));
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(text).size(12.5).color(if bad { crate::gui::bad() } else { v.text_color() })).truncate());
                    if running && ui.small_button(ic::X).on_hover_text("Stop copying").clicked() {
                        cancel = true;
                    }
                });
                if running {
                    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(160.0), 3.0), Sense::hover());
                    ui.painter().rect_filled(r, CornerRadius::same(1), v.widgets.noninteractive.bg_stroke.color);
                    let fill = r.with_max_x(r.left() + r.width() * pct as f32 / 100.0);
                    ui.painter().rect_filled(fill, CornerRadius::same(1), ACCENT);
                }
            });
    });
    if cancel {
        job.cancel.store(true, Ordering::Release);
        if place(t) == Place::Agent {
            out.push(Cmd::CancelUpload { tab: t.id });
        }
    }
}

/// The hint over a pane while files are dragged over it.
pub(super) fn hint(t: &Tab, ui: &egui::Ui, area: Rect) {
    let text = match place(t) {
        Place::Here => "Drop to type their paths".to_string(),
        Place::Ssh { host, .. } => format!("Drop to copy them to {host} and type their paths"),
        Place::Agent => "Drop to copy them to the host and type their paths".to_string(),
    };
    let p = ui.painter();
    p.rect_filled(area, CornerRadius::same(4), ACCENT.gamma_multiply(0.10));
    p.rect_stroke(area.shrink(2.0), CornerRadius::same(4), egui::Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
    let galley = p.layout_no_wrap(format!("{}  {text}", ic::UPLOAD), FontId::proportional(15.0), ui.visuals().strong_text_color());
    let r = Rect::from_center_size(area.center(), galley.size() + Vec2::new(28.0, 16.0));
    p.rect_filled(r, CornerRadius::same(6), ui.visuals().window_fill);
    p.galley(r.min + Vec2::new(14.0, 8.0), galley, ui.visuals().strong_text_color());
}

fn ssh_command(host: &str, port: Option<&str>, opts: &[String]) -> std::process::Command {
    let mut c = std::process::Command::new("ssh");
    c.arg("-T");
    c.args(opts);
    // A host that stops answering ends the copy instead of holding it.
    c.args(["-o", "ConnectTimeout=15", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"]);
    if let Some(p) = port.filter(|p| !p.is_empty()) {
        c.args(["-p", p]);
    }
    // Its keys, or the password saved for it; never a prompt nobody sees.
    match crate::vault::get(host) {
        Some(pw) => {
            c.args(["-o", "NumberOfPasswordPrompts=1"]);
            c.envs(crate::vault::askpass_env(host, port.and_then(|p| p.parse().ok()), &pw));
        }
        None => {
            c.args(["-o", "BatchMode=yes"]);
        }
    }
    c.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    crate::sys::quiet(&mut c);
    c
}

fn first_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|l| !l.is_empty())
}

/// Copies each file (a folder's files in their places) into a folder of its own in the host's
/// drop folder, over SFTP: ssh's own file transfer, the same on Linux and Windows hosts, with no
/// shell between (Windows' shells stall on large input). What is a day old there is cleared after.
fn ssh_copy(host: &str, port: Option<&str>, opts: &[String], paths: &[PathBuf], done: &AtomicU64, cancel: &Arc<AtomicBool>, windows_out: &AtomicBool) -> Outcome {
    let mut sftp = Sftp::start(host, port, opts)?;
    // Stop ends ssh at once, even while it waits on a host that does not answer.
    let _watch = Watch::start(sftp.child.clone(), cancel.clone());
    let home = sftp.realpath(".")?;
    // "/C:/Users/alex" is how Windows' SFTP server names a folder.
    let windows = home.as_bytes().get(2) == Some(&b':') || home.as_bytes().get(1) == Some(&b':');
    windows_out.store(windows, Ordering::Release);
    let base = format!("{}/{}", home.trim_end_matches('/'), if windows { "AppData/Local/Temp/sshremote-drop" } else { ".cache/sshremote/drop" });
    let folder = format!("{base}/{}", crate::files::batch_name());
    sftp.mkdirs(&folder)?;
    let mut there = Vec::new();
    let mut taken = std::collections::HashSet::new();
    for p in paths {
        if cancel.load(Ordering::Acquire) {
            return Err("stopped".into());
        }
        // Two files of one name (from two folders) both arrive, the second as "name-2".
        let name = crate::files::unique(crate::files::typable(&p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), &mut taken);
        if p.is_dir() {
            let mut files = Vec::new();
            crate::files::walk(p, &name, &mut files);
            sftp.mkdirs(&format!("{folder}/{name}"))?;
            for (f, rel) in files {
                let rel: Vec<String> = rel.split(['/', '\\']).map(crate::files::typable).collect();
                let to = format!("{folder}/{}", rel.join("/"));
                if let Some((dir, _)) = to.rsplit_once('/') {
                    sftp.mkdirs(dir)?;
                }
                sftp.put(&f, &to, done, cancel)?;
            }
            there.push(format!("{folder}/{name}"));
        } else {
            let to = format!("{folder}/{name}");
            sftp.put(p, &to, done, cancel)?;
            there.push(to);
        }
    }
    drop(sftp);
    clear_old(host, port, opts, windows);
    // As the host's own programs name them.
    Ok(there.into_iter().map(|p| if windows { p.trim_start_matches('/').replace('/', "\\") } else { p }).collect())
}

/// Clears what was dropped there over a day ago, in the background.
fn clear_old(host: &str, port: Option<&str>, opts: &[String], windows: bool) {
    let remote = if windows {
        let script = "Get-ChildItem -LiteralPath (Join-Path $env:LOCALAPPDATA 'Temp\\sshremote-drop') -Directory -ErrorAction SilentlyContinue | \
                      Where-Object { $_.LastWriteTime -lt (Get-Date).AddDays(-1) } | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue";
        let utf16: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        use base64::Engine;
        format!("powershell -NoProfile -NonInteractive -EncodedCommand {}", base64::engine::general_purpose::STANDARD.encode(utf16))
    } else {
        "sh -c 'find \"$HOME/.cache/sshremote/drop\" -mindepth 1 -maxdepth 1 -mmin +1440 -exec rm -rf {} + 2>/dev/null'".to_string()
    };
    let mut c = ssh_command(host, port, opts);
    c.arg(host).arg(remote).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    if let Ok(mut child) = c.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// The few requests of SFTP (version 3) a copy needs, over `ssh -s HOST sftp`.
struct Sftp {
    child: Arc<Mutex<std::process::Child>>,
    w: std::process::ChildStdin,
    r: std::io::BufReader<std::process::ChildStdout>,
    next: u32,
    /// The last of what ssh said (read as it comes, so a talkative ssh never stalls).
    said: Arc<Mutex<String>>,
    /// Folders made already.
    made: std::collections::HashSet<String>,
}

/// Ends the copy's ssh when Stop is pressed, until the copy is over.
struct Watch(Arc<AtomicBool>);

impl Watch {
    fn start(child: Arc<Mutex<std::process::Child>>, cancel: Arc<AtomicBool>) -> Watch {
        let finished = Arc::new(AtomicBool::new(false));
        let f = finished.clone();
        std::thread::spawn(move || {
            while !f.load(Ordering::Acquire) {
                if cancel.load(Ordering::Acquire) {
                    let _ = child.lock().unwrap_or_else(|e| e.into_inner()).kill();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
        Watch(finished)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

const FXP_INIT: u8 = 1;
const FXP_VERSION: u8 = 2;
const FXP_OPEN: u8 = 3;
const FXP_CLOSE: u8 = 4;
const FXP_WRITE: u8 = 6;
const FXP_MKDIR: u8 = 14;
const FXP_REALPATH: u8 = 16;
const FXP_STATUS: u8 = 101;
const FXP_HANDLE: u8 = 102;
const FXP_NAME: u8 = 104;
/// Bytes a write carries, and writes in flight at once: a megabyte under way, as sftp keeps.
const CHUNK: usize = 32 << 10;
const IN_FLIGHT: usize = 32;

fn put_str(b: &mut Vec<u8>, s: &[u8]) {
    b.extend_from_slice(&(s.len() as u32).to_be_bytes());
    b.extend_from_slice(s);
}

impl Sftp {
    fn start(host: &str, port: Option<&str>, opts: &[String]) -> Result<Sftp, String> {
        let mut c = ssh_command(host, port, opts);
        c.arg("-s").arg(host).arg("sftp");
        let mut child = c.spawn().map_err(|e| format!("ssh: {e}"))?;
        crate::sys::tie(&child);
        let w = child.stdin.take().ok_or("ssh")?;
        let r = std::io::BufReader::new(child.stdout.take().ok_or("ssh")?);
        let said = Arc::new(Mutex::new(String::new()));
        if let Some(mut e) = child.stderr.take() {
            let said = said.clone();
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                while let Ok(n @ 1..) = e.read(&mut b) {
                    let mut s = said.lock().unwrap_or_else(|e| e.into_inner());
                    s.push_str(&String::from_utf8_lossy(&b[..n]));
                    if s.len() > 8000 {
                        let cut = s.ceil_char_boundary(s.len() - 4000);
                        s.drain(..cut);
                    }
                }
            });
        }
        let mut s = Sftp { child: Arc::new(Mutex::new(child)), w, r, next: 1, said, made: std::collections::HashSet::new() };
        let mut init = vec![FXP_INIT];
        init.extend_from_slice(&3u32.to_be_bytes());
        if s.send(&init).is_err() || !matches!(s.read(), Ok((FXP_VERSION, _))) {
            return Err(s.why("the host's SFTP did not answer"));
        }
        Ok(s)
    }

    /// Why ssh failed, from what it said.
    fn why(&mut self, otherwise: &str) -> String {
        {
            let mut c = self.child.lock().unwrap_or_else(|e| e.into_inner());
            let _ = c.kill();
            let _ = c.wait();
        }
        // What ssh said last, once its output is all read.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let err = self.said.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let why = first_line(&err).unwrap_or(otherwise).to_string();
        if why.to_lowercase().contains("permission denied") { "it needs SSH keys, or a password saved for this host".into() } else { why }
    }

    fn send(&mut self, body: &[u8]) -> std::io::Result<()> {
        self.w.write_all(&(body.len() as u32).to_be_bytes())?;
        self.w.write_all(body)
    }

    /// The next reply: its type and what follows it.
    fn read(&mut self) -> std::io::Result<(u8, Vec<u8>)> {
        let mut len = [0u8; 4];
        self.r.read_exact(&mut len)?;
        let len = u32::from_be_bytes(len) as usize;
        if len == 0 || len > 1 << 20 {
            return Err(std::io::Error::other("a bad SFTP reply"));
        }
        let mut body = vec![0u8; len];
        self.r.read_exact(&mut body)?;
        Ok((body[0], body[1..].to_vec()))
    }

    fn request(&mut self, kind: u8, rest: &[u8]) -> Result<u32, String> {
        let id = self.next;
        self.next += 1;
        let mut b = vec![kind];
        b.extend_from_slice(&id.to_be_bytes());
        b.extend_from_slice(rest);
        self.send(&b).map_err(|_| self.why("the connection ended"))?;
        Ok(id)
    }

    /// A reply's status: Ok, or its message.
    fn status(kind: u8, body: &[u8]) -> Result<(), String> {
        if kind != FXP_STATUS || body.len() < 8 {
            return Err("an unexpected SFTP reply".into());
        }
        let code = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
        if code == 0 {
            return Ok(());
        }
        let msg = body.get(12..).and_then(|m| {
            let n = u32::from_be_bytes([body[8], body[9], body[10], body[11]]) as usize;
            m.get(..n)
        });
        Err(msg.map(|m| String::from_utf8_lossy(m).into_owned()).filter(|m| !m.is_empty()).unwrap_or_else(|| format!("SFTP error {code}")))
    }

    fn reply(&mut self) -> Result<(u8, Vec<u8>), String> {
        self.read().map_err(|_| self.why("the connection ended"))
    }

    fn realpath(&mut self, path: &str) -> Result<String, String> {
        let mut b = Vec::new();
        put_str(&mut b, path.as_bytes());
        self.request(FXP_REALPATH, &b)?;
        let (kind, body) = self.reply()?;
        if kind != FXP_NAME || body.len() < 12 {
            return Self::status(kind, &body).and(Err("no home folder".into()));
        }
        let n = u32::from_be_bytes([body[8], body[9], body[10], body[11]]) as usize;
        body.get(12..12 + n).map(|s| String::from_utf8_lossy(s).into_owned()).ok_or_else(|| "a bad SFTP reply".into())
    }

    /// Makes each folder of `path` that is not there yet.
    fn mkdirs(&mut self, path: &str) -> Result<(), String> {
        let mut at = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            at = if at.is_empty() && !path.starts_with('/') { part.to_string() } else { format!("{at}/{part}") };
            // Its drive ("/C:") is no folder to make, nor one made already.
            if part.ends_with(':') || !self.made.insert(at.clone()) {
                continue;
            }
            let mut b = Vec::new();
            put_str(&mut b, at.as_bytes());
            // Made the user's alone (where the host keeps permissions).
            b.extend_from_slice(&4u32.to_be_bytes());
            b.extend_from_slice(&0o700u32.to_be_bytes());
            self.request(FXP_MKDIR, &b)?;
            // One that is there already says so; the file's own open says if it is not a folder.
            let _ = self.reply()?;
        }
        Ok(())
    }

    /// Writes the file at `from` to `to`, counting what was sent into `done`.
    fn put(&mut self, from: &std::path::Path, to: &str, done: &AtomicU64, cancel: &AtomicBool) -> Result<(), String> {
        let mut file = std::fs::File::open(from).map_err(|e| format!("{}: {e}", from.display()))?;
        let mut b = Vec::new();
        put_str(&mut b, to.as_bytes());
        b.extend_from_slice(&(0x02u32 | 0x08 | 0x10).to_be_bytes()); // write, create, truncate
        b.extend_from_slice(&4u32.to_be_bytes());
        b.extend_from_slice(&0o600u32.to_be_bytes());
        self.request(FXP_OPEN, &b)?;
        let (kind, body) = self.reply()?;
        if kind != FXP_HANDLE || body.len() < 8 {
            return Self::status(kind, &body).and(Err("the host did not open it".into())).map_err(|e| format!("{to}: {e}"));
        }
        let n = u32::from_be_bytes([body[4], body[5], body[6], body[7]]) as usize;
        let handle = body.get(8..8 + n).ok_or("a bad SFTP reply")?.to_vec();
        let mut offset = 0u64;
        let mut flying = 0usize;
        let mut sizes = std::collections::HashMap::new();
        let mut buf = vec![0u8; CHUNK];
        let mut failed = None;
        loop {
            let n = if failed.is_none() && !cancel.load(Ordering::Acquire) {
                file.read(&mut buf).map_err(|e| format!("{}: {e}", from.display()))?
            } else {
                0
            };
            if n > 0 {
                let mut w = Vec::with_capacity(n + 64);
                put_str(&mut w, &handle);
                w.extend_from_slice(&offset.to_be_bytes());
                put_str(&mut w, &buf[..n]);
                let id = self.request(FXP_WRITE, &w)?;
                sizes.insert(id, n as u64);
                offset += n as u64;
                flying += 1;
            }
            // The replies, as they come: once a megabyte is under way, or when it is all sent.
            while flying > 0 && (flying >= IN_FLIGHT || n == 0) {
                let (kind, body) = self.reply()?;
                flying -= 1;
                let id = body.get(..4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
                match Self::status(kind, &body) {
                    Ok(()) => {
                        done.fetch_add(sizes.remove(&id).unwrap_or(0), Ordering::AcqRel);
                    }
                    Err(e) => {
                        failed.get_or_insert(e);
                    }
                }
            }
            if n == 0 {
                break;
            }
        }
        let mut c = Vec::new();
        put_str(&mut c, &handle);
        self.request(FXP_CLOSE, &c)?;
        let (kind, body) = self.reply()?;
        if cancel.load(Ordering::Acquire) {
            return Err("stopped".into());
        }
        if let Some(e) = failed {
            return Err(format!("{to}: {e}"));
        }
        Self::status(kind, &body).map_err(|e| format!("{to}: {e}"))
    }
}

impl Drop for Sftp {
    fn drop(&mut self) {
        let mut c = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = c.kill();
        let _ = c.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file and a folder copied to a real host: SSHREMOTE_DROP_HOST=ALIAS cargo test -- --ignored
    /// copies_over_ssh (a POSIX host or a Windows one, reached with keys).
    #[test]
    #[ignore]
    fn copies_over_ssh() {
        let Ok(host) = std::env::var("SSHREMOTE_DROP_HOST") else { return };
        let dir = std::env::temp_dir().join(format!("sshremote-drop-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("folder/sub")).unwrap();
        std::fs::write(dir.join("my shot.png"), vec![7u8; 300_000]).unwrap();
        std::fs::write(dir.join("folder/sub/note.txt"), b"hello").unwrap();
        std::fs::create_dir_all(dir.join("other")).unwrap();
        std::fs::write(dir.join("other/my shot.png"), b"second").unwrap();
        let (done, cancel, win) = (AtomicU64::new(0), Arc::new(AtomicBool::new(false)), AtomicBool::new(false));
        let paths = ssh_copy(&host, None, &[], &[dir.join("my shot.png"), dir.join("folder"), dir.join("other/my shot.png")], &done, &cancel, &win).unwrap();
        println!("windows={} paths={paths:?}", win.load(Ordering::Acquire));
        assert_eq!(done.load(Ordering::Acquire), 300_011);
        assert_eq!(paths.len(), 3);
        assert!(paths[0].ends_with("my shot.png") && paths[1].ends_with("folder") && paths[2].ends_with("my shot-2.png"), "{paths:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_host_of_an_ssh_command_line() {
        let v = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        let none = Vec::<String>::new();
        assert_eq!(ssh_of(&v("ssh web1")), Some(("web1".into(), None, none.clone())));
        assert_eq!(ssh_of(&v("ssh -p 2222 dev@build")), Some(("dev@build".into(), Some("2222".into()), none.clone())));
        // Who and how go with the copy; a terminal, port forwards and the background do not.
        assert_eq!(
            ssh_of(&v("ssh -p2222 -t -i key -o A=b -l alice -J jump -L 80:x:80 -NCv dev@build ls")),
            Some(("dev@build".into(), Some("2222".into()), v("-i key -o A=b -l alice -J jump -Cv")))
        );
        assert_eq!(ssh_of(&v("ssh -lalice -oUser=bob web1")).unwrap().2, v("-lalice -oUser=bob"));
        assert_eq!(ssh_of(&v("/usr/bin/sshremote --saved-login web1 -- ssh web1")), Some(("web1".into(), None, none.clone())));
        assert_eq!(ssh_of(&v(r"C:\Windows\System32\OpenSSH\ssh.exe host")), Some(("host".into(), None, none.clone())));
        assert_eq!(ssh_of(&v("bash -l")), None);
        // "ssh" as another program's word is no ssh command.
        assert_eq!(ssh_of(&v("docker exec -it ssh bash")), None);
    }

    #[test]
    fn paths_are_typed_as_the_shell_reads_them() {
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(typed(&p(&["/tmp/a.png", "/tmp/my shot.png", "/tmp/it's"]), false), "/tmp/a.png '/tmp/my shot.png' '/tmp/it'\\''s' ");
        assert_eq!(typed(&p(&[r"C:\Users\a\x.png", r"C:\Users\a b\y.png"]), true), r#"C:\Users\a\x.png "C:\Users\a b\y.png" "#);
        // cmd splits at , and =: quoted. A paste's end in a name never reaches the shell.
        assert_eq!(typed(&p(&[r"C:\t\a,b.png"]), true), r#""C:\t\a,b.png" "#);
        assert_eq!(typed(&p(&["/tmp/x\u{1b}[201~\r.png"]), false), "'/tmp/x[201~.png' ");
        // Names a shell reads whole on either computer; two of one name apart.
        assert_eq!(crate::files::typable("$(calc).png"), "_(calc).png");
        assert_eq!(crate::files::typable("100% `x`!.png"), "100_ _x__.png");
        assert_eq!(crate::files::typable("한글 사진.png"), "한글 사진.png");
        assert_eq!(crate::files::typable("con.txt"), "_con.txt");
        assert_eq!(crate::files::typable("x\u{1b}[201~\r"), "x_[201~_");
        let mut taken = std::collections::HashSet::new();
        let names: Vec<String> = ["shot.png", "shot.png", "Shot.png", "a"].iter().map(|n| crate::files::unique(n.to_string(), &mut taken)).collect();
        assert_eq!(names, ["shot.png", "shot-2.png", "Shot-3.png", "a"]);
    }
}
