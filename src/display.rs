//! The resolution of the host's screen that is shown, set from the viewer (as the display mode,
//! not the picture's size) and set back as it was when the session ends. On Windows through the
//! display settings, without saving them (signing out puts the saved ones back too); in X11
//! sessions through RandR; under Wayland through the desktop: GNOME's display configuration, KDE
//! Plasma's kscreen-doctor, Sway's swaymsg, Hyprland's hyprctl and COSMIC's cosmic-randr; on
//! macOS through Core Graphics' display modes, for as long as this program runs.

/// A screen's resolutions, largest first, and the one it has.
#[derive(Debug, Clone, PartialEq)]
pub struct Modes {
    pub list: Vec<(u16, u16)>,
    pub current: (u16, u16),
}

/// Smaller than this is no desktop to work in.
#[cfg_attr(not(any(windows, target_os = "linux", target_os = "macos")), allow(dead_code))]
const SMALLEST: (u16, u16) = (640, 480);

/// The resolutions once each, largest first.
#[cfg_attr(not(any(windows, target_os = "linux", target_os = "macos")), allow(dead_code))]
fn tidy(mut list: Vec<(u16, u16)>) -> Vec<(u16, u16)> {
    list.retain(|&(w, h)| w >= SMALLEST.0 && h >= SMALLEST.1);
    list.sort_by_key(|&(w, h)| std::cmp::Reverse((w as u32 * h as u32, w)));
    list.dedup();
    list
}

/// How screens are set here.
enum Way {
    #[cfg(windows)]
    Windows,
    #[cfg(target_os = "linux")]
    RandR,
    #[cfg(target_os = "linux")]
    Mutter,
    #[cfg(target_os = "linux")]
    Sway(std::path::PathBuf),
    #[cfg(target_os = "linux")]
    Hypr(String),
    #[cfg(target_os = "linux")]
    KScreen,
    #[cfg(target_os = "linux")]
    Cosmic,
    #[cfg(target_os = "macos")]
    Mac,
}

/// None elsewhere: no screen is set there.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
impl Way {
    fn find(_backend: &str) -> Option<Way> {
        None
    }

    fn screens(&self) -> Vec<String> {
        match *self {}
    }

    fn modes(&self, _id: &str) -> Option<Modes> {
        match *self {}
    }

    fn set(&self, _id: &str, _w: u16, _h: u16) -> Result<(), String> {
        match *self {}
    }

    fn adds(&self) -> bool {
        match *self {}
    }

    fn add(&self, _w: u16, _h: u16) -> Result<Added, String> {
        match *self {}
    }

    fn remove(&self, _a: Added) -> Result<(), String> {
        match *self {}
    }
}

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
impl Way {
    /// For the capture backend named `backend`, where this desktop lets its screens be set.
    fn find(backend: &str) -> Option<Way> {
        match backend {
            #[cfg(windows)]
            "windows" => Some(Way::Windows),
            #[cfg(target_os = "linux")]
            "x11" => Some(Way::RandR),
            #[cfg(target_os = "linux")]
            "gnome" => Some(Way::Mutter),
            // Sway and Hyprland read by wlr-screencopy, the other Wayland desktops by the portal.
            #[cfg(target_os = "linux")]
            "wlroots" | "portal" | "portal-view" => linux::wayland(),
            #[cfg(target_os = "macos")]
            "mac" => Some(Way::Mac),
            _ => None,
        }
    }

    /// Every screen it knows, by name, that is on.
    fn screens(&self) -> Vec<String> {
        match self {
            #[cfg(windows)]
            Way::Windows => Vec::new(),
            #[cfg(target_os = "linux")]
            Way::RandR => linux::randr_screens(),
            #[cfg(target_os = "linux")]
            Way::Mutter => linux::mutter_screens(),
            #[cfg(target_os = "linux")]
            Way::Sway(sock) => linux::sway_outputs(sock).into_iter().map(|o| o.name).collect(),
            #[cfg(target_os = "linux")]
            Way::Hypr(sig) => linux::hypr_monitors(sig).into_iter().map(|o| o.name).collect(),
            #[cfg(target_os = "linux")]
            Way::KScreen => linux::kscreen_outputs().into_iter().map(|o| o.name).collect(),
            #[cfg(target_os = "linux")]
            Way::Cosmic => linux::cosmic_outputs().into_iter().map(|o| o.name).collect(),
            #[cfg(target_os = "macos")]
            Way::Mac => mac::screens(),
        }
    }

    fn modes(&self, id: &str) -> Option<Modes> {
        let m = match self {
            #[cfg(windows)]
            Way::Windows => win::modes(id),
            #[cfg(target_os = "linux")]
            Way::RandR => linux::randr_modes(id),
            #[cfg(target_os = "linux")]
            Way::Mutter => linux::mutter_modes(id),
            #[cfg(target_os = "linux")]
            Way::Sway(sock) => linux::sway_outputs(sock).into_iter().find(|o| o.name == id).map(|o| o.modes()),
            #[cfg(target_os = "linux")]
            Way::Hypr(sig) => linux::hypr_monitors(sig).into_iter().find(|o| o.name == id && !linux::virtual_gpu(&o.name)).map(|o| o.modes()),
            #[cfg(target_os = "linux")]
            Way::KScreen => linux::kscreen_outputs().into_iter().find(|o| o.name == id).map(|o| o.modes()),
            #[cfg(target_os = "linux")]
            Way::Cosmic => linux::cosmic_outputs().into_iter().find(|o| o.name == id).map(|o| o.modes()),
            #[cfg(target_os = "macos")]
            Way::Mac => mac::modes(id),
        }?;
        Some(Modes { list: tidy(m.list), current: m.current })
    }

    fn set(&self, id: &str, w: u16, h: u16) -> Result<(), String> {
        match self {
            #[cfg(windows)]
            Way::Windows => win::set(id, w, h),
            #[cfg(target_os = "linux")]
            Way::RandR => linux::randr_set(id, w, h),
            #[cfg(target_os = "linux")]
            Way::Mutter => linux::mutter_set(id, w, h),
            #[cfg(target_os = "linux")]
            Way::Sway(sock) => linux::sway_set(sock, id, w, h),
            #[cfg(target_os = "linux")]
            Way::Hypr(sig) => linux::hypr_set(sig, id, w, h),
            #[cfg(target_os = "linux")]
            Way::KScreen => linux::kscreen_set(id, w, h),
            #[cfg(target_os = "linux")]
            Way::Cosmic => linux::cosmic_set(id, w, h),
            #[cfg(target_os = "macos")]
            Way::Mac => mac::set(id, w, h),
        }
    }

