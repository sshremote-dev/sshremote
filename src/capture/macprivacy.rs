//! A Mac's screen hidden from whoever sits there while the viewer still sees it, and its own
//! keyboard and mouse kept out: a cover of black windows above everything in a process of its own
//! (`sshremote --cover`) that screen capture leaves out, and an event tap that drops every event
//! this program did not post.

use std::ffi::c_void;
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU64, Ordering};
use std::time::Duration;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_core_foundation::{CFMachPort, CFRunLoop, CGPoint, CGRect, CGSize, kCFRunLoopCommonModes};
use objc2_core_graphics::{CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType};
use objc2_foundation::NSString;

/// What this program's own events carry in `kCGEventSourceUserData`.
pub const MARK: i64 = crate::sys::INJECTED as i64;

static BLOCK: AtomicBool = AtomicBool::new(false);
static TAKEN_BACK: AtomicBool = AtomicBool::new(false);
/// Where this program's own events last left the pointer (x and y as bits): macOS moves the
/// pointer for the mouse there before any tap sees the move, so it is put back.
static AT: (AtomicU64, AtomicU64) = (AtomicU64::new(0), AtomicU64::new(0));
/// The tap once made (it lives as long as the program; off while nothing is kept out).
static TAP: AtomicPtr<CFMachPort> = AtomicPtr::new(std::ptr::null_mut());
/// The cover's process while it is up (0: none), and a count of its comings and goings that the
/// capture follows: a new stream leaves it out, then the cover is shown.
static COVER_PID: AtomicI32 = AtomicI32::new(0);
static TURN: AtomicU64 = AtomicU64::new(0);
static COVER: Mutex<Option<(Child, ChildStdin)>> = Mutex::new(None);

/// Whether the screen can be hidden here: without Metal (a virtual machine's display)
/// WindowServer dies drawing pictures that leave a window out, ending the session.
pub fn hides() -> bool {
    super::mac::metal()
}

/// The cover's process, if it is up, and the turn it is at.
pub fn cover() -> (Option<i32>, u64) {
    let pid = COVER_PID.load(Ordering::Acquire);
    ((pid != 0).then_some(pid), TURN.load(Ordering::Acquire))
}

/// The capture leaves out the cover of `turn` now: it may be shown.
pub fn seen(turn: u64) {
    let mut c = COVER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, input)) = c.as_mut().filter(|_| TURN.load(Ordering::Acquire) == turn) {
        let _ = input.write_all(b"show\n").and_then(|()| input.flush());
    }
}

/// Keeps the Mac's own input out (`block`) and its screen covered (`hide`), or lets them be.
pub fn set(block: bool, hide: bool) -> Result<(), String> {
    TAKEN_BACK.store(false, Ordering::Relaxed);
    if block {
        tap()?;
        if let Some(e) = CGEvent::new(None) {
            keep_at(CGEvent::location(Some(&e)));
        }
    }
    BLOCK.store(block, Ordering::Relaxed);
    if let Some(t) = NonNull::new(TAP.load(Ordering::Acquire)) {
        // SAFETY: the tap lives as long as the program.
        CGEvent::tap_enable(unsafe { t.as_ref() }, block);
    }
    if hide && !hides() {
        return Err("hiding the screen needs a Mac with Metal graphics (a virtual machine has none)".into());
    }
    let mut c = COVER.lock().unwrap_or_else(|e| e.into_inner());
    match (hide, c.is_some()) {
        (true, false) => *c = Some(start_cover()?),
        (false, true) => {
            if let Some((mut child, input)) = c.take() {
                // Its input closed, it ends.
                drop(input);
                for _ in 0..50 {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        _ => return Ok(()),
    }
    COVER_PID.store(c.as_ref().map_or(0, |(child, _)| child.id() as i32), Ordering::Release);
    TURN.fetch_add(1, Ordering::AcqRel);
    Ok(())
}

/// Whether the person at the Mac took its keyboard and mouse back (Control+P) since last asked.
pub fn taken_back() -> bool {
    TAKEN_BACK.swap(false, Ordering::Relaxed)
}

/// The cover's process, its windows made but not yet shown.
fn start_cover() -> Result<(Child, ChildStdin), String> {
    // Tests run as a test binary, which has no cover: they name the built program instead.
    #[cfg(test)]
    let exe = std::env::var_os("SSHREMOTE_EXE").map(std::path::PathBuf::from).ok_or("set SSHREMOTE_EXE")?;
    #[cfg(not(test))]
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg("--cover")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("the screen could not be covered: {e}"))?;
    let (input, output) = (child.stdin.take(), child.stdout.take());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        if let Some(o) = output {
            let _ = std::io::BufReader::new(o).read_line(&mut line);
        }
        let _ = tx.send(line);
    });
    match (rx.recv_timeout(Duration::from_secs(5)), input) {
        (Ok(l), Some(input)) if l.trim() == "ready" => Ok((child, input)),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            Err("the screen could not be covered here".into())
        }
    }
}

