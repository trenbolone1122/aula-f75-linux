#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Hidden,
    Opening,
    Visible,
    Closing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowAction {
    None,
    Focus,
    Close,
}

/// Native window lifecycle and the latest requested visibility are tracked separately.
pub(crate) struct WindowState {
    phase: Phase,
    want_visible: bool,
    focus: bool,
    quitting: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            phase: Phase::Opening,
            want_visible: true,
            focus: false,
            quitting: false,
        }
    }
}

impl WindowState {
    pub(crate) fn is_visible(&self) -> bool {
        matches!(self.phase, Phase::Visible | Phase::Closing)
    }

    pub(crate) fn set_visible(&mut self, visible: bool) {
        if !self.quitting {
            self.want_visible = visible;
            self.focus = visible && self.phase == Phase::Visible;
        }
    }

    pub(crate) fn toggle(&mut self) {
        self.set_visible(!self.want_visible);
    }

    pub(crate) fn quit(&mut self) {
        self.set_visible(false);
        self.quitting = true;
    }

    pub(crate) fn is_quitting(&self) -> bool {
        self.quitting
    }

    pub(crate) fn begin_open(&mut self) -> bool {
        if self.phase == Phase::Hidden && self.want_visible && !self.quitting {
            self.phase = Phase::Opening;
            true
        } else {
            false
        }
    }

    pub(crate) fn frame(&mut self, close_requested: bool) -> WindowAction {
        if self.phase == Phase::Closing {
            // egui echoes our Close as a viewport event. Don't discard a later Open.
            return WindowAction::None;
        }
        if close_requested {
            self.set_visible(false);
            self.phase = Phase::Closing;
            return WindowAction::None;
        }
        if !self.want_visible {
            self.phase = Phase::Closing;
            return WindowAction::Close;
        }
        self.phase = Phase::Visible;
        if std::mem::take(&mut self.focus) {
            WindowAction::Focus
        } else {
            WindowAction::None
        }
    }

    pub(crate) fn closed(&mut self) {
        // A native exit that didn't pass through the normal close frame also hides.
        if self.phase != Phase::Closing {
            self.set_visible(false);
        }
        self.phase = Phase::Hidden;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible_window() -> WindowState {
        let mut state = WindowState::default();
        assert_eq!(state.frame(false), WindowAction::None);
        assert!(state.is_visible());
        state
    }

    #[test]
    fn repeated_toggles_open_and_hide_the_window() {
        let mut state = visible_window();
        for _ in 0..3 {
            state.toggle();
            assert_eq!(state.frame(false), WindowAction::Close);
            assert_eq!(state.frame(true), WindowAction::None);
            state.closed();
            assert!(!state.is_visible());
            assert!(!state.begin_open());
            state.toggle();
            assert!(state.begin_open());
            assert!(!state.begin_open());
            assert_eq!(state.frame(false), WindowAction::None);
            assert!(state.is_visible());
        }
    }

    #[test]
    fn explicit_open_on_a_visible_window_does_not_schedule_reopening() {
        let mut state = visible_window();
        state.set_visible(true);
        assert_eq!(state.frame(false), WindowAction::Focus);
        state.set_visible(false);
        assert_eq!(state.frame(false), WindowAction::Close);
        state.closed();
        assert!(!state.begin_open());

        let mut state = visible_window();
        state.set_visible(true);
        assert_eq!(state.frame(true), WindowAction::None);
        state.closed();
        assert!(!state.begin_open());
    }

    #[test]
    fn explicit_open_during_close_reopens_exactly_once() {
        let mut state = visible_window();
        state.set_visible(false);
        assert_eq!(state.frame(false), WindowAction::Close);
        state.set_visible(true);
        assert_eq!(state.frame(true), WindowAction::None);
        state.closed();
        assert!(state.begin_open());
        assert!(!state.begin_open());
        assert_eq!(state.frame(false), WindowAction::None);
        assert!(state.is_visible());
    }

    #[test]
    fn rapid_toggles_follow_the_last_requested_visibility() {
        let mut state = visible_window();
        state.toggle();
        state.toggle();
        assert_ne!(state.frame(false), WindowAction::Close);
        state.toggle();
        assert_eq!(state.frame(false), WindowAction::Close);
        state.toggle();
        state.toggle();
        state.closed();
        assert!(!state.begin_open());
    }

    #[test]
    fn quit_overrides_pending_open_in_both_states() {
        let mut state = visible_window();
        state.quit();
        state.set_visible(true);
        assert_eq!(state.frame(false), WindowAction::Close);
        state.closed();
        assert!(state.is_quitting());
        assert!(!state.begin_open());

        let mut state = visible_window();
        state.frame(true);
        state.closed();
        state.set_visible(true);
        state.quit();
        state.toggle();
        assert!(state.is_quitting());
        assert!(!state.begin_open());
    }
}
