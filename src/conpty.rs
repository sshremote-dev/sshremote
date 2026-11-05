//! Under Windows OpenSSH every byte a program writes passes through a pseudo
//! console (ConPTY), which renders it into a screen buffer and re-emits only
//! text, so sixel images never reach the client. The pseudo console host
//! writes its output to a pipe that leads straight to sshd; this opens that
//! same pipe so the viewer's bytes reach the client verbatim.

use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::FromRawHandle;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_PIPE, GetFileType};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(h: HANDLE, class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
    fn NtQuerySystemInformation(class: u32, info: *mut c_void, len: u32, ret: *mut u32) -> i32;
}

#[repr(C)]
struct HandleEntry {
    object: usize,
    pid: usize,
    handle: usize,
    access: u32,
    backtrace: u16,
    type_index: u16,
    attributes: u32,
    reserved: u32,
}

/// Handle values and granted access of every handle open in process `pid`.
fn handles_of(pid: u32) -> Result<Vec<(usize, u32)>, String> {
    let mut buf = vec![0u8; 1 << 22];
    loop {
        let mut need = 0u32;
        // SystemExtendedHandleInformation
        let st = unsafe { NtQuerySystemInformation(64, buf.as_mut_ptr() as _, buf.len() as u32, &mut need) };
        if st == 0 {
            break;
        }
        // STATUS_INFO_LENGTH_MISMATCH
        if st as u32 != 0xC000_0004 || buf.len() > 1 << 30 {
            return Err(format!("NtQuerySystemInformation: {st:#x}"));
        }
        buf = vec![0u8; buf.len().max(need as usize) * 2];
    }
    let count = usize::from_ne_bytes(buf[..8].try_into().unwrap());
    let entries = unsafe { std::slice::from_raw_parts(buf.as_ptr().add(16) as *const HandleEntry, count) };
    Ok(entries.iter().filter(|e| e.pid == pid as usize).map(|e| (e.handle, e.access)).collect())
}

/// The host's standard output as recorded in its process parameters (x64 PEB offsets).
unsafe fn peb_stdout(host: HANDLE) -> Option<usize> {
    let mut pbi = [0usize; 6];
    if unsafe { NtQueryInformationProcess(host, 0, pbi.as_mut_ptr() as _, size_of_val(&pbi) as u32, null_mut()) } != 0 {
        return None;
    }
    let read = |addr: usize| {
        let mut v = 0usize;
        let ok =
            unsafe { ReadProcessMemory(host, addr as _, &mut v as *mut usize as _, size_of::<usize>(), null_mut()) };
        (ok != 0).then_some(v)
    };
    read(read(pbi[1] + 0x20)? + 0x28)
}

/// The write end of the pseudo console's output pipe, duplicated from its host process:
/// the host's standard output, or else the only pipe handle it holds for writing.
pub fn raw_output() -> Result<File, String> {
    unsafe {
        // ProcessConsoleHostProcess names the conhost serving this process; the low bits are flags.
        let mut v = 0usize;
        let st = NtQueryInformationProcess(GetCurrentProcess(), 49, &mut v as *mut usize as _, 8, null_mut());
        if st != 0 || v & !3 == 0 {
            return Err(format!("no console host ({st:#x})"));
        }
        let pid = (v & !3) as u32;
        let host = OpenProcess(PROCESS_DUP_HANDLE | PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if host.is_null() {
            return Err(format!("cannot open the console host {pid}: {}", std::io::Error::last_os_error()));
        }
        let dup_pipe = |h: usize| {
            let mut mine = null_mut();
            if DuplicateHandle(host, h as HANDLE, GetCurrentProcess(), &mut mine, 0, 0, DUPLICATE_SAME_ACCESS) == 0 {
                return None;
            }
            if GetFileType(mine) == FILE_TYPE_PIPE {
                Some(mine)
            } else {
                CloseHandle(mine);
                None
            }
        };
        if let Some(h) = peb_stdout(host).and_then(dup_pipe) {
            CloseHandle(host);
            return Ok(File::from_raw_handle(h));
        }
        let mut found = Vec::new();
        for (h, access) in handles_of(pid)? {
            // FILE_WRITE_DATA without FILE_READ_DATA.
            if access & 2 == 0 || access & 1 != 0 {
                continue;
            }
            found.extend(dup_pipe(h));
        }
        CloseHandle(host);
        match found.as_slice() {
            [one] => Ok(File::from_raw_handle(*one)),
            [] => Err(format!("the console host {pid} has no output pipe")),
            many => {
                for h in many {
                    CloseHandle(*h);
                }
                Err(format!("the console host {pid} has {} candidate output pipes", many.len()))
            }
        }
    }
}
