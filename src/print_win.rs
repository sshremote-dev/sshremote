//! Windows' side of printing. The host's printer is Windows' own "Microsoft Print To PDF" driver
//! on a local port that is a named pipe: each job opens it once, so the agent of a viewer that
//! prints, listening there, has each PDF whole and apart (`listen`); with nobody listening the
//! spooler keeps the job and tries again. Here, a PDF is printed by drawing its pages with
//! Windows' own PDF renderer onto the default printer.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT};

use crate::sys::wide;

const PIPE: &str = r"\\.\pipe\sshremote-print";

fn powershell(script: &str) -> Result<String, String> {
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| format!("PowerShell: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).lines().find(|l| !l.trim().is_empty()).unwrap_or("PowerShell failed").trim().to_string())
    }
}

pub fn installed() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegOpenKeyExW};
    let key = wide(&format!(r"SYSTEM\CurrentControlSet\Control\Print\Printers\{}", super::NAME));
    let mut h: HKEY = std::ptr::null_mut();
    // SAFETY: a key opened for reading and closed at once.
    unsafe {
        let ok = RegOpenKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, KEY_READ, &mut h) == 0;
        if ok {
            RegCloseKey(h);
        }
        ok
    }
}

pub fn install() -> Result<String, String> {
    if !crate::admin::elevated() {
        return Err("setting up the printer needs an administrator".into());
    }
    let script = format!(
        "$ErrorActionPreference = 'Stop'; \
         if (-not (Get-PrinterDriver -Name 'Microsoft Print To PDF' -ErrorAction SilentlyContinue)) {{ Add-PrinterDriver -Name 'Microsoft Print To PDF' }}; \
         if (-not (Get-PrinterPort -Name '{PIPE}' -ErrorAction SilentlyContinue)) {{ Add-PrinterPort -Name '{PIPE}' }}; \
         if (-not (Get-Printer -Name '{name}' -ErrorAction SilentlyContinue)) {{ Add-Printer -Name '{name}' -DriverName 'Microsoft Print To PDF' -PortName '{PIPE}' -Comment 'Prints on the computer viewing this one' -Location 'The viewing computer' }}",
        name = super::NAME
    );
    powershell(&script).map_err(|e| format!("the printer could not be set up: {e}"))?;
    Ok(format!("The printer \"{}\" is set up: what is printed to it prints on the computer viewing this one.", super::NAME))
}

pub fn uninstall() -> Result<String, String> {
    let script = format!(
        "Remove-Printer -Name '{name}' -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 500; Remove-PrinterPort -Name '{PIPE}' -ErrorAction SilentlyContinue",
        name = super::NAME
    );
    powershell(&script)?;
    Ok(format!("The printer \"{}\" is gone.", super::NAME))
}

/// The name of the job the printer prints now, if Windows says.
fn printing_now() -> Option<String> {
    use windows_sys::Win32::Graphics::Printing::{ClosePrinter, EnumJobsW, JOB_INFO_1W, JOB_STATUS_PRINTING, OpenPrinterW, PRINTER_HANDLE};
    let name = wide(super::NAME);
    let mut h = PRINTER_HANDLE::default();
    // SAFETY: the printer is opened and closed here; the jobs are read from a buffer of the size
    // the first call asks for.
    unsafe {
        if OpenPrinterW(name.as_ptr(), &mut h, std::ptr::null()) == 0 {
            return None;
        }
        let (mut need, mut count) = (0u32, 0u32);
        EnumJobsW(h, 0, 64, 1, std::ptr::null_mut(), 0, &mut need, &mut count);
        let mut buf = vec![0u64; (need as usize).div_ceil(8).max(1)];
        let found = if EnumJobsW(h, 0, 64, 1, buf.as_mut_ptr().cast(), need, &mut need, &mut count) != 0 {
            let jobs = std::slice::from_raw_parts(buf.as_ptr() as *const JOB_INFO_1W, count as usize);
            jobs.iter().find(|j| j.Status & JOB_STATUS_PRINTING != 0).or(jobs.first()).filter(|j| !j.pDocument.is_null()).map(|j| {
                let mut n = 0;
                while *j.pDocument.add(n) != 0 {
                    n += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(j.pDocument, n))
            })
        } else {
            None
        };
        ClosePrinter(h);
        found
    }
}

/// A name of a file for a job's title.
fn file_name(title: &str) -> String {
    let t: String = title.chars().map(|c| if c.is_alphanumeric() || " ._()-".contains(c) { c } else { '_' }).take(80).collect();
    let t = t.trim().trim_end_matches(".pdf").trim().to_string();
    if t.is_empty() { "document".into() } else { t }
}

