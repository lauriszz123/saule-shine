//! `Compositor` — Shine for a display server: the windows of other Shine
//! programs, as images to draw and targets to send input to.
//!
//! A window manager written in Saule opens its own window (the whole screen
//! on Saule Linux, an ordinary window on a desktop), calls
//! `Compositor.start()`, and from then on every Shine program it starts opens
//! its window *here* instead (see [`crate::protocol`]). The window manager
//! decides everything a window manager decides — where each window goes, how
//! big it is, which one has focus, what the title bars look like — and uses
//! this class only for the plumbing:
//!
//! ```saule
//! Window.create(title: "wm")
//! local width, height = Window.getSize()     -- the real screen size
//! Compositor.start()
//! Os.spawn("/bin/apps/gamemaker")
//!
//! while Window.isOpen() do
//!   for ev in Compositor.poll() do           -- ev: WindowEvent
//!     match ev.kind
//!       case WindowEventKind.Opened then     -- ev.id, ev.title, ev.width, ev.height
//!         -- The app only asked for ev.width × ev.height; the WM decides.
//!         Compositor.configure(ev.id, width, height)
//!       case WindowEventKind.Closed then …   -- ev.id
//!       case _ then nil
//!     end
//!   end
//!   for ev: table in Window.pollEvents() do
//!     -- route input: Compositor.sendKey(focused, key, true, shift, ctrl, alt) …
//!   end
//!   Graphics.clear()
//!   for id: integer in Compositor.windows() do
//!     local image = Compositor.image(id)
//!     if image != nil then Graphics.draw(image!, x, y) end
//!   end
//!   Graphics.present()
//! end
//! ```

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Duration;

use saule_ipc::{ConnId, Handler, Looper, ObjRef, ObjectId, SharedMemory, Wire};
use saule_sdk::prelude::*;
use saule_sdk::saule_export;

use crate::objects::Image;
use crate::protocol::{BYTES_PER_PIXEL, DEFAULT_SERVICE, DISPLAY_ENV};
use crate::state;

/// Something that happened to a window, for `Compositor.poll`.
enum Happened {
    Opened(i64, String, i64, i64, bool),
    Frame(i64),
    Title(i64, String),
    Cursor(i64, String),
    Closed(i64),
}

/// What happened to an app's window, for `Compositor.poll`.
#[saule_enum]
#[derive(Clone, Copy, PartialEq)]
pub enum WindowEventKind {
    /// An app opened a window: `id`, `title`, the `width` and `height` it
    /// asked for, and whether it is `resizable`.
    Opened,
    /// The window has new pixels in `Compositor.image(id)`.
    Frame,
    /// The app changed the window's `title`.
    Title,
    /// The app wants the mouse `cursor` shown over its window to be another
    /// one: `"arrow"`, `"ibeam"`, `"hand"`, …
    Cursor,
    /// The window is gone: its app closed it or ended.
    Closed,
}

/// One thing that happened to an app's window since the last
/// `Compositor.poll`. `id` is always set; fields that do not apply to its
/// `kind` are `""` or `0`.
#[saule_class]
pub struct WindowEvent {
    kind: WindowEventKind,
    id: i64,
    title: String,
    width: i64,
    height: i64,
    resizable: bool,
    cursor: String,
}

impl WindowEvent {
    fn blank(kind: WindowEventKind, id: i64) -> Self {
        WindowEvent {
            kind,
            id,
            title: String::new(),
            width: 0,
            height: 0,
            resizable: true,
            cursor: String::new(),
        }
    }

    fn from_happened(h: Happened) -> Self {
        match h {
            Happened::Opened(id, title, width, height, resizable) => WindowEvent {
                title,
                width,
                height,
                resizable,
                ..WindowEvent::blank(WindowEventKind::Opened, id)
            },
            Happened::Frame(id) => WindowEvent::blank(WindowEventKind::Frame, id),
            Happened::Title(id, title) => WindowEvent {
                title,
                ..WindowEvent::blank(WindowEventKind::Title, id)
            },
            Happened::Cursor(id, cursor) => WindowEvent {
                cursor,
                ..WindowEvent::blank(WindowEventKind::Cursor, id)
            },
            Happened::Closed(id) => WindowEvent::blank(WindowEventKind::Closed, id),
        }
    }
}

