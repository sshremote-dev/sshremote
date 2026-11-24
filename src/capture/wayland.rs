//! Wayland desktops other than GNOME (KDE Plasma, COSMIC, Hyprland, Sway...): one monitor through
//! the xdg desktop portal, as video calls share a screen, and input through its remote-desktop
//! portal where the desktop has one (wlroots ones show the screen only). The person at the screen
//! allows it the first time; the desktop's restore token, kept here, lets later sessions in
//! without anyone there, where the desktop keeps such tokens (KDE Plasma does).

use gstreamer as gst;

use super::gnome::{Pipe, pipeline, to_image};
use super::portal::Portal;
use super::{Capture, Image, Input};

/// What reads the portal's stream: this program, with the pointer beside the frames, where the
/// desktop sends it so; GStreamer otherwise, the pointer in the frames.
enum Feed {
    Own(super::pw::Stream),
    Gst(Pipe),
}

/// The portal's session and the stream it gives (the stream ends first).
struct Live {
    feed: Feed,
    portal: Portal,
}

pub struct Wayland {
    /// Closed while the screen is set to another resolution (a portal may fail on a stream whose
    /// size changes), then opened again.
    live: Option<Live>,
    controls: bool,
    /// Input of its own where the portal takes none (wlroots desktops).
    wl: Option<super::wlinput::WlInput>,
    desk: (usize, usize),
    target: (usize, usize),
    /// The picture made smaller for the viewer, by area averaging.
    shrink: super::Shrink,
    /// The newest picture, and whether it is to be given again (the first look, a new size, the
    /// pointer drawn in or not): a desktop sends a frame only when its screen changes.
    last: Image,
    again: bool,
    /// The viewer draws the pointer; the number of its picture last sent.
    apart: bool,
    sent_shape: u64,
    /// What the stream read calls as something comes (`Capture::set_waker`).
    wake: super::Waker,
}

// SAFETY: the pipeline and the portal's connection are used from the thread that holds them.
unsafe impl Send for Wayland {}

fn token_file() -> Option<std::path::PathBuf> {
    crate::hosts::config_dir().map(|d| d.join("portal-restore-token"))
}

impl Wayland {
    pub fn open() -> Result<Wayland, String> {
        super::gnome::ensure_env();
        gst::init().map_err(|e| format!("GStreamer init: {e}"))?;
        let kept = token_file().and_then(|f| std::fs::read_to_string(f).ok()).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let portal = super::portal::screen(kept.as_deref())?;
        // The next session's way in, without the dialog (a token is good for one session).
        if let (Some(t), Some(f)) = (&portal.restore, token_file()) {
            let _ = f.parent().map(std::fs::create_dir_all);
            let _ = std::fs::write(f, t);
        }
        let (feed, img) = feed_for(&portal, portal.node)?;
        // Keys through a virtual keyboard where the compositor offers one (it types any character;
        // COSMIC's portal may not pass keys on), the rest through the portal.
        let wl = super::wlinput::WlInput::open().ok().filter(|w| w.has_pointer() || portal.controls());
        Ok(Wayland {
            controls: portal.controls(),
            live: Some(Live { feed, portal }),
            wl,
            desk: (img.w, img.h),
            target: (img.w, img.h),
            shrink: super::Shrink::default(),
            last: img,
            again: true,
            apart: false,
            sent_shape: 0,
            wake: Default::default(),
        })
    }

    /// Whether input reaches the desktop (it has the remote-desktop portal).
    pub fn controls(&self) -> bool {
        self.controls || self.wl.is_some()
    }

    /// The screens shared, when there are several to pick from.
    fn screens(&self) -> &[super::portal::Screen] {
        match &self.live {
            Some(l) if l.portal.screens.len() > 1 => &l.portal.screens,
            _ => &[],
        }
    }
}

/// The stream `node` of `portal` read, and its first picture.
fn feed_for(portal: &super::portal::Portal, node: u32) -> Result<(Feed, Image), String> {
    // Read here, buffers given back at once: GStreamer's reader keeps the few that wlroots'
    // portals (Sway, Hyprland) have, and their screen copying then stops after the first
    // picture. COSMIC's is left to GStreamer until tried.
    let cosmic = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|d| d.eq_ignore_ascii_case("COSMIC")));
    let own = (portal.apart || (super::pw::usable() && !cosmic)).then(|| super::pw::Stream::open(Some(portal.fd()), node, std::time::Duration::from_secs(10)));
    Ok(match own.map(|r| r.map(|s| (s.take().0, s))) {
        Some(Ok((Some(f), s))) => (Feed::Own(s), Image { w: f.w, h: f.h, stride: f.w * 4, data: f.data }),
        other => {
            // Read by GStreamer instead: the picture without the pointer, rather than none.
            if let Some(Err((_, true))) = other {
                super::pw::give_up();
            }
            let pipe = pipeline(&portal.source_of(node), None)?;
            let first = pipe.sink.try_pull_sample(gst::ClockTime::from_seconds(10)).ok_or("no picture from the shared screen within 10 s")?;
            (Feed::Gst(pipe), to_image(&first)?)
        }
    })
}

