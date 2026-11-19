#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    Start(String),
    Stop,
    Busy,
}
#[derive(Debug)]
enum State {
    Idle,
    Recording {
        key: String,
        start: u64,
        toggle: bool,
    },
    Busy,
}
pub struct Gestures {
    state: State,
    tap_ms: u64,
}
impl Gestures {
    pub fn new(tap_ms: u64) -> Self {
        Self {
            state: State::Idle,
            tap_ms,
        }
    }
    pub fn press(&mut self, key: &str, mode: &str, now: u64) -> Effect {
        match &self.state {
            State::Idle => {
                self.state = State::Recording {
                    key: key.into(),
                    start: now,
                    toggle: false,
                };
                Effect::Start(mode.into())
            }
            State::Recording { toggle: true, .. } => {
                self.state = State::Busy;
                Effect::Stop
            }
            State::Recording { .. } => Effect::None,
            State::Busy => Effect::Busy,
        }
    }
    pub fn release(&mut self, key: &str, now: u64) -> Effect {
        if let State::Recording {
            key: held,
            start,
            toggle,
        } = &mut self.state
            && key == held
            && !*toggle
        {
            if now.saturating_sub(*start) < self.tap_ms {
                *toggle = true;
            } else {
                self.state = State::Busy;
                return Effect::Stop;
            }
        }
        Effect::None
    }
    pub fn tick(&mut self, now: u64) -> Effect {
        if matches!(self.state,State::Recording{start,..} if now.saturating_sub(start)>=600_000) {
            self.state = State::Busy;
            Effect::Stop
        } else {
            Effect::None
        }
    }
    pub fn ready(&mut self) {
        self.state = State::Idle;
    }
    pub fn busy(&mut self) {
        self.state = State::Busy;
    }
    pub fn is_ready(&self) -> bool {
        matches!(self.state, State::Idle)
    }
    pub fn is_toggle(&self) -> bool {
        matches!(self.state, State::Recording { toggle: true, .. })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tap_and_other_shortcut_stop() {
        let mut g = Gestures::new(300);
        assert_eq!(g.press("a", "clean", 100), Effect::Start("clean".into()));
        assert_eq!(g.release("a", 399), Effect::None);
        assert_eq!(g.press("b", "raw", 500), Effect::Stop);
        assert_eq!(g.release("b", 510), Effect::None);
        assert_eq!(g.press("a", "clean", 600), Effect::Busy);
    }
    #[test]
    fn hold_boundary_and_repeats() {
        let mut g = Gestures::new(300);
        g.press("a", "raw", 0);
        assert_eq!(g.press("a", "raw", 200), Effect::None);
        assert_eq!(g.release("b", 400), Effect::None);
        assert_eq!(g.release("a", 300), Effect::Stop);
        g.ready();
        assert_eq!(g.press("a", "clean", 400), Effect::Start("clean".into()));
    }
    #[test]
    fn toggle_same_shortcut() {
        let mut g = Gestures::new(300);
        g.press("a", "raw", 0);
        g.release("a", 10);
        assert_eq!(g.press("a", "raw", 1000), Effect::Stop);
    }
    #[test]
    fn cap_hold_and_toggle() {
        for toggle in [false, true] {
            let mut g = Gestures::new(300);
            g.press("a", "raw", 500);
            if toggle {
                g.release("a", 501);
            }
            assert_eq!(g.tick(600499), Effect::None);
            assert_eq!(g.tick(600500), Effect::Stop);
            assert_eq!(g.press("b", "clean", 600501), Effect::Busy);
        }
    }
}
