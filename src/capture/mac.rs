//! macOS: the screen through ScreenCaptureKit, input as Core Graphics events. Both need the
//! person's yes, once, in System Settings > Privacy & Security (Screen & System Audio Recording,
//! and Accessibility): macOS asks for it the first time, and the viewer is told where it is.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGDisplayCopyDisplayMode, CGDisplayMode, CGEvent, CGEventField, CGEventFlags, CGEventSource,
    CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMainDisplayID, CGMouseButton, CGPreflightScreenCaptureAccess,
    CGRequestScreenCaptureAccess, CGScrollEventUnit,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType};

use super::{Capture, Image, Input, Monitor, Waker};
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};
use crate::xk;

/// Where to allow what the screen and the input need, as System Settings names it.
const SCREEN_CONSENT: &str = "This Mac has not let SSH Remote record its screen: allow it in System Settings > Privacy & Security > Screen & System Audio Recording, then connect again.";
const INPUT_CONSENT: &str = "This Mac has not let SSH Remote control it: allow it in System Settings > Privacy & Security > Accessibility; the screen is shown meanwhile.";

/// The newest picture the stream gave, and who to wake for it.
struct Latest {
    frame: Mutex<Option<Image>>,
    fresh: AtomicBool,
    wake: Waker,
}

impl Latest {
    /// Copies a sample's pixels (BGRA, rows `stride` apart), when it has any: a still screen's
    /// samples carry none.
    fn take(&self, buffer: &CMSampleBuffer) {
        let Some(pixels) = (unsafe { buffer.image_buffer() }) else { return };
        // SAFETY: the base address is read only between the lock and the unlock.
        unsafe {
            if CVPixelBufferLockBaseAddress(&pixels, CVPixelBufferLockFlags::ReadOnly) != 0 {
                return;
            }
            let (w, h, stride) = (CVPixelBufferGetWidth(&pixels), CVPixelBufferGetHeight(&pixels), CVPixelBufferGetBytesPerRow(&pixels));
            let base = CVPixelBufferGetBaseAddress(&pixels) as *const u8;
            if !base.is_null() && w > 0 && h > 0 && stride >= w * 4 {
                let src = std::slice::from_raw_parts(base, stride * h);
                let mut slot = self.frame.lock().unwrap_or_else(|e| e.into_inner());
                // The last picture's buffer when the agent has not taken it: no new one each time.
                let mut data = slot.take().map(|i| i.data).unwrap_or_default();
                data.clear();
                data.extend_from_slice(src);
                *slot = Some(Image { w, h, stride, data });
                self.fresh.store(true, Ordering::Release);
            }
            CVPixelBufferUnlockBaseAddress(&pixels, CVPixelBufferLockFlags::ReadOnly);
        }
        super::wake(&self.wake);
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and nothing here is unsafe to call.
    #[unsafe(super(NSObject))]
    #[name = "SshRemoteScreenOutput"]
    #[ivars = Arc<Latest>]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output(&self, _stream: &SCStream, buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind == SCStreamOutputType::Screen {
                self.ivars().take(buffer);
            }
        }
    }
);

impl Output {
    fn new(latest: Arc<Latest>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(latest);
        // SAFETY: NSObject's init on a fresh instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// Objective-C objects handed from the thread that made them to the one that waits.
struct Sent<T>(T);
// SAFETY: they are only passed on, never used by two threads at once.
unsafe impl<T> Send for Sent<T> {}

fn said(e: *mut NSError) -> String {
    // SAFETY: a non-null NSError from the system, read here only.
    unsafe { e.as_ref() }.map(|e| e.localizedDescription().to_string()).unwrap_or_else(|| "it did not say why".into())
}

/// What can be shared now: the displays, after the person allowed it (the first ask shows the
/// system's question).
fn shareable() -> Result<Retained<SCShareableContent>, String> {
    let (tx, rx) = mpsc::channel();
    let done = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: the content the handler is given, retained to outlive it.
        let r = match unsafe { Retained::retain(content) } {
            Some(c) => Ok(Sent(c)),
            None => Err(said(error)),
        };
        let _ = tx.send(r);
    });
    // SAFETY: the handler is called once, on a queue of the system's.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&done) };
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(c)) => Ok(c.0),
        Ok(Err(_)) if !CGPreflightScreenCaptureAccess() => Err(SCREEN_CONSENT.into()),
        Ok(Err(e)) => Err(format!("the screen could not be listed: {e}")),
        Err(_) => Err("the screen could not be listed in time".into()),
    }
}

