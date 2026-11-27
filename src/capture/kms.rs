//! The console's screen as the display hardware shows it (DRM/KMS), for remote access running as
//! root: the login screen, a locked screen or any desktop, whichever compositor draws it. Linear
//! buffers are read as they are; tiled or compressed ones go through the GPU (`gl`). The pointer
//! is read from its own plane, and input goes in through virtual devices (`uinput`).

use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use super::{Capture, Image, Input, Monitor, Pointer, Shrink};

#[path = "kms_gl.rs"]
mod gl;
#[path = "uinput.rs"]
mod uinput;

const fn ioc(dir: u32, ty: u32, nr: u32, size: usize) -> libc::c_ulong {
    ((dir << 30) | ((size as u32) << 16) | (ty << 8) | nr) as libc::c_ulong
}
const fn drm_rw<T>(nr: u32) -> libc::c_ulong {
    ioc(3, 0x64, nr, std::mem::size_of::<T>())
}
const fn drm_w<T>(nr: u32) -> libc::c_ulong {
    ioc(1, 0x64, nr, std::mem::size_of::<T>())
}

#[repr(C)]
#[derive(Default)]
struct CardRes {
    fb_ptr: u64,
    crtc_ptr: u64,
    connector_ptr: u64,
    encoder_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ModeInfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    kind: u32,
    name: [u8; 32],
}

#[repr(C)]
#[derive(Default)]
struct Crtc {
    set_connectors_ptr: u64,
    count_connectors: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: ModeInfo,
}

#[repr(C)]
#[derive(Default)]
struct Fb2 {
    fb_id: u32,
    width: u32,
    height: u32,
    pixel_format: u32,
    flags: u32,
    handles: [u32; 4],
    pitches: [u32; 4],
    offsets: [u32; 4],
    modifier: [u64; 4],
}

#[repr(C)]
#[derive(Default)]
struct PrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