#[saule_methods]
impl WindowEvent {
    /// What happened.
    #[saule(getter)]
    pub fn kind(&self) -> WindowEventKind {
        self.kind
    }

    /// The window it happened to: the id the other `Compositor` functions
    /// take.
    #[saule(getter)]
    pub fn id(&self) -> i64 {
        self.id
    }

    /// The window's title, for `Opened` and `Title`.
    #[saule(getter)]
    pub fn title(&self) -> String {
        self.title.clone()
    }

    /// The width the app asked for, for `Opened`. Only a request: the
    /// window gets the size `Compositor.configure` gives it.
    #[saule(getter)]
    pub fn width(&self) -> i64 {
        self.width
    }

    /// The height the app asked for, for `Opened`. Only a request: the
    /// window gets the size `Compositor.configure` gives it.
    #[saule(getter)]
    pub fn height(&self) -> i64 {
        self.height
    }

    /// Whether the app lets its window be resized, for `Opened`: one that
    /// doesn't (an About box, a dialog) is drawn at the size it asked for,
    /// which a window manager might take as a sign it should float.
    #[saule(getter)]
    pub fn resizable(&self) -> bool {
        self.resizable
    }

    /// The cursor's name, for `Cursor`.
    #[saule(getter)]
    pub fn cursor(&self) -> String {
        self.cursor.clone()
    }
}

struct Frame {
    mem: SharedMemory,
    w: i64,
    h: i64,
}

struct Win {
    conn: ConnId,
    /// The app's events object, for input and `configure`.
    events: ObjectId,
    title: String,
    pid: Option<u32>,
    /// The size the app draws at, from its last `attach`.
    frame: Option<Frame>,
    /// The latest frame, as the image `Compositor.image` hands out — one
    /// object, kept, so every call returns the same image.
    image: Option<(SObject<Image>, i64, i64)>,
    /// Two GPU buffers the app shared for its frames (`attachShared`), and
    /// their size, until the next frame takes them into the renderer.
    #[cfg(target_os = "linux")]
    shared: Option<([crate::gpu::dmabuf::DmaBuf; 2], i64, i64)>,
    /// The size of the buffers last shared: what a shared frame is.
    shared_size: (i64, i64),
}

struct Server {
    name: String,
    /// Taken out while calls are being answered.
    looper: Option<Looper>,
    windows: BTreeMap<i64, Win>,
    next_id: i64,
    happened: Vec<Happened>,
}

thread_local! {
    static SERVER: RefCell<Option<Server>> = const { RefCell::new(None) };
}

fn with_server<R>(f: impl FnOnce(&mut Server) -> Result<R, String>) -> Result<R, String> {
    SERVER.with(|s| match s.borrow_mut().as_mut() {
        Some(server) => f(server),
        None => Err("the compositor is not started — call Compositor.start() first".into()),
    })
}

/// Whether this process is the display server for `service` — so its own
/// `Window.create` opens a real window rather than one inside itself.
pub(crate) fn serving(service: &str) -> bool {
    SERVER.with(|s| s.borrow().as_ref().is_some_and(|srv| srv.name == service))
}

fn int(w: &Wire) -> Result<i64, String> {
    match w {
        Wire::Int(i) => Ok(*i),
        _ => Err("expected an integer".into()),
    }
}

/// Answers apps' calls about their windows.
struct Apps;

impl Apps {
    fn owned(s: &mut Server, from: ConnId, id: i64) -> Result<&mut Win, String> {
        s.windows
            .get_mut(&id)
            .filter(|w| w.conn == from)
            .ok_or_else(|| format!("no window {id} of yours"))
    }

    /// The window's image, `w` × `h`: the one it has, or a new one when
    /// there is none or it is another size.
    fn image_for(id: i64, image: Option<(SObject<Image>, i64, i64)>, w: i64, h: i64) -> Result<i64, String> {
        match image {
            Some((img, iw, ih)) if (iw, ih) == (w, h) => Ok(img.borrow()?.handle),
            _ => {
                let handle = state::draw(|g| g.new_blank_image(w, h))??;
                let img = SObject::new(Image::from_handle(handle)?);
                with_server(|s| {
                    if let Some(win) = s.windows.get_mut(&id) {
                        win.image = Some((img, w, h));
                    }
                    Ok(())
                })?;
                Ok(handle)
            }
        }
    }

