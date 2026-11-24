//! Keyboard and mouse on the bare screen, read from the kernel's evdev
//! devices (`/dev/input/event*`).
//!
//! Every keyboard, mouse, touchpad and tablet is opened, and their events are
//! merged: there is one pointer and one keyboard state, whichever device moved
//! them. Devices plugged in later are picked up within a couple of seconds;
//! unplugged ones are dropped.
//!
//! Pointers come in three kinds:
//! - **relative** (mice, and PS/2 touchpads in their default mode — the
//!   netbook's): movement is added to the pointer position.
//! - **absolute** (tablets, touchscreens, QEMU's `usb-tablet`): a position
//!   in the device's own range, scaled to the screen.
//! - **touchpads reporting absolute finger positions**: the finger's
//!   movement while it touches is turned into relative movement.
//!
//! Keystrokes and typed text go straight into
//! [`crate::keyboard::TextCollector`], as minifb's own callback does, so the
//! keyboard module sees the same stream of edges whichever backend runs.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::raw::c_ulong;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use minifb::{InputCallback, Key};

use super::keymap;
use crate::keyboard::TextCollector;

const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_TOOL_FINGER: u16 = 0x145;
const BTN_TOUCH: u16 = 0x14a;

const KEY_LEFTCTRL: u16 = 29;
const KEY_LEFTSHIFT: u16 = 42;
const KEY_RIGHTSHIFT: u16 = 54;
const KEY_LEFTALT: u16 = 56;
const KEY_CAPSLOCK: u16 = 58;
const KEY_RIGHTCTRL: u16 = 97;
const KEY_RIGHTALT: u16 = 100;
const KEY_DELETE: u16 = 111;

/// How often `/dev/input` is looked at again for new devices.
const RESCAN_EVERY: Duration = Duration::from_secs(2);

/// One `struct input_event` on 64-bit Linux: a `timeval`, then type, code
/// and value.
const EVENT_SIZE: usize = 24;

/// `EVIOCGBIT(ev, len)`: which codes of event type `ev` a device has.
fn eviocgbit(ev: u16, len: usize) -> c_ulong {
    (2 << 30) | ((len as c_ulong) << 16) | (0x45 << 8) | (0x20 + ev as c_ulong)
}

/// `EVIOCGABS(axis)`: an absolute axis's range.
fn eviocgabs(axis: u16) -> c_ulong {
    (2 << 30) | (24 << 16) | (0x45 << 8) | (0x40 + axis as c_ulong)
}

#[repr(C)]
#[derive(Default)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[derive(Clone, Copy, PartialEq)]
enum Pointer {
    /// No absolute axes: a keyboard, or a mouse moving relatively.
    Relative,
    /// Absolute X and Y mapped onto the screen.
    Absolute { x: (i32, i32), y: (i32, i32) },
    /// Absolute X and Y of a finger on a touchpad, turned into movement.
    Touchpad { x: (i32, i32), y: (i32, i32) },
}

struct Device {
    path: PathBuf,
    file: File,
    pointer: Pointer,
    /// Touchpads: where the finger was at the last event while touching.
    finger: Option<(i32, i32)>,
    /// The absolute position this frame's events are building up.
    abs: (Option<i32>, Option<i32>),
    touching: bool,
}

impl Device {
    fn open(path: PathBuf) -> Option<Device> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
            .ok()?;
        let fd = file.as_raw_fd();

        let mut abs_bits = [0u8; 8];
        let mut key_bits = [0u8; 96];
        // Safety: each request writes at most the length encoded in it into
        // the array passed, which is that long.
        unsafe {
            libc::ioctl(fd, eviocgbit(EV_ABS, abs_bits.len()), abs_bits.as_mut_ptr());
            libc::ioctl(fd, eviocgbit(EV_KEY, key_bits.len()), key_bits.as_mut_ptr());
        }
        let has = |bits: &[u8], code: u16| bits[code as usize / 8] & (1 << (code % 8)) != 0;

        let pointer = if has(&abs_bits, ABS_X) && has(&abs_bits, ABS_Y) {
            let range = |axis| {
                let mut info = AbsInfo::default();
                // Safety: writes one `input_absinfo`, which `AbsInfo` mirrors.
                unsafe { libc::ioctl(fd, eviocgabs(axis), &mut info) };
                (info.minimum, info.maximum.max(info.minimum + 1))
            };
            let (x, y) = (range(ABS_X), range(ABS_Y));
            if has(&key_bits, BTN_TOOL_FINGER) {
                Pointer::Touchpad { x, y }
            } else {
                Pointer::Absolute { x, y }
            }
        } else {
            Pointer::Relative
        };