#[repr(C)]
#[derive(Default)]
struct GemClose {
    handle: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct ClientCap {
    capability: u64,
    value: u64,
}

#[repr(C)]
#[derive(Default)]
struct PlaneRes {
    plane_id_ptr: u64,
    count_planes: u32,
}

#[repr(C)]
#[derive(Default)]
struct Plane {
    plane_id: u32,
    crtc_id: u32,
    fb_id: u32,
    possible_crtcs: u32,
    gamma_size: u32,
    count_format_types: u32,
    format_type_ptr: u64,
}

#[repr(C)]
#[derive(Default)]
struct ObjProps {
    props_ptr: u64,
    prop_values_ptr: u64,
    count_props: u32,
    obj_id: u32,
    obj_type: u32,
}

#[repr(C)]
#[derive(Default)]
struct Prop {
    values_ptr: u64,
    enum_blob_ptr: u64,
    prop_id: u32,
    flags: u32,
    name: [u8; 32],
    count_values: u32,
    count_enum_blobs: u32,
}

#[repr(C)]
#[derive(Default)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
#[derive(Default)]
struct Connector {
    encoders_ptr: u64,
    modes_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct Encoder {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

const GET_RESOURCES: libc::c_ulong = drm_rw::<CardRes>(0xA0);
const GET_CRTC: libc::c_ulong = drm_rw::<Crtc>(0xA1);
const GET_ENCODER: libc::c_ulong = drm_rw::<Encoder>(0xA6);
const GET_CONNECTOR: libc::c_ulong = drm_rw::<Connector>(0xA7);
const GET_PROPERTY: libc::c_ulong = drm_rw::<Prop>(0xAA);
const MAP_DUMB: libc::c_ulong = drm_rw::<MapDumb>(0xB3);
const GET_PLANE_RESOURCES: libc::c_ulong = drm_rw::<PlaneRes>(0xB5);
const GET_PLANE: libc::c_ulong = drm_rw::<Plane>(0xB6);
const OBJ_GET_PROPERTIES: libc::c_ulong = drm_rw::<ObjProps>(0xB9);
const GET_FB2: libc::c_ulong = drm_rw::<Fb2>(0xCE);
const PRIME_HANDLE_TO_FD: libc::c_ulong = drm_rw::<PrimeHandle>(0x2D);
const GEM_CLOSE: libc::c_ulong = drm_w::<GemClose>(0x09);
const SET_CLIENT_CAP: libc::c_ulong = drm_w::<ClientCap>(0x0D);
const DMA_BUF_SYNC: libc::c_ulong = ioc(1, b'b' as u32, 0, 8);

const OBJECT_PLANE: u32 = 0xeeee_eeee;
const FB_MODIFIERS: u32 = 2;
const MOD_LINEAR: u64 = 0;
const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

pub(super) const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
pub(super) const ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
pub(super) const XBGR8888: u32 = u32::from_le_bytes(*b"XB24");
pub(super) const ABGR8888: u32 = u32::from_le_bytes(*b"AB24");
const XRGB2101010: u32 = u32::from_le_bytes(*b"XR30");
const ARGB2101010: u32 = u32::from_le_bytes(*b"AR30");
const XBGR2101010: u32 = u32::from_le_bytes(*b"XB30");
const ABGR2101010: u32 = u32::from_le_bytes(*b"AB30");
const RGB565: u32 = u32::from_le_bytes(*b"RG16");

fn ioctl<T>(fd: &File, req: libc::c_ulong, arg: &mut T) -> std::io::Result<()> {
    loop {
        // SAFETY: `arg` is the #[repr(C)] struct the request is sized for.
        let r = unsafe { libc::ioctl(fd.as_raw_fd(), req as _, arg as *mut T) };
        if r == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if !matches!(e.raw_os_error(), Some(libc::EINTR) | Some(libc::EAGAIN)) {
            return Err(e);
        }
    }
}

/// A list the kernel fills in two calls: how many first, then the items.
fn ids(n: u32) -> Vec<u32> {
    vec![0u32; n as usize]
}

/// A display output in use: its CRTC (what scans a framebuffer out) and its connector's name.
#[derive(Clone, Debug)]
struct Output {
    crtc: u32,
    /// Its index among the card's CRTCs (planes say which they can serve by these bits).
    index: u32,
    name: String,
    w: usize,
    h: usize,
}

/// A buffer the kernel exported, mapped to be read.
struct Mapped {
    ptr: *mut libc::c_void,
    len: usize,
    dmabuf: Option<OwnedFd>,
}

impl Mapped {
    fn bytes(&self) -> &[u8] {
        // SAFETY: mapped readable for `len` bytes until dropped.
        unsafe { std::slice::from_raw_parts(self.ptr as *const u8, self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        if let Some(fd) = &self.dmabuf {
            let mut flags: u64 = 4 | 1; // DMA_BUF_SYNC_END | READ
            // SAFETY: an ioctl on our own dma-buf fd with its u64 argument.
            unsafe { libc::ioctl(fd.as_raw_fd(), DMA_BUF_SYNC as _, &mut flags) };
        }
        // SAFETY: what mmap gave.
        unsafe { libc::munmap(self.ptr, self.len) };
    }
}

pub struct Card {
    file: File,
    path: String,
    /// Plane property ids by name, read once.
    prop_names: std::collections::HashMap<u32, String>,
}

impl Card {
    fn open(path: &str) -> Result<Card, String> {
        let file = std::fs::OpenOptions::new().read(true).write(true).open(path).map_err(|e| format!("{path}: {e}"))?;
        let card = Card { file, path: path.to_string(), prop_names: Default::default() };
        // Every plane, the pointer's too, and the properties only atomic clients see (its place);
        // virtual machines' GPUs show their pointer plane only to those that know its hot spot.
        for (cap, value) in [(2u64, 1u64), (3, 1), (6, 1)] {
            let _ = ioctl(&card.file, SET_CLIENT_CAP, &mut ClientCap { capability: cap, value });
        }
        Ok(card)
    }

    fn outputs(&self) -> Result<Vec<Output>, String> {
        let mut res = CardRes::default();
        ioctl(&self.file, GET_RESOURCES, &mut res).map_err(|e| format!("{}: {e}", self.path))?;
        let (mut crtcs, mut conns, mut encs) = (ids(res.count_crtcs), ids(res.count_connectors), ids(res.count_encoders));
        let mut res2 = CardRes {
            crtc_ptr: crtcs.as_mut_ptr() as u64,
            count_crtcs: res.count_crtcs,
            connector_ptr: conns.as_mut_ptr() as u64,
            count_connectors: res.count_connectors,
            encoder_ptr: encs.as_mut_ptr() as u64,
            count_encoders: res.count_encoders,
            ..Default::default()
        };
        ioctl(&self.file, GET_RESOURCES, &mut res2).map_err(|e| format!("{}: {e}", self.path))?;
        crtcs.truncate(res2.count_crtcs.min(res.count_crtcs) as usize);
        conns.truncate(res2.count_connectors.min(res.count_connectors) as usize);
        // Which connector each CRTC drives, for its name.
        let mut names = std::collections::HashMap::new();
        for &c in &conns {
            let mut k = Connector { connector_id: c, ..Default::default() };
            if ioctl(&self.file, GET_CONNECTOR, &mut k).is_err() || k.connection != 1 || k.encoder_id == 0 {
                continue;
            }
            let mut e = Encoder { encoder_id: k.encoder_id, ..Default::default() };
            if ioctl(&self.file, GET_ENCODER, &mut e).is_ok() && e.crtc_id != 0 {
                names.insert(e.crtc_id, format!("{}-{}", connector_kind(k.connector_type), k.connector_type_id));
            }
        }
        let mut out = Vec::new();
        for (index, &id) in crtcs.iter().enumerate() {
            let mut c = Crtc { crtc_id: id, ..Default::default() };
            if ioctl(&self.file, GET_CRTC, &mut c).is_err() || c.mode_valid == 0 || c.fb_id == 0 {
                continue;
            }
            let name = names.get(&id).cloned().unwrap_or_else(|| format!("CRTC-{id}"));
            out.push(Output { crtc: id, index: index as u32, name, w: c.mode.hdisplay as usize, h: c.mode.vdisplay as usize });
        }
        Ok(out)
    }

    /// What `crtc` scans out now: its framebuffer.
    fn crtc_fb(&self, crtc: u32) -> Result<u32, String> {
        let mut c = Crtc { crtc_id: crtc, ..Default::default() };
        ioctl(&self.file, GET_CRTC, &mut c).map_err(|e| format!("{}: {e}", self.path))?;
        Ok(c.fb_id)
    }

    fn fb(&self, id: u32) -> Result<Fb2, String> {
        let mut f = Fb2 { fb_id: id, ..Default::default() };
        ioctl(&self.file, GET_FB2, &mut f).map_err(|e| format!("reading framebuffer {id}: {e}"))?;
        if f.handles[0] == 0 {
            close_handles(self, &f);
            return Err("the screen can only be read with root's rights (CAP_SYS_ADMIN)".into());
        }
        Ok(f)
    }

    fn prime(&self, handle: u32) -> Result<OwnedFd, String> {
        let mut p = PrimeHandle { handle, flags: (libc::O_CLOEXEC | libc::O_RDWR) as u32, fd: -1 };
        if ioctl(&self.file, PRIME_HANDLE_TO_FD, &mut p).is_err() {
            p.flags = libc::O_CLOEXEC as u32;
            ioctl(&self.file, PRIME_HANDLE_TO_FD, &mut p).map_err(|e| format!("exporting the screen's buffer: {e}"))?;
        }
        // SAFETY: a new fd the kernel just gave us.
        Ok(unsafe { OwnedFd::from_raw_fd(p.fd) })
    }

    /// Maps plane 0 of a linear framebuffer: through its dma-buf, or as a dumb buffer.
    fn map(&self, f: &Fb2) -> Result<Mapped, String> {
        let len = f.offsets[0] as usize + f.pitches[0] as usize * f.height as usize;
        if let Ok(fd) = self.prime(f.handles[0]) {
            // SAFETY: a read-only shared mapping of the exported buffer.
            let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, fd.as_raw_fd(), 0) };
            if ptr != libc::MAP_FAILED {
                let mut flags: u64 = 1; // DMA_BUF_SYNC_START | READ
                // SAFETY: as in Mapped::drop.
                unsafe { libc::ioctl(fd.as_raw_fd(), DMA_BUF_SYNC as _, &mut flags) };
                return Ok(Mapped { ptr, len, dmabuf: Some(fd) });
            }
        }
        let mut m = MapDumb { handle: f.handles[0], ..Default::default() };
        ioctl(&self.file, MAP_DUMB, &mut m).map_err(|e| format!("mapping the screen's buffer: {e}"))?;
        // SAFETY: the offset the kernel gave for this buffer on the card's fd.
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, self.file.as_raw_fd(), m.offset as libc::off_t) };
        if ptr == libc::MAP_FAILED {
            return Err(format!("mapping the screen's buffer: {}", std::io::Error::last_os_error()));
        }
        Ok(Mapped { ptr, len, dmabuf: None })
    }

