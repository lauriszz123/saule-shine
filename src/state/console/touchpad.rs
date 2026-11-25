//! What fingers on a touchpad mean, beyond moving the pointer.
//!
//! The kernel says where the first finger is and how many are down; a
//! desktop's input library usually makes gestures of that, and on the bare
//! screen there is none, so the few a touchpad is unusable without are
//! made here:
//!
//! - **two fingers moving scroll**, as a wheel would;
//! - **a tap is a click**: one finger the primary button, two the
//!   secondary, three the middle one. A tap is a touch that is short and
//!   stays where it is; not while typing, when the heel of a hand brushing
//!   the pad would otherwise click wherever the pointer is;
//! - **pressing the pad down with two fingers on it** is the secondary
//!   button too: a newer laptop's whole pad is one button.
//!
//! One [`Touch`] per touchpad, told what the kernel reports and asked what
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

/// What fingers moving on the pad move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Moves {
    Pointer,
    Scroll,
}

#[derive(Default)]
pub(super) struct Touch {
    /// Fingers on the pad now, as the kernel last said.
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
    /// The kernel said `count` fingers are down (`down`), or no longer
    /// are. True when the number changed: the finger whose position is
    /// reported may be another one now, so movement starts afresh.
    pub(super) fn fingers(&mut self, count: u8, down: bool) -> bool {
        let now = if down {
            count
        } else if self.fingers == count {
            0
        } else {
            self.fingers
        };
        let changed = now != self.fingers;
        self.fingers = now;
        self.most = self.most.max(now);
        changed
    }

    /// The pad was touched.
    pub(super) fn began(&mut self, at: Instant) {
        self.since = Some(at);
        self.most = self.most.max(self.fingers).max(1);
        self.travelled = 0.0;
        self.pressed = false;
    }

    /// The last finger left the pad. The button a tap clicks (0 primary,
    /// 1 secondary, 2 middle), if this touch was one.
    pub(super) fn ended(&mut self, at: Instant, typing: bool) -> Option<usize> {
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

    /// The touch moved by `share` of the pad's size: what that moves.
    pub(super) fn moved(&mut self, share: f64) -> Moves {
        self.travelled += share;
        if self.fingers >= 2 {
            Moves::Scroll
        } else {
            Moves::Pointer
        }
    }

    /// The pad itself was pressed down: which button that is.
    pub(super) fn pressed(&mut self) -> usize {
        self.pressed = true;
        if self.fingers >= 2 { 1 } else { 0 }
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

    /// A touch with `fingers` down from `from` to `to` milliseconds, moving
    /// `travel` of the pad: what it clicks.
    fn touch(fingers: u8, from: u64, to: u64, travel: f64, typing: bool) -> Option<usize> {
        let mut touch = Touch::default();
        touch.fingers(1, true);
        touch.began(at(from));
        if fingers > 1 {
            touch.fingers(1, false);
            touch.fingers(fingers, true);
        }
        touch.moved(travel);
        touch.fingers(fingers, false);
        touch.ended(at(to), typing)
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
    fn one_finger_moves_the_pointer_and_two_scroll() {
        let mut touch = Touch::default();
        touch.fingers(1, true);
        touch.began(at(0));
        assert_eq!(touch.moved(0.01), Moves::Pointer);

        // A second finger comes down: the kernel takes one count away and
        // gives the other, and the number having changed is said once.
        assert!(touch.fingers(1, false));
        assert!(touch.fingers(2, true));
        assert_eq!(touch.moved(0.01), Moves::Scroll);
        assert!(!touch.fingers(2, true));

        // And leaves again.
        touch.fingers(2, false);
        touch.fingers(1, true);
        assert_eq!(touch.moved(0.01), Moves::Pointer);
    }

    #[test]
    fn pressing_the_pad_is_the_secondary_button_with_two_fingers_on_it() {
        let mut touch = Touch::default();
        touch.fingers(1, true);
        touch.began(at(0));
        assert_eq!(touch.pressed(), 0);

        touch.fingers(1, false);
        touch.fingers(2, true);
        assert_eq!(touch.pressed(), 1);
    }

    #[test]
    fn a_press_of_the_pad_is_not_also_a_tap() {
        let mut touch = Touch::default();
        touch.fingers(1, true);
        touch.began(at(0));
        touch.pressed();
        touch.fingers(1, false);
        assert_eq!(touch.ended(at(80), false), None);
    }

    #[test]
    fn a_touch_starts_clean_after_the_one_before() {
        // Two fingers, then one: the second tap is the primary button's.
        let mut touch = Touch::default();
        touch.fingers(2, true);
        touch.began(at(0));
        touch.fingers(2, false);
        assert_eq!(touch.ended(at(50), false), Some(1));

        touch.fingers(1, true);
        touch.began(at(500));
        touch.fingers(1, false);
        assert_eq!(touch.ended(at(560), false), Some(0));
        // Lifting with no touch begun says nothing.
        assert_eq!(touch.ended(at(600), false), None);
    }
}
