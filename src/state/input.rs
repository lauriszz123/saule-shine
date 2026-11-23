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