    fn props(&self, obj: u32, kind: u32) -> Vec<(u32, u64)> {
        let mut p = ObjProps { obj_id: obj, obj_type: kind, ..Default::default() };
        if ioctl(&self.file, OBJ_GET_PROPERTIES, &mut p).is_err() || p.count_props == 0 {
            return Vec::new();
        }
        let n = p.count_props;
        let (mut ids, mut values) = (vec![0u32; n as usize], vec![0u64; n as usize]);
        let mut p2 = ObjProps { props_ptr: ids.as_mut_ptr() as u64, prop_values_ptr: values.as_mut_ptr() as u64, count_props: n, obj_id: obj, obj_type: kind };
        if ioctl(&self.file, OBJ_GET_PROPERTIES, &mut p2).is_err() {
            return Vec::new();
        }
        let n = p2.count_props.min(n) as usize;
        ids.into_iter().zip(values).take(n).collect()
    }

    fn prop_name(&mut self, id: u32) -> String {
        if let Some(n) = self.prop_names.get(&id) {
            return n.clone();
        }
        let mut p = Prop { prop_id: id, ..Default::default() };
        let name = if ioctl(&self.file, GET_PROPERTY, &mut p).is_ok() {
            String::from_utf8_lossy(&p.name).trim_end_matches('\0').to_string()
        } else {
            String::new()
        };
        self.prop_names.insert(id, name.clone());
        name
    }

    /// A plane's properties by name.
    fn named_props(&mut self, plane: u32) -> std::collections::HashMap<String, u64> {
        self.props(plane, OBJECT_PLANE).into_iter().map(|(id, v)| (self.prop_name(id), v)).collect()
    }

    fn planes(&self) -> Vec<u32> {
        let mut r = PlaneRes::default();
        if ioctl(&self.file, GET_PLANE_RESOURCES, &mut r).is_err() || r.count_planes == 0 {
            return Vec::new();
        }
        let mut v = ids(r.count_planes);
        let mut r2 = PlaneRes { plane_id_ptr: v.as_mut_ptr() as u64, count_planes: r.count_planes };
        if ioctl(&self.file, GET_PLANE_RESOURCES, &mut r2).is_err() {
            return Vec::new();
        }
        v.truncate(r2.count_planes.min(r.count_planes) as usize);
        v
    }

