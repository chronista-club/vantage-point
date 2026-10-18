//! Pure per-device setup reconciliation. No device or filesystem I/O.
use std::time::Duration;

pub(super) const DEBOUNCE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Action {
    Register,
    Apply(String),
    Confirm(String),
    Failed,
}

pub(super) struct Observation<'a> {
    pub lease: Option<&'a str>,
    pub hash: &'a str,
    pub configuring: bool,
    pub current_setup: Option<u8>,
    pub error: bool,
    pub now: Duration,
}

#[derive(Default)]
pub(super) struct State {
    lease: Option<String>,
    desired: String,
    due: Duration,
    saved: Option<String>,
    registered: bool,
    pending: Option<Pending>,
    failed_hash: Option<String>,
}

enum Pending {
    Register,
    Request(String),
    Applying { hash: String, busy_seen: bool },
}

impl State {
    pub fn new(saved: Option<String>) -> Self {
        Self {
            saved,
            ..Self::default()
        }
    }

    pub fn observe(&mut self, o: Observation<'_>) -> Option<Action> {
        if self.desired != o.hash {
            self.desired = o.hash.to_owned();
            self.due = o.now + DEBOUNCE;
            self.failed_hash = None;
        }
        if self.lease.as_deref() != o.lease {
            self.lease = o.lease.map(str::to_owned);
            self.registered = false;
            self.pending = None;
            self.failed_hash = None;
            self.due = o.now;
        }
        o.lease?;
        if let Some(Pending::Applying { hash, busy_seen }) = &mut self.pending {
            *busy_seen |= o.configuring;
            if !o.configuring && *busy_seen {
                let hash = hash.clone();
                self.pending = None;
                if o.error || o.current_setup != Some(6) {
                    self.failed_hash = Some(hash);
                    return Some(Action::Failed);
                }
                self.saved = Some(hash.clone());
                return Some(Action::Confirm(hash));
            }
        }
        if o.configuring || self.pending.is_some() || self.failed_hash.as_deref() == Some(o.hash) {
            return None;
        }
        if !self.registered {
            self.pending = Some(Pending::Register);
            return Some(Action::Register);
        }
        if self.saved.as_deref() != Some(o.hash) && o.now >= self.due {
            self.saved = None;
            self.pending = Some(Pending::Request(o.hash.to_owned()));
            return Some(Action::Apply(o.hash.to_owned()));
        }
        None
    }

    pub fn registered(&mut self) {
        if matches!(self.pending, Some(Pending::Register)) {
            self.registered = true;
            self.pending = None;
        }
    }

    pub fn accepted(&mut self, configuring: bool) {
        if let Some(Pending::Request(hash)) = self.pending.take() {
            self.pending = Some(Pending::Applying {
                hash,
                busy_seen: configuring,
            });
        }
    }

