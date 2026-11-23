//! Taking the Linux virtual terminal out of the way while Shine owns the
//! screen, and putting it back afterwards.
//!
//! Two things would otherwise fight Shine for the machine:
//!
//! - the kernel's text console (fbcon) keeps drawing — boot messages, the
//!   blinking cursor — straight over the frames Shine shows. Graphics mode
//!   (`KD_GRAPHICS`) stops it.
//! - every keystroke Shine reads from evdev also reaches the terminal, and so
//!   whatever shell is reading it. Keyboard mode `K_OFF` stops that.
//!
//! Both are restored when the [`Vt`] is dropped. A process that dies without
//! dropping it leaves the terminal blank and deaf; Saule Linux's init puts it
//! back when the session ends, for exactly that case.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::raw::{c_int, c_ulong};

const KDSETMODE: c_ulong = 0x4B3A;
const KDGETMODE: c_ulong = 0x4B3B;
const KDGKBMODE: c_ulong = 0x4B44;
const KDSKBMODE: c_ulong = 0x4B45;
const KD_GRAPHICS: c_int = 1;
const K_OFF: c_int = 4;

pub(super) struct Vt {
    tty: File,
    mode: c_int,
    keyboard: c_int,
}

impl Vt {
    /// Switch the current virtual terminal to graphics mode with its keyboard
    /// off. `None` when there is no virtual terminal to switch (no `CONFIG_VT`,
    /// or not allowed to) — Shine still works then, only with the text console
    /// possibly showing through.
    pub(super) fn take() -> Option<Vt> {
        // tty0 is whichever virtual terminal is showing.
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty0")
            .ok()?;
        let fd = tty.as_raw_fd();

        let mut mode: c_int = 0;
        let mut keyboard: c_int = 0;
        // Safety: both requests write one int through the pointer given.
        unsafe {
            if libc::ioctl(fd, KDGETMODE, &mut mode) != 0
                || libc::ioctl(fd, KDGKBMODE, &mut keyboard) != 0
            {
                return None;
            }
            if libc::ioctl(fd, KDSETMODE, KD_GRAPHICS as c_ulong) != 0 {
                return None;
            }
            libc::ioctl(fd, KDSKBMODE, K_OFF as c_ulong);
        }

        Some(Vt {
            tty,
            mode,
            keyboard,
        })
    }
}

impl Drop for Vt {
    fn drop(&mut self) {
        let fd = self.tty.as_raw_fd();
        // Safety: both requests take the mode by value.
        unsafe {
            libc::ioctl(fd, KDSKBMODE, self.keyboard as c_ulong);
            libc::ioctl(fd, KDSETMODE, self.mode as c_ulong);
        }
    }
}
