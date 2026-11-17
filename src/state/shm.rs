//! Presenting through X11 shared memory (MIT-SHM).
//!
//! minifb presents with a plain `XPutImage`: every frame, every pixel is
//! written down the X socket to the server. Before that it also resamples the
//! whole framebuffer into a second buffer (`ScaleMode::Stretch` runs even at
//! 1:1). At a maximised window that is two full-frame copies plus ~9 MB of
//! socket traffic per frame, and under WSLg the socket leads to XWayland in
//! another container — the measured cost of an `XPutImage` + sync of a
//! 2000×1125 frame there is ~2.9 ms of server time, against ~0.9 ms for the
//! same frame through shared memory.
//!
//! So on X11 the engine presents itself: it opens its *own* connection to the
//! same server, shares one segment of memory with it, copies the framebuffer
//! into that segment and asks the server to put it on minifb's window. Only
//! the request travels down the socket. minifb still owns the window and
//! still pumps its events; it just no longer moves the pixels.
//!
//! Everything here is best-effort. A server without MIT-SHM, a remote
//! display, a segment the server cannot attach (separate IPC namespaces) or a
//! visual that isn't 32 bits per pixel all make [`ShmPresenter::new`] or
//! [`ShmPresenter::present`] say no, and the engine falls back to minifb for
//! the rest of the window's life.

use std::os::raw::{c_char, c_int, c_ulong};
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use x11_dl::{xlib, xshm};

/// Set by [`trap_errors`] when the server reports an error on *our*
/// connection — a failed attach, a put to a window that has gone.
static FAILED: AtomicBool = AtomicBool::new(false);

/// Our connection, so the error handler can tell our errors from minifb's.
static OURS: AtomicPtr<xlib::Display> = AtomicPtr::new(ptr::null_mut());

/// The handler that was installed before ours. Errors on any other
/// connection — minifb's — are passed straight on to it, so installing this
/// changes nothing about how minifb's own errors are handled.
///
/// Set once, by the first presenter: a later one would get our own handler
/// back from `XSetErrorHandler` and must not record that as "previous".
static PREVIOUS: OnceLock<Option<Handler>> = OnceLock::new();

type Handler = unsafe extern "C" fn(*mut xlib::Display, *mut xlib::XErrorEvent) -> c_int;

/// Xlib's error handler is process-wide, so ours forwards everything that is
/// not about our connection.
unsafe extern "C" fn trap_errors(
    display: *mut xlib::Display,
    event: *mut xlib::XErrorEvent,
) -> c_int {
    if display == OURS.load(Ordering::Relaxed) {
        FAILED.store(true, Ordering::Relaxed);
        return 0;
    }

    match PREVIOUS.get().copied().flatten() {
        Some(previous) => unsafe { previous(display, event) },
        None => 0,
    }
}

/// One shared segment and the image that describes it.
struct Segment {
    image: *mut xlib::XImage,
    /// Boxed because Xlib keeps a pointer to it: `XShmCreateImage` stores
    /// the address in the image, and every `XShmPutImage` reads the segment
    /// id back through it. A plain field moved into place after creation
    /// left the image pointing at a dead stack slot, and the server
    /// rejected every put with BadShmSeg.
    info: Box<xshm::XShmSegmentInfo>,
    w: usize,
    h: usize,
}

pub(crate) struct ShmPresenter {
    xlib: xlib::Xlib,
    xext: xshm::Xext,
    display: *mut xlib::Display,
    window: xlib::Window,
    gc: xlib::GC,
    visual: *mut xlib::Visual,
    depth: c_int,
    segment: Option<Segment>,
    /// A put has been sent and the server may still be reading the segment,
    /// so it must not be written until the server has caught up.
    in_flight: bool,
}