    /// Whether screens can be added here for a session.
    fn adds(&self) -> bool {
        match self {
            #[cfg(windows)]
            Way::Windows => amyuni::adds(),
            #[cfg(target_os = "linux")]
            Way::RandR | Way::Sway(_) | Way::Hypr(_) | Way::Mutter | Way::KScreen => true,
            #[cfg(target_os = "macos")]
            Way::Mac => mac::adds(),
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    /// Adds a screen of `w`x`h` right of the others.
    fn add(&self, w: u16, h: u16) -> Result<Added, String> {
        match self {
            #[cfg(windows)]
            Way::Windows => amyuni::add(w, h).map(Added::named),
            #[cfg(target_os = "linux")]
            Way::RandR => linux::randr_add(w, h).map(Added::named),
            #[cfg(target_os = "linux")]
            Way::Sway(sock) => linux::sway_add(sock, w, h).map(Added::named),
            #[cfg(target_os = "linux")]
            Way::Hypr(sig) => linux::hypr_add(sig, w, h).map(Added::named),
            #[cfg(target_os = "linux")]
            Way::Mutter => crate::capture::gnome_add_virtual(w, h).map(|(id, keep)| Added { id, _keep: Some(Box::new(keep)) }),
            #[cfg(target_os = "linux")]
            Way::KScreen => crate::capture::kwin_add(w, h).map(|(id, keep)| Added { id, _keep: Some(Box::new(keep)) }),
            #[cfg(target_os = "macos")]
            Way::Mac => mac::add(w, h).map(|(id, display)| Added { id, _display: Some(display) }),
            #[allow(unreachable_patterns)]
            _ => Err(NO_ADDING.into()),
        }
    }

    fn remove(&self, a: Added) -> Result<(), String> {
        match self {
            #[cfg(windows)]
            Way::Windows => amyuni::remove(&a.id),
            #[cfg(target_os = "linux")]
            Way::RandR => linux::randr_remove(&a.id),
            #[cfg(target_os = "linux")]
            Way::Sway(sock) => linux::sway_remove(sock, &a.id),
            #[cfg(target_os = "linux")]
            Way::Hypr(sig) => linux::hypr_remove(sig, &a.id),
            // The display goes with the object that made it.
            #[allow(unreachable_patterns)]
            _ => {
                drop(a);
                Ok(())
            }
        }
    }
}

const NO_ADDING: &str = "screens cannot be added to this computer's desktop from here";

/// The most screens one session adds.
const MOST_ADDED: usize = 4;

/// A screen a session added (a virtual one, as a dummy plug gives), taken away when it ends.
pub struct Added {
    /// Its system name, as the capture's `screen_ids` gives it.
    id: String,
    /// The display itself, where it lasts as long as the object that made it (macOS).
    #[cfg(target_os = "macos")]
    _display: Option<mac::Virtual>,
    /// GNOME's and KWin's, each lasting as long as what made it.
    #[cfg(target_os = "linux")]
    _keep: Option<Box<dyn Send>>,
}

impl Added {
    #[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
    fn named(id: String) -> Added {
        Added {
            id,
            #[cfg(target_os = "macos")]
            _display: None,
            #[cfg(target_os = "linux")]
            _keep: None,
        }
    }
}

/// The screens a session set, set back as they were when it ends, and those it added, taken away.
pub struct Session {
    way: Option<Way>,
    /// Each screen set, and the resolution it had before.
    own: Vec<(String, (u16, u16))>,
    added: Vec<Added>,
}

impl Session {
    pub fn new(backend: &str) -> Session {
        Session { way: Way::find(backend), own: Vec::new(), added: Vec::new() }
    }

    /// Whether screens can be added to this desktop (`add`).
    pub fn adds(&self) -> bool {
        self.way.as_ref().is_some_and(Way::adds)
    }

    /// Whether they could be, once the helper is installed here (Windows' driver comes with it).
    pub fn adds_with_helper(&self) -> bool {
        #[cfg(windows)]
        if matches!(self.way, Some(Way::Windows)) {
            return amyuni::without_driver();
        }
        false
    }

    /// Adds a screen of `w`x`h` for this session, right of the others; its system name, as the
    /// capture's `screen_ids` will give it once the capture hears of it.
    pub fn add(&mut self, w: u16, h: u16) -> Result<String, String> {
        let way = self.way.as_ref().filter(|w| w.adds()).ok_or(NO_ADDING)?;
        if self.added.len() >= MOST_ADDED {
            return Err(format!("{MOST_ADDED} screens added are the most"));
        }
        if !(640..=7680).contains(&w) || !(480..=4320).contains(&h) {
            return Err(format!("{w}x{h} is no size for a screen"));
        }
        let a = way.add(w, h)?;
        let id = a.id.clone();
        self.added.push(a);
        Ok(id)
    }

    /// Takes away screen `id`, one this session added.
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        let i = self.added.iter().position(|a| a.id == id).ok_or("only a screen added from here can be taken away")?;
        let a = self.added.remove(i);
        self.own.retain(|(s, _)| s != id);
        self.way.as_ref().ok_or(NO_ADDING)?.remove(a)
    }

    /// Whether screen `id` is one this session added.
    pub fn added(&self, id: &str) -> bool {
        self.added.iter().any(|a| a.id == id)
    }

    /// The screen shown: as the capture names it, else the only one there is.
    pub fn screen(&self, named: Option<String>) -> Option<String> {
        let way = self.way.as_ref()?;
        named.or_else(|| {
            let all = way.screens();
            if all.len() == 1 { all.into_iter().next() } else { None }
        })
    }

    /// Screen `id`'s resolutions and the one it has, and its own (before this session set it).
    pub fn modes(&self, id: &str) -> Option<(Modes, (u16, u16))> {
        let m = self.way.as_ref()?.modes(id)?;
        let own = self.own.iter().find(|(s, _)| s == id).map_or(m.current, |(_, o)| *o);
        Some((m, own))
    }

    /// What setting screen `id` to `w`x`h` (0x0: its own again) would change it to: none when it
    /// has that already, an error when it cannot have it. Nothing is changed.
    pub fn plan(&self, id: &str, w: u16, h: u16) -> Result<Option<(u16, u16)>, String> {
        let way = self.way.as_ref().ok_or("the resolution of this computer's screen cannot be set from here")?;
        let m = way.modes(id).ok_or("that screen's resolutions are not known")?;
        let own = self.own.iter().find(|(s, _)| s == id).map(|(_, o)| *o);
        let (w, h) = if (w, h) == (0, 0) { own.unwrap_or(m.current) } else { (w, h) };
        if (w, h) == m.current {
            return Ok(None);
        }
        if !m.list.contains(&(w, h)) && Some((w, h)) != own {
            return Err(format!("that screen has no {w}x{h}"));
        }
        Ok(Some((w, h)))
    }

    /// Sets screen `id` to `w`x`h`, one of its resolutions (0x0: its own again).
    pub fn set(&mut self, id: &str, w: u16, h: u16) -> Result<(), String> {
        let Some((w, h)) = self.plan(id, w, h)? else { return Ok(()) };
        let way = self.way.as_ref().ok_or("the resolution of this computer's screen cannot be set from here")?;
        let m = way.modes(id).ok_or("that screen's resolutions are not known")?;
        let own = self.own.iter().find(|(s, _)| s == id).map(|(_, o)| *o);
        way.set(id, w, h)?;
        // A screen may say yes and keep its size (one in a window of another desktop): what it
        // has a moment later is what counts.
        let now = (0..10).find_map(|i| {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            way.modes(id).map(|m| m.current).filter(|c| *c == (w, h))
        });
        if now.is_none() {
            // Asked back, so nothing is left waiting to take it later.
            let _ = way.set(id, m.current.0, m.current.1);
            return Err(format!("the screen kept its resolution; {w}x{h} was not taken"));
        }
        if own.is_none() {
            self.own.push((id.to_string(), m.current));
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let Some(way) = self.way.as_ref() else { return };
        for (id, (w, h)) in self.own.drain(..) {
            if way.modes(&id).is_some_and(|m| m.current != (w, h)) {
                let _ = way.set(&id, w, h);
            }
        }
        for a in self.added.drain(..).rev() {
            let _ = way.remove(a);
        }
    }
}

/// A Mac's displays by their Core Graphics number, their modes by the size they give the desktop
/// (in points, as System Settings lists them: a Retina display's pixels are twice as many).
#[cfg(target_os = "macos")]
mod mac {
    use super::Modes;
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, Type};
    use objc2_core_graphics::{
        CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration, CGConfigureDisplayWithDisplayMode, CGConfigureOption, CGDisplayConfigRef,
        CGDisplayCopyAllDisplayModes, CGDisplayCopyDisplayMode, CGDisplayMode, CGError, CGGetActiveDisplayList, kCGDisplayShowDuplicateLowResolutionModes,
    };

    /// A display added for a session (Core Graphics' virtual display, which DeskPad and
    /// BetterDisplay make too, through classes macOS does not document): it lasts as long as this.
    pub struct Virtual {
        _display: objc2::rc::Retained<objc2::runtime::AnyObject>,
        _queue: dispatch2::DispatchRetained<dispatch2::DispatchQueue>,
    }

    // SAFETY: only kept and let go; macOS serves the display on its own queue.
    unsafe impl Send for Virtual {}

    fn class(name: &std::ffi::CStr) -> Option<&'static objc2::runtime::AnyClass> {
        objc2::runtime::AnyClass::get(name)
    }

    /// Where macOS has the classes, and a Metal GPU: without one (a virtual machine's display)
    /// WindowServer draws such a display in software and dies in it, ending the session.
    pub fn adds() -> bool {
        static METAL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        #[link(name = "Metal", kind = "framework")]
        unsafe extern "C" {
            fn MTLCreateSystemDefaultDevice() -> *mut objc2::runtime::AnyObject;
        }
        class(c"CGVirtualDisplay").is_some()
            && *METAL.get_or_init(|| {
                // SAFETY: a plain call; the device it makes is let go at once.
                unsafe { objc2::rc::Retained::from_raw(MTLCreateSystemDefaultDevice()) }.is_some()
            })
    }

    /// Adds a display of `w`x`h`: its Core Graphics number, and what keeps it.
    pub fn add(w: u16, h: u16) -> Result<(String, Virtual), String> {
        use objc2::msg_send;
        use objc2::rc::{Allocated, Retained};
        use objc2::runtime::{AnyObject, Bool};
        use objc2_core_foundation::CGSize;
        static SERIAL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        let no = "this macOS cannot add displays";
        let (descriptor, display, settings, mode) =
            (class(c"CGVirtualDisplayDescriptor").ok_or(no)?, class(c"CGVirtualDisplay").ok_or(no)?, class(c"CGVirtualDisplaySettings").ok_or(no)?, class(c"CGVirtualDisplayMode").ok_or(no)?);
        let serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let queue = dispatch2::DispatchQueue::new("dev.sshremote.display", None);
        // SAFETY: the classes' own methods with the types they take (as DeskPad calls them); every
        // object made here is kept until it is no longer needed.
        unsafe {
            let desc: Retained<AnyObject> = msg_send![descriptor, new];
            let q: *mut AnyObject = (&*queue as *const dispatch2::DispatchQueue).cast_mut().cast();
            let _: () = msg_send![&*desc, setQueue: q];
            let name = objc2_foundation::NSString::from_str("SSH Remote");
            let _: () = msg_send![&*desc, setName: &*name];
            let _: () = msg_send![&*desc, setMaxPixelsWide: w as u32];
            let _: () = msg_send![&*desc, setMaxPixelsHigh: h as u32];
            // 96 dots to the inch, as an ordinary monitor has.
            let _: () = msg_send![&*desc, setSizeInMillimeters: CGSize::new(w as f64 * 25.4 / 96.0, h as f64 * 25.4 / 96.0)];
            let _: () = msg_send![&*desc, setVendorID: 0x5352u32];
            let _: () = msg_send![&*desc, setProductID: 0x0001u32];
            let _: () = msg_send![&*desc, setSerialNum: serial];
            let made: Allocated<AnyObject> = msg_send![display, alloc];
            let made: Option<Retained<AnyObject>> = msg_send![made, initWithDescriptor: &*desc];
            let made = made.ok_or("macOS did not add a display")?;
            let s: Retained<AnyObject> = msg_send![settings, new];
            let _: () = msg_send![&*s, setHiDPI: 0u32];
            let m: Allocated<AnyObject> = msg_send![mode, alloc];
            let m: Option<Retained<AnyObject>> = msg_send![m, initWithWidth: w as u32, height: h as u32, refreshRate: 60.0f64];
            let m = m.ok_or("macOS did not take the display's size")?;
            let modes = objc2_foundation::NSArray::from_retained_slice(&[m]);
            let _: () = msg_send![&*s, setModes: &*modes];
            let ok: Bool = msg_send![&*made, applySettings: &*s];
            if !ok.as_bool() {
                return Err(format!("macOS did not give the new display {w}x{h}"));
            }
            let id: u32 = msg_send![&*made, displayID];
            Ok((id.to_string(), Virtual { _display: made, _queue: queue }))
        }
    }

