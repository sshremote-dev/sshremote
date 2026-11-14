//! The computer's cameras as screens to look at (as RustDesk's "view camera"): listed after its
//! monitors, shown when picked, never controlled. Linux reads them through GStreamer (v4l2, or
//! PipeWire's own); Windows through Media Foundation. With SSHREMOTE_TEST_CAMERA set a moving
//! test picture is a camera too.

use super::{Capture, Image, Input, Monitor, scale_nearest};

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
    last: Option<Image>,
    again: bool,
}

// SAFETY: the stream is used from the thread that holds the capture.
unsafe impl Send for Camera {}

impl Camera {
    pub fn open(cam: &Cam) -> Result<Camera, String> {
        let mut stream = imp::Stream::open(cam)?;
        let first = stream.next(std::time::Duration::from_secs(8))?.ok_or_else(|| format!("{} sent no picture within 8 s", cam.name))?;
        Ok(Camera { stream, desk: (first.w, first.h), target: (first.w, first.h), last: Some(first), again: true })
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
        Ok(Some(scale_nearest(&img.data, img.w, img.h, img.stride, self.target.0, self.target.1)))
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
            list.push(Monitor { name: "Screen".into(), w, h, primary: true });
        }
        list.extend(self.cams.iter().map(|c| Monitor { name: format!("Camera: {}", c.name), w: 0, h: 0, primary: false }));
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
    fn input(&mut self, ev: Input) -> Result<(), String> {
        self.now_mut().input(ev)
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
    use super::Cam;
    use crate::capture::Image;

    pub fn list() -> Vec<Cam> {
        Vec::new()
    }

    pub struct Stream;

    impl Stream {
        pub fn open(_cam: &Cam) -> Result<Stream, String> {
            Err("cameras are not read on Windows yet".into())
        }
        pub fn next(&mut self, _wait: std::time::Duration) -> Result<Option<Image>, String> {
            Ok(None)
        }
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
