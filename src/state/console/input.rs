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
//! - **touchpads reporting absolute finger positions** (a PS/2 touchpad
//!   the kernel knows the maker of, and every newer laptop's): the finger's
//!   movement while it touches is turned into relative movement, two
//!   fingers scroll, and a tap clicks (see [`super::touchpad`]).
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
use super::touchpad::{self, Pad, Shape, Tap};
use crate::devices::{self, Found, Kind};
use crate::keyboard::TextCollector;
use crate::layout::{self, Typed};

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;

const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
/// A wheel in 120ths of a notch: every wheel says this as well as whole
/// notches, and one that turns smoothly says it between them.
const REL_WHEEL_HI_RES: u16 = 0x0b;
const REL_HWHEEL_HI_RES: u16 = 0x0c;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;

/// What a device says of itself (`EVIOCGPROP`): that the whole of a
/// touchpad presses down as its button, and that its slots hold a box
/// round two fingers and not the fingers.
const INPUT_PROP_BUTTONPAD: u16 = 0x02;
const INPUT_PROP_SEMI_MT: u16 = 0x03;

const KEY_ENTER: u16 = 28;
const KEY_A: u16 = 30;
const KEY_Z: u16 = 44;

const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_TOOL_FINGER: u16 = 0x145;
const BTN_TOOL_QUINTTAP: u16 = 0x148;
const BTN_TOUCH: u16 = 0x14a;
const BTN_TOOL_DOUBLETAP: u16 = 0x14d;
const BTN_TOOL_TRIPLETAP: u16 = 0x14e;
const BTN_TOOL_QUADTAP: u16 = 0x14f;

/// How long a tap on a touchpad holds its button down: long enough for a
/// program to see it pressed in one frame and let go in a later one.
const TAP_HELD: Duration = Duration::from_millis(50);

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
/// `EVIOCGNAME(len)`: the device's name for itself.
fn eviocgname(len: usize) -> c_ulong {
    (2 << 30) | ((len as c_ulong) << 16) | ((b'E' as c_ulong) << 8) | 0x06
}

/// `EVIOCGID`: a `struct input_id`, four 16-bit numbers, the first of them
/// the bus the device is on.
const EVIOCGID: c_ulong = (2 << 30) | (8 << 16) | ((b'E' as c_ulong) << 8) | 0x02;

/// The buses a device that is part of the machine is on: the keyboard
/// controller, I2C and SPI (a laptop's touchpad), and the machine itself.
/// What is plugged in comes by USB or Bluetooth.
const BUILT_IN_BUSES: [u16; 5] = [0x10, 0x11, 0x18, 0x19, 0x1c];

