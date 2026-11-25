//! Keyboard layouts: what a key types where the keyboard isn't a US one.
//!
//! On the bare screen the kernel reports a key's *position*; what is
//! printed on it, and typed by it, is the layout's to say. The layouts are
//! XKB's — the descriptions every Linux desktop uses — read by
//! libxkbcommon. A system without them types as a US keyboard, by the
//! table in `state/console/keymap.rs`.
//!
//! A layout only says what keys *type*. Which key is which for shortcuts
//! stays its position, so Ctrl+C is the same two keys on a Russian
//! keyboard as on any other.
//!
//! One layout at a time, for every keyboard: `Keyboard.setLayout("lt")`,
//! or the environment's `XKB_DEFAULT_LAYOUT` when the first key is typed.
//! In a window on another desktop that desktop's layout types, and this
//! one is not used.

#[cfg(target_os = "linux")]
mod accents;
mod list;
#[cfg(target_os = "linux")]
mod xkb;

use saule_sdk::prelude::*;
use saule_sdk::saule_export;

#[cfg(target_os = "linux")]
pub(crate) use xkb::{is_third_level, moved, typed};

/// What a key press types under the layout in use.
#[cfg(target_os = "linux")]
pub(crate) enum Typed {
    /// No layout but the built-in one: `keymap.rs` says what is typed.
    BuiltIn,
    /// The layout's answer: this text, which may be none.
    Text(Option<String>),
}

/// `Keyboard.setLayout(layout, variant)` — type as the XKB layout named
/// (`"lt"`, `"de"`, `"us"`), in its variant (`""` for the usual one).
/// False where it can't be: no such layout, or none in this system; the
/// layout in use stays.
#[saule_export(class = "Keyboard", name = "setLayout")]
fn keyboard_set_layout(layout: String, variant: String) -> bool {
    #[cfg(target_os = "linux")]
    {
        xkb::set(&layout, &variant)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (layout, variant);
        false
    }
}

/// `Keyboard.getLayout()` — the layout in use as it was asked for:
/// `"lt"`, `"de(nodeadkeys)"`; `"us"` for the built-in table.
#[saule_export(class = "Keyboard", name = "getLayout")]
fn keyboard_get_layout() -> String {
    #[cfg(target_os = "linux")]
    {
        xkb::name()
    }
    #[cfg(not(target_os = "linux"))]
    {
        "us".to_string()
    }
}

/// `Keyboard.layouts()` — the layouts this system has, each as
/// `"code<TAB>Name"` (`"lt\tLithuanian"`), by name. Empty where it has
/// none but the built-in US one.
#[saule_export(class = "Keyboard", name = "layouts")]
fn keyboard_layouts() -> Result<STable<SString>, String> {
    let table = STable::new();
    for (code, name) in list::layouts() {
        table.push(format!("{code}\t{name}"))?;
    }
    Ok(table)
}