    /// Show the app's frame `index` from the buffers it shared: nothing is
    /// copied, the window's image is that buffer (see
    /// `Renderer::show_shared_frame`). An error when the GPU can't take the
    /// buffers, which tells the app to send pixels instead.
    #[cfg(target_os = "linux")]
    fn commit_shared(from: ConnId, id: i64, index: usize) -> Result<(), String> {
        let (buffers, (w, h), image) = with_server(|s| {
            let win = Apps::owned(s, from, id)?;
            Ok((win.shared.take(), win.shared_size, win.image.clone()))
        })?;
        if w <= 0 || h <= 0 {
            return Err("a shared frame before any shared buffers".into());
        }
        let handle = Apps::image_for(id, image, w, h)?;
        state::draw(|g| {
            let idx = g.canvas_index(handle, "Compositor")?;
            let buffers = buffers.as_ref().map(|(b, w, h)| (b, *w as usize, *h as usize));
            g.show_shared_frame(idx, buffers, index)
        })??;
        with_server(|s| {
            s.happened.push(Happened::Frame(id));
            Ok(())
        })
    }

    /// Make the app's latest frame the window's image.
    fn commit(from: ConnId, id: i64) -> Result<(), String> {
        // Read the frame and what image there is, then let go of the server
        // before calling into the renderer.
        let (pixels, w, h, image) = with_server(|s| {
            let win = Apps::owned(s, from, id)?;
            let frame = win.frame.as_ref().ok_or("a frame committed before any attach")?;
            Ok((
                frame.mem.as_slice().to_vec(),
                frame.w,
                frame.h,
                win.image.clone(),
            ))
        })?;
        let handle = Apps::image_for(id, image, w, h)?;
        state::draw(|g| g.upload_opaque(handle, &pixels))??;
        with_server(|s| {
            s.happened.push(Happened::Frame(id));
            Ok(())
        })
    }
}

