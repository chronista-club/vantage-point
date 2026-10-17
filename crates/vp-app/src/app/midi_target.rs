/// Application-wide MIDI target. Losing OS focus does not change the target.
pub(super) struct MidiTarget<T>(Option<T>);

impl<T: Copy + Eq> MidiTarget<T> {
    pub(super) fn new() -> Self {
        Self(None)
    }
    pub(super) fn focus(&mut self, id: T) {
        self.0 = Some(id);
    }
    pub(super) fn close(&mut self, id: T) {
        if self.0 == Some(id) {
            self.0 = None;
        }
    }
    pub(super) fn accepts(&self, id: T) -> bool {
        self.0 == Some(id)
    }
}

/// Restore on WebviewReady as well as focus changes (focus can precede bundle load).
pub(super) fn indicator_js(active: bool) -> String {
    format!(
        "document.documentElement.dataset.midiTarget='{active}';window.dispatchEvent(new CustomEvent('vp:midi-target',{{detail:{active}}}));"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_last_activated_window_receives_input_and_close_does_not_fall_back() {
        let mut target = MidiTarget::new();
        assert!(!target.accepts(1));
        target.focus(1);
        assert!(target.accepts(1));
        assert!(!target.accepts(2));
        // OS focus loss intentionally does not clear the remembered window.
        assert!(target.accepts(1));
        target.focus(2);
        assert!(!target.accepts(1));
        assert!(target.accepts(2));
        target.close(1);
        assert!(target.accepts(2));
        target.close(2);
        assert!(!target.accepts(1));
        assert!(!target.accepts(2));
        target.focus(1);
        assert!(target.accepts(1));
    }
}
