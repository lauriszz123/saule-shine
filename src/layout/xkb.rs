//! A layout read by libxkbcommon, and the one in use.
//!
//! The library and XKB's descriptions are looked for when a layout is
//! asked for, not linked: a system without them, or without the layout,
//! keeps what it had.

use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::c_char;
use std::ptr;

use xkbcommon_dl::{
    XkbCommon, xkb_context, xkb_context_flags, xkb_key_direction, xkb_keymap,
    xkb_keymap_compile_flags, xkb_rule_names, xkb_state, xkbcommon_option,
};

use super::Typed;
use super::accents::{self, Accent};

/// The kernel's key codes are XKB's less eight: a number X11 has carried
/// since keyboards had fewer keys.
const OFFSET: u32 = 8;

/// The kernel's code for Num Lock,
const KEY_NUMLOCK: u16 = 69;
/// and the key symbol of the key that picks a key's third character
/// (AltGr).
const ISO_LEVEL3_SHIFT: u32 = 0xfe03;

struct Layout {
    xkb: &'static XkbCommon,
    context: *mut xkb_context,
    keymap: *mut xkb_keymap,
    state: *mut xkb_state,
    /// The accent of a dead key just pressed, waiting for its letter.
    dead: Option<Accent>,
    name: String,
}

impl Layout {
    fn load(layout: &str, variant: &str) -> Option<Layout> {
        let xkb = xkbcommon_option()?;
        let layout_c = CString::new(layout).ok()?;
        let variant_c = CString::new(variant).ok()?;
        let names = xkb_rule_names {
            rules: ptr::null(),
            model: ptr::null(),
            layout: layout_c.as_ptr(),
            variant: variant_c.as_ptr(),
            options: ptr::null(),
        };

        // Safety: each call is given what the one before it returned, checked
        // for null, and the names outlive the call that reads them.
        unsafe {
            let context = (xkb.xkb_context_new)(xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
            if context.is_null() {
                return None;
            }
            let keymap = (xkb.xkb_keymap_new_from_names)(
                context,
                &names,
                xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            );
            if keymap.is_null() {
                (xkb.xkb_context_unref)(context);
                return None;
            }
            let state = (xkb.xkb_state_new)(keymap);
            if state.is_null() {
                (xkb.xkb_keymap_unref)(keymap);
                (xkb.xkb_context_unref)(context);
                return None;
            }
            let name = if variant.is_empty() {
                layout.to_string()
            } else {
                format!("{layout}({variant})")
            };
            let mut layout = Layout {
                xkb,
                context,
                keymap,
                state,
                dead: None,
                name,
            };
            // The keypad types digits until Num Lock says otherwise, as it
            // does with the built-in table.
            layout.moved(KEY_NUMLOCK, true);
            layout.moved(KEY_NUMLOCK, false);
            Some(layout)
        }
    }

    /// The key symbol the key has as things stand.
    fn symbol(&self, code: u16) -> u32 {
        // Safety: a live state, and any key code is one it accepts.
        unsafe { (self.xkb.xkb_state_key_get_one_sym)(self.state, u32::from(code) + OFFSET) }
    }

    /// A key went down or came up: shift, AltGr and the locks follow it.
    fn moved(&mut self, code: u16, down: bool) {
        let direction = if down {
            xkb_key_direction::XKB_KEY_DOWN
        } else {
            xkb_key_direction::XKB_KEY_UP
        };
        // Safety: a live state, and any key code is one it accepts.
        unsafe {
            (self.xkb.xkb_state_update_key)(self.state, u32::from(code) + OFFSET, direction);
        }
    }

    /// What the key types as things stand, before it is counted as down:
    /// nothing for a key that types nothing, or a dead key, whose accent
    /// waits for the next letter.
    fn typed(&mut self, code: u16) -> Option<String> {
        let key = u32::from(code) + OFFSET;
        let sym = self.symbol(code);
        // Safety: a live state; the buffer is as long as the call is told.
        let text = unsafe {
            let mut buffer = [0u8; 16];
            let length = (self.xkb.xkb_state_key_get_utf8)(
                self.state,
                key,
                buffer.as_mut_ptr().cast::<c_char>(),
                buffer.len(),
            );
            let length = usize::try_from(length).unwrap_or(0).min(buffer.len() - 1);
            String::from_utf8_lossy(&buffer[..length]).into_owned()
        };

        if let Some(accent) = accents::accent(sym) {
            // Twice is the accent itself.
            let before = self.dead.replace(accent);
            return before.map(|_| {
                self.dead = None;
                accent.alone.to_string()
            });
        }
        let mut letters = text.chars().filter(|c| !c.is_control());
        let letter = letters.next()?;
        match self.dead.take() {
            Some(accent) => Some(accents::composed(accent, letter)),
            None => Some(std::iter::once(letter).chain(letters).collect()),
        }
    }
}

impl Drop for Layout {
    fn drop(&mut self) {
        // Safety: each was made by `load` and is let go of once, here.
        unsafe {
            (self.xkb.xkb_state_unref)(self.state);
            (self.xkb.xkb_keymap_unref)(self.keymap);
            (self.xkb.xkb_context_unref)(self.context);
        }
    }
}

thread_local! {
    /// The layout in use; `None` is the US table.
    static CURRENT: RefCell<Option<Layout>> = const { RefCell::new(None) };
    static ASKED_ENVIRONMENT: RefCell<bool> = const { RefCell::new(false) };
}

/// Use `layout` (XKB's name for it: `lt`, `de`, `us`) in its `variant`
/// (`""` for the usual one). False when it can't be: no such layout, or
/// no layouts in this system at all; what was in use stays.
pub(crate) fn set(layout: &str, variant: &str) -> bool {
    ASKED_ENVIRONMENT.with(|asked| *asked.borrow_mut() = true);
    match Layout::load(layout, variant) {
        Some(loaded) => {
            CURRENT.with(|current| *current.borrow_mut() = Some(loaded));
            true
        }
        None => false,
    }
}

/// The layout in use, as it was asked for: `lt`, `de(nodeadkeys)`; `us`
/// for the built-in table.
pub(crate) fn name() -> String {
    from_environment();
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .map_or_else(|| "us".to_string(), |layout| layout.name.clone())
    })
}

