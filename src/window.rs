//! Window module — real OS window lifecycle and the open/event state that
//! drives a Saule-side game loop.
//!
//! Each function is a plain, safe Rust function annotated with
//! `#[saule_export]`; the SDK generates the C-ABI shim and the manifest entry.

use saule_sdk::saule_export;

use crate::state;

/// The size `Window.create` opens at when none is given: the Saule Linux
/// netbook's screen, so a program sized for it looks the same everywhere.
const DEFAULT_SIZE: (i64, i64) = (1024, 600);

/// `Window.create(width?, height?, title?, resizable?)` — open a real OS
/// window and allocate its framebuffer. Without a size it is 1024×600; give
/// both or neither. On the bare screen (Saule Linux) the window is the whole
/// screen at its own resolution, whatever size is asked for. The title
/// defaults to `"Saule"`, and the window is resizable unless you say
/// otherwise; the framebuffer follows the window, so `getSize` reports the
/// live dimensions.
#[saule_export(class = "Window", name = "create")]
pub(crate) fn window_create(
    width: Option<i64>,
    height: Option<i64>,
    title: Option<String>,
    resizable: Option<bool>,
) -> Result<(), String> {
    let (width, height) = match (width, height) {
        (Some(width), Some(height)) => (width, height),
        (None, None) => DEFAULT_SIZE,
        _ => return Err("Window.create: give both width and height, or neither".into()),
    };
    let title = title.unwrap_or_else(|| "Saule".to_string());
    state::create(width, height, &title, resizable.unwrap_or(true))?;
    crate::timer::reset_clock();
    Ok(())
}

/// `Window.setTitle(title)` — change the title bar text.
#[saule_export(class = "Window", name = "setTitle")]
fn window_set_title(title: String) -> Result<(), String> {
    state::with(|e| e.set_title(&title))
}

/// `Window.getPosition()` — the window's top-left corner in screen
/// coordinates, as `x, y`.
#[saule_export(class = "Window", name = "getPosition")]
fn window_get_position() -> Result<(i64, i64), String> {
    state::with(|e| e.position())
}

/// `Window.setPosition(x, y)` — move the window.
#[saule_export(class = "Window", name = "setPosition")]
fn window_set_position(x: i64, y: i64) -> Result<(), String> {
    state::with(|e| e.set_position(x, y))
}

/// `Window.setTopmost(topmost)` — keep the window above the others.
#[saule_export(class = "Window", name = "setTopmost")]
fn window_set_topmost(topmost: bool) -> Result<(), String> {
    state::with(|e| e.set_topmost(topmost))
}

/// `Window.isFocused()` — whether the window has keyboard focus. Worth checking
/// before running an animation nobody is looking at.
#[saule_export(class = "Window", name = "isFocused")]
fn window_is_focused() -> bool {
    state::with(|e| e.is_focused()).unwrap_or(false)
}

/// `Window.getScale()` — the display's scale factor: `1.0` at 96 DPI, `1.5` at
/// 150%, and so on. Sizes and positions everywhere else are in physical pixels,
/// so this is the number a UI multiplies by to stay the same apparent size on a
/// high-density display.
///
/// Windows reports the real value; other platforms report `1.0` for now.
#[saule_export(class = "Window", name = "getScale")]
fn window_get_scale() -> Result<f64, String> {
    state::with(|e| e.dpi_scale())
}

/// `Window.isOpen()` — the game-loop condition. False once the user closes the
/// window or holds Escape (or if no window exists).
#[saule_export(class = "Window", name = "isOpen")]
pub(crate) fn window_is_open() -> bool {
    state::with(|e| e.is_open()).unwrap_or(false)
}

/// `Window.pollEvents()` — pump the OS event queue and return everything that
/// happened since the last call.
///
/// Each entry is `[kind, payload…]`: `["keyPressed", key, shift, ctrl, alt]`,
/// `["mouseMoved", x, y, dx, dy]`, `["textInput", text]`, and so on. The
/// tagged-union shape does not survive the native ABI, so the Saule side
/// rebuilds it — `UIKit/Events.sau` decodes these into an `Event` enum with
/// tuple payloads to `match` on.
///
/// This is the primary input API. `Keyboard.isDown` and `Mouse.getPos` remain
/// for the question events cannot answer — what is held *right now* — but the
/// per-frame edge queries are gone; a press is an event.
#[saule_export(class = "Window", name = "pollEvents")]
fn window_poll_events() -> Result<saule_sdk::types::STable, String> {
    state::with(|e| {
        e.poll_events();
        crate::event::to_table(e.events())
    })?
}

/// The size of everything, as the person chose it: 0 for "as suits the
/// screen". Bits of an `f64`, so it can be read from anywhere.
static UI_SCALE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static UI_SCALE_READ: std::sync::Once = std::sync::Once::new();

/// The scale chosen, or 0 for none: `Window.setScale`, and before that
/// the environment's `SHINE_SCALE`.
pub(crate) fn chosen_scale() -> f64 {
    UI_SCALE_READ.call_once(|| {
        let from_environment = std::env::var("SHINE_SCALE")
            .ok()
            .and_then(|text| text.trim().parse::<f64>().ok())
            .filter(|scale| scale.is_finite() && *scale > 0.0);
        if let Some(scale) = from_environment {
            UI_SCALE.store(scale.clamp(0.5, 4.0).to_bits(), std::sync::atomic::Ordering::Relaxed);
        }
    });
    f64::from_bits(UI_SCALE.load(std::sync::atomic::Ordering::Relaxed))
}

