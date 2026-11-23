//! macOS: the screen (or one window, or one program's windows) and its sound through
//! ScreenCaptureKit, input as Core Graphics events. Both need the person's yes, once, in System
//! Settings > Privacy & Security (Screen & System Audio Recording, and Accessibility): macOS asks
//! for it the first time, and the viewer is told where it is.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFDictionary, CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode, CGEvent, CGEventField, CGEventFlags, CGEventSource,
    CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMainDisplayID, CGMouseButton, CGPreflightScreenCaptureAccess,
    CGRectMakeWithDictionaryRepresentation, CGRequestScreenCaptureAccess, CGScrollEventUnit, CGWindowListCopyWindowInfo,
    CGWindowListOption,
};
use objc2_core_media::{CMAudioFormatDescriptionGetStreamBasicDescription, CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCRunningApplication, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType, SCWindow,
};

use super::{Capture, Image, Input, Monitor, Share, WindowInfo, Waker};
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};
use crate::xk;

/// Where to allow what the screen and the input need, as System Settings names it.
const SCREEN_CONSENT: &str = "This Mac has not let SSH Remote record its screen: allow it in System Settings > Privacy & Security > Screen & System Audio Recording, then connect again.";
const INPUT_CONSENT: &str = "This Mac has not let SSH Remote control it: allow it in System Settings > Privacy & Security > Accessibility; the screen is shown meanwhile.";
const NO_METAL: &str = "This Mac draws without Metal graphics (a virtual machine with no graphics card of its own), where showing one window or program alone crashes its window server: only the whole screen can be shared.";

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
            self.put(CVPixelBufferGetBaseAddress(&pixels) as *const u8, w, h, stride);
            CVPixelBufferUnlockBaseAddress(&pixels, CVPixelBufferLockFlags::ReadOnly);
        }
        super::wake(&self.wake);
    }

    /// Keeps a picture of `h` rows `stride` bytes apart at `base`, while it is locked.
    unsafe fn put(&self, base: *const u8, w: usize, h: usize, stride: usize) {
        if base.is_null() || w == 0 || h == 0 || stride < w * 4 {
            return;
        }
        // SAFETY: the caller holds the pixels locked, `stride * h` bytes from `base`.
        let src = unsafe { std::slice::from_raw_parts(base, stride * h) };
        let mut slot = self.frame.lock().unwrap_or_else(|e| e.into_inner());
        // The last picture's buffer when the agent has not taken it: no new one each time.
        let mut data = slot.take().map(|i| i.data).unwrap_or_default();
        data.clear();
        data.extend_from_slice(src);
        *slot = Some(Image { w, h, stride, data });
        self.fresh.store(true, Ordering::Release);
    }
}

/// A display's pictures at the login window, taken a few times a second: ScreenCaptureKit's
/// streams came black there or failed, and Core Graphics' display streams came black, while its
/// still pictures of the display showed the login window every time.
struct Drawn {
    stop: Arc<AtomicBool>,
    taker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Drawn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.taker.take() {
            let _ = t.join();
        }
    }
}

/// Starts taking display `id`'s pictures at `size`, at most `fps` a second (15 at most: each is a
/// whole screen read again), passing on only those that differ from the one before.
#[allow(deprecated)]
fn draw(id: u32, size: (usize, usize), fps: u8, latest: Arc<Latest>) -> Result<Drawn, String> {
    use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGDisplayCreateImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGInterpolationQuality};
    if CGDisplayCreateImage(id).is_none() {
        return Err("the login window's screen could not be read".into());
    }
    let (w, h) = (size.0.max(2), size.1.max(2));
    let stop = Arc::new(AtomicBool::new(false));
    let halt = stop.clone();
    let every = Duration::from_secs_f64(1.0 / fps.clamp(1, 15) as f64);
    let taker = std::thread::Builder::new()
        .name("login-window-pictures".into())
        .spawn(move || {
            let space = CGColorSpace::new_device_rgb();
            let (mut pixels, mut last) = (vec![0u8; w * 4 * h], Vec::new());
            while !halt.load(Ordering::Acquire) {
                let began = Instant::now();
                if let Some(img) = CGDisplayCreateImage(id) {
                    // SAFETY: a context drawing into `pixels`, `w * 4` bytes a row, gone before they are read.
                    let drawn = unsafe {
                        CGBitmapContextCreate(pixels.as_mut_ptr().cast(), w, h, 8, w * 4, space.as_deref(), CGImageAlphaInfo::NoneSkipFirst.0 | CGImageByteOrderInfo::Order32Little.0)
                    }
                    .map(|c| {
                        CGContext::set_interpolation_quality(Some(&c), CGInterpolationQuality::Medium);
                        CGContext::draw_image(Some(&c), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64)), Some(&img));
                    });
                    if drawn.is_some() && pixels != last {
                        // SAFETY: `pixels` holds `h` rows of `w * 4` bytes.
                        unsafe { latest.put(pixels.as_ptr(), w, h, w * 4) };
                        super::wake(&latest.wake);
                        last.clone_from(&pixels);
                    }
                }
                std::thread::sleep(every.saturating_sub(began.elapsed()));
            }
        })
        .map_err(|e| format!("the login window's screen could not be read: {e}"))?;
    Ok(Drawn { stop, taker: Some(taker) })
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

/// Who takes the sound: interleaved samples, their rate and their channels.
type Hear = Mutex<Box<dyn FnMut(&[f32], u32, usize) + Send>>;

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and nothing here is unsafe to call.
    #[unsafe(super(NSObject))]
    #[name = "SshRemoteSoundOutput"]
    #[ivars = Hear]
    struct SoundOutput;

    unsafe impl NSObjectProtocol for SoundOutput {}

    unsafe impl SCStreamOutput for SoundOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output(&self, _stream: &SCStream, buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind == SCStreamOutputType::Audio
                && let Some((pcm, rate, ch)) = samples(buffer)
            {
                (self.ivars().lock().unwrap_or_else(|e| e.into_inner()))(&pcm, rate, ch);
            }
        }
    }
);

