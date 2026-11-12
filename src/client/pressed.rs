//! Keys pressed and let go at the host, modifiers kept in line.

use super::*;

impl Viewer {
    /// A key press or release, with the modifiers brought in line first.
    pub(super) fn key_state(&mut self, k: &event::KeyEvent) -> io::Result<()> {
        let Some((keysym, _)) = keys::keysym(k) else { return Ok(()) };
        let keysym = keys::base(keysym);
        let mods = keys::mod_bits(keys::altgr_free(k));
        if k.kind != KeyEventKind::Release {
            self.sync_mods(mods)?;
            self.held.insert(keysym);
            return self.sess.send(ToAgent::KeyState { keysym, down: true });
        }
        // A release reported as another symbol (Shift let go first: '1' for the '!' pressed) matches
        // no key held: every key held is let go, as one let go early does no harm and one left
        // down repeats at the host.
        let up: Vec<u32> = if self.held.remove(&keysym) { vec![keysym] } else { std::mem::take(&mut self.held).into_iter().collect() };
        for keysym in up {
            self.sess.send(ToAgent::KeyState { keysym, down: false })?;
        }
        self.sync_mods(mods)
    }

    /// Presses or releases Shift, Ctrl, Alt and Super on the host to match `mods`.
    pub(super) fn sync_mods(&mut self, mods: u8) -> io::Result<()> {
        for (bit, keysym) in keys::MODIFIERS {
            let want = mods & bit != 0;
            if want != (self.held_mods & bit != 0) {
                self.sess.send(ToAgent::KeyState { keysym, down: want })?;
                self.held_mods ^= bit;
            }
        }
        Ok(())
    }

    /// Modifiers still held at the host that are no longer down here: the Windows console reports
    /// no event for Shift, Ctrl or Alt alone, so their release is looked up.
    pub(super) fn check_mods(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        if self.held_mods != 0 && self.probe.is_none() {
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT};
            let down = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) } < 0;
            let mut now = 0;
            for (bit, vk) in [(crate::proto::MOD_SHIFT, VK_SHIFT), (crate::proto::MOD_CTRL, VK_CONTROL), (crate::proto::MOD_ALT, VK_MENU), (MOD_SUPER, VK_LWIN)] {
                if down(vk) {
                    now |= bit;
                }
            }
            if now & self.held_mods != self.held_mods {
                self.sync_mods(now & self.held_mods)?;
            }
        }
        Ok(())
    }

    /// Lets go of everything held on the host (the menu, leaving).
    pub(super) fn release_all(&mut self) -> io::Result<()> {
        for keysym in std::mem::take(&mut self.held) {
            self.sess.send(ToAgent::KeyState { keysym, down: false })?;
        }
        self.sync_mods(0)
    }
}