    fn plane(&self, id: u32) -> Option<Plane> {
        let mut p = Plane { plane_id: id, ..Default::default() };
        ioctl(&self.file, GET_PLANE, &mut p).ok().map(|_| p)
    }
}

fn close_handles(card: &Card, f: &Fb2) {
    let mut done = Vec::new();
    for &h in &f.handles {
        if h != 0 && !done.contains(&h) {
            done.push(h);
            let _ = ioctl(&card.file, GEM_CLOSE, &mut GemClose { handle: h, pad: 0 });
        }
    }
}

fn connector_kind(t: u32) -> &'static str {
    const KINDS: [&str; 21] = [
        "Unknown", "VGA", "DVI-I", "DVI-D", "DVI-A", "Composite", "SVIDEO", "LVDS", "Component", "DIN", "DP", "HDMI-A", "HDMI-B", "TV",
        "eDP", "Virtual", "DSI", "DPI", "Writeback", "SPI", "USB",
    ];
    KINDS.get(t as usize).copied().unwrap_or("Unknown")
}

/// Rows of a framebuffer in `fourcc` to BGRx, `w` pixels each.
pub(super) fn to_bgrx(src: &[u8], pitch: usize, fourcc: u32, w: usize, h: usize, out: &mut Vec<u8>) -> Result<(), String> {
    out.resize(w * h * 4, 0);
    let bpp = if fourcc == RGB565 { 2 } else { 4 };
    if src.len() < pitch * (h.max(1) - 1) + w * bpp {
        return Err("the screen's buffer is smaller than its size says".into());
    }
    for y in 0..h {
        let row = &src[y * pitch..y * pitch + w * bpp];
        let dst = &mut out[y * w * 4..(y + 1) * w * 4];
        match fourcc {
            XRGB8888 | ARGB8888 => dst.copy_from_slice(row),
            XBGR8888 | ABGR8888 => {
                for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(row.as_chunks::<4>().0) {
                    *d = [s[2], s[1], s[0], 255];
                }
            }
            XRGB2101010 | ARGB2101010 | XBGR2101010 | ABGR2101010 => {
                let bgr = matches!(fourcc, XBGR2101010 | ABGR2101010);
                for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(row.as_chunks::<4>().0) {
                    let v = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                    let (hi, mid, lo) = (((v >> 20) & 0x3ff) >> 2, ((v >> 10) & 0x3ff) >> 2, (v & 0x3ff) >> 2);
                    let (r, b) = if bgr { (lo, hi) } else { (hi, lo) };
                    *d = [b as u8, mid as u8, r as u8, 255];
                }
            }
            RGB565 => {
                for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(row.as_chunks::<2>().0) {
                    let v = u16::from_le_bytes([s[0], s[1]]);
                    let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
                    *d = [(b << 3 | b >> 2) as u8, (g << 2 | g >> 4) as u8, (r << 3 | r >> 2) as u8, 255];
                }
            }
            other => return Err(format!("the screen is in a pixel format this program cannot read ({})", fourcc_name(other))),
        }
    }
    Ok(())
}

fn fourcc_name(f: u32) -> String {
    f.to_le_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect()
}

/// The pointer as its plane shows it.
#[derive(Default)]
struct Cursor {
    plane: Option<u32>,
    fb: u32,
    hot: (usize, usize),
    shape_sent: bool,
}

/// The first card with a screen in use, and its outputs; else the first card there is, with none.
fn find_card() -> Result<(Card, Vec<Output>), String> {
    let mut paths: Vec<String> = std::fs::read_dir("/dev/dri")
        .map_err(|e| format!("/dev/dri: {e}"))?
        .flatten()
        .map(|e| e.path().display().to_string())
        .filter(|p| p.rsplit('/').next().is_some_and(|n| n.starts_with("card")))
        .collect();
    paths.sort();
    let mut first = None;
    let mut why = String::from("no display hardware (/dev/dri/card*) was found");
    for p in paths {
        match Card::open(&p).and_then(|c| c.outputs().map(|o| (c, o))) {
            Ok((c, o)) if !o.is_empty() => return Ok((c, o)),
            Ok((c, _)) => {
                if first.is_none() {
                    first = Some(c);
                }
            }
            Err(e) => why = e,
        }
    }
    first.map(|c| (c, Vec::new())).ok_or(why)
}

pub struct Kms {
    card: Card,
    outputs: Vec<Output>,
    current: usize,
    target: (usize, usize),
    shrink: Shrink,
    frame: Vec<u8>,
    last: Vec<u8>,
    last_at: Option<Instant>,
    fps: u8,
    gl: Option<Result<gl::Gl, String>>,
    cursor: Cursor,
    apart: bool,
    input: uinput::Devices,
}

