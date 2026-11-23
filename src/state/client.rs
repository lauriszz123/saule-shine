//! The client backend: a window inside a Saule display server.
//!
//! Chosen by `Window.create` when `SAULE_DISPLAY` names a display service
//! (see [`crate::protocol`]). To the program it is a window like any other:
//! frames go out through shared memory, input comes in as calls from the
//! server, and the size is whatever the server configures — so a program
//! written for a desktop window runs here unchanged.

use std::time::Duration;

use minifb::{CursorStyle, InputCallback, Key};
use saule_ipc::{CallError, ConnId, Handler, Looper, ObjRef, ROOT, SharedMemory, Wire};

use super::backend::Backend;
use crate::keyboard::{self, TextCollector};
use crate::protocol::{BYTES_PER_PIXEL, EVENTS_INTERFACE, EVENTS_KEY};

/// What the server has told this window, gathered by [`Events`] as its calls
/// arrive.
#[derive(Default)]
struct Input {
    size: (usize, usize),
    open: bool,
    focused: bool,
    pointer: Option<(f64, f64)>,
    buttons: [bool; 3],
    /// Wheel movement taken in during the current pump.
    wheel: (f64, f64),
    held: Vec<Key>,
}

/// Answers the server's calls on this window's events object.
struct Events<'a> {
    input: &'a mut Input,
}

fn int(w: &Wire) -> Option<i64> {
    match w {
        Wire::Int(i) => Some(*i),
        Wire::Float(f) => Some(*f as i64),
        _ => None,
    }
}

fn float(w: &Wire) -> Option<f64> {
    match w {
        Wire::Float(f) => Some(*f),
        Wire::Int(i) => Some(*i as f64),
        _ => None,
    }
}

fn boolean(w: &Wire) -> bool {
    matches!(w, Wire::Bool(true))
}

impl Handler for Events<'_> {
    fn call(
        &mut self,
        _: &mut Looper,
        _: ConnId,
        key: u64,
        method: &str,
        args: Vec<Wire>,
    ) -> Result<Vec<Wire>, String> {
        if key != EVENTS_KEY {
            return Err(format!("no object {key} here"));
        }
        let input = &mut *self.input;
        match (method, args.as_slice()) {
            ("configure", [w, h]) => {
                if let (Some(w), Some(h)) = (int(w), int(h))
                    && w > 0
                    && h > 0
                {
                    input.size = (w as usize, h as usize);
                }
            }
            ("key", [Wire::Str(name), down, ..]) => {
                if let Some(key) = keyboard::parse_key(name) {
                    let down = boolean(down);
                    if down && !input.held.contains(&key) {
                        input.held.push(key);
                    } else if !down {
                        input.held.retain(|&k| k != key);
                    }
                    TextCollector.set_key_state(key, down);
                }
            }
            ("text", [Wire::Str(text)]) => {
                let mut collector = TextCollector;
                for ch in text.chars() {
                    collector.add_char(ch as u32);
                }
            }
            ("pointer", [x, y]) => {
                if let (Some(x), Some(y)) = (float(x), float(y)) {
                    input.pointer = Some((x, y));
                }
            }
            ("leave", []) => input.pointer = None,
            ("button", [b, down]) => {
                if let Some(b) = int(b)
                    && (1..=3).contains(&b)
                {
                    input.buttons[b as usize - 1] = boolean(down);
                }
            }
            ("wheel", [dx, dy]) => {
                input.wheel.0 += float(dx).unwrap_or(0.0);
                input.wheel.1 += float(dy).unwrap_or(0.0);
            }
            ("focus", [focused]) => input.focused = boolean(focused),
            ("close", []) => input.open = false,
            // A newer server's call this build does not know: ignored, so
            // the two can be updated one at a time.
            _ => {}
        }
        Ok(Vec::new())
    }

    fn disconnected(&mut self, _: &mut Looper, _: ConnId) {
        // The display server has gone: the window with it.
        self.input.open = false;
    }
}

/// The frame's shared memory, at the size it was attached at.
struct Frame {
    mem: SharedMemory,
    w: usize,
    h: usize,
}

pub(crate) struct Client {
    looper: Looper,
    conn: ConnId,
    id: i64,
    input: Input,
    frame: Option<Frame>,
    /// The wheel movement of the last pump, as the engine reads it.
    wheel: (f64, f64),
}

impl Client {
    pub(crate) fn open(service: &str, title: &str, width: usize, height: usize) -> Result<Client, String> {
        let fail = |e: &dyn std::fmt::Display| format!("Window.create: display `{service}`: {e}");
        let mut looper = Looper::new();
        let conn = looper.connect(service).map_err(|e| fail(&e))?;
        let mut input = Input {
            size: (width, height),
            open: true,
            focused: true,
            ..Input::default()
        };
        let reply = looper
            .call(
                &mut Events { input: &mut input },
                conn,
                ROOT,
                "createWindow",
                vec![
                    Wire::Str(title.to_string()),
                    Wire::Int(width as i64),
                    Wire::Int(height as i64),
                    Wire::Object(ObjRef::Local {
                        key: EVENTS_KEY,
                        interface: EVENTS_INTERFACE.into(),
                    }),
                ],
            )
            .map_err(|e| fail(&e))?;
        let id = reply.first().and_then(int).ok_or_else(|| fail(&"no window id came back"))?;
        Ok(Client {
            looper,
            conn,
            id,
            input,
            frame: None,
            wheel: (0.0, 0.0),
        })
    }

