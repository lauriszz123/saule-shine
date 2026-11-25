//! The window protocol between Shine in an app and Shine in a display server.
//!
//! A Saule display server — a window manager written in Saule — runs on
//! Shine too, and calls [`crate::compositor`]'s `Compositor.start`. Every
//! Shine program started with `SAULE_DISPLAY` naming that service then opens
//! its window *in* the display server, through the client backend
//! ([`crate::state::client`]): `Window.create`, `Graphics.present` and the
//! input queries work exactly as on a desktop, so an app runs unchanged.
//!
//! It runs on Saule's IPC transport (`saule-ipc`): calls on objects over a
//! Unix socket or a Windows named pipe, with the frame's pixels in shared
//! memory rather than in the messages.
//!
//! **App → server**, calls on the service's root object:
//!
//! | call | |
//! |---|---|
//! | `createWindow(title, w, h, events, resizable?) -> id` | `events` is the app's object for the calls below; `resizable` (default true) says whether the app lets the window be resized |
//! | `attach(id, pixels, w, h)` | shared memory of `w×h` 32-bit pixels, sent once per size |
//! | `commit(id)` | a new frame is in the pixels; **waited for**: the server has taken it when this returns, so the app may draw the next — which also paces the app to the server |
//! | `setTitle(id, title)`, `setCursor(id, name)`, `close(id)` | one-way |
//!
//! **Server → app**, one-way calls on `events`:
//!
//! | call | |
//! |---|---|
//! | `configure(w, h)` | the window's size from now on — in a tiling layout the server decides it |
//! | `key(name, down, shift, ctrl, alt)` | a key, by Shine's key name |
//! | `text(text)` | typed text, already through the server's keyboard layout |
//! | `scale(scale)` | the scale the server draws at, for the window to draw at too |
//! | `pointer(x, y)`, `leave()` | the pointer over the window, in its own coordinates; or gone from it |
//! | `button(button, down)`, `wheel(dx, dy)` | mouse buttons (1 left, 2 right, 3 middle) and the wheel |
//! | `focus(focused)`, `close()` | keyboard focus; the user asked to close the window |

/// The environment variable naming the display service an app should open
/// its windows in. Set by `Compositor.start` for the processes the server
/// starts.
pub const DISPLAY_ENV: &str = "SAULE_DISPLAY";

/// The display service's name when the server does not choose one.
pub const DEFAULT_SERVICE: &str = "saule.display";

/// The key the app exports its events object under. One window per process,
/// so one object.
pub const EVENTS_KEY: u64 = 1;

/// What the events object is, on the wire.
pub const EVENTS_INTERFACE: &str = "ShineWindowEvents";

/// Bytes per pixel in the shared frame: `0x00RRGGBB`, little-endian — the
/// layout of Shine's own framebuffer.
pub const BYTES_PER_PIXEL: usize = 4;
