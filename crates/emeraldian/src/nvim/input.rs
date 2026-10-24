//! crossterm input → nvim, and the keys emeraldian keeps for itself.
//!
//! Two things live here, both pure data in and pure data out:
//!
//! * [`key_to_nvim`] and [`mouse_to_nvim`] turn crossterm events into the key
//!   notation and `nvim_input_mouse` arguments nvim understands. Coordinates
//!   become pane-relative here, so the session only has to know where the note
//!   pane starts.
//! * [`is_reserved`] is key policy A: while the nvim pane has focus everything
//!   goes to nvim except a small set of app keys (see the design spec).

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::keymap::Keymap;

/// One key as nvim should receive it, or `None` when it carries no input:
/// key-up events and keys like the bare modifier press the Kitty protocol
/// reports have no nvim meaning, and guessing one would type something.
#[must_use]
pub fn key_to_nvim(key: &KeyEvent) -> Option<String> {
    if key.kind == KeyEventKind::Release {
        return None;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // The key's name inside `<...>`, and what it is when nothing is held:
    // characters go literally, everything else only has its `<name>`.
    let (name, bare) = match key.code {
        // The three characters `nvim_input` reads as notation and would
        // otherwise wait on for the rest of a key name.
        KeyCode::Char('<') => ("lt".to_string(), "<lt>".to_string()),
        KeyCode::Char('\\') => ("Bslash".to_string(), "<Bslash>".to_string()),
        KeyCode::Char('|') => ("Bar".to_string(), "<Bar>".to_string()),
        KeyCode::Char(' ') => ("Space".to_string(), " ".to_string()),
        KeyCode::Char(ch) => {
            // Nvim writes chords in lower case, and the Kitty protocol reports
            // `Ctrl+Shift+x` as the shifted character.
            let ch = if ctrl && ch.is_ascii_alphabetic() {
                ch.to_ascii_lowercase()
            } else {
                ch
            };
            (ch.to_string(), ch.to_string())
        }
        KeyCode::Esc => ("Esc".to_string(), "<Esc>".to_string()),
        KeyCode::Enter => ("CR".to_string(), "<CR>".to_string()),
        KeyCode::Tab => ("Tab".to_string(), "<Tab>".to_string()),
        KeyCode::BackTab => ("S-Tab".to_string(), "<S-Tab>".to_string()),
        KeyCode::Backspace => ("BS".to_string(), "<BS>".to_string()),
        KeyCode::Delete => ("Del".to_string(), "<Del>".to_string()),
        KeyCode::Insert => ("Insert".to_string(), "<Insert>".to_string()),
        KeyCode::Left => ("Left".to_string(), "<Left>".to_string()),
        KeyCode::Right => ("Right".to_string(), "<Right>".to_string()),
        KeyCode::Up => ("Up".to_string(), "<Up>".to_string()),
        KeyCode::Down => ("Down".to_string(), "<Down>".to_string()),
        KeyCode::Home => ("Home".to_string(), "<Home>".to_string()),
        KeyCode::End => ("End".to_string(), "<End>".to_string()),
        KeyCode::PageUp => ("PageUp".to_string(), "<PageUp>".to_string()),
        KeyCode::PageDown => ("PageDown".to_string(), "<PageDown>".to_string()),
        KeyCode::F(n) => (format!("F{n}"), format!("<F{n}>")),
        // Locks, media keys, a bare modifier press: no notation, no input.
        _ => return None,
    };

    // Shift is already part of the character for text (`Shift+a` arrives as
    // `A`), and `BackTab` is its own key, so adding it would send `<S-S-Tab>`
    // or `<S-M-X>`. Only a chord whose character we lower-cased needs it.
    let shift_in_name = match key.code {
        KeyCode::BackTab => true,
        KeyCode::Char(_) => !ctrl,
        _ => false,
    };

    let mut prefix = String::new();
    if ctrl {
        prefix.push_str("C-");
    }
    if shift && !shift_in_name {
        prefix.push_str("S-");
    }
    if alt {
        prefix.push_str("M-");
    }

    if prefix.is_empty() {
        return Some(bare);
    }
    Some(format!("<{prefix}{name}>"))
}

/// One mouse event as `nvim_input_mouse` takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseInput {
    pub button: &'static str,
    pub action: &'static str,
    /// `"S"`, `"C"` and `"A"` for the modifiers held, in that order.
    pub modifier: String,
    pub row: i64,
    pub col: i64,
}

