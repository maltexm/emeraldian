//! Keeping the terminal in raw mode.
//!
//! The picture probe in `images::Images::probe` asks the terminal what it can
//! draw from a helper thread inside `ratatui-image`. That thread switches the
//! terminal to non-canonical mode, remembers the settings it found, and puts
//! them back when it is done. When the terminal answers after the probe's
//! two-second window — inside tmux, sometimes in kitty — `probe` has
//! already returned, the TUI has already gone raw, and the helper thread then
//! restores the *cooked* settings underneath it. Keys stop arriving one at a
//! time and `^O` is echoed instead of opening the switcher.
//!
//! The thread cannot be stopped, so the loop re-asserts raw mode whenever it
//! finds the terminal cooked again.

/// Puts `fd` back into raw mode if line editing came back on.
///
/// Returns whether anything had to be fixed. A terminal that is still raw is
/// not touched, and one that can't be queried (not a terminal at all) is left
/// alone.
#[cfg(unix)]
pub fn ensure_raw(fd: impl std::os::fd::AsFd) -> bool {
    use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

    let Ok(mut termios) = tcgetattr(&fd) else {
        return false;
    };
    if !termios.local_modes.contains(LocalModes::ICANON) {
        return false;
    }
    termios.make_raw();
    tcsetattr(&fd, OptionalActions::Now, &termios).is_ok()
}

/// Same, for the terminal the app is attached to.
#[cfg(unix)]
pub fn ensure_stdin_raw() -> bool {
    ensure_raw(std::io::stdin())
}

#[cfg(not(unix))]
pub fn ensure_stdin_raw() -> bool {
    false
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::fd::OwnedFd;

    use rustix::fs::{Mode, OFlags, open};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use rustix::termios::{InputModes, LocalModes, OptionalActions, tcgetattr, tcsetattr};

    use super::ensure_raw;

    /// A pty pair; the slave end is what a program sees as its terminal.
    /// The master is returned too so the pair stays open.
    fn pty() -> (OwnedFd, OwnedFd) {
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("openpt");
        grantpt(&master).expect("grantpt");
        unlockpt(&master).expect("unlockpt");
        let name = ptsname(&master, Vec::new()).expect("ptsname");
        let slave = open(&name, OFlags::RDWR | OFlags::NOCTTY, Mode::empty()).expect("open slave");
        (master, slave)
    }

    #[test]
    fn a_cooked_terminal_is_made_raw_again() {
        let (_master, slave) = pty();
        let before = tcgetattr(&slave).expect("tcgetattr");
        assert!(
            before
                .local_modes
                .contains(LocalModes::ICANON | LocalModes::ECHO),
            "a fresh pty is cooked"
        );

        assert!(
            ensure_raw(&slave),
            "it reports that it had to fix something"
        );

        let after = tcgetattr(&slave).expect("tcgetattr");
        assert!(!after.local_modes.contains(LocalModes::ICANON));
        assert!(!after.local_modes.contains(LocalModes::ECHO));
        assert!(
            !after.input_modes.contains(InputModes::ICRNL),
            "Enter must arrive as a carriage return, not be translated"
        );
    }

    #[test]
    fn a_raw_terminal_is_left_alone() {
        let (_master, slave) = pty();
        let mut raw = tcgetattr(&slave).expect("tcgetattr");
        raw.make_raw();
        // Something a raw terminal may legitimately carry beyond cfmakeraw.
        raw.local_modes.insert(LocalModes::TOSTOP);
        tcsetattr(&slave, OptionalActions::Now, &raw).expect("tcsetattr");

        assert!(!ensure_raw(&slave), "nothing to fix");

        let after = tcgetattr(&slave).expect("tcgetattr");
        assert_eq!(after.local_modes, raw.local_modes);
        assert_eq!(after.input_modes, raw.input_modes);
    }

    #[test]
    fn something_that_is_not_a_terminal_is_ignored() {
        let file = std::fs::File::open("/dev/null").expect("open");
        assert!(!ensure_raw(&file));
    }
}