/// `EVIOCGPROP(len)`: the device's properties, a bit each.
fn eviocgprop(len: usize) -> c_ulong {
    (2 << 30) | ((len as c_ulong) << 16) | (0x45 << 8) | 0x09
}

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
    /// What it is, for [`crate::devices`]: `None` for the buttons on the
    /// case, the lid and the like, which nothing is set on.
    found: Option<Found>,
    /// The absolute position this frame's events are building up.
    abs: (Option<i32>, Option<i32>),
    touching: bool,
    /// Touchpads: the fingers on it, and what they come to.
    pad: Option<Pad>,
    /// Touchpads: the button a press of the pad itself went down as.
    pad_button: usize,
    /// Mice: how far this report has moved it so far, when the report
    /// before ended, and whether its wheel says parts of a notch.
    rel: (i32, i32),
    moved_at: Option<f64>,
    fine_wheel: bool,
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
        let mut rel_bits = [0u8; 2];
        let mut prop_bits = [0u8; 8];
        let mut name = [0u8; 128];
        let mut id = [0u16; 4];
        // Safety: each request writes at most the length encoded in it into
        // the array passed, which is that long.
        unsafe {
            libc::ioctl(fd, eviocgbit(EV_ABS, abs_bits.len()), abs_bits.as_mut_ptr());
            libc::ioctl(fd, eviocgbit(EV_KEY, key_bits.len()), key_bits.as_mut_ptr());
            libc::ioctl(fd, eviocgbit(EV_REL, rel_bits.len()), rel_bits.as_mut_ptr());
            libc::ioctl(fd, eviocgprop(prop_bits.len()), prop_bits.as_mut_ptr());
            libc::ioctl(fd, eviocgname(name.len() - 1), name.as_mut_ptr());
            libc::ioctl(fd, EVIOCGID, id.as_mut_ptr());
        }
        let has = |bits: &[u8], code: u16| bits[code as usize / 8] & (1 << (code % 8)) != 0;

        let mut pad = None;
        let pointer = if has(&abs_bits, ABS_X) && has(&abs_bits, ABS_Y) {
            // An axis's least and most, and its units a millimetre.
            let axis = |axis| {
                let mut info = AbsInfo::default();
                // Safety: writes one `input_absinfo`, which `AbsInfo` mirrors.
                unsafe { libc::ioctl(fd, eviocgabs(axis), &mut info) };
                ((info.minimum, info.maximum.max(info.minimum + 1)), info.resolution)
            };
            if has(&key_bits, BTN_TOOL_FINGER) {
                // Each finger by itself where the pad says so (slots);
                // their axes may differ from the one position's.
                let slots = [ABS_MT_SLOT, ABS_MT_POSITION_X, ABS_MT_POSITION_Y]
                    .iter()
                    .all(|&code| has(&abs_bits, code));
                let (across, down) = if slots {
                    (axis(ABS_MT_POSITION_X), axis(ABS_MT_POSITION_Y))
                } else {
                    (axis(ABS_X), axis(ABS_Y))
                };
                pad = Some(Pad::new(Shape {
                    x: across.0,
                    y: down.0,
                    resolution: (across.1, down.1),
                    buttonpad: has(&prop_bits, INPUT_PROP_BUTTONPAD),
                    slots,
                    semi: has(&prop_bits, INPUT_PROP_SEMI_MT),
                }));
                Pointer::Touchpad { x: across.0, y: down.0 }
            } else {
                Pointer::Absolute { x: axis(ABS_X).0, y: axis(ABS_Y).0 }
            }
        } else {
            Pointer::Relative
        };

        let kind = match pointer {
            Pointer::Touchpad { .. } => Some(Kind::Touchpad),
            // A screen is touched; anything else that says where it is has
            // buttons of its own.
            Pointer::Absolute { .. } if has(&key_bits, BTN_LEFT) => Some(Kind::Tablet),
            Pointer::Absolute { .. } => Some(Kind::Touchscreen),
            Pointer::Relative
                if has(&rel_bits, REL_X) && has(&rel_bits, REL_Y) && has(&key_bits, BTN_LEFT) =>
            {
                Some(Kind::Mouse)
            }
            // Letters and Enter: not a case's two or three buttons.
            Pointer::Relative
                if has(&key_bits, KEY_A) && has(&key_bits, KEY_Z) && has(&key_bits, KEY_ENTER) =>
            {
                Some(Kind::Keyboard)
            }
            Pointer::Relative => None,
        };
        let length = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        let found = kind.map(|kind| Found {
            kind,
            built_in: BUILT_IN_BUSES.contains(&id[0]),
            name: String::from_utf8_lossy(&name[..length]).trim().to_string(),
        });

        Some(Device {
            path,
            file,
            pointer,
            found,
            abs: (None, None),
            touching: false,
            pad,
            pad_button: 0,
            rel: (0, 0),
            moved_at: None,
            fine_wheel: has(&rel_bits, REL_WHEEL_HI_RES),
        })
    }
}

pub(super) struct Input {
    devices: Vec<Device>,
    last_scan: Instant,
    /// When input was last taken in: how long a scroll that is carrying
    /// on has carried on since.
    last_pump: Instant,
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
    /// A tap on a touchpad: the button it is holding down, and until when.
    tap: Option<(usize, Instant)>,
    /// A touch straight after a tap, dragging: whether the primary button
    /// has gone down for it yet (it waits for the tap's click to end).
    drag: Option<bool>,
    /// When a key was last pressed: a tap straight after is a hand
    /// brushing the pad.
    typed: Option<Instant>,
}

