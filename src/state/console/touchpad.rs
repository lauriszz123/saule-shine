//! What fingers on a touchpad mean.
//!
//! The kernel says where each finger is, a report at a time; a desktop's
//! input library usually makes pointer movement and gestures of that, and
//! on the bare screen there is none, so what a touchpad is unusable
//! without is made here:
//!
//! - **one finger moves the pointer**, by how far that finger moved since
//!   the report before: each finger is followed by itself (the kernel's
//!   slots), so another finger landing or lifting moves nothing. A pad
//!   that only says where "the" finger is starts afresh whenever the
//!   number of fingers changes, for the same reason;
//! - **the pointer keeps still under a still finger**: a pad's readings
//!   wander by a few units, and movement is only what is left after that
//!   much slack ([`SLACK`]). A reading that leaps, as the last one does
//!   when a finger lifts off, is passed over ([`LEAP`]);
//! - **slow is fine and fast is far**: the pointer moves further for the
//!   same distance when the finger moves faster ([`gain`]);
//! - **two fingers moving scroll**, as a wheel would, wherever on the pad
//!   they are and however they lie to each other, and the pointer keeps
//!   out of it: it is never moved and then put back. Two fingers never
//!   come down in the same report, so a finger that has just touched
//!   moves nothing for a moment ([`SETTLES`]). And once a touch has had
//!   two fingers on it, it scrolls until the last one lifts, however many
//!   the pad then says there are: two fingers close together are one wide
//!   finger to an older pad, in the middle of a scroll, and the one that
//!   lifts last is on its way off the pad, not pointing. A pad that
//!   counts two fingers and says where only one is scrolls by that one;
//! - **a scroll let go of while moving carries on**, slowing, as a wheel
//!   with some weight to it would ([`Pad::coast`]): until it has run
//!   down, or the pad is touched again;
//! - **a tap and then a touch drags**: a finger put back down straight
//!   after a tap ([`DRAGS_WITHIN`]) holds the primary button until it
//!   lifts, so what was tapped is dragged, and two taps are two clicks. A
//!   second finger joining it makes it a scroll, and lets go;
//! - **a tap is a click**: one finger the primary button, two the
//!   secondary, three the middle one. A tap is a touch that is short and
//!   stays where it is; not while typing, when the heel of a hand brushing
//!   the pad would otherwise click wherever the pointer is;
//! - **a pad that is its own button** (the whole surface presses down):
//!   pressed with two fingers on it, or at its bottom right, it is the
//!   secondary button. A finger that came down along its bottom edge,
//!   where the button is pressed, and stays where it came down, is a
//!   thumb waiting there: while another finger is on the pad it neither
//!   moves the pointer nor counts as a second finger, so pointing with
//!   one finger and pressing with the thumb does not scroll, or click the
//!   wrong button. One that moves ([`THUMB_MOVES`]), or goes along with
//!   the finger above it ([`ALONG`]), is no thumb: it is the lower of two
//!   fingers scrolling. Which of the two it is shows only once a finger
//!   has moved a millimetre or so, and until then nothing is moved: what
//!   a pointing finger did meanwhile is made up for as soon as it is
//!   known to be pointing. And **while the
//!   pad is held down nothing scrolls**: whichever finger moves, moves
//!   the pointer, which is how something is dragged with one finger
//!   holding the pad down and another moving.
//!
//! One [`Pad`] per touchpad, told what the kernel reports and asked what
//! it comes to. Nothing here reads a device or a clock, so all of it is
//! tested without either.

use std::time::{Duration, Instant};

/// The longest a touch lasts and is still a tap,
const TAP_TIME: Duration = Duration::from_millis(200);
/// and the furthest it moves, as a share of the pad's size.
const TAP_TRAVEL: f64 = 0.03;
/// How long after a key a tap is taken for a hand brushing the pad.
pub(super) const WHILE_TYPING: Duration = Duration::from_millis(400);

/// How many notches of a wheel a swipe the length of the pad scrolls.
pub(super) const NOTCHES_A_PAD: f64 = 10.0;

/// How far a reading may wander with nothing moving, as a share of the
/// pad's width: about a tenth of a millimetre.
const SLACK: f64 = 0.0015;
/// A move in one report this large, as a share of the pad's width (four
/// or five millimetres), and several times the one before, is the pad's
/// reading leaping and not the finger: a finger does not go from still
/// to there in a hundredth of a second.
const LEAP: f64 = 0.06;
/// A finger that comes down below this far down a pad that is its own
/// button is resting where the button is pressed,
const RESTS_BELOW: f64 = 0.82;
/// until it has come up past here.
const LEAVES_ABOVE: f64 = 0.72;
/// A press with a finger this far across the bottom is the secondary
/// button's.
const SECONDARY_FROM: f64 = 0.6;

/// How long after a touch begins the pointer stays put, in seconds: the
/// second finger of two is three or four reports behind the first.
const SETTLES: f64 = 0.06;
/// How far a finger resting where the pad is pressed moves, as a share of
/// the pad's width, before it is no thumb but a finger scrolling: about
/// two millimetres. A thumb waiting to press rolls less than that.
const THUMB_MOVES: f64 = 0.03;
/// How soon after a tap a finger must come back down for its touch to
/// hold the button the tap clicked.
const DRAGS_WITHIN: Duration = Duration::from_millis(250);
/// How far a finger beside one resting where the pad is pressed goes, as
/// a share of the pad's width, before it is known which the two are: a
/// millimetre or so. The one below has come along by then (two fingers
/// scrolling) or stayed (a thumb, and a finger pointing).
const ALONG: f64 = 0.012;
/// A finger that has gone this far is pointing, whatever the one below
/// did meanwhile.
const SURELY: f64 = 0.05;
/// A scroll let go of faster than this, in pads a second, carries on;
const COASTS_FROM: f64 = 0.7;
/// slowing so that it has this many seconds' worth of its speed still to
/// go (it loses about nine tenths of its speed in a second),
const COAST_TIME: f64 = 0.4;
/// and stops below this, in pads a second.
const COAST_ENDS: f64 = 0.04;
/// How far back a scroll's speed as it is let go of is measured, seconds.
const SPEED_OVER: f64 = 0.1;

/// The most fingers a pad follows.
const SLOTS: usize = 10;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

/// How much further the pointer goes for a finger moving at `speed`, in
/// widths of the pad a second: a little under the plain distance when
/// placing the pointer slowly, twice it when sweeping across.
pub(super) fn gain(speed: f64) -> f64 {
    (0.6 + 0.5 * speed).clamp(0.6, 2.0)
}

/// A touchpad as the kernel describes it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Shape {
    /// The least and most of each axis.
    pub(super) x: (i32, i32),
    pub(super) y: (i32, i32),
    /// Units a millimetre each way; 0 where the pad does not say.
    pub(super) resolution: (i32, i32),
    /// The whole pad presses down as its one button.
    pub(super) buttonpad: bool,
    /// Each finger is reported by itself, in a slot.
    pub(super) slots: bool,
    /// The slots hold two corners of a box round the fingers, not the
    /// fingers (an older Synaptics pad with two on it).
    pub(super) semi: bool,
}

impl Shape {
    fn width(&self) -> f64 {
        f64::from((self.x.1 - self.x.0).max(1))
    }

    fn height(&self) -> f64 {
        f64::from((self.y.1 - self.y.0).max(1))
    }

    /// How far across and down the pad a reading is, 0 to 1.
    fn across(&self, x: f64) -> f64 {
        (x - f64::from(self.x.0)) / self.width()
    }

    fn down(&self, y: f64) -> f64 {
        (y - f64::from(self.y.0)) / self.height()
    }

    /// Units down the pad as the same length in units across it.
    fn squared(&self, dy: f64) -> f64 {
        match self.resolution {
            (rx, ry) if rx > 0 && ry > 0 => dy * f64::from(rx) / f64::from(ry),
            _ => dy,
        }
    }
}

