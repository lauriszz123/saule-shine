//! The desktop backend: a minifb window on X11, Windows or macOS, plus the
//! per-platform DPI queries minifb lacks and, on Linux, presenting through
//! X11 shared memory.

use minifb::{CursorStyle, Key, MouseButton, MouseMode, Scale, Window, WindowOptions};

use super::backend::Backend;
use crate::keyboard::TextCollector;

/// One wheel notch in minifb's units: it reports `WHEEL_DELTA` (120) scaled by
/// 0.1, so a single click arrives as 12.0. Normalising to 1.0 per notch is what
/// makes "pixels per notch" mean anything on the Saule side.
const WHEEL_NOTCH: f64 = 12.0;

pub(crate) struct Desktop {
    /// Presents frames through X11 shared memory instead of minifb's
    /// `XPutImage` when the server allows it (see `state/shm.rs`). `None`
    /// means minifb presents. Declared before `window` so it is dropped
    /// first, while the window it draws into still exists.
    #[cfg(target_os = "linux")]
    shm: Option<super::shm::ShmPresenter>,
    window: Window,
    /// The wheel delta read right after the last pump. minifb clears it at
    /// the start of every `update`, so it is captured while it is valid.
    wheel: (f64, f64),
    /// Where the window should go, until it has been shown: see
    /// [`Desktop::place`].
    pending_position: Option<(i64, i64)>,
}

impl Desktop {
    pub(crate) fn open(
        title: &str,
        width: usize,
        height: usize,
        resizable: bool,
    ) -> Result<Desktop, String> {
        let mut window = Window::new(
            title,
            width,
            height,
            WindowOptions {
                resize: resizable,
                scale: Scale::X1,
                // Keep the framebuffer 1:1 with the window: the engine
                // reallocates it on resize rather than letting minifb stretch
                // a stale buffer.
                scale_mode: minifb::ScaleMode::Stretch,
                ..WindowOptions::default()
            },
        )
        .map_err(|e| format!("Window.create: {e}"))?;

        // In the middle of one monitor, whatever the system and however many
        // monitors it has; see `state/placement.rs`. On X11 the move waits
        // for the first frame (see `place`); elsewhere it can be made now.
        let target = super::placement::centred_position(&window, width, height);
        #[cfg(target_os = "linux")]
        let pending_position = target;
        #[cfg(not(target_os = "linux"))]
        let pending_position = {
            if let Some((x, y)) = target {
                window.set_position(x as isize, y as isize);
            }
            None
        };

        // We pace the loop ourselves in `present` (a single sleep per frame),
        // so leave minifb's own limiter off — otherwise both `update` and
        // `update_with_buffer` would each block and halve the frame rate.
        window.set_target_fps(0);

        // Typed text and key edges arrive through this callback while `update`
        // pumps the queue; `Engine::collect_events` and `KeyState::sync` drain
        // what it collects.
        window.set_input_callback(Box::new(TextCollector));

        // `SHINE_NO_SHM=1` keeps minifb presenting, to rule shared memory out
        // when chasing a display problem.
        #[cfg(target_os = "linux")]
        let shm = if std::env::var_os("SHINE_NO_SHM").is_some() {
            None
        } else {
            super::shm::ShmPresenter::new(window.get_window_handle() as std::os::raw::c_ulong)
        };
        #[cfg(target_os = "linux")]
        if std::env::var_os("SHINE_TRACE_PRESENT").is_some() {
            let how = if shm.is_some() {
                "X11 shared memory"
            } else {
                "minifb"
            };
            eprintln!("shine: presenting through {how}");
        }

        Ok(Desktop {
            #[cfg(target_os = "linux")]
            shm,
            window,
            wheel: (0.0, 0.0),
            pending_position,
        })
    }

