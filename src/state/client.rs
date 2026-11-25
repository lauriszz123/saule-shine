//! The client backend: a window inside a Saule display server.
//!
//! Chosen by `Window.create` when `SAULE_DISPLAY` names a display service
//! (see [`crate::protocol`]). To the program it is a window like any other:
//! frames go out through shared memory, input comes in as calls from the
//! server, and the size is whatever the server configures — so a program
//! written for a desktop window runs here unchanged.
//!
//! It draws on the GPU too, where there is one: with no window of its own it
//! has a context on no surface (`gpu::Device::headless`), the renderer draws
//! into the screen's texture, and each frame is read straight from that
//! texture into the shared memory the server reads. The server puts it
//! straight into its own texture (`Renderer::upload_frame`), so the pixels
//! cross from one GPU to the other through one buffer, untouched by either
//! side's CPU drawing. (Handing the GPU buffer itself over, as a dma-buf,
//! would save that last read; see `docs/gpu.md`.)

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
    /// The scale the server draws at, which this window is drawn at too:
    /// what the server last said, or what the environment said at the
    /// start (`SHINE_SCALE`, which a server sets for what it starts).
    scale: f64,
    /// Asked to close while the program holds closing: for the next poll.
    close_asked: bool,
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
            ("scale", [scale]) => {
                if let Some(scale) = float(scale).filter(|s| s.is_finite() && *s > 0.0) {
                    input.scale = scale.clamp(0.5, 4.0);
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
            ("focus", [focused]) => {
                input.focused = boolean(focused);
                // The keyboard has gone elsewhere, and so will the news of
                // every key let go of from now: all of them are let go of
                // here. Without this a shortcut that takes the keyboard
                // away (the window manager's own, held with Ctrl) leaves
                // Ctrl held for good as far as this program can tell, and
                // the next letters typed are shortcuts.
                if !input.focused {
                    for key in std::mem::take(&mut input.held) {
                        TextCollector.set_key_state(key, false);
                    }
                }
            }
            // Asked to close: closed at once, unless the program has said
            // it decides that itself (`Window.holdClose`), which is then
            // told and left open.
            ("close", []) => {
                if crate::window::close_held() {
                    input.close_asked = true;
                } else {
                    input.open = false;
                }
            }
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
    /// The GPU the renderer draws with, when there is one.
    gpu: Option<std::rc::Rc<crate::gpu::Device>>,
    /// Frames handed over as GPU buffers the server shares (see
    /// `present_shared`): the two buffers, which one is next, and whether
    /// sharing is still worth trying (the server or the driver may refuse,
    /// once, and pixels go through shared memory from then on).
    #[cfg(target_os = "linux")]
    shared: Option<([crate::gpu::Texture; 2], usize)>,
    /// The last frame named to the server, until it has answered: see
    /// `present_shared`.
    #[cfg(target_os = "linux")]
    committing: Option<saule_ipc::CallId>,
    #[cfg(target_os = "linux")]
    share: bool,
}

impl Client {
    pub(crate) fn open(
        service: &str,
        title: &str,
        width: usize,
        height: usize,
        resizable: bool,
    ) -> Result<Client, String> {
        let fail = |e: &dyn std::fmt::Display| format!("Window.create: display `{service}`: {e}");
        let mut looper = Looper::new();
        let conn = looper.connect(service).map_err(|e| fail(&e))?;
        let mut input = Input {
            size: (width, height),
            open: true,
            scale: {
                let chosen = crate::window::chosen_scale();
                if chosen > 0.0 {
                    chosen
                } else {
                    1.0
                }
            },
            close_asked: false,
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
                    Wire::Bool(resizable),
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
            gpu: headless_gpu(),
            #[cfg(target_os = "linux")]
            shared: None,
            #[cfg(target_os = "linux")]
            committing: None,
            #[cfg(target_os = "linux")]
            share: std::env::var_os("SHINE_SHARE_FRAMES").is_none_or(|v| v != "0"),
        })
    }

    /// Hand `texture` over without a copy through the CPU: into the next of
    /// two GPU buffers shared with the server (shared first, and again when
    /// the size changes), copied there on the GPU, then named to the server
    /// as the frame to show. Two, so the app draws its next frame into one
    /// while the server may still be showing the other.
    ///
    /// The server's answer to that naming is not waited for here but at the
    /// next frame, just before the other buffer is written: that buffer is
    /// the server's to show until it has taken this one. So the app works
    /// out its next frame while the server gets round to this one — it
    /// looks once a frame of its own — instead of standing still for it,
    /// which on a slow machine was a third of every frame.
    ///
    /// The buffers are a little larger than the window (`shared_size`), and
    /// each frame is named with its own size: the corner of the buffer it
    /// fills. A window being resized is a new size every frame, and buffers
    /// made, exported and imported again for each — four of them, on both
    /// sides — were a quarter of a second a step on a netbook; now that
    /// happens once in a hundred pixels.
    #[cfg(target_os = "linux")]
    fn present_shared(&mut self, texture: &crate::gpu::Texture) -> Result<(), String> {
        use crate::gpu::{dmabuf, Texture};
        self.pump();
        if !self.input.open {
            return Ok(());
        }
        let dev = self.gpu.clone().ok_or("no GPU")?;
        if dev.egl.as_ref().is_none_or(|egl| !egl.can_export()) {
            return Err("the driver can't share buffers".into());
        }
        self.committed()?;
        if !self.input.open {
            return Ok(());
        }
        let (w, h) = (texture.w, texture.h);
        let fits = self
            .shared
            .as_ref()
            .is_some_and(|(buffers, _)| shared_fits((buffers[0].w, buffers[0].h), (w, h)));
        if !fits {
            let (bw, bh) = shared_size(w, h);
            let buffers = [Texture::new(&dev, bw, bh)?, Texture::new(&dev, bw, bh)?];
            let mut args = vec![Wire::Int(self.id), Wire::Int(bw as i64), Wire::Int(bh as i64)];
            for buffer in &buffers {
                let shared = dmabuf::export(&dev, buffer).ok_or("the driver won't share a buffer")?;
                args.extend([
                    Wire::Fd(shared.fd),
                    Wire::Int(i64::from(shared.fourcc)),
                    Wire::Int(i64::from(shared.stride)),
                    Wire::Int(i64::from(shared.offset)),
                    Wire::Int(shared.modifier as i64),
                ]);
            }
            self.call("attachShared", args)?;
            if crate::gpu::tracing() && self.shared.is_none() {
                eprintln!("shine: frames go to the display server as GPU buffers, uncopied");
            }
            self.shared = Some((buffers, 0));
        }
        let (buffers, next) = self.shared.as_mut().expect("made above");
        let index = *next;
        *next = 1 - index;
        buffers[index].copy_from(texture);
        // Finished before the server is told: its GPU reads what this one
        // wrote, and nothing else orders the two.
        // SAFETY: a GL call on this app's current context.
        unsafe { glow::HasContext::finish(&dev.gl) };
        let args = vec![
            Wire::Int(self.id),
            Wire::Int(index as i64),
            Wire::Int(w as i64),
            Wire::Int(h as i64),
        ];
        match self.looper.call_async(self.conn, ROOT, "commitShared", args) {
            Ok(call) => self.committing = Some(call),
            Err(CallError::Disconnected) => self.input.open = false,
            Err(e) => return Err(format!("commitShared: {e}")),
        }
        Ok(())
    }

    /// Wait for the server's answer to the last frame named to it, if it
    /// hasn't come yet; a server that has gone is the window closing.
    #[cfg(target_os = "linux")]
    fn committed(&mut self) -> Result<(), String> {
        let Some(call) = self.committing.take() else {
            return Ok(());
        };
        let Client { looper, input, .. } = self;
        match looper.wait(&mut Events { input: &mut *input }, call) {
            Ok(_) => Ok(()),
            Err(CallError::Disconnected) => {
                self.input.open = false;
                Ok(())
            }
            Err(e) => Err(format!("commitShared: {e}")),
        }
    }

    /// A call to the server, waiting for its answer; a server that has gone
    /// is the window closing.
    fn call(&mut self, method: &str, args: Vec<Wire>) -> Result<(), String> {
        let Client {
            looper,
            conn,
            input,
            ..
        } = self;
        match looper.call(&mut Events { input: &mut *input }, *conn, ROOT, method, args) {
            Ok(_) => Ok(()),
            Err(CallError::Disconnected) => {
                self.input.open = false;
                Ok(())
            }
            Err(e) => Err(format!("{method}: {e}")),
        }
    }

    /// Fill the shared frame (`w` × `h`, attached anew when the size
    /// changes) with `fill`, then tell the server it's ready.
    fn send(&mut self, w: usize, h: usize, fill: impl FnOnce(&mut [u8])) -> Result<(), String> {
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
        fill(&mut frame.mem.as_mut_slice()[..w * h * 4]);
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

    fn take_close_asked(&mut self) -> bool {
        std::mem::take(&mut self.input.close_asked)
    }

    fn size(&self) -> (usize, usize) {
        self.input.size
    }

    fn scale(&self) -> f64 {
        self.input.scale
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
        // Safety: `[u32]` → the bytes of the same memory; the frame is
        // little-endian `0x00RRGGBB`, exactly Shine's own pixel.
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), w * h * 4) };
        self.send(w, h, |frame| frame.copy_from_slice(bytes))
    }

    fn gpu_device(&self) -> Option<std::rc::Rc<crate::gpu::Device>> {
        self.gpu.clone()
    }

    /// A frame the GPU drew: handed over as a GPU buffer where the server
    /// and the driver can share one, else read from its texture straight
    /// into the shared memory, with no stop in the CPU's copy of the screen.
    fn present_texture(&mut self, texture: &crate::gpu::Texture) -> Result<bool, String> {
        #[cfg(target_os = "linux")]
        if self.share {
            match self.present_shared(texture) {
                Ok(()) => return Ok(true),
                Err(e) => {
                    if crate::gpu::tracing() {
                        eprintln!("shine: frames can't be shared ({e}); sending pixels");
                    }
                    self.share = false;
                    self.shared = None;
                }
            }
        }
        self.send(texture.w, texture.h, |frame| texture.read_bytes(frame))?;
        Ok(true)
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

/// A GPU to draw on, if drawing on the GPU is wanted and one starts.
#[cfg(target_os = "linux")]
fn headless_gpu() -> Option<std::rc::Rc<crate::gpu::Device>> {
    if !crate::gpu::drawing_wanted() {
        return None;
    }
    match crate::gpu::Device::headless() {
        Ok(dev) => {
            if crate::gpu::tracing() {
                eprintln!("shine: drawing on the GPU: {}", dev.description);
            }
            Some(std::rc::Rc::new(dev))
        }
        Err(e) => {
            if crate::gpu::tracing() {
                eprintln!("shine: no GPU to draw on ({e}); drawing on the CPU");
            }
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn headless_gpu() -> Option<std::rc::Rc<crate::gpu::Device>> {
    None
}

/// How much larger than the window its shared buffers are made, in pixels
/// a side: a window can grow this far before new ones are needed.
#[cfg(target_os = "linux")]
const SHARED_STEP: usize = 128;

/// The size of the buffers shared for a `w` × `h` window: the next whole
/// step up each way.
#[cfg(target_os = "linux")]
fn shared_size(w: usize, h: usize) -> (usize, usize) {
    let up = |n: usize| n.max(1).div_ceil(SHARED_STEP) * SHARED_STEP;
    (up(w), up(h))
}

/// Whether buffers of `have` still serve a `w` × `h` window: large enough,
/// and not so much larger (a window shrunk a long way) that new, smaller
/// ones are worth making for the memory.
#[cfg(target_os = "linux")]
fn shared_fits(have: (usize, usize), (w, h): (usize, usize)) -> bool {
    let (need_w, need_h) = shared_size(w, h);
    let roomy = |have: usize, need: usize| have >= need && have <= need + 2 * SHARED_STEP;
    roomy(have.0, need_w) && roomy(have.1, need_h)
}

#[cfg(all(test, target_os = "linux"))]
mod shared_tests {
    use super::*;

    #[test]
    fn buffers_are_a_step_larger_than_the_window() {
        assert_eq!(shared_size(1016, 564), (1024, 640));
        assert_eq!(shared_size(1024, 640), (1024, 640));
        assert_eq!(shared_size(1, 1), (128, 128));
    }

    #[test]
    fn a_window_being_resized_keeps_its_buffers_for_a_while() {
        let have = shared_size(700, 500);
        // Growing within the step, and shrinking a fair way: the same.
        assert!(shared_fits(have, (768, 512)));
        assert!(shared_fits(have, (700, 500)));
        assert!(shared_fits(have, (500, 300)));
        // Grown past them, or shrunk to a fraction: new ones.
        assert!(!shared_fits(have, (769, 500)));
        assert!(!shared_fits(have, (700, 513)));
        assert!(!shared_fits(have, (300, 500)));
    }
}
