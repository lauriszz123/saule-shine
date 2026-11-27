//! What the engine needs from the thing it draws on: a desktop window, or the
//! whole screen of a machine with no desktop at all.
//!
//! [`crate::state::Engine`] owns one `Box<dyn Backend>` and asks it for
//! exactly this — everything above (the renderer, key and mouse edges, the
//! event queue, frame pacing) is shared by every backend. Two exist:
//!
//! - [`super::desktop::Desktop`]: a minifb window on X11, Windows or macOS.
//! - `super::console::Console` (Linux): the bare screen through DRM/KMS and
//!   input from evdev, for Saule Linux, where Shine *is* the display.
//!
//! Key identity is minifb's [`Key`] in both: it is the type the keyboard
//! module's name table, edge tracking and repeat timers are built on, and a
//! backend without minifb maps its own key codes onto it.
//!
//! Keystrokes and typed text do not go through this trait. They are pushed,
//! as they arrive, into [`crate::keyboard::TextCollector`] — minifb calls it
//! from inside its event pump, and the console backend calls it the same way
//! from its own — so a key tapped between two polls still reports its press.

use minifb::{CursorStyle, Key};

pub(crate) trait Backend {
    /// `false` once the window has been closed.
    fn is_open(&self) -> bool;

    /// Whether the window was asked to close since this was last asked,
    /// while the program holds closing for itself to decide
    /// (`Window.holdClose`). Only a backend that can leave the window open
    /// says so; on the others the window has already closed.
    fn take_close_asked(&mut self) -> bool {
        false
    }

    /// The drawable size in the units pointer positions are reported in.
    /// Multiply by [`Backend::backing_scale`] for framebuffer pixels.
    fn size(&self) -> (usize, usize);

    /// The display's scale factor, `1.0` meaning 96 DPI (`Window.getScale`).
    fn scale(&self) -> f64;

    /// Framebuffer pixels per unit of [`Backend::size`] and pointer
    /// positions: the Retina backing scale on macOS, `1.0` everywhere else.
    fn backing_scale(&self) -> f64;

    /// Take in whatever input has arrived, without showing a new frame.
    fn pump(&mut self);

    /// Show `buf` (`w`×`h` pixels, `0x00RRGGBB`) and take in input, as
    /// [`Backend::pump`] does.
    fn present(&mut self, buf: &[u32], w: usize, h: usize) -> Result<(), String>;

    /// The GPU this backend presents through, for the renderer to draw on
    /// too; `None` without one.
    fn gpu_device(&self) -> Option<std::rc::Rc<crate::gpu::Device>> {
        None
    }

    /// Show a frame the GPU drew (the screen's texture) without reading it
    /// back. `false` when this backend can't, and the CPU's copy has to be
    /// presented instead.
    fn present_texture(&mut self, _texture: &crate::gpu::Texture) -> Result<bool, String> {
        Ok(false)
    }

    /// Wait until input arrives, `timeout` at the longest, and take it in
    /// as [`Backend::pump`] does. `false`, at once, from a backend with
    /// nothing to wait on: the caller then paces the frame as it always
    /// did.
    ///
    /// For a program with nothing to draw and nothing to do until somebody
    /// touches it: sixty turns of its loop a second to find that out was
    /// a fifth of a netbook's processor for every window left open.
    fn rest(&mut self, _timeout: std::time::Duration) -> bool {
        false
    }

    /// The backend has a reason to show a frame even though nothing new was
    /// drawn — a cursor it draws itself has moved, say.
    fn wants_present(&self) -> bool {
        false
    }

    /// Every key held right now.
    fn keys_down(&self) -> Vec<Key>;

    fn is_key_down(&self, key: Key) -> bool;

    /// The pointer position in [`Backend::size`] units, or `None` while it
    /// is outside the window.
    fn mouse_pos(&self) -> Option<(f64, f64)>;

    /// The pointer position clamped to the window.
    fn mouse_pos_clamped(&self) -> (f64, f64);

    /// Left, right and middle button, held right now.
    fn mouse_buttons(&self) -> [bool; 3];

    /// Wheel movement taken in by the last pump or present, in notches
    /// (`1.0` per click, positive `y` away from the user).
    fn wheel(&self) -> (f64, f64);

    /// Whether the window has keyboard focus.
    fn is_active(&mut self) -> bool;

    fn set_title(&mut self, title: &str);

    fn position(&self) -> (i64, i64);

    fn set_position(&mut self, x: i64, y: i64);

    fn set_topmost(&mut self, topmost: bool);

    fn set_cursor(&mut self, cursor: CursorStyle);

    fn set_cursor_visible(&mut self, visible: bool);

    /// The clipboard this window's display server keeps for all its apps:
    /// `None` where the window is not in one, and the clipboard is the
    /// system's or this process's own (see [`crate::clipboard`]).
    fn clipboard(&mut self) -> Option<String> {
        None
    }

    /// Put `text` on the display server's clipboard; false where there is
    /// none to put it on.
    fn set_clipboard(&mut self, _text: &str) -> bool {
        false
    }
}