impl Input {
    pub(super) fn open(bounds: (usize, usize)) -> Input {
        let bounds = (bounds.0 as f64, bounds.1 as f64);
        let mut input = Input {
            devices: Vec::new(),
            last_scan: Instant::now(),
            last_pump: Instant::now(),
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
            tap: None,
            drag: None,
            typed: None,
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
        self.tell();
    }

    /// Say what devices there are now.
    fn tell(&self) {
        devices::set(self.devices.iter().filter_map(|d| d.found.clone()).collect());
    }

    pub(super) fn held(&self) -> &[Key] {
        &self.held
    }

    /// Read everything the devices have for us, without waiting.
    pub(super) fn pump(&mut self) {
        if self.last_scan.elapsed() >= RESCAN_EVERY {
            self.scan();
        }
        // A tap lets go of its button, having held it at least a frame.
        if let Some((button, until)) = self.tap
            && Instant::now() >= until
        {
            self.buttons[button] = false;
            self.tap = None;
        } else if self.drag == Some(false) && self.tap.is_none() {
            // The frame after: two taps are two clicks, not a long one.
            self.buttons[0] = true;
            self.drag = Some(true);
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
                            // When the kernel had it, in seconds: a
                            // touchpad's reports are timed by it, however
                            // late they are read.
                            let whole = |at: usize| {
                                i64::from_ne_bytes(raw[at..at + 8].try_into().expect("eight bytes"))
                            };
                            let at = whole(0) as f64 + whole(8) as f64 / 1e6;
                            let kind = u16::from_ne_bytes([raw[16], raw[17]]);
                            let code = u16::from_ne_bytes([raw[18], raw[19]]);
                            let value = i32::from_ne_bytes([raw[20], raw[21], raw[22], raw[23]]);
                            events.push((i, kind, code, value, at));
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
        for (device, kind, code, value, at) in events {
            self.event(device, kind, code, value, at);
        }
        // A scroll let go of while moving carries on, a frame's worth a
        // frame, until it has run down or its pad is touched.
        let since = self.last_pump.elapsed().as_secs_f64();
        self.last_pump = Instant::now();
        for device in &mut self.devices {
            let Some(pad) = &mut device.pad else {
                continue;
            };
            if !crate::pointer::coasts() {
                pad.still();
            }
            let (across, down) = pad.coast(since);
            if (across, down) != (0.0, 0.0) {
                let (wx, wy) = crate::pointer::pad_wheel((
                    across * touchpad::NOTCHES_A_PAD,
                    -down * touchpad::NOTCHES_A_PAD,
                ));
                self.wheel_acc.0 += wx;
                self.wheel_acc.1 += wy;
            }
        }
        // A tablet's or a screen's position is applied once per device
        // per pump, with whichever of X and Y moved. (A touchpad's is
        // taken report by report as it is read: see `report`.)
        for i in 0..self.devices.len() {
            self.apply_absolute(i);
        }
        if !gone.is_empty() {
            for i in gone.into_iter().rev() {
                self.devices.remove(i);
            }
            self.tell();
        }

        self.wheel = self.wheel_acc;
        self.wheel_acc = (0.0, 0.0);
    }

    fn event(&mut self, device: usize, kind: u16, code: u16, value: i32, at: f64) {
        match kind {
            EV_KEY => self.key(device, code, value),
            // The end of one report (`SYN_REPORT`).
            EV_SYN if code == 0 => self.report(device, at),
            EV_REL => match code {
                // Moved at the report's end, by how fast that was.
                REL_X => self.devices[device].rel.0 += value,
                REL_Y => self.devices[device].rel.1 += value,
                // Positive is away from the user, as Shine reports it. A
                // wheel that says parts of a notch is heard by those.
                REL_WHEEL if !self.devices[device].fine_wheel => self.wheel_acc.1 += value as f64,
                REL_HWHEEL if !self.devices[device].fine_wheel => self.wheel_acc.0 += value as f64,
                REL_WHEEL_HI_RES if self.devices[device].fine_wheel => {
                    self.wheel_acc.1 += value as f64 / 120.0
                }
                REL_HWHEEL_HI_RES if self.devices[device].fine_wheel => {
                    self.wheel_acc.0 += value as f64 / 120.0
                }
                _ => {}
            },
            EV_ABS => {
                let d = &mut self.devices[device];
                match (&mut d.pad, code) {
                    (Some(pad), _) => pad.axis(code, value),
                    (None, ABS_X) => d.abs.0 = Some(value),
                    (None, ABS_Y) => d.abs.1 = Some(value),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// A touchpad's report has ended: move the pointer, or scroll, by
    /// what its fingers did in it.
    fn report(&mut self, device: usize, at: f64) {
        let bounds = self.bounds;
        let d = &mut self.devices[device];
        let (dx, dy) = std::mem::take(&mut d.rel);
        if (dx, dy) != (0, 0) {
            // A mouse: further for the same movement made faster. Its
            // reports come every millisecond to every hundredth of a
            // second; one after a pause says nothing of how fast.
            let took = d.moved_at.map_or(0.008, |before| (at - before).clamp(0.001, 0.03));
            d.moved_at = Some(at);
            let (dx, dy) = (dx as f64, dy as f64);
            let quickened = if crate::pointer::quickens() {
                crate::pointer::quickened(dx.hypot(dy) / took)
            } else {
                1.0
            };
            let by = crate::pointer::speed() * quickened;
            self.move_by(dx * by, dy * by);
        }
        let d = &mut self.devices[device];
        let (Some(pad), Pointer::Touchpad { x, y }) = (&mut d.pad, d.pointer) else {
            return;
        };
        let moved = pad.report(at);
        let dragging = pad.dragging();
        if moved.pointer != (0.0, 0.0) {
            // A slow sweep of the pad's width covers about the screen's,
            // or as much of it as keeps the pad's own shape.
            let (wide, high) = ((x.1 - x.0) as f64, (y.1 - y.0) as f64);
            let reach = bounds.0.min(bounds.1 * wide / high) * crate::pointer::pad_speed();
            self.move_by(moved.pointer.0 * reach, moved.pointer.1 * reach);
        }
        if moved.scroll != (0.0, 0.0) {
            // As a wheel turns: fingers drawn toward the user are the
            // wheel turned toward the user, which Shine counts as negative.
            let (wx, wy) = crate::pointer::pad_wheel((
                moved.scroll.0 * touchpad::NOTCHES_A_PAD,
                -moved.scroll.1 * touchpad::NOTCHES_A_PAD,
            ));
            self.wheel_acc.0 += wx;
            self.wheel_acc.1 += wy;
        }
        // A second finger made the drag a scroll.
        if !dragging {
            self.drag_ends();
        }
    }

    /// The touch that held the button a tap clicked is over.
    fn drag_ends(&mut self) {
        match self.drag.take() {
            Some(true) => self.buttons[0] = false,
            // Over before its button went down: a click all the same,
            // unless the tap's own is still going.
            Some(false) if self.tap.is_none() => {
                self.buttons[0] = true;
                self.tap = Some((0, Instant::now() + TAP_HELD));
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
            Pointer::Touchpad { .. } => {}
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
        let touchpad = matches!(self.devices[device].pointer, Pointer::Touchpad { .. });
        match code {
            // A touchpad's own button, where the whole pad is the button:
            // the secondary one with two fingers on it.
            BTN_LEFT if touchpad => {
                let d = &mut self.devices[device];
                match &mut d.pad {
                    Some(pad) if down => d.pad_button = pad.pressed(),
                    Some(pad) => pad.released(),
                    None => {}
                }
                self.buttons[d.pad_button] = down;
            }
            BTN_LEFT => self.buttons[0] = down,
            BTN_RIGHT => self.buttons[1] = down,
            BTN_MIDDLE => self.buttons[2] = down,
            BTN_TOOL_FINGER | BTN_TOOL_DOUBLETAP | BTN_TOOL_TRIPLETAP | BTN_TOOL_QUADTAP
            | BTN_TOOL_QUINTTAP
                if touchpad =>
            {
                let count = match code {
                    BTN_TOOL_FINGER => 1,
                    BTN_TOOL_DOUBLETAP => 2,
                    BTN_TOOL_TRIPLETAP => 3,
                    BTN_TOOL_QUADTAP => 4,
                    _ => 5,
                };
                if let Some(pad) = &mut self.devices[device].pad {
                    pad.counted(count, down);
                }
            }
            BTN_TOUCH => {
                let now = Instant::now();
                let typing = self
                    .typed
                    .is_some_and(|at| now.duration_since(at) < touchpad::WHILE_TYPING);
                let d = &mut self.devices[device];
                d.touching = down;
                match (&mut d.pad, d.pointer) {
                    // A touchpad's touch is not a click, unless it was a
                    // tap.
                    (Some(pad), _) => {
                        let tapped = pad.touched(down, now, typing);
                        let taps = crate::pointer::tap_to_click();
                        match tapped {
                            Some(Tap::Click(button)) if taps => {
                                self.buttons[button] = true;
                                self.tap = Some((button, now + TAP_HELD));
                            }
                            Some(Tap::Hold) if taps && crate::pointer::tap_drags() => {
                                self.drag = Some(false)
                            }
                            Some(Tap::Release) => self.drag_ends(),
                            _ => {}
                        }
                    }
                    // A touchscreen's touch is its click.
                    (None, Pointer::Absolute { .. }) => self.buttons[0] = down,
                    _ => {}
                }
            }
            _ => {
                if value == 1 {
                    self.typed = Some(Instant::now());
                    for device in &mut self.devices {
                        if let Some(pad) = &mut device.pad {
                            pad.still();
                        }
                    }
                }
                self.keyboard(code, value)
            }
        }
    }

    fn keyboard(&mut self, code: u16, value: i32) {
        // A layout's AltGr picks a key's third character: it is part of
        // typing, and no program is told Alt is held.
        let third_level = code == KEY_RIGHTALT && layout::is_third_level(code);
        match code {
            KEY_LEFTSHIFT => self.shift[0] = value != 0,
            KEY_RIGHTSHIFT => self.shift[1] = value != 0,
            KEY_LEFTCTRL => self.ctrl[0] = value != 0,
            KEY_RIGHTCTRL => self.ctrl[1] = value != 0,
            KEY_LEFTALT => self.alt[0] = value != 0,
            KEY_RIGHTALT => self.alt[1] = value != 0 && !third_level,
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

        // The volume and brightness keys and their like: events by name,
        // again for each of the kernel's repeats, so holding one goes on
        // turning the volume.
        if let Some(name) = keymap::named(code) {
            crate::keyboard::push_named(name, value != 0);
            return;
        }

        // What the key types is asked of the layout before the layout
        // counts it as down: 1 = press, 0 = release, 2 = the kernel's
        // auto-repeat, which types the character again and changes nothing.
        let typed = if value == 0 {
            Typed::Text(None)
        } else {
            layout::typed(code)
        };
        if value != 2 {
            layout::moved(code, value == 1);
        }
        if third_level {
            return;
        }

        // Repeats are not new presses: Shine times key repeat itself.
        let mut collector = TextCollector;
        if let Some(key) = keymap::key(code) {
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
                }
                _ => {}
            }
        }

        // Ctrl and Alt combinations are shortcuts, not typing.
        if value == 0 || ctrl || alt {
            return;
        }
        match typed {
            Typed::BuiltIn => {
                let shift = self.shift.iter().any(|&d| d);
                if let Some(ch) = keymap::text(code, shift, self.caps) {
                    collector.add_char(ch as u32);
                }
            }
            Typed::Text(text) => {
                for ch in text.unwrap_or_default().chars() {
                    collector.add_char(ch as u32);
                }
            }
        }
    }
}
