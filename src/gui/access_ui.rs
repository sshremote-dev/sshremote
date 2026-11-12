//! Remote access by ID as the home window shows it: the settings read once, and the relay and the
//! service asked about in the background; the drawing is the home's (`home/this_computer.rs`).

use eframe::egui::{self, CornerRadius};

use super::{App, bad, good};

/// What remote access keeps between frames.
#[derive(Default)]
pub(super) struct AccessUi {
    #[cfg(feature = "remote")]
    pub(super) cfg: Option<crate::access::Config>,
    pub(super) show_password: bool,
    pub(super) new_password: String,
    pub(super) relay: String,
    /// Whether the relay sees this ID now (for which ID and relay), looked up in the background
    /// every few seconds, one lookup at a time.
    pub(super) online: Shared<Option<Online>>,
    pub(super) looked: Option<std::time::Instant>,
    /// The service: installed, running, and running this program file; looked up in the background.
    pub(super) service: Shared<Option<Service>>,
    pub(super) service_looked: Option<std::time::Instant>,
    /// Why the stored settings could not be read or kept, shown instead of an ID.
    #[cfg(feature = "remote")]
    pub(super) problem: Option<String>,
}

type Shared<T> = std::sync::Arc<std::sync::Mutex<T>>;

/// The relay's answer: for which ID and relay, and whether it sees the ID there.
type Online = (String, String, Result<bool, String>);

#[derive(Clone, Copy, Default)]
pub(super) struct Service {
    pub(super) installed: bool,
    /// It runs now (and so serves the viewers, not this window).
    pub(super) runs: bool,
    /// It runs this program file (not an older copy).
    pub(super) this: bool,
}

/// Runs `look` on a thread of its own unless one still runs for `slot`, then repaints.
fn in_background<T: Send + 'static>(slot: &Shared<Option<T>>, busy: &'static std::sync::atomic::AtomicBool, ctx: &egui::Context, look: impl FnOnce() -> T + Send + 'static) {
    use std::sync::atomic::Ordering;
    if busy.swap(true, Ordering::AcqRel) {
        return;
    }
    let (slot, ctx) = (slot.clone(), ctx.clone());
    std::thread::spawn(move || {
        // Free again even if the lookup panics.
        struct Free(&'static std::sync::atomic::AtomicBool);
        impl Drop for Free {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _free = Free(busy);
        let v = look();
        *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
        ctx.request_repaint();
    });
}

#[cfg(feature = "remote")]
static ONLINE_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "remote")]
static SERVICE_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl App {
    /// The settings kept, read once (a fresh ID kept at once, so the ID shown is the one others
    /// will use), and the service and the relay asked about in the background now and then.
    /// `None` when they cannot be read or kept (see `problem`).
    #[cfg(feature = "remote")]
    pub(super) fn access_status(&mut self, ctx: &egui::Context) -> Option<(crate::access::Config, Service)> {
        use crate::access;
        let a = &mut self.access;
        if a.cfg.is_none() && a.problem.is_none() {
            // Kept at once, so the ID shown is the one others will use; settings that cannot be
            // read or kept are said so, never shown as made-up ones.
            match access::stored() {
                Ok(Some(c)) => a.cfg = Some(c),
                Ok(None) => {
                    let c = access::fresh();
                    match access::save(&c) {
                        Ok(()) => a.cfg = Some(c),
                        Err(e) => a.problem = Some(e),
                    }
                }
                Err(e) => a.problem = Some(e),
            }
        }
        let cfg = a.cfg.clone()?;
        if a.service_looked.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(10)) {
            a.service_looked = Some(std::time::Instant::now());
            in_background(&a.service, &SERVICE_BUSY, ctx, || Service { installed: access::service_installed(), runs: access::service_runs(), this: access::service_is_this() });
        }
        // The relay's view of this ID, asked again every 5 s while it is shown.
        if cfg.enabled && a.looked.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(5)) {
            a.looked = Some(std::time::Instant::now());
            let (id, relay) = (cfg.id.clone(), cfg.relay.clone());
            in_background(&a.online, &ONLINE_BUSY, ctx, move || {
                let r = access::online(&id, &relay);
                (id, relay, r)
            });
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(5));
        let svc = a.service.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or_default();
        Some((cfg, svc))
    }

    /// Whether viewers can reach this computer now, as a colour and a line.
    #[cfg(feature = "remote")]
    pub(super) fn access_state(&self, cfg: &crate::access::Config, svc: Service, weak: egui::Color32) -> (egui::Color32, String) {
        use crate::access;
        // Only an answer about this ID at this relay counts.
        let online = self.access.online.lock().unwrap_or_else(|e| e.into_inner()).clone().filter(|(id, relay, _)| *id == cfg.id && *relay == cfg.relay).map(|(_, _, r)| r);
        // When the service runs remote access, this window's own loop is off: the relay says.
        let local = if svc.runs { access::State::Off } else { access::state() };
        if !cfg.enabled {
            return (weak, "Off: nobody can connect by ID".into());
        }
        if !svc.runs && access::locked() {
            return (bad(), "Too many wrong passwords in a row: none is taken for a day, or until you set a new one".into());
        }
        match (&local, online) {
            (access::State::InUse(n), _) => (good(), format!("A viewer is connected ({n})")),
            (access::State::Failed(e), _) => (bad(), e.clone()),
            (_, Some(Ok(true))) => (good(), format!("Ready: waiting at {}", cfg.relay)),
            (_, Some(Ok(false))) => (bad(), if svc.runs { "Not at the relay yet: the service connects in a moment".into() } else { "Connecting to the relay\u{2026}".into() }),
            (_, Some(Err(e))) => (bad(), format!("The relay could not be reached: {e}")),
            (_, None) => (weak, "Connecting\u{2026}".into()),
        }
    }

    /// Keeps changed remote access settings; the relay is asked again about the new ones.
    #[cfg(feature = "remote")]
    pub(super) fn save_access(&mut self, cfg: crate::access::Config) {
        match crate::access::save(&cfg) {
            Ok(()) => {
                self.access.cfg = Some(cfg);
                self.access.looked = None;
                *self.access.online.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            Err(e) => {
                self.error = Some(e);
                self.access.cfg = None;
            }
        }
    }

    /// Installs, updates or removes the service that runs remote access with the computer; it is
    /// looked up again after.
    #[cfg(feature = "remote")]
    pub(super) fn service_change(&mut self, install: bool) {
        let r = if install { crate::access::install_service() } else { crate::access::uninstall_service() };
        self.error = r.err().map(|e| format!("Service: {e}"));
        self.access.service_looked = None;
        *self.access.service.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.service = None;
    }

}

/// An on/off switch.
pub(super) fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(38.0, 20.0);
    let (rect, mut r) = ui.allocate_exact_size(size, egui::Sense::click());
    if r.clicked() {
        *on = !*on;
        r.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(r.id, *on);
    let fill = if *on { super::brand() } else { ui.visuals().widgets.inactive.bg_fill };
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 7.0, egui::Color32::WHITE);
    r
}