/// What a touch beginning or ending comes to, besides movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tap {
    /// A tap: a click of this button (0 primary, 1 secondary, 2 middle).
    Click(usize),
    /// A touch straight after a tap: the primary button goes down,
    Hold,
    /// and with that touch lifted, up.
    Release,
}

/// What a report came to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Motion {
    /// How far the pointer goes, across and down, in widths of the pad
    /// (the same length either way), with [`gain`] in it.
    pub(super) pointer: (f64, f64),
    /// How far two fingers scrolled, as shares of the pad's width and
    /// height.
    pub(super) scroll: (f64, f64),
}

#[derive(Clone, Copy)]
struct Slot {
    id: i32,
    x: i32,
    y: i32,
}

/// Which reading a contact is: a slot and the kernel's number for the
/// finger in it, or one of the stand-ins below.
type Key = (usize, i32);
/// The one position of a pad without slots; its second number is how many
/// fingers were down, so that a change of that is another contact.
const ONLY: usize = SLOTS;
/// The two corners of a box round two fingers.
const CORNER: usize = SLOTS + 1;

#[derive(Clone, Copy)]
struct Contact {
    key: Key,
    /// Where it is taken to be: the reading, less its slack.
    x: f64,
    y: f64,
    /// Came down where the pad is pressed, and has not left there.
    resting: bool,
    /// How far it moved in the report before, as a share of the width.
    last: f64,
    /// How far it is from where it came down, across and down.
    gone: (f64, f64),
}

pub(super) struct Pad {
    shape: Shape,
    /// What the kernel has said since the last report ended.
    slot: usize,
    slots: [Slot; SLOTS],
    only: (i32, i32),
    touching: bool,
    /// How many fingers the kernel counts (`BTN_TOOL_*`).
    counted: u8,
    /// Where each finger was at the last report.
    contacts: Vec<Contact>,
    /// When the last report was, in seconds.
    at: Option<f64>,
    /// The pad itself is held down.
    held: bool,
    /// When this touch's first report was.
    began: Option<f64>,
    /// This touch has had two fingers on it: it scrolls from then on.
    two: bool,
    /// When a tap of one finger last ended, and whether the touch on the
    /// pad now is one that began straight after and holds its button.
    tapped: Option<Instant>,
    dragging: bool,
    /// A finger beside a thumb is known to be pointing (see [`ALONG`]),
    /// and what it would have moved the pointer while it was not.
    sure: bool,
    unsure: (f64, f64),
    /// What was scrolled lately, and when: its speed as it is let go of.
    scrolled: Vec<(f64, f64, f64)>,
    /// A scroll carrying on after it was let go of: its speed across and
    /// down, in pads a second.
    coasting: Option<(f64, f64)>,
    touch: Touch,
}

impl Pad {
    pub(super) fn new(shape: Shape) -> Pad {
        Pad {
            shape,
            slot: 0,
            slots: [Slot { id: -1, x: 0, y: 0 }; SLOTS],
            only: (0, 0),
            touching: false,
            counted: 0,
            contacts: Vec::new(),
            at: None,
            held: false,
            began: None,
            two: false,
            tapped: None,
            dragging: false,
            sure: false,
            unsure: (0.0, 0.0),
            scrolled: Vec::new(),
            coasting: None,
            touch: Touch::default(),
        }
    }

    /// An absolute axis changed.
    pub(super) fn axis(&mut self, code: u16, value: i32) {
        match code {
            ABS_X => self.only.0 = value,
            ABS_Y => self.only.1 = value,
            ABS_MT_SLOT => self.slot = (value.max(0) as usize).min(SLOTS - 1),
            ABS_MT_TRACKING_ID => self.slots[self.slot].id = value,
            ABS_MT_POSITION_X => self.slots[self.slot].x = value,
            ABS_MT_POSITION_Y => self.slots[self.slot].y = value,
            _ => {}
        }
    }

    /// The kernel counts `count` fingers down (`down`), or no longer does.
    pub(super) fn counted(&mut self, count: u8, down: bool) {
        if down {
            self.counted = count;
        } else if self.counted == count {
            self.counted = 0;
        }
        // A pad that follows each finger says how many by its slots.
        if !self.shape.slots {
            self.touch.count(self.counted);
        }
    }

    /// The pad was touched, or the last finger left it, at `at`: what
    /// that comes to, if anything. `typing` is a key pressed a moment
    /// ago, which makes a tap a hand brushing the pad.
    pub(super) fn touched(&mut self, down: bool, at: Instant, typing: bool) -> Option<Tap> {
        self.touching = down;
        if down {
            // A hand on the pad stops what was carrying on.
            self.coasting = None;
            self.touch.began(at);
            let after_a_tap = self
                .tapped
                .take()
                .is_some_and(|tapped| at.duration_since(tapped) <= DRAGS_WITHIN);
            self.dragging = after_a_tap && !typing;
            return self.dragging.then_some(Tap::Hold);
        }
        self.let_go();
        self.contacts.clear();
        self.at = None;
        self.began = None;
        self.two = false;
        self.sure = false;
        self.unsure = (0.0, 0.0);
        self.scrolled.clear();
        let tapped = self.touch.ended(at, typing);
        if tapped == Some(0) {
            // Another touch straight after this one holds the button too:
            // a third tap is a third click.
            self.tapped = Some(at);
        }
        if std::mem::take(&mut self.dragging) {
            return Some(Tap::Release);
        }
        tapped.map(Tap::Click)
    }

    /// Whether the touch on the pad is holding the button a tap clicked.
    pub(super) fn dragging(&self) -> bool {
        self.dragging
    }

    /// A key was pressed: a scroll that was carrying on stops, so that
    /// Ctrl pressed after scrolling a page does not zoom it.
    pub(super) fn still(&mut self) {
        self.coasting = None;
    }

    /// The last finger has left: a scroll that was still moving carries
    /// on at the speed it had, measured over its last moments and not by
    /// its very last report, which is the fingers leaving.
    fn let_go(&mut self) {
        if !self.two || self.scrolled.len() < 3 {
            return;
        }
        let last = self.scrolled[self.scrolled.len() - 1].0;
        let lately: Vec<&(f64, f64, f64)> = self.scrolled[..self.scrolled.len() - 1]
            .iter()
            .filter(|(at, _, _)| last - at <= SPEED_OVER)
            .collect();
        let Some(first) = lately.first() else {
            return;
        };
        // From the report before the first of them to the last of them.
        let took = (lately[lately.len() - 1].0 - first.0) + 0.0125;
        let (across, down) = lately.iter().fold((0.0, 0.0), |sum, m| (sum.0 + m.1, sum.1 + m.2));
        let speed = (across / took, down / took);
        if speed.0.hypot(speed.1) >= COASTS_FROM {
            self.coasting = Some(speed);
        }
    }

    /// What a scroll that is carrying on scrolls in the next `seconds`:
    /// asked every frame, pad touched or not. As shares of the pad, like
    /// [`Motion::scroll`].
    pub(super) fn coast(&mut self, seconds: f64) -> (f64, f64) {
        let Some((across, down)) = self.coasting else {
            return (0.0, 0.0);
        };
        // The speed falls away smoothly; over these seconds it covers the
        // difference between what it had left to go and what it has now.
        let kept = (-seconds.clamp(0.0, 0.25) / COAST_TIME).exp();
        let slower = (across * kept, down * kept);
        self.coasting = (slower.0.hypot(slower.1) >= COAST_ENDS).then_some(slower);
        ((across - slower.0) * COAST_TIME, (down - slower.1) * COAST_TIME)
    }

    /// The pad itself was pressed down: which button that is.
    pub(super) fn pressed(&mut self) -> usize {
        self.touch.pressed = true;
        self.held = true;
        self.coasting = None;
        let at_the_right = self
            .lowest()
            .is_some_and(|(across, down)| down >= RESTS_BELOW && across >= SECONDARY_FROM);
        // A pad with buttons of its own below it: its left one is that,
        // however many fingers are on the pad.
        if !self.shape.buttonpad {
            return 0;
        }
        if at_the_right || self.touch.fingers >= 2 {
            1
        } else {
            0
        }
    }