/// Translates a mouse event into nvim's coordinates: zero-based and relative
/// to the pane at `origin` (nvim never learns where the pane sits, so the
/// screen position has to be taken out here). `None` when the event happened
/// outside the pane, which belongs to whatever is drawn there.
#[must_use]
pub fn mouse_to_nvim(ev: &MouseEvent, origin: (u16, u16)) -> Option<MouseInput> {
    let col = i64::from(ev.column.checked_sub(origin.0)?);
    let row = i64::from(ev.row.checked_sub(origin.1)?);

    let (button, action) = match ev.kind {
        MouseEventKind::Down(button) => (button_name(button), "press"),
        MouseEventKind::Up(button) => (button_name(button), "release"),
        MouseEventKind::Drag(button) => (button_name(button), "drag"),
        // nvim ignores the action of a motion, but it needs the event to
        // honour `'mousemoveevent'` and window drag hints.
        MouseEventKind::Moved => ("move", "move"),
        MouseEventKind::ScrollUp => ("wheel", "up"),
        MouseEventKind::ScrollDown => ("wheel", "down"),
        MouseEventKind::ScrollLeft => ("wheel", "left"),
        MouseEventKind::ScrollRight => ("wheel", "right"),
    };

    let mut modifier = String::new();
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        modifier.push('C');
    }
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        modifier.push('S');
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        modifier.push('A');
    }

    Some(MouseInput {
        button,
        action,
        modifier,
        row,
        col,
    })
}

fn button_name(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    }
}

