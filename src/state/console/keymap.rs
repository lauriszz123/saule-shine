//! Linux key codes (`KEY_*` from `linux/input-event-codes.h`) to Shine's keys,
//! and the characters they type on a US keyboard.
//!
//! The kernel reports *positions*, not characters: code 16 is the key left of
//! W whatever is printed on it. Turning that into text is a layout, and this
//! is the US one — the only layout so far (see ROADMAP: "Keyboard layouts
//! beyond US").

use minifb::Key;

/// The Shine key for a Linux key code, or `None` for keys Shine has no name
/// for.
pub(super) fn key(code: u16) -> Option<Key> {
    use Key::*;
    Some(match code {
        1 => Escape,
        2 => Key1,
        3 => Key2,
        4 => Key3,
        5 => Key4,
        6 => Key5,
        7 => Key6,
        8 => Key7,
        9 => Key8,
        10 => Key9,
        11 => Key0,
        12 => Minus,
        13 => Equal,
        14 => Backspace,
        15 => Tab,
        16 => Q,
        17 => W,
        18 => E,
        19 => R,
        20 => T,
        21 => Y,
        22 => U,
        23 => I,
        24 => O,
        25 => P,
        26 => LeftBracket,
        27 => RightBracket,
        28 => Enter,
        29 => LeftCtrl,
        30 => A,
        31 => S,
        32 => D,
        33 => F,
        34 => G,
        35 => H,
        36 => J,
        37 => K,
        38 => L,
        39 => Semicolon,
        40 => Apostrophe,
        41 => Backquote,
        42 => LeftShift,
        43 => Backslash,
        44 => Z,
        45 => X,
        46 => C,
        47 => V,
        48 => B,
        49 => N,
        50 => M,
        51 => Comma,
        52 => Period,
        53 => Slash,
        54 => RightShift,
        55 => NumPadAsterisk,
        56 => LeftAlt,
        57 => Space,
        58 => CapsLock,
        59 => F1,
        60 => F2,
        61 => F3,
        62 => F4,
        63 => F5,
        64 => F6,
        65 => F7,
        66 => F8,
        67 => F9,
        68 => F10,
        69 => NumLock,
        70 => ScrollLock,
        71 => NumPad7,
        72 => NumPad8,
        73 => NumPad9,
        74 => NumPadMinus,
        75 => NumPad4,
        76 => NumPad5,
        77 => NumPad6,
        78 => NumPadPlus,
        79 => NumPad1,
        80 => NumPad2,
        81 => NumPad3,
        82 => NumPad0,
        83 => NumPadDot,
        // KEY_102ND: the extra key beside left shift on ISO keyboards.
        86 => Backslash,
        87 => F11,
        88 => F12,
        96 => NumPadEnter,
        97 => RightCtrl,
        98 => NumPadSlash,
        100 => RightAlt,
        102 => Home,
        103 => Up,
        104 => PageUp,
        105 => Left,
        106 => Right,
        107 => End,
        108 => Down,
        109 => PageDown,
        110 => Insert,
        111 => Delete,
        119 => Pause,
        125 => LeftSuper,
        126 => RightSuper,
        127 => Menu,
        183 => F13,
        184 => F14,
        185 => F15,
        _ => return None,
    })
}

/// Shine's name for a key the window library has none for: the keys of a
/// laptop's top row, and the buttons on its case. They are events only
/// (`keyPressed` with this name), repeating while held as the kernel
/// repeats them.
pub(super) fn named(code: u16) -> Option<&'static str> {
    Some(match code {
        99 => "printscreen",
        113 => "mute",
        114 => "volumedown",
        115 => "volumeup",
        116 => "power",
        142 => "sleep",
        163 => "audionext",
        164 => "audioplay",
        165 => "audioprev",
        224 => "brightnessdown",
        225 => "brightnessup",
        229 => "kbdillumdown",
        230 => "kbdillumup",
        248 => "micmute",
        _ => return None,
    })
}

/// The character a key types on a US keyboard, or `None` for keys that type
/// nothing (arrows, modifiers, Enter, …: those are key events, not text).
///
/// `shift` and `caps` are the state of the shift keys and of Caps Lock. Caps
/// Lock only affects letters, the way every desktop does it. The keypad types
/// digits as if Num Lock were on — Saule Linux has no Num Lock light to say
/// otherwise.
pub(super) fn text(code: u16, shift: bool, caps: bool) -> Option<char> {
    let (plain, shifted) = match code {
        2 => ('1', '!'),
        3 => ('2', '@'),
        4 => ('3', '#'),
        5 => ('4', '$'),
        6 => ('5', '%'),
        7 => ('6', '^'),
        8 => ('7', '&'),
        9 => ('8', '*'),
        10 => ('9', '('),
        11 => ('0', ')'),
        12 => ('-', '_'),
        13 => ('=', '+'),
        26 => ('[', '{'),
        27 => (']', '}'),
        39 => (';', ':'),
        40 => ('\'', '"'),
        41 => ('`', '~'),
        43 | 86 => ('\\', '|'),
        51 => (',', '<'),
        52 => ('.', '>'),
        53 => ('/', '?'),
        57 => (' ', ' '),
        55 => ('*', '*'),
        74 => ('-', '-'),
        78 => ('+', '+'),
        98 => ('/', '/'),
        83 => ('.', '.'),
        71 => ('7', '7'),
        72 => ('8', '8'),
        73 => ('9', '9'),
        75 => ('4', '4'),
        76 => ('5', '5'),
        77 => ('6', '6'),
        79 => ('1', '1'),
        80 => ('2', '2'),
        81 => ('3', '3'),
        82 => ('0', '0'),
        _ => {
            let letter = letter(code)?;
            return Some(if shift != caps {
                letter.to_ascii_uppercase()
            } else {
                letter
            });
        }
    };
    Some(if shift { shifted } else { plain })
}

/// The lowercase letter on a letter key.
fn letter(code: u16) -> Option<char> {
    const ROWS: [(u16, &str); 3] = [(16, "qwertyuiop"), (30, "asdfghjkl"), (44, "zxcvbnm")];
    ROWS.iter().find_map(|&(first, row)| {
        let i = code.checked_sub(first)? as usize;
        row.as_bytes().get(i).map(|&b| b as char)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_follow_shift_and_caps_lock() {
        assert_eq!(text(30, false, false), Some('a'));
        assert_eq!(text(30, true, false), Some('A'));
        assert_eq!(text(30, false, true), Some('A'));
        assert_eq!(text(30, true, true), Some('a'));
        assert_eq!(text(50, false, false), Some('m'));
        assert_eq!(text(16, false, false), Some('q'));
    }

    #[test]
    fn caps_lock_leaves_digits_and_symbols_alone() {
        assert_eq!(text(2, false, true), Some('1'));
        assert_eq!(text(2, true, false), Some('!'));
        assert_eq!(text(53, true, true), Some('?'));
    }

    #[test]
    fn keys_that_type_nothing_have_no_text() {
        for code in [1, 14, 15, 28, 29, 42, 103, 125] {
            assert_eq!(text(code, false, false), None, "code {code}");
        }
    }

    #[test]
    fn every_letter_key_maps_to_its_letter() {
        for (code, ch) in [(16, 'q'), (25, 'p'), (30, 'a'), (38, 'l'), (44, 'z'), (50, 'm')] {
            assert_eq!(text(code, false, false), Some(ch));
            let name = crate::keyboard::key_name(key(code).unwrap()).unwrap();
            assert_eq!(name, ch.to_string());
        }
    }
}
