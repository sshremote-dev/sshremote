//! The computer's cameras as screens to look at (as RustDesk's "view camera"): listed after its
//! monitors, shown when picked, never controlled. Linux reads them through GStreamer (v4l2, or
//! PipeWire's own); Windows through Media Foundation. With SSHREMOTE_TEST_CAMERA set a moving
//! test picture is a camera too.

use super::{Capture, Image, Input, Monitor};

/// A camera: its name, and how to open it.
#[derive(Clone, Debug)]
pub struct Cam {
    pub name: String,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    source: String,
}

/// The cameras this computer has (none where it cannot tell).
pub fn list() -> Vec<Cam> {
    let mut all = imp::list();
    if std::env::var_os("SSHREMOTE_TEST_CAMERA").is_some() {
        all.push(Cam { name: "Test camera".into(), source: "test".into() });
    }
    all
}

/// A camera being looked at: its newest picture, given again when asked anew (a still scene
/// sends frames all the same, but a size change must not wait for one).
pub struct Camera {
    stream: imp::Stream,
    desk: (usize, usize),
    target: (usize, usize),
    /// The picture made smaller for the viewer, by area averaging.
    shrink: super::Shrink,
    last: Option<Image>,
    again: bool,
}

impl Camera {
    pub fn open(cam: &Cam) -> Result<Camera, String> {
        let mut stream = imp::Stream::open(cam)?;
        let first = stream.next(std::time::Duration::from_secs(8))?.ok_or_else(|| format!("{} sent no picture within 8 s", cam.name))?;
        Ok(Camera { stream, desk: (first.w, first.h), target: (first.w, first.h), shrink: super::Shrink::default(), last: Some(first), again: true })
    }
}

impl Capture for Camera {
    fn name(&self) -> &'static str {
        "camera"
    }

    fn desktop_size(&self) -> (usize, usize) {
        self.desk
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.target = (w.max(1), h.max(1));
        self.again = true;
        Ok(())
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        if let Some(img) = self.stream.next(std::time::Duration::ZERO)? {
            self.desk = (img.w, img.h);
            self.last = Some(img);
        } else if !std::mem::take(&mut self.again) {
            return Ok(None);
        }
        self.again = false;
        let Some(img) = &self.last else { return Ok(None) };
        if self.target == (img.w, img.h) {
            return Ok(Some(Image { w: img.w, h: img.h, stride: img.stride, data: img.data.clone() }));
        }
        Ok(Some(self.shrink.image(&img.data, img.w, img.h, img.stride, self.target.0, self.target.1)))
    }

    /// A camera is looked at, not used: input goes nowhere.
    fn input(&mut self, _ev: Input) -> Result<(), String> {
        Ok(())
    }
}

/// The screen with the cameras after its monitors: picking one shows it, picking a monitor
/// shows the screen again.
pub struct WithCameras {
    screen: Box<dyn Capture>,
    cams: Vec<Cam>,
    cam: Option<(usize, Camera)>,
}

impl WithCameras {
    pub fn new(screen: Box<dyn Capture>, cams: Vec<Cam>) -> WithCameras {
        WithCameras { screen, cams, cam: None }
    }

    /// The screen's monitors as the viewer counts them (one, where it tells none).
    fn screens(&self) -> usize {
        self.screen.monitors().len().max(1)
    }

    fn now(&self) -> &dyn Capture {
        match &self.cam {
            Some((_, c)) => c,
            None => self.screen.as_ref(),
        }
    }

    fn now_mut(&mut self) -> &mut dyn Capture {
        match &mut self.cam {
            Some((_, c)) => c,
            None => self.screen.as_mut(),
        }
    }
}