impl Kms {
    pub fn open() -> Result<Kms, String> {
        let (card, mut outputs) = find_card()?;
        let mut input = uinput::Devices::open(outputs.first().map_or(1920, |o| o.w), outputs.first().map_or(1080, |o| o.h))?;
        // A locked screen turns its display off after a while: a key no one types wakes it.
        if outputs.is_empty() {
            input.wake()?;
            let until = Instant::now() + Duration::from_secs(4);
            while outputs.is_empty() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(100));
                outputs = card.outputs()?;
            }
            if outputs.is_empty() {
                return Err(t!("The computer's screen is off and did not wake").into());
            }
            input.resize(outputs[0].w, outputs[0].h)?;
        }
        let (w, h) = (outputs[0].w, outputs[0].h);
        // Readable at all? A framebuffer read without root's rights has no handles.
        let fb = card.crtc_fb(outputs[0].crtc)?;
        let f = card.fb(fb)?;
        close_handles(&card, &f);
        let mut k = Kms {
            card,
            outputs,
            current: 0,
            target: (w, h),
            shrink: Shrink::default(),
            frame: Vec::new(),
            last: Vec::new(),
            last_at: None,
            fps: 30,
            gl: None,
            cursor: Cursor::default(),
            apart: false,
            input,
        };
        k.find_cursor();
        Ok(k)
    }

    fn output(&self) -> &Output {
        &self.outputs[self.current.min(self.outputs.len() - 1)]
    }

    /// The pointer's plane on the output shown.
    fn find_cursor(&mut self) {
        let (crtc, bit) = (self.output().crtc, 1u32 << self.output().index);
        self.cursor = Cursor::default();
        for id in self.card.planes() {
            let Some(p) = self.card.plane(id) else { continue };
            if p.possible_crtcs & bit == 0 && p.crtc_id != crtc {
                continue;
            }
            if self.card.named_props(id).get("type") == Some(&2) {
                self.cursor.plane = Some(id);
                if p.crtc_id == crtc {
                    break;
                }
            }
        }
    }

    /// The screen now, BGRx in `self.frame`.
    fn read(&mut self) -> Result<(), String> {
        let o = self.output().clone();
        let fb = self.card.crtc_fb(o.crtc)?;
        if fb == 0 {
            return Err("the screen is off".into());
        }
        read_framebuffer(&self.card, &mut self.gl, fb, o.w, o.h, &mut self.frame)
    }
}

/// Framebuffer `fb` as BGRx rows into `out`, at most `w` x `h` of it: a linear one mapped and
/// read, a tiled or compressed one laid out plainly by the GPU (`gl`, opened when first needed).
fn read_framebuffer(card: &Card, gl: &mut Option<Result<gl::Gl, String>>, fb: u32, w: usize, h: usize, out: &mut Vec<u8>) -> Result<(), String> {
    let f = card.fb(fb)?;
    let modifier = if f.flags & FB_MODIFIERS != 0 { f.modifier[0] } else { MOD_LINEAR };
    let (w, h) = ((f.width as usize).min(w.max(1)), (f.height as usize).min(h.max(1)));
    // Tests take the GPU's way for any buffer, to try it where no buffer is tiled.
    let plain = (modifier == MOD_LINEAR || modifier == MOD_INVALID) && !(cfg!(test) && std::env::var_os("SSHREMOTE_KMS_GL").is_some());
    let read = if plain {
        card.map(&f).and_then(|m| to_bgrx(&m.bytes()[f.offsets[0] as usize..], f.pitches[0] as usize, f.pixel_format, w, h, out))
    } else {
        match gl.get_or_insert_with(|| gl::Gl::open(&card.path)) {
            Ok(gl) => {
                let mut fds = Vec::new();
                let mut planes = Vec::new();
                let res = (|| {
                    for i in 0..4 {
                        if f.handles[i] == 0 {
                            break;
                        }
                        let fd = card.prime(f.handles[i])?;
                        planes.push(gl::PlaneDesc { fd: fd.as_raw_fd(), offset: f.offsets[i], pitch: f.pitches[i], modifier: f.modifier[i] });
                        fds.push(fd);
                    }
                    gl.read(f.width, f.height, f.pixel_format, &planes, out).map(|_| {
                        if (w, h) != (f.width as usize, f.height as usize) {
                            crop(out, f.width as usize, w, h);
                        }
                    })
                })();
                drop(fds);
                res
            }
            Err(e) => Err(format!("the screen's buffer is tiled ({}) and the GPU could not read it: {e}", modifier_name(modifier))),
        }
    };
    close_handles(card, &f);
    read
}

fn crop(frame: &mut Vec<u8>, from_w: usize, w: usize, h: usize) {
    for y in 1..h {
        frame.copy_within(y * from_w * 4..y * from_w * 4 + w * 4, y * w * 4);
    }
    frame.truncate(w * h * 4);
}

fn modifier_name(m: u64) -> String {
    let vendor = match m >> 56 {
        1 => "Intel",
        2 => "AMD",
        3 => "NVIDIA",
        4 => "Samsung",
        5 => "Qualcomm",
        6 => "Vivante",
        7 => "Broadcom",
        8 => "Arm",
        9 => "Allwinner",
        10 => "Amlogic",
        _ => "vendor",
    };
    format!("{vendor} 0x{m:016x}")
}

