//! Keys from Android's on-screen keyboard.
//!
//! The keyboard types into the activity's own text field (`WordCraftActivity.TextInput`), which
//! hands text to the app as text and every other key (Backspace, Enter, the arrows, a Ctrl
//! shortcut from the keyboard's Ctrl key) as an Android key code with its meta state. This maps
//! those to egui's keys and modifiers. Hardware keyboards don't come this way: winit reads them.
//!
//! Pure mapping, tested on the desktop.

use egui::{Event, Key, Modifiers};

/// Android `KeyEvent` meta-state bits.
const META_SHIFT_ON: i32 = 0x1;
const META_ALT_ON: i32 = 0x2;
const META_CTRL_ON: i32 = 0x1000;

/// egui's modifiers for an Android meta state.
pub fn modifiers(meta: i32) -> Modifiers {
    // Android has no Command key: Ctrl is the shortcut modifier (the Meta/Windows key is ignored).
    let ctrl = meta & META_CTRL_ON != 0;
    Modifiers { alt: meta & META_ALT_ON != 0, ctrl, shift: meta & META_SHIFT_ON != 0, mac_cmd: false, command: ctrl }
}

/// egui's key for an Android key code, for the keys an on-screen keyboard sends as keys.
pub fn key(code: i32) -> Option<Key> {
    Some(match code {
        7 => Key::Num0,
        8 => Key::Num1,
        9 => Key::Num2,
        10 => Key::Num3,
        11 => Key::Num4,
        12 => Key::Num5,
        13 => Key::Num6,
        14 => Key::Num7,
        15 => Key::Num8,
        16 => Key::Num9,
        19 => Key::ArrowUp,
        20 => Key::ArrowDown,
        21 => Key::ArrowLeft,
        22 => Key::ArrowRight,
        29..=54 => letter(code - 29)?,
        61 => Key::Tab,
        62 => Key::Space,
        66 | 160 => Key::Enter,
        67 => Key::Backspace,
        92 => Key::PageUp,
        93 => Key::PageDown,
        111 => Key::Escape,
        112 => Key::Delete,
        122 => Key::Home,
        123 => Key::End,
        124 => Key::Insert,
        _ => return None,
    })
}

fn letter(i: i32) -> Option<Key> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    LETTERS.get(usize::try_from(i).ok()?).copied()
}

/// The events one key press makes: pressed, then released. A key egui doesn't know makes none.
pub fn press(code: i32, meta: i32) -> Vec<Event> {
    let Some(key) = key(code) else {
        log::debug!("keyboard: key code {code} has no egui key");
        return Vec::new();
    };
    let modifiers = modifiers(meta);
    let mut events = vec![
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers },
        Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers },
    ];
    // Ctrl+C / Ctrl+X / Ctrl+V from the keyboard's own Ctrl key: egui-winit turns those into
    // clipboard events on the desktop; do the same here.
    if modifiers.command {
        match key {
            Key::C => events.insert(0, Event::Copy),
            Key::X => events.insert(0, Event::Cut),
            Key::V => events.insert(0, Event::Paste(String::new())),
            _ => {}
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_keys() {
        assert_eq!(key(67), Some(Key::Backspace));
        assert_eq!(key(112), Some(Key::Delete));
        assert_eq!(key(66), Some(Key::Enter));
        assert_eq!(key(160), Some(Key::Enter));
        assert_eq!(key(21), Some(Key::ArrowLeft));
        assert_eq!(key(29), Some(Key::A));
        assert_eq!(key(54), Some(Key::Z));
        assert_eq!(key(7), Some(Key::Num0));
        assert_eq!(key(999), None);
        assert_eq!(key(-1), None);
    }

    #[test]
    fn modifier_bits() {
        assert_eq!(modifiers(0), Modifiers::NONE);
        let m = modifiers(META_CTRL_ON | META_SHIFT_ON);
        assert!(m.ctrl && m.command && m.shift && !m.alt);
        assert!(modifiers(META_ALT_ON).alt);
    }

    #[test]
    fn a_press_is_down_then_up() {
        let e = press(67, 0);
        assert!(matches!(
            e.as_slice(),
            [Event::Key { key: Key::Backspace, pressed: true, .. }, Event::Key { key: Key::Backspace, pressed: false, .. }]
        ));
        assert!(press(999, 0).is_empty());
    }

    #[test]
    fn ctrl_shortcuts_reach_the_clipboard() {
        assert!(matches!(press(31, META_CTRL_ON).first(), Some(Event::Copy)));
        assert!(matches!(press(52, META_CTRL_ON).first(), Some(Event::Cut)));
        assert!(matches!(press(50, META_CTRL_ON).first(), Some(Event::Paste(_))));
        assert!(matches!(press(30, META_CTRL_ON).first(), Some(Event::Key { key: Key::B, .. })), "Ctrl+B is a plain shortcut");
    }
}
