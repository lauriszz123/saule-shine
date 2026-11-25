//! What the machine is typed and pointed at with: `Devices.list()`.
//!
//! On the bare screen Shine reads the input devices itself, so it knows
//! each one: a keyboard, a mouse, a touchpad, and whether it is part of
//! the machine or plugged into it. A program that offers settings for them
//! asks, and offers only those there is something to set on. In a window
//! on another desktop that desktop has the devices, and the list is empty.

use std::sync::Mutex;

use saule_sdk::prelude::*;
use saule_sdk::saule_export;

/// What a device is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum Kind {
    Keyboard,
    Mouse,
    Touchpad,
    Touchscreen,
    /// A pointer that says where it is, not how far it moved: a drawing
    /// tablet, or a virtual machine's pointer.
    Tablet,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Keyboard => "keyboard",
            Kind::Mouse => "mouse",
            Kind::Touchpad => "touchpad",
            Kind::Touchscreen => "touchscreen",
            Kind::Tablet => "tablet",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) kind: Kind,
    /// Part of the machine, not plugged into it.
    pub(crate) built_in: bool,
    /// As the device names itself.
    pub(crate) name: String,
}

static FOUND: Mutex<Vec<Found>> = Mutex::new(Vec::new());

/// The devices there are now, from whatever reads them.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn set(found: Vec<Found>) {
    *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = found;
}

/// `Devices.list()` — each input device as `"kind<TAB>where<TAB>name"`:
/// kind is `keyboard`, `mouse`, `touchpad`, `touchscreen` or `tablet`;
/// where is `built-in` or `connected`. Empty in a window on another
/// desktop.
#[saule_export(class = "Devices", name = "list")]
fn devices_list() -> Result<STable<SString>, String> {
    let table = STable::new();
    for device in FOUND.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        let place = if device.built_in {
            "built-in"
        } else {
            "connected"
        };
        table.push(format!("{}\t{place}\t{}", device.kind.name(), device.name))?;
    }
    Ok(table)
}