/// One display as both of macOS's measures have it: its place and size in points (where events
/// go) and in pixels (what the picture has).
#[derive(Clone)]
struct Display {
    id: u32,
    at: (f64, f64),
    points: (f64, f64),
    pixels: (usize, usize),
}

/// A stream of one display's pictures, ended when dropped.
struct Live {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
}

impl Drop for Live {
    fn drop(&mut self) {
        // SAFETY: stopping a started stream; nothing waits for the answer.
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}

fn config(size: (usize, usize), fps: u8) -> Retained<SCStreamConfiguration> {
    // SAFETY: a fresh configuration, set before any stream takes it.
    unsafe {
        let c = SCStreamConfiguration::new();
        c.setWidth(size.0.max(2));
        c.setHeight(size.1.max(2));
        c.setPixelFormat(kCVPixelFormatType_32BGRA);
        // The pointer comes in the pictures, as the system draws it.
        c.setShowsCursor(true);
        c.setMinimumFrameInterval(CMTime::new(1, fps.clamp(1, 60) as i32));
        c.setQueueDepth(4);
        c
    }
}

fn start(display: &SCDisplay, size: (usize, usize), fps: u8, latest: Arc<Latest>) -> Result<Live, String> {
    // SAFETY: Objective-C calls on objects made here, the stream started once with a handler
    // called once.
    unsafe {
        let filter = SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), display, &NSArray::new());
        let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config(size, fps), None);
        let output = Output::new(latest);
        let queue = DispatchQueue::new("dev.sshremote.screen", None);
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), SCStreamOutputType::Screen, Some(&queue))
            .map_err(|e| format!("the screen's pictures could not be taken: {}", e.localizedDescription()))?;
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |error: *mut NSError| {
            let _ = tx.send((!error.is_null()).then(|| said(error)));
        });
        stream.startCaptureWithCompletionHandler(Some(&done));
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(None) => Ok(Live { stream, _output: output, _queue: queue }),
            Ok(Some(_)) if !CGPreflightScreenCaptureAccess() => Err(SCREEN_CONSENT.into()),
            Ok(Some(e)) => Err(format!("the screen's pictures did not start: {e}")),
            Err(_) => Err("the screen's pictures did not start in time".into()),
        }
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    // ApplicationServices (HIServices): whether this program may post input, asking if not.
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
}

/// Whether input may be posted; the first time it may not, macOS's own question is shown.
fn may_post(asked: &mut bool) -> bool {
    use objc2_foundation::{NSDictionary, NSNumber, NSString};
    let prompt = !std::mem::replace(asked, true);
    let key = NSString::from_str("AXTrustedCheckOptionPrompt");
    let options = NSDictionary::from_slices(&[&*key], &[&*NSNumber::new_bool(prompt)]);
    // SAFETY: a dictionary, toll-free bridged to the CFDictionary the call takes.
    unsafe { AXIsProcessTrustedWithOptions(Retained::as_ptr(&options) as *const std::ffi::c_void) }
}

pub struct Mac {
    content: Retained<SCShareableContent>,
    displays: Vec<Display>,
    current: usize,
    target: (usize, usize),
    fps: u8,
    live: Option<Live>,
    latest: Arc<Latest>,
    /// Where the pointer was sent, in points, and the buttons held there.
    at: CGPoint,
    held: [bool; 5],
    /// The last click, to count double and triple clicks as macOS does.
    last_click: Option<(Instant, CGPoint, u8, i64)>,
    /// Modifier keys held (keysyms), for the flags of the keys typed meanwhile.
    mods: Vec<u32>,
    /// Asked for Accessibility already; told the viewer already.
    asked: bool,
    notice: Option<String>,
    /// When the stream was last given a new size: pictures of the old one still come a while.
    resized: Option<Instant>,
}

