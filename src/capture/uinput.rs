//! A keyboard and a pointer made in the kernel (uinput), for the login screen and the lock screen,
//! where no desktop takes input from programs: whatever draws the screen reads them like any
//! other devices. Keys are put where the system's keyboard layout has them.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;

use super::super::Input;
use crate::proto::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

const fn ioc(dir: u32, nr: u32, size: usize) -> libc::c_ulong {
    ((dir << 30) | ((size as u32) << 16) | ((b'U' as u32) << 8) | nr) as libc::c_ulong
}
const UI_DEV_CREATE: libc::c_ulong = ioc(0, 1, 0);
const UI_DEV_SETUP: libc::c_ulong = ioc(1, 3, std::mem::size_of::<Setup>());
const UI_ABS_SETUP: libc::c_ulong = ioc(1, 4, std::mem::size_of::<AbsSetup>());
const UI_SET_EVBIT: libc::c_ulong = ioc(1, 100, 4);
const UI_SET_KEYBIT: libc::c_ulong = ioc(1, 101, 4);
const UI_SET_RELBIT: libc::c_ulong = ioc(1, 102, 4);
const UI_SET_ABSBIT: libc::c_ulong = ioc(1, 103, 4);

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTALT: u16 = 56;
const KEY_LEFTMETA: u16 = 125;
const KEY_RIGHTALT: u16 = 100;