impl ShmPresenter {
    /// Prepare to present into `window` (an X11 window id, minifb's). `None`
    /// when shared-memory presenting is not available here.
    pub(crate) fn new(window: c_ulong) -> Option<ShmPresenter> {
        if window == 0 {
            return None;
        }

        let xlib = xlib::Xlib::open().ok()?;
        let xext = xshm::Xext::open().ok()?;

        // SAFETY: plain Xlib calls on a connection this function owns; every
        // pointer comes from Xlib itself and is checked before use.
        unsafe {
            let display = (xlib.XOpenDisplay)(ptr::null());
            if display.is_null() {
                return None;
            }

            if (xext.XShmQueryExtension)(display) == 0 {
                (xlib.XCloseDisplay)(display);
                return None;
            }

            if OURS
                .compare_exchange(
                    ptr::null_mut(),
                    display,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_err()
            {
                // Another presenter is live (a second `Window.create` before
                // the first one was dropped). Keep it simple: only one at a
                // time presents through shared memory.
                (xlib.XCloseDisplay)(display);
                return None;
            }

            let previous = (xlib.XSetErrorHandler)(Some(trap_errors));
            PREVIOUS.get_or_init(|| previous);

            let screen = (xlib.XDefaultScreen)(display);
            let gc = (xlib.XCreateGC)(display, window, 0, ptr::null_mut());

            Some(ShmPresenter {
                visual: (xlib.XDefaultVisual)(display, screen),
                depth: (xlib.XDefaultDepth)(display, screen),
                xlib,
                xext,
                display,
                window,
                gc,
                segment: None,
                in_flight: false,
            })
        }
    }

    /// Put `buf` (`w` × `h` pixels, `0RGB`) on the window. `false` means
    /// shared memory turned out not to work, and the caller should present
    /// some other way from now on.
    pub(crate) fn present(&mut self, buf: &[u32], w: usize, h: usize) -> bool {
        if w == 0 || h == 0 || buf.len() < w * h {
            return false;
        }

        // SAFETY: Xlib calls on our own connection; the segment's memory is
        // ours and sized from the image the server described.
        unsafe {
            // Don't touch the segment while the server may still be reading
            // the previous frame out of it. By now — a whole frame of work
            // later — it is almost always done, so this rarely waits.
            if self.in_flight {
                (self.xlib.XSync)(self.display, 0);
                self.in_flight = false;
            }

            if FAILED.load(Ordering::Relaxed) {
                return false;
            }

            if !self.fits(w, h) && !self.allocate(w, h) {
                return false;
            }

            let segment = self.segment.as_ref().expect("allocated above");
            let image = &*segment.image;
            let stride = image.bytes_per_line as usize / 4;
            let dst = segment.info.shmaddr as *mut u32;

            if stride == w {
                ptr::copy_nonoverlapping(buf.as_ptr(), dst, w * h);
            } else {
                for row in 0..h {
                    ptr::copy_nonoverlapping(buf.as_ptr().add(row * w), dst.add(row * stride), w);
                }
            }

            (self.xext.XShmPutImage)(
                self.display,
                self.window,
                self.gc,
                segment.image,
                0,
                0,
                0,
                0,
                w as u32,
                h as u32,
                0,
            );
            (self.xlib.XFlush)(self.display);
            self.in_flight = true;
        }

        true
    }

    fn fits(&self, w: usize, h: usize) -> bool {
        matches!(&self.segment, Some(s) if s.w == w && s.h == h)
    }

    /// (Re)create the shared segment for a `w` × `h` frame, and check the
    /// server can actually attach it.
    unsafe fn allocate(&mut self, w: usize, h: usize) -> bool {
        unsafe {
            self.release();

            let mut info: Box<xshm::XShmSegmentInfo> = Box::new(std::mem::zeroed());
            let image = (self.xext.XShmCreateImage)(
                self.display,
                self.visual,
                self.depth as u32,
                xlib::ZPixmap,
                ptr::null_mut(),
                &mut *info,
                w as u32,
                h as u32,
            );
            if image.is_null() {
                return false;
            }

            // The framebuffer is 32-bit `0RGB`; any other layout would need a
            // conversion per pixel, which is minifb's job, not this one.
            if (*image).bits_per_pixel != 32 {
                (self.xlib.XDestroyImage)(image);
                return false;
            }

            let size = (*image).bytes_per_line as usize * h;
            info.shmid = libc::shmget(libc::IPC_PRIVATE, size, libc::IPC_CREAT | 0o600);
            if info.shmid < 0 {
                (self.xlib.XDestroyImage)(image);
                return false;
            }

            let addr = libc::shmat(info.shmid, ptr::null(), 0);
            if addr as isize == -1 {
                libc::shmctl(info.shmid, libc::IPC_RMID, ptr::null_mut());
                (self.xlib.XDestroyImage)(image);
                return false;
            }

            info.shmaddr = addr as *mut c_char;
            info.readOnly = 0;
            (*image).data = info.shmaddr;

            FAILED.store(false, Ordering::Relaxed);
            let attached = (self.xext.XShmAttach)(self.display, &mut *info) != 0;
            // Wait for the server's answer: an attach it cannot honour (a
            // server in another IPC namespace) only shows up as an error.
            (self.xlib.XSync)(self.display, 0);

            // Marked for removal now, so the segment cannot outlive the
            // process even if it dies; it stays usable while attached.
            libc::shmctl(info.shmid, libc::IPC_RMID, ptr::null_mut());

            self.segment = Some(Segment { image, info, w, h });

            attached && !FAILED.load(Ordering::Relaxed)
        }
    }

    /// Detach and free the current segment, if any.
    unsafe fn release(&mut self) {
        let Some(mut segment) = self.segment.take() else {
            return;
        };

        unsafe {
            (self.xlib.XSync)(self.display, 0);
            (self.xext.XShmDetach)(self.display, &mut *segment.info);
            (self.xlib.XSync)(self.display, 0);
            libc::shmdt(segment.info.shmaddr as *const libc::c_void);

            // `XDestroyImage` frees `data` too, which is the shared segment —
            // already detached above, and never Xlib's to free.
            (*segment.image).data = ptr::null_mut();
            (self.xlib.XDestroyImage)(segment.image);
        }
    }
}

impl Drop for ShmPresenter {
    fn drop(&mut self) {
        // SAFETY: tearing down what `new` and `allocate` set up, on our own
        // connection, after which nothing refers to it.
        unsafe {
            self.release();
            (self.xlib.XFreeGC)(self.display, self.gc);
            (self.xlib.XCloseDisplay)(self.display);
        }
        OURS.store(ptr::null_mut(), Ordering::Relaxed);
    }
}