impl Capture for WithCameras {
    fn pick(&mut self) -> Result<String, String> {
        self.screen.pick()
    }
    fn poll_share(&mut self) -> Option<Result<(u8, String), String>> {
        self.screen.poll_share()
    }
    fn install_helper(&mut self) -> Result<String, String> {
        self.screen.install_helper()
    }
    fn windows(&self) -> Result<Vec<super::WindowInfo>, String> {
        self.screen.windows()
    }
    fn bring_forward(&mut self, id: u64) -> Result<(), String> {
        self.screen.bring_forward(id)
    }
    fn share(&mut self, scope: super::Share) -> Result<String, String> {
        // Sharing a window or the screen leaves the camera.
        self.cam = None;
        self.screen.share(scope)
    }
    fn name(&self) -> &'static str {
        self.now().name()
    }
    fn monitors(&self) -> Vec<Monitor> {
        let mut list = self.screen.monitors();
        if list.is_empty() {
            let (w, h) = self.screen.desktop_size();
            list.push(Monitor { name: "Screen".into(), w, h, primary: true, at: None });
        }
        list.extend(self.cams.iter().map(|c| Monitor { name: format!("Camera: {}", c.name), w: 0, h: 0, primary: false, at: None }));
        list
    }
    fn current(&self) -> usize {
        match &self.cam {
            Some((i, _)) => self.screens() + i,
            None => self.screen.current(),
        }
    }
    fn select(&mut self, index: usize) -> Result<(), String> {
        let screens = self.screens();
        if index < screens {
            self.cam = None;
            return if self.screen.monitors().is_empty() { Ok(()) } else { self.screen.select(index) };
        }
        let i = index - screens;
        let cam = self.cams.get(i).cloned().ok_or("no such camera")?;
        let c = Camera::open(&cam)?;
        self.cam = Some((i, c));
        Ok(())
    }
    fn desktop_size(&self) -> (usize, usize) {
        self.now().desktop_size()
    }
    fn resizable(&self) -> bool {
        self.now().resizable()
    }
    fn origin(&self) -> (i32, i32) {
        self.now().origin()
    }
    fn shared_window(&self) -> Option<u64> {
        if self.cam.is_some() { None } else { self.screen.shared_window() }
    }
    fn screen_id(&self) -> Option<String> {
        if self.cam.is_some() { None } else { self.screen.screen_id() }
    }
    fn screen_ids(&self) -> Vec<String> {
        let mut ids = self.screen.screen_ids();
        ids.resize(self.screens(), String::new());
        ids.extend(self.cams.iter().map(|c| format!("Camera: {}", c.name)));
        ids
    }
    fn screens_changed(&mut self) {
        self.screen.screens_changed()
    }
    fn screen_changing(&mut self) {
        self.screen.screen_changing()
    }
    fn screen_changed(&mut self) {
        self.screen.screen_changed()
    }
    fn shared_app(&self) -> Option<String> {
        if self.cam.is_some() { None } else { self.screen.shared_app() }
    }
    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.now_mut().set_target(w, h)
    }
    fn set_density(&mut self, d: f64) {
        self.now_mut().set_density(d)
    }
    fn grab(&mut self) -> Result<Option<Image>, String> {
        self.now_mut().grab()
    }
    fn recycle(&mut self, img: Image) {
        if self.cam.is_none() {
            self.screen.recycle(img);
        }
    }
    fn set_fps(&mut self, fps: u8) {
        self.screen.set_fps(fps)
    }
    fn set_waker(&mut self, wake: Box<dyn Fn() + Send>) -> bool {
        self.screen.set_waker(wake)
    }
    fn notice(&mut self) -> Option<String> {
        self.screen.notice()
    }
    // A camera does not wake the agent: it is looked at every millisecond while shown.
    fn wakes(&self) -> bool {
        self.cam.is_none() && self.screen.wakes()
    }
    fn pointer_apart(&mut self, on: bool) -> bool {
        self.screen.pointer_apart(on)
    }
    fn focused(&self) -> Option<(u64, (i32, i32))> {
        self.screen.focused()
    }
    fn pointer(&mut self) -> (Option<super::Pointer>, Option<(i32, i32)>) {
        // A camera's picture has no pointer over it.
        if self.cam.is_some() { (None, None) } else { self.screen.pointer() }
    }
    fn input(&mut self, ev: Input) -> Result<(), String> {
        self.now_mut().input(ev)
    }
    fn commit_text(&mut self, text: &str) -> bool {
        self.now_mut().commit_text(text)
    }
    fn shortcut_key(&self) -> u32 {
        self.screen.shortcut_key()
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;

    use super::Cam;
    use crate::capture::Image;

    pub fn list() -> Vec<Cam> {
        if gst::init().is_err() {
            return Vec::new();
        }
        let m = gst::DeviceMonitor::new();
        m.add_filter(Some("Video/Source"), None);
        if m.start().is_err() {
            return Vec::new();
        }
        let devices = m.devices();
        m.stop();
        let mut out: Vec<Cam> = Vec::new();
        // A camera PipeWire offers as well as v4l2 is listed once, by v4l2.
        for d in devices.iter() {
            let props = d.properties();
            let get = |k: &str| props.as_ref().and_then(|p| p.get::<String>(k).ok());
            let Some(path) = get("device.path").or_else(|| get("api.v4l2.path")).filter(|p| p.starts_with("/dev/video")) else { continue };
            if out.iter().any(|c| c.source.ends_with(&path)) {
                continue;
            }
            out.push(Cam { name: d.display_name().to_string(), source: format!("v4l2src device={path}") });
        }
        out
    }

    pub struct Stream {
        p: gst::Pipeline,
        sink: gst_app::AppSink,
    }

    impl Drop for Stream {
        fn drop(&mut self) {
            let _ = self.p.set_state(gst::State::Null);
        }
    }

    impl Stream {
        pub fn open(cam: &Cam) -> Result<Stream, String> {
            gst::init().map_err(|e| format!("GStreamer: {e}"))?;
            let src = if cam.source == "test" { "videotestsrc is-live=true pattern=ball ! video/x-raw,width=640,height=480,framerate=15/1".to_string() } else { cam.source.clone() };
            let desc = format!("{src} ! queue leaky=downstream max-size-buffers=1 ! videoconvert ! video/x-raw,format=BGRx ! appsink name=sink max-buffers=1 drop=true sync=false");
            let p = gst::parse::launch(&desc).map_err(|e| format!("camera: {e}"))?.downcast::<gst::Pipeline>().map_err(|_| "camera: no pipeline".to_string())?;
            let sink = p.by_name("sink").and_then(|s| s.downcast::<gst_app::AppSink>().ok()).ok_or("camera: no sink")?;
            p.set_state(gst::State::Playing).map_err(|e| format!("{} did not start: {e}", cam.name))?;
            Ok(Stream { p, sink })
        }

        /// The next picture, waiting at most `wait`.
        pub fn next(&mut self, wait: std::time::Duration) -> Result<Option<Image>, String> {
            let Some(s) = self.sink.try_pull_sample(gst::ClockTime::from_nseconds(wait.as_nanos() as u64)) else {
                if self.sink.is_eos() {
                    return Err("the camera stopped".into());
                }
                return Ok(None);
            };
            crate::capture::gnome::to_image(&s).map(Some)
        }
    }
}

