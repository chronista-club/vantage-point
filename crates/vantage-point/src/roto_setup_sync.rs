//! Bounded, nonblocking setup RPCs alongside the MIDI lease loop.
use crate::{
    roto_setup::Generated,
    roto_setup_files::{Hashes, Pins},
    roto_setup_state::{Action, Observation, State},
};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Instant};

#[derive(Clone, Debug)]
pub(super) struct Status {
    pub device: String,
    pub lease: Option<String>,
    pub sequence: u64,
    pub configuring: bool,
    pub current_setup: Option<u8>,
    pub error: Option<String>,
}

#[async_trait::async_trait]
pub(super) trait Transport: Send + std::marker::Sync + 'static {
    async fn register(&self, device: &str) -> anyhow::Result<Status>;
    async fn apply(&self, device: &str, lease: &str, content: Generated) -> anyhow::Result<Status>;
}

pub(super) struct Sync<T: Transport> {
    transport: Arc<T>,
    pins: Pins,
    hashes: Hashes,
    devices: BTreeMap<String, DeviceSync>,
    start: Instant,
}

struct DeviceSync {
    lease: String,
    state: State,
    task: Option<tokio::task::JoinHandle<(Action, anyhow::Result<Status>)>>,
    after_sequence: Option<u64>,
    busy_warned: bool,
}