/// While it lives, the jobs the printer sends through its pipe become PDFs in `dir`
/// ("N-title.pdf", whole once renamed so), unless another agent listens there first.
pub struct Listener {
    stop: Arc<AtomicBool>,
}

pub fn listen(dir: &Path) -> Option<Listener> {
    std::fs::create_dir_all(dir).ok()?;
    let name = wide(PIPE);
    // SAFETY: a pipe made here, handed to the thread that serves it and closes it.
    let pipe: HANDLE = unsafe { CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, 1, 0, 1 << 16, 0, std::ptr::null()) };
    if pipe == INVALID_HANDLE_VALUE {
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (halt, dir, pipe) = (stop.clone(), dir.to_path_buf(), pipe as usize);
    std::thread::Builder::new()
        .name("print-jobs".into())
        .spawn(move || {
            let pipe = pipe as HANDLE;
            let mut n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            loop {
                // SAFETY: the pipe this thread owns; a connection that came before the call counts.
                let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
                if halt.load(Ordering::Acquire) {
                    break;
                }
                if connected {
                    n += 1;
                    let title = printing_now().unwrap_or_default();
                    let part = dir.join(format!("{n}-{}.pdf.part", file_name(&title)));
                    let mut data = Vec::new();
                    let mut chunk = vec![0u8; 1 << 16];
                    loop {
                        let mut got = 0u32;
                        // SAFETY: reads into `chunk`, at most its length.
                        let ok = unsafe { ReadFile(pipe, chunk.as_mut_ptr(), chunk.len() as u32, &mut got, std::ptr::null_mut()) } != 0;
                        if got > 0 {
                            data.extend_from_slice(&chunk[..got as usize]);
                        }
                        // The job is whole when the spooler closes its end.
                        if !ok || got == 0 {
                            break;
                        }
                    }
                    if data.starts_with(b"%PDF") && std::fs::write(&part, &data).is_ok() {
                        let _ = std::fs::rename(&part, part.with_extension(""));
                    }
                }
                // SAFETY: the pipe this thread owns.
                unsafe { DisconnectNamedPipe(pipe) };
            }
            // SAFETY: closed once, by its owner.
            unsafe { CloseHandle(pipe) };
        })
        .ok()?;
    Some(Listener { stop })
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Its thread waits for a connection: one wakes it to see it should end.
        let name = wide(PIPE);
        // SAFETY: a handle opened and closed at once.
        unsafe {
            let h = CreateFileW(name.as_ptr(), GENERIC_WRITE, 0, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut());
            if h != INVALID_HANDLE_VALUE {
                CloseHandle(h);
            }
        }
    }
}

/// The default printer's name.
fn default_printer() -> Option<String> {
    use windows_sys::Win32::Graphics::Printing::GetDefaultPrinterW;
    let mut n = 0u32;
    // SAFETY: the first call says the size, the second fills a buffer of it.
    unsafe {
        GetDefaultPrinterW(std::ptr::null_mut(), &mut n);
        if n == 0 {
            return None;
        }
        let mut buf = vec![0u16; n as usize];
        if GetDefaultPrinterW(buf.as_mut_ptr(), &mut n) == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]))
    }
}

