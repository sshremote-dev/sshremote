//! Without the remote feature the workspace holds terminals only: these stand-ins have no
//! values, so the screen and files code paths compile but are never reached.

use super::*;

/// A connection to an agent; there is never one here.
pub(crate) enum Session {}

pub(crate) enum Ev {}

/// What the user chose in a connection; never made here.
pub(super) enum Prefs {}

pub(super) type Deferred = Box<dyn FnOnce(&mut Remote, &egui::Context) -> Result<(), String>>;

pub(super) struct Remote {
    pub(super) active: bool,
    pub(super) full_toggle: bool,
    pub(super) filling: bool,
    pub(super) grab_wanted: bool,
    pub(super) pane_closed: bool,
    pub(super) spread_asked: bool,
    never: Session,
}

impl Remote {
    pub(super) fn rtt(&self) -> f32 {
        match self.never {}
    }
    pub(super) fn close(&mut self) {
        match self.never {}
    }
    pub(super) fn mode(&self) -> super::workspace::PaneMode {
        match self.never {}
    }
    pub(super) fn set_mode(&mut self, _: super::workspace::PaneMode) -> Result<(), String> {
        match self.never {}
    }
    pub(super) fn take_terms(&mut self) -> Option<crate::termwin::TermWindow> {
        match self.never {}
    }
    pub(super) fn adopt_terms(&mut self, _: crate::termwin::TermWindow) {
        match self.never {}
    }
    pub(super) fn show(&mut self, _: &mut egui::Ui, _: Vec<(u32, bool)>) -> Result<(), String> {
        match self.never {}
    }
    pub(super) fn toolbar_items(&mut self, _: &mut egui::Ui, _: &mut Option<Deferred>) {
        match self.never {}
    }
    #[cfg(test)]
    pub(super) fn shown_rect(&self) -> Option<Rect> {
        match self.never {}
    }
    #[cfg(test)]
    pub(super) fn frames(&self) -> u32 {
        match self.never {}
    }
    #[cfg(test)]
    pub(super) fn set_view_only(&mut self, _: bool) {
        match self.never {}
    }
    #[cfg(test)]
    pub(super) fn view_only(&self) -> bool {
        match self.never {}
    }
    #[cfg(test)]
    pub(super) fn toast_texts(&self) -> Vec<&str> {
        match self.never {}
    }
    pub(super) fn open_panels(&mut self, _: bool, _: bool) {
        match self.never {}
    }
    pub(super) fn background(&mut self, _: &egui::Context) -> Result<(), String> {
        match self.never {}
    }
    pub(super) fn prefs(&self) -> Prefs {
        match self.never {}
    }
    pub(super) fn prefs_on(&self, _: u8) -> Prefs {
        match self.never {}
    }
    pub(super) fn monitors(&self) -> (usize, usize) {
        match self.never {}
    }
    pub(super) fn restore(&mut self, _: Prefs) -> Result<(), String> {
        match self.never {}
    }
    pub(super) fn notify(&mut self, _: &str) {
        match self.never {}
    }
    pub(super) fn paste_text(&mut self, _: &egui::Context, _: String) -> Result<(), String> {
        match self.never {}
    }
}