    /// The pad was let go of.
    pub(super) fn released(&mut self) {
        self.held = false;
    }

    /// Where the finger furthest down the pad is, as shares across and
    /// down it: the one that presses a pad that is its own button.
    fn lowest(&self) -> Option<(f64, f64)> {
        let shape = self.shape;
        let low = self.contacts.iter().max_by(|a, b| a.y.total_cmp(&b.y))?;
        if low.key.0 != CORNER {
            return Some((shape.across(low.x), shape.down(low.y)));
        }
        // Two corners of a box: the fingers are at one pair of opposite
        // corners or the other. The one position the pad also gives is a
        // real finger's, which says which pair; the lower finger is that
        // one, or the one opposite it.
        let left = self.contacts.iter().map(|c| c.x).fold(f64::MAX, f64::min);
        let right = self.contacts.iter().map(|c| c.x).fold(f64::MIN, f64::max);
        let top = self.contacts.iter().map(|c| c.y).fold(f64::MAX, f64::min);
        let (x, y) = (f64::from(self.only.0), f64::from(self.only.1));
        let known_is_lower = (y - low.y).abs() <= (y - top).abs();
        let across = if known_is_lower { x } else { left + right - x };
        Some((shape.across(across), shape.down(low.y)))
    }

    /// The readings as they stand: each with what it is.
    fn readings(&self) -> Vec<(Key, f64, f64)> {
        if !self.touching {
            return Vec::new();
        }
        if !self.shape.slots {
            let (x, y) = self.only;
            return vec![((ONLY, i32::from(self.counted)), f64::from(x), f64::from(y))];
        }
        let down: Vec<(usize, &Slot)> = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.id >= 0)
            .collect();
        if self.shape.semi && down.len() >= 2 {
            let (a, b) = (down[0].1, down[1].1);
            return vec![
                (
                    (CORNER, 0),
                    f64::from(a.x.min(b.x)),
                    f64::from(a.y.min(b.y)),
                ),
                (
                    (CORNER, 1),
                    f64::from(a.x.max(b.x)),
                    f64::from(a.y.max(b.y)),
                ),
            ];
        }
        down.into_iter()
            .map(|(i, slot)| ((i, slot.id), f64::from(slot.x), f64::from(slot.y)))
            .collect()
    }

    /// A report has ended, at `at` seconds: what it comes to.
    pub(super) fn report(&mut self, at: f64) -> Motion {
        let shape = self.shape;
        let slack = SLACK * shape.width();
        let readings = self.readings();
        // A box round two fingers: whether one of them is a thumb where
        // the pad is pressed is settled as the box appears, by where its
        // bottom and top are then. Two fingers that scroll down into that
        // strip are still two fingers.
        let boxed = readings.first().is_some_and(|(key, _, _)| key.0 == CORNER);
        let lowest = readings.iter().map(|r| shape.down(r.2)).fold(f64::MIN, f64::max);
        let highest = readings.iter().map(|r| shape.down(r.2)).fold(f64::MAX, f64::min);
        let thumb_in_box = shape.buttonpad && lowest >= RESTS_BELOW && highest <= LEAVES_ABOVE;

        let mut next = Vec::new();
        let mut moves = Vec::new();
        for (key, x, y) in readings {
            let Some(was) = self.contacts.iter().find(|c| c.key == key) else {
                // Which finger points is to be seen afresh.
                self.sure = false;
                self.unsure = (0.0, 0.0);
                // A finger that has just come down has moved nowhere.
                let low = shape.buttonpad && shape.down(y) >= RESTS_BELOW;
                let resting = if boxed { thumb_in_box } else { low };
                next.push(Contact { key, x, y, resting, last: 0.0, gone: (0.0, 0.0) });
                moves.push((0.0, 0.0));
                continue;
            };
            let (nx, ny) = (taken_up(was.x, x, slack), taken_up(was.y, y, slack));
            let (dx, dy) = (nx - was.x, ny - was.y);
            let far = (dx.abs() + shape.squared(dy).abs()) / shape.width();
            let leapt = far > LEAP && far > was.last * 4.0 + 0.02;
            let (dx, dy) = if leapt { (0.0, 0.0) } else { (dx, dy) };
            let gone = (was.gone.0 + dx, was.gone.1 + dy);
            // A thumb stays where it came down: one that has left the
            // strip, or moved, is a finger. (A box's corners are seen to
            // together, below.)
            let moved = gone.0.hypot(shape.squared(gone.1)) / shape.width() > THUMB_MOVES;
            let resting = was.resting && (boxed || (shape.down(ny) > LEAVES_ABOVE && !moved));
            next.push(Contact { key, x: nx, y: ny, resting, last: far, gone });
            moves.push((dx, dy));
        }
        // A thumb that goes along with the finger above it is the lower
        // of two fingers; so is a box whose bottom has left the strip.
        if let Some((lower, upper, along)) = beside_a_thumb(&next, &shape, boxed) {
            let together = lower >= ALONG && lower * 2.0 >= upper && along;
            if lower > THUMB_MOVES || together || (boxed && lowest <= LEAVES_ABOVE) {
                for contact in &mut next {
                    contact.resting = false;
                }
            }
        }
        self.contacts = next;

        let before = self.at.replace(at);
        if self.contacts.is_empty() {
            self.at = None;
            return Motion::default();
        }
        let began = *self.began.get_or_insert(at);

        // Which of them move things, and how many fingers that is.
        let (fingers, (dx, dy)) = if boxed {
            let both = (moves[0].0 + moves[1].0, moves[0].1 + moves[1].1);
            if self.contacts[0].resting || self.held {
                // A thumb where the pad is pressed and a finger above it,
                // or one finger holding the pad down: that one is still,
                // so what the box did the other finger did.
                (1, both)
            } else {
                (self.counted.max(2), (both.0 / 2.0, both.1 / 2.0))
            }
        } else if !shape.slots || (shape.semi && self.counted >= 2) {
            // One position, however many fingers the pad counts: the
            // second finger of a box is a report or two behind the count.
            (self.counted.max(1), moves[0])
        } else if self.held && self.contacts.len() >= 2 {
            // Held down with more than one finger on it: one holds, and
            // the one that moves furthest drags.
            let far = |m: &(f64, f64)| m.0.abs() + m.1.abs();
            let most = moves.iter().copied().fold((0.0, 0.0), |most, m| {
                if far(&m) > far(&most) { m } else { most }
            });
            (1, most)
        } else {
            let mut moving: Vec<usize> =
                (0..self.contacts.len()).filter(|&i| !self.contacts[i].resting).collect();
            // A finger alone moves the pointer wherever it came down.
            if moving.is_empty() && self.contacts.len() == 1 {
                moving.push(0);
            }
            let n = moving.len().max(1) as f64;
            let sum = moving
                .iter()
                .fold((0.0, 0.0), |sum, &i| (sum.0 + moves[i].0, sum.1 + moves[i].1));
            (moving.len() as u8, (sum.0 / n, sum.1 / n))
        };
        if shape.slots {
            self.touch.count(fingers);
        }
        let (across, down) = (dx / shape.width(), shape.squared(dy) / shape.width());
        self.touch.travelled += across.abs() + down.abs();
        if fingers == 0 {
            return Motion::default();
        }
        // Held down, whichever finger moves drags, and at once: the pad
        // was pressed on purpose.
        let pointing = || {
            // Reports come eighty or so times a second; one that is late
            // is not a slow finger.
            let took = before.map_or(0.0125, |before| (at - before).clamp(0.004, 0.05));
            let faster = gain(across.hypot(down) / took);
            (across * faster, down * faster)
        };
        if self.held {
            return Motion { pointer: pointing(), scroll: (0.0, 0.0) };
        }
        if fingers >= 2 {
            self.two = true;
            // A scroll, not a drag: the button is let go of.
            self.dragging = false;
        }
        if self.two {
            let scroll = (across, dy / shape.height());
            self.scrolled.push((at, scroll.0, scroll.1));
            self.scrolled.retain(|(then, _, _)| at - then <= SPEED_OVER * 2.0);
            return Motion { pointer: (0.0, 0.0), scroll };
        }
        if at - began < SETTLES {
            return Motion::default();
        }
        let mut pointer = pointing();
        // Beside a finger that may be a thumb or may be about to scroll
        // with it, nothing moves until it is known which.
        if !self.sure
            && let Some((lower, upper, _)) = beside_a_thumb(&self.contacts, &shape, boxed)
        {
            self.unsure = (self.unsure.0 + pointer.0, self.unsure.1 + pointer.1);
            if (upper < ALONG || lower * 3.0 > upper) && upper < SURELY {
                return Motion::default();
            }
            self.sure = true;
            pointer = std::mem::take(&mut self.unsure);
        }
        Motion { pointer, scroll: (0.0, 0.0) }
    }
}