#[repr(C)]
struct Setup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct AbsSetup {
    code: u16,
    _pad: u16,
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct Event {
    sec: libc::time_t,
    usec: libc::suseconds_t,
    kind: u16,
    code: u16,
    value: i32,
}

fn set(f: &File, req: libc::c_ulong, value: u16) -> Result<(), String> {
    // SAFETY: a uinput setup ioctl taking an int.
    if unsafe { libc::ioctl(f.as_raw_fd(), req as _, value as libc::c_int) } < 0 {
        return Err(format!("uinput: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// A new device with `keys`, and relative or absolute axes.
fn device(name: &str, keys: &[u16], rel: &[u16], abs: Option<(usize, usize)>) -> Result<File, String> {
    let f = std::fs::OpenOptions::new().write(true).open("/dev/uinput").map_err(|e| format!("/dev/uinput: {e}"))?;
    set(&f, UI_SET_EVBIT, EV_KEY)?;
    for &k in keys {
        set(&f, UI_SET_KEYBIT, k)?;
    }
    if !rel.is_empty() {
        set(&f, UI_SET_EVBIT, EV_REL)?;
        for &r in rel {
            set(&f, UI_SET_RELBIT, r)?;
        }
    }
    if let Some((w, h)) = abs {
        set(&f, UI_SET_EVBIT, EV_ABS)?;
        for (code, max) in [(0u16, w), (1, h)] {
            set(&f, UI_SET_ABSBIT, code)?;
            let a = AbsSetup { code, _pad: 0, value: 0, minimum: 0, maximum: max.saturating_sub(1).max(1) as i32, fuzz: 0, flat: 0, resolution: 0 };
            // SAFETY: the struct UI_ABS_SETUP is sized for.
            if unsafe { libc::ioctl(f.as_raw_fd(), UI_ABS_SETUP as _, &a) } < 0 {
                return Err(format!("uinput: {}", std::io::Error::last_os_error()));
            }
        }
    }
    let mut s = Setup { bustype: 0x06, vendor: 0x5352, product: 0x0001, version: 1, name: [0; 80], ff_effects_max: 0 };
    s.name[..name.len().min(79)].copy_from_slice(&name.as_bytes()[..name.len().min(79)]);
    // SAFETY: as above.
    unsafe {
        if libc::ioctl(f.as_raw_fd(), UI_DEV_SETUP as _, &s) < 0 || libc::ioctl(f.as_raw_fd(), UI_DEV_CREATE as _) < 0 {
            return Err(format!("uinput: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(f)
}

fn send(f: &mut File, events: &[(u16, u16, i32)]) -> Result<(), String> {
    let mut buf = Vec::with_capacity((events.len() + 1) * std::mem::size_of::<Event>());
    for &(kind, code, value) in events.iter().chain([(EV_SYN, 0, 0)].iter()) {
        let e = Event { sec: 0, usec: 0, kind, code, value };
        // SAFETY: a plain #[repr(C)] struct as its bytes.
        buf.extend_from_slice(unsafe { std::slice::from_raw_parts(&e as *const Event as *const u8, std::mem::size_of::<Event>()) });
    }
    f.write_all(&buf).map_err(|e| format!("uinput: {e}"))
}

pub struct Devices {
    keyboard: File,
    pointer: File,
    /// For the mouse for games: moves by steps, made when first used.
    relative: Option<File>,
    size: (usize, usize),
    keys: Keymap,
    held: Vec<u16>,
}

const BUTTONS: [u16; 5] = [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA];

impl Devices {
    pub fn open(w: usize, h: usize) -> Result<Devices, String> {
        let all: Vec<u16> = (1..=248).collect();
        let keyboard = device("SSH Remote keyboard", &all, &[], None)?;
        let pointer = device("SSH Remote pointer", &BUTTONS, &[REL_WHEEL, REL_HWHEEL], Some((w, h)))?;
        Ok(Devices { keyboard, pointer, relative: None, size: (w, h), keys: Keymap::system(), held: Vec::new() })
    }

    /// The screen shown changed size: the pointer's range follows (set only as it is made).
    pub fn resize(&mut self, w: usize, h: usize) -> Result<(), String> {
        if (w, h) != self.size {
            self.pointer = device("SSH Remote pointer", &BUTTONS, &[REL_WHEEL, REL_HWHEEL], Some((w, h)))?;
            self.size = (w, h);
        }
        Ok(())
    }

    /// Wakes a sleeping display, as a key nobody types does (Shift alone).
    pub fn wake(&mut self) -> Result<(), String> {
        send(&mut self.keyboard, &[(EV_KEY, KEY_LEFTSHIFT, 1)])?;
        send(&mut self.keyboard, &[(EV_KEY, KEY_LEFTSHIFT, 0)])
    }

    fn key(&mut self, code: u16, down: bool) -> Result<(), String> {
        if down && !self.held.contains(&code) {
            self.held.push(code);
        } else if !down {
            self.held.retain(|&c| c != code);
        }
        send(&mut self.keyboard, &[(EV_KEY, code, i32::from(down))])
    }

    /// Presses or lets go of the key that types `keysym`; with `level`, also the modifiers its
    /// level needs (Shift, AltGr) when they are not held already.
    fn keysym(&mut self, keysym: u32, down: bool, level: bool) -> Result<(), String> {
        let Some((code, mods)) = self.keys.find(keysym).cloned() else { return Ok(()) };
        let extra: Vec<u16> = if level { mods.iter().copied().filter(|m| !self.held.contains(m)).collect() } else { Vec::new() };
        if down {
            for &m in &extra {
                self.key(m, true)?;
            }
            self.key(code, true)?;
            if !extra.is_empty() {
                // A level's modifiers go with the key; held ones are the viewer's own.
                for &m in extra.iter().rev() {
                    self.key(m, false)?;
                }
            }
            Ok(())
        } else {
            self.key(code, false)
        }
    }

    pub fn input(&mut self, ev: Input) -> Result<(), String> {
        match ev {
            Input::Move { x, y } => {
                let (w, h) = self.size;
                let (x, y) = (x.clamp(0.0, w.saturating_sub(1) as f64) as i32, y.clamp(0.0, h.saturating_sub(1) as f64) as i32);
                send(&mut self.pointer, &[(EV_ABS, 0, x), (EV_ABS, 1, y)])
            }
            Input::MoveBy { dx, dy } => {
                if self.relative.is_none() {
                    self.relative = Some(device("SSH Remote mouse", &BUTTONS, &[REL_X, REL_Y], None)?);
                }
                send(self.relative.as_mut().expect("made above"), &[(EV_REL, REL_X, dx), (EV_REL, REL_Y, dy)])
            }
            Input::Button { button, pressed } => {
                let code = match button {
                    1 => BTN_LEFT,
                    2 => BTN_MIDDLE,
                    3 => BTN_RIGHT,
                    8 => BTN_SIDE,
                    9 => BTN_EXTRA,
                    _ => return Ok(()),
                };
                send(&mut self.pointer, &[(EV_KEY, code, i32::from(pressed))])
            }
            Input::Scroll { steps } => send(&mut self.pointer, &[(EV_REL, REL_WHEEL, -steps)]),
            Input::HScroll { steps } => send(&mut self.pointer, &[(EV_REL, REL_HWHEEL, steps)]),
            Input::KeyState { keysym, down } => self.keysym(keysym, down, down),
            Input::Key { keysym, mods } => {
                let held: Vec<u16> = [(MOD_SHIFT, KEY_LEFTSHIFT), (MOD_CTRL, KEY_LEFTCTRL), (MOD_ALT, KEY_LEFTALT), (MOD_SUPER, KEY_LEFTMETA)]
                    .into_iter()
                    .filter(|(m, _)| mods & m != 0)
                    .map(|(_, k)| k)
                    .collect();
                for &k in &held {
                    self.key(k, true)?;
                }
                // With Shift already held, the key's own level is what the viewer asked for.
                let r = self.keysym(keysym, true, mods & MOD_SHIFT == 0).and_then(|_| self.keysym(keysym, false, false));
                for &k in held.iter().rev() {
                    self.key(k, false)?;
                }
                r
            }
            // The kernel or a session manager may take Ctrl+Alt+Del to restart the computer.
            Input::SecureAttention => Ok(()),
        }
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        // Nothing stays pressed in the devices' last moments.
        for code in std::mem::take(&mut self.held) {
            let _ = send(&mut self.keyboard, &[(EV_KEY, code, 0)]);
        }
    }
}

/// Where each keysym is: its key and the modifiers its level takes.
struct Keymap(HashMap<u32, (u16, Vec<u16>)>);

impl Keymap {
    fn find(&self, keysym: u32) -> Option<&(u16, Vec<u16>)> {
        // A capital letter the layout has only as Shift + its small one.
        self.0.get(&keysym).or_else(|| self.0.get(&lower(keysym)))
    }

    /// The layout the login screen uses (systemd's locale settings), through libxkbcommon;
    /// a US keyboard without it.
    fn system() -> Keymap {
        let names = layout().unwrap_or_default();
        xkb_keymap(&names).unwrap_or_else(|e| {
            eprintln!("sshremote: keys as on a US keyboard ({e})");
            Keymap::us()
        })
    }

    fn us() -> Keymap {
        let mut m = HashMap::new();
        for &(code, base, shifted) in crate::capture::wlinput::US {
            m.entry(base).or_insert((code as u16, Vec::new()));
            if shifted != 0 {
                m.entry(shifted).or_insert((code as u16, vec![KEY_LEFTSHIFT]));
            }
        }
        Keymap(m)
    }
}

fn lower(keysym: u32) -> u32 {
    match keysym {
        0x41..=0x5a => keysym + 0x20,
        k => k,
    }
}

/// The keyboard layout systemd keeps for the console and the login screen: (layout, variant,
/// model, options).
#[derive(Default)]
struct Names {
    layout: String,
    variant: String,
    model: String,
    options: String,
}

fn layout() -> Option<Names> {
    zbus::block_on(async {
        let conn = zbus::Connection::system().await.ok()?;
        let p = zbus::Proxy::new(&conn, "org.freedesktop.locale1", "/org/freedesktop/locale1", "org.freedesktop.locale1").await.ok()?;
        let mut v = Vec::new();
        for n in ["X11Layout", "X11Variant", "X11Model", "X11Options"] {
            v.push(p.get_property::<String>(n).await.unwrap_or_default());
        }
        let [layout, variant, model, options] = <[String; 4]>::try_from(v).ok()?;
        Some(Names { layout, variant, model, options })
    })
}

#[repr(C)]
struct RuleNames {
    rules: *const libc::c_char,
    model: *const libc::c_char,
    layout: *const libc::c_char,
    variant: *const libc::c_char,
    options: *const libc::c_char,
}

/// Every keysym of the first layout (group) with its key and level modifiers, through
/// libxkbcommon loaded at run time.
fn xkb_keymap(names: &Names) -> Result<Keymap, String> {
    type Ctx = *mut libc::c_void;
    let lib = ["libxkbcommon.so.0", "libxkbcommon.so"]
        .iter()
        .find_map(|n| {
            let c = std::ffi::CString::new(*n).ok()?;
            // SAFETY: loading a system library by its soname.
            let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            (!h.is_null()).then_some(h)
        })
        .ok_or("libxkbcommon is not installed")?;
    macro_rules! sym {
        ($name:literal, $t:ty) => {{
            // SAFETY: the symbol has this C signature (libxkbcommon 1.0 and later).
            let p = unsafe { libc::dlsym(lib, concat!($name, "\0").as_ptr() as *const libc::c_char) };
            if p.is_null() {
                return Err(format!("libxkbcommon lacks {}", $name));
            }
            // SAFETY: as above.
            unsafe { std::mem::transmute::<*mut libc::c_void, $t>(p) }
        }};
    }
    let context_new = sym!("xkb_context_new", unsafe extern "C" fn(i32) -> Ctx);
    let from_names = sym!("xkb_keymap_new_from_names", unsafe extern "C" fn(Ctx, *const RuleNames, i32) -> Ctx);
    let min_kc = sym!("xkb_keymap_min_keycode", unsafe extern "C" fn(Ctx) -> u32);
    let max_kc = sym!("xkb_keymap_max_keycode", unsafe extern "C" fn(Ctx) -> u32);
    let levels = sym!("xkb_keymap_num_levels_for_key", unsafe extern "C" fn(Ctx, u32, u32) -> u32);
    let syms_by_level = sym!("xkb_keymap_key_get_syms_by_level", unsafe extern "C" fn(Ctx, u32, u32, u32, *mut *const u32) -> i32);
    let mods_for_level = sym!("xkb_keymap_key_get_mods_for_level", unsafe extern "C" fn(Ctx, u32, u32, u32, *mut u32, usize) -> usize);
    let mod_index = sym!("xkb_keymap_mod_get_index", unsafe extern "C" fn(Ctx, *const libc::c_char) -> u32);
    let keymap_unref = sym!("xkb_keymap_unref", unsafe extern "C" fn(Ctx));
    let context_unref = sym!("xkb_context_unref", unsafe extern "C" fn(Ctx));
    let c = |s: &str| std::ffi::CString::new(s).unwrap_or_default();
    let (layout, variant, model, options) = (c(&names.layout), c(&names.variant), c(&names.model), c(&names.options));
    let opt = |s: &std::ffi::CString| if s.as_bytes().is_empty() { std::ptr::null() } else { s.as_ptr() };
    let rn = RuleNames { rules: std::ptr::null(), model: opt(&model), layout: opt(&layout), variant: opt(&variant), options: opt(&options) };
    // SAFETY: libxkbcommon calls with the objects it made, freed at the end.
    unsafe {
        let ctx = context_new(0);
        if ctx.is_null() {
            return Err("no xkb context".into());
        }
        let km = from_names(ctx, &rn, 0);
        if km.is_null() {
            context_unref(ctx);
            return Err(format!("no keymap for layout '{}'", names.layout));
        }
        // Modifier bits to the keys that set them; Caps Lock and others are not used to reach a level.
        let bit = |name: &str| {
            let i = mod_index(km, c(name).as_ptr());
            if i < 32 { 1u32 << i } else { 0 }
        };
        let to_keys = [(bit("Shift"), KEY_LEFTSHIFT), (bit("Mod5"), KEY_RIGHTALT), (bit("LevelThree"), KEY_RIGHTALT), (bit("Control"), KEY_LEFTCTRL), (bit("Mod1"), KEY_LEFTALT)];
        let mut map: HashMap<u32, (u16, Vec<u16>)> = HashMap::new();
        for kc in min_kc(km)..=max_kc(km).min(255 + 8) {
            if kc < 9 {
                continue;
            }
            for level in 0..levels(km, kc, 0) {
                let mut syms: *const u32 = std::ptr::null();
                let n = syms_by_level(km, kc, 0, level, &mut syms);
                if n <= 0 || syms.is_null() {
                    continue;
                }
                let mut masks = [0u32; 8];
                let got = mods_for_level(km, kc, 0, level, masks.as_mut_ptr(), masks.len());
                // The simplest way to the level: the fewest modifiers, none that cannot be pressed.
                let Some(keys) = masks[..got.min(8)]
                    .iter()
                    .filter_map(|&m| {
                        let mut left = m;
                        let mut keys = Vec::new();
                        for &(b, k) in &to_keys {
                            if b != 0 && left & b != 0 {
                                left &= !b;
                                if !keys.contains(&k) {
                                    keys.push(k);
                                }
                            }
                        }
                        (left == 0).then_some(keys)
                    })
                    .min_by_key(|k| k.len())
                else {
                    continue;
                };
                for &s in std::slice::from_raw_parts(syms, n as usize) {
                    let better = map.get(&s).is_none_or(|(_, have)| keys.len() < have.len());
                    if better {
                        map.insert(s, ((kc - 8) as u16, keys.clone()));
                    }
                }
            }
        }
        keymap_unref(km);
        context_unref(ctx);
        if map.is_empty() {
            return Err("the keymap has no keys".into());
        }
        Ok(Keymap(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_structs_are_the_kernels_size() {
        assert_eq!(std::mem::size_of::<Setup>(), 92);
        assert_eq!(std::mem::size_of::<AbsSetup>(), 28);
        assert_eq!(std::mem::size_of::<Event>(), 24);
        assert_eq!(UI_DEV_SETUP, 0x405c5503);
        assert_eq!(UI_SET_KEYBIT, 0x40045565);
    }

    #[test]
    fn a_layout_puts_keys_where_it_has_them() {
        let Ok(de) = xkb_keymap(&Names { layout: "de".into(), ..Default::default() }) else { return };
        // A German keyboard: z and y trade places, @ is AltGr+q.
        assert_eq!(de.find('z' as u32).map(|k| k.0), Some(21));
        assert_eq!(de.find('y' as u32).map(|k| k.0), Some(44));
        assert_eq!(de.find('@' as u32), Some(&(16, vec![KEY_RIGHTALT])));
        assert_eq!(de.find('Z' as u32).map(|k| k.1.clone()), Some(vec![KEY_LEFTSHIFT]));
        let us = Keymap::us();
        assert_eq!(us.find('@' as u32), Some(&(3, vec![KEY_LEFTSHIFT])));
    }
}