impl Mac {
    pub fn open() -> Result<Mac, String> {
        if !CGPreflightScreenCaptureAccess() {
            // The system's question (once; then System Settings has it).
            CGRequestScreenCaptureAccess();
        }
        let content = shareable()?;
        let main = CGMainDisplayID();
        // SAFETY: reading the displays the content lists.
        let mut displays: Vec<Display> = unsafe { content.displays() }
            .iter()
            .map(|d| {
                // SAFETY: plain getters of a listed display.
                let (id, frame) = unsafe { (d.displayID(), d.frame()) };
                let mode = CGDisplayCopyDisplayMode(id);
                let pixels = (CGDisplayMode::pixel_width(mode.as_deref()), CGDisplayMode::pixel_height(mode.as_deref()));
                let points = (frame.size.width, frame.size.height);
                // A display whose mode cannot be read is taken at one pixel a point.
                let pixels = if pixels.0 == 0 { (points.0 as usize, points.1 as usize) } else { pixels };
                Display { id, at: (frame.origin.x, frame.origin.y), points, pixels }
            })
            .collect();
        if displays.is_empty() {
            return Err("this Mac shows no display to share".into());
        }
        // The main display first, then left to right.
        displays.sort_by(|a, b| (a.id != main).cmp(&(b.id != main)).then(a.at.0.total_cmp(&b.at.0)));
        let target = displays[0].pixels;
        let latest = Arc::new(Latest { frame: Mutex::new(None), fresh: AtomicBool::new(false), wake: Default::default() });
        let mut mac = Mac {
            content,
            displays,
            current: 0,
            target,
            fps: 30,
            live: None,
            latest,
            at: CGPoint::new(0.0, 0.0),
            held: [false; 5],
            last_click: None,
            mods: Vec::new(),
            asked: false,
            notice: None,
            resized: None,
        };
        mac.restart()?;
        Ok(mac)
    }

    fn display(&self) -> &Display {
        &self.displays[self.current]
    }

    /// The SCDisplay of the one shown.
    fn sc_display(&self) -> Option<Retained<SCDisplay>> {
        let id = self.display().id;
        // SAFETY: reading the listed displays.
        unsafe { self.content.displays() }.iter().find(|d| unsafe { d.displayID() } == id)
    }

    /// The pictures' size: the viewer's, never more than the display's own pixels.
    fn size(&self) -> (usize, usize) {
        let (dw, dh) = self.display().pixels;
        let k = (self.target.0 as f64 / dw as f64).min(self.target.1 as f64 / dh as f64).min(1.0);
        (((dw as f64 * k) as usize).max(2) & !1, ((dh as f64 * k) as usize).max(2) & !1)
    }

    fn restart(&mut self) -> Result<(), String> {
        self.live = None;
        let d = self.sc_display().ok_or("that display is gone")?;
        self.live = Some(start(&d, self.size(), self.fps, self.latest.clone())?);
        Ok(())
    }

    /// New settings for the running stream (its size, its rate).
    fn reconfigure(&mut self) {
        self.resized = Some(Instant::now());
        if let Some(l) = &self.live {
            // SAFETY: a fresh configuration given to the running stream.
            unsafe { l.stream.updateConfiguration_completionHandler(&config(self.size(), self.fps), None) };
        }
    }

    /// Desktop pixels of the display shown to the point events go to.
    fn point(&self, x: f64, y: f64) -> CGPoint {
        let d = self.display();
        let (kx, ky) = (d.points.0 / d.pixels.0 as f64, d.points.1 / d.pixels.1 as f64);
        CGPoint::new(d.at.0 + x.clamp(0.0, d.pixels.0 as f64 - 1.0) * kx, d.at.1 + y.clamp(0.0, d.pixels.1 as f64 - 1.0) * ky)
    }