/// Drops what this program did not post; Control+P from the Mac's own keyboard ends it.
unsafe extern "C-unwind" fn tapped(_: CGEventTapProxy, kind: CGEventType, event: NonNull<CGEvent>, _: *mut c_void) -> *mut CGEvent {
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        if let Some(t) = NonNull::new(TAP.load(Ordering::Acquire)).filter(|_| BLOCK.load(Ordering::Relaxed)) {
            // SAFETY: the tap lives as long as the program.
            CGEvent::tap_enable(unsafe { t.as_ref() }, true);
        }
        return event.as_ptr();
    }
    // SAFETY: the event the tap is given, for the call.
    let e = unsafe { event.as_ref() };
    let moves = [CGEventType::MouseMoved, CGEventType::LeftMouseDragged, CGEventType::RightMouseDragged, CGEventType::OtherMouseDragged].contains(&kind);
    if !BLOCK.load(Ordering::Relaxed) {
        return event.as_ptr();
    }
    if CGEvent::integer_value_field(Some(e), CGEventField::EventSourceUserData) == MARK {
        if moves || matches!(kind, CGEventType::LeftMouseDown | CGEventType::LeftMouseUp | CGEventType::RightMouseDown | CGEventType::RightMouseUp) {
            keep_at(CGEvent::location(Some(e)));
        }
        return event.as_ptr();
    }
    if moves {
        let at = CGPoint::new(f64::from_bits(AT.0.load(Ordering::Relaxed)), f64::from_bits(AT.1.load(Ordering::Relaxed)));
        objc2_core_graphics::CGWarpMouseCursorPosition(at);
    }
    const P: i64 = 0x23;
    if kind == CGEventType::KeyDown && CGEvent::integer_value_field(Some(e), CGEventField::KeyboardEventKeycode) == P && CGEvent::flags(Some(e)).contains(CGEventFlags::MaskControl) {
        BLOCK.store(false, Ordering::Relaxed);
        TAKEN_BACK.store(true, Ordering::Relaxed);
    }
    std::ptr::null_mut()
}

fn keep_at(p: CGPoint) {
    AT.0.store(p.x.to_bits(), Ordering::Relaxed);
    AT.1.store(p.y.to_bits(), Ordering::Relaxed);
}

/// The tap on the system's input, made once on a thread of its own.
fn tap() -> Result<(), String> {
    static MADE: Mutex<bool> = Mutex::new(false);
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    if *made {
        return Ok(());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("input-kept-out".into())
        .spawn(move || {
            // Every kind of input: keys, buttons, moves, scrolling, gestures, the media keys.
            let mask = (1u64..=34).filter(|&k| k != 15).fold(0u64, |m, k| m | 1 << k);
            // SAFETY: the callback is the function above, which takes no user data.
            let t = unsafe { CGEvent::tap_create(CGEventTapLocation::HIDEventTap, CGEventTapPlacement::HeadInsertEventTap, CGEventTapOptions::Default, mask, Some(tapped), std::ptr::null_mut()) };
            let Some(t) = t else {
                let _ = tx.send(Err("the keyboard and mouse could not be kept out: SSH Remote needs Accessibility in System Settings > Privacy & Security".to_string()));
                return;
            };
            let Some(source) = CFMachPort::new_run_loop_source(None, Some(&t), 0) else {
                let _ = tx.send(Err("the keyboard and mouse could not be kept out".to_string()));
                return;
            };
            let Some(here) = CFRunLoop::current() else { return };
            // SAFETY: a mode the system defines.
            here.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
            CGEvent::tap_enable(&t, false);
            TAP.store(objc2_core_foundation::CFRetained::into_raw(t).as_ptr(), Ordering::Release);
            let _ = tx.send(Ok(()));
            CFRunLoop::run();
        })
        .map_err(|e| e.to_string())?;
    let r = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| Err("the keyboard and mouse could not be kept out in time".into()));
    *made = r.is_ok();
    r
}