impl Capture for Kms {
    fn name(&self) -> &'static str {
        "kms"
    }

    fn monitors(&self) -> Vec<Monitor> {
        self.outputs.iter().enumerate().map(|(i, o)| Monitor { name: o.name.clone(), w: o.w, h: o.h, primary: i == 0, at: None }).collect()
    }

    fn current(&self) -> usize {
        self.current
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        if index >= self.outputs.len() {
            return Err(format!("there is no screen {}", index + 1));
        }
        self.current = index;
        self.last.clear();
        let o = self.output().clone();
        self.input.resize(o.w, o.h)?;
        self.find_cursor();
        Ok(())
    }

    fn screen_id(&self) -> Option<String> {
        Some(self.output().name.clone())
    }

    fn screens_changed(&mut self) {
        if let Ok(o) = self.card.outputs()
            && !o.is_empty()
        {
            self.outputs = o;
            self.current = self.current.min(self.outputs.len() - 1);
            let o = self.output().clone();
            let _ = self.input.resize(o.w, o.h);
            self.find_cursor();
        }
    }

    fn desktop_size(&self) -> (usize, usize) {
        (self.output().w, self.output().h)
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.target = (w.max(1), h.max(1));
        self.last.clear();
        Ok(())
    }

    fn set_fps(&mut self, fps: u8) {
        self.fps = fps.max(1);
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        let gap = Duration::from_secs(1) / u32::from(self.fps);
        if self.last_at.is_some_and(|t| t.elapsed() < gap) {
            return Ok(None);
        }
        self.last_at = Some(Instant::now());
        // The outputs may have changed (a monitor off, a new mode at the login screen); while
        // the display sleeps the last picture stays, until input wakes it.
        let o = self.output().clone();
        if self.card.crtc_fb(o.crtc).is_ok_and(|fb| fb == 0) {
            self.screens_changed();
            if self.card.crtc_fb(self.output().crtc).is_ok_and(|fb| fb == 0) {
                return Ok(None);
            }
        }
        self.read()?;
        // A screen that did not change is not sent again.
        if self.frame == self.last {
            return Ok(None);
        }
        std::mem::swap(&mut self.frame, &mut self.last);
        let (w, h) = (self.output().w.min(self.last.len() / 4 / self.output().h.max(1)), self.output().h);
        Ok(Some(self.shrink.image(&self.last, w, h, w * 4, self.target.0, self.target.1)))
    }

    fn pointer_apart(&mut self, on: bool) -> bool {
        self.apart = on && self.cursor.plane.is_some();
        self.cursor.shape_sent = false;
        self.apart
    }

    fn pointer(&mut self) -> (Option<Pointer>, Option<(i32, i32)>) {
        let Some(id) = self.cursor.plane else { return (None, None) };
        let Some(p) = self.card.plane(id) else { return (None, None) };
        if p.fb_id == 0 || p.crtc_id != self.output().crtc {
            return (None, None);
        }
        let props = self.card.named_props(id);
        let mut shape = None;
        if p.fb_id != self.cursor.fb || !self.cursor.shape_sent {
            self.cursor.fb = p.fb_id;
            self.cursor.shape_sent = true;
            if let Some(mut s) = self.cursor_shape(p.fb_id) {
                // Drivers for virtual machines say where the pointer points; others leave it to
                // the picture: the top-left of what is drawn.
                s.hot = match (props.get("HOTSPOT_X"), props.get("HOTSPOT_Y")) {
                    (Some(&x), Some(&y)) if x > 0 || y > 0 => ((x as usize).min(s.w.saturating_sub(1)), (y as usize).min(s.h.saturating_sub(1))),
                    _ => tip(&s),
                };
                self.cursor.hot = s.hot;
                shape = Some(s);
            }
        }
        let signed = |v: Option<&u64>| v.map_or(0, |&v| v as i64 as i32);
        let (x, y) = (signed(props.get("CRTC_X")), signed(props.get("CRTC_Y")));
        let at = (x + self.cursor.hot.0 as i32, y + self.cursor.hot.1 as i32);
        // In the picture's pixels when it is shown smaller.
        let o = self.output();
        let at = ((at.0 as i64 * self.target.0 as i64 / o.w.max(1) as i64) as i32, (at.1 as i64 * self.target.1 as i64 / o.h.max(1) as i64) as i32);
        (shape, Some(at))
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        let (w, h) = (self.output().w, self.output().h);
        let ev = match ev {
            // The viewer's points are the picture's; the devices take the screen's.
            Input::Move { x, y } => Input::Move { x: x * w as f64 / self.target.0 as f64, y: y * h as f64 / self.target.1 as f64 },
            ev => ev,
        };
        self.input.input(ev)
    }
}

impl Kms {
    fn cursor_shape(&mut self, fb: u32) -> Option<Pointer> {
        let f = self.card.fb(fb).ok()?;
        let (w, h) = (f.width as usize, f.height as usize);
        let mut bgrx = Vec::new();
        let ok = matches!(f.pixel_format, ARGB8888 | ABGR8888)
            && self.card.map(&f).ok().is_some_and(|m| {
                let rows = &m.bytes()[f.offsets[0] as usize..];
                bgrx.resize(w * h * 4, 0);
                for y in 0..h {
                    let from = y * f.pitches[0] as usize;
                    if let (Some(src), Some(dst)) = (rows.get(from..from + w * 4), bgrx.get_mut(y * w * 4..(y + 1) * w * 4)) {
                        dst.copy_from_slice(src);
                    }
                }
                true
            });
        close_handles(&self.card, &f);
        if !ok {
            return None;
        }
        let swap = f.pixel_format == ARGB8888;
        // Premultiplied as the hardware blends it; the viewer wants it straight.
        let rgba = bgrx
            .as_chunks::<4>().0.iter()
            .flat_map(|p| {
                let (b, g, r, a) = if swap { (p[0], p[1], p[2], p[3]) } else { (p[2], p[1], p[0], p[3]) };
                let un = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 };
                [un(r), un(g), un(b), a]
            })
            .collect();
        Some(Pointer { w, h, hot: (0, 0), rgba })
    }
}