    pub fn screens() -> Vec<String> {
        let (mut ids, mut n) = ([0u32; 16], 0u32);
        // SAFETY: room for 16 displays; their count is written back.
        unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut n) };
        ids[..(n as usize).min(16)].iter().map(|id| id.to_string()).collect()
    }

    /// A mode's size in points, and its pixels to a point.
    fn size(m: &CGDisplayMode) -> ((u16, u16), usize) {
        let (w, h) = (CGDisplayMode::width(Some(m)), CGDisplayMode::height(Some(m)));
        let side = |v: usize| v.min(u16::MAX as usize) as u16;
        ((side(w), side(h)), CGDisplayMode::pixel_width(Some(m)) / w.max(1))
    }

    /// Display `id`'s modes for a desktop, each size once (at the scale the display has now where
    /// it can, else the sharper; the faster), and the one it has.
    fn all(id: &str) -> Option<(Vec<CFRetained<CGDisplayMode>>, CFRetained<CGDisplayMode>)> {
        let id: u32 = id.parse().ok()?;
        let current = CGDisplayCopyDisplayMode(id)?;
        // SAFETY: Core Graphics' own key, a static string.
        let key: &CFString = unsafe { kCGDisplayShowDuplicateLowResolutionModes };
        // The modes at another scale too (HiDPI, or not), to choose among.
        let options = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(true)]);
        // SAFETY: the options are the dictionary of the key and value the call takes.
        let list = unsafe { CGDisplayCopyAllDisplayModes(id, Some(options.as_opaque())) }?;
        let scale = size(&current).1;
        let rank = |m: &CGDisplayMode| {
            let k = size(m).1;
            (k == scale, k, CGDisplayMode::refresh_rate(Some(m)) as u32)
        };
        let mut picked: Vec<CFRetained<CGDisplayMode>> = Vec::new();
        for i in 0..list.count() {
            // SAFETY: an index inside the array, whose values are display modes.
            let m: &CGDisplayMode = unsafe { &*list.value_at_index(i).cast::<CGDisplayMode>() };
            if !CGDisplayMode::is_usable_for_desktop_gui(Some(m)) {
                continue;
            }
            match picked.iter().position(|o| size(o).0 == size(m).0) {
                Some(j) if rank(m) > rank(&picked[j]) => picked[j] = m.retain(),
                Some(_) => {}
                None => picked.push(m.retain()),
            }
        }
        Some((picked, current))
    }

    pub fn modes(id: &str) -> Option<Modes> {
        let (list, current) = all(id)?;
        Some(Modes { list: list.iter().map(|m| size(m).0).collect(), current: size(&current).0 })
    }

    pub fn set(id: &str, w: u16, h: u16) -> Result<(), String> {
        let (list, _) = all(id).ok_or("that display is gone")?;
        let mode = list.into_iter().find(|m| size(m).0 == (w, h)).ok_or_else(|| format!("that display has no {w}x{h}"))?;
        let display: u32 = id.parse().map_err(|_| "no such display")?;
        let mut config: CGDisplayConfigRef = std::ptr::null_mut();
        // SAFETY: a configuration begun here and then completed or cancelled; the mode is one
        // this display listed.
        let e = unsafe {
            let e = CGBeginDisplayConfiguration(&mut config);
            if e != CGError::Success {
                e
            } else if let e = CGConfigureDisplayWithDisplayMode(config, display, Some(&mode), None)
                && e != CGError::Success
            {
                CGCancelDisplayConfiguration(config);
                e
            } else {
                // Only while this program runs: the display is as it was once it ends, even unasked.
                CGCompleteDisplayConfiguration(config, CGConfigureOption::ForAppOnly)
            }
        };
        if e == CGError::Success { Ok(()) } else { Err(format!("macOS did not set the display to {w}x{h} (error {})", e.0)) }
    }
}

#[cfg(windows)]
mod win {
    use super::Modes;
    use windows_sys::Win32::Graphics::Gdi::{
        ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DM_BITSPERPEL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
        EnumDisplaySettingsExW,
    };

    fn device(id: &str) -> Vec<u16> {
        let full = if id.starts_with(r"\\.\") { id.to_string() } else { format!(r"\\.\{id}") };
        full.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn mode(dev: &[u16], i: u32) -> Option<DEVMODEW> {
        // SAFETY: a zeroed DEVMODEW with its size set, as the call expects.
        let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
        dm.dmSize = size_of::<DEVMODEW>() as u16;
        (unsafe { EnumDisplaySettingsExW(dev.as_ptr(), i, &mut dm, 0) } != 0).then_some(dm)
    }

    fn all(dev: &[u16]) -> Vec<DEVMODEW> {
        (0..).map_while(|i| mode(dev, i)).collect()
    }

    pub fn modes(id: &str) -> Option<Modes> {
        let dev = device(id);
        let cur = mode(&dev, ENUM_CURRENT_SETTINGS)?;
        let list = all(&dev).iter().filter(|m| m.dmBitsPerPel == cur.dmBitsPerPel).map(|m| (m.dmPelsWidth as u16, m.dmPelsHeight as u16)).collect();
        Some(Modes { list, current: (cur.dmPelsWidth as u16, cur.dmPelsHeight as u16) })
    }

    pub fn set(id: &str, w: u16, h: u16) -> Result<(), String> {
        let dev = device(id);
        let cur = mode(&dev, ENUM_CURRENT_SETTINGS).ok_or("the screen's mode is not known")?;
        // That size at the rate in use, else at its highest.
        let mut dm = all(&dev)
            .into_iter()
            .filter(|m| (m.dmPelsWidth, m.dmPelsHeight, m.dmBitsPerPel) == (w as u32, h as u32, cur.dmBitsPerPel))
            .max_by_key(|m| (m.dmDisplayFrequency == cur.dmDisplayFrequency, m.dmDisplayFrequency))
            .ok_or_else(|| format!("the screen has no {w}x{h}"))?;
        dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_BITSPERPEL | DM_DISPLAYFREQUENCY;
        // Not saved: what Windows keeps for the screen is not changed.
        let r = unsafe { ChangeDisplaySettingsExW(dev.as_ptr(), &dm, std::ptr::null_mut(), 0, std::ptr::null()) };
        if r == DISP_CHANGE_SUCCESSFUL { Ok(()) } else { Err(format!("Windows did not set {w}x{h} (code {r})")) }
    }
}

/// Screens added on Windows: Amyuni's virtual display driver (usbmmidd v2, signed through
/// Microsoft), unpacked from this program; its device made with the service (the helper;
/// `service::install`), which has an administrator's rights, where none is there, and removed with
/// it. RustDesk adds its screens with the same driver: a device of its own there is used as it is,
/// and neither it nor the driver is ever removed from here. Without one no screen is added. Each
/// screen is one `enableidd 1` of its tool; `enableidd 0` takes the last one added away.
#[cfg(windows)]
pub(crate) mod amyuni {
    use std::path::PathBuf;
    use std::process::Command;
    use windows_sys::Win32::Foundation::POINTL;
    use windows_sys::Win32::Graphics::Gdi::{
        CDS_NORESET, CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICEW, DM_PELSHEIGHT,
        DM_PELSWIDTH, DM_POSITION, ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW, EnumDisplaySettingsExW,
    };
    use windows_sys::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW};

    pub const NO_DRIVER: &str = "screens are added to Windows by a driver installed with the helper: install it there (remote access started with the computer)";

    /// The driver as Amyuni gives it, unchanged (its signature covers its files).
    const ZIP: &[u8] = include_bytes!("../assets/windows/usbmmidd_v2.zip");
    /// The sizes its screens offer, which it reads as one is added (up to ten).
    const SIZES: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\WUDF\Services\usbmmIdd\Parameters\Monitors";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// For x64 only: Amyuni has no Arm driver.
    fn offered() -> bool {
        cfg!(target_arch = "x86_64")
    }

    /// Where the driver is installed (by the service's installation).
    pub fn adds() -> bool {
        offered() && installed()
    }

    /// Where it would be, once the service is installed with it.
    pub fn without_driver() -> bool {
        offered() && !installed()
    }

    /// The devices this program made, kept to remove them and no other.
    fn ours() -> PathBuf {
        folder().join("usbmmidd-devices.txt")
    }

    /// The driver's devices, by instance (`ROOT\DISPLAY\0001`), as its tool lists them.
    fn devices() -> Vec<String> {
        tool_says(&["findall", "usbmmidd"])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once(" : ").or_else(|| l.split_once(": ")).map(|(id, _)| id.trim().to_string()))
            .filter(|id| id.contains('\\'))
            .collect()
    }

    /// Makes the driver's device where none works yet, with the service (an administrator's
    /// rights): once, quietly; the one made is noted.
    pub fn install() -> Result<(), String> {
        if !offered() || installed() {
            return Ok(());
        }
        let before = devices();
        tool(&["install", "usbmmidd.inf", "usbmmidd"])?;
        let made: Vec<String> = devices().into_iter().filter(|d| !before.contains(d)).collect();
        if !made.is_empty() {
            let _ = std::fs::write(ours(), made.join("\n"));
        }
        let up = (0..50).any(|_| installed() || { std::thread::sleep(std::time::Duration::from_millis(100)); false });
        if up { Ok(()) } else { Err("the virtual display driver did not start".into()) }
    }