impl Drop for DeviceSync {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl<T: Transport> Sync<T> {
    pub fn new(transport: Arc<T>, dir: &Path) -> Self {
        Self {
            transport,
            pins: Pins::new(dir),
            hashes: Hashes::load(dir),
            devices: BTreeMap::new(),
            start: Instant::now(),
        }
    }
    pub async fn tick(&mut self, statuses: &[Status]) {
        // Revocation is processed even when pin input is unavailable. Dropping an entry
        // cancels its request; the server remains responsible for validating the token.
        self.devices.retain(|id, entry| {
            statuses
                .iter()
                .any(|s| s.device == *id && s.lease.as_deref() == Some(entry.lease.as_str()))
        });
        let Some(generated) = self.pins.refresh() else {
            return;
        };
        for status in statuses {
            let Some(lease) = status.lease.as_ref() else {
                continue;
            };
            let entry = self
                .devices
                .entry(status.device.clone())
                .or_insert_with(|| DeviceSync {
                    lease: lease.clone(),
                    state: State::new(self.hashes.get(&status.device)),
                    task: None,
                    after_sequence: None,
                    busy_warned: false,
                });
            if entry.task.as_ref().is_some_and(|t| t.is_finished()) {
                let response = entry.task.take().unwrap().await;
                match response {
                    Ok((action, Ok(reply)))
                        if reply.device == status.device
                            && reply.lease.as_ref() == Some(lease)
                            && (!matches!(action, Action::Apply(_)) || reply.configuring) =>
                    {
                        entry.after_sequence = Some(reply.sequence);
                        match action {
                            Action::Register => entry.state.registered(),
                            Action::Apply(_) => entry.state.accepted(reply.configuring),
                            _ => unreachable!("only RPC actions are spawned"),
                        }
                    }
                    other => {
                        entry.state.failed(self.start.elapsed());
                        tracing::warn!(device = %status.device, result = ?other, "ROTO setup request failed; registration retries with backoff, Apply waits for next lease or pin edit");
                    }
                }
            }
            if entry
                .after_sequence
                .is_some_and(|seq| status.sequence <= seq)
            {
                continue;
            }
            if status.configuring && !entry.busy_warned {
                tracing::debug!(device = %status.device, current_setup = ?status.current_setup, "ROTO setup: configuring; deferring until next idle snapshot");
            }
            entry.busy_warned = status.configuring;
            let action = entry.state.observe(Observation {
                lease: Some(lease),
                hash: &generated.hash,
                configuring: status.configuring,
                error: status.error.is_some(),
                now: self.start.elapsed(),
            });
            match action {
                Some(Action::Confirm(hash)) => {
                    if let Err(error) = self.hashes.confirm(&status.device, &hash) {
                        tracing::warn!(%error, "ROTO setup: cannot save confirmed hash");
                    }
                    tracing::info!(device = %status.device, %hash, "ROTO VP setup completed");
                }
                Some(Action::Failed) => {
                    tracing::warn!(device = %status.device, error = ?status.error, "ROTO setup failed; waiting for next lease or pin edit");
                }
                Some(action @ (Action::Register | Action::Apply(_))) => {
                    if matches!(action, Action::Apply(_))
                        && let Err(error) = self.hashes.invalidate(&status.device)
                    {
                        entry.state.failed(self.start.elapsed());
                        tracing::warn!(%error, "ROTO setup: cannot invalidate old hash; skipping write");
                        continue;
                    }
                    let transport = self.transport.clone();
                    let device = status.device.clone();
                    let lease = lease.clone();
                    let content = generated.clone();
                    entry.task = Some(tokio::spawn(async move {
                        let operation = async {
                            match &action {
                                Action::Register => transport.register(&device).await,
                                Action::Apply(_) => transport.apply(&device, &lease, content).await,
                                _ => unreachable!(),
                            }
                        };
                        let result = match tokio::time::timeout(
                            std::time::Duration::from_secs(4),
                            operation,
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(error) => Err(error.into()),
                        };
                        (action, result)
                    }));
                }
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<&'static str>>,
        stalled: AtomicBool,
        fail: AtomicBool,
        omit_receipt: AtomicBool,
    }
    fn status(sequence: u64, configuring: bool) -> Status {
        Status {
            device: "roto".into(),
            lease: Some("lease".into()),
            sequence,
            configuring,
            current_setup: Some(6),
            error: None,
        }
    }
    #[async_trait::async_trait]
    impl Transport for Fake {
        async fn register(&self, _device: &str) -> anyhow::Result<Status> {
            self.calls.lock().unwrap().push("register");
            if self.stalled.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(status(2, false))
        }
        async fn apply(
            &self,
            _device: &str,
            _lease: &str,
            _content: Generated,
        ) -> anyhow::Result<Status> {
            self.calls.lock().unwrap().push("apply");
            anyhow::ensure!(!self.fail.load(Ordering::SeqCst), "not_implemented");
            Ok(status(4, !self.omit_receipt.load(Ordering::SeqCst)))
        }
    }

    async fn step(sync: &mut Sync<Fake>, state: Status) {
        sync.tick(&[state]).await;
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn rpc_order_and_sequence_barrier_prevent_acceptance_from_becoming_success() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        let mut sync = Sync::new(fake.clone(), dir.path());
        step(&mut sync, status(1, false)).await; // register
        step(&mut sync, status(3, false)).await; // register completed; apply
        step(&mut sync, status(3, false)).await; // stale snapshot predates apply acceptance
        assert_eq!(*fake.calls.lock().unwrap(), vec!["register", "apply"]);
        assert!(!dir.path().join("roto-setup-hashes.json").exists());
        step(&mut sync, status(5, false)).await;
        assert!(dir.path().join("roto-setup-hashes.json").exists());
        drop(sync);
        let mut restarted = Sync::new(fake.clone(), dir.path());
        step(&mut restarted, status(6, false)).await;
        step(&mut restarted, status(7, false)).await;
        assert_eq!(
            *fake.calls.lock().unwrap(),
            vec!["register", "apply", "register"]
        );
    }

    #[tokio::test]
    async fn stalled_rpc_does_not_block_revocation_or_other_work() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        fake.stalled.store(true, Ordering::SeqCst);
        let mut sync = Sync::new(fake.clone(), dir.path());
        step(&mut sync, status(1, false)).await;
        assert_eq!(*fake.calls.lock().unwrap(), vec!["register"]);
        assert!(sync.devices["roto"].task.is_some());
        let mut released = status(2, false);
        released.lease = None;
        step(&mut sync, released).await;
        assert!(sync.devices.is_empty());
        assert!(!dir.path().join("roto-setup-hashes.json").exists());
    }

    #[tokio::test]
    async fn unsupported_apply_is_attempted_once_and_never_cached() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        fake.fail.store(true, Ordering::SeqCst);
        let mut sync = Sync::new(fake.clone(), dir.path());
        for seq in 1..20 {
            step(&mut sync, status(seq, false)).await;
        }
        assert_eq!(*fake.calls.lock().unwrap(), vec!["register", "apply"]);
        assert!(!dir.path().join("roto-setup-hashes.json").exists());
    }

