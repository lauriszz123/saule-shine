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
    /// A touchpad's own: fingers are not a mouse, and somebody with both
    /// wants each its own way.
    pad_speed: f64,
    pad_scroll: f64,
    pad_natural: bool,
    tap: bool,
}

static FEEL: Mutex<Feel> = Mutex::new(Feel {
    speed: 1.0,
    scroll: 1.0,
    natural: false,
    swapped: false,
    pad_speed: 1.0,
    pad_scroll: 1.0,
    pad_natural: false,
    tap: true,
});

fn feel() -> Feel {
    *FEEL.lock().unwrap_or_else(|e| e.into_inner())
}

fn change(f: impl FnOnce(&mut Feel)) {
    f(&mut FEEL.lock().unwrap_or_else(|e| e.into_inner()));
}

/// How far the pointer goes for a mouse's movement.
pub(crate) fn speed() -> f64 {
    feel().speed
}

/// How far the pointer goes for a finger's movement on a touchpad.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn pad_speed() -> f64 {
    feel().pad_speed
}

/// Whether a tap on a touchpad is a click.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn tap_to_click() -> bool {
    feel().tap
}

/// Two fingers' scrolling on a touchpad, in a wheel's notches, as what a
/// wheel would have to turn for [`wheel`] to make the touchpad's own
/// speed and direction of it: the two are added up before they are
/// reported, and [`wheel`] is applied to the sum.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn pad_wheel((x, y): (f64, f64)) -> (f64, f64) {
    let feel = feel();
    let wheel = if feel.natural { -feel.scroll } else { feel.scroll };
    let pad = if feel.pad_natural { -feel.pad_scroll } else { feel.pad_scroll };
    (x * pad / wheel, y * pad / wheel)
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

/// `Mouse.setTouchpadSpeed(speed)` — how far the pointer goes for a
/// finger's movement on a touchpad: 1 is the pad's width for about the
/// screen's. Kept to 0.1–5.
#[saule_export(class = "Mouse", name = "setTouchpadSpeed")]
fn mouse_set_touchpad_speed(speed: f64) {
    change(|f| f.pad_speed = speed.clamp(0.1, 5.0));
}

/// `Mouse.setTouchpadScrollSpeed(speed)` — how far two fingers scroll.
/// Kept to 0.1–5.
#[saule_export(class = "Mouse", name = "setTouchpadScrollSpeed")]
fn mouse_set_touchpad_scroll_speed(speed: f64) {
    change(|f| f.pad_scroll = speed.clamp(0.1, 5.0));
}

/// `Mouse.setTouchpadNaturalScrolling(on)` — what is scrolled moves the
/// way two fingers on a touchpad do.
#[saule_export(class = "Mouse", name = "setTouchpadNaturalScrolling")]
fn mouse_set_touchpad_natural_scrolling(on: bool) {
    change(|f| f.pad_natural = on);
}

/// `Mouse.setTapToClick(on)` — a tap on a touchpad is a click: one finger
/// the primary button, two the secondary, three the middle one. On unless
/// turned off.
#[saule_export(class = "Mouse", name = "setTapToClick")]
fn mouse_set_tap_to_click(on: bool) {
    change(|f| f.tap = on);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_touchpad_scrolls_its_own_way_whatever_the_wheel_does() {
        mouse_set_natural_scrolling(true);
        mouse_set_scroll_speed(2.0);
        mouse_set_touchpad_scroll_speed(0.5);
        // Through `wheel`, as the sum of both is: half, and not reversed.
        assert_eq!(wheel(pad_wheel((0.0, 4.0))), (0.0, 2.0));
        mouse_set_touchpad_natural_scrolling(true);
        assert_eq!(wheel(pad_wheel((0.0, 4.0))), (0.0, -2.0));
        mouse_set_natural_scrolling(false);
        mouse_set_scroll_speed(1.0);
        mouse_set_touchpad_scroll_speed(1.0);
        mouse_set_touchpad_natural_scrolling(false);
    }

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