#[cfg(windows)]
mod imp {
    pub use super::win::{Stream, list};
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::Cam;
    use crate::capture::Image;

    pub fn list() -> Vec<Cam> {
        Vec::new()
    }

    pub struct Stream;

    impl Stream {
        pub fn open(_cam: &Cam) -> Result<Stream, String> {
            Err("cameras are not read on this system".into())
        }
        pub fn next(&mut self, _wait: std::time::Duration) -> Result<Option<Image>, String> {
            Ok(None)
        }
    }
}

#[cfg(windows)]
mod win {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};

    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::core::{Interface, PWSTR};

    use super::Cam;
    use crate::capture::Image;

    fn start() {
        unsafe {
            // Already set up on this thread (in another way) is as good.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        }
    }

    fn string(a: &IMFActivate, key: &windows::core::GUID) -> Option<String> {
        let (mut p, mut n) = (PWSTR::null(), 0u32);
        unsafe { a.GetAllocatedString(key, &mut p, &mut n) }.ok()?;
        let s = unsafe { p.to_string() }.ok();
        unsafe { CoTaskMemFree(Some(p.0 as _)) };
        s
    }

    /// The video capture devices, each with its name and symbolic link (its lasting name).
    fn devices() -> Vec<(IMFActivate, String, String)> {
        start();
        let mut attrs = None;
        if unsafe { MFCreateAttributes(&mut attrs, 1) }.is_err() {
            return Vec::new();
        }
        let Some(attrs) = attrs else { return Vec::new() };
        if unsafe { attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID) }.is_err() {
            return Vec::new();
        }
        let (mut list, mut count) = (std::ptr::null_mut(), 0u32);
        if unsafe { MFEnumDeviceSources(&attrs, &mut list, &mut count) }.is_err() || list.is_null() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in 0..count as usize {
            // SAFETY: the array holds `count` activations, each taken out once.
            if let Some(a) = unsafe { (*list.add(i)).take() } {
                let name = string(&a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_else(|| "Camera".into());
                let link = string(&a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK).unwrap_or_default();
                out.push((a, name, link));
            }
        }
        unsafe { CoTaskMemFree(Some(list as _)) };
        out
    }

    pub fn list() -> Vec<Cam> {
        devices().into_iter().map(|(_, name, source)| Cam { name, source }).collect()
    }

    /// Pictures read on a thread of their own (Media Foundation waits for each), the newest kept.
    pub struct Stream {
        latest: Arc<Mutex<Option<Image>>>,
        failed: Arc<Mutex<Option<String>>>,
        stop: Arc<AtomicBool>,
    }

    impl Drop for Stream {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
        }
    }

    impl Stream {
        pub fn open(cam: &Cam) -> Result<Stream, String> {
            let (latest, failed, stop) = (Arc::new(Mutex::new(None)), Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
            let (l, f, st, source) = (latest.clone(), failed.clone(), stop.clone(), cam.source.clone());
            let (ready_tx, ready) = mpsc::channel::<Result<(), String>>();
            std::thread::spawn(move || {
                let reader = match open_reader(&source) {
                    Ok(r) => {
                        let _ = ready_tx.send(Ok(()));
                        r
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                while !st.load(Ordering::Acquire) {
                    match read(&reader) {
                        Ok(Some(img)) => *l.lock().unwrap_or_else(|e| e.into_inner()) = Some(img),
                        Ok(None) => {}
                        Err(e) => {
                            *f.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                            return;
                        }
                    }
                }
            });
            ready.recv_timeout(std::time::Duration::from_secs(10)).map_err(|_| format!("{} did not open", cam.name))??;
            Ok(Stream { latest, failed, stop })
        }

        pub fn next(&mut self, wait: std::time::Duration) -> Result<Option<Image>, String> {
            let until = std::time::Instant::now() + wait;
            loop {
                if let Some(e) = self.failed.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                    return Err(e);
                }
                if let Some(img) = self.latest.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    return Ok(Some(img));
                }
                if std::time::Instant::now() >= until {
                    return Ok(None);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    /// The camera of `link`, read as 32-bit BGRx through Media Foundation's own converter.
    fn open_reader(link: &str) -> Result<IMFSourceReader, String> {
        fn e(what: &'static str) -> impl Fn(windows::core::Error) -> String {
            move |x| format!("camera {what}: {x}")
        }
        let (act, ..) = devices().into_iter().find(|(_, _, l)| l == link).ok_or("the camera is gone")?;
        let source: IMFMediaSource = unsafe { act.ActivateObject() }.map_err(e("open"))?;
        let mut attrs = None;
        unsafe { MFCreateAttributes(&mut attrs, 1) }.map_err(e("attributes"))?;
        let attrs = attrs.ok_or("no attributes")?;
        unsafe { attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1) }.map_err(e("conversion"))?;
        let reader = unsafe { MFCreateSourceReaderFromMediaSource(&source, &attrs) }.map_err(e("reader"))?;
        let mt = unsafe { MFCreateMediaType() }.map_err(e("type"))?;
        unsafe {
            mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(e("type"))?;
            mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).map_err(e("type"))?;
            reader.SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &mt).map_err(e("format"))?;
        }
        Ok(reader)
    }

    /// The next picture (None: a gap in the stream), waiting for it.
    fn read(reader: &IMFSourceReader) -> Result<Option<Image>, String> {
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let (mut flags, mut sample) = (0u32, None);
        unsafe { reader.ReadSample(stream, 0, None, Some(&mut flags), None, Some(&mut sample)) }.map_err(|x| format!("camera: {x}"))?;
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            return Err("the camera stopped".into());
        }
        let Some(sample) = sample else { return Ok(None) };
        let size = unsafe { reader.GetCurrentMediaType(stream).and_then(|t| t.GetUINT64(&MF_MT_FRAME_SIZE)) }.map_err(|x| format!("camera: {x}"))?;
        let (w, h) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
        let buf = unsafe { sample.ConvertToContiguousBuffer() }.map_err(|x| format!("camera: {x}"))?;
        let mut data = vec![0u8; w * h * 4];
        // Rows as the buffer lays them (a 2D buffer says its pitch, bottom-up when negative).
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            let (mut line0, mut pitch) = (std::ptr::null_mut(), 0i32);
            unsafe { b2.Lock2D(&mut line0, &mut pitch) }.map_err(|x| format!("camera: {x}"))?;
            for y in 0..h {
                // SAFETY: row y starts `pitch` bytes after row y-1, each w*4 bytes long.
                let row = unsafe { std::slice::from_raw_parts(line0.offset(y as isize * pitch as isize), w * 4) };
                data[y * w * 4..(y + 1) * w * 4].copy_from_slice(row);
            }
            let _ = unsafe { b2.Unlock2D() };
        } else {
            let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
            unsafe { buf.Lock(&mut p, None, Some(&mut len)) }.map_err(|x| format!("camera: {x}"))?;
            let n = data.len().min(len as usize);
            data[..n].copy_from_slice(unsafe { std::slice::from_raw_parts(p, n) });
            let _ = unsafe { buf.Unlock() };
        }
        Ok(Some(Image { w, h, stride: w * 4, data }))
    }
}