    #[tokio::test]
    async fn successful_apply_is_cached_even_when_the_selected_setup_is_different_or_unknown() {
        for selected in [Some(2), None] {
            let dir = tempfile::tempdir().unwrap();
            let fake = Arc::new(Fake::default());
            let mut sync = Sync::new(fake.clone(), dir.path());
            for sequence in [1, 3, 5] {
                let mut snapshot = status(sequence, false);
                snapshot.current_setup = selected;
                step(&mut sync, snapshot).await;
            }
            assert_eq!(*fake.calls.lock().unwrap(), ["register", "apply"]);
            assert!(
                Hashes::load(dir.path()).get("roto").is_some(),
                "Apply completion with selected={selected:?}"
            );
        }
    }

    #[tokio::test]
    async fn registration_error_or_join_failure_retries_before_apply_without_pin_edits() {
        struct RetryFake {
            calls: Mutex<Vec<&'static str>>,
            panic_first: bool,
        }
        #[async_trait::async_trait]
        impl Transport for RetryFake {
            async fn register(&self, _: &str) -> anyhow::Result<Status> {
                let first = {
                    let mut calls = self.calls.lock().unwrap();
                    calls.push("register");
                    calls.len() == 1
                };
                if first {
                    assert!(!self.panic_first, "simulated registration task panic");
                    anyhow::bail!("temporary registration failure");
                }
                Ok(status(2, false))
            }
            async fn apply(&self, _: &str, _: &str, _: Generated) -> anyhow::Result<Status> {
                self.calls.lock().unwrap().push("apply");
                Ok(status(4, true))
            }
        }
        for panic_first in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let fake = Arc::new(RetryFake {
                calls: Mutex::default(),
                panic_first,
            });
            let mut sync = Sync::new(fake.clone(), dir.path());
            for sequence in 1..=3 {
                sync.tick(&[status(sequence, false)]).await;
                tokio::task::yield_now().await;
            }
            assert_eq!(*fake.calls.lock().unwrap(), ["register"]);
            sync.start -= std::time::Duration::from_secs(2);
            for sequence in 4..=6 {
                sync.tick(&[status(sequence, false)]).await;
                tokio::task::yield_now().await;
            }
            assert_eq!(
                *fake.calls.lock().unwrap(),
                ["register", "register", "apply"]
            );
            assert!(Hashes::load(dir.path()).get("roto").is_some());
        }
    }

    #[tokio::test]
    async fn write_invalidates_old_cache_before_rpc_and_revocation_cannot_confirm_it() {
        let dir = tempfile::tempdir().unwrap();
        Hashes::load(dir.path())
            .confirm("roto", "old-content")
            .unwrap();
        let fake = Arc::new(Fake::default());
        let mut sync = Sync::new(fake.clone(), dir.path());
        step(&mut sync, status(1, false)).await;
        step(&mut sync, status(3, false)).await;
        assert_eq!(Hashes::load(dir.path()).get("roto"), None);
        let mut revoked = status(5, false);
        revoked.lease = None;
        step(&mut sync, revoked).await;
        assert_eq!(Hashes::load(dir.path()).get("roto"), None);
        assert_eq!(*fake.calls.lock().unwrap(), vec!["register", "apply"]);
    }

    #[tokio::test]
    async fn missing_busy_receipt_cannot_be_confirmed_by_an_unrelated_later_operation() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(Fake::default());
        fake.omit_receipt.store(true, Ordering::SeqCst);
        let mut sync = Sync::new(fake.clone(), dir.path());
        step(&mut sync, status(1, false)).await;
        step(&mut sync, status(3, false)).await;
        step(&mut sync, status(5, true)).await;
        step(&mut sync, status(6, false)).await;
        assert_eq!(Hashes::load(dir.path()).get("roto"), None);
        assert_eq!(*fake.calls.lock().unwrap(), vec!["register", "apply"]);
    }
}
