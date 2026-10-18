//! Adapter to the midistage setup contract; no physical port access.
use crate::{
    roto_setup::{Generated, SETUP},
    roto_setup_sync::{Status, Transport},
};
use midistage_client::{ApplyDeviceSetup, Client, SetDeviceSetups, Snapshot, protocol::Phase};

pub(super) fn statuses(snapshot: &Snapshot) -> Vec<Status> {
    snapshot
        .devices
        .iter()
        .filter(|device| device.state.profile_id == "roto")
        .map(|device| Status {
            device: device.state.device_id.clone(),
            lease: device
                .state
                .lease
                .as_ref()
                .filter(|lease| {
                    device.state.present
                        && device.state.phase == Phase::Active
                        && lease.session_id == snapshot.session_id
                })
                .map(|lease| lease.token.clone()),
            sequence: snapshot.sequence,
            configuring: device.configuring,
            current_setup: device.current_setup,
            error: device.config_error.clone(),
        })
        .collect()
}

fn reply_status(snapshot: &Snapshot, device: &str) -> anyhow::Result<Status> {
    statuses(snapshot)
        .into_iter()
        .find(|s| s.device == device)
        .ok_or_else(|| anyhow::anyhow!("ROTO device missing from setup reply"))
}

#[async_trait::async_trait]
impl Transport for Client {
    async fn register(&self, device: &str) -> anyhow::Result<Status> {
        let reply = self
            .set_device_setups(&SetDeviceSetups {
                device_id: device.to_owned(),
                setups: vec![SETUP],
            })
            .await?;
        anyhow::ensure!(
            reply.devices.iter().any(|d| d.state.device_id == device
                && d.setup_owners
                    .get("vp")
                    .is_some_and(|seats| seats.contains(&SETUP))),
            "VP setup registration missing from reply"
        );
        reply_status(&reply, device)
    }