    /// Move the window to where it was asked to go, once it is on screen.
    ///
    /// On X11, minifb maps the window as it creates it, and the window
    /// manager then places it whenever it gets to the request. A move sent
    /// before that lands first and is overwritten, so a new window's
    /// position — Shine's own centring or an early `Window.setPosition` —
    /// waits for the first frame to be shown. Elsewhere nothing is ever
    /// pending.
    fn place(&mut self) {
        if let Some((x, y)) = self.pending_position.take() {
            self.window.set_position(x as isize, y as isize);
        }
    }

    fn read_wheel(&mut self) {
        self.wheel = self.window.get_scroll_wheel().map_or((0.0, 0.0), |(x, y)| {
            (f64::from(x) / WHEEL_NOTCH, f64::from(y) / WHEEL_NOTCH)
        });
    }

    /// Put the framebuffer on screen through shared memory, then pump the
    /// window's events. `false` when that path is unavailable — never set
    /// up, or found not to work, in which case it is dropped for good and
    /// minifb presents from here on.
    #[cfg(target_os = "linux")]
    fn present_shared(&mut self, buf: &[u32], w: usize, h: usize) -> bool {
        let Some(shm) = self.shm.as_mut() else {
            return false;
        };

        if !shm.present(buf, w, h) {
            if std::env::var_os("SHINE_TRACE_PRESENT").is_some() {
                eprintln!("shine: shared memory failed, presenting through minifb");
            }
            self.shm = None;
            return false;
        }

        self.window.update();
        true
    }

    #[cfg(not(target_os = "linux"))]
    fn present_shared(&mut self, _buf: &[u32], _w: usize, _h: usize) -> bool {
        false
    }
}

impl Backend for Desktop {
    fn is_open(&self) -> bool {
        self.window.is_open()
    }

    fn size(&self) -> (usize, usize) {
        self.window.get_size()
    }

    fn scale(&self) -> f64 {
        query_scale(&self.window)
    }

    fn backing_scale(&self) -> f64 {
        backing_scale_for(self.scale())
    }

    fn pump(&mut self) {
        self.window.update();
        self.read_wheel();
        self.place();
    }

    fn present(&mut self, buf: &[u32], w: usize, h: usize) -> Result<(), String> {
        if !self.present_shared(buf, w, h) {
            self.window
                .update_with_buffer(buf, w, h)
                .map_err(|e| format!("Graphics.present: {e}"))?;
        }
        self.read_wheel();
        self.place();
        Ok(())
    }

    fn keys_down(&self) -> Vec<Key> {
        self.window.get_keys()
    }

    fn is_key_down(&self, key: Key) -> bool {
        self.window.is_key_down(key)
    }

    fn mouse_pos(&self) -> Option<(f64, f64)> {
        self.window
            .get_mouse_pos(MouseMode::Discard)
            .map(|(x, y)| (x as f64, y as f64))
    }

    fn mouse_pos_clamped(&self) -> (f64, f64) {
        self.window
            .get_mouse_pos(MouseMode::Clamp)
            .map_or((0.0, 0.0), |(x, y)| (x as f64, y as f64))
    }

    fn mouse_buttons(&self) -> [bool; 3] {
        [
            self.window.get_mouse_down(MouseButton::Left),
            self.window.get_mouse_down(MouseButton::Right),
            self.window.get_mouse_down(MouseButton::Middle),
        ]
    }

    fn wheel(&self) -> (f64, f64) {
        self.wheel
    }

    fn is_active(&mut self) -> bool {
        self.window.is_active()
    }

    fn set_title(&mut self, title: &str) {
        self.window.set_title(title);
    }

    fn position(&self) -> (i64, i64) {
        let (x, y) = self.window.get_position();
        (x as i64, y as i64)
    }

    fn set_position(&mut self, x: i64, y: i64) {
        if self.pending_position.is_some() {
            self.pending_position = Some((x, y));
        } else {
            self.window.set_position(x as isize, y as isize);
        }
    }

    fn set_topmost(&mut self, topmost: bool) {
        self.window.topmost(topmost);
    }

