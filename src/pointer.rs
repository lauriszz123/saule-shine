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
    /// A mouse moved fast goes further; a tap and a touch drags; a scroll
    /// let go of while moving carries on.
    quick: bool,
    tap_drag: bool,
    coasts: bool,
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
    quick: true,
    tap_drag: true,
    coasts: true,
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

/// Whether a mouse moved fast goes further ([`quickened`]).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn quickens() -> bool {
    feel().quick
}

/// Whether a touch straight after a tap holds the tap's button.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn tap_drags() -> bool {
    feel().tap_drag
}

/// Whether a scroll let go of while moving carries on.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn coasts() -> bool {
    feel().coasts
}

/// How much further a mouse moved at `speed`, in its counts a second,
/// takes the pointer than its counts say: nothing further when slow,
/// where a pixel is aimed at, rising to twice as far for a sweep of the
/// arm, so that the screen is crossed without lifting the mouse.
///
/// A mouse counts about forty to the millimetre: 400 a second is a
/// centimetre a second, the hand placing the pointer; 1900 is a quick
/// flick of the wrist.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn quickened(speed: f64) -> f64 {
    const FROM: f64 = 400.0;
    const TO: f64 = 1900.0;
    const MOST: f64 = 2.0;
    1.0 + (MOST - 1.0) * ((speed - FROM) / (TO - FROM)).clamp(0.0, 1.0)
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

/// `Mouse.setAcceleration(on)` — a mouse moved fast takes the pointer
/// further than the same distance moved slowly, up to twice as far; off,
/// the pointer goes as far as the mouse says however fast. On unless
/// turned off.
#[saule_export(class = "Mouse", name = "setAcceleration")]
fn mouse_set_acceleration(on: bool) {
    change(|f| f.quick = on);
}

/// `Mouse.setTapAndDrag(on)` — on a touchpad, a touch straight after a
/// tap holds the primary button until it lifts, which drags what was
/// tapped. Only where a tap is a click (`Mouse.setTapToClick`). On unless
/// turned off.
#[saule_export(class = "Mouse", name = "setTapAndDrag")]
fn mouse_set_tap_and_drag(on: bool) {
    change(|f| f.tap_drag = on);
}

/// `Mouse.setTouchpadInertia(on)` — two fingers' scroll let go of while
/// moving carries on, slowing, until it has run down, the pad is touched
/// or a key is pressed. On unless turned off.
#[saule_export(class = "Mouse", name = "setTouchpadInertia")]
fn mouse_set_touchpad_inertia(on: bool) {
    change(|f| f.coasts = on);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mouse_moved_slowly_goes_as_far_as_it_says_and_faster_goes_further() {
        // Placing the pointer: a count is a pixel.
        assert_eq!(quickened(0.0), 1.0);
        assert_eq!(quickened(300.0), 1.0);
        // Faster is further, evenly, and never more than twice.
        let mut last = 1.0;
        for speed in (400..4000).step_by(100) {
            let now = quickened(speed as f64);
            assert!(now >= last && now <= 2.0, "{speed}: {now}");
            assert!(now - last < 0.1, "a step at {speed}: {last} to {now}");
            last = now;
        }
        assert_eq!(quickened(10_000.0), 2.0);
    }

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