        Some(Device {
            path,
            file,
            pointer,
            finger: None,
            abs: (None, None),
            touching: false,
        })
    }
}

pub(super) struct Input {
    devices: Vec<Device>,
    last_scan: Instant,
    /// The screen size: the pointer stays inside it.
    bounds: (f64, f64),
    pub(super) pos: (f64, f64),
    pub(super) buttons: [bool; 3],
    /// Wheel movement in notches since the last pump started.
    pub(super) wheel: (f64, f64),
    /// Taken in during the current pump, becoming [`Input::wheel`] at its end.
    wheel_acc: (f64, f64),
    held: Vec<Key>,
    /// Linux codes of the modifiers held, for text and Ctrl-Alt-Del.
    shift: [bool; 2],
    ctrl: [bool; 2],
    alt: [bool; 2],
    caps: bool,
}

impl Input {
    pub(super) fn open(bounds: (usize, usize)) -> Input {
        let bounds = (bounds.0 as f64, bounds.1 as f64);
        let mut input = Input {
            devices: Vec::new(),
            last_scan: Instant::now(),
            bounds,
            // The pointer starts in the middle of the screen.
            pos: (bounds.0 / 2.0, bounds.1 / 2.0),
            buttons: [false; 3],
            wheel: (0.0, 0.0),
            wheel_acc: (0.0, 0.0),
            held: Vec::new(),
            shift: [false; 2],
            ctrl: [false; 2],
            alt: [false; 2],
            caps: false,
        };
        input.scan();
        input
    }

    /// Open every input device not open yet.
    fn scan(&mut self) {
        self.last_scan = Instant::now();
        let Ok(entries) = std::fs::read_dir("/dev/input") else {
            return;
        };
        let open: HashSet<PathBuf> = self.devices.iter().map(|d| d.path.clone()).collect();
        for entry in entries.flatten() {
            let path = entry.path();
            let is_event = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"));
            if is_event
                && !open.contains(&path)
                && let Some(device) = Device::open(path)
            {
                self.devices.push(device);
            }
        }
    }

    pub(super) fn held(&self) -> &[Key] {
        &self.held
    }

    /// Read everything the devices have for us, without waiting.
    pub(super) fn pump(&mut self) {
        if self.last_scan.elapsed() >= RESCAN_EVERY {
            self.scan();
        }

        let mut events = Vec::new();
        let mut gone = Vec::new();
        for (i, device) in self.devices.iter_mut().enumerate() {
            let mut buf = [0u8; EVENT_SIZE * 64];
            loop {
                match device.file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        for raw in buf[..n].chunks_exact(EVENT_SIZE) {
                            let kind = u16::from_ne_bytes([raw[16], raw[17]]);
                            let code = u16::from_ne_bytes([raw[18], raw[19]]);
                            let value = i32::from_ne_bytes([raw[20], raw[21], raw[22], raw[23]]);
                            events.push((i, kind, code, value));
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    // ENODEV: unplugged.
                    Err(_) => {
                        gone.push(i);
                        break;
                    }
                }
            }
        }
        for (device, kind, code, value) in events {
            self.event(device, kind, code, value);
        }
        // Absolute positions are applied once per device per pump, with
        // whichever of X and Y moved.
        for i in 0..self.devices.len() {
            self.apply_absolute(i);
        }
        for i in gone.into_iter().rev() {
            self.devices.remove(i);
        }

        self.wheel = self.wheel_acc;
        self.wheel_acc = (0.0, 0.0);
    }