impl SoundOutput {
    fn new(hear: Hear) -> Retained<Self> {
        let this = Self::alloc().set_ivars(hear);
        // SAFETY: NSObject's init on a fresh instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// A sound sample's frames as interleaved 32-bit floats, their rate and channels; ScreenCaptureKit
/// gives each channel after the other.
fn samples(buffer: &CMSampleBuffer) -> Option<(Vec<f32>, u32, usize)> {
    const LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");
    const FLOAT: u32 = 1;
    const NON_INTERLEAVED: u32 = 1 << 5;
    // SAFETY: getters of a valid sample, its bytes copied out within their length.
    unsafe {
        let format = buffer.format_description()?;
        let a = CMAudioFormatDescriptionGetStreamBasicDescription(&format).as_ref()?;
        let ch = a.mChannelsPerFrame as usize;
        if a.mFormatID != LINEAR_PCM || a.mFormatFlags & FLOAT == 0 || a.mBitsPerChannel != 32 || ch == 0 {
            return None;
        }
        let frames = buffer.num_samples().max(0) as usize;
        let block = buffer.data_buffer()?;
        let mut raw = vec![0f32; block.data_length() / 4];
        if raw.is_empty() || block.copy_data_bytes(0, raw.len() * 4, NonNull::new(raw.as_mut_ptr().cast())?) != 0 {
            return None;
        }
        if a.mFormatFlags & NON_INTERLEAVED == 0 {
            raw.truncate(frames * ch);
            return Some((raw, a.mSampleRate as u32, ch));
        }
        let plane = raw.len() / ch;
        let frames = frames.min(plane);
        let mut out = Vec::with_capacity(frames * ch);
        for i in 0..frames {
            out.extend((0..ch).map(|c| raw[c * plane + i]));
        }
        Some((out, a.mSampleRate as u32, ch))
    }
}

/// This Mac's sound, as it plays, handed to `hear` until what this returns is dropped: a stream
/// of the main display that takes nothing but its sound (macOS 13 and later; allowed with the
/// screen, as Screen & System Audio Recording). The sound this program plays itself is left out.
pub fn sound(hear: Box<dyn FnMut(&[f32], u32, usize) + Send>) -> Result<Box<dyn std::any::Any>, String> {
    // The pictures it must have are the smallest and slowest there are.
    let c = config((2, 2), 1, None);
    // SAFETY: a fresh configuration, set before the stream takes it.
    unsafe {
        c.setCapturesAudio(true);
        c.setExcludesCurrentProcessAudio(true);
        c.setSampleRate(48_000);
        c.setChannelCount(2);
    }
    listen(hear, &c, false)
}

/// A stream of the main display with settings `c` whose sound goes to `hear` (and pictures, when
/// `seen`, nowhere).
fn listen(hear: Box<dyn FnMut(&[f32], u32, usize) + Send>, c: &SCStreamConfiguration, seen: bool) -> Result<Box<dyn std::any::Any>, String> {
    if !objc2::available!(macos = 13.0) {
        return Err("sound is taken from macOS 13 on".into());
    }
    if !CGPreflightScreenCaptureAccess() {
        return Err(SCREEN_CONSENT.into());
    }
    let content = shareable()?;
    let main = CGMainDisplayID();
    // SAFETY: reading the displays the content lists.
    let display = unsafe { content.displays() }.iter().find(|d| unsafe { d.displayID() } == main).ok_or("this Mac shows no display")?;
    // SAFETY: a fresh filter for one stream, started once with a handler called once.
    unsafe {
        let filter = SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &NSArray::new());
        let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, c, None);
        let output = SoundOutput::new(Mutex::new(hear));
        let queue = DispatchQueue::new("dev.sshremote.sound", None);
        let kinds: &[SCStreamOutputType] = if seen { &[SCStreamOutputType::Audio, SCStreamOutputType::Screen] } else { &[SCStreamOutputType::Audio] };
        for &kind in kinds {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(ProtocolObject::from_ref(&*output), kind, Some(&queue))
                .map_err(|e| format!("the sound could not be taken: {}", e.localizedDescription()))?;
        }
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |error: *mut NSError| {
            let _ = tx.send((!error.is_null()).then(|| said(error)));
        });
        stream.startCaptureWithCompletionHandler(Some(&done));
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(None) => Ok(Box::new(Heard { stream, _output: output, _queue: queue })),
            Ok(Some(e)) => Err(format!("the sound did not start: {e}")),
            Err(_) => Err("the sound did not start in time".into()),
        }
    }
}

/// The sound's stream, ended when dropped.
struct Heard {
    stream: Retained<SCStream>,
    _output: Retained<SoundOutput>,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
}

