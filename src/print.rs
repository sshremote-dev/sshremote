//! What the host prints, printed on the viewer: SSH Remote's printer on the host turns each job
//! into a PDF in a folder of its own (`spool_dir`), the agent tells the viewer of it, and the
//! viewer fetches it as any file, then prints it on its own printer or keeps it.
//!
//! The printer: on Linux a CUPS queue whose backend (a script of the program's, run by CUPS as
//! root) writes each job's PDF into /var/spool/sshremote-print/UID for the user who printed it;
//! on Windows, Windows' own "Microsoft Print To PDF" driver on a port into a folder of the
//! program's. (On macOS the system keeps CUPS's backends sealed: a Mac prints here but has no
//! such printer.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The printer's name as people see it.
pub const NAME: &str = "SSH Remote";

/// The folder this user's print jobs land in.
pub fn spool_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    // SAFETY: a plain call.
    return Some(PathBuf::from(format!("/var/spool/sshremote-print/{}", unsafe { libc::getuid() })));
    #[cfg(windows)]
    return std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join(r"SSH Remote\print"));
    #[cfg(not(any(target_os = "linux", windows)))]
    None
}

/// Whether SSH Remote's printer is there for jobs to come from (this user's folder comes with
/// their first job).
pub fn installed() -> bool {
    spool_dir().is_some() && os::installed()
}

/// One job taken from the folder and told to a viewer, not yet answered.
struct Told {
    path: PathBuf,
    /// Its name in the folder before it was taken, to put it back for another viewer.
    was: PathBuf,
}

/// The jobs in this user's folder: each is taken (renamed) by the first agent to see it, told to
/// its viewer, and deleted once the viewer has it; one not answered goes back as the agent ends.
pub struct Spool {
    dir: PathBuf,
    told: HashMap<u32, Told>,
    next: u32,
    /// On Windows, the printer's pipe served while this lives (another agent may serve it).
    #[cfg(windows)]
    _listen: Option<os::Listener>,
}

/// Jobs are whole once the backend has renamed them to `.pdf`.
fn whole(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// A job's name for people: its file's, less the job number the backend put before it.
fn shown_name(p: &Path) -> String {
    let file = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    match file.split_once('-') {
        Some((n, rest)) if !rest.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => rest.to_string(),
        _ => file,
    }
}

impl Spool {
    pub fn new() -> Option<Spool> {
        let dir = spool_dir()?;
        #[cfg(windows)]
        let _listen = os::listen(&dir);
        Some(Spool {
            dir,
            told: HashMap::new(),
            next: 1,
            #[cfg(windows)]
            _listen,
        })
    }

    #[cfg(test)]
    fn at(dir: PathBuf) -> Spool {
        Spool {
            dir,
            told: HashMap::new(),
            next: 1,
            #[cfg(windows)]
            _listen: None,
        }
    }

    /// The jobs that came since last asked, taken for this viewer: (id, name, size, path).
    pub fn poll(&mut self) -> Vec<(u32, String, u64, PathBuf)> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        let mut out = Vec::new();
        let mut found: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| whole(p)).collect();
        found.sort();
        for was in found {
            let taken = was.with_extension(format!("pdf.{}.taken", std::process::id()));
            // Another agent (another viewer) took it first.
            if std::fs::rename(&was, &taken).is_err() {
                continue;
            }
            let size = std::fs::metadata(&taken).map(|m| m.len()).unwrap_or(0);
            let id = self.next;
            self.next += 1;
            out.push((id, shown_name(&was), size, taken.clone()));
            self.told.insert(id, Told { path: taken, was });
        }
        out
    }

    /// The viewer has job `id`: its file goes.
    pub fn done(&mut self, id: u32) {
        if let Some(t) = self.told.remove(&id) {
            let _ = std::fs::remove_file(t.path);
        }
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        for t in self.told.values() {
            let _ = std::fs::rename(&t.path, &t.was);
        }
    }
}

/// Sets up SSH Remote's printer (as an administrator), or takes it away.
pub fn set_up(on: bool) -> Result<String, String> {
    if on { os::install() } else { os::uninstall() }
}

/// Prints the PDF at `path` on this computer's default printer; what to tell the person.
pub fn print_here(path: &Path, title: &str) -> Result<String, String> {
    os::print(path, title)
}