/// The first time anything is typed: the layout the environment names.
fn from_environment() {
    let asked = ASKED_ENVIRONMENT.with(|asked| asked.replace(true));
    if asked {
        return;
    }
    let layout = std::env::var("XKB_DEFAULT_LAYOUT").unwrap_or_default();
    if !layout.is_empty() && layout != "us" {
        let variant = std::env::var("XKB_DEFAULT_VARIANT").unwrap_or_default();
        set(&layout, &variant);
    }
}

/// A key went down, or is held and repeating, under the layout in use:
/// what it types. Called before [`moved`] for the same press.
pub(crate) fn typed(code: u16) -> Typed {
    from_environment();
    CURRENT.with(|current| match current.borrow_mut().as_mut() {
        Some(layout) => Typed::Text(layout.typed(code)),
        None => Typed::BuiltIn,
    })
}

/// A key went down or came up.
pub(crate) fn moved(code: u16, down: bool) {
    CURRENT.with(|current| {
        if let Some(layout) = current.borrow_mut().as_mut() {
            layout.moved(code, down);
        }
    });
}

/// Whether the key is the layout's AltGr: then it is part of typing, and
/// not the Alt of a shortcut.
pub(crate) fn is_third_level(code: u16) -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(|layout| layout.symbol(code) == ISO_LEVEL3_SHIFT)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_1: u16 = 2;
    const KEY_Q: u16 = 16;
    const KEY_E: u16 = 18;
    const KEY_LEFTSHIFT: u16 = 42;
    const KEY_RIGHTALT: u16 = 100;
    const KEY_KP1: u16 = 79;

    /// A layout, where this machine has libxkbcommon and XKB's
    /// descriptions; the tests say nothing where it has not.
    fn layout(name: &str, variant: &str) -> Option<Layout> {
        Layout::load(name, variant)
    }

    fn press(layout: &mut Layout, code: u16) -> Option<String> {
        let typed = layout.typed(code);
        layout.moved(code, true);
        layout.moved(code, false);
        typed
    }

    #[test]
    fn a_layout_types_what_is_printed_on_its_keys() {
        let Some(mut lt) = layout("lt", "") else {
            return;
        };
        // The Lithuanian number row is letters; AltGr has the digits.
        assert_eq!(press(&mut lt, KEY_1).as_deref(), Some("\u{0105}"));
        lt.moved(KEY_LEFTSHIFT, true);
        assert_eq!(press(&mut lt, KEY_1).as_deref(), Some("\u{0104}"));
        lt.moved(KEY_LEFTSHIFT, false);
        assert!(lt.symbol(KEY_RIGHTALT) == ISO_LEVEL3_SHIFT);
        lt.moved(KEY_RIGHTALT, true);
        assert_eq!(press(&mut lt, KEY_1).as_deref(), Some("1"));
        lt.moved(KEY_RIGHTALT, false);
        assert_eq!(press(&mut lt, KEY_Q).as_deref(), Some("q"));
    }

    #[test]
    fn a_dead_key_waits_for_its_letter() {
        let Some(mut de) = layout("de", "") else {
            return;
        };
        // The key right of the German 0-ß pair is a dead acute accent.
        const KEY_EQUAL: u16 = 13;
        assert_eq!(press(&mut de, KEY_EQUAL), None);
        assert_eq!(press(&mut de, KEY_E).as_deref(), Some("\u{00e9}"));
        // Twice is the accent itself.
        assert_eq!(press(&mut de, KEY_EQUAL), None);
        assert_eq!(press(&mut de, KEY_EQUAL).as_deref(), Some("\u{00b4}"));
        assert_eq!(press(&mut de, KEY_E).as_deref(), Some("e"));
    }

    #[test]
    fn the_keypad_types_digits_from_the_start() {
        let Some(mut us) = layout("us", "") else {
            return;
        };
        assert_eq!(press(&mut us, KEY_KP1).as_deref(), Some("1"));
    }

    #[test]
    fn a_layout_there_is_none_of_is_refused() {
        if layout("us", "").is_none() {
            return;
        }
        assert!(layout("no-such-layout", "").is_none());
    }
}