/// Where an arrow points: its first drawn pixel from the top.
fn tip(p: &Pointer) -> (usize, usize) {
    for y in 0..p.h {
        for x in 0..p.w {
            if p.rgba[(y * p.w + x) * 4 + 3] > 0x40 {
                return (x, y);
            }
        }
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_structs_are_the_kernels_size() {
        assert_eq!(std::mem::size_of::<CardRes>(), 64);
        assert_eq!(std::mem::size_of::<Crtc>(), 104);
        assert_eq!(std::mem::size_of::<Fb2>(), 104);
        assert_eq!(std::mem::size_of::<PrimeHandle>(), 12);
        assert_eq!(std::mem::size_of::<PlaneRes>(), 16);
        assert_eq!(std::mem::size_of::<Plane>(), 32);
        assert_eq!(std::mem::size_of::<ObjProps>(), 32);
        assert_eq!(std::mem::size_of::<Prop>(), 64);
        assert_eq!(std::mem::size_of::<Connector>(), 80);
        assert_eq!(std::mem::size_of::<Encoder>(), 20);
        assert_eq!(GET_FB2, 0xC06864CE);
        assert_eq!(GET_RESOURCES, 0xC04064A0);
    }

    /// As root on a computer with a screen (a VM at its login screen): the screen into
    /// SSHREMOTE_KMS_TEST (a PNG), then SSHREMOTE_KMS_DO: "click X Y", "type TEXT", "enter",
    /// "wait MS", "shot FILE", separated by ";".
    #[test]
    #[ignore]
    fn the_console_is_read_and_typed_into() {
        let Ok(out) = std::env::var("SSHREMOTE_KMS_TEST") else { return };
        let mut k = Kms::open().unwrap();
        for id in k.card.planes() {
            let p = k.card.plane(id).unwrap();
            let props = k.card.named_props(id);
            eprintln!("plane {id}: crtc {} fb {} possible {:b} props {:?}", p.crtc_id, p.fb_id, p.possible_crtcs, props);
        }
        let apart = k.pointer_apart(true);
        eprintln!("outputs {:?}, cursor plane {:?}, apart {apart}", k.outputs, k.cursor.plane);
        let shot = |k: &mut Kms, file: &str| {
            k.last.clear();
            k.last_at = None;
            let (w, h) = k.desktop_size();
            k.set_target(w, h).unwrap();
            let img = k.grab().unwrap().expect("a picture");
            std::fs::write(file, crate::shot::png_from_bgrx(&img).unwrap()).unwrap();
            let (shape, at) = k.pointer();
            eprintln!("{file}: {}x{}, pointer at {at:?}, shape {:?}", img.w, img.h, shape.map(|s| (s.w, s.h, s.hot)));
        };
        shot(&mut k, &out);
        for step in std::env::var("SSHREMOTE_KMS_DO").unwrap_or_default().split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let (verb, rest) = step.split_once(' ').unwrap_or((step, ""));
            match verb {
                "click" => {
                    let (x, y) = rest.split_once(' ').unwrap();
                    k.input(Input::Move { x: x.parse().unwrap(), y: y.parse().unwrap() }).unwrap();
                    std::thread::sleep(Duration::from_millis(100));
                    k.input(Input::Button { button: 1, pressed: true }).unwrap();
                    k.input(Input::Button { button: 1, pressed: false }).unwrap();
                }
                "type" => {
                    for c in rest.chars() {
                        k.input(Input::Key { keysym: c as u32, mods: 0 }).unwrap();
                        std::thread::sleep(Duration::from_millis(30));
                    }
                }
                "enter" => k.input(Input::Key { keysym: 0xff0d, mods: 0 }).unwrap(),
                "wait" => std::thread::sleep(Duration::from_millis(rest.parse().unwrap())),
                "shot" => {
                    shot(&mut k, rest);
                    eprintln!("console: {:?}", crate::console::now());
                }
                other => panic!("{other}?"),
            }
        }
    }

    /// As root where there is a GPU (SSHREMOTE_KMS_GBM=/dev/dri/card0): framebuffers of each kind
    /// the GPU can scan out (plain, tiled, compressed) are made with GBM, filled with a pattern,
    /// added as framebuffers and read back the way the screen is.
    #[test]
    #[ignore]
    fn the_gpus_own_framebuffers_read_back_as_they_were() {
        let Ok(path) = std::env::var("SSHREMOTE_KMS_GBM") else { return };
        type P = *mut libc::c_void;
        // SAFETY: libgbm's documented C functions, on objects it made, freed at the end.
        unsafe {
            let lib = libc::dlopen(c"libgbm.so.1".as_ptr(), libc::RTLD_NOW);
            assert!(!lib.is_null(), "libgbm");
            macro_rules! f {
                ($n:literal, $t:ty) => {
                    std::mem::transmute::<P, $t>(libc::dlsym(lib, concat!($n, "\0").as_ptr() as *const libc::c_char))
                };
            }
            let create_device = f!("gbm_create_device", unsafe extern "C" fn(i32) -> P);
            let create = f!("gbm_bo_create_with_modifiers", unsafe extern "C" fn(P, u32, u32, u32, *const u64, u32) -> P);
            let map = f!("gbm_bo_map", unsafe extern "C" fn(P, u32, u32, u32, u32, u32, *mut u32, *mut P) -> P);
            let unmap = f!("gbm_bo_unmap", unsafe extern "C" fn(P, P));
            let planes = f!("gbm_bo_get_plane_count", unsafe extern "C" fn(P) -> i32);
            let handle = f!("gbm_bo_get_handle_for_plane", unsafe extern "C" fn(P, i32) -> u64);
            let stride = f!("gbm_bo_get_stride_for_plane", unsafe extern "C" fn(P, i32) -> u32);
            let offset = f!("gbm_bo_get_offset", unsafe extern "C" fn(P, i32) -> u32);
            let modifier = f!("gbm_bo_get_modifier", unsafe extern "C" fn(P) -> u64);
            let destroy = f!("gbm_bo_destroy", unsafe extern "C" fn(P));
            // The framebuffers' maker, and a reader of its own as a screen reader is.
            let maker = Card::open(&path).unwrap();
            let reader = Card::open(&path).unwrap();
            let dev = create_device(maker.file.as_raw_fd());
            assert!(!dev.is_null(), "gbm device");
            let (w, h) = (1920u32, 1080u32);
            let pattern = |x: u32, y: u32| [(x * 7) as u8, (y * 3) as u8, ((x ^ y) * 5) as u8];
            let mut gl = None;
            let kinds: [(&str, u64); 5] = [("linear", 0), ("X-tiled", 0x0100_0000_0000_0001), ("Y-tiled", 0x0100_0000_0000_0002), ("Y-tiled, compressed (CCS)", 0x0100_0000_0000_0004), ("4-tiled", 0x0100_0000_0000_0009)];
            let mut read_any = 0;
            for (name, m) in kinds {
                let bo = create(dev, w, h, XRGB8888, &m, 1);
                if bo.is_null() {
                    eprintln!("{name}: this GPU does not make it");
                    continue;
                }
                let (mut st, mut md) = (0u32, std::ptr::null_mut());
                let px = map(bo, 0, 0, w, h, 3, &mut st, &mut md) as *mut u8;
                assert!(!px.is_null(), "{name}: map");
                for y in 0..h {
                    for x in 0..w {
                        let p = px.add((y * st + x * 4) as usize);
                        let c = pattern(x, y);
                        *p = c[0];
                        *p.add(1) = c[1];
                        *p.add(2) = c[2];
                        *p.add(3) = 255;
                    }
                }
                unmap(bo, md);
                let (got, n) = (modifier(bo), planes(bo).clamp(0, 4));
                let mut fb = Fb2 { width: w, height: h, pixel_format: XRGB8888, flags: FB_MODIFIERS, ..Default::default() };
                for i in 0..n {
                    fb.handles[i as usize] = handle(bo, i) as u32;
                    fb.pitches[i as usize] = stride(bo, i);
                    fb.offsets[i as usize] = offset(bo, i);
                    fb.modifier[i as usize] = got;
                }
                let added = ioctl(&maker.file, drm_rw::<Fb2>(0xB8), &mut fb);
                if let Err(e) = added {
                    eprintln!("{name} ({}): not a framebuffer here: {e}", modifier_name(got));
                    destroy(bo);
                    continue;
                }
                let mut out = Vec::new();
                // The first read opens the GPU's reader; the best of the next ones is what a screen costs.
                let mut r = read_framebuffer(&reader, &mut gl, fb.fb_id, w as usize, h as usize, &mut out);
                let mut took = Duration::MAX;
                for _ in 0..5 {
                    let t = Instant::now();
                    r = r.and_then(|_| read_framebuffer(&reader, &mut gl, fb.fb_id, w as usize, h as usize, &mut out));
                    took = took.min(t.elapsed());
                }
                let mut id = fb.fb_id;
                let _ = ioctl(&maker.file, drm_rw::<u32>(0xAF), &mut id);
                destroy(bo);
                r.unwrap_or_else(|e| panic!("{name}: {e}"));
                let wrong = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| out[((y * w + x) * 4) as usize..][..3] != pattern(x, y)).count();
                eprintln!("{name} ({}, {} planes): read in {took:?}, {wrong} pixels wrong", modifier_name(got), n);
                assert_eq!(wrong, 0, "{name}");
                read_any += 1;
            }
            assert!(read_any >= 3, "too few kinds could be tried");
        }
    }

    #[test]
    fn pixels_of_every_kind_turn_bgrx() {
        let mut out = Vec::new();
        to_bgrx(&[1, 2, 3, 0], 4, XRGB8888, 1, 1, &mut out).unwrap();
        assert_eq!(out, [1, 2, 3, 0]);
        to_bgrx(&[1, 2, 3, 0], 4, XBGR8888, 1, 1, &mut out).unwrap();
        assert_eq!(out, [3, 2, 1, 255]);
        // Pure red in 10 bits a channel.
        to_bgrx(&(0x3ffu32 << 20).to_le_bytes(), 4, XRGB2101010, 1, 1, &mut out).unwrap();
        assert_eq!(out, [0, 0, 255, 255]);
        to_bgrx(&0xf800u16.to_le_bytes(), 2, RGB565, 1, 1, &mut out).unwrap();
        assert_eq!(out, [0, 0, 255, 255]);
        assert!(to_bgrx(&[0; 4], 4, u32::from_le_bytes(*b"NV12"), 1, 1, &mut out).is_err());
    }
}