#[cfg(unix)]
fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("{cmd} failed") } else { err })
    }
}

/// Printing a PDF on a computer with CUPS (Linux, macOS).
#[cfg(unix)]
fn print_lp(path: &Path, title: &str) -> Result<String, String> {
    let default = run("lpstat", &["-d"]).ok().and_then(|s| s.split_once(':').map(|(_, p)| p.trim().to_string())).filter(|p| !p.is_empty() && !p.contains(' '));
    let Some(printer) = default else { return Err(n!("this computer has no default printer").into()) };
    run("lp", &["-d", &printer, "-t", title, "--", &path.to_string_lossy()])?;
    Ok(crate::lang::fill(n!("printed on {}"), &[&printer]))
}

#[cfg(target_os = "linux")]
mod os {
    use super::run;

    const QUEUE: &str = "SSH_Remote";
    const BACKEND: &str = "/usr/lib/cups/backend/sshremote";
    const SPOOL: &str = "/var/spool/sshremote-print";

    /// The CUPS backend: each job's PDF into the folder of the user who printed it, whole once
    /// renamed. Run as root (mode 0700), it gives the file to that user.
    const SCRIPT: &str = r#"#!/bin/sh
# CUPS backend of SSH Remote: what is printed to its printer goes to the computer viewing this one.
if [ $# -eq 0 ]; then
    echo 'direct sshremote "Unknown" "SSH Remote"'
    exit 0
fi
uid=$(id -u "$2" 2>/dev/null) || { echo "ERROR: no user $2" >&2; exit 5; }
dir=/var/spool/sshremote-print/$uid
mkdir -p "$dir" && chown "$uid" "$dir" && chmod 700 "$dir" || exit 1
name=$(printf '%s' "$3" | tr -c 'A-Za-z0-9 ._()-' '_' | cut -c1-80)
name=${name%.pdf}
[ -n "$name" ] || name=document
part="$dir/$1-$name.pdf.part"
if [ -n "$6" ]; then cat "$6" > "$part"; else cat > "$part"; fi || exit 1
chown "$uid" "$part" && chmod 600 "$part" && mv "$part" "$dir/$1-$name.pdf"
"#;

    /// A printer of PDFs: CUPS turns whatever is printed into one before the backend has it.
    const PPD: &str = r#"*PPD-Adobe: "4.3"
*FormatVersion: "4.3"
*FileVersion: "1.0"
*LanguageVersion: English
*LanguageEncoding: ISOLatin1
*PCFileName: "SSHREMOT.PPD"
*Manufacturer: "SSH Remote"
*Product: "(SSH Remote)"
*ModelName: "SSH Remote"
*ShortNickName: "SSH Remote"
*NickName: "SSH Remote: prints on the viewing computer"
*PSVersion: "(3010.000) 0"
*LanguageLevel: "3"
*ColorDevice: True
*DefaultColorSpace: RGB
*FileSystem: False
*Throughput: "1"
*LandscapeOrientation: Plus90
*TTRasterizer: Type42
*cupsVersion: 2.2
*cupsFilter2: "application/pdf application/vnd.cups-pdf 0 -"
*OpenUI *PageSize/Media Size: PickOne
*OrderDependency: 10 AnySetup *PageSize
*DefaultPageSize: A4
*PageSize A4/A4: "<</PageSize[595 842]/ImagingBBox null>>setpagedevice"
*PageSize Letter/US Letter: "<</PageSize[612 792]/ImagingBBox null>>setpagedevice"
*CloseUI: *PageSize
*OpenUI *PageRegion: PickOne
*OrderDependency: 10 AnySetup *PageRegion
*DefaultPageRegion: A4
*PageRegion A4/A4: "<</PageSize[595 842]/ImagingBBox null>>setpagedevice"
*PageRegion Letter/US Letter: "<</PageSize[612 792]/ImagingBBox null>>setpagedevice"
*CloseUI: *PageRegion
*DefaultImageableArea: A4
*ImageableArea A4/A4: "0 0 595 842"
*ImageableArea Letter/US Letter: "0 0 612 792"
*DefaultPaperDimension: A4
*PaperDimension A4/A4: "595 842"
*PaperDimension Letter/US Letter: "612 792"
"#;

    pub fn installed() -> bool {
        std::path::Path::new(BACKEND).exists() && run("lpstat", &["-p", QUEUE]).is_ok()
    }

    pub fn install() -> Result<String, String> {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: a plain call.
        if unsafe { libc::geteuid() } != 0 {
            return Err("setting up the printer needs an administrator: sudo sshremote remote-access printer on".into());
        }
        if !std::path::Path::new("/usr/lib/cups/backend").is_dir() {
            return Err("this computer has no CUPS (install the cups package first)".into());
        }
        fn e(what: &'static str) -> impl Fn(std::io::Error) -> String {
            move |x| format!("{what}: {x}")
        }
        std::fs::write(BACKEND, SCRIPT).map_err(e(BACKEND))?;
        std::fs::set_permissions(BACKEND, std::fs::Permissions::from_mode(0o700)).map_err(e(BACKEND))?;
        // Each user's folder is made by the backend; this one only holds them.
        std::fs::create_dir_all(SPOOL).map_err(e(SPOOL))?;
        std::fs::set_permissions(SPOOL, std::fs::Permissions::from_mode(0o755)).map_err(e(SPOOL))?;
        let ppd = std::env::temp_dir().join(format!("sshremote-{}.ppd", std::process::id()));
        std::fs::write(&ppd, PPD).map_err(e("the printer's description"))?;
        let r = run("lpadmin", &["-p", QUEUE, "-E", "-v", "sshremote:/", "-P", &ppd.to_string_lossy(), "-D", super::NAME, "-L", "The computer viewing this one", "-o", "printer-is-shared=false"]);
        let _ = std::fs::remove_file(&ppd);
        r?;
        Ok(format!("The printer \"{}\" is set up: what is printed to it prints on the computer viewing this one.", super::NAME))
    }

    pub fn uninstall() -> Result<String, String> {
        // SAFETY: a plain call.
        if unsafe { libc::geteuid() } != 0 {
            return Err("taking the printer away needs an administrator: sudo sshremote remote-access printer off".into());
        }
        let _ = run("lpadmin", &["-x", QUEUE]);
        let _ = std::fs::remove_file(BACKEND);
        let _ = std::fs::remove_dir_all(SPOOL);
        Ok(format!("The printer \"{}\" is gone.", super::NAME))
    }

    pub fn print(path: &std::path::Path, title: &str) -> Result<String, String> {
        super::print_lp(path, title)
    }
}

#[cfg(target_os = "macos")]
mod os {
    pub fn installed() -> bool {
        false
    }