    fn flags(&self) -> CGEventFlags {
        let mut f = CGEventFlags::empty();
        for &k in &self.mods {
            f |= match k {
                xk::SHIFT_L | xk::SHIFT_R => CGEventFlags::MaskShift,
                xk::CONTROL_L | xk::CONTROL_R => CGEventFlags::MaskControl,
                xk::ALT_L | xk::ALT_R => CGEventFlags::MaskAlternate,
                xk::SUPER_L | xk::SUPER_R => CGEventFlags::MaskCommand,
                _ => CGEventFlags::empty(),
            };
        }
        f
    }

    fn post(&mut self, e: Option<CFRetained<CGEvent>>) -> Result<(), String> {
        let e = e.ok_or("the event could not be made")?;
        if !may_post(&mut self.asked) {
            if self.notice.is_none() {
                self.notice = Some(INPUT_CONSENT.into());
            }
            return Ok(());
        }
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
        Ok(())
    }

    fn mouse(&mut self, kind: CGEventType, button: CGMouseButton, clicks: i64) -> Result<(), String> {
        let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
        let e = CGEvent::new_mouse_event(src.as_deref(), kind, self.at, button);
        if let Some(e) = &e {
            CGEvent::set_flags(Some(e), self.flags());
            if clicks > 0 {
                CGEvent::set_integer_value_field(Some(e), CGEventField::MouseEventClickState, clicks);
            }
            if button.0 > 2 {
                CGEvent::set_integer_value_field(Some(e), CGEventField::MouseEventButtonNumber, button.0 as i64);
            }
        }
        self.post(e)
    }

    /// The pointer moved to `at`, as a drag while a button is held.
    fn moved(&mut self, at: CGPoint, by: Option<(i32, i32)>) -> Result<(), String> {
        self.at = at;
        let (kind, button) = match self.held {
            [true, ..] => (CGEventType::LeftMouseDragged, CGMouseButton::Left),
            [_, _, true, ..] => (CGEventType::RightMouseDragged, CGMouseButton::Right),
            [_, true, ..] | [.., true, _] | [.., true] => (CGEventType::OtherMouseDragged, CGMouseButton::Center),
            _ => (CGEventType::MouseMoved, CGMouseButton::Left),
        };
        let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
        let e = CGEvent::new_mouse_event(src.as_deref(), kind, at, button);
        if let (Some(e), Some((dx, dy))) = (&e, by) {
            // Games read the movement, not the place.
            CGEvent::set_integer_value_field(Some(e), CGEventField::MouseEventDeltaX, dx as i64);
            CGEvent::set_integer_value_field(Some(e), CGEventField::MouseEventDeltaY, dy as i64);
        }
        self.post(e)
    }

    fn button(&mut self, button: u8, pressed: bool) -> Result<(), String> {
        // 1 left, 2 middle, 3 right, 8 and 9 back and forward, as X numbers them.
        let (slot, cg) = match button {
            1 => (0, CGMouseButton::Left),
            2 => (1, CGMouseButton::Center),
            3 => (2, CGMouseButton::Right),
            8 => (3, CGMouseButton(3)),
            9 => (4, CGMouseButton(4)),
            _ => return Ok(()),
        };
        self.held[slot] = pressed;
        let kind = match (slot, pressed) {
            (0, true) => CGEventType::LeftMouseDown,
            (0, false) => CGEventType::LeftMouseUp,
            (2, true) => CGEventType::RightMouseDown,
            (2, false) => CGEventType::RightMouseUp,
            (_, true) => CGEventType::OtherMouseDown,
            (_, false) => CGEventType::OtherMouseUp,
        };
        // Presses of one button close together in time and place make a double or triple click.
        let clicks = match (pressed, self.last_click) {
            (true, Some((t, p, b, n))) if b == button && t.elapsed() < Duration::from_millis(500) && (p.x - self.at.x).abs() < 4.0 && (p.y - self.at.y).abs() < 4.0 => n + 1,
            (true, _) => 1,
            (false, Some((_, _, b, n))) if b == button => n,
            (false, _) => 1,
        };
        if pressed {
            self.last_click = Some((Instant::now(), self.at, button, clicks));
        }
        self.mouse(kind, cg, clicks)
    }

