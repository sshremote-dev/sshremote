//! What this computer's screen shows (Linux, from logind): the login screen, a desktop that is
//! locked, or one signed in and open. Remote access at the login screen serves the first two and
//! leaves an open desktop to the service of the person signed in.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Console {
    /// The login screen (a greeter session), or nobody's session on the screen at all.
    Login,
    /// A desktop session of `uid`, locked.
    Locked(u32),
    /// A desktop session of `uid`, open.
    Open(u32),
}

impl Console {
    /// Whether the person `uid` has the screen, signed in and unlocked.
    pub fn open_for(self, uid: u32) -> bool {
        self == Console::Open(uid)
    }
}

/// The session on seat 0's screen now; None when logind cannot be asked.
pub fn now() -> Option<Console> {
    zbus::block_on(async {
        let conn = zbus::Connection::system().await.ok()?;
        let seat = zbus::Proxy::new(&conn, "org.freedesktop.login1", "/org/freedesktop/login1/seat/seat0", "org.freedesktop.login1.Seat").await.ok()?;
        let (_, path): (String, zbus::OwnedObjectPath) = seat.get_property("ActiveSession").await.ok()?;
        if path.as_str() == "/" {
            return Some(Console::Login);
        }
        let s = zbus::Proxy::new(&conn, "org.freedesktop.login1", path, "org.freedesktop.login1.Session").await.ok()?;
        let class: String = s.get_property("Class").await.ok()?;
        let kind: String = s.get_property("Type").await.unwrap_or_default();
        let (uid, _): (u32, zbus::OwnedObjectPath) = s.get_property("User").await.ok()?;
        let locked: bool = s.get_property("LockedHint").await.unwrap_or(false);
        Some(if class.starts_with("greeter") || class == "lock-screen" || !matches!(kind.as_str(), "wayland" | "x11" | "mir") {
            Console::Login
        } else if locked {
            Console::Locked(uid)
        } else {
            Console::Open(uid)
        })
    })
}