impl Drop for Heard {
    fn drop(&mut self) {
        // SAFETY: stopping a started stream; nothing waits for the answer.
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}

/// Objective-C objects handed from the thread that made them to the one that waits.
struct Sent<T>(T);
// SAFETY: they are only passed on, never used by two threads at once.
unsafe impl<T> Send for Sent<T> {}

fn said(e: *mut NSError) -> String {
    // SAFETY: a non-null NSError from the system, read here only.
    unsafe { e.as_ref() }.map(|e| format!("{} ({} {})", e.localizedDescription(), e.domain(), e.code())).unwrap_or_else(|| "it did not say why".into())
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

/// A stream's settings: its pictures' size and rate, and the part of the display they show
/// (points from the display's corner; all of it when None).
fn config(size: (usize, usize), fps: u8, part: Option<CGRect>) -> Retained<SCStreamConfiguration> {
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
        if let Some(r) = part {
            c.setSourceRect(r);
        }
        c
    }
}

fn start(filter: &SCContentFilter, settings: &SCStreamConfiguration, latest: Arc<Latest>) -> Result<Live, String> {
    // SAFETY: Objective-C calls on objects made here, the stream started once with a handler
    // called once.
    unsafe {
        let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), filter, settings, None);
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
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    // A program's accessibility element, its attributes and actions: to bring a window forward.
    fn AXUIElementCreateApplication(pid: i32) -> *const c_void;
    fn AXUIElementSetAttributeValue(element: *const c_void, attribute: *const c_void, value: *const c_void) -> i32;
    fn AXUIElementCopyAttributeValue(element: *const c_void, attribute: *const c_void, value: *mut *const c_void) -> i32;
    fn AXUIElementPerformAction(element: *const c_void, action: *const c_void) -> i32;
    // The window server's number of a window's element (what window managers use for it).
    fn _AXUIElementGetWindow(element: *const c_void, id: *mut u32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    // The Mac's graphics device for Metal; none where macOS draws without one.
    fn MTLCreateSystemDefaultDevice() -> *mut AnyObject;
}

/// Whether this Mac draws with Metal. Without it (virtual machines with no graphics card of their
/// own) the window server composes in software, and a stream of one program's windows makes it
/// abort (seen on macOS 26 in VMware: an assertion in CA::OGL::Context::push_surface).
fn metal() -> bool {
    static METAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // SAFETY: the device made here (or none) is owned and released at once.
    *METAL.get_or_init(|| unsafe { Retained::from_raw(MTLCreateSystemDefaultDevice()) }.is_some())
}

/// Brings a program forward, and its window `id` in front of its others; nothing happens where
/// this program may not control the Mac.
fn raise(pid: i32, id: Option<u32>) {
    let name = |s: &str| Retained::into_raw(NSString::from_str(s)) as *const c_void;
    // SAFETY: an element made and released here; the attribute names and values are retained
    // NSStrings and NSNumbers (toll-free bridged to the CF types taken), released after.
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return;
        }
        let (front, windows, raise) = (name("AXFrontmost"), name("AXWindows"), name("AXRaise"));
        let yes = Retained::into_raw(NSNumber::new_bool(true)) as *const c_void;
        AXUIElementSetAttributeValue(app, front, yes);
        let mut list: *const c_void = std::ptr::null();
        if let Some(id) = id
            && AXUIElementCopyAttributeValue(app, windows, &mut list) == 0
            && !list.is_null()
        {
            let list = &*(list as *const NSArray<AnyObject>);
            for w in list.iter() {
                let mut n = 0u32;
                let w = Retained::as_ptr(&w) as *const c_void;
                if _AXUIElementGetWindow(w, &mut n) == 0 && n == id {
                    AXUIElementPerformAction(w, raise);
                    break;
                }
            }
            CFRelease(list as *const NSArray<AnyObject> as *const c_void);
        }
        for p in [front, windows, raise, yes] {
            CFRelease(p);
        }
        CFRelease(app);
    }
}

/// A window as the window server lists it: its number, its program's process and name, its
/// title, its layer (0 for ordinary windows) and where it is, in points.
#[derive(Clone, Debug)]
struct Win {
    id: u32,
    pid: i32,
    app: String,
    title: String,
    layer: i64,
    rect: CGRect,
}

impl Win {
    fn contains(&self, p: CGPoint) -> bool {
        let r = &self.rect;
        p.x >= r.origin.x && p.y >= r.origin.y && p.x < r.origin.x + r.size.width && p.y < r.origin.y + r.size.height
    }
}

/// The windows on screen, front to back; or only window `only`, wherever it is.
fn listed(only: Option<u32>) -> Vec<Win> {
    let option = match only {
        Some(_) => CGWindowListOption::OptionIncludingWindow,
        None => CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
    };
    let Some(list) = CGWindowListCopyWindowInfo(option, only.unwrap_or(0)) else { return Vec::new() };
    // SAFETY: an array of dictionaries, toll-free bridged to Foundation's, read while it lives.
    let list = unsafe { &*(CFRetained::as_ptr(&list).as_ptr() as *const NSArray<NSDictionary<NSString, AnyObject>>) };
    let mut out = Vec::new();
    for d in list.iter() {
        let get = |k: &str| d.objectForKey(&NSString::from_str(k));
        let num = |k: &str| get(k).and_then(|o| o.downcast::<NSNumber>().ok());
        let text = |k: &str| get(k).and_then(|o| o.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default();
        let (Some(id), Some(pid), Some(bounds)) = (num("kCGWindowNumber"), num("kCGWindowOwnerPID"), get("kCGWindowBounds")) else {
            continue;
        };
        let mut rect = CGRect::default();
        // SAFETY: the bounds are a dictionary, read here only.
        if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(&*(Retained::as_ptr(&bounds) as *const CFDictionary)), &mut rect) }
            || num("kCGWindowAlpha").is_some_and(|a| a.doubleValue() <= 0.0)
        {
            continue;
        }
        let layer = num("kCGWindowLayer").map_or(0, |l| l.longLongValue());
        out.push(Win { id: id.unsignedIntValue(), pid: pid.intValue(), app: text("kCGWindowOwnerName"), title: text("kCGWindowName"), layer, rect });
    }
    out
}

/// What is shown: the whole display, one window with its program's menus and dialogs over it
/// (cut to the window), or every window of one program.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Scope {
    Screen,
    Window { id: u32, pid: i32 },
    App { pid: i32 },
}

impl Scope {
    fn pid(self) -> Option<i32> {
        match self {
            Scope::Screen => None,
            Scope::Window { pid, .. } | Scope::App { pid } => Some(pid),
        }
    }
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
    /// What ScreenCaptureKit lists; none at the login window, where it may list nothing.
    content: Option<Retained<SCShareableContent>>,
    displays: Vec<Display>,
    current: usize,
    /// What is shown, and where: the display, or the window's part of it.
    scope: Scope,
    area: Display,
    /// When the shared window's place was last looked at; whether it closed since.
    looked: Instant,
    ended: bool,
    /// The windows on screen as last listed (for where clicks land), and when.
    seen: (Instant, Vec<Win>),
    target: (usize, usize),
    fps: u8,
    live: Option<Live>,
    drawn: Option<Drawn>,
    latest: Arc<Latest>,
    hands: Hands,
    /// When the stream was last given a new size: pictures of the old one still come a while.
    resized: Option<Instant>,
}

