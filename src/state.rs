//! The live engine: the OS window, input latching, frame pacing, and the
//! [`Renderer`] that owns everything drawable.
//!
//! The split matters. Everything that turns a `Graphics.*` call into pixels
//! lives in [`crate::render`] and touches no OS handle, so the whole drawing
//! pipeline can be exercised headlessly. What is left here is the part that
//! genuinely needs a window: opening one, pumping its queue, latching input,
//! and pacing the frame.
//!
//! The interpreter is single-threaded and calls every native symbol from the
//! same thread, so the state lives in a `thread_local!`. That sidesteps the
//! `Send`/`Sync` requirements a `static` would impose (minifb's `Window` is
//! not `Send`), and matches the actual call pattern exactly.

mod input;
mod window;

pub(crate) use input::*;
pub(crate) use window::*;

use std::cell::RefCell;
use std::time::Instant;

use minifb::{Scale, Window, WindowOptions};

use crate::event::Event;
use crate::keyboard::{self, KeyState, TextCollector};
use crate::render::Renderer;

/// Live engine state, created by `Window.create`.
pub struct Engine {
    pub(crate) window: Window,
    /// Render targets, graphics state, and resources.
    pub(crate) r: Renderer,
    /// Per-frame keyboard edges, latched by [`Engine::poll_events`].
    pub(crate) keys: KeyState,
    /// Per-frame mouse edges and wheel, latched by [`Engine::poll_events`].
    pub(crate) mouse: MouseState,
    /// Instant the next presented frame should be released at — the single
    /// pacing point (see [`Engine::present`]).
    pub(crate) next_frame: Instant,
    /// Frame budget, or `None` when the loop runs unthrottled.
    pub(crate) frame_dur: Option<std::time::Duration>,
    /// OS scale factor, refreshed each frame so dragging a window between
    /// monitors of different DPI is picked up.
    pub(crate) scale: f64,
    /// This frame's events, rebuilt by every [`Engine::poll_events`].
    pub(crate) events: Vec<Event>,
    /// Previous pointer position, for the motion delta. `None` until the
    /// pointer is first seen, so the opening frame reports no phantom move.
    pub(crate) last_mouse: Option<(f64, f64)>,
    /// Previous focus state, so only changes are reported.
    pub(crate) last_focused: bool,
    /// Whether the pointer was over the window last frame, so `mouseEntered`
    /// and `mouseLeft` report transitions rather than a state.
    pub(crate) pointer_inside: bool,
    /// Recent clicks per button, for double-click detection.
    pub(crate) last_click: [Option<(Instant, f64, f64)>; 3],
    /// Whether holding Escape should end the loop.
    ///
    /// Love2D's default, and off here. A toolkit uses Escape to dismiss a
    /// modal — the engine cannot both quit and let the app handle it, and
    /// quitting is the one the app cannot opt out of. `Closed` already covers
    /// the real close.
    pub(crate) quit_on_escape: bool,
    /// Whether the `Closed` event has already been delivered, so the close is
    /// reported exactly once rather than on every poll after it.
    pub(crate) close_reported: bool,
}

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}

