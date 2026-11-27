//! Mouse state and the wheel / button bookkeeping the script-facing
//! queries read.

/// Per-frame mouse edges and wheel movement, latched by
/// [`Engine::poll_events`] the same way [`KeyState`] latches key edges.
///
/// Backends report the wheel as a delta that is only valid for the pump
/// that produced it, so it has to be captured at the frame boundary or it is
/// lost. Buttons are sampled the same way, and both feed the `MousePressed` /
/// `MouseReleased` / `WheelMoved` events the frame's queue is built from.
#[derive(Default)]
pub struct MouseState {
    /// Left, right, middle — indices 0, 1, 2 (button numbers 1, 2, 3).
    down: [bool; 3],
    pressed: [bool; 3],
    released: [bool; 3],
    wheel: (f64, f64),
    /// Edges and wheel movement seen during a `present`. Same reason as
    /// [`crate::keyboard::KeyState`]: the backend is pumped twice a frame
    /// and each pump clears its own scroll delta, so half of them would be
    /// thrown away before Saule ever saw them.
    carried_pressed: [bool; 3],
    carried_released: [bool; 3],
    carried_wheel: (f64, f64),
}

impl MouseState {
    /// Latch this frame's buttons and wheel, given the backend's view right
    /// after `pollEvents` pumped it. Called once per `Window.pollEvents`.
    pub(crate) fn sync(&mut self, buttons: [bool; 3], wheel: (f64, f64)) {
        let (buttons, wheel) = (crate::pointer::buttons(buttons), crate::pointer::wheel(wheel));
        for (i, &now) in buttons.iter().enumerate() {
            self.pressed[i] = (now && !self.down[i]) || self.carried_pressed[i];
            self.released[i] = (!now && self.down[i]) || self.carried_released[i];
            self.down[i] = now;
        }

        self.wheel = (wheel.0 + self.carried_wheel.0, wheel.1 + self.carried_wheel.1);

        self.carried_pressed = [false; 3];
        self.carried_released = [false; 3];
        self.carried_wheel = (0.0, 0.0);
    }

    /// Sample the mouse after presenting, keeping the edges and wheel movement
    /// for the next [`MouseState::sync`].
    pub(crate) fn observe(&mut self, buttons: [bool; 3], wheel: (f64, f64)) {
        let (buttons, wheel) = (crate::pointer::buttons(buttons), crate::pointer::wheel(wheel));
        for (i, &now) in buttons.iter().enumerate() {
            if now && !self.down[i] {
                self.carried_pressed[i] = true;
            }

            if !now && self.down[i] {
                self.carried_released[i] = true;
            }

            self.down[i] = now;
        }

        self.carried_wheel = (self.carried_wheel.0 + wheel.0, self.carried_wheel.1 + wheel.1);
    }

    /// `1` = left, `2` = right, `3` = middle; anything else is not a button.
    pub(crate) fn slot(button: i64) -> Option<usize> {
        match button {
            1 => Some(0),
            2 => Some(1),
            3 => Some(2),
            _ => None,
        }
    }

    pub fn is_down(&self, button: i64) -> bool {
        Self::slot(button).is_some_and(|i| self.down[i])
    }

    pub fn was_pressed(&self, button: i64) -> bool {
        Self::slot(button).is_some_and(|i| self.pressed[i])
    }

    pub fn was_released(&self, button: i64) -> bool {
        Self::slot(button).is_some_and(|i| self.released[i])
    }

    /// Wheel movement since the last `pollEvents`, as `(x, y)`. Positive `y` is
    /// a scroll up / away from the user.
    pub fn wheel(&self) -> (f64, f64) {
        self.wheel
    }
}

/// The mouse's buttons as a backend shows them to the frame loop.
///
/// The loop does not hear presses: it looks at whether each button is
/// down, once after every pump, and takes a change from the last look
/// for a press or a release. A click is over in a twentieth of a second,
/// and a frame can take longer: both halves then arrive in one pump, the
/// button is up at the next look as it was at the last, and the click
/// never happened. On a slow machine that was any click made while a
/// window was busy.
///
/// So a button changes at most once between two looks. Further changes
/// wait their turn, one a look: a press and release that arrive together
/// are down at this look and up at the next.
#[derive(Debug, Default)]
pub(crate) struct Buttons {
    shown: [bool; 3],
    /// Whether each has changed since the last look.
    changed: [bool; 3],
    /// The levels still to be shown, oldest first.
    waiting: [std::collections::VecDeque<bool>; 3],
}

impl Buttons {
    /// The button is now `down`.
    pub(crate) fn set(&mut self, button: usize, down: bool) {
        let Some(waiting) = self.waiting.get_mut(button) else {
            return;
        };
        let latest = waiting.back().copied().unwrap_or(self.shown[button]);
        if latest == down {
            return;
        }
        if self.changed[button] || !waiting.is_empty() {
            waiting.push_back(down);
        } else {
            self.shown[button] = down;
            self.changed[button] = true;
        }
    }

    /// A look has been taken: what was waiting may show, one change a
    /// button. Called as a pump begins, before it reads anything new.
    pub(crate) fn next(&mut self) {
        for button in 0..3 {
            self.changed[button] = false;
            if let Some(down) = self.waiting[button].pop_front() {
                self.shown[button] = down;
                self.changed[button] = true;
            }
        }
    }

    /// What the loop sees at this look.
    pub(crate) fn shown(&self) -> [bool; 3] {
        self.shown
    }
}

#[cfg(test)]
mod button_tests {
    use super::Buttons;

    #[test]
    fn a_click_inside_one_pump_is_a_press_and_then_a_release() {
        let mut buttons = Buttons::default();
        buttons.next();
        buttons.set(0, true);
        buttons.set(0, false);
        assert_eq!(buttons.shown(), [true, false, false]);
        buttons.next();
        assert_eq!(buttons.shown(), [false, false, false]);
        buttons.next();
        assert_eq!(buttons.shown(), [false, false, false]);
    }

    #[test]
    fn two_clicks_inside_one_pump_are_two_clicks() {
        let mut buttons = Buttons::default();
        buttons.next();
        for down in [true, false, true, false] {
            buttons.set(0, down);
        }
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(buttons.shown()[0]);
            buttons.next();
        }
        assert_eq!(seen, [true, false, true, false, false]);
    }

    #[test]
    fn a_button_held_stays_down_and_others_are_their_own() {
        let mut buttons = Buttons::default();
        buttons.next();
        buttons.set(1, true);
        buttons.set(1, true);
        buttons.next();
        buttons.set(0, true);
        assert_eq!(buttons.shown(), [true, true, false]);
        buttons.next();
        buttons.set(1, false);
        assert_eq!(buttons.shown(), [true, false, false]);
    }

    #[test]
    fn a_release_and_press_inside_one_pump_are_both_seen() {
        let mut buttons = Buttons::default();
        buttons.next();
        buttons.set(0, true);
        buttons.next();
        buttons.set(0, false);
        buttons.set(0, true);
        assert_eq!(buttons.shown(), [false, false, false]);
        buttons.next();
        assert_eq!(buttons.shown(), [true, false, false]);
    }
}