impl Mac {
    pub fn open() -> Result<Mac, String> {
        if !CGPreflightScreenCaptureAccess() {
            // The system's question (once; then System Settings has it).
            CGRequestScreenCaptureAccess();
        }
        // Just after start-up ScreenCaptureKit lists nothing at the login window: Core Graphics'
        // own list there, as its pictures are taken by Core Graphics too.
        let content = if before_sign_in() { None } else { Some(shareable()?) };
        let main = CGMainDisplayID();
        let listed: Vec<(u32, CGRect)> = match &content {
            // SAFETY: reading the displays the content lists, with their plain getters.
            Some(c) => unsafe { c.displays() }.iter().map(|d| unsafe { (d.displayID(), d.frame()) }).collect(),
            None => {
                let (mut ids, mut n) = ([0u32; 16], 0u32);
                // SAFETY: room for 16 displays; their count is written back.
                unsafe { objc2_core_graphics::CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut n) };
                ids[..(n as usize).min(16)].iter().map(|&id| (id, CGDisplayBounds(id))).collect()
            }
        };
        let mut displays: Vec<Display> = listed
            .into_iter()
            .map(|(id, frame)| {
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
        let area = displays[0].clone();
        let latest = Arc::new(Latest { frame: Mutex::new(None), fresh: AtomicBool::new(false), wake: Default::default() });
        let mut mac = Mac {
            content,
            displays,
            current: 0,
            scope: Scope::Screen,
            area,
            looked: Instant::now(),
            ended: false,
            seen: (Instant::now(), Vec::new()),
            target,
            fps: 30,
            live: None,
            drawn: None,
            latest,
            hands: Hands::default(),
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
        unsafe { self.content.as_ref()?.displays() }.iter().find(|d| unsafe { d.displayID() } == id)
    }

    /// The pictures' size: the viewer's, never more than what is shown has pixels.
    fn size(&self) -> (usize, usize) {
        let (w, h) = self.area.pixels;
        (self.target.0.clamp(2, w.max(2)), self.target.1.clamp(2, h.max(2)))
    }

    /// The window's part of the display, in points from the display's corner.
    fn part(&self) -> Option<CGRect> {
        let (d, a) = (self.display(), &self.area);
        matches!(self.scope, Scope::Window { .. })
            .then(|| CGRect::new(CGPoint::new(a.at.0 - d.at.0, a.at.1 - d.at.1), CGSize::new(a.points.0, a.points.1)))
    }

    fn restart(&mut self) -> Result<(), String> {
        self.live = None;
        self.drawn = None;
        if before_sign_in() && self.scope == Scope::Screen {
            self.drawn = Some(draw(self.display().id, self.size(), self.fps, self.latest.clone())?);
            return Ok(());
        }
        let d = self.sc_display().ok_or("that display is gone")?;
        // SAFETY: a filter made from the display and program the content lists.
        let filter = unsafe {
            match self.scope.pid() {
                None => SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &d, &NSArray::new()),
                Some(pid) => {
                    let app: Retained<SCRunningApplication> =
                        self.content.as_ref().ok_or("no program can be shown alone here")?.applications().iter().find(|a| a.processID() == pid).ok_or("that program is gone")?;
                    SCContentFilter::initWithDisplay_includingApplications_exceptingWindows(
                        SCContentFilter::alloc(),
                        &d,
                        &NSArray::from_retained_slice(&[app]),
                        &NSArray::<SCWindow>::new(),
                    )
                }
            }
        };
        self.live = Some(start(&filter, &config(self.size(), self.fps, self.part()), self.latest.clone())?);
        Ok(())
    }

    /// New settings for the running stream (its size, its rate, the window's part).
    fn reconfigure(&mut self) {
        self.resized = Some(Instant::now());
        // The login window's pictures are taken at the size they started with: started again.
        if self.drawn.is_some() {
            if let Err(e) = self.restart() {
                eprintln!("sshremote: {e}");
            }
            return;
        }
        if let Some(l) = &self.live {
            // SAFETY: a fresh configuration given to the running stream.
            unsafe { l.stream.updateConfiguration_completionHandler(&config(self.size(), self.fps, self.part()), None) };
        }
    }

    /// The part of its display a window covers, as a display of its own (points and pixels);
    /// None when it is on none of them.
    fn area_of(&self, rect: CGRect) -> Option<(usize, Display)> {
        let (cx, cy) = (rect.origin.x + rect.size.width / 2.0, rect.origin.y + rect.size.height / 2.0);
        let inside = |d: &Display| cx >= d.at.0 && cy >= d.at.1 && cx < d.at.0 + d.points.0 && cy < d.at.1 + d.points.1;
        let i = self.displays.iter().position(inside)?;
        let d = &self.displays[i];
        let (x0, y0) = (rect.origin.x.max(d.at.0), rect.origin.y.max(d.at.1));
        let (x1, y1) = ((rect.origin.x + rect.size.width).min(d.at.0 + d.points.0), (rect.origin.y + rect.size.height).min(d.at.1 + d.points.1));
        let (kx, ky) = (d.pixels.0 as f64 / d.points.0, d.pixels.1 as f64 / d.points.1);
        let points = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
        let pixels = (((points.0 * kx).round() as usize).max(2), ((points.1 * ky).round() as usize).max(2));
        Some((i, Display { id: d.id, at: (x0, y0), points, pixels }))
    }

    /// Shows something else; the whole screen again if that fails.
    fn show(&mut self, scope: Scope, current: usize, area: Display) -> Result<(), String> {
        (self.scope, self.current, self.area) = (scope, current, area);
        (self.resized, self.hands.inside) = (Some(Instant::now()), true);
        if let Err(e) = self.restart() {
            (self.scope, self.area) = (Scope::Screen, self.display().clone());
            let _ = self.restart();
            return Err(e);
        }
        Ok(())
    }

    /// Follows the shared window where it moves or as it is resized, and the whole screen again
    /// once it or its program is gone.
    fn follow(&mut self) {
        if self.scope == Scope::Screen || self.looked.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.looked = Instant::now();
        let gone = match self.scope {
            Scope::Window { id, .. } => match listed(Some(id)).into_iter().find(|w| w.id == id).and_then(|w| self.area_of(w.rect)) {
                Some((i, area)) => {
                    let a = &self.area;
                    let moved = (a.at.0 - area.at.0).abs() + (a.at.1 - area.at.1).abs() + (a.points.0 - area.points.0).abs() + (a.points.1 - area.points.1).abs() > 0.5;
                    if i != self.current {
                        let _ = self.show(self.scope, i, area);
                    } else if moved {
                        self.area = area;
                        self.reconfigure();
                    }
                    false
                }
                None => true,
            },
            // SAFETY: asking whether a process is there, sending it nothing.
            Scope::App { pid } => unsafe { libc::kill(pid, 0) != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) },
            Scope::Screen => false,
        };
        if gone {
            let d = self.display().clone();
            let _ = self.show(Scope::Screen, self.current, d);
            self.ended = true;
        }
    }