    fn scroll(&mut self, vertical: i32, horizontal: i32) -> Result<(), String> {
        let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
        // Up and left are positive here; the viewer's steps go down and right.
        let e = CGEvent::new_scroll_wheel_event2(src.as_deref(), CGScrollEventUnit::Line, 2, -vertical, -horizontal, 0);
        self.post(e)
    }

    fn key(&mut self, code: u16, down: bool, flags: CGEventFlags) -> Result<(), String> {
        let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
        let e = CGEvent::new_keyboard_event(src.as_deref(), code, down);
        if let Some(e) = &e {
            CGEvent::set_flags(Some(e), flags);
        }
        self.post(e)
    }

    /// Text where no key types it: one key event carrying it, as input methods send theirs.
    fn text(&mut self, text: &str) -> Result<(), String> {
        let units: Vec<u16> = text.encode_utf16().collect();
        for chunk in units.chunks(16) {
            for down in [true, false] {
                let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
                let e = CGEvent::new_keyboard_event(src.as_deref(), 0, down);
                if let Some(e) = &e {
                    // SAFETY: the units outlive the call, their count given.
                    unsafe { CGEvent::keyboard_set_unicode_string(Some(e), chunk.len() as _, chunk.as_ptr()) };
                }
                self.post(e)?;
            }
        }
        Ok(())
    }
}

/// macOS's key code (where the key is on an ANSI keyboard) for a keysym, and whether the
/// character needs Shift there.
fn code_of(keysym: u32) -> Option<(u16, bool)> {
    const LETTERS: [u16; 26] = [0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f, 0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06];
    const DIGITS: [u16; 10] = [0x1d, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1a, 0x1c, 0x19];
    if let Some(code) = special(keysym) {
        return Some((code, false));
    }
    Some(match keysym {
        0x61..=0x7a => (LETTERS[(keysym - 0x61) as usize], false),
        0x41..=0x5a => (LETTERS[(keysym - 0x41) as usize], true),
        0x30..=0x39 => (DIGITS[(keysym - 0x30) as usize], false),
        0x20 => (0x31, false),
        // The other characters of an ANSI keyboard, each with the one Shift makes of it.
        _ => {
            const KEYS: [(u16, char, char); 11] = [
                (0x27, '\'', '"'),
                (0x2b, ',', '<'),
                (0x1b, '-', '_'),
                (0x2f, '.', '>'),
                (0x2c, '/', '?'),
                (0x29, ';', ':'),
                (0x18, '=', '+'),
                (0x21, '[', '{'),
                (0x2a, '\\', '|'),
                (0x1e, ']', '}'),
                (0x32, '`', '~'),
            ];
            const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
            let c = char::from_u32(keysym).filter(|c| c.is_ascii())?;
            if let Some(i) = SHIFTED_DIGITS.find(c) {
                return Some((DIGITS[i], true));
            }
            let &(code, plain, _) = KEYS.iter().find(|k| k.1 == c || k.2 == c)?;
            (code, c != plain)
        }
    })
}

/// Keys that type no character.
fn special(keysym: u32) -> Option<u16> {
    const F: [u16; 20] = [0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f, 0x69, 0x6b, 0x71, 0x6a, 0x40, 0x4f, 0x50, 0x5a];
    Some(match keysym {
        xk::RETURN => 0x24,
        xk::TAB => 0x30,
        xk::BACKSPACE => 0x33,
        xk::ESCAPE => 0x35,
        xk::DELETE => 0x75,
        xk::HOME => 0x73,
        xk::END => 0x77,
        xk::PAGE_UP => 0x74,
        xk::PAGE_DOWN => 0x79,
        xk::LEFT => 0x7b,
        xk::RIGHT => 0x7c,
        xk::DOWN => 0x7d,
        xk::UP => 0x7e,
        // Insert: the Help key, where Apple keyboards had one.
        xk::INSERT => 0x72,
        xk::SHIFT_L => 0x38,
        xk::SHIFT_R => 0x3c,
        xk::CONTROL_L => 0x3b,
        xk::CONTROL_R => 0x3e,
        xk::ALT_L => 0x3a,
        xk::ALT_R => 0x3d,
        xk::SUPER_L => 0x37,
        xk::SUPER_R => 0x36,
        xk::CAPS_LOCK => 0x39,
        // The keypad's Enter.
        0xff8d => 0x4c,
        k if (xk::F1..xk::F1 + 20).contains(&k) => F[(k - xk::F1) as usize],
        _ => return None,
    })
}