/// The cameras here, named; opened (the camera's light on) only with SSHREMOTE_CAMERA_OPEN=1.
/// cargo test -- --ignored --nocapture cameras_are_listed
#[cfg(test)]
#[test]
#[ignore]
fn cameras_are_listed() {
    let cams = list();
    println!("cameras: {:?}", cams.iter().map(|c| c.name.as_str()).collect::<Vec<_>>());
    if std::env::var_os("SSHREMOTE_CAMERA_OPEN").is_some()
        && let Some(c) = cams.first()
    {
        let mut cam = Camera::open(c).expect("it opens");
        let img = cam.grab().unwrap().expect("a picture");
        println!("{}: {}x{}", c.name, img.w, img.h);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A camera picked after the screen shows its moving picture, takes no input, and picking
    /// the screen again shows the screen.
    #[test]
    fn a_camera_is_a_screen_to_look_at() {
        let screen: Box<dyn Capture> = Box::new(crate::capture::synthetic::Synthetic::new());
        let cams = vec![Cam { name: "Test camera".into(), source: "test".into() }];
        let mut c = WithCameras::new(screen, cams);
        let names: Vec<String> = c.monitors().into_iter().map(|m| m.name).collect();
        assert_eq!(names.last().unwrap(), "Camera: Test camera");
        let at = names.len() - 1;
        if gstreamer::init().is_err() || gstreamer::ElementFactory::find("videotestsrc").is_none() {
            return;
        }
        c.select(at).unwrap();
        assert_eq!((c.name(), c.current(), c.desktop_size()), ("camera", at, (640, 480)));
        let first = c.grab().unwrap().expect("its picture");
        assert_eq!((first.w, first.h), (640, 480));
        c.set_target(320, 240).unwrap();
        let small = c.grab().unwrap().expect("again at the new size");
        assert_eq!((small.w, small.h), (320, 240));
        assert!(c.input(Input::Move { x: 1.0, y: 1.0 }).is_ok());
        c.select(0).unwrap();
        assert_eq!(c.name(), "synthetic");
    }
}