    /// The windows on screen, listed again when the last list is older than a tenth of a second.
    fn windows_now(&mut self) -> &[Win] {
        if self.seen.0.elapsed() > Duration::from_millis(100) || self.seen.1.is_empty() {
            self.seen = (Instant::now(), listed(None));
        }
        &self.seen.1
    }

    /// Whether a point (in points) lands on the shared program's windows, not on another's over them.
    fn lands_inside(&mut self, p: CGPoint) -> bool {
        let Some(pid) = self.scope.pid() else { return true };
        // The window server's own (shadows, overlays) take no clicks.
        self.windows_now().iter().find(|w| w.contains(p) && w.app != "Window Server").is_some_and(|w| w.pid == pid)
    }

    /// Whether the keys typed now go to the shared program: it is the one in front.
    fn keys_go_inside(&mut self) -> bool {
        let Some(pid) = self.scope.pid() else { return true };
        self.windows_now().iter().find(|w| w.layer == 0).is_some_and(|w| w.pid == pid)
    }
}

/// Input as Core Graphics events: where the pointer was sent (points) and the buttons held there,
/// the last click (to count double and triple clicks as macOS does), the modifier keys held
/// (keysyms, for the flags of the keys typed meanwhile), and whether Accessibility was asked
/// for and the viewer told.
#[derive(Default)]
struct Hands {
    at: CGPoint,
    held: [bool; 5],
    last_click: Option<(Instant, CGPoint, u8, i64)>,
    mods: Vec<u32>,
    asked: bool,
    notice: Option<String>,
    /// Whether the pointer is over the shared windows (while some are shared).
    inside: bool,
    source: Option<CFRetained<CGEventSource>>,
}

/// Whether this runs at the login window (as root, started there by its launch agent): events
/// posted there the usual way are dropped or hang, while the old calls are taken.
fn before_sign_in() -> bool {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() == 0 }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    // The old ways to post the pointer, its buttons and the wheel (taken at the login window).
    fn CGPostMouseEvent(at: CGPoint, update: libc::boolean_t, buttons: u32, first: libc::boolean_t, ...) -> i32;
    fn CGPostScrollWheelEvent(wheels: u32, first: i32, ...) -> i32;
}

impl Display {
    /// The main display as Core Graphics measures it.
    #[cfg_attr(not(test), allow(dead_code))]
    fn main() -> Display {
        let id = CGMainDisplayID();
        let b = CGDisplayBounds(id);
        let mode = CGDisplayCopyDisplayMode(id);
        let pixels = (CGDisplayMode::pixel_width(mode.as_deref()), CGDisplayMode::pixel_height(mode.as_deref()));
        let points = (b.size.width, b.size.height);
        let pixels = if pixels.0 == 0 { (points.0 as usize, points.1 as usize) } else { pixels };
        Display { id, at: (b.origin.x, b.origin.y), points, pixels }
    }

    /// Its pixels to the point events go to.
    fn point(&self, x: f64, y: f64) -> CGPoint {
        let (kx, ky) = (self.points.0 / self.pixels.0 as f64, self.points.1 / self.pixels.1 as f64);
        CGPoint::new(self.at.0 + x.clamp(0.0, self.pixels.0 as f64 - 1.0) * kx, self.at.1 + y.clamp(0.0, self.pixels.1 as f64 - 1.0) * ky)
    }
}

impl Hands {
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

    /// The one source the events come from (as the keyboard and mouse here: what they post is
    /// held as theirs), made once rather than for every event.
    fn source(&mut self) -> Option<CFRetained<CGEventSource>> {
        if self.source.is_none() {
            self.source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
        }
        self.source.clone()
    }

    /// The pointer and its buttons as they are now, the old way.
    fn old_mouse(&mut self) -> Result<(), String> {
        let b = |held: bool| held as libc::boolean_t;
        // SAFETY: three buttons' states follow, as said: left, right, middle.
        unsafe { CGPostMouseEvent(self.at, 1, 3, b(self.held[0]), b(self.held[2]), b(self.held[1])) };
        Ok(())
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
        let src = self.source();
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
        if before_sign_in() {
            let _ = objc2_core_graphics::CGWarpMouseCursorPosition(at);
            return self.old_mouse();
        }
        let src = self.source();
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
        if before_sign_in() {
            return self.old_mouse();
        }
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
        if before_sign_in() {
            // SAFETY: two wheels' steps follow, as said.
            unsafe { CGPostScrollWheelEvent(2, -vertical, -horizontal) };
            return Ok(());
        }
        let src = self.source();
        // Up and left are positive here; the viewer's steps go down and right.
        let e = CGEvent::new_scroll_wheel_event2(src.as_deref(), CGScrollEventUnit::Line, 2, -vertical, -horizontal, 0);
        self.post(e)
    }