impl Handler for Apps {
    fn call(
        &mut self,
        looper: &mut Looper,
        from: ConnId,
        _key: u64,
        method: &str,
        args: Vec<Wire>,
    ) -> Result<Vec<Wire>, String> {
        match method {
            "createWindow" => {
                // `resizable` came later, last: a client from before it
                // sends four, and its window is resizable, as they all were.
                let (title, w, h, events, resizable) = match args.as_slice() {
                    [Wire::Str(title), w, h, Wire::Object(ObjRef::Remote { id: events, .. })] => {
                        (title, w, h, events, true)
                    }
                    [
                        Wire::Str(title),
                        w,
                        h,
                        Wire::Object(ObjRef::Remote { id: events, .. }),
                        Wire::Bool(resizable),
                    ] => (title, w, h, events, *resizable),
                    _ => return Err("createWindow(title, w, h, events, resizable?)".into()),
                };
                let (w, h) = (int(w)?, int(h)?);
                let pid = looper.peer(from).and_then(|p| p.pid);
                let title = title.clone();
                let events = *events;
                with_server(|s| {
                    s.next_id += 1;
                    let id = s.next_id;
                    s.windows.insert(
                        id,
                        Win {
                            conn: from,
                            events,
                            title: title.clone(),
                            pid,
                            frame: None,
                            image: None,
                            #[cfg(target_os = "linux")]
                            shared: None,
                            shared_size: (0, 0),
                        },
                    );
                    s.happened.push(Happened::Opened(id, title, w, h, resizable));
                    Ok(vec![Wire::Int(id)])
                })
            }
            "attach" => {
                let mut args = args.into_iter();
                let (Some(id), Some(Wire::Shm(handle, len)), Some(w), Some(h)) =
                    (args.next(), args.next(), args.next(), args.next())
                else {
                    return Err("attach(id, pixels, w, h)".into());
                };
                let (id, w, h) = (int(&id)?, int(&w)?, int(&h)?);
                if w <= 0 || h <= 0 || w > 16384 || h > 16384 {
                    return Err(format!("attach: a {w}×{h} frame"));
                }
                let need = (w * h) as usize * BYTES_PER_PIXEL;
                if (len as usize) < need {
                    return Err(format!("attach: {len} bytes for a {w}×{h} frame"));
                }
                let mem = SharedMemory::from_fd(handle, need).map_err(|e| format!("attach: {e}"))?;
                with_server(|s| {
                    Apps::owned(s, from, id)?.frame = Some(Frame { mem, w, h });
                    Ok(Vec::new())
                })
            }
            "commit" => {
                let id = int(args.first().ok_or("commit(id)")?)?;
                Apps::commit(from, id)?;
                Ok(Vec::new())
            }
            // An app drawing on the GPU shares two buffers for its frames
            // instead of shared memory: what each holds is described with
            // it, as dma-buf importers need (see `gpu::dmabuf`).
            #[cfg(target_os = "linux")]
            "attachShared" => {
                let mut args = args.into_iter();
                let id = int(&args.next().ok_or("attachShared(id, …)")?)?;
                let (w, h) = (int(&args.next().ok_or("attachShared: w")?)?, int(&args.next().ok_or("attachShared: h")?)?);
                if w <= 0 || h <= 0 || w > 16384 || h > 16384 {
                    return Err(format!("attachShared: a {w}×{h} frame"));
                }
                let mut buffers = Vec::with_capacity(2);
                for _ in 0..2 {
                    let (Some(Wire::Fd(fd)), Some(fourcc), Some(stride), Some(offset), Some(modifier)) =
                        (args.next(), args.next(), args.next(), args.next(), args.next())
                    else {
                        return Err("attachShared(id, w, h, [fd, fourcc, stride, offset, modifier] × 2)".into());
                    };
                    buffers.push(crate::gpu::dmabuf::DmaBuf {
                        fd,
                        fourcc: int(&fourcc)? as u32,
                        stride: int(&stride)? as u32,
                        offset: int(&offset)? as u32,
                        modifier: int(&modifier)? as u64,
                    });
                }
                let second = buffers.pop().expect("two read");
                let first = buffers.pop().expect("two read");
                with_server(|s| {
                    let win = Apps::owned(s, from, id)?;
                    win.shared = Some(([first, second], w, h));
                    win.shared_size = (w, h);
                    Ok(Vec::new())
                })
            }
            #[cfg(target_os = "linux")]
            "commitShared" => {
                let [id, index] = args.as_slice() else {
                    return Err("commitShared(id, index)".into());
                };
                Apps::commit_shared(from, int(id)?, int(index)? as usize)?;
                Ok(Vec::new())
            }
            "setTitle" | "setCursor" => {
                let [id, Wire::Str(text)] = args.as_slice() else {
                    return Err(format!("{method}(id, text)"));
                };
                let id = int(id)?;
                let text = text.clone();
                with_server(|s| {
                    let win = Apps::owned(s, from, id)?;
                    if method == "setTitle" {
                        win.title = text.clone();
                        s.happened.push(Happened::Title(id, text));
                    } else {
                        s.happened.push(Happened::Cursor(id, text));
                    }
                    Ok(Vec::new())
                })
            }
            "close" => {
                let id = int(args.first().ok_or("close(id)")?)?;
                with_server(|s| {
                    if s.windows.get(&id).is_some_and(|w| w.conn == from) {
                        s.windows.remove(&id);
                        s.happened.push(Happened::Closed(id));
                    }
                    Ok(Vec::new())
                })
            }
            other => Err(format!("the display server has no call `{other}`")),
        }
    }

    fn disconnected(&mut self, _: &mut Looper, conn: ConnId) {
        // The app ended or crashed: its windows go with it.
        let _ = with_server(|s| {
            let gone: Vec<i64> = s
                .windows
                .iter()
                .filter(|(_, w)| w.conn == conn)
                .map(|(&id, _)| id)
                .collect();
            for id in gone {
                s.windows.remove(&id);
                s.happened.push(Happened::Closed(id));
            }
            Ok(())
        });
    }
}

