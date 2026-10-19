//! Regression coverage for mem_1Cg8P1aoBWatWQaaVTMXo5.
use super::*;
use crate::{capability::eventbus::EventBus, midi_access::OwnedPorts};
use midistage_client::{Snapshot, protocol::wire::Reply};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;
use unison::network::{
    InternalMeshKeypair, ProtocolServer, TrustAnchors,
    channel::UnisonChannel,
    server::{ConnectionEvent, ConnectionEventReceiver, ServerHandle},
};

struct Service {
    server: ServerHandle,
    endpoint: Endpoint,
    events: ConnectionEventReceiver,
    hello_seen: Arc<Notify>,
    hello_reply: Arc<Notify>,
    snapshot_seen: Arc<Notify>,
    released_at_close: Arc<AtomicBool>,
}

impl Service {
    async fn start(registry: Weak<RwLock<DeviceRegistry>>, hold_hello: bool) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let pair = InternalMeshKeypair::generate(["localhost".into(), "::1".into()]).unwrap();
        let TrustAnchors::Custom(certs) = pair.client_trust_anchors else {
            unreachable!()
        };
        let server = Arc::new(ProtocolServer::with_identity("midi-test", "1.0.0", "test"));
        let events = server.subscribe_connection_events();
        let hello_seen = Arc::new(Notify::new());
        let hello_reply = Arc::new(Notify::new());
        let snapshot_seen = Arc::new(Notify::new());
        let released_at_close = Arc::new(AtomicBool::new(false));
        let (seen, reply, snapshot, released) = (
            hello_seen.clone(),
            hello_reply.clone(),
            snapshot_seen.clone(),
            released_at_close.clone(),
        );
        server
            .register_channel("midistage", move |_ctx, stream| {
                let (seen, reply, snapshot, released, registry) = (
                    seen.clone(),
                    reply.clone(),
                    snapshot.clone(),
                    released.clone(),
                    registry.clone(),
                );
                async move {
                    let channel: UnisonChannel = UnisonChannel::new(stream);
                    while let Ok(message) = channel.recv().await {
                        if message.method == "Snapshot" {
                            // Keep the RPC unanswered: shutdown must interrupt this wait.
                            snapshot.notify_one();
                            continue;
                        }
                        seen.notify_one();
                        if hold_hello {
                            reply.notified().await;
                        }
                        let response = Reply {
                            snapshot: Some(Snapshot {
                                sequence: 1,
                                protocol_version: PROTOCOL_VERSION,
                                server_epoch: "test".into(),
                                session_id: "test-session".into(),
                                devices: vec![],
                                service: Default::default(),
                            }),
                            error: None,
                        };
                        if channel
                            .send_response(message.id, &message.method, &response)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    if let Some(registry) = registry.upgrade() {
                        let state = registry.read().await;
                        released.store(
                            !state.midi_enabled()
                                && state.midi_client.is_none()
                                && state.midi_access.read().unwrap().owned.is_empty(),
                            Ordering::SeqCst,
                        );
                    }
                    Ok(())
                }
            })
            .await;
        let handle = server
            .listener("[::1]:0")
            .cert(pair.server_cert_source)
            .spawn()
            .await
            .unwrap();
        let endpoint = Endpoint {
            protocol_version: PROTOCOL_VERSION,
            port: handle.local_addr().port(),
            server_epoch: "test".into(),
            certificate_der: certs[0].as_ref().to_vec(),
            auth_token: String::new(),
        };
        Self {
            server: handle,
            endpoint,
            events,
            hello_seen,
            hello_reply,
            snapshot_seen,
            released_at_close,
        }
    }

    fn endpoint_file(&self) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        serde_json::to_writer(file.as_file(), &self.endpoint).unwrap();
        file
    }

    async fn assert_disconnected(mut self) {
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(
                    self.events.recv_skip_lagged().await.unwrap(),
                    ConnectionEvent::Disconnected { .. }
                ) {
                    break;
                }
            }
            // Stream handler cleanup and connection events are independent tasks.
            while !self.released_at_close.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        self.server.shutdown().await.unwrap();
        assert!(
            result.is_ok(),
            "QUIC must disconnect after local release, before idle timeout"
        );
    }
}

fn registry() -> Arc<RwLock<DeviceRegistry>> {
    Arc::new(RwLock::new(DeviceRegistry::new(Arc::new(EventBus::new()))))
}

#[tokio::test]
async fn daemon_shutdown_closes_midistage_session() {
    let registry = registry();
    let service = Service::start(Arc::downgrade(&registry), false).await;
    let (client, _) = Client::connect(
        &service.endpoint,
        Hello {
            protocol_version: PROTOCOL_VERSION,
            client_id: "vp".into(),
            display_name: "test".into(),
            auth_token: String::new(),
            native_midi: false,
            initial_enabled_profiles: vec![],
        },
    )
    .await
    .unwrap();
    {
        let mut state = registry.write().await;
        state.midi_client = Some(Arc::new(client));
        state.midi_access.write().unwrap().replace(vec![OwnedPorts {
            device_id: "test".into(),
            lease: "test".into(),
            inputs: vec!["test-input".into()],
            outputs: vec![],
        }]);
    }
    shutdown_connection(&Arc::downgrade(&registry), None).await;
    service.assert_disconnected().await;
}