/// The PDF's pages drawn by Windows at `size` (each page fitted into it): BGRA rows, and each
/// page's width and height.
fn pages(path: &Path, fit: (u32, u32)) -> Result<Vec<(Vec<u8>, u32, u32)>, String> {
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    let e = |what: &'static str| move |x: windows::core::Error| format!("{what}: {x}");
    // SAFETY: a plain call; set up already (in another way) is as good.
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let full = std::path::absolute(path).map_err(|x| x.to_string())?;
    let file = StorageFile::GetFileFromPathAsync(&windows::core::HSTRING::from(full.to_string_lossy().as_ref())).and_then(|a| a.join()).map_err(e("the file"))?;
    let doc = PdfDocument::LoadFromFileAsync(&file).and_then(|a| a.join()).map_err(e("the PDF"))?;
    let mut out = Vec::new();
    for i in 0..doc.PageCount().map_err(e("the PDF"))? {
        let page = doc.GetPage(i).map_err(e("a page"))?;
        let size = page.Size().map_err(e("a page"))?;
        let scale = (fit.0 as f64 / size.Width.max(1.0) as f64).min(fit.1 as f64 / size.Height.max(1.0) as f64);
        let (w, h) = (((size.Width as f64 * scale) as u32).max(1), ((size.Height as f64 * scale) as u32).max(1));
        let opts = PdfPageRenderOptions::new().map_err(e("drawing"))?;
        opts.SetDestinationWidth(w).map_err(e("drawing"))?;
        opts.SetDestinationHeight(h).map_err(e("drawing"))?;
        let stream = InMemoryRandomAccessStream::new().map_err(e("drawing"))?;
        page.RenderWithOptionsToStreamAsync(&stream, &opts).and_then(|a| a.join()).map_err(e("drawing"))?;
        let n = stream.Size().map_err(e("drawing"))? as u32;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).map_err(e("drawing"))?).map_err(e("drawing"))?;
        reader.LoadAsync(n).and_then(|a| a.join()).map_err(e("drawing"))?;
        let mut png = vec![0u8; n as usize];
        reader.ReadBytes(&mut png).map_err(e("drawing"))?;
        let mut dec = png::Decoder::new(std::io::Cursor::new(png));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut r = dec.read_info().map_err(|x| format!("a page: {x}"))?;
        let mut px = vec![0u8; r.output_buffer_size()];
        let info = r.next_frame(&mut px).map_err(|x| format!("a page: {x}"))?;
        let step = info.color_type.samples();
        let mut bgra = Vec::with_capacity((info.width * info.height * 4) as usize);
        for p in px[..info.buffer_size()].chunks_exact(step) {
            let (r, g, b, a) = match step {
                1 | 2 => (p[0], p[0], p[0], if step == 2 { p[1] } else { 255 }),
                _ => (p[0], p[1], p[2], if step == 4 { p[3] } else { 255 }),
            };
            // On white paper: what is see-through is white.
            let over = |c: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
            bgra.extend_from_slice(&[over(b), over(g), over(r), 255]);
        }
        out.push((bgra, info.width, info.height));
    }
    Ok(out)
}

pub fn print(path: &Path, title: &str) -> Result<String, String> {
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDCW, DIB_RGB_COLORS, DeleteDC, GetDeviceCaps, HORZRES, SRCCOPY, StretchDIBits, VERTRES,
    };
    use windows_sys::Win32::Storage::Xps::{DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
    let printer = default_printer().ok_or("this computer has no default printer")?;
    let (spool, device) = (wide("WINSPOOL"), wide(&printer));
    // SAFETY: a printer's device context made, drawn on and deleted here.
    unsafe {
        let dc = CreateDCW(spool.as_ptr(), device.as_ptr(), std::ptr::null(), std::ptr::null());
        if dc.is_null() {
            return Err(format!("{printer} could not be opened"));
        }
        let room = (GetDeviceCaps(dc, HORZRES as _).max(1) as u32, GetDeviceCaps(dc, VERTRES as _).max(1) as u32);
        // At most about 40 million pixels a page.
        let shrink = (40e6 / (room.0 as f64 * room.1 as f64)).sqrt().min(1.0);
        let fit = ((room.0 as f64 * shrink) as u32, (room.1 as f64 * shrink) as u32);
        let drawn = match pages(path, fit) {
            Ok(p) => p,
            Err(e) => {
                DeleteDC(dc);
                return Err(e);
            }
        };
        let name = wide(title);
        let doc = DOCINFOW { cbSize: std::mem::size_of::<DOCINFOW>() as i32, lpszDocName: name.as_ptr(), lpszOutput: std::ptr::null(), lpszDatatype: std::ptr::null(), fwType: 0 };
        if StartDocW(dc, &doc) <= 0 {
            DeleteDC(dc);
            return Err(format!("{printer} did not take the job"));
        }
        for (px, w, h) in &drawn {
            StartPage(dc);
            let k = (room.0 as f64 / *w as f64).min(room.1 as f64 / *h as f64);
            let (dw, dh) = ((*w as f64 * k) as i32, (*h as f64 * k) as i32);
            let (x, y) = ((room.0 as i32 - dw) / 2, (room.1 as i32 - dh) / 2);
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: *w as i32, biHeight: -(*h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..std::mem::zeroed() };
            StretchDIBits(dc, x, y, dw, dh, 0, 0, *w as i32, *h as i32, px.as_ptr().cast(), &bi, DIB_RGB_COLORS, SRCCOPY);
            EndPage(dc);
        }
        EndDoc(dc);
        DeleteDC(dc);
        Ok(format!("{} {} printed on {printer}", drawn.len(), if drawn.len() == 1 { "page" } else { "pages" }))
    }
}
