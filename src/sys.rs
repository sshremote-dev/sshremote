//! Small things every part of the program needs from the system, in one place: the home folder,
//! starting helper programs without a console window, and Windows strings and command lines.

use std::path::PathBuf;
use std::process::Command;

/// The user's home folder (USERPROFILE on Windows, HOME elsewhere).
pub fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default()
}

/// `~/.ssh`.
pub fn ssh_dir() -> PathBuf {
    home_dir().join(".ssh")
}

/// A helper program started from a window: on Windows it gets no console window of its own.
pub fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    cmd
}

/// Whether Insert is down now: Ctrl+Insert and Shift+Insert reach the windows as a copy and a
/// paste, and only this tells them from Ctrl+C and Ctrl+V.
pub fn insert_held() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_INSERT};
        // SAFETY: a plain query of the key's state.
        unsafe { GetAsyncKeyState(VK_INSERT as i32) < 0 }
    }
    #[cfg(not(windows))]
    false
}

/// A string as Windows wants it: UTF-16 ending in a zero.
#[cfg(windows)]
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

#[cfg(any(windows, test))]
/// One argument quoted the way the C runtime splits a command line: backslashes before a quote,
/// and at the end before the closing quote, are doubled.
pub fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut q = String::from('"');
    let mut slashes = 0;
    for c in a.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                q.push_str(&"\\".repeat(slashes * 2 + 1));
                q.push('"');
                slashes = 0;
            }
            c => {
                q.push_str(&"\\".repeat(slashes));
                q.push(c);
                slashes = 0;
            }
        }
    }
    q.push_str(&"\\".repeat(slashes * 2));
    q.push('"');
    q
}

/// A command line from separate arguments.
#[cfg(any(windows, test))]
pub fn command_line(argv: &[String]) -> String {
    argv.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn arguments_are_quoted_for_the_c_runtime() {
        assert_eq!(super::quote_arg("plain"), "plain");
        assert_eq!(super::quote_arg("two words"), "\"two words\"");
        assert_eq!(super::quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(super::quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(super::quote_arg(""), "\"\"");
    }
}