impl Capture for Wayland {
    fn name(&self) -> &'static str {
        if self.controls() { "portal" } else { "portal-view" }
    }

    fn desktop_size(&self) -> (usize, usize) {
        self.desk
    }

    fn monitors(&self) -> Vec<super::Monitor> {
        self.screens()
            .iter()
            .enumerate()
            .map(|(i, s)| super::Monitor { name: format!("screen {}", i + 1), w: s.size.0.max(0) as usize, h: s.size.1.max(0) as usize, primary: i == 0 })
            .collect()
    }

    fn current(&self) -> usize {
        let node = self.live.as_ref().map_or(0, |l| l.portal.node);
        self.screens().iter().position(|s| s.node == node).unwrap_or(0)
    }

    fn select(&mut self, index: usize) -> Result<(), String> {
        let Some(live) = self.live.as_ref() else { return Err("the screen sharing is not on now".into()) };
        let s = live.portal.screens.get(index).cloned().ok_or("that computer has no such monitor")?;
        let was = live.portal.node;
        if s.node == was {
            return Ok(());
        }
        // The stream shown ends first: a desktop may not feed two at once (KWin's second one gets
        // no buffers). Should the other not come, the one shown comes back.
        let Some(Live { feed, mut portal }) = self.live.take() else { unreachable!() };
        drop(feed);
        match feed_for(&portal, s.node) {
            Ok((feed, img)) => {
                portal.node = s.node;
                self.live = Some(Live { feed, portal });
                (self.desk, self.last, self.again, self.sent_shape) = ((img.w, img.h), img, true, 0);
                Ok(())
            }
            Err(e) => {
                if let Ok((feed, img)) = feed_for(&portal, was) {
                    self.live = Some(Live { feed, portal });
                    (self.desk, self.last, self.again) = ((img.w, img.h), img, true);
                } else {
                    // Neither came: the sharing starts over, as after a resolution change.
                    drop(portal);
                    self.screen_changed();
                }
                Err(format!("that monitor's picture did not come: {e}"))
            }
        }
    }

    fn origin(&self) -> (i32, i32) {
        let node = self.live.as_ref().map_or(0, |l| l.portal.node);
        self.screens().iter().find(|s| s.node == node).map_or((0, 0), |s| s.at)
    }

    fn set_target(&mut self, w: usize, h: usize) -> Result<(), String> {
        self.target = (w.max(1), h.max(1));
        self.again = true;
        Ok(())
    }

    fn screen_changing(&mut self) {
        self.live = None;
    }

    fn screen_changed(&mut self) {
        if self.live.is_none()
            && let Ok(again) = Wayland::open()
        {
            // Who to wake stays (the agent gives that once), and the picture's size where the
            // screen kept its own.
            let (apart, wake, desk, target) = (self.apart, std::mem::take(&mut self.wake), self.desk, self.target);
            *self = again;
            self.wake = wake;
            if self.desk == desk {
                self.target = target;
            }
            self.apart = apart && self.pointer_apart(true);
        }
    }

    fn pointer_apart(&mut self, on: bool) -> bool {
        // The pointer comes beside the frames only where it was asked for that way.
        if !self.live.as_ref().is_some_and(|l| l.portal.apart && matches!(l.feed, Feed::Own(_))) {
            return false;
        }
        (self.apart, self.sent_shape, self.again) = (on, 0, true);
        true
    }

    fn pointer(&mut self) -> (Option<super::Pointer>, Option<(i32, i32)>) {
        let Some(Live { feed: Feed::Own(s), .. }) = &self.live else { return (None, None) };
        let p = s.pointer();
        let shape = match p.shape {
            // Hidden (by a game, a player).
            Some(s) if s.w == 0 => return (None, None),
            Some(s) => s,
            // Where it is, even before this stream sent its picture: the same pointer as on the
            // monitor shown before, whose picture the viewer has.
            None => return (None, p.at),
        };
        let new = (p.serial != self.sent_shape).then(|| shape.to_pointer());
        self.sent_shape = p.serial;
        (new, p.at)
    }

    fn set_waker(&mut self, wake: Box<dyn Fn() + Send>) -> bool {
        *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
        true
    }

    fn wakes(&self) -> bool {
        self.live.is_some()
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        let live = self.live.as_ref().ok_or("the screen sharing did not come back after its resolution changed")?;
        // The stream read now (one opened again after a resolution change too) wakes the agent.
        match &live.feed {
            Feed::Own(s) => s.wake_with(&self.wake),
            Feed::Gst(pipe) => pipe.wake_with(&self.wake),
        }
        let fresh = match &live.feed {
            Feed::Gst(pipe) => match pipe.sink.try_pull_sample(gst::ClockTime::ZERO) {
                Some(sample) => {
                    self.last = to_image(&sample)?;
                    true
                }
                None => false,
            },
            Feed::Own(s) => {
                if let Some(why) = s.stopped() {
                    return Err(format!("the screen sharing stopped on that computer ({why})"));
                }
                let (frame, moved) = s.take();
                // The pointer moved on a still screen: drawn in again where the viewer does not draw it.
                self.again |= moved && !self.apart;
                match frame {
                    Some(f) => {
                        let old = std::mem::replace(&mut self.last, Image { w: f.w, h: f.h, stride: f.w * 4, data: f.data });
                        s.recycle(old.data);
                        true
                    }
                    None => false,
                }
            }
        };
        if fresh {
            // The screen turned or its resolution changed: the viewer hears the new size.
            self.desk = (self.last.w, self.last.h);
        } else if !std::mem::take(&mut self.again) {
            if !live.portal.alive() {
                return Err("the screen sharing was stopped on that computer".into());
            }
            return Ok(None);
        }
        self.again = false;
        let img = &self.last;
        let mut out = Image { w: img.w, h: img.h, stride: img.stride, data: img.data.clone() };
        if !self.apart
            && let Some(Live { feed: Feed::Own(s), .. }) = &self.live
            && let p = s.pointer()
            && let (Some(at), Some(shape)) = (p.at, &p.shape)
        {
            super::pw::draw(&mut out.data, out.w, out.h, out.stride, shape, at);
        }
        if self.target == (out.w, out.h) {
            return Ok(Some(out));
        }
        Ok(Some(self.shrink.image(&out.data, out.w, out.h, out.stride, self.target.0, self.target.1)))
    }

    fn input(&mut self, ev: Input) -> Result<(), String> {
        // A desktop that only lets its screen be seen: said once, when the session starts.
        let key = matches!(ev, Input::Key { .. } | Input::KeyState { .. });
        if let Some(wl) = self.wl.as_mut().filter(|w| key || w.has_pointer()) {
            return wl.input(ev, self.desk);
        }
        match &self.live {
            Some(live) if live.portal.controls() => live.portal.input(ev, self.desk),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The screen keeps coming after its first picture: keys typed into the focused terminal
    /// show in later ones (wlroots' portals stopped copying the screen once GStreamer held their
    /// few buffers), and each wakes the agent. In the desktop's session: SSHREMOTE_PORTAL_TEST=1
    /// TESTS --ignored the_portal_screen_keeps_coming (SSHREMOTE_PORTAL_GST=1: read by GStreamer)
    #[test]
    #[ignore]
    fn the_portal_screen_keeps_coming() {
        if std::env::var_os("SSHREMOTE_PORTAL_TEST").is_none() {
            return;
        }
        if std::env::var_os("SSHREMOTE_PORTAL_GST").is_some() {
            super::super::pw::give_up();
        }
        let mut w = Wayland::open().expect("a portal");
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let count = woken.clone();
        assert!(w.set_waker(Box::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })));
        let first = (0..150).find_map(|_| w.grab().unwrap().or_else(|| (std::thread::sleep(std::time::Duration::from_millis(20)), None).1)).expect("a picture");
        for c in "echo still-coming".chars() {
            w.input(Input::Key { keysym: crate::xk::of_char(c), mods: 0 }).unwrap();
        }
        let mut later = 0;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < until {
            if let Some(img) = w.grab().unwrap()
                && img.data != first.data
            {
                later += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(later > 0, "pictures after the first");
        let gst = matches!(w.live.as_ref().map(|l| &l.feed), Some(Feed::Gst(_)));
        println!("read by {}, woken {} times for {later} later pictures", if gst { "GStreamer" } else { "PipeWire here" }, woken.load(std::sync::atomic::Ordering::Relaxed));
        assert!(woken.load(std::sync::atomic::Ordering::Relaxed) > 0, "the stream woke the agent");
    }

    /// A desktop with two monitors shares both: both are listed, each shows when picked, and the
    /// pointer goes where it is sent on the one shown. In a session with two screens:
    /// SSHREMOTE_PORTAL_TEST=2 TESTS --ignored the_portals_monitors_are_picked
    #[test]
    #[ignore]
    fn the_portals_monitors_are_picked() {
        if std::env::var_os("SSHREMOTE_PORTAL_TEST").is_none_or(|v| v != "2") {
            return;
        }
        let pause = || std::thread::sleep(std::time::Duration::from_millis(20));
        let mut w = Wayland::open().expect("a portal");
        let list = w.monitors();
        eprintln!("monitors {:?}", list.iter().map(|m| (m.name.clone(), m.w, m.h)).collect::<Vec<_>>());
        assert_eq!(list.len(), 2, "both shared");
        w.pointer_apart(true);
        for i in [1, 0, 1] {
            w.select(i).unwrap_or_else(|e| panic!("monitor {i}: {e}"));
            assert_eq!(w.current(), i);
            assert_eq!(w.desktop_size(), (list[i].w, list[i].h));
            w.set_target(list[i].w, list[i].h).unwrap();
            let img = (0..150).find_map(|_| w.grab().unwrap().or_else(|| (pause(), None).1)).expect("its picture");
            assert_eq!((img.w, img.h), (list[i].w, list[i].h));
            // On the screen shown, where it was sent.
            let to = (100 + 50 * i as i32, 120);
            w.input(Input::Move { x: to.0 as f64, y: to.1 as f64 }).unwrap();
            assert!((0..150).any(|_| w.pointer().1 == Some(to) || (pause(), false).1), "screen {i}: pointer at {:?}", w.pointer().1);
        }
        assert_eq!(w.origin(), (list[0].w as i32, 0), "the second to the right of the first");
    }

    /// A portal's pointer kept apart (KDE Plasma sends it as metadata): its picture comes once
    /// with where it is, pictures leave it out, and put back it is drawn in again. In the desktop's
    /// session: SSHREMOTE_PORTAL_TEST=1 TESTS --ignored the_portal_pointer_comes_apart
    #[test]
    #[ignore]
    fn the_portal_pointer_comes_apart() {
        if std::env::var_os("SSHREMOTE_PORTAL_TEST").is_none() {
            return;
        }
        let pause = || std::thread::sleep(std::time::Duration::from_millis(20));
        let next = |w: &mut Wayland| (0..150).find_map(|_| w.grab().unwrap().or_else(|| (pause(), None).1));
        let around = |img: &Image, (x, y): (usize, usize)| -> Vec<u8> {
            (y..y + 24).flat_map(|r| img.data[r * img.stride + x * 4..][..24 * 4].to_vec()).collect()
        };
        let mut w = Wayland::open().expect("a portal");
        assert!(w.pointer_apart(true), "the portal sends the pointer apart");
        let (mut shape, mut at) = (None, None);
        for (x, y) in [(250.0, 150.0), (300.0, 200.0)] {
            w.input(Input::Move { x, y }).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        for _ in 0..150 {
            let _ = w.grab().unwrap();
            let (s, a) = w.pointer();
            shape = shape.or(s);
            at = a;
            if shape.is_some() && at == Some((300, 200)) {
                break;
            }
            pause();
        }
        let shape = shape.expect("the pointer's picture");
        assert!(shape.w > 0 && shape.rgba.len() == shape.w * shape.h * 4, "{}x{}", shape.w, shape.h);
        assert_eq!(at, Some((300, 200)));
        assert!(w.pointer().0.is_none(), "the same picture is not sent again");
        w.input(Input::Move { x: 500.0, y: 400.0 }).unwrap();
        assert!((0..150).any(|_| w.pointer().1 == Some((500, 400)) || (pause(), false).1), "where it went");
        // Moved by so much (the mouse for games), and back.
        w.input(Input::MoveBy { dx: 40, dy: -30 }).unwrap();
        assert!((0..150).any(|_| w.pointer().1 == Some((540, 370)) || (pause(), false).1), "moved by 40, -30: {:?}", w.pointer().1);
        w.input(Input::MoveBy { dx: -40, dy: 30 }).unwrap();
        assert!((0..150).any(|_| w.pointer().1 == Some((500, 400)) || (pause(), false).1), "and back");
        w.again = true;
        let still = next(&mut w).expect("a picture");
        let bare = around(&still, (490, 390));
        assert!(w.pointer_apart(false));
        let with = next(&mut w).expect("a picture with the pointer");
        assert_ne!(around(&with, (490, 390)), bare, "the pointer drawn in");
        assert_eq!(around(&with, (100, 600)), around(&still, (100, 600)), "and nothing else changed");
    }
}