/// A one-way call to window `id`'s app. A window whose app has gone is
/// reported closed by the next `poll`, so this does not fail for it.
fn tell(id: i64, method: &str, args: Vec<Wire>) -> Result<(), String> {
    with_server(|s| {
        let win = s.windows.get(&id).ok_or_else(|| format!("no window {id}"))?;
        let (conn, events) = (win.conn, win.events);
        if let Some(lp) = s.looper.as_mut() {
            let _ = lp.send(conn, events, method, args);
        }
        Ok(())
    })
}

// --- Exports -------------------------------------------------------------------------

/// `Compositor.start([service])` — become the display server: Shine
/// programs this process starts from now on open their windows here. The
/// service defaults to `"saule.display"`. Open this process's own window
/// first: the windows of others are drawn into it.
#[saule_export(class = "Compositor", name = "start")]
fn compositor_start(service: Option<String>) -> Result<(), String> {
    state::with(|_| ()).map_err(|_| "Compositor.start: open a window first (Window.create)")?;
    if SERVER.with(|s| s.borrow().is_some()) {
        return Err("Compositor.start: already started".into());
    }
    let name = service.unwrap_or_else(|| DEFAULT_SERVICE.to_string());
    let mut looper = Looper::new();
    looper
        .publish(&name, 0)
        .map_err(|e| format!("Compositor.start: {e}"))?;
    // Safety: set before the programs that read it are started, and Saule
    // runs one thread; nothing else is reading the environment meanwhile.
    unsafe { std::env::set_var(DISPLAY_ENV, &name) };
    SERVER.with(|s| {
        *s.borrow_mut() = Some(Server {
            name,
            looper: Some(looper),
            windows: BTreeMap::new(),
            next_id: 0,
            happened: Vec::new(),
        })
    });
    Ok(())
}

/// `Compositor.poll()` — answer what apps have sent, and return what
/// happened to their windows since the last poll, in order, as
/// `WindowEvent`s: `match ev.kind` on `WindowEventKind.Opened`, `Frame`
/// (new pixels in `image(id)`), `Title`, `Cursor` and `Closed`.
#[saule_export(class = "Compositor", name = "poll")]
fn compositor_poll() -> Result<Vec<WindowEvent>, String> {
    let mut looper = with_server(|s| {
        s.looper
            .take()
            .ok_or_else(|| "Compositor.poll: already polling".to_string())
    })?;
    let mut result = Ok(());
    // Everything that has arrived, without waiting.
    for _ in 0..64 {
        match looper.dispatch(&mut Apps, Some(Duration::ZERO)) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                result = Err(format!("Compositor.poll: {e}"));
                break;
            }
        }
    }
    let happened = with_server(|s| {
        s.looper = Some(looper);
        Ok(std::mem::take(&mut s.happened))
    })?;
    result?;

    Ok(happened.into_iter().map(WindowEvent::from_happened).collect())
}

/// `Compositor.windows()` — the ids of every open window, oldest first.
#[saule_export(class = "Compositor", name = "windows")]
fn compositor_windows() -> Result<Vec<i64>, String> {
    with_server(|s| Ok(s.windows.keys().copied().collect()))
}

/// `Compositor.image(id)` — the window's latest frame, to draw with
/// `Graphics.draw`; `nil` until the app has presented one. The same image
/// every call, updated in place as frames arrive — a new one only when the
/// window's size changes.
#[saule_export(class = "Compositor", name = "image")]
fn compositor_image(id: i64) -> Result<Option<SObject<Image>>, String> {
    with_server(|s| Ok(s.windows.get(&id).and_then(|w| w.image.as_ref().map(|(i, _, _)| i.clone()))))
}

/// `Compositor.title(id)` — the window's title, `nil` for no such window.
#[saule_export(class = "Compositor", name = "title")]
fn compositor_title(id: i64) -> Result<Option<String>, String> {
    with_server(|s| Ok(s.windows.get(&id).map(|w| w.title.clone())))
}

/// `Compositor.pid(id)` — the process id of the window's app, as the system
/// reports it.
#[saule_export(class = "Compositor", name = "pid")]
fn compositor_pid(id: i64) -> Result<Option<i64>, String> {
    with_server(|s| Ok(s.windows.get(&id).and_then(|w| w.pid).map(i64::from)))
}