fn modifier(keysym: u32) -> bool {
    matches!(keysym, xk::SHIFT_L | xk::SHIFT_R | xk::CONTROL_L | xk::CONTROL_R | xk::ALT_L | xk::ALT_R | xk::SUPER_L | xk::SUPER_R)
}

impl Capture for Mac {
    fn name(&self) -> &'static str {
        "mac"
    }

    fn monitors(&self) -> Vec<Monitor> {
        let main = CGMainDisplayID();
        self.displays.iter().enumerate().map(|(i, d)| Monitor { name: format!("display {}", i + 1), w: d.pixels.0, h: d.pixels.1, primary: d.id == main }).collect()
    }

    fn current(&self) -> usize {
        self.current
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        if index >= self.displays.len() {
            return Err("this Mac has no such display".into());
        }
        if index == self.current {
            return Ok(());
        }
        let was = self.current;
        self.current = index;
        if let Err(e) = self.restart() {
            self.current = was;
            let _ = self.restart();
            return Err(e);
        }
        Ok(())
    }

    fn desktop_size(&self) -> (usize, usize) {
        self.display().pixels
    }

    fn origin(&self) -> (i32, i32) {
        let d = self.display();
        ((d.at.0 * d.pixels.0 as f64 / d.points.0) as i32, (d.at.1 * d.pixels.1 as f64 / d.points.1) as i32)
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        let before = self.size();
        self.target = (w.max(1), h.max(1));
        if self.size() != before {
            self.reconfigure();
        }
        Ok(())
    }

    fn set_fps(&mut self, fps: u8) {
        if fps != self.fps && fps > 0 {
            self.fps = fps;
            self.reconfigure();
        }
    }

    fn set_waker(&mut self, wake: Box<dyn Fn() + Send>) -> bool {
        *self.latest.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
        true
    }

    fn notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        if !self.latest.fresh.swap(false, Ordering::AcqRel) {
            return Ok(None);
        }
        let img = self.latest.frame.lock().unwrap_or_else(|e| e.into_inner()).take();
        // Pictures of the size before a change, still on their way, are passed over.
        if let Some(i) = &img
            && (i.w, i.h) != self.size()
            && self.resized.is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return Ok(None);
        }
        Ok(img)
    }

    fn recycle(&mut self, img: Image) {
        let mut slot = self.latest.frame.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() && !self.latest.fresh.load(Ordering::Acquire) {
            // Kept for the next picture's pixels; never taken as a picture itself.
            *slot = Some(Image { w: 0, h: 0, stride: 0, data: img.data });
        }
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        match ev {
            Input::Move { x, y } => {
                let at = self.point(x, y);
                self.moved(at, None)
            }
            Input::MoveBy { dx, dy } => {
                let d = self.display().clone();
                let (kx, ky) = (d.points.0 / d.pixels.0 as f64, d.points.1 / d.pixels.1 as f64);
                let at = CGPoint::new(
                    (self.at.x + dx as f64 * kx).clamp(d.at.0, d.at.0 + d.points.0 - 1.0),
                    (self.at.y + dy as f64 * ky).clamp(d.at.1, d.at.1 + d.points.1 - 1.0),
                );
                self.moved(at, Some((dx, dy)))
            }
            Input::Button { button, pressed } => self.button(button, pressed),
            Input::Scroll { steps } => self.scroll(steps, 0),
            Input::HScroll { steps } => self.scroll(0, steps),
            Input::KeyState { keysym, down } => {
                if modifier(keysym) {
                    self.mods.retain(|&k| k != keysym);
                    if down {
                        self.mods.push(keysym);
                    }
                }
                match code_of(keysym) {
                    Some((code, _)) => self.key(code, down, self.flags()),
                    // A character no key types here: as text, once, when it goes down.
                    None if down => {
                        let c = if xk::is_unicode(keysym) { keysym & 0xff_ffff } else { keysym };
                        char::from_u32(c).map_or(Ok(()), |c| self.text(&c.to_string()))
                    }
                    None => Ok(()),
                }
            }
            Input::Key { keysym, mods } => {
                let shortcut = mods & (MOD_CTRL | MOD_ALT | MOD_SUPER) != 0;
                let code = special(keysym).map(|c| (c, false)).or_else(|| if shortcut || keysym < 0x80 { code_of(keysym) } else { None });
                let Some((code, shift)) = code else {
                    let c = if xk::is_unicode(keysym) { keysym & 0xff_ffff } else { keysym };
                    return char::from_u32(c).map_or(Ok(()), |c| self.text(&c.to_string()));
                };
                let mut flags = self.flags();
                for (bit, f) in [(MOD_SHIFT, CGEventFlags::MaskShift), (MOD_CTRL, CGEventFlags::MaskControl), (MOD_ALT, CGEventFlags::MaskAlternate), (MOD_SUPER, CGEventFlags::MaskCommand)] {
                    if mods & bit != 0 {
                        flags |= f;
                    }
                }
                if shift {
                    flags |= CGEventFlags::MaskShift;
                }
                self.key(code, true, flags)?;
                self.key(code, false, flags)
            }
            Input::SecureAttention => Ok(()),
        }
    }

    fn commit_text(&mut self, text: &str) -> bool {
        self.text(text).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_find_their_place_on_a_mac_keyboard() {
        assert_eq!(code_of('a' as u32), Some((0x00, false)));
        assert_eq!(code_of('Z' as u32), Some((0x06, true)));
        assert_eq!(code_of('!' as u32), Some((0x12, true)));
        assert_eq!(code_of('/' as u32), Some((0x2c, false)));
        assert_eq!(code_of('?' as u32), Some((0x2c, true)));
        assert_eq!(code_of(xk::RETURN), Some((0x24, false)));
        assert_eq!(code_of(xk::F1 + 11), Some((0x6f, false)));
        assert_eq!(code_of(xk::unicode('한' as u32)), None);
    }

    /// The screen as macOS gives it, and the pointer moved on it: only on a Mac that allowed this
    /// program to record and control it. SSHREMOTE_MAC_TEST=1 cargo test -- --ignored a_mac_screen
    #[test]
    #[ignore]
    fn a_mac_screen_is_read_and_driven() {
        if std::env::var_os("SSHREMOTE_MAC_TEST").is_none() {
            return;
        }
        let mut m = Mac::open().expect("the screen");
        let (w, h) = m.desktop_size();
        assert!(w > 0 && h > 0);
        m.set_target(640, 400).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let img = loop {
            if let Some(i) = m.grab().unwrap()
                && i.w > 0
                && i.w <= 640
            {
                break i;
            }
            assert!(Instant::now() < until, "no picture came");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(img.w <= 640 && img.h <= 400 && img.data.len() >= img.stride * img.h, "{}x{}", img.w, img.h);
        let here = || CGEvent::new(None).map(|e| CGEvent::location(Some(&e))).expect("where the pointer is");
        m.input(Input::Move { x: w as f64 / 2.0, y: h as f64 / 2.0 }).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let want = m.point(w as f64 / 2.0, h as f64 / 2.0);
        let at = here();
        println!("{w}x{h} shown as {}x{}, notice {:?}; pointer at {:.0},{:.0} (sent {:.0},{:.0})", img.w, img.h, m.notice(), at.x, at.y, want.x, want.y);
        assert!((at.x - want.x).abs() < 2.0 && (at.y - want.y).abs() < 2.0, "the pointer went where it was sent");
        m.input(Input::MoveBy { dx: 20, dy: -10 }).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let by = here();
        assert!(by.x > at.x && by.y < at.y, "moved by: {:.0},{:.0}", by.x, by.y);
    }
}