/// `sshremote --cover`: black windows over every display, above all else, that let clicks
/// through and that screen capture leaves out. They show once a line `show` comes (the capture
/// leaves them out by then) or after three seconds; the process ends with its input.
pub fn run_cover() -> Result<(), String> {
    // SAFETY: loads the framework the classes below come from; a plain call.
    unsafe { libc::dlopen(c"/System/Library/Frameworks/AppKit.framework/AppKit".as_ptr(), libc::RTLD_LAZY) };
    let class = |n: &std::ffi::CStr| AnyClass::get(n).ok_or("AppKit is not on this Mac");
    let (app, screen, window, color, field, font) = (class(c"NSApplication")?, class(c"NSScreen")?, class(c"NSWindow")?, class(c"NSColor")?, class(c"NSTextField")?, class(c"NSFont")?);
    let level = objc2_core_graphics::CGShieldingWindowLevel() as isize;
    let mut shown: Vec<Retained<AnyObject>> = Vec::new();
    // SAFETY: AppKit's own calls with the arguments they take, on the main thread.
    unsafe {
        let app: Retained<AnyObject> = msg_send![app, sharedApplication];
        // An accessory: no Dock icon, no menu bar of its own.
        let _: bool = msg_send![&*app, setActivationPolicy: 1isize];
        let screens: Retained<AnyObject> = msg_send![screen, screens];
        let count: usize = msg_send![&*screens, count];
        let black: Retained<AnyObject> = msg_send![color, blackColor];
        let grey: Retained<AnyObject> = msg_send![color, colorWithWhite: 0.5f64, alpha: 1.0f64];
        let text = NSString::from_str("This computer is being used remotely");
        for i in 0..count {
            let s: Retained<AnyObject> = msg_send![&*screens, objectAtIndex: i];
            let frame: CGRect = msg_send![&*s, frame];
            let made: Allocated<AnyObject> = msg_send![window, alloc];
            let w: Option<Retained<AnyObject>> = msg_send![made, initWithContentRect: frame, styleMask: 0usize, backing: 2usize, defer: false];
            let Some(w) = w else { continue };
            let _: () = msg_send![&*w, setReleasedWhenClosed: false];
            let _: () = msg_send![&*w, setLevel: level];
            let _: () = msg_send![&*w, setBackgroundColor: &*black];
            let _: () = msg_send![&*w, setOpaque: true];
            let _: () = msg_send![&*w, setHasShadow: false];
            let _: () = msg_send![&*w, setIgnoresMouseEvents: true];
            // Left out of what is shared (NSWindowSharingNone), on every Space and over full screen.
            let _: () = msg_send![&*w, setSharingType: 0usize];
            let _: () = msg_send![&*w, setCollectionBehavior: (1usize | 16 | 64 | 256)];
            let _: () = msg_send![&*w, setAlphaValue: 0.0f64];
            let label: Retained<AnyObject> = msg_send![field, labelWithString: &*text];
            let size: Retained<AnyObject> = msg_send![font, systemFontOfSize: 24.0f64];
            let _: () = msg_send![&*label, setFont: &*size];
            let _: () = msg_send![&*label, setTextColor: &*grey];
            // NSTextAlignmentCenter on macOS.
            let _: () = msg_send![&*label, setAlignment: 2isize];
            let line = CGRect::new(CGPoint::new(0.0, frame.size.height / 2.0 - 20.0), CGSize::new(frame.size.width, 40.0));
            let _: () = msg_send![&*label, setFrame: line];
            let view: Retained<AnyObject> = msg_send![&*w, contentView];
            let _: () = msg_send![&*view, addSubview: &*label];
            let _: () = msg_send![&*w, orderFrontRegardless];
            shown.push(w);
        }
        if shown.is_empty() {
            return Err("no display to cover".into());
        }
        let ptrs: Vec<usize> = shown.iter().map(|w| Retained::as_ptr(w) as usize).collect();
        let show = move || {
            let ptrs = ptrs.clone();
            dispatch2::DispatchQueue::main().exec_async(move || {
                for &p in &ptrs {
                    // SAFETY: the windows live while the application runs, on the main thread here.
                    let _: () = msg_send![p as *mut AnyObject, setAlphaValue: 1.0f64];
                }
            });
        };
        let soon = show.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            soon();
        });
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(l) if l.trim() == "show" => show(),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            std::process::exit(0);
        });
        println!("ready");
        let _ = std::io::stdout().flush();
        let _: () = msg_send![&*app, run];
    }
    Ok(())
}