/// `Compositor.configure(id, width, height)` — give window `id` its size.
///
/// The window manager decides how big every window is. The size an app
/// asks for in `Window.create` only arrives as a request — `ev.width` and
/// `ev.height` of its `Opened` event — and a window keeps that size until it
/// is configured. After this call the app's `Window.getSize()` returns the
/// new size, it gets a `Resized` event, and it redraws at that size; its
/// next frame arrives in `image(id)` at the new size.
///
/// Call it when a window opens, and again whenever the layout changes: a
/// window opens or closes, or the screen's own size (`Window.getSize()`)
/// changes. To fill the screen:
///
/// ```saule
/// local width, height = Window.getSize()
/// Compositor.configure(ev.id, width, height)
/// ```
#[saule_export(class = "Compositor", name = "configure")]
fn compositor_configure(id: i64, width: i64, height: i64) -> Result<(), String> {
    if width <= 0 || height <= 0 {
        return Err("Compositor.configure: width and height must be positive".into());
    }
    tell(id, "configure", vec![Wire::Int(width), Wire::Int(height)])
}

/// `Compositor.sendKey(id, key, down, shift, ctrl, alt)` — a key pressed or
/// released, by Shine's key name, to the window's app.
#[saule_export(class = "Compositor", name = "sendKey")]
fn compositor_send_key(
    id: i64,
    key: String,
    down: bool,
    shift: bool,
    ctrl: bool,
    alt: bool,
) -> Result<(), String> {
    tell(
        id,
        "key",
        vec![
            Wire::Str(key),
            Wire::Bool(down),
            Wire::Bool(shift),
            Wire::Bool(ctrl),
            Wire::Bool(alt),
        ],
    )
}

/// `Compositor.sendText(id, text)` — typed text, to the window's app.
#[saule_export(class = "Compositor", name = "sendText")]
fn compositor_send_text(id: i64, text: String) -> Result<(), String> {
    tell(id, "text", vec![Wire::Str(text)])
}

/// `Compositor.sendPointer(id, x, y)` — the pointer is over the window, at
/// `(x, y)` in the window's own coordinates.
#[saule_export(class = "Compositor", name = "sendPointer")]
fn compositor_send_pointer(id: i64, x: f64, y: f64) -> Result<(), String> {
    tell(id, "pointer", vec![Wire::Float(x), Wire::Float(y)])
}

/// `Compositor.sendLeave(id)` — the pointer has left the window.
#[saule_export(class = "Compositor", name = "sendLeave")]
fn compositor_send_leave(id: i64) -> Result<(), String> {
    tell(id, "leave", Vec::new())
}

/// `Compositor.sendButton(id, button, down)` — a mouse button (1 left,
/// 2 right, 3 middle) pressed or released over the window.
#[saule_export(class = "Compositor", name = "sendButton")]
fn compositor_send_button(id: i64, button: i64, down: bool) -> Result<(), String> {
    tell(id, "button", vec![Wire::Int(button), Wire::Bool(down)])
}

/// `Compositor.sendWheel(id, dx, dy)` — wheel movement in notches, positive
/// `dy` away from the user.
#[saule_export(class = "Compositor", name = "sendWheel")]
fn compositor_send_wheel(id: i64, dx: f64, dy: f64) -> Result<(), String> {
    tell(id, "wheel", vec![Wire::Float(dx), Wire::Float(dy)])
}

/// `Compositor.sendFocus(id, focused)` — the window gained or lost keyboard
/// focus.
#[saule_export(class = "Compositor", name = "sendFocus")]
fn compositor_send_focus(id: i64, focused: bool) -> Result<(), String> {
    tell(id, "focus", vec![Wire::Bool(focused)])
}

/// `Compositor.requestClose(id)` — ask the window's app to close it, as a
/// title bar's close button does. The app decides; `poll` reports a
/// `WindowEventKind.Closed` event when it has.
#[saule_export(class = "Compositor", name = "requestClose")]
fn compositor_request_close(id: i64) -> Result<(), String> {
    tell(id, "close", Vec::new())
}