    async fn apply(
        &self,
        device: &str,
        lease: &str,
        generated: Generated,
    ) -> anyhow::Result<Status> {
        // KDL's content is JSON; decode with the upstream SDK before crossing the boundary.
        let content = serde_json::from_value(generated.content)?;
        let reply = self
            .apply_device_setup(&ApplyDeviceSetup {
                device_id: device.to_owned(),
                lease_token: lease.to_owned(),
                setup: SETUP,
                content,
            })
            .await?;
        reply_status(&reply, device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// Run explicitly with the pinned upstream protocol_fixture. It supplies fake
    /// devices and never opens CoreMIDI/Serial; the endpoint lives only in TempDir.
    #[tokio::test]
    #[ignore = "requires MIDISTAGE_TEST_SERVER built from the pinned SDK revision"]
    async fn isolated_sdk_loopback_registers_applies_and_caches_confirmed_content() {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        struct Observed {
            client: Client,
            calls: Mutex<Vec<&'static str>>,
            error: Mutex<Option<String>>,
        }
        #[async_trait::async_trait]
        impl Transport for Observed {
            async fn register(&self, device: &str) -> anyhow::Result<Status> {
                self.calls.lock().unwrap().push("register");
                Transport::register(&self.client, device).await
            }
            async fn apply(
                &self,
                device: &str,
                lease: &str,
                content: Generated,
            ) -> anyhow::Result<Status> {
                self.calls.lock().unwrap().push("apply");
                let result = Transport::apply(&self.client, device, lease, content).await;
                *self.error.lock().unwrap() = result.as_ref().err().map(ToString::to_string);
                result
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let binary = std::env::var_os("MIDISTAGE_TEST_SERVER")
            .expect("fixed-revision protocol_fixture path");
        let mut server = tokio::process::Command::new(binary)
            .arg(dir.path())
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let endpoint = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) = std::fs::read(dir.path().join("endpoint.json")) {
                    break serde_json::from_slice::<midistage_client::Endpoint>(&bytes).unwrap();
                }
                assert!(
                    server.try_wait().unwrap().is_none(),
                    "fixture exited before publishing its endpoint"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let (client, _) = Client::connect(
            &endpoint,
            midistage_client::Hello {
                protocol_version: midistage_client::protocol::PROTOCOL_VERSION,
                client_id: "vp".into(),
                display_name: "VP isolated test".into(),
                auth_token: String::new(),
                native_midi: false,
                initial_enabled_profiles: vec!["roto".into()],
            },
        )
        .await
        .unwrap();
        let observed = Arc::new(Observed {
            client,
            calls: Mutex::default(),
            error: Mutex::default(),
        });
        let mut sync = crate::roto_setup_sync::Sync::new(observed.clone(), dir.path());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = observed.client.snapshot().await.unwrap();
                sync.tick(&statuses(&snapshot)).await;
                assert_eq!(*observed.error.lock().unwrap(), None, "Apply must succeed");
                if crate::roto_setup_files::Hashes::load(dir.path())
                    .get("roto")
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("register, Apply and completion confirmation must finish");
        let expected = crate::roto_setup::generate(
            &Value::Null,
            &std::collections::BTreeMap::new(),
            &crate::roto_midi::VP_ROTO_SETUP,
        );
        assert_eq!(
            crate::roto_setup_files::Hashes::load(dir.path()).get("roto"),
            Some(expected.hash)
        );
        for _ in 0..20 {
            let snapshot = observed.client.snapshot().await.unwrap();
            let roto = snapshot
                .devices
                .iter()
                .find(|d| d.state.device_id == "roto")
                .unwrap();
            assert_eq!(roto.setup_owners.get("vp"), Some(&vec![SETUP]));
            assert_eq!(roto.current_setup, Some(SETUP));
            assert!(!roto.configuring);
            assert_eq!(roto.config_error, None);
            sync.tick(&statuses(&snapshot)).await;
        }
        assert_eq!(*observed.calls.lock().unwrap(), ["register", "apply"]);
        assert_eq!(*observed.error.lock().unwrap(), None);
        drop(sync);
        observed.client.close().await.unwrap();
        server.kill().await.unwrap();
        server.wait().await.unwrap();
    }
    fn snapshot() -> Value {
        json!({"sequence":10, "protocol_version":1, "server_epoch":"epoch", "session_id":"session",
            "devices":[{"device_id":"roto", "profile_id":"roto", "name":"ROTO", "present":true,
                "assignment":{"client_id":"vp", "revision":1, "expected":true}, "phase":"active",
                "lease":{"session_id":"session", "token":"lease"}, "releasing_to":null,
                "controls":[], "native_ports":{"inputs":[], "outputs":[]}, "error":null,
                "configuring":true, "current_setup":SETUP, "setup_owners":{"vp":[SETUP]}, "config_error":null}]})
    }

    #[test]
    fn owned_roto_snapshot_preserves_configuration_status() {
        let snap: Snapshot = serde_json::from_value(snapshot()).unwrap();
        let view = statuses(&snap);
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].lease.as_deref(), Some("lease"));
        assert_eq!(view[0].sequence, 10);
        assert!(view[0].configuring);
        assert_eq!(view[0].current_setup, Some(SETUP));
    }

    #[test]
    fn foreign_releasing_absent_or_non_roto_devices_are_not_owned_targets() {
        for (field, value) in [
            ("phase", json!("releasing")),
            ("present", json!(false)),
            ("lease", json!({"session_id":"another", "token":"lease"})),
            ("lease", Value::Null),
        ] {
            let mut value_json = snapshot();
            value_json["devices"][0][field] = value;
            let snap: Snapshot = serde_json::from_value(value_json).unwrap();
            let view = statuses(&snap);
            assert_eq!(view.len(), 1);
            assert_eq!(view[0].lease, None, "{field}");
        }
        let mut value = snapshot();
        value["devices"][0]["profile_id"] = json!("lpd8");
        assert!(statuses(&serde_json::from_value(value).unwrap()).is_empty());
    }

    #[test]
    fn generated_content_roundtrips_through_the_actual_sdk_without_field_loss() {
        let generated = crate::roto_setup::generate(
            &Value::Null,
            &std::collections::BTreeMap::new(),
            &crate::roto_midi::VP_ROTO_SETUP,
        );
        let content: midistage_client::SetupContent =
            serde_json::from_value(generated.content.clone()).unwrap();
        assert_eq!(serde_json::to_value(&content).unwrap(), generated.content);
        assert!(content.knobs.is_empty());
        assert_eq!(content.switches.len(), 8);
        assert_eq!(
            content.switches[7].action,
            midistage_client::SwitchAction::Push
        );
    }
}