    fn key(&mut self, code: u16, down: bool, flags: CGEventFlags) -> Result<(), String> {
        if before_sign_in() {
            // The old call takes no flags: modifiers asked for and not held go down around the key.
            let held = self.flags();
            let extra: Vec<u16> = [(CGEventFlags::MaskShift, 0x38), (CGEventFlags::MaskControl, 0x3b), (CGEventFlags::MaskAlternate, 0x3a), (CGEventFlags::MaskCommand, 0x37)]
                .into_iter()
                .filter(|&(f, _)| flags.contains(f) && !held.contains(f))
                .map(|(_, c)| c)
                .collect();
            #[allow(deprecated)]
            {
                if down {
                    extra.iter().for_each(|&c| _ = objc2_core_graphics::CGPostKeyboardEvent(0, c, true));
                }
                let _ = objc2_core_graphics::CGPostKeyboardEvent(0, code, down);
                if !down {
                    extra.iter().rev().for_each(|&c| _ = objc2_core_graphics::CGPostKeyboardEvent(0, c, false));
                }
            }
            return Ok(());
        }
        let src = self.source();
        let e = CGEvent::new_keyboard_event(src.as_deref(), code, down);
        if let Some(e) = &e {
            CGEvent::set_flags(Some(e), flags);
        }
        self.post(e)
    }