    /// Removes the device made here and the unpacked files, with the service; the driver itself
    /// and anyone else's device stay.
    pub fn uninstall() {
        let made = std::fs::read_to_string(ours()).unwrap_or_default();
        for id in made.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let _ = tool(&["remove", &format!("@{id}")]);
        }
        let _ = std::fs::remove_file(ours());
        let _ = std::fs::remove_dir_all(folder().join("usbmmidd_v2"));
    }

    /// Whether the driver works here: a device of it is there, with its way in.
    pub(super) fn installed() -> bool {
        use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW, CR_SUCCESS};
        // The driver's device interface ({b5ffd75f-da40-4353-8ff8-b6daf6f1d8ca}).
        let guid = windows_sys::core::GUID { data1: 0xb5ffd75f, data2: 0xda40, data3: 0x4353, data4: [0x8f, 0xf8, 0xb6, 0xda, 0xf6, 0xf1, 0xd8, 0xca] };
        let mut len = 0u32;
        // SAFETY: asks the length of the list of that interface's present instances.
        let r = unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, &guid, std::ptr::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) };
        // An empty list is its one closing nul.
        r == CR_SUCCESS && len > 1
    }

    /// Beside the service's program, under Program Files, which only administrators change: the
    /// tool runs as an administrator or SYSTEM, so nobody else may have put it there.
    fn folder() -> PathBuf {
        crate::service::installed_exe()
            .and_then(|e| e.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\SSH Remote"))
    }

    /// The driver's files, unpacked by an administrator; its tool used only while it is the one
    /// this program carries, byte for byte.
    fn unpack() -> Result<PathBuf, String> {
        let base = folder();
        let dir = base.join("usbmmidd_v2");
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(ZIP)).map_err(|e| e.to_string())?;
        let mut tool = Vec::new();
        std::io::Read::read_to_end(&mut z.by_name("usbmmidd_v2/deviceinstaller64.exe").map_err(|e| e.to_string())?, &mut tool).map_err(|e| e.to_string())?;
        if std::fs::read(dir.join("deviceinstaller64.exe")).is_ok_and(|on| on == tool) {
            return Ok(dir);
        }
        if !crate::admin::elevated() {
            return Err("the virtual display driver's files are not there: install the helper".into());
        }
        let _ = std::fs::remove_dir_all(&dir);
        z.extract(&base).map_err(|e| format!("the virtual display driver could not be unpacked: {e}"))?;
        Ok(dir)
    }

    /// What the driver's tool says to `args`.
    fn tool_says(args: &[&str]) -> Result<String, String> {
        let dir = unpack()?;
        let mut cmd = Command::new(dir.join("deviceinstaller64.exe"));
        cmd.args(args).current_dir(&dir);
        let out = crate::sys::quiet(&mut cmd).output().map_err(|e| format!("the virtual display driver's tool: {e}"))?;
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() { Ok(said) } else { Err(format!("the virtual display driver's tool said: {said}")) }
    }

    fn tool(args: &[&str]) -> Result<(), String> {
        tool_says(args).map(drop)
    }

    /// Makes `w`x`h` one of the sizes its screens offer, in place of the last where it is not.
    fn offer(w: u16, h: u16) {
        let want = format!("{w},{h}");
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: a key opened, read and written as text values, and closed again.
        unsafe {
            if RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide(SIZES).as_ptr(), 0, KEY_READ | KEY_SET_VALUE, &mut key) != 0 {
                return;
            }
            let read = |name: &str| {
                let mut buf = [0u16; 64];
                let mut len = (buf.len() * 2) as u32;
                (RegQueryValueExW(key, wide(name).as_ptr(), std::ptr::null(), std::ptr::null_mut(), buf.as_mut_ptr() as _, &mut len) == 0)
                    .then(|| String::from_utf16_lossy(&buf[..(len as usize / 2)]).trim_end_matches('\0').to_string())
            };
            if !(0..10).any(|i| read(&i.to_string()).as_deref() == Some(want.as_str())) {
                let slot = (0..10).find(|i| read(&i.to_string()).is_none()).unwrap_or(9).to_string();
                let data = wide(&want);
                RegSetValueExW(key, wide(&slot).as_ptr(), 0, REG_SZ, data.as_ptr() as _, (data.len() * 2) as u32);
            }
            RegCloseKey(key);
        }
    }

    /// Display `id`'s size and place on the desktop, as Windows has them now.
    #[cfg(test)]
    pub(super) fn current(id: &str) -> Option<((u32, u32), (i32, i32))> {
        // SAFETY: as in `place`.
        let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
        dm.dmSize = size_of::<DEVMODEW>() as u16;
        if unsafe { EnumDisplaySettingsExW(wide(&format!(r"\\.\{id}")).as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm, 0) } == 0 {
            return None;
        }
        let at = unsafe { dm.Anonymous1.Anonymous2.dmPosition };
        Some(((dm.dmPelsWidth, dm.dmPelsHeight), (at.x, at.y)))
    }

    /// The display devices with a monitor, by name (`DISPLAY5`), and whether each is on the desktop.
    pub(super) fn displays() -> Vec<(String, bool)> {
        let mut out = Vec::new();
        for i in 0.. {
            // SAFETY: a zeroed DISPLAY_DEVICEW with its size set, as the call expects.
            let mut d: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
            d.cb = size_of::<DISPLAY_DEVICEW>() as u32;
            if unsafe { EnumDisplayDevicesW(std::ptr::null(), i, &mut d, 0) } == 0 {
                break;
            }
            let n = d.DeviceName.iter().position(|&c| c == 0).unwrap_or(d.DeviceName.len());
            let name = String::from_utf16_lossy(&d.DeviceName[..n]);
            let mut m: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
            m.cb = size_of::<DISPLAY_DEVICEW>() as u32;
            if unsafe { EnumDisplayDevicesW(wide(&name).as_ptr(), 0, &mut m, 0) } != 0 {
                out.push((name.trim_start_matches(r"\\.\").to_string(), d.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != 0));
            }
        }
        out
    }

    /// Screen `id` on the desktop right of the others, at `w`x`h` where it has that size.
    fn place(id: &str, w: u16, h: u16) -> Result<(), String> {
        let dev = |name: &str| wide(&format!(r"\\.\{name}"));
        let mut right = 0;
        for (name, on) in displays() {
            if !on || name == id {
                continue;
            }
            // SAFETY: a zeroed DEVMODEW with its size set; the position is read from its union as
            // the display's (not a printer's) settings.
            let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
            dm.dmSize = size_of::<DEVMODEW>() as u16;
            if unsafe { EnumDisplaySettingsExW(dev(&name).as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm, 0) } != 0 {
                let at = unsafe { dm.Anonymous1.Anonymous2.dmPosition };
                right = right.max(at.x + dm.dmPelsWidth as i32);
            }
        }
        let set = |fields, (w, h): (u16, u16)| {
            // SAFETY: as above, the union written as a display's.
            let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
            dm.dmSize = size_of::<DEVMODEW>() as u16;
            dm.dmFields = fields;
            (dm.dmPelsWidth, dm.dmPelsHeight) = (w as u32, h as u32);
            dm.Anonymous1.Anonymous2.dmPosition = POINTL { x: right, y: 0 };
            unsafe { ChangeDisplaySettingsExW(dev(id).as_ptr(), &dm, std::ptr::null_mut(), CDS_UPDATEREGISTRY | CDS_NORESET, std::ptr::null()) }
        };
        // Its size where it offers it, else the one it has; then where it goes.
        if set(DM_PELSWIDTH | DM_PELSHEIGHT | DM_POSITION, (w, h)) != 0 && set(DM_POSITION, (0, 0)) != 0 {
            return Err("Windows did not place the new screen".into());
        }
        // SAFETY: applies what was set above.
        unsafe { ChangeDisplaySettingsExW(std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), 0, std::ptr::null()) };
        Ok(())
    }

    pub fn add(w: u16, h: u16) -> Result<String, String> {
        if !installed() {
            return Err(NO_DRIVER.into());
        }
        if crate::admin::elevated() {
            offer(w, h);
        }
        let before: Vec<String> = displays().into_iter().map(|d| d.0).collect();
        tool(&["enableidd", "1"])?;
        let new = (0..50)
            .find_map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(100));
                displays().into_iter().map(|d| d.0).find(|d| !before.contains(d))
            })
            .ok_or("Windows added no screen (four are the most)")?;
        if let Err(e) = place(&new, w, h) {
            let _ = tool(&["enableidd", "0"]);
            return Err(e);
        }
        Ok(new)
    }

    /// Takes away the screen added last (the driver's way); the session takes them away in turn.
    pub fn remove(_id: &str) -> Result<(), String> {
        tool(&["enableidd", "0"])
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux::MutterState;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::Value;
    use x11rb::connection::Connection as _;
    use x11rb::protocol::randr::{self, ConnectionExt as _};
    use x11rb::protocol::xproto::ConnectionExt as _;
    use zbus::OwnedVariant;

    use super::{Modes, Way};

    /// Under Wayland, with the screen-sharing portal: the desktop's own way, by what answers.
    pub(super) fn wayland() -> Option<Way> {
        let run = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_default();
        let newest = |dir: &Path, keep: &dyn Fn(&str) -> bool| {
            std::fs::read_dir(dir).ok()?.flatten().filter(|e| keep(&e.file_name().to_string_lossy())).max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok()).map(|e| e.path())
        };
        if let Some(sock) = std::env::var_os("SWAYSOCK").map(PathBuf::from).or_else(|| newest(&run, &|n| n.starts_with("sway-ipc.") && n.ends_with(".sock")))
            && !sway_outputs(&sock).is_empty()
        {
            return Some(Way::Sway(sock));
        }
        if let Some(dir) = newest(&run.join("hypr"), &|_| true) {
            let sig = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !hypr_monitors(&sig).is_empty() {
                return Some(Way::Hypr(sig));
            }
        }
        if !cosmic_outputs().is_empty() {
            return Some(Way::Cosmic);
        }
        (!kscreen_outputs().is_empty()).then_some(Way::KScreen)
    }

    /// What a desktop's tool says, given a few seconds (a tool that hangs does not hold the
    /// session).
    fn run(cmd: &mut Command) -> Result<std::process::Output, String> {
        use std::io::Read;
        let mut child = cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().map_err(|e| e.to_string())?;
        let mut stdout = child.stdout.take().ok_or("no output")?;
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.read_to_end(&mut out);
            out
        });
        let until = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let status = loop {
            if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
                break s;
            }
            if std::time::Instant::now() > until {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the desktop's tool did not answer".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        Ok(std::process::Output { status, stdout: reader.join().unwrap_or_default(), stderr: Vec::new() })
    }

    fn json(cmd: &mut Command) -> Option<Value> {
        let out = run(cmd).ok().filter(|o| o.status.success())?;
        serde_json::from_slice(&out.stdout).ok()
    }

    /// An output as a desktop's own tool tells of it.
    pub(super) struct Output {
        pub name: String,
        pub current: (u16, u16),
        /// Each mode: its size, rate (Hz) and what the tool calls it.
        pub modes: Vec<((u16, u16), f64, String)>,
    }

    impl Output {
        pub fn modes(&self) -> Modes {
            Modes { list: self.modes.iter().map(|m| m.0).collect(), current: self.current }
        }

        /// The mode of that size at the rate nearest the one in use.
        fn pick(&self, w: u16, h: u16) -> Result<&((u16, u16), f64, String), String> {
            let rate = self.modes.iter().find(|m| m.0 == self.current).map_or(60.0, |m| m.1);
            self.modes
                .iter()
                .filter(|m| m.0 == (w, h))
                .min_by(|a, b| (a.1 - rate).abs().total_cmp(&(b.1 - rate).abs()))
                .ok_or_else(|| format!("{} has no {w}x{h}", self.name))
        }
    }

    /// Sizes offered for a screen that takes any (a virtual one, which lists no modes).
    const COMMON: [(u16, u16); 12] =
        [(3840, 2160), (2560, 1440), (1920, 1200), (1920, 1080), (1680, 1050), (1600, 900), (1440, 900), (1366, 768), (1280, 1024), (1280, 800), (1280, 720), (1024, 768)];

    /// The common sizes and the one it has, named as `name` says, for a screen without modes.
    fn any_size(current: (u16, u16), name: impl Fn(u16, u16) -> String) -> Vec<((u16, u16), f64, String)> {
        COMMON.iter().copied().chain([current]).map(|(w, h)| ((w, h), 60.0, name(w, h))).collect()
    }

    fn num(v: &Value) -> u16 {
        v.as_u64().unwrap_or(0).min(u16::MAX as u64) as u16
    }

    pub(super) fn sway_outputs(sock: &Path) -> Vec<Output> {
        json(Command::new("swaymsg").arg("-s").arg(sock).args(["-t", "get_outputs", "-r"])).map_or_else(Vec::new, |all| sway_parse(&all))
    }

    pub(super) fn sway_parse(all: &Value) -> Vec<Output> {
        all.as_array()
            .into_iter()
            .flatten()
            .filter(|o| o["active"].as_bool() == Some(true))
            .map(|o| {
                let current = (num(&o["current_mode"]["width"]), num(&o["current_mode"]["height"]));
                let mut modes: Vec<_> = o["modes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| {
                        let rate = m["refresh"].as_f64().unwrap_or(60000.0) / 1000.0;
                        let (w, h) = (num(&m["width"]), num(&m["height"]));
                        ((w, h), rate, format!("{w}x{h}@{rate:.3}Hz"))
                    })
                    .collect();
                if modes.is_empty() {
                    modes = any_size(current, |w, h| format!("--custom {w}x{h}"));
                }
                Output { name: o["name"].as_str().unwrap_or("").to_string(), current, modes }
            })
            .collect()
    }

    pub(super) fn sway_set(sock: &Path, id: &str, w: u16, h: u16) -> Result<(), String> {
        let out = sway_outputs(sock).into_iter().find(|o| o.name == id).ok_or("that screen is gone")?;
        let mode = &out.pick(w, h)?.2;
        let done = run(Command::new("swaymsg").arg("-s").arg(sock).arg(format!("output \"{id}\" mode {mode}")))?;
        if done.status.success() { Ok(()) } else { Err(format!("Sway did not set {w}x{h}")) }
    }

    fn hyprctl(sig: &str) -> Command {
        let mut c = Command::new("hyprctl");
        c.env("HYPRLAND_INSTANCE_SIGNATURE", sig);
        c
    }

    /// Hyprland's monitors, with where each is and its scale (to set it again as it is).
    pub(super) fn hypr_monitors(sig: &str) -> Vec<Output> {
        hypr_raw(sig).into_iter().map(|(o, _)| o).collect()
    }

    fn hypr_raw(sig: &str) -> Vec<(Output, String)> {
        json(hyprctl(sig).args(["-j", "monitors"])).map_or_else(Vec::new, |all| hypr_parse(&all))
    }

    pub(super) fn hypr_parse(all: &Value) -> Vec<(Output, String)> {
        all.as_array()
            .into_iter()
            .flatten()
            .map(|m| {
                let current = (num(&m["width"]), num(&m["height"]));
                let mut modes: Vec<_> = m["availableModes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| {
                        let s = s.as_str()?;
                        let (size, rate) = s.split_once('@')?;
                        let (w, h) = size.split_once('x')?;
                        Some(((w.parse().ok()?, h.parse().ok()?), rate.trim_end_matches("Hz").parse().unwrap_or(60.0), s.trim_end_matches("Hz").to_string()))
                    })
                    .collect();
                // A screen of no make or model (a headless one, whose one listed mode is not its
                // own) or with no modes takes any size.
                let virtual_screen = m["make"].as_str().unwrap_or("").is_empty() && m["model"].as_str().unwrap_or("").is_empty();
                if modes.is_empty() || virtual_screen {
                    modes = any_size(current, |w, h| format!("{w}x{h}"));
                }
                let place = format!("{}x{},{}", m["x"].as_i64().unwrap_or(0), m["y"].as_i64().unwrap_or(0), m["scale"].as_f64().unwrap_or(1.0));
                (Output { name: m["name"].as_str().unwrap_or("").to_string(), current, modes }, place)
            })
            .collect()
    }

    /// Whether output `name` is a screen of a virtual machine's graphics: once Hyprland has made
    /// one smaller it cannot make it larger again (it tests the new mode with the old, smaller
    /// picture, which such a driver refuses), so a screen there would stay small.
    pub(super) fn virtual_gpu(name: &str) -> bool {
        let Ok(all) = std::fs::read_dir("/sys/class/drm") else { return false };
        let suffix = format!("-{name}");
        all.flatten()
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(&suffix).map(str::to_string))
            .filter_map(|card| std::fs::read_link(format!("/sys/class/drm/{card}/device/driver")).ok())
            .any(|d| matches!(d.file_name().and_then(|f| f.to_str()), Some("virtio-pci" | "virtio_gpu" | "vmwgfx" | "qxl" | "bochs-drm" | "cirrus" | "cirrus-qemu" | "vboxvideo" | "hyperv_drm" | "simpledrm")))
    }

    pub(super) fn hypr_set(sig: &str, id: &str, w: u16, h: u16) -> Result<(), String> {
        let all = hypr_raw(sig);
        let (out, place) = all.iter().find(|(o, _)| o.name == id).ok_or("that screen is gone")?;
        let mode = &out.pick(w, h)?.2;
        let done = run(hyprctl(sig).args(["keyword", "monitor", &format!("{id},{mode},{place}")]))?;
        if done.status.success() && String::from_utf8_lossy(&done.stdout).trim() == "ok" { Ok(()) } else { Err(format!("Hyprland did not set {w}x{h}")) }
    }

    /// Adds a headless output of `w`x`h` right of the others; its name.
    pub(super) fn sway_add(sock: &Path, w: u16, h: u16) -> Result<String, String> {
        let msg = |cmd: &str| run(Command::new("swaymsg").arg("-s").arg(sock).arg(cmd)).map(|o| o.status.success());
        let all = || json(Command::new("swaymsg").arg("-s").arg(sock).args(["-t", "get_outputs", "-r"])).unwrap_or(Value::Null);
        let names = |all: &Value| -> Vec<String> { all.as_array().into_iter().flatten().filter_map(|o| o["name"].as_str().map(str::to_string)).collect() };
        let before = all();
        let right = before
            .as_array()
            .into_iter()
            .flatten()
            .filter(|o| o["active"].as_bool() == Some(true))
            .map(|o| o["rect"]["x"].as_i64().unwrap_or(0) + o["rect"]["width"].as_i64().unwrap_or(0))
            .max()
            .unwrap_or(0);
        if !msg("create_output")? {
            return Err("Sway did not add a screen".into());
        }
        let had = names(&before);
        let name = names(&all()).into_iter().find(|n| !had.contains(n)).ok_or("Sway added no screen")?;
        if !msg(&format!("output \"{name}\" mode --custom {w}x{h} position {right} 0"))? {
            let _ = sway_remove(sock, &name);
            return Err(format!("Sway did not give the new screen {w}x{h}"));
        }
        Ok(name)
    }

    pub(super) fn sway_remove(sock: &Path, id: &str) -> Result<(), String> {
        let done = run(Command::new("swaymsg").arg("-s").arg(sock).arg(format!("output \"{id}\" unplug")))?;
        if done.status.success() { Ok(()) } else { Err(format!("Sway did not take {id} away")) }
    }

    /// Adds a headless monitor of `w`x`h` right of the others; its name.
    pub(super) fn hypr_add(sig: &str, w: u16, h: u16) -> Result<String, String> {
        let names = || -> Vec<String> {
            json(hyprctl(sig).args(["-j", "monitors", "all"])).and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().filter_map(|m| m["name"].as_str().map(str::to_string)).collect()
        };
        let had = names();
        if !run(hyprctl(sig).args(["output", "create", "headless"]))?.status.success() {
            return Err("Hyprland did not add a screen".into());
        }
        let name = (0..20)
            .find_map(|i| {
                if i > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                names().into_iter().find(|n| !had.contains(n))
            })
            .ok_or("Hyprland added no screen")?;
        let done = run(hyprctl(sig).args(["keyword", "monitor", &format!("{name},{w}x{h}@60,auto-right,1")]))?;
        if !(done.status.success() && String::from_utf8_lossy(&done.stdout).trim() == "ok") {
            let _ = hypr_remove(sig, &name);
            return Err(format!("Hyprland did not give the new screen {w}x{h}"));
        }
        Ok(name)
    }

    pub(super) fn hypr_remove(sig: &str, id: &str) -> Result<(), String> {
        let done = run(hyprctl(sig).args(["output", "remove", id]))?;
        if done.status.success() { Ok(()) } else { Err(format!("Hyprland did not take {id} away")) }
    }

    /// KDE's tool, which reaches KWin through the session's Wayland display (Qt's other ways to
    /// draw find no screens, or hang).
    fn kscreen_doctor() -> Command {
        let mut c = Command::new("kscreen-doctor");
        c.env("QT_QPA_PLATFORM", "wayland");
        c
    }

    pub(super) fn kscreen_outputs() -> Vec<Output> {
        json(kscreen_doctor().arg("-j")).map_or_else(Vec::new, |all| kscreen_parse(&all))
    }

    pub(super) fn kscreen_parse(all: &Value) -> Vec<Output> {
        all["outputs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|o| o["enabled"].as_bool() == Some(true))
            .map(|o| {
                let id = |v: &Value| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())).unwrap_or_default();
                let modes: Vec<((u16, u16), f64, String)> = o["modes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| ((num(&m["size"]["width"]), num(&m["size"]["height"])), m["refreshRate"].as_f64().unwrap_or(60.0), id(&m["id"])))
                    .collect();
                let current = id(&o["currentModeId"]);
                let current = modes.iter().find(|m| m.2 == current).map_or((num(&o["size"]["width"]), num(&o["size"]["height"])), |m| m.0);
                Output { name: o["name"].as_str().unwrap_or("").to_string(), current, modes }
            })
            .collect()
    }

    pub(super) fn kscreen_set(id: &str, w: u16, h: u16) -> Result<(), String> {
        let out = kscreen_outputs().into_iter().find(|o| o.name == id).ok_or("that screen is gone")?;
        let mode = &out.pick(w, h)?.2;
        let done = run(kscreen_doctor().arg(format!("output.{id}.mode.{mode}")))?;
        if done.status.success() { Ok(()) } else { Err(format!("KDE did not set {w}x{h}")) }
    }

    /// COSMIC's outputs as `cosmic-randr list --kdl` tells them: each `output "NAME" enabled=...`
    /// block, its `mode W H RATE_MHZ` lines, the one in use marked `current=#true`.
    pub(super) fn cosmic_outputs() -> Vec<Output> {
        run(Command::new("cosmic-randr").args(["list", "--kdl"])).ok().filter(|o| o.status.success()).map_or_else(Vec::new, |o| cosmic_parse(&String::from_utf8_lossy(&o.stdout)))
    }

    pub(super) fn cosmic_parse(text: &str) -> Vec<Output> {
        let mut out: Vec<(Output, bool)> = Vec::new();
        for line in text.lines().map(str::trim) {
            if let Some(rest) = line.strip_prefix("output \"") {
                let name = rest.split('"').next().unwrap_or("").to_string();
                out.push((Output { name, current: (0, 0), modes: Vec::new() }, rest.contains("enabled=#true")));
            } else if let Some(rest) = line.strip_prefix("mode ")
                && let Some((o, _)) = out.last_mut()
            {
                let mut f = rest.split_whitespace();
                let (Some(w), Some(h), Some(r)) = (f.next().and_then(|v| v.parse().ok()), f.next().and_then(|v| v.parse().ok()), f.next().and_then(|v| v.parse::<f64>().ok())) else { continue };
                let rate = r / 1000.0;
                if rest.contains("current=#true") {
                    o.current = (w, h);
                }
                o.modes.push(((w, h), rate, format!("{rate:.3}")));
            }
        }
        out.into_iter().filter(|(o, on)| *on && o.current != (0, 0)).map(|(o, _)| o).collect()
    }

    pub(super) fn cosmic_set(id: &str, w: u16, h: u16) -> Result<(), String> {
        let out = cosmic_outputs().into_iter().find(|o| o.name == id).ok_or("that screen is gone")?;
        let rate = &out.pick(w, h)?.2;
        let done = run(Command::new("cosmic-randr").args(["mode", id, &w.to_string(), &h.to_string(), "--refresh", rate]))?;
        if done.status.success() { Ok(()) } else { Err(format!("COSMIC did not set {w}x{h}")) }
    }

    fn x() -> Option<(x11rb::rust_connection::RustConnection, u32)> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        conn.randr_query_version(1, 3).ok()?.reply().ok()?;
        Some((conn, root))
    }

    /// The outputs that show something: name, output, its CRTC.
    fn randr_outputs(conn: &impl x11rb::connection::Connection, res: &randr::GetScreenResourcesCurrentReply) -> Vec<(String, randr::GetOutputInfoReply, u32)> {
        res.outputs
            .iter()
            .filter_map(|&o| {
                let info = conn.randr_get_output_info(o, res.config_timestamp).ok()?.reply().ok()?;
                let name = String::from_utf8_lossy(&info.name).into_owned();
                let crtc = info.crtc;
                (crtc != 0).then_some((name, info, crtc))
            })
            .collect()
    }

    pub(super) fn randr_screens() -> Vec<String> {
        let Some((conn, root)) = x() else { return Vec::new() };
        let Some(res) = conn.randr_get_screen_resources_current(root).ok().and_then(|c| c.reply().ok()) else { return Vec::new() };
        randr_outputs(&conn, &res).into_iter().map(|o| o.0).collect()
    }

    fn sideways(rotation: randr::Rotation) -> bool {
        rotation.contains(randr::Rotation::ROTATE90) || rotation.contains(randr::Rotation::ROTATE270)
    }

    fn rate(m: &randr::ModeInfo) -> f64 {
        let total = m.htotal as f64 * m.vtotal as f64;
        if total > 0.0 { m.dot_clock as f64 / total } else { 0.0 }
    }

    pub(super) fn randr_modes(id: &str) -> Option<Modes> {
        let (conn, root) = x()?;
        let res = conn.randr_get_screen_resources_current(root).ok()?.reply().ok()?;
        let (_, info, crtc) = randr_outputs(&conn, &res).into_iter().find(|o| o.0 == id)?;
        let c = conn.randr_get_crtc_info(crtc, res.config_timestamp).ok()?.reply().ok()?;
        let size = |m: &randr::ModeInfo| if sideways(c.rotation) { (m.height, m.width) } else { (m.width, m.height) };
        let list = info.modes.iter().filter_map(|id| res.modes.iter().find(|m| m.id == *id)).map(size).collect();
        Some(Modes { list, current: (c.width, c.height) })
    }

    pub(super) fn randr_set(id: &str, w: u16, h: u16) -> Result<(), String> {
        let e = |e: &dyn std::fmt::Display| format!("X11: {e}");
        let (conn, root) = x().ok_or("no X display with RandR")?;
        let res = conn.randr_get_screen_resources_current(root).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?;
        let outputs = randr_outputs(&conn, &res);
        let (_, info, crtc) = outputs.iter().find(|o| o.0 == id).ok_or("that screen is gone")?;
        let c = conn.randr_get_crtc_info(*crtc, res.config_timestamp).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?;
        let side = sideways(c.rotation);
        let modes: Vec<&randr::ModeInfo> = info.modes.iter().filter_map(|m| res.modes.iter().find(|x| x.id == *m)).collect();
        let now = modes.iter().find(|m| m.id == c.mode).map_or(60.0, |m| rate(m));
        let mode = modes
            .iter()
            .filter(|m| if side { (m.height, m.width) == (w, h) } else { (m.width, m.height) == (w, h) })
            .min_by(|a, b| (rate(a) - now).abs().total_cmp(&(rate(b) - now).abs()))
            .ok_or_else(|| format!("that screen has no {w}x{h}"))?;
        // The X screen spans every output: as large as they are once this one has its new size.
        let mut right = c.x as i32 + w as i32;
        let mut bottom = c.y as i32 + h as i32;
        for (_, _, other) in outputs.iter().filter(|o| o.2 != *crtc) {
            if let Ok(o) = conn.randr_get_crtc_info(*other, res.config_timestamp).map_err(|x| e(&x)).and_then(|r| r.reply().map_err(|x| e(&x))) {
                right = right.max(o.x as i32 + o.width as i32);
                bottom = bottom.max(o.y as i32 + o.height as i32);
            }
        }
        let geo = conn.get_geometry(root).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?;
        let screen = &conn.setup().roots.iter().find(|s| s.root == root).ok_or("no X screen")?.clone();
        let mm = |px: i32, old_px: u16, old_mm: u16| ((px as f64 * old_mm as f64 / old_px.max(1) as f64).round() as u32).max(1);
        let sized = |conn: &x11rb::rust_connection::RustConnection| {
            conn.randr_set_screen_size(root, right as u16, bottom as u16, mm(right, screen.width_in_pixels, screen.width_in_millimeters), mm(bottom, screen.height_in_pixels, screen.height_in_millimeters))
                .map_err(|x| e(&x))
                .and_then(|c| c.check().map_err(|x| e(&x)))
        };
        let grows = right > geo.width as i32 || bottom > geo.height as i32;
        if grows {
            sized(&conn)?;
        }
        let r = conn
            .randr_set_crtc_config(*crtc, x11rb::CURRENT_TIME, res.config_timestamp, c.x, c.y, mode.id, c.rotation, &c.outputs)
            .map_err(|x| e(&x))?
            .reply()
            .map_err(|x| e(&x))?;
        if r.status != randr::SetConfig::SUCCESS {
            return Err(format!("the X server did not set {w}x{h}"));
        }
        if !grows && (right != geo.width as i32 || bottom != geo.height as i32) {
            sized(&conn)?;
        }
        conn.flush().map_err(|x| e(&x))?;
        Ok(())
    }

    /// X with RandR 1.5, which lists and sets monitors apart from outputs.
    fn x15() -> Option<(x11rb::rust_connection::RustConnection, u32)> {
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        let v = conn.randr_query_version(1, 5).ok()?.reply().ok()?;
        (v.major_version > 1 || v.minor_version >= 5).then_some((conn, root))
    }

    /// The X screen's millimetres for `px` pixels across (or down), as it has them now.
    fn mm(conn: &x11rb::rust_connection::RustConnection, root: u32, px: i32, across: bool) -> u32 {
        let Some(s) = conn.setup().roots.iter().find(|s| s.root == root) else { return 1 };
        let (p, m) = if across { (s.width_in_pixels, s.width_in_millimeters) } else { (s.height_in_pixels, s.height_in_millimeters) };
        ((px as f64 * m as f64 / p.max(1) as f64).round() as u32).max(1)
    }

    /// Adds a monitor of `w`x`h` right of the others, a part of the X screen grown to hold it (no
    /// output shows it, but windows go to it, are maximized on it and are read from it as on any
    /// monitor); its name.
    pub(super) fn randr_add(w: u16, h: u16) -> Result<String, String> {
        let e = |e: &dyn std::fmt::Display| format!("X11: {e}");
        let (conn, root) = x15().ok_or("this X server cannot add monitors (it has no RandR 1.5)")?;
        let mons = conn.randr_get_monitors(root, true).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?.monitors;
        let names: Vec<String> = mons.iter().filter_map(|m| conn.get_atom_name(m.name).ok()?.reply().ok()).map(|n| String::from_utf8_lossy(&n.name).into_owned()).collect();
        let name = (1..).map(|n| format!("VIRTUAL-{n}")).find(|n| !names.contains(n)).unwrap_or_default();
        let right = mons.iter().map(|m| m.x as i32 + m.width as i32).max().unwrap_or(0).max(0);
        let geo = conn.get_geometry(root).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?;
        let (nw, nh) = ((right + w as i32).max(geo.width as i32), (h as i32).max(geo.height as i32));
        let range = conn.randr_get_screen_size_range(root).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?;
        if nw > range.max_width as i32 || nh > range.max_height as i32 || right > i16::MAX as i32 {
            return Err(format!("the X screen can be at most {}x{}: no room for another {w}x{h}", range.max_width, range.max_height));
        }
        if (nw, nh) != (geo.width as i32, geo.height as i32) {
            conn.randr_set_screen_size(root, nw as u16, nh as u16, mm(&conn, root, nw, true), mm(&conn, root, nh, false))
                .map_err(|x| e(&x))?
                .check()
                .map_err(|x| e(&x))?;
        }
        let atom = conn.intern_atom(false, name.as_bytes()).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?.atom;
        let m = randr::MonitorInfo {
            name: atom,
            primary: false,
            automatic: false,
            x: right as i16,
            y: 0,
            width: w,
            height: h,
            width_in_millimeters: mm(&conn, root, w as i32, true),
            height_in_millimeters: mm(&conn, root, h as i32, false),
            outputs: Vec::new(),
        };
        conn.randr_set_monitor(root, m).map_err(|x| e(&x))?.check().map_err(|x| e(&x))?;
        // The part the X screen grew by holds what its memory held: black, then the desktop's
        // background where it has one (asked to draw it again).
        let black = conn.setup().roots.iter().find(|s| s.root == root).map_or(0, |s| s.black_pixel);
        if let Ok(gc) = conn.generate_id() {
            let _ = conn.create_gc(gc, root, &x11rb::protocol::xproto::CreateGCAux::new().foreground(black).subwindow_mode(x11rb::protocol::xproto::SubwindowMode::CLIP_BY_CHILDREN));
            let _ = conn.poly_fill_rectangle(root, gc, &[x11rb::protocol::xproto::Rectangle { x: right as i16, y: 0, width: w, height: nh as u16 }]);
            let _ = conn.free_gc(gc);
        }
        let _ = conn.clear_area(true, root, right as i16, 0, w, nh as u16);
        conn.flush().map_err(|x| e(&x))?;
        Ok(name)
    }

    /// Takes monitor `id` away, and the X screen back to what the others span.
    pub(super) fn randr_remove(id: &str) -> Result<(), String> {
        let e = |e: &dyn std::fmt::Display| format!("X11: {e}");
        let (conn, root) = x15().ok_or("no X display with RandR 1.5")?;
        let atom = conn.intern_atom(true, id.as_bytes()).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?.atom;
        if atom == 0 {
            return Ok(());
        }
        conn.randr_delete_monitor(root, atom).map_err(|x| e(&x))?.check().map_err(|x| e(&x))?;
        let mons = conn.randr_get_monitors(root, true).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?.monitors;
        let right = mons.iter().map(|m| m.x as i32 + m.width as i32).max().unwrap_or(0);
        let bottom = mons.iter().map(|m| m.y as i32 + m.height as i32).max().unwrap_or(0);
        let geo = conn.get_geometry(root).map_err(|x| e(&x))?.reply().map_err(|x| e(&x))?;
        if right > 0 && bottom > 0 && (right < geo.width as i32 || bottom < geo.height as i32) {
            // Not smaller than the outputs need, which the server refuses; then it stays as large.
            let _ = conn.randr_set_screen_size(root, right as u16, bottom as u16, mm(&conn, root, right, true), mm(&conn, root, bottom, false)).map(|c| c.check());
        }
        conn.flush().map_err(|x| e(&x))?;
        Ok(())
    }

    // GetCurrentState's answer, which the GNOME capture reads too.
    pub(crate) type MutterMode = (String, i32, i32, f64, f64, Vec<f64>, HashMap<String, OwnedVariant>);
    pub(crate) type MutterMonitor = ((String, String, String, String), Vec<MutterMode>, HashMap<String, OwnedVariant>);
    pub(crate) type MutterLogical = (i32, i32, f64, u32, bool, Vec<(String, String, String, String)>, HashMap<String, OwnedVariant>);
    pub(crate) type MutterState = (u32, Vec<MutterMonitor>, Vec<MutterLogical>, HashMap<String, OwnedVariant>);

    /// Calls `method` of GNOME's display configuration.
    fn mutter<B, R>(method: &str, body: &B) -> zbus::Result<R>
    where
        B: zbus::export::serde::Serialize + zbus::DynamicType,
        R: for<'d> zbus::wire::DynamicDeserialize<'d>,
    {
        zbus::block_on(async {
            let conn = zbus::Connection::session().await?;
            let p = zbus::Proxy::new(&conn, "org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig", "org.gnome.Mutter.DisplayConfig").await?;
            p.call(method, body).await
        })
    }

    fn mutter_state() -> Option<MutterState> {
        mutter("GetCurrentState", &()).ok()
    }

    fn flag(props: &HashMap<String, OwnedVariant>, key: &str) -> bool {
        props.get(key).is_some_and(|v| bool::try_from(v).unwrap_or(false))
    }

    pub(super) fn mutter_screens() -> Vec<String> {
        mutter_state().map(|s| s.2.iter().flat_map(|l| l.5.iter().map(|m| m.0.clone())).collect()).unwrap_or_default()
    }

    pub(super) fn mutter_modes(id: &str) -> Option<Modes> {
        let (_, monitors, ..) = mutter_state()?;
        let (_, modes, _) = monitors.into_iter().find(|m| m.0.0 == id)?;
        let current = modes.iter().find(|m| flag(&m.6, "is-current"))?;
        let current = (current.1 as u16, current.2 as u16);
        Some(Modes { list: modes.iter().map(|m| (m.1 as u16, m.2 as u16)).collect(), current })
    }

    /// A logical monitor as ApplyMonitorsConfig takes it: place, scale, turn, primary, and each
    /// monitor's connector and mode.
    pub(super) type Placed = (i32, i32, f64, u32, bool, Vec<(String, String, HashMap<String, OwnedVariant>)>);

    /// The layout with monitor `id` at `w`x`h`, the ones beside or below it moved to keep touching
    /// it, and the properties to keep.
    pub(super) fn mutter_layout(state: &MutterState, id: &str, w: u16, h: u16) -> Result<(Vec<Placed>, HashMap<String, OwnedVariant>), String> {
        let (_, monitors, logical, props) = state;
        let current_of = |c: &str| monitors.iter().find(|m| m.0.0 == c).and_then(|m| m.1.iter().find(|x| flag(&x.6, "is-current")).cloned());
        let (_, modes, _) = monitors.iter().find(|m| m.0.0 == id).ok_or("that screen is gone")?;
        let old = current_of(id).ok_or("that screen shows nothing")?;
        let new = modes
            .iter()
            .filter(|m| (m.1, m.2) == (w as i32, h as i32))
            .min_by(|a, b| (a.3 - old.3).abs().total_cmp(&(b.3 - old.3).abs()))
            .ok_or_else(|| format!("that screen has no {w}x{h}"))?;
        // In the logical layout a monitor takes its size divided by its scale.
        let physical = props.get("layout-mode").and_then(|v| u32::try_from(v).ok()) == Some(2);
        let own = logical.iter().find(|l| l.5.iter().any(|m| m.0 == id)).ok_or("that screen is not laid out")?;
        let scale = if new.5.iter().any(|s| (s - own.2).abs() < 1e-6) { own.2 } else { new.4 };
        let span = |px: i32, s: f64| if physical { px } else { (px as f64 / s).round() as i32 };
        let side = own.3 % 2 == 1;
        let (ow, oh) = if side { (old.2, old.1) } else { (old.1, old.2) };
        let (nw, nh) = if side { (new.2, new.1) } else { (new.1, new.2) };
        let (dx, dy) = (span(nw, scale) - span(ow, own.2), span(nh, scale) - span(oh, own.2));
        let (right, bottom) = (own.0 + span(ow, own.2), own.1 + span(oh, own.2));
        let layout = logical
            .iter()
            .map(|l| {
                let mine = l.5.iter().any(|m| m.0 == id);
                let x = if !mine && l.0 >= right { l.0 + dx } else { l.0 };
                let y = if !mine && l.1 >= bottom { l.1 + dy } else { l.1 };
                let monitors = l.5.iter().filter_map(|m| Some((m.0.clone(), if m.0 == id { new.0.clone() } else { current_of(&m.0)?.0 }, HashMap::new()))).collect();
                (x, y, if mine { scale } else { l.2 }, l.3, l.4, monitors)
            })
            .collect();
        let mut keep: HashMap<String, OwnedVariant> = HashMap::new();
        if let Some(mode) = props.get("layout-mode").and_then(|v| v.try_clone().ok()) {
            keep.insert("layout-mode".into(), mode);
        }
        Ok((layout, keep))
    }

    pub(super) fn mutter_set(id: &str, w: u16, h: u16) -> Result<(), String> {
        let state = mutter_state().ok_or("GNOME's display configuration does not answer")?;
        let (layout, keep) = mutter_layout(&state, id, w, h)?;
        // Temporary (1): not saved, so the saved layout comes back with the next login.
        let r: zbus::Result<()> = mutter("ApplyMonitorsConfig", &(state.0, 1u32, layout, keep));
        r.map_err(|e| format!("GNOME did not set {w}x{h}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolutions_are_listed_once_largest_first() {
        assert_eq!(tidy(vec![(1280, 720), (1920, 1080), (1280, 720), (800, 600), (320, 200), (1280, 1024)]), vec![(1920, 1080), (1280, 1024), (1280, 720), (800, 600)]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_desktops_tools_are_read() {
        use serde_json::json;
        let sway = linux::sway_parse(&json!([
            {"name": "DP-1", "active": true, "current_mode": {"width": 2560, "height": 1440, "refresh": 59951},
             "modes": [{"width": 2560, "height": 1440, "refresh": 59951}, {"width": 1920, "height": 1080, "refresh": 60000}, {"width": 1920, "height": 1080, "refresh": 50000}]},
            {"name": "HEADLESS-1", "active": true, "current_mode": {"width": 1280, "height": 800, "refresh": 0}, "modes": []},
            {"name": "HDMI-A-1", "active": false, "modes": []}]));
        assert_eq!(sway.len(), 2);
        assert_eq!((sway[0].name.as_str(), sway[0].current, sway[0].modes().list.len()), ("DP-1", (2560, 1440), 3));
        assert!(sway[1].modes.iter().any(|m| m.2 == "--custom 1920x1080"), "a virtual screen takes any size");
        let hypr = linux::hypr_parse(&json!([{"name": "eDP-1", "make": "BOE", "model": "0x0BCA", "width": 1920, "height": 1200, "x": 0, "y": 0, "scale": 1.25, "availableModes": ["1920x1200@60.00Hz", "1920x1200@48.00Hz", "1280x800@60.00Hz"]},
            {"name": "HEADLESS-2", "make": "", "model": "", "width": 1280, "height": 800, "x": 1920, "y": 0, "scale": 1.0, "availableModes": ["1920x1080@0.06Hz"]}]));
        assert!(hypr[1].0.modes.iter().any(|m| m.2 == "1024x768") && !hypr[1].0.modes.iter().any(|m| m.1 < 1.0), "a headless screen takes any size");
        assert_eq!(hypr[0].1, "0x0,1.25");
        assert_eq!(hypr[0].0.modes().list, vec![(1920, 1200), (1920, 1200), (1280, 800)]);
        let kde = linux::kscreen_parse(&json!({"outputs": [{"name": "Virtual-0", "enabled": true, "currentModeId": "2", "size": {"width": 1280, "height": 800},
            "modes": [{"id": "1", "refreshRate": 60, "size": {"width": 1920, "height": 1080}}, {"id": "2", "refreshRate": 60, "size": {"width": 1280, "height": 800}}]}]}));
        assert_eq!((kde[0].current, kde[0].modes().list.len()), ((1280, 800), 2));
        let cosmic = linux::cosmic_parse(
            "output \"Virtual-1\" enabled=#true {\n  description make=\"RHT\" model=\"QEMU Monitor\"\n  modes {\n    mode 1920 1080 60000\n    mode 1280 800 74989 current=#true preferred=#true\n    mode 1024 768 60004\n  }\n}\noutput \"HDMI-A-1\" enabled=#false {\n  modes {\n    mode 1920 1080 60000\n  }\n}\n",
        );
        assert_eq!(cosmic.len(), 1, "a display that is off is left out");
        assert_eq!((cosmic[0].name.as_str(), cosmic[0].current, cosmic[0].modes().list.len()), ("Virtual-1", (1280, 800), 3));
        assert_eq!(cosmic[0].modes[1].2, "74.989");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn gnome_moves_the_screen_beside_one_that_grows() {
        use std::collections::HashMap;
        use zbus::{OwnedVariant, Variant};
        let yes = || -> HashMap<String, OwnedVariant> { HashMap::from([("is-current".to_string(), OwnedVariant::try_from(Variant::from(true)).unwrap())]) };
        let mode = |id: &str, w, h, current: bool| (id.to_string(), w, h, 60.0, 1.0, vec![1.0, 2.0], if current { yes() } else { HashMap::new() });
        let spec = |c: &str| (c.to_string(), "v".to_string(), "p".to_string(), "s".to_string());
        let state: linux::MutterState = (
            7,
            vec![
                (spec("DP-1"), vec![mode("a", 1920, 1080, false), mode("b", 1280, 720, true)], HashMap::new()),
                (spec("DP-2"), vec![mode("c", 1920, 1080, true)], HashMap::new()),
            ],
            vec![(0, 0, 1.0, 0, true, vec![spec("DP-1")], HashMap::new()), (1280, 0, 1.0, 0, false, vec![spec("DP-2")], HashMap::new())],
            HashMap::new(),
        );
        let (layout, _) = linux::mutter_layout(&state, "DP-1", 1920, 1080).unwrap();
        assert_eq!((layout[0].0, layout[0].5[0].1.as_str()), (0, "a"));
        assert_eq!((layout[1].0, layout[1].5[0].1.as_str()), (1920, "c"), "the one on its right moves along");
        assert!(linux::mutter_layout(&state, "DP-1", 800, 600).is_err());
    }

    /// This computer's screens and their resolutions, nothing set: cargo test -- --ignored --nocapture screens_here
    #[test]
    #[ignore]
    #[cfg(any(windows, target_os = "linux"))]
    fn screens_here_are_listed() {
        #[cfg(windows)]
        let (way, ids) = (Way::Windows, (1..=4).map(|i| format!("DISPLAY{i}")).collect::<Vec<_>>());
        #[cfg(target_os = "linux")]
        let way = if std::env::var_os("WAYLAND_DISPLAY").is_some() { linux::wayland().unwrap_or(Way::Mutter) } else { Way::RandR };
        #[cfg(target_os = "linux")]
        let ids = way.screens();
        for id in ids {
            if let Some(m) = way.modes(&id) {
                println!("{id}: now {}x{}, {} resolutions: {:?}", m.current.0, m.current.1, tidy(m.list.clone()).len(), tidy(m.list));
            }
        }
    }

    /// The first screen set to another resolution for a few seconds, then set back as the
    /// session ends: SSHREMOTE_TEST_SET_SCREEN=1 cargo test -- --ignored --nocapture screen_here_is_set
    #[test]
    #[ignore]
    #[cfg(any(windows, target_os = "linux"))]
    fn screen_here_is_set_and_set_back() {
        // It changes the screen of whoever runs it: only when asked by name, never in a run of all.
        if std::env::var_os("SSHREMOTE_TEST_SET_SCREEN").is_none() {
            eprintln!("skipped: set SSHREMOTE_TEST_SET_SCREEN=1 to let it change this computer's screen");
            return;
        }
        #[cfg(windows)]
        let mut s = Session::new("windows");
        #[cfg(target_os = "linux")]
        let mut s = Session::new(if std::env::var_os("WAYLAND_DISPLAY").is_some() { "portal" } else { "x11" });
        let id = s.screen(if cfg!(windows) { Some("DISPLAY1".into()) } else { None }).expect("a screen");
        let (m, own) = s.modes(&id).expect("its resolutions");
        let want = [(1280, 720), (1024, 768)].into_iter().chain(m.list.iter().copied()).find(|r| m.list.contains(r) && *r != own).expect("another resolution");
        println!("{id}: {}x{}, set to {}x{}", own.0, own.1, want.0, want.1);
        s.set(&id, want.0, want.1).expect("set");
        println!("now {:?}", s.modes(&id).map(|m| m.0.current));
        std::thread::sleep(std::time::Duration::from_secs(3));
        drop(s);
        let back = Way::find(if cfg!(windows) { "windows" } else { "x11" }).and_then(|w| w.modes(&id)).map(|m| m.current);
        println!("after the session: {back:?}");
        assert_eq!(back, Some(own));
    }

    #[test]
    fn a_session_where_screens_cannot_be_set_says_so() {
        let mut s = Session::new("browser");
        assert_eq!(s.screen(Some("x".into())), None);
        assert!(s.set("x", 800, 600).is_err());
    }

    /// A Mac's displays and their modes as listed here, nothing set.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn a_macs_modes_are_listed() {
        let s = Session::new("mac");
        let all = mac::screens();
        assert!(!all.is_empty());
        for id in &all {
            let (m, own) = s.modes(id).expect("its modes");
            println!("{id}: {m:?}");
            assert!(m.list.contains(&m.current) && own == m.current, "{m:?}");
        }
    }

    /// Windows' virtual display driver installed (as the service's installation does), a screen
    /// added and placed right of the others at its size, taken away; another of a size the driver
    /// does not list at first, taken away by the session's end; the device made removed again
    /// (the driver, and a device another program made, stay). It changes the display: run with its
    /// person's yes, in the signed-in session as an administrator. Nothing on the screen is pictured.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn a_windows_screen_is_added_and_taken_away() {
        let soon = |what: &dyn Fn() -> bool| (0..50).any(|_| what() || { std::thread::sleep(std::time::Duration::from_millis(100)); false });
        let on = |id: &str| amyuni::displays().iter().any(|(n, attached)| n == id && *attached);
        println!("before: {:?}", amyuni::displays());
        amyuni::install().expect("the driver installed");
        assert!(amyuni::installed());
        let mut s = Session::new("windows");
        assert!(s.adds(), "screens are added with the driver there");
        let id = s.add(1920, 1080).expect("a screen added");
        assert!(soon(&|| on(&id)), "{id} on the desktop: {:?}", amyuni::displays());
        let (size, at) = amyuni::current(&id).expect("its settings");
        println!("added {id}: {size:?} at {at:?}; all {:?}", amyuni::displays());
        assert_eq!(size, (1920, 1080));
        let others = amyuni::displays().into_iter().filter(|(n, a)| *a && *n != id).filter_map(|(n, _)| amyuni::current(&n)).map(|(s, at)| at.0 + s.0 as i32).max().unwrap_or(0);
        assert_eq!(at, (others, 0), "right of the others");
        std::thread::sleep(std::time::Duration::from_secs(3));
        s.remove(&id).expect("taken away");
        assert!(soon(&|| !on(&id)), "{id} gone: {:?}", amyuni::displays());
        let id = s.add(1280, 800).expect("another, of a size offered for it");
        assert!(soon(&|| on(&id)));
        println!("added {id}: {:?}", amyuni::current(&id));
        assert_eq!(amyuni::current(&id).map(|c| c.0), Some((1280, 800)));
        drop(s);
        assert!(soon(&|| !on(&id)), "gone with the session: {:?}", amyuni::displays());
        if std::env::var_os("SSHREMOTE_KEEP_DRIVER").is_none() {
            amyuni::uninstall();
            assert!(!amyuni::installed(), "the driver removed");
        }
        println!("after: {:?}", amyuni::displays());
    }

    /// A screen added and held for SSHREMOTE_HOLD_SECS (its device made, and removed after), to try
    /// this computer as one with two monitors meanwhile; its name is written to the file
    /// SSHREMOTE_HOLD_NOTE names once it is there. Same conditions as the test above.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn a_windows_screen_held_for_a_while() {
        let secs = std::env::var("SSHREMOTE_HOLD_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(60);
        amyuni::install().expect("the driver installed");
        let mut s = Session::new("windows");
        let id = s.add(1920, 1080).expect("a screen added");
        println!("added {id}: {:?}; all {:?}", amyuni::current(&id), amyuni::displays());
        if let Some(note) = std::env::var_os("SSHREMOTE_HOLD_NOTE") {
            std::fs::write(note, &id).expect("the note");
        }
        std::thread::sleep(std::time::Duration::from_secs(secs));
        drop(s);
        amyuni::uninstall();
        println!("after: {:?}", amyuni::displays());
    }

    /// A display added for a session: macOS lists it, the capture finds and shows it at its size
    /// (only the size is looked at), and it is gone once taken away, or once the session ends.
    /// Run as the signed app (mactest.sh), which may take the screen's pictures.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn a_mac_gets_a_display_for_the_session() {
        let soon = |what: &dyn Fn() -> bool| (0..40).any(|_| what() || { std::thread::sleep(std::time::Duration::from_millis(100)); false });
        // The capture first, as a session has it.
        let mut cap = crate::capture::open("mac").expect("the capture");
        let mut s = Session::new("mac");
        // Not on a Mac without Metal (this test's virtual machine): there WindowServer died.
        if !s.adds() {
            assert!(s.add(1280, 720).is_err());
            println!("no displays added here (no Metal)");
            return;
        }
        let id = s.add(1280, 720).expect("a display added");
        assert!(soon(&|| mac::screens().contains(&id)), "listed: {:?}", mac::screens());
        let sized = soon(&|| s.modes(&id).is_some_and(|(m, _)| m.current == (1280, 720)));
        assert!(sized, "its mode: {:?}", s.modes(&id));
        let at = (0..40).find_map(|_| {
            cap.screens_changed();
            let at = cap.screen_ids().iter().position(|x| *x == id);
            if at.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            at
        });
        let at = at.expect("the capture lists it");
        cap.select(at).expect("shown");
        assert_eq!(cap.desktop_size(), (1280, 720));
        let picture = (0..50).find_map(|_| cap.grab().ok().flatten().or_else(|| { std::thread::sleep(std::time::Duration::from_millis(100)); None }));
        let p = picture.expect("a picture of it");
        println!("added {id}: picture {}x{}", p.w, p.h);
        cap.select(0).expect("back on the first");
        s.remove(&id).expect("taken away");
        assert!(soon(&|| !mac::screens().contains(&id)), "gone: {:?}", mac::screens());
        // One added and left: the session's end takes it away.
        let id = s.add(1920, 1080).expect("another");
        assert!(soon(&|| mac::screens().contains(&id)));
        drop(s);
        assert!(soon(&|| !mac::screens().contains(&id)), "gone with the session");
    }
}