    fn set_cursor(&mut self, cursor: CursorStyle) {
        self.window.set_cursor_style(cursor);
    }

    fn set_cursor_visible(&mut self, visible: bool) {
        self.window.set_cursor_visibility(visible);
    }
}

/// Ask the OS for this window's scale factor, with 1.0 meaning "96 DPI".
///
/// minifb has no DPI query, so this goes through the native window handle.
/// Only Windows is wired up; the others report 1.0 rather than guessing, which
/// at least makes the limitation visible instead of subtly wrong.
///
#[cfg(windows)]
fn query_scale(window: &Window) -> f64 {
    let handle = window.get_window_handle();

    if handle.is_null() {
        return 1.0;
    }

    // Safety: the handle is minifb's live HWND, and `GetDpiForWindow` only
    // reads from it.
    let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(handle as _) };

    if dpi == 0 {
        return 1.0;
    }

    f64::from(dpi) / 96.0
}

/// Ask the NSWindow for its Retina backing scale.
///
/// minifb has no DPI query and its own macOS `scale_factor` is the integer
/// window magnification from [`minifb::Scale`], unrelated to this — so the
/// engine used to report `1.0` here and draw a point-resolution framebuffer
/// that the OS then magnified. On a Retina display that is a soft, visibly
/// pixelated UI, worst of all in text.
///
/// The backend can do better than that: it presents the framebuffer as a Metal
/// texture on an `MTKView`, whose drawable is already sized in *physical*
/// pixels. Handing it a framebuffer at physical resolution (see
/// [`Backend::backing_scale`]) makes that mapping 1:1 instead of a 2× upscale.
///
/// `mfb_open` returns an `OSXWindow`, which is an `NSWindow` subclass, so this
/// is a plain `backingScaleFactor` message. It goes through the Objective-C
/// runtime directly rather than pulling in an `objc` crate for one selector.
#[cfg(target_os = "macos")]
fn query_scale(window: &Window) -> f64 {
    use std::ffi::{c_char, c_void};

    unsafe extern "C" {
        fn sel_registerName(name: *const c_char) -> *const c_void;
        fn objc_msgSend();
    }

    let handle = window.get_window_handle();
    if handle.is_null() {
        return 1.0;
    }

    // Safety: `handle` is minifb's live `OSXWindow*` (an `NSWindow`), and
    // `backingScaleFactor` is a no-argument property getter returning CGFloat.
    // `objc_msgSend` has no single Rust type, so it is transmuted to the exact
    // signature of the call being made — which is what the ABI requires on
    // aarch64, where a mismatched cast would pass arguments in the wrong
    // registers.
    let scale = unsafe {
        let sel = sel_registerName(c"backingScaleFactor".as_ptr());
        if sel.is_null() {
            return 1.0;
        }
        let send: extern "C" fn(*mut c_void, *const c_void) -> f64 =
            std::mem::transmute(objc_msgSend as *const ());
        send(handle, sel)
    };

    // A window that is not on a screen yet can answer 0.
    if scale.is_finite() && scale >= 1.0 {
        scale
    } else {
        1.0
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn query_scale(_window: &Window) -> f64 {
    1.0
}

/// Framebuffer pixels per unit of the size and pointer coordinates minifb
/// reports, given the display's scale factor.
///
/// On macOS minifb works in *points*, so this is the Retina backing scale and
/// the framebuffer is allocated that much larger. On Windows the process is
/// per-monitor DPI aware and minifb already reports physical pixels, so
/// scaling again would double-count; X11 does no scaling here at all. Both are
/// therefore `1.0` regardless of what `getScale()` reports.
#[cfg(target_os = "macos")]
fn backing_scale_for(scale: f64) -> f64 {
    scale
}

#[cfg(not(target_os = "macos"))]
fn backing_scale_for(_scale: f64) -> f64 {
    1.0
}