#[tokio::test]
async fn shutdown_interrupts_pending_snapshot_and_joins_reconnect_task() {
    let registry = registry();
    let service = Service::start(Arc::downgrade(&registry), false).await;
    let file = service.endpoint_file();
    let task = start_with_path(Arc::downgrade(&registry), Some(file.path().into()));
    tokio::time::timeout(Duration::from_secs(3), service.snapshot_seen.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), task.shutdown())
        .await
        .expect("pending Snapshot must not delay shutdown");
    service.assert_disconnected().await;
}

#[tokio::test]
async fn shutdown_recovers_client_from_in_flight_hello() {
    let registry = registry();
    let service = Service::start(Arc::downgrade(&registry), true).await;
    let file = service.endpoint_file();
    let task = start_with_path(Arc::downgrade(&registry), Some(file.path().into()));
    tokio::time::timeout(Duration::from_secs(3), service.hello_seen.notified())
        .await
        .unwrap();
    let stop = task.shutdown.clone();
    let shutdown = tokio::spawn(task.shutdown());
    stop.cancelled().await;
    assert!(
        registry.read().await.midi_client.is_none(),
        "Hello has not returned its client yet"
    );
    service.hello_reply.notify_one();
    tokio::time::timeout(Duration::from_secs(2), shutdown)
        .await
        .unwrap()
        .unwrap();
    service.assert_disconnected().await;
}

#[tokio::test]
async fn shutdown_without_service_stops_retries() {
    let registry = registry();
    let task = start_with_path(Arc::downgrade(&registry), None);
    tokio::time::timeout(Duration::from_secs(1), task.shutdown())
        .await
        .unwrap();
    assert!(!registry.read().await.midi_enabled());
    assert!(registry.read().await.midi_client.is_none());
}

#[tokio::test]
async fn shutdown_joins_task_before_releasing_local_ports() {
    let registry = registry();
    registry
        .read()
        .await
        .midi_access
        .write()
        .unwrap()
        .replace(vec![OwnedPorts {
            device_id: "test".into(),
            lease: "test".into(),
            inputs: vec!["test-input".into()],
            outputs: vec![],
        }]);
    let stop = CancellationToken::new();
    let token = stop.clone();
    let state = registry.clone();
    let joined_before_release = Arc::new(AtomicBool::new(false));
    let observed = joined_before_release.clone();
    let task = tokio::spawn(async move {
        token.cancelled().await;
        observed.store(
            state
                .read()
                .await
                .midi_access
                .read()
                .unwrap()
                .permits_input("test-input"),
            Ordering::SeqCst,
        );
        None
    });
    MidiUseTask {
        shutdown: stop,
        task,
        registry: Arc::downgrade(&registry),
    }
    .shutdown()
    .await;
    assert!(joined_before_release.load(Ordering::SeqCst));
    assert!(
        registry
            .read()
            .await
            .midi_access
            .read()
            .unwrap()
            .owned
            .is_empty()
    );
}

#[tokio::test]
async fn client_close_has_a_two_second_budget_and_propagates_errors() {
    let error = close_with_timeout(async { anyhow::bail!("transport failed") })
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "transport failed");
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        close_with_timeout(std::future::pending()),
    )
    .await;
    let error = result.expect("close must be bounded").unwrap_err();
    assert!(error.to_string().contains("timed out after 2s"));
}

#[tokio::test(start_paused = true)]
async fn unresponsive_task_is_aborted_and_joined_within_shutdown_budget() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let registry = registry();
    let dropped = Arc::new(AtomicBool::new(false));
    let mark = dropped.clone();
    let ready = Arc::new(Notify::new());
    let started = ready.clone();
    let task = tokio::spawn(async move {
        let _guard = Dropped(mark);
        started.notify_one();
        std::future::pending::<Option<Arc<Client>>>().await
    });
    ready.notified().await;
    let worker = MidiUseTask {
        shutdown: CancellationToken::new(),
        task,
        registry: Arc::downgrade(&registry),
    };
    tokio::time::timeout(Duration::from_secs(14), worker.shutdown())
        .await
        .expect("unresponsive task must not hang exit");
    assert!(
        dropped.load(Ordering::SeqCst),
        "abort must be joined before returning"
    );
    assert!(!registry.read().await.midi_enabled());
}

#[test]
fn process_stop_grace_covers_task_recovery_and_client_close() {
    assert!(crate::daemon::process::DAEMON_STOP_TIMEOUT > TASK_STOP_TIMEOUT + CLIENT_CLOSE_TIMEOUT);
}
