//! The screen, through DRM/KMS: the kernel's interface to the display
//! controller, which every Linux graphics driver provides (i915 on the
//! netbook, bochs in QEMU, simpledrm on whatever the firmware set up).
//!
//! Two "dumb buffers" — plain memory the controller can show — take turns:
//! a frame is copied into the one not on screen, then a page flip makes it
//! the one on screen at the next vertical blank. The flip is what keeps the
//! picture from tearing, and its completion event is what paces the next
//! frame to the display. A driver that cannot flip gets the frame copied
//! into the one buffer it shows instead.
//!
//! What the hardware can do beyond that — a cursor it moves by itself — is
//! used when the driver offers it (see [`Display::set_cursor_image`]).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::raw::c_ulong;
use std::ptr::NonNull;

use drm::buffer::{Buffer, DrmFourcc};
use drm::control::{
    connector, crtc, dumbbuffer::DumbBuffer, framebuffer, Device as ControlDevice, Mode,
    ModeTypeFlags, PageFlipFlags,
};

/// An open `/dev/dri/cardN`.
struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}
impl ControlDevice for Card {}

/// A dumb buffer, mapped into this process for as long as it lives.
struct Mapped {
    buffer: DumbBuffer,
    ptr: NonNull<u8>,
    len: usize,
}

/// `DRM_IOCTL_MODE_MAP_DUMB`: `_IOWR('d', 0xB3, struct drm_mode_map_dumb)`.
const DRM_IOCTL_MODE_MAP_DUMB: c_ulong = (3 << 30) | (16 << 16) | (0x64 << 8) | 0xB3;

#[repr(C)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

impl Mapped {
    fn new(card: &Card, size: (u32, u32), format: DrmFourcc) -> io::Result<Mapped> {
        let buffer = card.create_dumb_buffer(size, format, 32)?;
        // Mapped by hand rather than through `map_dumb_buffer`, whose mapping
        // borrows the buffer and so could not live beside it here.
        let mut req = MapDumb {
            handle: buffer.handle().into(),
            pad: 0,
            offset: 0,
        };
        let len = buffer.pitch() as usize * size.1 as usize;
        // Safety: MAP_DUMB fills in `offset`; mmap then maps exactly the
        // buffer's bytes from the card at that offset.
        let ptr = unsafe {
            if libc::ioctl(card.0.as_raw_fd(), DRM_IOCTL_MODE_MAP_DUMB, &mut req) != 0 {
                let err = io::Error::last_os_error();
                let _ = card.destroy_dumb_buffer(buffer);
                return Err(err);
            }
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                card.0.as_raw_fd(),
                req.offset as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            let err = io::Error::last_os_error();
            let _ = card.destroy_dumb_buffer(buffer);
            return Err(err);
        }
        let mut mapped = Mapped {
            buffer,
            ptr: NonNull::new(ptr.cast()).expect("mmap returned null"),
            len,
        };
        mapped.bytes().fill(0);
        Ok(mapped)
    }

    fn bytes(&mut self) -> &mut [u8] {
        // Safety: the mapping is `len` bytes, writable, and lives as long as
        // `self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    fn pitch(&self) -> usize {
        self.buffer.pitch() as usize
    }

    fn release(self, card: &Card) {
        // Safety: unmapping what `new` mapped, once.
        unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len) };
        let _ = card.destroy_dumb_buffer(self.buffer);
    }
}

struct Frame {
    mem: Mapped,
    fb: framebuffer::Handle,
}

pub(super) struct Display {
    card: Card,
    crtc: crtc::Handle,
    width: usize,
    height: usize,
    /// Two buffers when flipping, the first of them alone when not.
    frames: Vec<Frame>,
    /// Which of `frames` is on screen.
    front: usize,
    /// Page flips work on this driver. Cleared for good on the first one
    /// that fails.
    flipping: bool,
    /// A flip was asked for and its completion event not yet read.
    flip_pending: bool,
    /// The hardware cursor's image, while the driver has one.
    cursor: Option<Mapped>,
}

/// The side of a hardware cursor image. 64×64 is what every driver with a
/// cursor accepts.
pub(super) const CURSOR_SIZE: usize = 64;