    /// One-way call about this window; a server that has gone is the window
    /// closing, which the next pump reports.
    fn tell(&mut self, method: &str, mut args: Vec<Wire>) {
        args.insert(0, Wire::Int(self.id));
        if self.looper.send(self.conn, ROOT, method, args) == Err(CallError::Disconnected) {
            self.input.open = false;
        }
    }

    /// Give the server shared memory for frames of `w×h`.
    fn attach(&mut self, w: usize, h: usize) -> Result<(), String> {
        let mem = SharedMemory::create(w * h * BYTES_PER_PIXEL)
            .map_err(|e| format!("Graphics.present: no memory for the frame: {e}"))?;
        let handle = mem.share().map_err(|e| format!("Graphics.present: {e}"))?;
        let args = vec![
            Wire::Int(self.id),
            Wire::Shm(handle, mem.len() as u64),
            Wire::Int(w as i64),
            Wire::Int(h as i64),
        ];
        let Client { looper, conn, input, .. } = self;
        match looper.call(&mut Events { input }, *conn, ROOT, "attach", args) {
            Ok(_) => {
                self.frame = Some(Frame { mem, w, h });
                Ok(())
            }
            Err(CallError::Disconnected) => {
                self.input.open = false;
                Ok(())
            }
            Err(e) => Err(format!("Graphics.present: {e}")),
        }
    }
}

impl Backend for Client {
    fn is_open(&self) -> bool {
        self.input.open
    }

    fn size(&self) -> (usize, usize) {
        self.input.size
    }

    fn scale(&self) -> f64 {
        1.0
    }

    fn backing_scale(&self) -> f64 {
        1.0
    }

    fn pump(&mut self) {
        self.input.wheel = (0.0, 0.0);
        let Client { looper, input, .. } = self;
        // Everything that has arrived, without waiting; a few rounds at
        // most, in case answering one call let another in.
        for _ in 0..8 {
            match looper.dispatch(&mut Events { input: &mut *input }, Some(Duration::ZERO)) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        if !looper.is_open(self.conn) {
            self.input.open = false;
        }
        self.wheel = self.input.wheel;
    }

    fn present(&mut self, buf: &[u32], w: usize, h: usize) -> Result<(), String> {
        self.pump();
        if !self.input.open {
            return Ok(());
        }
        if !matches!(&self.frame, Some(f) if f.w == w && f.h == h) {
            self.attach(w, h)?;
        }
        let Some(frame) = self.frame.as_mut() else {
            return Ok(());
        };
        // Safety: `[u32]` → the bytes of the same memory; the frame is
        // little-endian `0x00RRGGBB`, exactly Shine's own pixel.
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), w * h * 4) };
        frame.mem.as_mut_slice()[..bytes.len()].copy_from_slice(bytes);
        let Client {
            looper,
            conn,
            input,
            id,
            ..
        } = self;
        match looper.call(&mut Events { input: &mut *input }, *conn, ROOT, "commit", vec![Wire::Int(*id)]) {
            Ok(_) => Ok(()),
            Err(CallError::Disconnected) => {
                self.input.open = false;
                Ok(())
            }
            Err(e) => Err(format!("Graphics.present: {e}")),
        }
    }

    fn keys_down(&self) -> Vec<Key> {
        self.input.held.clone()
    }

    fn is_key_down(&self, key: Key) -> bool {
        self.input.held.contains(&key)
    }

    fn mouse_pos(&self) -> Option<(f64, f64)> {
        self.input.pointer
    }

    fn mouse_pos_clamped(&self) -> (f64, f64) {
        let (w, h) = self.input.size;
        self.input.pointer.map_or((0.0, 0.0), |(x, y)| {
            (
                x.clamp(0.0, w.saturating_sub(1) as f64),
                y.clamp(0.0, h.saturating_sub(1) as f64),
            )
        })
    }

    fn mouse_buttons(&self) -> [bool; 3] {
        self.input.buttons
    }

    fn wheel(&self) -> (f64, f64) {
        self.wheel
    }

    fn is_active(&mut self) -> bool {
        self.input.focused
    }

    fn set_title(&mut self, title: &str) {
        self.tell("setTitle", vec![Wire::Str(title.to_string())]);
    }

    // Where a window sits, and whether it is on top, is the server's
    // decision — in a tiling layout there is no choosing.
    fn position(&self) -> (i64, i64) {
        (0, 0)
    }

    fn set_position(&mut self, _x: i64, _y: i64) {}

    fn set_topmost(&mut self, _topmost: bool) {}

    fn set_cursor(&mut self, cursor: CursorStyle) {
        self.tell("setCursor", vec![Wire::Str(cursor_name(cursor).into())]);
    }

    fn set_cursor_visible(&mut self, visible: bool) {
        let name = if visible { "arrow" } else { "none" };
        self.tell("setCursor", vec![Wire::Str(name.into())]);
    }
}

/// The name `Mouse.setCursor` takes for a cursor style.
fn cursor_name(style: CursorStyle) -> &'static str {
    match style {
        CursorStyle::Arrow => "arrow",
        CursorStyle::Ibeam => "ibeam",
        CursorStyle::Crosshair => "crosshair",
        CursorStyle::ClosedHand => "grab",
        CursorStyle::OpenHand => "hand",
        CursorStyle::ResizeLeftRight => "resizeleftright",
        CursorStyle::ResizeUpDown => "resizeupdown",
        CursorStyle::ResizeAll => "resizeall",
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // `Window.close`, or a new `Window.create`: the server removes the
        // window now rather than when the connection drops.
        let id = self.id;
        let _ = self.looper.send(self.conn, ROOT, "close", vec![Wire::Int(id)]);
        let _ = self
            .looper
            .dispatch(&mut Events { input: &mut self.input }, Some(Duration::ZERO));
    }
}
