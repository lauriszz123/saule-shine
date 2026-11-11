//! `Event` — the frame's input as typed objects, for a loop you write
//! yourself.
//!
//! Love2D's `love.run` pumps the queue and hands each event to a callback.
//! Shine leaves the loop to the program, as `love.run` itself does: pump,
//! read the events, update, draw, present. `Event.poll()` is the reading
//! part, and each event is an object whose `kind` is an `EventKind` to
//! `match` on:
//!
//! ```text
//! for e in Event.poll() do
//!     match e.kind
//!         case EventKind.KeyPressed then onKey(e.key)
//!         case EventKind.MousePressed then click(e.x, e.y, e.button)
//!         case _ then
//!     end
//! end
//! ```
//!
//! `Window.pollEvents()` still returns the untyped `[kind, payload…]`
//! records, for code written against it.

use saule_sdk::prelude::*;

use crate::event::Event as Raw;
use crate::state;

/// What happened.
#[saule_enum]
#[derive(Clone, Copy, PartialEq)]
pub enum EventKind {
    /// A key went down: `key`, and `shift`, `ctrl`, `alt`.
    KeyPressed,
    /// A key came up: `key`, and the modifiers.
    KeyReleased,
    /// Typed text, layout- and modifier-aware: `text`.
    TextInput,
    /// The pointer moved: `x`, `y`, and the change `dx`, `dy`.
    MouseMoved,
    /// A button went down: `x`, `y`, `button` (1 left, 2 right, 3 middle).
    MousePressed,
    /// A button came up: `x`, `y`, `button`.
    MouseReleased,
    /// A second press close to the first: `x`, `y`, `button`.
    MouseDoubleClicked,
    /// The wheel turned: `dx`, `dy` in notches.
    WheelMoved,
    /// The pointer came over the window: `x`, `y`.
    MouseEntered,
    /// The pointer left the window.
    MouseLeft,
    /// The window changed size: `width`, `height`.
    Resized,
    /// The window gained or lost focus: `focused`.
    FocusChanged,
    /// The user asked to close the window. Love2D's `quit`.
    Quit,
}

/// One thing that happened since the last poll. Fields that do not apply to
/// its `kind` are zero, `""` or `false`.
#[saule_class]
pub struct Event {
    kind: EventKind,
    key: String,
    text: String,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
    button: i64,
    width: i64,
    height: i64,
    focused: bool,
    shift: bool,
    ctrl: bool,
    alt: bool,
}

impl Event {
    fn blank(kind: EventKind) -> Self {
        Event {
            kind,
            key: String::new(),
            text: String::new(),
            x: 0.0,
            y: 0.0,
            dx: 0.0,
            dy: 0.0,
            button: 0,
            width: 0,
            height: 0,
            focused: false,
            shift: false,
            ctrl: false,
            alt: false,
        }
    }

    fn from_raw(raw: &Raw) -> Self {
        match raw {
            Raw::KeyPressed {
                key,
                shift,
                ctrl,
                alt,
            }
            | Raw::KeyReleased {
                key,
                shift,
                ctrl,
                alt,
            } => Event {
                key: (*key).to_string(),
                shift: *shift,
                ctrl: *ctrl,
                alt: *alt,
                ..Event::blank(if matches!(raw, Raw::KeyPressed { .. }) {
                    EventKind::KeyPressed
                } else {
                    EventKind::KeyReleased
                })
            },
            Raw::TextInput(text) => Event {
                text: text.clone(),
                ..Event::blank(EventKind::TextInput)
            },
            Raw::MouseMoved { x, y, dx, dy } => Event {
                x: *x,
                y: *y,
                dx: *dx,
                dy: *dy,
                ..Event::blank(EventKind::MouseMoved)
            },
            Raw::MousePressed { x, y, button } => Event {
                x: *x,
                y: *y,
                button: *button,
                ..Event::blank(EventKind::MousePressed)
            },
            Raw::MouseReleased { x, y, button } => Event {
                x: *x,
                y: *y,
                button: *button,
                ..Event::blank(EventKind::MouseReleased)
            },
            Raw::MouseDoubleClicked { x, y, button } => Event {
                x: *x,
                y: *y,
                button: *button,
                ..Event::blank(EventKind::MouseDoubleClicked)
            },
            Raw::WheelMoved { dx, dy } => Event {
                dx: *dx,
                dy: *dy,
                ..Event::blank(EventKind::WheelMoved)
            },
            Raw::MouseEntered { x, y } => Event {
                x: *x,
                y: *y,
                ..Event::blank(EventKind::MouseEntered)
            },
            Raw::MouseLeft => Event::blank(EventKind::MouseLeft),
            Raw::Resized { width, height } => Event {
                width: *width,
                height: *height,
                ..Event::blank(EventKind::Resized)
            },
            Raw::FocusChanged(focused) => Event {
                focused: *focused,
                ..Event::blank(EventKind::FocusChanged)
            },
            Raw::Closed => Event::blank(EventKind::Quit),
        }
    }
}

#[saule_methods]
impl Event {
    /// Pump the window's queue and return what happened since the last
    /// poll, in order. Call once per frame, before updating.
    pub fn poll() -> Result<Vec<Event>, String> {
        state::with(|e| {
            e.poll_events();
            e.events().iter().map(Event::from_raw).collect()
        })
    }

    /// Ask the loop to end: closes the window, so `Window.isOpen()` turns
    /// false. Love2D's `love.event.quit()`.
    pub fn quit() {
        state::close();
    }

    #[saule(getter)]
    pub fn kind(&self) -> EventKind {
        self.kind
    }

    /// The key, by Love2D's names: `"a"`, `"space"`, `"left"`, `"escape"`…
    #[saule(getter)]
    pub fn key(&self) -> String {
        self.key.clone()
    }

    #[saule(getter)]
    pub fn text(&self) -> String {
        self.text.clone()
    }

    #[saule(getter)]
    pub fn x(&self) -> f64 {
        self.x
    }

    #[saule(getter)]
    pub fn y(&self) -> f64 {
        self.y
    }

    #[saule(getter)]
    pub fn dx(&self) -> f64 {
        self.dx
    }

    #[saule(getter)]
    pub fn dy(&self) -> f64 {
        self.dy
    }

    #[saule(getter)]
    pub fn button(&self) -> i64 {
        self.button
    }

    #[saule(getter)]
    pub fn width(&self) -> i64 {
        self.width
    }

    #[saule(getter)]
    pub fn height(&self) -> i64 {
        self.height
    }

    #[saule(getter)]
    pub fn focused(&self) -> bool {
        self.focused
    }

    #[saule(getter)]
    pub fn shift(&self) -> bool {
        self.shift
    }

    #[saule(getter)]
    pub fn ctrl(&self) -> bool {
        self.ctrl
    }

    #[saule(getter)]
    pub fn alt(&self) -> bool {
        self.alt
    }
}
