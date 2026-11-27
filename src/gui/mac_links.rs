//! macOS: `sshremote://` links clicked anywhere open in the app. The system hands a link to the
//! app as an Apple Event (GURL), which the window's toolkit passes over: a handler of our own
//! takes it, and the window opens what it names (`take`, each frame).

use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AllocAnyThread, define_class, msg_send, sel};
use objc2_foundation::NSString;

/// The links come in, until the window takes them.
static OPENED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WAKE: OnceLock<eframe::egui::Context> = OnceLock::new();

/// kInternetEventClass and kAEGetURL ('GURL'), and keyDirectObject ('----'), as FourCharCodes.
const GURL: u32 = u32::from_be_bytes(*b"GURL");
const DIRECT: u32 = u32::from_be_bytes(*b"----");

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and nothing here is unsafe to call.
    #[unsafe(super(NSObject))]
    #[name = "SshRemoteLinks"]
    struct Links;

    unsafe impl NSObjectProtocol for Links {}

    impl Links {
        #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
        fn handle(&self, event: &AnyObject, _reply: &AnyObject) {
            // SAFETY: an NSAppleEventDescriptor, asked for its direct object and that as text.
            let link: Option<Retained<AnyObject>> = unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT] };
            let text: Option<Retained<NSString>> = link.and_then(|d| unsafe { msg_send![&*d, stringValue] });
            let Some(url) = text.map(|t| t.to_string()).filter(|u| u.starts_with("sshremote://")) else { return };
            eprintln!("sshremote: opening {url}");
            OPENED.lock().unwrap_or_else(|e| e.into_inner()).push(url);
            if let Some(ctx) = WAKE.get() {
                ctx.request_repaint();
            }
        }
    }
);

/// Takes links from now on, before the window is up (the link that started the app comes as
/// soon as it starts); the handler stays for as long as the app runs.
pub fn listen() {
    // SAFETY: NSObject's init on a fresh instance; the shared Apple Event manager given an object
    // that answers the selector, for GURL events.
    unsafe {
        let links: Retained<Links> = msg_send![Links::alloc(), init];
        let manager: Retained<AnyObject> = msg_send![objc2::class!(NSAppleEventManager), sharedAppleEventManager];
        let _: () = msg_send![&*manager, setEventHandler: &*links, andSelector: sel!(handleGetURLEvent:withReplyEvent:), forEventClass: GURL, andEventID: GURL];
        std::mem::forget(links);
    }
}

/// The window to wake when one comes.
pub fn wake(ctx: &eframe::egui::Context) {
    let _ = WAKE.set(ctx.clone());
    ctx.request_repaint();
}

/// The links that came since the last look.
pub fn take() -> Vec<String> {
    std::mem::take(&mut *OPENED.lock().unwrap_or_else(|e| e.into_inner()))
}
