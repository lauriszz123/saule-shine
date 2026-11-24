//! How the pointer feels: its speed, the wheel's speed and direction, and
//! which button is the primary one. The window manager sets these from
//! Settings; every backend's buttons and wheel go through them before a
//! program sees them, so the window manager's own views and the apps it
//! passes the mouse on to all feel the same.
//!
//! The pointer's speed only reaches mice and touchpads Shine moves itself
//! (the bare screen: see `state::console::input`). In a window on another
//! desktop, that desktop moves the pointer.

use std::sync::Mutex;

use saule_sdk::saule_export;

#[derive(Clone, Copy)]
struct Feel {
    speed: f64,
    scroll: f64,
    natural: bool,
    swapped: bool,
}

static FEEL: Mutex<Feel> = Mutex::new(Feel {
    speed: 1.0,
    scroll: 1.0,
    natural: false,
    swapped: false,
});

fn feel() -> Feel {
    *FEEL.lock().unwrap_or_else(|e| e.into_inner())
}

fn change(f: impl FnOnce(&mut Feel)) {
    f(&mut FEEL.lock().unwrap_or_else(|e| e.into_inner()));
}

/// How far the pointer goes for a mouse's or touchpad's movement.
pub(crate) fn speed() -> f64 {
    feel().speed
}

/// The buttons as the program should see them: left and right swapped for
/// a left hand.
pub(crate) fn buttons(mut buttons: [bool; 3]) -> [bool; 3] {
    if feel().swapped {
        buttons.swap(0, 1);
    }
    buttons
}

/// The wheel as the program should see it: faster or slower, and turned
/// the other way for natural scrolling (the content follows the fingers).
pub(crate) fn wheel((x, y): (f64, f64)) -> (f64, f64) {
    let feel = feel();
    let k = if feel.natural { -feel.scroll } else { feel.scroll };
    (x * k, y * k)
}

/// `Mouse.setPointerSpeed(speed)` — how far the pointer goes for a mouse's
/// movement: 1 as the mouse reports it, 2 twice as far. Kept to 0.1–5.
#[saule_export(class = "Mouse", name = "setPointerSpeed")]
fn mouse_set_pointer_speed(speed: f64) {
    change(|f| f.speed = speed.clamp(0.1, 5.0));
}

/// `Mouse.setScrollSpeed(speed)` — how far the wheel scrolls: 1 as it
/// turns, 2 twice as far. Kept to 0.1–5.
#[saule_export(class = "Mouse", name = "setScrollSpeed")]
fn mouse_set_scroll_speed(speed: f64) {
    change(|f| f.scroll = speed.clamp(0.1, 5.0));
}

/// `Mouse.setNaturalScrolling(on)` — the wheel the other way round, so
/// what is scrolled moves the way the fingers do.
#[saule_export(class = "Mouse", name = "setNaturalScrolling")]
fn mouse_set_natural_scrolling(on: bool) {
    change(|f| f.natural = on);
}

/// `Mouse.setButtonsSwapped(on)` — the right button is the primary one,
/// for a left hand: what it presses arrives as button 1.
#[saule_export(class = "Mouse", name = "setButtonsSwapped")]
fn mouse_set_buttons_swapped(on: bool) {
    change(|f| f.swapped = on);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_feel_changes_buttons_and_wheel() {
        mouse_set_buttons_swapped(true);
        mouse_set_natural_scrolling(true);
        mouse_set_scroll_speed(2.0);
        assert_eq!(buttons([true, false, false]), [false, true, false]);
        assert_eq!(wheel((0.0, 1.0)), (0.0, -2.0));
        mouse_set_buttons_swapped(false);
        mouse_set_natural_scrolling(false);
        mouse_set_scroll_speed(1.0);
        assert_eq!(buttons([true, false, false]), [true, false, false]);
        assert_eq!(wheel((0.0, 1.0)), (0.0, 1.0));
    }
}