/// With a finger resting where the pad is pressed and one finger besides:
/// how far each is from where it came down, the resting one first, as
/// shares of the pad's width, and whether they went the same way.
///
/// A box round two fingers says where its sides are, not its fingers: the
/// lower finger has moved as far down as the bottom has and as far across
/// as the side that moved less, the upper one as the top and the side
/// that moved more.
fn beside_a_thumb(contacts: &[Contact], shape: &Shape, boxed: bool) -> Option<(f64, f64, bool)> {
    let [a, b] = contacts else {
        return None;
    };
    let far = |(x, y): (f64, f64)| x.hypot(shape.squared(y)) / shape.width();
    if boxed {
        if !a.resting {
            return None;
        }
        let (less, more) = if a.gone.0.abs() < b.gone.0.abs() {
            (a.gone.0, b.gone.0)
        } else {
            (b.gone.0, a.gone.0)
        };
        let (lower, upper) = ((less, b.gone.1), (more, a.gone.1));
        return Some((far(lower), far(upper), lower.1 * upper.1 >= 0.0));
    }
    let (thumb, finger) = match (a.resting, b.resting) {
        (true, false) => (a, b),
        (false, true) => (b, a),
        _ => return None,
    };
    let along = thumb.gone.0 * finger.gone.0 + thumb.gone.1 * finger.gone.1 > 0.0;
    Some((far(thumb.gone), far(finger.gone), along))
}

/// Where a contact that was taken to be at `was` is taken to be when it
/// reads `now`: no nearer the reading than `slack`, and not moved at all
/// while the reading is within that of it.
fn taken_up(was: f64, now: f64, slack: f64) -> f64 {
    if now > was + slack {
        now - slack
    } else if now < was - slack {
        now + slack
    } else {
        was
    }
}

/// One touch, from the first finger down to the last one up: whether it
/// was a tap.
#[derive(Default)]
struct Touch {
    /// Fingers on the pad now.
    fingers: u8,
    /// The most there were at once since the pad was touched.
    most: u8,
    /// When it was touched, while it is.
    since: Option<Instant>,
    /// How far the touch has moved, in shares of the pad's size.
    travelled: f64,
    /// The pad was pressed down during this touch: not a tap, then.
    pressed: bool,
}

impl Touch {
    fn count(&mut self, fingers: u8) {
        self.fingers = fingers;
        self.most = self.most.max(fingers);
    }

    /// The pad was touched.
    fn began(&mut self, at: Instant) {
        self.since = Some(at);
        self.most = self.most.max(self.fingers).max(1);
        self.travelled = 0.0;
        self.pressed = false;
    }

    /// The last finger left the pad. The button a tap clicks, if this
    /// touch was one.
    fn ended(&mut self, at: Instant, typing: bool) -> Option<usize> {
        let since = self.since.take()?;
        let most = std::mem::take(&mut self.most);
        self.fingers = 0;

        let brief = at.duration_since(since) <= TAP_TIME;
        let still = self.travelled <= TAP_TRAVEL;
        if !brief || !still || self.pressed || typing {
            return None;
        }
        match most {
            0 | 1 => Some(0),
            2 => Some(1),
            3 => Some(2),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        // One fixed start for every instant a test compares.
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *START.get_or_init(Instant::now) + Duration::from_millis(ms)
    }

    /// A pad 4000 units by 3000 that follows each finger; `buttonpad`
    /// when the whole of it presses down.
    fn pad(buttonpad: bool) -> Pad {
        Pad::new(Shape {
            x: (0, 4000),
            y: (0, 3000),
            resolution: (0, 0),
            buttonpad,
            slots: true,
            semi: false,
        })
    }

    /// An older pad: one position, and a count of fingers.
    fn plain() -> Pad {
        Pad::new(Shape {
            x: (1000, 5000),
            y: (1000, 4000),
            ..Shape::default()
        })
    }

    /// Put finger `id` in `slot` at (`x`, `y`), or lift it (`id` -1).
    fn finger(pad: &mut Pad, slot: i32, id: i32, x: i32, y: i32) {
        pad.axis(ABS_MT_SLOT, slot);
        pad.axis(ABS_MT_TRACKING_ID, id);
        if id >= 0 {
            pad.axis(ABS_MT_POSITION_X, x);
            pad.axis(ABS_MT_POSITION_Y, y);
        }
    }

    /// The first report of a touch (`n` 0), and then, once it has
    /// settled, reports eighty times a second.
    fn report(pad: &mut Pad, n: u32) -> Motion {
        let settled = if n == 0 { 0.0 } else { 0.05 };
        pad.report(settled + f64::from(n) * 0.0125)
    }

    #[test]
    fn one_finger_moves_the_pointer_by_how_far_it_moved() {
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 7, 1000, 1000);
        // Coming down moves nothing, wherever the pointer was left.
        assert_eq!(report(&mut pad, 0), Motion::default());
        finger(&mut pad, 0, 7, 1100, 1000);
        let moved = report(&mut pad, 1);
        assert!(moved.pointer.0 > 0.0 && moved.pointer.1 == 0.0, "{moved:?}");
        assert_eq!(moved.scroll, (0.0, 0.0));
        // Lifted and put down across the pad: nothing again.
        finger(&mut pad, 0, -1, 0, 0);
        pad.touched(false, at(100), false);
        report(&mut pad, 8);
        pad.touched(true, at(300), false);
        finger(&mut pad, 0, 8, 3500, 2500);
        assert_eq!(report(&mut pad, 24), Motion::default());
    }