    fn event(&mut self, device: usize, kind: u16, code: u16, value: i32) {
        match kind {
            EV_KEY => self.key(device, code, value),
            EV_REL => match code {
                REL_X => self.move_by(value as f64 * crate::pointer::speed(), 0.0),
                REL_Y => self.move_by(0.0, value as f64 * crate::pointer::speed()),
                // Positive is away from the user, as Shine reports it.
                REL_WHEEL => self.wheel_acc.1 += value as f64,
                REL_HWHEEL => self.wheel_acc.0 += value as f64,
                _ => {}
            },
            EV_ABS => {
                let d = &mut self.devices[device];
                match code {
                    ABS_X => d.abs.0 = Some(value),
                    ABS_Y => d.abs.1 = Some(value),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn apply_absolute(&mut self, i: usize) {
        let bounds = self.bounds;
        let d = &mut self.devices[i];
        let (ax, ay) = std::mem::take(&mut d.abs);
        if ax.is_none() && ay.is_none() {
            return;
        }
        match d.pointer {
            Pointer::Relative => {}
            Pointer::Absolute { x, y } => {
                let scale = |v: i32, (lo, hi): (i32, i32), size: f64| {
                    (v - lo) as f64 / (hi - lo) as f64 * (size - 1.0)
                };
                if let Some(v) = ax {
                    self.pos.0 = scale(v, x, bounds.0);
                }
                if let Some(v) = ay {
                    self.pos.1 = scale(v, y, bounds.1);
                }
                self.clamp();
            }
            Pointer::Touchpad { x, y } => {
                if !d.touching {
                    d.finger = None;
                    return;
                }
                let (last_x, last_y) = d.finger.unwrap_or((ax.unwrap_or(0), ay.unwrap_or(0)));
                let (nx, ny) = (ax.unwrap_or(last_x), ay.unwrap_or(last_y));
                let moved = d.finger.map(|_| (nx - last_x, ny - last_y));
                d.finger = Some((nx, ny));
                if let Some((dx, dy)) = moved {
                    // The pad's width covers about the screen's width.
                    let sx = bounds.0 / (x.1 - x.0) as f64;
                    let sy = bounds.1 / (y.1 - y.0) as f64;
                    let s = sx.min(sy) * crate::pointer::speed();
                    self.move_by(dx as f64 * s, dy as f64 * s);
                }
            }
        }
    }

    fn move_by(&mut self, dx: f64, dy: f64) {
        self.pos.0 += dx;
        self.pos.1 += dy;
        self.clamp();
    }

    fn clamp(&mut self) {
        self.pos.0 = self.pos.0.clamp(0.0, (self.bounds.0 - 1.0).max(0.0));
        self.pos.1 = self.pos.1.clamp(0.0, (self.bounds.1 - 1.0).max(0.0));
    }

    fn key(&mut self, device: usize, code: u16, value: i32) {
        let down = value != 0;
        match code {
            BTN_LEFT => self.buttons[0] = down,
            BTN_RIGHT => self.buttons[1] = down,
            BTN_MIDDLE => self.buttons[2] = down,
            BTN_TOUCH => {
                let d = &mut self.devices[device];
                d.touching = down;
                if !down {
                    d.finger = None;
                }
                // A touchscreen's touch is its click; a touchpad's is not
                // (its buttons, or its own tap-to-click, send BTN_LEFT).
                if matches!(d.pointer, Pointer::Absolute { .. }) {
                    self.buttons[0] = down;
                }
            }
            _ => self.keyboard(code, value),
        }
    }

    fn keyboard(&mut self, code: u16, value: i32) {
        match code {
            KEY_LEFTSHIFT => self.shift[0] = value != 0,
            KEY_RIGHTSHIFT => self.shift[1] = value != 0,
            KEY_LEFTCTRL => self.ctrl[0] = value != 0,
            KEY_RIGHTCTRL => self.ctrl[1] = value != 0,
            KEY_LEFTALT => self.alt[0] = value != 0,
            KEY_RIGHTALT => self.alt[1] = value != 0,
            KEY_CAPSLOCK if value == 1 => self.caps = !self.caps,
            _ => {}
        }
        let ctrl = self.ctrl.iter().any(|&d| d);
        let alt = self.alt.iter().any(|&d| d);

        // The keyboard is ours, so the kernel no longer sees Ctrl-Alt-Del.
        // Pass it on the way it would have: SIGINT to init, which reboots.
        if code == KEY_DELETE && value == 1 && ctrl && alt {
            // Safety: sending a signal has no memory effects.
            unsafe { libc::kill(1, libc::SIGINT) };
        }

        let Some(key) = keymap::key(code) else {
            return;
        };
        let mut collector = TextCollector;

        // 1 = press, 0 = release, 2 = the kernel's auto-repeat. Repeats are
        // not new presses — Shine times key repeat itself — but they do type
        // the character again, as holding a key does everywhere.
        match value {
            1 => {
                if !self.held.contains(&key) {
                    self.held.push(key);
                }
                collector.set_key_state(key, true);
            }
            0 => {
                self.held.retain(|&k| k != key);
                collector.set_key_state(key, false);
                return;
            }
            _ => {}
        }

        // Ctrl and Alt combinations are shortcuts, not typing.
        if !ctrl && !alt {
            let shift = self.shift.iter().any(|&d| d);
            if let Some(ch) = keymap::text(code, shift, self.caps) {
                collector.add_char(ch as u32);
            }
        }
    }
}