    pub fn install() -> Result<String, String> {
        Err("a Mac cannot have SSH Remote's printer: macOS keeps the printing system's parts sealed".into())
    }

    pub fn uninstall() -> Result<String, String> {
        Ok("A Mac has no SSH Remote printer.".into())
    }

    pub fn print(path: &std::path::Path, title: &str) -> Result<String, String> {
        super::print_lp(path, title)
    }
}

#[cfg(windows)]
#[path = "print_win.rs"]
mod os;

#[cfg(not(any(unix, windows)))]
mod os {
    pub fn installed() -> bool {
        false
    }

    pub fn install() -> Result<String, String> {
        Err("not on this system".into())
    }

    pub fn uninstall() -> Result<String, String> {
        Ok(String::new())
    }

    pub fn print(_: &std::path::Path, _: &str) -> Result<String, String> {
        Err(n!("this system cannot print here").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page printed on the host to SSH Remote's printer comes to the viewer through a local
    /// agent, is fetched and printed on the viewer's default printer. Needs CUPS with the
    /// printer set up (`remote-access printer on`) and a default printer, and SSHREMOTE_EXE.
    #[test]
    #[ignore]
    fn a_page_printed_there_prints_here() {
        use crate::proto::{ToAgent, ToClient};
        use crate::session::Ev;
        use std::time::{Duration, Instant};
        let opt = crate::parse_client(&["--local".into(), "--capture".into(), "synthetic".into()]).unwrap();
        let (mut sess, _tx, rx) = crate::session::Session::connect(&opt).expect("a local agent (SSHREMOTE_EXE)");
        let next = |until: Instant, want: &dyn Fn(&ToClient) -> bool| -> ToClient {
            loop {
                let left = until.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(Ev::Net(m)) if want(&m) => return m,
                    Ok(Ev::Closed(e)) => panic!("closed: {e}"),
                    Ok(_) => {}
                    Err(_) => panic!("nothing in time"),
                }
            }
        };
        let can = next(Instant::now() + Duration::from_secs(10), &|m| matches!(m, ToClient::Can(_)));
        assert!(matches!(can, ToClient::Can(b) if b & crate::proto::CAN_PRINT != 0), "the agent has the printer");
        sess.send(ToAgent::Prints(true)).unwrap();
        #[cfg(unix)]
        run("lp", &["-d", "SSH_Remote", "-t", "Test page", "/etc/os-release"]).unwrap();
        #[cfg(windows)]
        {
            let script = format!("$d = New-Object System.Drawing.Printing.PrintDocument; $d.DocumentName = 'Test page'; $d.PrinterSettings.PrinterName = '{NAME}'; $d.add_PrintPage({{ param($s, $e) $e.Graphics.DrawString('A test page from SSH Remote', (New-Object System.Drawing.Font('Arial', 24)), [System.Drawing.Brushes]::Black, 100, 100) }}); $d.Print()");
            let out = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &format!("Add-Type -AssemblyName System.Drawing; {script}")]).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
        let ToClient::PrintJob { id, size, name, path } = next(Instant::now() + Duration::from_secs(30), &|m| matches!(m, ToClient::PrintJob { .. })) else { unreachable!() };
        eprintln!("job {id}: {name}, {size} bytes at {path}");
        assert_eq!(name, "Test page.pdf");
        let dir = std::env::temp_dir().join(format!("sshremote-print-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut inbox = crate::files::Inbox::default();
        sess.send(ToAgent::Fetch { tag: 9 | crate::files::DONE_TAG, path: path.clone() }).unwrap();
        let until = Instant::now() + Duration::from_secs(20);
        let mut file = None;
        loop {
            match next(until, &|m| matches!(m, ToClient::FetchStart { .. } | ToClient::FetchData { .. } | ToClient::FetchEnd { .. } | ToClient::FetchDone { .. })) {
                ToClient::FetchStart { id, size, name, .. } => {
                    sess.fetch_start(&mut inbox, id, size, &dir, &name).unwrap();
                    file = Some(dir.join(name));
                }
                ToClient::FetchData { id, data } => drop(sess.fetch_data(&mut inbox, id, &data).unwrap()),
                ToClient::FetchEnd { id, ok, text } => {
                    let note = inbox.end(id, ok, &text);
                    assert!(ok && matches!(note, Some(crate::files::Note::Done { ok: true, .. })), "{text}");
                }
                _ => break,
            }
        }
        let file = file.expect("the job's file");
        let data = std::fs::read(&file).unwrap();
        assert!(data.starts_with(b"%PDF"), "a PDF came");
        sess.send(ToAgent::Printed(id)).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(!Path::new(&path).exists(), "the host let the job go");
        let said = print_here(&file, &name).unwrap();
        eprintln!("{said}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_jobs_name_leaves_its_number_out() {
        assert_eq!(shown_name(Path::new("/x/12-Report 2025.pdf")), "Report 2025.pdf");
        assert_eq!(shown_name(Path::new("/x/Report.pdf")), "Report.pdf");
        assert_eq!(shown_name(Path::new("/x/a-b-c.pdf")), "a-b-c.pdf");
    }

    #[test]
    fn a_job_is_taken_once_and_goes_back_if_not_answered() {
        let dir = std::env::temp_dir().join(format!("sshremote-spool-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("7-Letter.pdf"), b"%PDF-1.4").unwrap();
        std::fs::write(dir.join("8-Half.pdf.part"), b"%PDF").unwrap();
        let mut a = Spool::at(dir.clone());
        let jobs = a.poll();
        assert_eq!(jobs.len(), 1);
        assert_eq!((jobs[0].1.as_str(), jobs[0].2), ("Letter.pdf", 8));
        // A second agent finds nothing of it.
        let mut b = Spool::at(dir.clone());
        assert!(b.poll().is_empty());
        drop(a);
        assert!(dir.join("7-Letter.pdf").exists(), "back for another viewer");
        let jobs = b.poll();
        assert_eq!(jobs.len(), 1);
        b.done(jobs[0].0);
        assert!(std::fs::read_dir(&dir).unwrap().flatten().all(|e| e.path().extension().is_some_and(|x| x == "part")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