    pub fn failed(&mut self) {
        self.failed_hash = Some(match self.pending.take() {
            Some(Pending::Request(hash) | Pending::Applying { hash, .. }) => hash,
            _ => self.desired.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    // Task: mem_1Cg9QiQeVsk7KD3fcwhXqh
    use super::*;
    fn obs<'a>(lease: Option<&'a str>, hash: &'a str, ms: u64) -> Observation<'a> {
        Observation {
            lease,
            hash,
            configuring: false,
            current_setup: Some(6),
            error: false,
            now: Duration::from_millis(ms),
        }
    }

    #[test]
    fn acquisition_registers_before_applying_and_only_completion_confirms() {
        let mut s = State::default();
        assert_eq!(s.observe(obs(None, "a", 0)), None);
        assert_eq!(
            s.observe(obs(Some("lease"), "a", 0)),
            Some(Action::Register)
        );
        assert_eq!(s.observe(obs(Some("lease"), "a", 0)), None);
        s.registered();
        assert_eq!(
            s.observe(obs(Some("lease"), "a", 0)),
            Some(Action::Apply("a".into()))
        );
        s.accepted(true);
        let mut busy = obs(Some("lease"), "a", 10);
        busy.configuring = true;
        assert_eq!(s.observe(busy), None);
        assert_eq!(
            s.observe(obs(Some("lease"), "a", 20)),
            Some(Action::Confirm("a".into()))
        );
        assert_eq!(s.observe(obs(Some("lease"), "a", 30)), None);
    }

    #[test]
    fn saved_hash_still_registers_but_does_not_write() {
        let mut s = State::new(Some("a".into()));
        assert_eq!(
            s.observe(obs(Some("lease"), "a", 0)),
            Some(Action::Register)
        );
        s.registered();
        assert_eq!(s.observe(obs(Some("lease"), "a", 0)), None);
    }

    #[test]
    fn editing_away_and_back_allows_retry_after_an_intervening_success() {
        let mut s = State::default();
        s.observe(obs(Some("lease"), "a", 0));
        s.registered();
        s.observe(obs(Some("lease"), "a", 0));
        s.failed();
        s.observe(obs(Some("lease"), "b", 100));
        assert_eq!(
            s.observe(obs(Some("lease"), "b", 2100)),
            Some(Action::Apply("b".into()))
        );
        s.accepted(true);
        assert_eq!(
            s.observe(obs(Some("lease"), "b", 2200)),
            Some(Action::Confirm("b".into()))
        );
        assert_eq!(s.observe(obs(Some("lease"), "a", 2300)), None);
        assert_eq!(
            s.observe(obs(Some("lease"), "a", 4300)),
            Some(Action::Apply("a".into()))
        );
    }

    #[test]
    fn edits_debounce_to_latest_and_wait_for_the_next_owned_lease() {
        let mut s = State::new(Some("a".into()));
        s.observe(obs(Some("lease"), "a", 0));
        s.registered();
        assert_eq!(s.observe(obs(Some("lease"), "b", 100)), None);
        assert_eq!(s.observe(obs(Some("lease"), "c", 1100)), None);
        assert_eq!(s.observe(obs(Some("lease"), "c", 3099)), None);
        assert_eq!(
            s.observe(obs(Some("lease"), "c", 3100)),
            Some(Action::Apply("c".into()))
        );
        assert_eq!(s.observe(obs(None, "d", 3200)), None);
        s.accepted(true); // Late reply after revocation must do nothing.
        assert_eq!(s.observe(obs(None, "d", 6000)), None);
        assert_eq!(
            s.observe(obs(Some("next"), "d", 6100)),
            Some(Action::Register)
        );
        s.registered();
        assert_eq!(
            s.observe(obs(Some("next"), "d", 6100)),
            Some(Action::Apply("d".into()))
        );
    }

    #[test]
    fn failures_never_loop_for_the_same_lease_and_content() {
        let mut s = State::default();
        s.observe(obs(Some("lease"), "a", 0));
        s.failed();
        for ms in [200, 2000, 20000] {
            assert_eq!(s.observe(obs(Some("lease"), "a", ms)), None);
        }
        assert_eq!(
            s.observe(obs(Some("lease"), "b", 21000)),
            Some(Action::Register)
        );
        s.registered();
        assert_eq!(
            s.observe(obs(Some("lease"), "b", 23000)),
            Some(Action::Apply("b".into()))
        );
        s.accepted(true);
        let mut error = obs(Some("lease"), "b", 24000);
        error.error = true;
        assert_eq!(s.observe(error), Some(Action::Failed));
        assert_eq!(s.observe(obs(Some("lease"), "b", 25000)), None);
    }

    #[test]
    fn busy_service_defers_and_unobserved_acceptance_never_marks_saved() {
        let mut s = State::default();
        let mut busy = obs(Some("lease"), "a", 0);
        busy.configuring = true;
        assert_eq!(s.observe(busy), None);
        assert_eq!(
            s.observe(obs(Some("lease"), "a", 200)),
            Some(Action::Register)
        );
        s.registered();
        s.observe(obs(Some("lease"), "a", 200));
        s.accepted(false);
        assert_eq!(s.observe(obs(Some("lease"), "a", 400)), None);
        let mut busy = obs(Some("lease"), "a", 500);
        busy.configuring = true;
        assert_eq!(s.observe(busy), None);
        assert_eq!(
            s.observe(obs(Some("lease"), "a", 600)),
            Some(Action::Confirm("a".into()))
        );
    }

    #[test]
    fn failed_write_invalidates_the_previous_success_even_if_user_reverts_pins() {
        let mut s = State::new(Some("a".into()));
        s.observe(obs(Some("lease"), "a", 0));
        s.registered();
        s.observe(obs(Some("lease"), "b", 100));
        assert_eq!(
            s.observe(obs(Some("lease"), "b", 2100)),
            Some(Action::Apply("b".into()))
        );
        s.failed();
        s.observe(obs(Some("lease"), "a", 2200));
        assert_eq!(
            s.observe(obs(Some("lease"), "a", 4200)),
            Some(Action::Apply("a".into()))
        );
    }
}