    /// Text where no key types it: one key event carrying it, as input methods send theirs.
    fn text(&mut self, text: &str) -> Result<(), String> {
        // The old calls carry key codes only.
        if before_sign_in() {
            return Ok(());
        }
        let units: Vec<u16> = text.encode_utf16().collect();
        for chunk in units.chunks(16) {
            for down in [true, false] {
                let src = self.source();
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

    /// One piece of input on display `d`.
    fn input(&mut self, d: &Display, ev: Input) -> Result<(), String> {
        match ev {
            Input::Move { x, y } => {
                let at = d.point(x, y);
                self.moved(at, None)
            }
            Input::MoveBy { dx, dy } => {
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
        let d = self.displays[index].clone();
        if let Err(e) = self.show(Scope::Screen, index, d) {
            self.current = was;
            self.area = self.display().clone();
            let _ = self.restart();
            return Err(e);
        }
        Ok(())
    }

    fn desktop_size(&self) -> (usize, usize) {
        self.area.pixels
    }

    fn origin(&self) -> (i32, i32) {
        let a = &self.area;
        ((a.at.0 * a.pixels.0 as f64 / a.points.0) as i32, (a.at.1 * a.pixels.1 as f64 / a.points.1) as i32)
    }

    fn windows(&self) -> Result<Vec<WindowInfo>, String> {
        if !metal() {
            return Err(NO_METAL.into());
        }
        let me = std::process::id() as i32;
        let d = self.display();
        let k = d.pixels.0 as f64 / d.points.0;
        Ok(listed(None)
            .into_iter()
            .filter(|w| w.layer == 0 && w.pid != me && !w.app.is_empty() && w.rect.size.width >= 40.0 && w.rect.size.height >= 40.0)
            .map(|w| WindowInfo {
                id: w.id as u64,
                title: if w.title.is_empty() { w.app.clone() } else { w.title },
                app: w.app,
                w: (w.rect.size.width * k).round().min(u16::MAX as f64) as u16,
                h: (w.rect.size.height * k).round().min(u16::MAX as f64) as u16,
            })
            .collect())
    }

    fn share(&mut self, scope: Share) -> Result<String, String> {
        let win = match scope {
            Share::Screen => {
                let d = self.display().clone();
                self.show(Scope::Screen, self.current, d)?;
                return Ok("the whole screen".into());
            }
            _ if !metal() => return Err(NO_METAL.into()),
            Share::Window(id) | Share::App(id) => listed(Some(id as u32)).into_iter().find(|w| w.id as u64 == id),
            Share::ActiveWindow | Share::ActiveApp => {
                let me = std::process::id() as i32;
                listed(None).into_iter().find(|w| w.layer == 0 && w.pid != me)
            }
        };
        let win = win.ok_or("that window is gone")?;
        // Programs started since are listed too.
        self.content = Some(shareable()?);
        let (i, area) = self.area_of(win.rect).ok_or("that window is on no display")?;
        let (next, text) = if matches!(scope, Share::App(_) | Share::ActiveApp) {
            let d = self.displays[i].clone();
            ((Scope::App { pid: win.pid }, d), format!("every window of {}", win.app))
        } else {
            let title = if win.title.is_empty() { &win.app } else { &win.title };
            ((Scope::Window { id: win.id, pid: win.pid }, area), format!("the window \"{title}\""))
        };
        self.show(next.0, i, next.1)?;
        self.ended = false;
        raise(win.pid, matches!(next.0, Scope::Window { .. }).then_some(win.id));
        Ok(text)
    }

    fn poll_share(&mut self) -> Option<Result<(u8, String), String>> {
        std::mem::take(&mut self.ended).then(|| Ok((0, "the whole screen, as the shared window closed".into())))
    }

    fn shared_window(&self) -> Option<u64> {
        match self.scope {
            Scope::Window { id, .. } => Some(id as u64),
            _ => None,
        }
    }

    fn shared_app(&self) -> Option<String> {
        let pid = self.scope.pid()?;
        // SAFETY: reading the listed programs.
        unsafe { self.content.as_ref()?.applications() }.iter().find(|a| unsafe { a.processID() } == pid).map(|a| unsafe { a.applicationName() }.to_string())
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
        self.hands.notice.take()
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        self.follow();
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
        let a = self.area.clone();
        if self.scope != Scope::Screen {
            // Only the shared windows take clicks and keys.
            match &ev {
                Input::Move { x, y } => self.hands.inside = self.lands_inside(a.point(*x, *y)),
                Input::Button { pressed: true, .. } if !self.hands.inside => return Err(super::OUTSIDE.into()),
                Input::MoveBy { .. } => return Err(super::WHOLE_SCREEN_MOUSE.into()),
                Input::Scroll { .. } | Input::HScroll { .. } if !self.hands.inside => return Ok(()),
                Input::Key { .. } | Input::KeyState { down: true, .. } if !self.keys_go_inside() => {
                    return Err("keys go only to the shared window; click it first".into());
                }
                _ => {}
            }
        }
        self.hands.input(&a, ev)
    }

    fn commit_text(&mut self, text: &str) -> bool {
        self.hands.text(text).is_ok()
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
        let want = m.display().point(w as f64 / 2.0, h as f64 / 2.0);
        let at = here();
        println!("{w}x{h} shown as {}x{}, notice {:?}; pointer at {:.0},{:.0} (sent {:.0},{:.0})", img.w, img.h, m.notice(), at.x, at.y, want.x, want.y);
        assert!((at.x - want.x).abs() < 2.0 && (at.y - want.y).abs() < 2.0, "the pointer went where it was sent");
        m.input(Input::MoveBy { dx: 20, dy: -10 }).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let by = here();
        assert!(by.x > at.x && by.y < at.y, "moved by: {:.0},{:.0}", by.x, by.y);
    }

    #[test]
    #[ignore]
    fn a_mac_sound_starts() {
        let audio = |c: Retained<SCStreamConfiguration>, on: bool| {
            // SAFETY: setting a fresh configuration.
            unsafe { c.setCapturesAudio(on) };
            c
        };
        let tries: Vec<(&str, Retained<SCStreamConfiguration>, bool)> = vec![
            ("pictures 640x360 30/s", audio(config((640, 360), 30, None), false), true),
            ("pictures 64x64 1/s", audio(config((64, 64), 1, None), false), true),
            ("pictures 2x2 1/s", audio(config((2, 2), 1, None), false), true),
            ("sound, pictures 640x360 30/s", audio(config((640, 360), 30, None), true), true),
            ("sound alone 640x360", audio(config((640, 360), 30, None), true), false),
        ];
        for (name, c, seen) in tries {
            let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let m = n.clone();
            let r = listen(Box::new(move |_, _, _| _ = m.fetch_add(1, Ordering::Relaxed)), &c, seen);
            std::thread::sleep(Duration::from_secs(2));
            println!("{name}: {:?}, sound buffers {}", r.as_ref().map(|_| ()), n.load(Ordering::Relaxed));
        }
        let r = sound(Box::new(|_, _, _| {}));
        println!("sound(): {:?}", r.as_ref().map(|_| ()));
    }

    /// What can be read of the screen and how input can be posted from here (run at the login
    /// window by a LoginWindow launch agent, as the app): each way in a process of its own, as
    /// some hang there for good (making a mouse event with an event source did).
    #[test]
    #[ignore]
    fn the_screen_as_seen_from_here() {
        if let Ok(way) = std::env::var("SSHREMOTE_MAC_WAY") {
            return way_from_here(&way);
        }
        println!(
            "uid {}, screen recording allowed {}, accessibility {}, event posting {}, metal {}",
            unsafe { libc::getuid() },
            CGPreflightScreenCaptureAccess(),
            may_post(&mut true),
            objc2_core_graphics::CGPreflightPostEventAccess(),
            metal()
        );
        for way in [
            "list", "capture", "image", "windows", "warp", "move", "move-legacy", "type-legacy",
        ] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "--nocapture", "capture::mac::tests::the_screen_as_seen_from_here"])
                .env("SSHREMOTE_MAC_WAY", way)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let until = Instant::now() + Duration::from_secs(20);
            let done = loop {
                if child.try_wait().unwrap().is_some() {
                    break true;
                }
                if Instant::now() > until {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            let mut out = String::new();
            let _ = std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut out);
            let said: Vec<&str> = out.lines().filter_map(|l| l.strip_prefix("way: ")).collect();
            println!("{way}: {}{}", said.join("; "), if done { "" } else { " (hung, ended after 20 s)" });
        }
    }

    #[allow(deprecated)]
    fn way_from_here(way: &str) {
        let here = || CGEvent::new(None).map(|e| CGEvent::location(Some(&e))).unwrap_or(CGPoint { x: -1.0, y: -1.0 });
        match way {
            "list" => match shareable() {
                Ok(c) => println!("way: {} displays, {} windows", unsafe { c.displays() }.len(), unsafe { c.windows() }.len()),
                Err(e) => println!("way: {e}"),
            },
            "capture" => match Mac::open() {
                Ok(mut m) => {
                    let until = Instant::now() + Duration::from_secs(5);
                    while Instant::now() < until {
                        match m.grab() {
                            Ok(Some(img)) => {
                                let lit = img.data.iter().filter(|&&b| b > 16).count();
                                return println!("way: picture {}x{}, {lit} bytes above black", img.w, img.h);
                            }
                            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                            Err(e) => return println!("way: {e}"),
                        }
                    }
                    println!("way: no picture in 5 s");
                }
                Err(e) => println!("way: {e}"),
            },
            "image" => match objc2_core_graphics::CGDisplayCreateImage(CGMainDisplayID()) {
                Some(img) => {
                    let (w, h) = (objc2_core_graphics::CGImage::width(Some(&img)), objc2_core_graphics::CGImage::height(Some(&img)));
                    let data = objc2_core_graphics::CGImage::data_provider(Some(&img)).and_then(|p| objc2_core_graphics::CGDataProvider::data(Some(&p)));
                    let lit = data.map(|d| d.to_vec().iter().filter(|&&b| b > 16).count()).unwrap_or(0);
                    println!("way: {w}x{h}, {lit} bytes above black");
                }
                None => println!("way: none"),
            },
            "windows" => {
                let windows = listed(None);
                println!("way: {} ({:?})", windows.len(), windows.iter().take(6).map(|w| format!("{}:{}", w.app, w.layer)).collect::<Vec<_>>());
            }
            "move" => {
                let was = here();
                let to = CGPoint { x: was.x - 40.0, y: was.y + 30.0 };
                let e = CGEvent::new_mouse_event(None, CGEventType::MouseMoved, to, CGMouseButton::Left);
                CGEvent::post(CGEventTapLocation::HIDEventTap, e.as_deref());
                std::thread::sleep(Duration::from_millis(300));
                let at = here();
                let back = CGEvent::new_mouse_event(None, CGEventType::MouseMoved, was, CGMouseButton::Left);
                CGEvent::post(CGEventTapLocation::HIDEventTap, back.as_deref());
                println!("way: from {:.0},{:.0} sent to {:.0},{:.0}, at {:.0},{:.0}", was.x, was.y, to.x, to.y, at.x, at.y);
            }
            "move-legacy" => {
                let was = here();
                let to = CGPoint { x: was.x - 40.0, y: was.y + 30.0 };
                // SAFETY: one button's state follows, as said.
                let r = unsafe { CGPostMouseEvent(to, 1, 1, 0) };
                std::thread::sleep(Duration::from_millis(300));
                let at = here();
                let _ = unsafe { CGPostMouseEvent(was, 1, 1, 0) };
                println!("way: {r}, from {:.0},{:.0} sent to {:.0},{:.0}, at {:.0},{:.0}", was.x, was.y, to.x, to.y, at.x, at.y);
            }
            "warp" => {
                let was = here();
                let to = CGPoint { x: was.x - 40.0, y: was.y + 30.0 };
                let r = objc2_core_graphics::CGWarpMouseCursorPosition(to);
                std::thread::sleep(Duration::from_millis(300));
                let at = here();
                let _ = objc2_core_graphics::CGWarpMouseCursorPosition(was);
                println!("way: {r:?}, from {:.0},{:.0} sent to {:.0},{:.0}, at {:.0},{:.0}", was.x, was.y, to.x, to.y, at.x, at.y);
            }
            // An x typed where the keys go and taken back, seen on the screen: what is shown then
            // differs from every picture before (the caret blinks), and no longer once it is gone.
            "type-legacy" => {
                let shot = || {
                    let img = objc2_core_graphics::CGDisplayCreateImage(CGMainDisplayID());
                    let data = img.and_then(|i| objc2_core_graphics::CGImage::data_provider(Some(&i))).and_then(|p| objc2_core_graphics::CGDataProvider::data(Some(&p)));
                    data.map(|d| d.to_vec()).unwrap_or_default()
                };
                let differ = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x.abs_diff(**y) > 24).count();
                let key = |code: u16| {
                    for down in [true, false] {
                        let _ = objc2_core_graphics::CGPostKeyboardEvent(0, code, down);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    std::thread::sleep(Duration::from_millis(400));
                };
                let before: Vec<Vec<u8>> = (0..4)
                    .map(|_| {
                        let s = shot();
                        std::thread::sleep(Duration::from_millis(150));
                        s
                    })
                    .collect();
                key(0x07);
                let typed = shot();
                key(0x33);
                let taken = shot();
                let least = |s: &[u8]| before.iter().map(|b| differ(b, s)).min().unwrap_or(0);
                println!("way: changed {} after x, {} after Delete (blinking alone {})", least(&typed), least(&taken), differ(&before[0], &before[2]));
            }
            _ => {}
        }
    }

    /// The pointer and the keys driven, nothing recorded: run where macOS lets this program
    /// control it (as the allowed app, or over SSH when the SSH server may: sshd-keygen-wrapper in
    /// Accessibility). Nothing is clicked or typed: the pointer moves and comes back, and Shift,
    /// which does nothing alone, is held and let go.
    /// SSHREMOTE_MAC_TEST=1 cargo test -- --ignored a_mac_is_driven
    #[test]
    #[ignore]
    fn a_mac_is_driven_by_its_events() {
        if std::env::var_os("SSHREMOTE_MAC_TEST").is_none() {
            return;
        }
        let d = Display::main();
        let mut h = Hands::default();
        let here = || CGEvent::new(None).map(|e| CGEvent::location(Some(&e))).expect("where the pointer is");
        let settle = || std::thread::sleep(Duration::from_millis(200));
        let was = here();
        let (x, y) = (d.pixels.0 as f64 / 3.0, d.pixels.1 as f64 / 3.0);
        h.input(&d, Input::Move { x, y }).unwrap();
        settle();
        assert!(h.notice.is_none(), "{:?}", h.notice);
        let (want, at) = (d.point(x, y), here());
        assert!((at.x - want.x).abs() < 2.0 && (at.y - want.y).abs() < 2.0, "sent to {:.0},{:.0}, at {:.0},{:.0}", want.x, want.y, at.x, at.y);
        h.input(&d, Input::MoveBy { dx: 30, dy: -20 }).unwrap();
        settle();
        let by = here();
        assert!(by.x > at.x + 5.0 && by.y < at.y - 5.0, "moved by: {:.0},{:.0}", by.x, by.y);
        let shift = |down| CGEventSource::key_state(CGEventSourceStateID::HIDSystemState, 0x38) == down;
        h.input(&d, Input::KeyState { keysym: xk::SHIFT_L, down: true }).unwrap();
        settle();
        let held = shift(true) && CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState).contains(CGEventFlags::MaskShift);
        h.input(&d, Input::KeyState { keysym: xk::SHIFT_L, down: false }).unwrap();
        settle();
        let let_go = shift(false);
        // The pointer back where it was.
        let (kx, ky) = (d.pixels.0 as f64 / d.points.0, d.pixels.1 as f64 / d.points.1);
        h.input(&d, Input::Move { x: (was.x - d.at.0) * kx, y: (was.y - d.at.1) * ky }).unwrap();
        println!("pointer {:.0},{:.0} -> {:.0},{:.0} -> {:.0},{:.0}; Shift held {held}, let go {let_go}", was.x, was.y, at.x, at.y, by.x, by.y);
        assert!(held && let_go);
    }
}