/// The scale a screen `height` pixels high is drawn at on the bare
/// screen: the one chosen, or one that suits it. Nothing says how large
/// the screen is, only how many pixels it has, so this goes by those: a
/// laptop's 1080 lines want everything a quarter larger than a netbook's
/// 600, and a 4K screen twice.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn screen_scale(height: usize) -> f64 {
    let chosen = chosen_scale();
    if chosen > 0.0 {
        return chosen;
    }
    match height {
        0..=999 => 1.0,
        1000..=1299 => 1.25,
        1300..=1999 => 1.5,
        _ => 2.0,
    }
}

/// `Window.setScale(scale)` — draw everything `scale` times its size on
/// the bare screen: 1 a pixel to a point, 1.5 half as large again. 0 is
/// "as suits the screen", which is how it starts. `Window.getScale()`
/// answers with it from the next frame, and a UI that sizes itself by
/// that follows. In a window on another desktop that desktop's scale
/// stands, and this does nothing.
#[saule_export(class = "Window", name = "setScale")]
fn window_set_scale(scale: f64) {
    let _ = chosen_scale();
    let scale = if scale.is_finite() && scale > 0.0 {
        scale.clamp(0.5, 4.0)
    } else {
        0.0
    };
    UI_SCALE.store(scale.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// Whether the program decides for itself when its window closes.
static CLOSE_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn close_held() -> bool {
    CLOSE_HELD.load(std::sync::atomic::Ordering::Relaxed)
}

/// `Window.holdClose(on)` — leave closing the window to the program. A
/// request to close it (the window manager's close button) then arrives
/// as a `closeRequested` event with the window still open, and the
/// program closes it with `Window.close()` when it is ready: after asking
/// about what was not saved. Under a window manager of Shine's own; a
/// window on another desktop closes as that desktop has it.
#[saule_export(class = "Window", name = "holdClose")]
fn window_hold_close(on: bool) {
    CLOSE_HELD.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// `Window.close()` — close the window and end the loop.
#[saule_export(class = "Window", name = "close")]
fn window_close() {
    state::close();
}

/// `Window.getSize()` — the window's size right now, in pixels, as two
/// values: `local width, height = Window.getSize()`.
///
/// Always the real size, never the one asked for in `Window.create`:
/// - on a desktop, the window as it is now, so it changes when the window
///   is resized (ask every frame, or watch for the `Resized` event);
/// - on the bare screen (Saule Linux), the screen's resolution;
/// - inside a Saule display server, the size the server gave the window
///   with `Compositor.configure`.
#[saule_export(class = "Window", name = "getSize")]
fn window_get_size() -> Result<(i64, i64), String> {
    let (w, h) = state::with(|e| e.size())?;
    Ok((w as i64, h as i64))
}

/// `Window.setQuitOnEscape(enable)` — whether holding Escape ends the loop.
///
/// **Off by default, unlike Love2D.** Escape is how an application dismisses a
/// modal, closes a menu, or cancels an autocomplete — and when the engine
/// quits on it, the app has no way to decline. The `Closed` event and the
/// window's own close button cover the real close; this is here for a game
/// that genuinely wants the Love2D behaviour.
#[saule_export(class = "Window", name = "setQuitOnEscape")]
fn window_set_quit_on_escape(enable: bool) -> Result<(), String> {
    state::with(|e| e.set_quit_on_escape(enable))
}

/// `Window.getQuitOnEscape()` — whether Escape currently ends the loop.
#[saule_export(class = "Window", name = "getQuitOnEscape")]
fn window_get_quit_on_escape() -> bool {
    state::with(|e| e.quit_on_escape()).unwrap_or(false)
}

/// `Window.setTargetFPS(fps)` — cap the loop at `fps` frames per second, or
/// `0` to run unthrottled.
///
/// `Graphics.present` is the only thing pacing a Saule game loop, and the cap
/// used to be a hardcoded 60 with no way to ask for less — a mostly idle UI on
/// battery — or more, on a 120 Hz display.
#[saule_export(class = "Window", name = "setTargetFPS")]
fn window_set_target_fps(fps: i64) -> Result<(), String> {
    state::with(|e| e.set_target_fps(fps))?
}

/// `Window.waitEvents(seconds)` — end a frame that drew nothing by waiting
/// for input, `seconds` at the longest, in place of `Graphics.present()`.
///
/// A program that is doing nothing still went round its loop sixty times
/// a second to find that out. Where the window can tell when something
/// has arrived (a window on a Saule display server), this returns as soon
/// as it has, or when the time is up, and the loop goes round once for
/// it. Anywhere else it is `Graphics.present()`, which paces the frame as
/// it always did. Something drawn since the last present is presented
/// either way.
#[saule_export(class = "Window", name = "waitEvents")]
fn window_wait_events(seconds: f64) -> Result<(), String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("Window.waitEvents: the time must be a non-negative number".into());
    }
    let timeout = std::time::Duration::from_secs_f64(seconds.min(1.0));
    state::with(|e| {
        state::flush_releases(&mut e.r);
        e.rest(timeout)
    })??;
    Ok(())
}

/// `Window.getTargetFPS()` — the current cap, or `0` when uncapped.
#[saule_export(class = "Window", name = "getTargetFPS")]
fn window_get_target_fps() -> Result<i64, String> {
    state::with(|e| e.target_fps())
}