    #[test]
    fn a_reading_that_wanders_moves_nothing() {
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 2000, 1500);
        report(&mut pad, 0);
        for (n, (x, y)) in [(2003, 1498), (1998, 1502), (2004, 1503), (1996, 1497)]
            .into_iter()
            .enumerate()
        {
            finger(&mut pad, 0, 1, x, y);
            assert_eq!(report(&mut pad, n as u32 + 1), Motion::default());
        }
        // Past the slack it moves, by what is left over.
        finger(&mut pad, 0, 1, 2030, 1500);
        assert!(report(&mut pad, 5).pointer.0 > 0.0);
    }

    #[test]
    fn a_reading_that_leaps_is_passed_over() {
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 2000, 1500);
        report(&mut pad, 0);
        finger(&mut pad, 0, 1, 2020, 1500);
        assert!(report(&mut pad, 1).pointer.0 > 0.0);
        // A fifth of the pad in one report, from nearly still: the pad's
        // reading as the finger lifts, not the finger.
        finger(&mut pad, 0, 1, 2800, 1700);
        assert_eq!(report(&mut pad, 2), Motion::default());
        // And from there it is followed again.
        finger(&mut pad, 0, 1, 2830, 1700);
        assert!(report(&mut pad, 3).pointer.0 > 0.0);
    }

    #[test]
    fn a_fast_sweep_is_not_a_leap() {
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        let mut total = 0.0;
        let mut x = 500;
        finger(&mut pad, 0, 1, x, 1500);
        report(&mut pad, 0);
        // Getting faster report by report, up to a tenth of the pad each.
        for (n, step) in [20, 60, 150, 300, 400, 400, 400].into_iter().enumerate() {
            x += step;
            finger(&mut pad, 0, 1, x, 1500);
            total += report(&mut pad, n as u32 + 1).pointer.0;
        }
        // All of it counted, and more than its length: it was fast.
        assert!(total > f64::from(x - 500) / 4000.0, "{total}");
    }

    #[test]
    fn a_faster_finger_goes_further_for_the_same_distance() {
        assert!(gain(0.05) < 1.0);
        assert!(gain(3.0) > gain(0.5));
        assert_eq!(gain(100.0), 2.0);
        // Never backwards: moving faster never moves the pointer less.
        let mut last = 0.0;
        for speed in (0..400).map(|n| f64::from(n) * 0.01) {
            assert!(speed * gain(speed) >= last);
            last = speed * gain(speed);
        }
    }

    #[test]
    fn another_finger_landing_or_lifting_moves_nothing() {
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1000, 1000);
        report(&mut pad, 0);
        // A second comes down far away: no pointer, no scroll.
        finger(&mut pad, 1, 2, 3000, 2000);
        assert_eq!(report(&mut pad, 1), Motion::default());
        // The first lifts: the pointer does not go to the second.
        finger(&mut pad, 0, -1, 0, 0);
        assert_eq!(report(&mut pad, 2), Motion::default());
        // Nor does it follow the second from then on, however long that
        // one stays: a touch that has had two fingers on it scrolls.
        finger(&mut pad, 1, 2, 3040, 2000);
        assert_eq!(report(&mut pad, 3).pointer, (0.0, 0.0));
        finger(&mut pad, 1, 2, 3080, 2050);
        let moved = report(&mut pad, 30);
        assert!(moved.pointer == (0.0, 0.0) && moved.scroll.1 > 0.0, "{moved:?}");
        // Lifted and put down again, one finger points.
        finger(&mut pad, 1, -1, 0, 0);
        pad.touched(false, at(900), false);
        pad.touched(true, at(1000), false);
        finger(&mut pad, 0, 3, 1000, 1000);
        pad.report(2.0);
        finger(&mut pad, 0, 3, 1100, 1000);
        assert!(pad.report(2.1).pointer.0 > 0.0);
    }

    #[test]
    fn two_fingers_read_as_one_in_the_middle_of_a_scroll_still_scroll() {
        // An older pad's two fingers, close together: it counts one for
        // a few reports, and says where one is.
        let mut pad = Pad::new(Shape {
            x: (0, 4000),
            y: (0, 3000),
            buttonpad: true,
            slots: true,
            semi: true,
            ..Shape::default()
        });
        pad.touched(true, at(0), false);
        pad.counted(2, true);
        finger(&mut pad, 0, 1, 1000, 1500);
        finger(&mut pad, 1, 2, 1300, 1400);
        report(&mut pad, 0);
        finger(&mut pad, 0, 1, 1000, 1400);
        finger(&mut pad, 1, 2, 1300, 1300);
        assert!(report(&mut pad, 1).scroll.1 < 0.0);
        pad.counted(2, false);
        pad.counted(1, true);
        finger(&mut pad, 1, -1, 0, 0);
        finger(&mut pad, 0, 1, 1150, 1350);
        report(&mut pad, 2);
        for (n, y) in [1250, 1150, 1050].into_iter().enumerate() {
            finger(&mut pad, 0, 1, 1150, y);
            let moved = report(&mut pad, n as u32 + 3);
            assert!(moved.pointer == (0.0, 0.0) && moved.scroll.1 < 0.0, "{y}: {moved:?}");
        }
    }

    /// An older pad whose whole surface presses down: two slots, a box.
    fn boxed() -> Pad {
        Pad::new(Shape {
            x: (0, 4000),
            y: (0, 3000),
            buttonpad: true,
            slots: true,
            semi: true,
            ..Shape::default()
        })
    }

    #[test]
    fn two_fingers_scroll_however_they_lie_and_wherever_they_come_down() {
        // One above the other, the lower one down in the strip where the
        // pad is pressed: on a pad that follows each finger, and on one
        // that gives a box round the two. Both move, so both are fingers.
        for mut pad in [pad(true), boxed()] {
            pad.touched(true, at(0), false);
            pad.counted(2, true);
            finger(&mut pad, 0, 1, 2000, 2800);
            finger(&mut pad, 1, 2, 2050, 1800);
            report(&mut pad, 0);
            let mut pointer = (0.0, 0.0);
            let mut scrolled = 0.0;
            for (n, up) in [40, 90, 150, 220, 300, 380, 460].into_iter().enumerate() {
                finger(&mut pad, 0, 1, 2000, 2800 - up);
                finger(&mut pad, 1, 2, 2050, 1800 - up);
                let moved = report(&mut pad, n as u32 + 1);
                // Not on and back again: not at all.
                assert_eq!(moved.pointer, (0.0, 0.0));
                pointer = (pointer.0 + moved.pointer.0, pointer.1 + moved.pointer.1);
                scrolled += moved.scroll.1;
            }
            // Scrolled up, and the pointer where it was.
            assert!(scrolled < -0.08, "{scrolled}");
            assert!(pointer.0.abs() < 1e-9 && pointer.1.abs() < 1e-9, "{pointer:?}");
        }
        // Side by side, across the pad: the same.
        for mut pad in [pad(true), boxed()] {
            pad.touched(true, at(0), false);
            pad.counted(2, true);
            finger(&mut pad, 0, 1, 1200, 2850);
            finger(&mut pad, 1, 2, 1900, 2800);
            report(&mut pad, 0);
            let mut pointer = 0.0;
            let mut scrolled = 0.0;
            for (n, along) in [40, 90, 150, 220, 300, 380].into_iter().enumerate() {
                finger(&mut pad, 0, 1, 1200 + along, 2850);
                finger(&mut pad, 1, 2, 1900 + along, 2800);
                let moved = report(&mut pad, n as u32 + 1);
                assert_eq!(moved.pointer, (0.0, 0.0));
                pointer += moved.pointer.0;
                scrolled += moved.scroll.0;
            }
            assert!(scrolled > 0.05 && pointer.abs() < 1e-9, "{scrolled} {pointer}");
        }
    }

    /// Two fingers drawn up the pad at `step` units a report, lifted
    /// after `still` more reports of holding where they were.
    fn swipe(step: i32, still: u32) -> Pad {
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1500, 2500);
        finger(&mut pad, 1, 2, 2100, 2500);
        report(&mut pad, 0);
        let mut y = 2500;
        for n in 1..=10 {
            y -= step;
            finger(&mut pad, 0, 1, 1500, y);
            finger(&mut pad, 1, 2, 2100, y);
            report(&mut pad, n);
        }
        for n in 0..still {
            report(&mut pad, 11 + n);
        }
        finger(&mut pad, 0, -1, 0, 0);
        finger(&mut pad, 1, -1, 0, 0);
        pad.touched(false, at(400), false);
        pad
    }

    #[test]
    fn a_scroll_let_go_of_while_moving_carries_on_and_runs_down() {
        // A sixtieth of a second at a time, as frames come.
        let mut pad = swipe(120, 0);
        let first = pad.coast(1.0 / 60.0);
        assert!(first.1 < 0.0 && first.0 == 0.0, "{first:?}");
        let mut total = first.1;
        let mut last = first.1;
        let mut frames = 1;
        loop {
            let more = pad.coast(1.0 / 60.0);
            if more == (0.0, 0.0) {
                break;
            }
            // Always the same way, and never faster than before.
            assert!(more.1 < 0.0 && more.1 >= last, "{more:?} after {last}");
            last = more.1;
            total += more.1;
            frames += 1;
            assert!(frames < 600, "it never stops");
        }
        // A good part of the pad further, over a second or two.
        assert!(total < -0.5 && frames > 30, "{total} in {frames} frames");
    }

    #[test]
    fn a_scroll_that_stopped_or_was_slow_does_not_carry_on() {
        // Held still before lifting.
        assert_eq!(swipe(120, 12).coast(1.0 / 60.0), (0.0, 0.0));
        // Slow all the way.
        assert_eq!(swipe(8, 0).coast(1.0 / 60.0), (0.0, 0.0));
        // One finger's flick is pointing, not scrolling.
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1500, 2500);
        report(&mut pad, 0);
        for n in 1..=8 {
            finger(&mut pad, 0, 1, 1500, 2500 - n as i32 * 120);
            report(&mut pad, n);
        }
        finger(&mut pad, 0, -1, 0, 0);
        pad.touched(false, at(300), false);
        assert_eq!(pad.coast(1.0 / 60.0), (0.0, 0.0));
    }

    #[test]
    fn touching_the_pad_stops_a_scroll_that_was_carrying_on() {
        let mut pad = swipe(120, 0);
        assert!(pad.coast(1.0 / 60.0).1 < 0.0);
        pad.touched(true, at(600), false);
        assert_eq!(pad.coast(1.0 / 60.0), (0.0, 0.0));
    }

    #[test]
    fn a_second_finger_arriving_leaves_the_pointer_where_it_is() {
        // The first finger pointed for a while; the second comes down.
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1000, 1500);
        pad.report(0.0);
        finger(&mut pad, 0, 1, 1000, 1400);
        assert!(pad.report(0.1).pointer.1 < 0.0);
        finger(&mut pad, 1, 2, 1400, 1300);
        assert_eq!(pad.report(0.1125), Motion::default());
        // From there it scrolls, and the pointer is not moved again:
        // neither on, nor back.
        for n in 1..6 {
            finger(&mut pad, 0, 1, 1000, 1400 - n * 50);
            finger(&mut pad, 1, 2, 1400, 1300 - n * 50);
            let moved = pad.report(0.1125 + n as f64 * 0.0125);
            assert!(moved.pointer == (0.0, 0.0) && moved.scroll.1 < 0.0, "{moved:?}");
        }
    }

    #[test]
    fn beside_a_thumb_a_finger_moves_nothing_until_it_is_known_to_be_pointing() {
        // On a pad that follows each finger, and on one that gives a box.
        for mut pad in [pad(true), boxed()] {
            pad.touched(true, at(0), false);
            pad.counted(2, true);
            finger(&mut pad, 0, 1, 2000, 2800);
            finger(&mut pad, 1, 2, 2050, 1800);
            report(&mut pad, 0);
            // The upper one sets off, the lower stays: for the first
            // millimetre it could as well be a scroll beginning.
            let mut steps = Vec::new();
            for (n, across) in [10, 20, 30, 40, 60, 80, 100].into_iter().enumerate() {
                finger(&mut pad, 1, 2, 2050 + across, 1800);
                let moved = report(&mut pad, n as u32 + 6);
                assert_eq!(moved.scroll, (0.0, 0.0));
                steps.push(moved.pointer.0);
            }
            assert_eq!(steps[0], 0.0, "{steps:?}");
            assert!(steps.iter().all(|&step| step >= 0.0), "{steps:?}");
            // Known, what it had moved is made up for: all of the way.
            let all: f64 = steps.iter().sum();
            let mut alone = self::pad(false);
            alone.touched(true, at(0), false);
            finger(&mut alone, 0, 1, 2050, 1800);
            report(&mut alone, 0);
            let mut wanted = 0.0;
            for (n, across) in [10, 20, 30, 40, 60, 80, 100].into_iter().enumerate() {
                finger(&mut alone, 0, 1, 2050 + across, 1800);
                wanted += report(&mut alone, n as u32 + 6).pointer.0;
            }
            assert!(all > 0.0 && (all - wanted).abs() < 1e-9, "{all} {wanted}");
        }
    }

    #[test]
    fn the_first_finger_of_two_moves_nothing_while_the_second_comes_down() {
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1000, 1000);
        pad.report(0.0);
        // Already moving, a report later: the second finger is not down
        // yet, and the pointer waits for it.
        finger(&mut pad, 0, 1, 1000, 1060);
        assert_eq!(pad.report(0.0125), Motion::default());
        finger(&mut pad, 0, 1, 1000, 1120);
        finger(&mut pad, 1, 2, 1600, 1100);
        assert_eq!(pad.report(0.025).pointer, (0.0, 0.0));
        finger(&mut pad, 0, 1, 1000, 1200);
        finger(&mut pad, 1, 2, 1600, 1180);
        let moved = pad.report(0.0375);
        assert!(moved.pointer == (0.0, 0.0) && moved.scroll.1 > 0.0, "{moved:?}");
    }

    #[test]
    fn two_fingers_scrolling_into_the_bottom_of_the_pad_still_scroll() {
        // Each finger by itself: they came down above the strip.
        let mut pad = pad(true);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1000, 1800);
        finger(&mut pad, 1, 2, 1600, 1400);
        report(&mut pad, 0);
        for (n, y) in [2000, 2200, 2400, 2600, 2800, 2950].into_iter().enumerate() {
            finger(&mut pad, 0, 1, 1000, y);
            finger(&mut pad, 1, 2, 1600, y - 400);
            let moved = report(&mut pad, n as u32 + 1);
            assert!(moved.pointer == (0.0, 0.0) && moved.scroll.1 > 0.0, "{y}: {moved:?}");
        }

        // A box round the two: the same, though its bottom is in the
        // strip and its top above it by the end.
        let mut pad = Pad::new(Shape {
            x: (0, 4000),
            y: (0, 3000),
            buttonpad: true,
            slots: true,
            semi: true,
            ..Shape::default()
        });
        pad.touched(true, at(0), false);
        pad.counted(2, true);
        finger(&mut pad, 0, 1, 1000, 1800);
        finger(&mut pad, 1, 2, 1600, 1400);
        report(&mut pad, 0);
        for (n, y) in [2000, 2200, 2400, 2600, 2800, 2950].into_iter().enumerate() {
            finger(&mut pad, 0, 1, 1000, y);
            finger(&mut pad, 1, 2, 1600, y - 400);
            let moved = report(&mut pad, n as u32 + 1);
            assert!(moved.pointer == (0.0, 0.0) && moved.scroll.1 > 0.0, "{y}: {moved:?}");
        }
    }

    #[test]
    fn a_pad_that_counts_two_and_places_one_scrolls_by_that_one() {
        let mut pad = Pad::new(Shape {
            x: (0, 4000),
            y: (0, 3000),
            buttonpad: true,
            slots: true,
            semi: true,
            ..Shape::default()
        });
        pad.touched(true, at(0), false);
        pad.counted(2, true);
        finger(&mut pad, 0, 1, 1000, 1000);
        report(&mut pad, 0);
        finger(&mut pad, 0, 1, 1000, 1150);
        let moved = report(&mut pad, 1);
        assert!(moved.pointer == (0.0, 0.0) && moved.scroll.1 > 0.0, "{moved:?}");
    }

    #[test]
    fn two_fingers_moving_scroll() {
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1000, 1000);
        finger(&mut pad, 1, 2, 1600, 1000);
        report(&mut pad, 0);
        finger(&mut pad, 0, 1, 1000, 1200);
        finger(&mut pad, 1, 2, 1600, 1200);
        let moved = report(&mut pad, 1);
        assert_eq!(moved.pointer, (0.0, 0.0));
        assert!(moved.scroll.0 == 0.0 && moved.scroll.1 > 0.06, "{moved:?}");
    }

    #[test]
    fn a_thumb_where_the_pad_is_pressed_is_not_a_second_finger() {
        let mut pad = pad(true);
        pad.touched(true, at(0), false);
        // The thumb at the bottom, then a finger above it.
        finger(&mut pad, 0, 1, 1500, 2800);
        report(&mut pad, 0);
        finger(&mut pad, 1, 2, 2000, 1000);
        report(&mut pad, 1);
        finger(&mut pad, 1, 2, 2100, 1000);
        let moved = report(&mut pad, 2);
        assert!(
            moved.pointer.0 > 0.0 && moved.scroll == (0.0, 0.0),
            "{moved:?}"
        );
        // Pressed: the primary button, the thumb being at the left.
        assert_eq!(pad.pressed(), 0);
        // On a pad with buttons of its own the same two fingers scroll.
        let mut pad = self::pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1500, 2800);
        finger(&mut pad, 1, 2, 2000, 1000);
        report(&mut pad, 0);
        finger(&mut pad, 1, 2, 2000, 1200);
        finger(&mut pad, 0, 1, 1500, 3000);
        assert!(report(&mut pad, 1).scroll.1 > 0.0);
    }

    #[test]
    fn a_finger_alone_at_the_bottom_moves_the_pointer() {
        let mut pad = pad(true);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1500, 2800);
        report(&mut pad, 0);
        finger(&mut pad, 0, 1, 1700, 2800);
        assert!(report(&mut pad, 1).pointer.0 > 0.0);
    }

    #[test]
    fn a_thumb_that_comes_up_the_pad_is_a_finger() {
        let mut pad = pad(true);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1500, 2800);
        finger(&mut pad, 1, 2, 2000, 1000);
        report(&mut pad, 0);
        // Up past the line, a little a report: two fingers from then on.
        for (n, y) in [2600, 2400, 2200, 2000].into_iter().enumerate() {
            finger(&mut pad, 0, 1, 1500, y);
            report(&mut pad, n as u32 + 1);
        }
        finger(&mut pad, 0, 1, 1500, 1800);
        finger(&mut pad, 1, 2, 2000, 800);
        let moved = report(&mut pad, 5);
        assert!(
            moved.pointer == (0.0, 0.0) && moved.scroll.1 < 0.0,
            "{moved:?}"
        );
    }

    #[test]
    fn pressing_the_pad_at_its_bottom_right_or_with_two_fingers_is_the_secondary_button() {
        let mut pad = pad(true);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 3500, 2900);
        report(&mut pad, 0);
        assert_eq!(pad.pressed(), 1);

        let mut pad = self::pad(true);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1000, 1000);
        finger(&mut pad, 1, 2, 1600, 1000);
        report(&mut pad, 0);
        assert_eq!(pad.pressed(), 1);

        let mut pad = self::pad(true);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1000, 2900);
        report(&mut pad, 0);
        assert_eq!(pad.pressed(), 0);
    }

    #[test]
    fn a_pad_with_buttons_of_its_own_clicks_what_its_buttons_say() {
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 3500, 2900);
        finger(&mut pad, 1, 2, 1000, 1000);
        report(&mut pad, 0);
        assert_eq!(pad.pressed(), 0);
    }

    #[test]
    fn held_down_the_finger_that_moves_drags() {
        // Two fingers anywhere on the pad, which would scroll: pressed
        // down, the one that moves moves the pointer, until let go of.
        let mut pad = pad(true);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1000, 1000);
        report(&mut pad, 0);
        assert_eq!(pad.pressed(), 0);
        finger(&mut pad, 1, 2, 2400, 1200);
        report(&mut pad, 1);
        finger(&mut pad, 1, 2, 2500, 1300);
        let moved = report(&mut pad, 2);
        assert!(moved.pointer.0 > 0.0 && moved.pointer.1 > 0.0, "{moved:?}");
        assert_eq!(moved.scroll, (0.0, 0.0));
        pad.released();
        finger(&mut pad, 0, 1, 1000, 1100);
        finger(&mut pad, 1, 2, 2500, 1400);
        let moved = report(&mut pad, 3);
        assert!(
            moved.pointer == (0.0, 0.0) && moved.scroll.1 > 0.0,
            "{moved:?}"
        );

        // A pad with one position, two fingers counted: the same.
        let mut pad = plain();
        pad.counted(2, true);
        pad.touched(true, at(0), false);
        pad.axis(ABS_X, 2000);
        pad.axis(ABS_Y, 2000);
        report(&mut pad, 0);
        pad.pressed();
        pad.axis(ABS_X, 2150);
        let moved = report(&mut pad, 1);
        assert!(
            moved.pointer.0 > 0.0 && moved.scroll == (0.0, 0.0),
            "{moved:?}"
        );
    }

    #[test]
    fn a_box_round_two_fingers_scrolls_or_points_past_a_thumb() {
        let semi = |buttonpad| {
            Pad::new(Shape {
                x: (0, 4000),
                y: (0, 3000),
                buttonpad,
                slots: true,
                semi: true,
                ..Shape::default()
            })
        };
        // Two fingers side by side, drawn down the pad: the box moves as
        // they do, and that scrolls.
        let mut pad = semi(true);
        pad.touched(true, at(0), false);
        pad.counted(2, true);
        finger(&mut pad, 0, 1, 1000, 1000);
        finger(&mut pad, 1, 2, 1600, 1100);
        report(&mut pad, 0);
        finger(&mut pad, 0, 1, 1000, 1200);
        finger(&mut pad, 1, 2, 1600, 1300);
        let moved = report(&mut pad, 1);
        assert!(
            moved.pointer == (0.0, 0.0) && moved.scroll.1 > 0.06,
            "{moved:?}"
        );

        // A thumb at the bottom and a finger moving above it: the corner
        // that moves is the finger's, and the pointer goes with it.
        let mut pad = semi(true);
        pad.touched(true, at(0), false);
        pad.counted(2, true);
        finger(&mut pad, 0, 1, 1200, 2850);
        finger(&mut pad, 1, 2, 2400, 900);
        report(&mut pad, 0);
        finger(&mut pad, 0, 1, 1200, 2850);
        finger(&mut pad, 1, 2, 2500, 900);
        let moved = report(&mut pad, 1);
        assert!(
            moved.pointer.0 > 0.0 && moved.scroll == (0.0, 0.0),
            "{moved:?}"
        );
        // The pad's one position is the finger's, up at the right: the
        // thumb is the corner opposite, at the bottom left. Primary.
        pad.axis(ABS_X, 2500);
        pad.axis(ABS_Y, 900);
        assert_eq!(pad.pressed(), 0);

        // The thumb at the bottom right and the finger up at the left,
        // whichever of them the one position is: secondary.
        for known in [(900, 800), (3300, 2850)] {
            let mut pad = semi(true);
            pad.touched(true, at(0), false);
            pad.counted(2, true);
            finger(&mut pad, 0, 1, 900, 2850);
            finger(&mut pad, 1, 2, 3300, 800);
            pad.axis(ABS_X, known.0);
            pad.axis(ABS_Y, known.1);
            report(&mut pad, 0);
            assert_eq!(pad.pressed(), 1, "{known:?}");
        }
    }

    #[test]
    fn a_pad_with_one_position_starts_afresh_when_the_fingers_change() {
        let mut pad = plain();
        pad.counted(1, true);
        pad.touched(true, at(0), false);
        pad.axis(ABS_X, 2000);
        pad.axis(ABS_Y, 2000);
        report(&mut pad, 0);
        pad.axis(ABS_X, 2100);
        assert!(report(&mut pad, 1).pointer.0 > 0.0);
        // A second finger: the position is between them now. No jump,
        // and from here it scrolls.
        pad.counted(1, false);
        pad.counted(2, true);
        pad.axis(ABS_X, 2600);
        assert_eq!(report(&mut pad, 2), Motion::default());
        pad.axis(ABS_Y, 2200);
        let moved = report(&mut pad, 3);
        assert!(
            moved.pointer == (0.0, 0.0) && moved.scroll.1 > 0.0,
            "{moved:?}"
        );
    }

    /// A touch on a plain pad with `fingers` down from `from` to `to`
    /// milliseconds, moving `travel` of the pad: what it clicks.
    fn touch(fingers: u8, from: u64, to: u64, travel: f64, typing: bool) -> Option<usize> {
        let mut pad = plain();
        pad.counted(fingers, true);
        pad.touched(true, at(from), false);
        pad.axis(ABS_X, 2000);
        pad.axis(ABS_Y, 2000);
        pad.report(0.0);
        // Moved a step a report, as a finger does.
        for step in 1..=8 {
            pad.axis(
                ABS_X,
                2000 + (travel * 4000.0 * f64::from(step) / 8.0) as i32,
            );
            pad.report(f64::from(step) * 0.0125);
        }
        pad.counted(fingers, false);
        match pad.touched(false, at(to), typing) {
            Some(Tap::Click(button)) => Some(button),
            _ => None,
        }
    }

    #[test]
    fn a_short_still_touch_is_a_click() {
        assert_eq!(touch(1, 0, 90, 0.005, false), Some(0));
    }

    #[test]
    fn two_fingers_tap_the_secondary_button_and_three_the_middle() {
        assert_eq!(touch(2, 0, 90, 0.0, false), Some(1));
        assert_eq!(touch(3, 0, 90, 0.0, false), Some(2));
        assert_eq!(touch(4, 0, 90, 0.0, false), None);
    }

    #[test]
    fn a_touch_that_lasts_or_moves_is_not_a_tap() {
        assert_eq!(touch(1, 0, 400, 0.0, false), None);
        assert_eq!(touch(1, 0, 90, 0.2, false), None);
    }

    #[test]
    fn a_tap_while_typing_is_a_hand_brushing_the_pad() {
        assert_eq!(touch(1, 0, 90, 0.0, true), None);
    }

    #[test]
    fn a_tap_past_a_resting_thumb_is_the_primary_button() {
        let mut pad = pad(true);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1500, 2800);
        report(&mut pad, 0);
        // The finger taps while the thumb stays: the thumb's own touch
        // goes on, so the pad is not let go of; the count never passed 1.
        finger(&mut pad, 1, 2, 2000, 1000);
        report(&mut pad, 1);
        finger(&mut pad, 1, -1, 0, 0);
        report(&mut pad, 2);
        finger(&mut pad, 0, -1, 0, 0);
        assert_eq!(pad.touched(false, at(90), false), Some(Tap::Click(0)));
    }

    #[test]
    fn a_press_of_the_pad_is_not_also_a_tap() {
        let mut pad = pad(true);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1000, 1000);
        report(&mut pad, 0);
        pad.pressed();
        finger(&mut pad, 0, -1, 0, 0);
        assert_eq!(pad.touched(false, at(80), false), None);
    }

    #[test]
    fn a_touch_starts_clean_after_the_one_before() {
        // Two fingers, then one: the second tap is the primary button's.
        let mut pad = pad(false);
        pad.touched(true, at(0), false);
        finger(&mut pad, 0, 1, 1000, 1000);
        finger(&mut pad, 1, 2, 1600, 1000);
        report(&mut pad, 0);
        finger(&mut pad, 0, -1, 0, 0);
        finger(&mut pad, 1, -1, 0, 0);
        assert_eq!(pad.touched(false, at(50), false), Some(Tap::Click(1)));

        pad.touched(true, at(500), false);
        finger(&mut pad, 0, 3, 1000, 1000);
        report(&mut pad, 40);
        finger(&mut pad, 0, -1, 0, 0);
        assert_eq!(pad.touched(false, at(560), false), Some(Tap::Click(0)));
        // Lifting with no touch begun says nothing.
        assert_eq!(pad.touched(false, at(600), false), None);
    }

    /// One finger down at `from` ms and up at `to`, not having moved.
    fn tap(pad: &mut Pad, from: u64, to: u64) -> (Option<Tap>, Option<Tap>) {
        let down = pad.touched(true, at(from), false);
        finger(pad, 0, 1, 1000, 1000);
        pad.report(from as f64 / 1000.0);
        finger(pad, 0, -1, 0, 0);
        (down, pad.touched(false, at(to), false))
    }

    #[test]
    fn a_touch_straight_after_a_tap_holds_the_button_until_it_lifts() {
        let mut pad = pad(false);
        assert_eq!(tap(&mut pad, 0, 80), (None, Some(Tap::Click(0))));
        // Back down a tenth of a second on, and off across the pad: held
        // all the way, pointing as it goes, and let go of when it lifts.
        assert_eq!(pad.touched(true, at(180), false), Some(Tap::Hold));
        finger(&mut pad, 0, 2, 1000, 1000);
        pad.report(0.18);
        let mut moved = 0.0;
        for n in 1..=20 {
            finger(&mut pad, 0, 2, 1000 + n * 40, 1000);
            moved += pad.report(0.18 + n as f64 * 0.0125).pointer.0;
            assert!(pad.dragging());
        }
        assert!(moved > 0.1, "{moved}");
        finger(&mut pad, 0, -1, 0, 0);
        assert_eq!(pad.touched(false, at(900), false), Some(Tap::Release));
        // That was a drag, not a tap: the next touch is its own.
        assert_eq!(pad.touched(true, at(1000), false), None);
    }

    #[test]
    fn taps_one_after_another_are_as_many_clicks() {
        let mut pad = pad(false);
        assert_eq!(tap(&mut pad, 0, 80), (None, Some(Tap::Click(0))));
        // The second holds and lets go, which is a click; so the third.
        assert_eq!(tap(&mut pad, 200, 270), (Some(Tap::Hold), Some(Tap::Release)));
        assert_eq!(tap(&mut pad, 400, 470), (Some(Tap::Hold), Some(Tap::Release)));
        // Much later, a tap is a tap again.
        assert_eq!(tap(&mut pad, 2000, 2070), (None, Some(Tap::Click(0))));
    }

    #[test]
    fn a_touch_long_after_a_tap_or_after_a_secondary_one_holds_nothing() {
        let mut pad = pad(false);
        tap(&mut pad, 0, 80);
        assert_eq!(pad.touched(true, at(600), false), None);
        finger(&mut pad, 0, 1, 1000, 1000);
        pad.report(0.6);
        finger(&mut pad, 0, -1, 0, 0);
        pad.touched(false, at(1500), false);
        // Two fingers' tap is the secondary button: nothing to drag.
        assert_eq!(touch(2, 0, 60, 0.0, false), Some(1));
        let mut pad = self::pad(false);
        pad.touched(true, at(0), false);
        pad.counted(2, true);
        finger(&mut pad, 0, 1, 1000, 1000);
        finger(&mut pad, 1, 2, 1500, 1000);
        pad.report(0.0);
        finger(&mut pad, 0, -1, 0, 0);
        finger(&mut pad, 1, -1, 0, 0);
        pad.counted(2, false);
        assert_eq!(pad.touched(false, at(60), false), Some(Tap::Click(1)));
        assert_eq!(pad.touched(true, at(150), false), None);
    }

    #[test]
    fn a_second_finger_makes_a_drag_a_scroll_and_lets_go() {
        let mut pad = pad(false);
        tap(&mut pad, 0, 80);
        assert_eq!(pad.touched(true, at(180), false), Some(Tap::Hold));
        finger(&mut pad, 0, 2, 1000, 1000);
        pad.report(0.18);
        assert!(pad.dragging());
        finger(&mut pad, 1, 3, 1500, 1000);
        pad.report(0.2);
        assert!(!pad.dragging());
        finger(&mut pad, 0, 2, 1000, 1100);
        finger(&mut pad, 1, 3, 1500, 1100);
        assert!(pad.report(0.2125).scroll.1 > 0.0);
        finger(&mut pad, 0, -1, 0, 0);
        finger(&mut pad, 1, -1, 0, 0);
        // Its end is not the drag's: that was let go of already.
        assert_ne!(pad.touched(false, at(900), false), Some(Tap::Release));
    }

    #[test]
    fn a_key_pressed_stops_a_scroll_that_was_carrying_on() {
        let mut pad = swipe(120, 0);
        assert!(pad.coast(1.0 / 60.0).1 < 0.0);
        pad.still();
        assert_eq!(pad.coast(1.0 / 60.0), (0.0, 0.0));
    }

}
