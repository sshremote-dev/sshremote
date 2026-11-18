//! Wayland desktops other than GNOME (KDE Plasma, COSMIC, Hyprland, Sway...): one monitor through
//! the xdg desktop portal, as video calls share a screen, and input through its remote-desktop
//! portal where the desktop has one (wlroots ones show the screen only). The person at the screen
//! allows it the first time; the desktop's restore token, kept here, lets later sessions in
//! without anyone there, where the desktop keeps such tokens (KDE Plasma does).

use gstreamer as gst;

use super::gnome::{Pipe, pipeline, to_image};
use super::portal::Portal;
use super::{Capture, Image, Input};

/// The portal's session and the stream it gives (the stream ends first).
struct Live {
    pipe: Pipe,
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
    /// The newest picture, and whether it is to be given again (the first look, a new size): a
    /// desktop sends a frame only when its screen changes.
    last: Image,
    again: bool,
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
        let pipe = pipeline(&portal.source(), None)?;
        let first = pipe.sink.try_pull_sample(gst::ClockTime::from_seconds(10)).ok_or("no picture from the shared screen within 10 s")?;
        let img = to_image(&first)?;
        // Keys through a virtual keyboard where the compositor offers one (it types any character;
        // COSMIC's portal may not pass keys on), the rest through the portal.
        let wl = super::wlinput::WlInput::open().ok().filter(|w| w.has_pointer() || portal.controls());
        Ok(Wayland { controls: portal.controls(), live: Some(Live { pipe, portal }), wl, desk: (img.w, img.h), target: (img.w, img.h), shrink: super::Shrink::default(), last: img, again: true })
    }

    /// Whether input reaches the desktop (it has the remote-desktop portal).
    pub fn controls(&self) -> bool {
        self.controls || self.wl.is_some()
    }
}

impl Capture for Wayland {
    fn name(&self) -> &'static str {
        if self.controls() { "portal" } else { "portal-view" }
    }

    fn desktop_size(&self) -> (usize, usize) {
        self.desk
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
            *self = again;
        }
    }

    fn grab(&mut self) -> Result<Option<Image>, String> {
        let live = self.live.as_ref().ok_or("the screen sharing did not come back after its resolution changed")?;
        match live.pipe.sink.try_pull_sample(gst::ClockTime::ZERO) {
            Some(sample) => {
                self.last = to_image(&sample)?;
                // The screen turned or its resolution changed: the viewer hears the new size.
                self.desk = (self.last.w, self.last.h);
            }
            None if std::mem::take(&mut self.again) => {}
            None => {
                if !live.portal.alive() {
                    return Err("the screen sharing was stopped on that computer".into());
                }
                return Ok(None);
            }
        }
        self.again = false;
        let img = &self.last;
        if self.target == (img.w, img.h) {
            return Ok(Some(Image { w: img.w, h: img.h, stride: img.stride, data: img.data.clone() }));
        }
        Ok(Some(self.shrink.image(&img.data, img.w, img.h, img.stride, self.target.0, self.target.1)))
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