/// Create the window and framebuffer. Replaces any existing window.
pub fn create(width: i64, height: i64, title: &str, resizable: bool) -> Result<(), String> {
    declare_dpi_aware();

    if width <= 0 || height <= 0 {
        return Err("Window.create: width and height must be positive".into());
    }
    let width = width as usize;
    let height = height as usize;

    let mut window = Window::new(
        title,
        width,
        height,
        WindowOptions {
            resize: resizable,
            scale: Scale::X1,
            // Keep the framebuffer 1:1 with the window: the engine reallocates
            // it on resize rather than letting minifb stretch a stale buffer.
            scale_mode: minifb::ScaleMode::Stretch,
            ..WindowOptions::default()
        },
    )
    .map_err(|e| format!("Window.create: {e}"))?;

    // We pace the loop ourselves in `present` (a single sleep per frame), so
    // leave minifb's own limiter off — otherwise both `update` and
    // `update_with_buffer` would each block and halve the frame rate.
    window.set_target_fps(0);

    // Typed text and key edges arrive through this callback while `update`
    // pumps the queue; `Engine::collect_events` and `KeyState::sync` drain what
    // it collects.
    window.set_input_callback(Box::new(TextCollector));
    keyboard::reset_input();

    let scale = query_scale(&window);

    // The window was asked for in whatever units minifb takes; the framebuffer
    // is physical pixels. On a Retina display those differ from the first
    // frame, and waiting for the first `pollEvents` to notice would open with
    // one frame at half resolution.
    let backing = backing_scale_for(scale);
    let buffer_w = (width as f64 * backing).round() as usize;
    let buffer_h = (height as f64 * backing).round() as usize;

    // Objects bound to the previous window name its registries, not this one.
    crate::objects::reset_bindings();
    PENDING.with(|p| p.borrow_mut().clear());

    ENGINE.with(|cell| {
        *cell.borrow_mut() = Some(Engine {
            window,
            r: Renderer::new(buffer_w, buffer_h),
            keys: KeyState::default(),
            mouse: MouseState::default(),
            next_frame: Instant::now() + DEFAULT_FRAME_DUR,
            frame_dur: Some(DEFAULT_FRAME_DUR),
            scale,
            events: Vec::new(),
            last_mouse: None,
            last_focused: true,
            pointer_inside: false,
            last_click: [None; 3],
            quit_on_escape: false,
            close_reported: false,
        });
    });
    Ok(())
}

/// A resource whose last Saule reference is gone, waiting to be freed.
#[derive(Clone, Copy)]
pub(crate) enum Res {
    Surface(i64),
    Font(i64),
}

thread_local! {
    /// Releases deferred to the next frame. An object's `Drop` can run while
    /// the engine is borrowed — inside a draw call that dropped the last
    /// reference — or while the resource is still bound (the current canvas,
    /// the selected font), so it queues here instead of freeing on the spot.
    static PENDING: RefCell<Vec<Res>> = const { RefCell::new(Vec::new()) };
}

/// Queue `res` to be freed at the next `Graphics.present`.
pub(crate) fn release_later(res: Res) {
    // `try_with`: a drop during thread teardown has nothing left to free into.
    let _ = PENDING.try_with(|p| p.borrow_mut().push(res));
}

/// Free what [`release_later`] queued. A resource still in use — bound as
/// the target, selected, held by a `push("all")` — stays queued for the next
/// frame; one that no longer resolves (a previous window's) is dropped.
pub(crate) fn flush_releases(r: &mut Renderer) {
    let queued = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    if queued.is_empty() {
        return;
    }
    let mut keep = Vec::new();
    for res in queued {
        let outcome = match res {
            Res::Surface(h) => r.release_canvas(h),
            Res::Font(h) => r.release_font(h),
        };
        if let Err(e) = outcome
            && (e.contains("current render target")
                || e.contains("on loan")
                || e.contains("currently selected")
                || e.contains("Graphics.push"))
        {
            keep.push(res);
        }
    }
    PENDING.with(|p| p.borrow_mut().extend(keep));
}

/// Tear the window down (closes it on drop).
pub fn close() {
    ENGINE.with(|cell| *cell.borrow_mut() = None);
}

/// Run `f` against the live engine, or return an error if no window exists.
pub fn with<R>(f: impl FnOnce(&mut Engine) -> R) -> Result<R, String> {
    ENGINE.with(|cell| {
        let mut guard = cell.borrow_mut();
        match guard.as_mut() {
            Some(engine) => Ok(f(engine)),
            None => Err("no window — call Window.create first".into()),
        }
    })
}

/// Run `f` against the renderer. The common case: most `Graphics.*` calls
/// never touch the window at all.
pub fn draw<R>(f: impl FnOnce(&mut Renderer) -> R) -> Result<R, String> {
    with(|engine| f(&mut engine.r))
}