/// Key policy A: while the nvim pane has focus, is this key emeraldian's?
///
/// Kept: `Ctrl+Q`, `Ctrl+E`, the F-keys it dispatches (`F1`, `F2`, `F3`, `F5`,
/// `F8`), history on `Alt+←/→`, and tab switching on `Ctrl+Tab`,
/// `Ctrl+Shift+Tab`, `Ctrl+PgUp` and `Ctrl+PgDn`. Everything else — `Ctrl+O`,
/// `Ctrl+R`, `Ctrl+W`, `Ctrl+D`, `Ctrl+N/P`, `Ctrl+]`, `Tab`, Space — goes to
/// nvim, which is the point: those are the keys nvim users type without
/// thinking.
///
/// `[keys]` remaps are honoured here only when they need Alt or an F-key. A
/// plain `Ctrl` remap keeps working everywhere else in the app but loses to
/// nvim in this pane, where `Ctrl` letters are the editor's.
#[must_use]
pub fn is_reserved(key: &KeyEvent, keymap: &Keymap) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let kept = match key.code {
        // The app treats Shift as part of typing these, so it is not asked for.
        KeyCode::Char(ch) if ctrl && !alt => matches!(ch.to_ascii_lowercase(), 'q' | 'e'),
        // Only unmodified F-keys: a modified one is the user asking for
        // something else, and emeraldian's own handlers are plain.
        KeyCode::F(1 | 2 | 3 | 5 | 8) => key.modifiers.is_empty(),
        KeyCode::Left | KeyCode::Right => alt && !ctrl,
        KeyCode::Tab | KeyCode::BackTab | KeyCode::PageUp | KeyCode::PageDown => ctrl,
        _ => false,
    };
    if kept {
        return true;
    }

    // Which chord matched is not exposed, but a chord only matches a press
    // with the same Alt flag and the same key code — so the press itself says
    // whether the remap is one that survives in the nvim pane.
    let alt_or_function_key = alt || matches!(key.code, KeyCode::F(_));
    alt_or_function_key && keymap.action_for(key).is_some()
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use super::*;
    use crate::keymap::Keymap;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// What nvim receives for a key, written the way a mapping would be.
    fn notation(code: KeyCode, modifiers: KeyModifiers) -> Option<String> {
        key_to_nvim(&press(code, modifiers))
    }

    #[test]
    fn key_notation_table() {
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        let alt = KeyModifiers::ALT;
        let shift = KeyModifiers::SHIFT;

        // Characters that nvim reads as notation have to be escaped, or
        // `nvim_input("<")` would wait for the rest of a key name.
        let table: &[(KeyCode, KeyModifiers, &str)] = &[
            (KeyCode::Char('a'), none, "a"),
            (KeyCode::Char('<'), none, "<lt>"),
            (KeyCode::Char('\\'), none, "<Bslash>"),
            (KeyCode::Char('|'), none, "<Bar>"),
            // A shifted character already is one, so Shift is not repeated.
            (KeyCode::Char('A'), shift, "A"),
            (KeyCode::Char('!'), shift, "!"),
            (KeyCode::Char('a'), ctrl, "<C-a>"),
            (KeyCode::Char('x'), alt, "<M-x>"),
            (KeyCode::Char(' '), none, " "),
            (KeyCode::Char(' '), ctrl, "<C-Space>"),
            (KeyCode::BackTab, shift, "<S-Tab>"),
            (KeyCode::BackTab, ctrl | shift, "<C-S-Tab>"),
            (KeyCode::Char('x'), ctrl | shift, "<C-S-x>"),
            (KeyCode::F(12), none, "<F12>"),
            (KeyCode::Esc, none, "<Esc>"),
            (KeyCode::Enter, none, "<CR>"),
            (KeyCode::Backspace, none, "<BS>"),
            (KeyCode::Tab, none, "<Tab>"),
            (KeyCode::Delete, none, "<Del>"),
            (KeyCode::Insert, none, "<Insert>"),
            (KeyCode::Up, none, "<Up>"),
            (KeyCode::Down, none, "<Down>"),
            (KeyCode::Left, none, "<Left>"),
            (KeyCode::Right, none, "<Right>"),
            (KeyCode::Home, none, "<Home>"),
            (KeyCode::End, none, "<End>"),
            (KeyCode::PageUp, none, "<PageUp>"),
            (KeyCode::PageDown, none, "<PageDown>"),
            (KeyCode::Left, alt, "<M-Left>"),
            // Chords a legacy terminal cannot tell apart from another key: with
            // the Kitty protocol they arrive as themselves, and each has to
            // keep its own notation rather than collapse into Tab or Enter.
            (KeyCode::Char('i'), ctrl, "<C-i>"),
            (KeyCode::Char('m'), ctrl, "<C-m>"),
            (KeyCode::Char('['), ctrl, "<C-[>"),
        ];

        for (code, modifiers, want) in table {
            assert_eq!(
                notation(*code, *modifiers).as_deref(),
                Some(*want),
                "{code:?} with {modifiers:?}"
            );
        }

        // Key-ups are not input; sending one would double every keystroke.
        let release = KeyEvent::new_with_kind(KeyCode::Char('a'), none, KeyEventKind::Release);
        assert_eq!(key_to_nvim(&release), None);
        // And events nvim has no use for are dropped rather than guessed at.
        assert_eq!(notation(KeyCode::CapsLock, none), None);
        assert_eq!(notation(KeyCode::Null, none), None);
    }

    #[test]
    fn input_umlaut_and_emoji_pass_through() {
        // Not every keystroke is a key name: text goes to nvim as text.
        assert_eq!(
            notation(KeyCode::Char('ä'), KeyModifiers::NONE).as_deref(),
            Some("ä")
        );
        assert_eq!(
            notation(KeyCode::Char('😀'), KeyModifiers::NONE).as_deref(),
            Some("😀")
        );
        assert_eq!(
            notation(KeyCode::Char('漢'), KeyModifiers::NONE).as_deref(),
            Some("漢")
        );
    }

    fn keymap(pairs: &[(&str, &str)]) -> Keymap {
        Keymap::new(
            &pairs
                .iter()
                .map(|(name, key)| ((*name).to_string(), (*key).to_string()))
                .collect(),
        )
    }

    #[test]
    fn reserved_keys_policy_a() {
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        let alt = KeyModifiers::ALT;
        let defaults = Keymap::default();

        // Kept by emeraldian: quitting, leaving nvim for the reading view, the
        // F-keys it dispatches, history on Alt+arrows, and tab switching.
        for (code, modifiers) in [
            (KeyCode::Char('q'), ctrl),
            (KeyCode::Char('E'), ctrl),
            (KeyCode::F(1), none),
            (KeyCode::F(2), none),
            (KeyCode::F(3), none),
            (KeyCode::F(5), none),
            (KeyCode::F(8), none),
            (KeyCode::Left, alt),
            (KeyCode::Right, alt),
            (KeyCode::Tab, ctrl),
            (KeyCode::BackTab, ctrl | KeyModifiers::SHIFT),
            (KeyCode::PageUp, ctrl),
            (KeyCode::PageDown, ctrl),
        ] {
            assert!(
                is_reserved(&press(code, modifiers), &defaults),
                "{code:?} with {modifiers:?} should stay with emeraldian"
            );
        }

        // nvim's own keys. Stealing any of these would break muscle memory.
        for (code, modifiers) in [
            (KeyCode::Char('o'), ctrl),
            (KeyCode::Char('r'), ctrl),
            (KeyCode::Char('w'), ctrl),
            (KeyCode::Char('d'), ctrl),
            (KeyCode::Char('n'), ctrl),
            (KeyCode::Char('p'), ctrl),
            (KeyCode::Char(']'), ctrl),
            (KeyCode::Char('i'), ctrl),
            (KeyCode::Char('['), ctrl),
            (KeyCode::Tab, none),
            (KeyCode::Char(' '), none),
            (KeyCode::Char('x'), alt),
            // F-keys emeraldian does not dispatch, and modified F-keys.
            (KeyCode::F(4), none),
            (KeyCode::F(6), none),
            (KeyCode::F(11), none),
            (KeyCode::F(5), ctrl),
        ] {
            assert!(
                !is_reserved(&press(code, modifiers), &defaults),
                "{code:?} with {modifiers:?} should go to nvim"
            );
        }

        // A `[keys]` remap reaches the nvim pane only when it needs Alt or an
        // F-key; plain Ctrl remaps belong to nvim there.
        let remapped = keymap(&[
            ("toggle_left_sidebar", "alt+e"),
            ("toggle_chat", "ctrl+y"),
            ("refresh", "f6"),
        ]);
        assert!(remapped.problems.is_empty());
        assert!(is_reserved(&press(KeyCode::Char('e'), alt), &remapped));
        assert!(is_reserved(&press(KeyCode::Char('E'), alt), &remapped));
        assert!(!is_reserved(&press(KeyCode::Char('y'), ctrl), &remapped));
        assert!(is_reserved(&press(KeyCode::F(6), none), &remapped));
        // The same key without the remap still belongs to nvim.
        assert!(!is_reserved(&press(KeyCode::Char('e'), alt), &defaults));
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_left_click_relative() {
        // The pane starts at (20, 10) on screen; a click at (23, 12) is what
        // nvim calls row 2, col 3 — zero-based, like its own coordinates.
        let click = mouse(MouseEventKind::Down(MouseButton::Left), 23, 12);
        let input = mouse_to_nvim(&click, (20, 10)).expect("the click is inside the pane");
        assert_eq!(input.button, "left");
        assert_eq!(input.action, "press");
        assert_eq!(input.modifier, "");
        assert_eq!((input.row, input.col), (2, 3));

        // Above or left of the pane is somebody else's business.
        assert!(
            mouse_to_nvim(
                &mouse(MouseEventKind::Down(MouseButton::Left), 19, 12),
                (20, 10)
            )
            .is_none()
        );
        assert!(
            mouse_to_nvim(
                &mouse(MouseEventKind::Down(MouseButton::Left), 23, 9),
                (20, 10)
            )
            .is_none()
        );
    }

    #[test]
    fn mouse_wheel_release_and_modifiers() {
        let origin = (0, 0);
        let wheel = mouse_to_nvim(&mouse(MouseEventKind::ScrollUp, 4, 5), origin).unwrap();
        assert_eq!((wheel.button, wheel.action), ("wheel", "up"));
        assert_eq!((wheel.row, wheel.col), (5, 4));

        let wheel_down = mouse_to_nvim(&mouse(MouseEventKind::ScrollDown, 0, 0), origin).unwrap();
        assert_eq!((wheel_down.button, wheel_down.action), ("wheel", "down"));

        let release =
            mouse_to_nvim(&mouse(MouseEventKind::Up(MouseButton::Right), 1, 1), origin).unwrap();
        assert_eq!((release.button, release.action), ("right", "release"));

        let mut shifted = mouse(MouseEventKind::Drag(MouseButton::Left), 2, 3);
        shifted.modifiers = KeyModifiers::SHIFT | KeyModifiers::CONTROL;
        let drag = mouse_to_nvim(&shifted, origin).unwrap();
        assert_eq!((drag.button, drag.action), ("left", "drag"));
        assert_eq!(drag.modifier, "CS");
    }
}