impl Display {
    /// Open the first DRM device with a connected screen, and show black on
    /// it at the screen's preferred mode.
    pub(super) fn open() -> Result<Display, String> {
        let mut last_error = String::from("no /dev/dri/card* device");
        for n in 0..8 {
            let path = format!("/dev/dri/card{n}");
            let file = match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => {
                    last_error = format!("{path}: {e}");
                    continue;
                }
            };
            match Display::on(Card(file)) {
                Ok(display) => return Ok(display),
                Err(e) => last_error = format!("{path}: {e}"),
            }
        }
        Err(format!("no screen to draw on ({last_error})"))
    }

    fn on(card: Card) -> Result<Display, String> {
        let res = card
            .resource_handles()
            .map_err(|e| format!("reading its resources: {e}"))?;

        // The first connected output with a mode: on a laptop, its panel.
        let conn = res
            .connectors()
            .iter()
            .filter_map(|&c| card.get_connector(c, true).ok())
            .find(|c| c.state() == connector::State::Connected && !c.modes().is_empty())
            .ok_or("no connected screen")?;

        let mode = preferred_mode(conn.modes());

        // A CRTC that can drive it: the one already driving it (whatever the
        // firmware or fbcon set up), or else the first one its encoders allow.
        let crtc = conn
            .current_encoder()
            .and_then(|e| card.get_encoder(e).ok())
            .and_then(|e| e.crtc())
            .or_else(|| {
                conn.encoders().iter().find_map(|&e| {
                    let enc = card.get_encoder(e).ok()?;
                    res.filter_crtcs(enc.possible_crtcs()).first().copied()
                })
            })
            .ok_or("no display controller for the screen")?;

        let (w, h) = mode.size();
        let (width, height) = (w as usize, h as usize);

        let mut display = Display {
            card,
            crtc,
            width,
            height,
            frames: Vec::new(),
            front: 0,
            flipping: true,
            flip_pending: false,
            cursor: None,
        };
        for _ in 0..2 {
            let frame = display
                .new_frame()
                .map_err(|e| format!("allocating the screen: {e}"))?;
            display.frames.push(frame);
        }

        display
            .card
            .set_crtc(
                crtc,
                Some(display.frames[0].fb),
                (0, 0),
                &[conn.handle()],
                Some(mode),
            )
            .map_err(|e| format!("setting the {}×{} mode: {e}", width, height))?;

        if std::env::var_os("SHINE_TRACE_PRESENT").is_some() {
            eprintln!(
                "shine: drawing to the screen at {width}×{height} {} Hz ({:?})",
                mode.vrefresh(),
                mode.name()
            );
        }
        Ok(display)
    }

    fn new_frame(&self) -> io::Result<Frame> {
        let size = (self.width as u32, self.height as u32);
        let mem = Mapped::new(&self.card, size, DrmFourcc::Xrgb8888)?;
        let fb = match self.card.add_framebuffer(&mem.buffer, 24, 32) {
            Ok(fb) => fb,
            Err(e) => {
                mem.release(&self.card);
                return Err(e);
            }
        };
        Ok(Frame { mem, fb })
    }

    pub(super) fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Show `buf` (`w`×`h`, `0x00RRGGBB`), letting `overlay` paint over it on
    /// the way — the cursor, when there is no hardware one.
    ///
    /// Waits for the previous flip to land first, so this paces the caller to
    /// the display's refresh rate when frames come faster than that.
    pub(super) fn present(
        &mut self,
        buf: &[u32],
        w: usize,
        h: usize,
        overlay: impl FnOnce(&mut [u8], usize, usize, usize),
    ) {
        self.wait_for_flip();

        let target = if self.flipping { 1 - self.front } else { self.front };
        let (sw, sh) = (self.width, self.height);
        let frame = &mut self.frames[target];
        let pitch = frame.mem.pitch();
        let bytes = frame.mem.bytes();

        // Row by row: the buffer's pitch may be wider than the screen, and
        // the frame Shine drew may not be the screen's size (yet).
        let cw = w.min(sw);
        for y in 0..h.min(sh) {
            let src = &buf[y * w..y * w + cw];
            let dst = &mut bytes[y * pitch..y * pitch + cw * 4];
            // Safety: `[u32]` → bytes of the same memory; XRGB8888 is
            // little-endian 0x00RRGGBB, which is exactly Shine's pixel.
            let src = unsafe { std::slice::from_raw_parts(src.as_ptr().cast::<u8>(), cw * 4) };
            dst.copy_from_slice(src);
        }
        overlay(bytes, pitch, sw, sh);

        let fb = frame.fb;
        if self.flipping {
            match self
                .card
                .page_flip(self.crtc, fb, PageFlipFlags::EVENT, None)
            {
                Ok(()) => {
                    self.flip_pending = true;
                    self.front = target;
                    return;
                }
                Err(e) => {
                    if std::env::var_os("SHINE_TRACE_PRESENT").is_some() {
                        eprintln!("shine: page flips unavailable ({e}); drawing to one buffer");
                    }
                    // Draw into what is on screen from now on. This frame
                    // went into the other buffer, so show that one.
                    self.flipping = false;
                    let _ = self.card.set_crtc_fb(self.crtc, fb);
                    self.front = target;
                }
            }
        }
        // Drivers that show a copy of the buffer rather than the buffer
        // itself (simpledrm) need telling it changed.
        let _ = self.card.dirty_framebuffer(fb, &[]);
    }

    /// Block until the last page flip has happened.
    fn wait_for_flip(&mut self) {
        while self.flip_pending {
            match self.card.receive_events() {
                Ok(events) => {
                    for event in events {
                        if let drm::control::Event::PageFlip(_) = event {
                            self.flip_pending = false;
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => self.flip_pending = false,
            }
        }
    }

    /// Show `image` (`CURSOR_SIZE`² ARGB) as the hardware cursor. `false`
    /// when the driver has no hardware cursor, and the cursor must be drawn
    /// into the frames instead.
    pub(super) fn set_cursor_image(&mut self, image: &[u32]) -> bool {
        if self.cursor.is_none() {
            let size = (CURSOR_SIZE as u32, CURSOR_SIZE as u32);
            match Mapped::new(&self.card, size, DrmFourcc::Argb8888) {
                Ok(mem) => self.cursor = Some(mem),
                Err(_) => return false,
            }
        }
        let mem = self.cursor.as_mut().expect("just made");
        let pitch = mem.pitch();
        let bytes = mem.bytes();
        for (y, row) in image.chunks(CURSOR_SIZE).enumerate() {
            for (x, px) in row.iter().enumerate() {
                let at = y * pitch + x * 4;
                bytes[at..at + 4].copy_from_slice(&px.to_le_bytes());
            }
        }
        #[allow(deprecated)]
        let shown = self
            .card
            .set_cursor(self.crtc, Some(&mem.buffer))
            .is_ok();
        if !shown && let Some(mem) = self.cursor.take() {
            mem.release(&self.card);
        }
        shown
    }

    /// Hide the hardware cursor.
    pub(super) fn hide_cursor(&mut self) {
        #[allow(deprecated)]
        let _ = self.card.set_cursor::<DumbBuffer>(self.crtc, None);
    }

    /// Put the hardware cursor image's top-left corner at `(x, y)`.
    pub(super) fn move_cursor(&mut self, x: i32, y: i32) {
        #[allow(deprecated)]
        let _ = self.card.move_cursor(self.crtc, (x, y));
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        self.wait_for_flip();
        self.hide_cursor();
        if let Some(mem) = self.cursor.take() {
            mem.release(&self.card);
        }
        for frame in self.frames.drain(..) {
            let _ = self.card.destroy_framebuffer(frame.fb);
            frame.mem.release(&self.card);
        }
        // Closing the card gives the screen back: the kernel restores its
        // own console (fbdev emulation) when the last user lets go.
    }
}

trait SetCrtcFb {
    fn set_crtc_fb(&self, crtc: crtc::Handle, fb: framebuffer::Handle) -> io::Result<()>;
}

impl SetCrtcFb for Card {
    /// Point an already-set-up CRTC at another framebuffer, keeping its mode
    /// and outputs.
    fn set_crtc_fb(&self, crtc: crtc::Handle, fb: framebuffer::Handle) -> io::Result<()> {
        let info = self.get_crtc(crtc)?;
        let conns: Vec<connector::Handle> = self
            .resource_handles()?
            .connectors()
            .iter()
            .copied()
            .filter(|&c| {
                self.get_connector(c, false)
                    .ok()
                    .and_then(|c| c.current_encoder())
                    .and_then(|e| self.get_encoder(e).ok())
                    .and_then(|e| e.crtc())
                    == Some(crtc)
            })
            .collect();
        self.set_crtc(crtc, Some(fb), (0, 0), &conns, info.mode())
    }
}

/// The mode the screen says it prefers — a panel's native resolution — or
/// failing that the first (largest) one it lists.
fn preferred_mode(modes: &[Mode]) -> Mode {
    modes
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .unwrap_or(&modes[0])
        .to_owned()
}
