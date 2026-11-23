//! Grok の設定切替と effort の候補合わせ（design 42 §7）。
use std::sync::Mutex;

use super::acp_config::{AcpConfigNotice, AcpConfigOption};
use super::acp_host::AcpConfigHandle;
#[cfg(test)]
use ts_rs::TS;

/// 稼働中の Grok に適用する要求。registry に保存する intent ではない。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
#[serde(deny_unknown_fields)]
pub struct GrokSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reasoning_effort: Option<String>,
}

impl GrokSettings {
    pub fn validate_shape(&self) -> Result<(), String> {
        for (axis, value) in [
            ("model", &self.model),
            ("reasoning_effort", &self.reasoning_effort),
        ] {
            if let Some(value) = value
                && (value.is_empty() || value.len() > 512 || value.chars().any(char::is_control))
            {
                return Err(format!("grok の {axis} の値が不正"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrokEffortAdjustment {
    pub from: String,
    pub to: String,
}

#[derive(Default)]
pub(super) struct GrokFeedback {
    pub notice: Option<AcpConfigNotice>,
    pub error: Option<String>,
}

/// host と同じ寿命。registry に意図は保存しない。
#[derive(Default)]
pub(super) struct GrokConfigState {
    gate: tokio::sync::Mutex<()>,
    pub feedback: Mutex<GrokFeedback>,
    failed_surface: Mutex<Option<Vec<AcpConfigOption>>>,
}

impl GrokConfigState {
    fn report(&self, result: &Result<Option<GrokEffortAdjustment>, String>) {
        *self.feedback.lock().unwrap() = GrokFeedback {
            notice: result
                .as_ref()
                .ok()
                .and_then(|a| a.as_ref())
                .map(|a| AcpConfigNotice {
                    message: format!("effort を {} → {} に揃えました", a.from, a.to),
                    occurred_at_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                }),
            error: result.as_ref().err().cloned(),
        };
    }

    pub async fn apply(
        &self,
        handle: &AcpConfigHandle,
        desired: &GrokSettings,
    ) -> Result<(), String> {
        let _guard = self.gate.lock().await;
        let result = match desired.validate_shape() {
            Ok(()) => apply_switch(handle, desired).await,
            Err(error) => Err(error),
        };
        self.report(&result);
        // 失敗した同じ設定面を観測して、自動で再送しない。手動の再試行は許す。
        *self.failed_surface.lock().unwrap() = result.as_ref().err().and_then(|_| handle.options());
        result.map(|_| ())
    }

    pub async fn normalize(&self, handle: &AcpConfigHandle) -> Result<(), String> {
        // 切替中の model 通知を見ても、model→effort の間へ割り込まない。
        let Ok(_guard) = self.gate.try_lock() else {
            return Ok(());
        };
        let Some(options) = handle.options() else {
            return Ok(());
        };
        if self.failed_surface.lock().unwrap().as_ref() == Some(&options) {
            return Ok(());
        }
        let Some(effort) = options.iter().find(|o| o.id == "reasoning_effort") else {
            return Ok(());
        };
        if effort
            .choices
            .iter()
            .any(|c| c.value == effort.current_value)
        {
            return Ok(());
        }
        let result = match aligned_effort(effort) {
            Some(target) => match handle.set("reasoning_effort", &target).await {
                Ok(updated) => valid_effort(&updated).map(|actual| {
                    Some(GrokEffortAdjustment {
                        from: effort.current_value.clone(),
                        to: actual.current_value.clone(),
                    })
                }),
                Err(error) => Err(error),
            },
            None => Err("grok: reasoning_effort を有効な候補にそろえられません".into()),
        };
        self.report(&result);
        *self.failed_surface.lock().unwrap() = result.as_ref().err().and_then(|_| handle.options());
        result.map(|_| ())
    }
}

fn valid_effort(options: &[AcpConfigOption]) -> Result<&AcpConfigOption, String> {
    options
        .iter()
        .find(|o| o.id == "reasoning_effort")
        .filter(|o| o.choices.iter().any(|c| c.value == o.current_value))
        .ok_or_else(|| "grok: effort の切替後も有効な現在値を確認できません".to_string())
}

/// model の応答を読んでから effort を決める。途中の成功は巻き戻さない。
async fn apply_switch(
    handle: &AcpConfigHandle,
    desired: &GrokSettings,
) -> Result<Option<GrokEffortAdjustment>, String> {
    let Some(model) = &desired.model else {
        if let Some(effort) = &desired.reasoning_effort {
            handle.set("reasoning_effort", effort).await?;
        }
        return Ok(None);
    };
    let options = handle.set("model", model).await?;
    let Some(effort) = options.iter().find(|o| o.id == "reasoning_effort") else {
        return if desired.reasoning_effort.is_some() {
            Err("grok: この model には reasoning_effort の設定がありません".into())
        } else {
            Ok(None)
        };
    };
    if effort.choices.is_empty() {
        return Err("grok: reasoning_effort の候補が空です".into());
    }
    let mut requested = effort.clone();
    if let Some(value) = &desired.reasoning_effort {
        requested.current_value.clone_from(value);
    }
    let replacement = aligned_effort(&requested);
    if replacement.is_none()
        && !requested
            .choices
            .iter()
            .any(|c| c.value == requested.current_value)
    {
        return Err(
            "grok: reasoning_effort の候補の順位を比較できません。候補から選んでください".into(),
        );
    }
    let target = replacement.as_deref().unwrap_or(&requested.current_value);
    if target == effort.current_value {
        return Ok(replacement.map(|to| GrokEffortAdjustment {
            from: requested.current_value,
            to,
        }));
    }
    let updated = handle.set("reasoning_effort", target).await?;
    let actual = valid_effort(&updated)?;
    Ok(replacement.map(|_| GrokEffortAdjustment {
        from: requested.current_value,
        to: actual.current_value.clone(),
    }))
}

/// acp-probe（mem_1ChEhaHVsXPkjVuqpuadCZ）の effort の意味順を Grok 内に閉じる。
/// 候補は engine のものを使い、直近の下 → 直近の上 → 提示順の先頭で合わせる。
/// 現値が有効、または候補が空なら補正を返さない（空候補の失敗は適用側が扱う）。
pub(crate) fn aligned_effort(option: &AcpConfigOption) -> Option<String> {
    if option
        .choices
        .iter()
        .any(|c| c.value == option.current_value)
    {
        return None;
    }
    const ORDER: [&str; 4] = ["low", "medium", "high", "xhigh"];
    if let Some(rank) = ORDER.iter().position(|v| *v == option.current_value) {
        let lower = ORDER[..rank].iter().rev();
        let higher = ORDER[rank + 1..].iter();
        for candidate in lower.chain(higher) {
            if option.choices.iter().any(|c| c.value == *candidate) {
                return Some((*candidate).to_string());
            }
        }
        return None;
    }
    option.choices.first().map(|c| c.value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::acp_config::AcpConfigChoice;

    // Task mem_1ChKFNBY7VQYSwFseyxUs9
    #[test]
    fn effort_alignment_uses_only_engine_choices() {
        for (current, choices, expected) in [
            ("high", vec!["low", "high"], None),
            ("xhigh", vec!["low", "medium", "high"], Some("high")),
            ("xhigh", vec!["medium", "low"], Some("medium")),
            ("medium", vec!["xhigh", "low"], Some("low")),
            ("future", vec!["engine-first", "high"], Some("engine-first")),
            ("low", vec!["high", "medium"], Some("medium")),
            ("xhigh", vec![], None),
            ("high", vec!["future"], None),
        ] {
            let option = AcpConfigOption {
                id: "reasoning_effort".into(),
                name: "Reasoning effort".into(),
                current_value: current.into(),
                choices: choices
                    .iter()
                    .map(|v| AcpConfigChoice {
                        value: (*v).into(),
                        name: (*v).into(),
                        description: None,
                    })
                    .collect(),
            };
            assert_eq!(
                aligned_effort(&option).as_deref(),
                expected,
                "{current}: {choices:?}"
            );
        }
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;
    use crate::conversation::acp_host::testing::{FakeAgent, Script};
    use crate::conversation::{AcpAgentHost, AcpEngine, AcpHostConfig};
    use serde_json::{Value, json};

    fn options(model: &str, efforts: &[&str]) -> Vec<Value> {
        vec![
            json!({"id":"model","name":"Model","type":"select","currentValue":model,
                "options":[{"value":"model-a","name":"A"},{"value":"model-b","name":"B"}]}),
            json!({"id":"reasoning_effort","name":"Effort","type":"select","currentValue":"xhigh",
                "options":efforts.iter().map(|v| json!({"value":v,"name":v})).collect::<Vec<_>>()}),
        ]
    }

    async fn spawn(script: Script) -> (FakeAgent, AcpAgentHost) {
        let (agent, connector) = FakeAgent::new(script);
        let host = AcpAgentHost::spawn_with(
            AcpHostConfig {
                engine: AcpEngine::Grok,
                cwd: "/tmp".into(),
                repo: "vptest-grok-settings".into(),
                lane: "lead".into(),
                lane_label: "lead".into(),
                session_key: 1,
                session_id: None,
                config_intent: vec![],
            },
            connector,
        )
        .unwrap();
        host.take_replay_port()
            .unwrap()
            .source
            .attach()
            .await
            .unwrap();
        (agent, host)
    }

    fn request() -> GrokSettings {
        GrokSettings {
            model: Some("model-b".into()),

            ..Default::default()
        }
    }

    fn sets(agent: &FakeAgent) -> Vec<(String, String)> {
        agent
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == "session/set_config_option")
            .map(|(_, p)| {
                (
                    p["configId"].as_str().unwrap().into(),
                    p["value"].as_str().unwrap().into(),
                )
            })
            .collect()
    }

    // Task mem_1ChKFNBY7VQYSwFseyxUs9
    #[tokio::test]
    async fn grok_model_then_effort_uses_new_candidates_and_reports_adjustment_without_intent() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, host) = spawn(Script {
            config: options("model-a", &["xhigh", "high"]),
            config_after_model: Some(options("model-b", &["high", "low"])),
            ..Default::default()
        })
        .await;
        host.config_handle().apply_grok(&request()).await.unwrap();
        assert_eq!(
            sets(&agent),
            [
                ("model".into(), "model-b".into()),
                ("reasoning_effort".into(), "high".into())
            ]
        );
        let view = host.config_view().unwrap();
        assert_eq!(view.options[1].current_value, "high");
        assert_eq!(
            view.notice.unwrap().message,
            "effort を xhigh → high に揃えました"
        );
        let saved = crate::lane::session_registry::load("vptest-grok-settings", "lead", "grok");
        assert!(saved.sessions.iter().all(|s| s.settings.is_none()));
    }

    #[tokio::test]
    async fn grok_effort_failure_keeps_applied_model_and_reports_completion_error() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, host) = spawn(Script {
            config: options("model-a", &["xhigh", "high"]),
            config_after_model: Some(options("model-b", &["high"])),
            reject_config: Some("reasoning_effort".into()),
            ..Default::default()
        })
        .await;
        assert!(host.config_handle().apply_grok(&request()).await.is_err());
        assert_eq!(sets(&agent).len(), 2);
        let view = host.config_view().unwrap();
        assert_eq!(view.options[0].current_value, "model-b");
        assert_eq!(view.options[1].current_value, "xhigh");
        assert!(view.error.is_some());
        assert!(view.notice.is_none());
    }

    #[tokio::test]
    async fn grok_valid_effort_is_preserved_and_explicit_effort_follows_model() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, host) = spawn(Script {
            config: options("model-a", &["xhigh", "high"]),
            ..Default::default()
        })
        .await;
        host.config_handle().apply_grok(&request()).await.unwrap();
        assert_eq!(sets(&agent), [("model".into(), "model-b".into())]);
        let desired = GrokSettings {
            reasoning_effort: Some("high".into()),
            ..request()
        };
        host.config_handle().apply_grok(&desired).await.unwrap();
        assert_eq!(
            sets(&agent)[1..],
            [
                ("model".into(), "model-b".into()),
                ("reasoning_effort".into(), "high".into())
            ]
        );
        assert!(host.config_view().unwrap().notice.is_none());
    }
    #[tokio::test]
    async fn grok_requests_do_not_interleave_between_model_and_effort() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, host) = spawn(Script {
            config: options("model-a", &["xhigh", "high", "low"]),
            config_after_model: Some(options("model-b", &["high", "low"])),
            hold_config: true,
            ..Default::default()
        })
        .await;
        let handle = host.config_handle();
        let first = tokio::spawn(async move { handle.apply_grok(&request()).await });
        agent.wait_for("session/set_config_option").await;
        let handle = host.config_handle();
        let second = tokio::spawn(async move {
            handle
                .apply_grok(&GrokSettings {
                    reasoning_effort: Some("low".into()),

                    ..Default::default()
                })
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(
            sets(&agent).len(),
            1,
            "別要求を model と effort の間へ送らない"
        );
        assert!(host.config_view().unwrap().notice.is_none());
        agent.release_config();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while sets(&agent).len() < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(sets(&agent)[1], ("reasoning_effort".into(), "high".into()));
        assert!(
            host.config_view().unwrap().notice.is_none(),
            "model だけでは未完了"
        );
        agent.release_config();
        first.await.unwrap().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while sets(&agent).len() < 3 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(sets(&agent)[2], ("reasoning_effort".into(), "low".into()));
        agent.release_config();
        second.await.unwrap().unwrap();
        let view = host.config_view().unwrap();
        assert_eq!(view.options[1].current_value, "low");
        assert!(view.error.is_none());
    }

    #[tokio::test]
    async fn grok_missing_or_empty_effort_axis_never_sends_an_invented_value() {
        let _state = crate::test_env::state_dir_async().await;
        for absent in [true, false] {
            let mut next = options("model-b", &[]);
            if absent {
                next.pop();
            }
            let (agent, host) = spawn(Script {
                config: options("model-a", &["xhigh"]),
                config_after_model: Some(next),
                ..Default::default()
            })
            .await;
            let result = host.config_handle().apply_grok(&request()).await;
            assert_eq!(result.is_ok(), absent);
            assert_eq!(sets(&agent), [("model".into(), "model-b".into())]);
            assert_eq!(host.config_view().unwrap().error.is_none(), absent);
        }
    }

    #[tokio::test]
    async fn grok_observation_normalizes_invalid_loaded_effort_without_model_intent() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, host) = spawn(Script {
            config: options("model-b", &["high", "low"]),
            ..Default::default()
        })
        .await;
        host.config_handle().normalize_grok().await.unwrap();
        assert_eq!(host.config_view().unwrap().options[1].current_value, "high");
        host.config_handle().normalize_grok().await.unwrap();
        assert_eq!(sets(&agent), [("reasoning_effort".into(), "high".into())]);
    }

    #[tokio::test]
    async fn grok_observation_stops_after_an_invalid_normalization_response() {
        let _state = crate::test_env::state_dir_async().await;
        let (agent, host) = spawn(Script {
            config: options("model-b", &["high"]),
            ignore_config: true,
            ..Default::default()
        })
        .await;
        let _ = host.config_handle().normalize_grok().await;
        let _ = host.config_handle().normalize_grok().await;
        assert_eq!(
            sets(&agent),
            [("reasoning_effort".into(), "high".into())],
            "同じ不正応答では往復しない"
        );
        assert!(host.config_view().unwrap().error.is_some());
    }

    /// 実機 1 会話・0 turn。候補合わせと別 process の load を同じ試験会話で確かめる。
    #[tokio::test]
    #[ignore = "実機 grok CLI + auth が必要（ローカル E2E 専用）"]
    async fn e2e_real_grok_settings_survive_load_with_aligned_effort() {
        use crate::conversation::ConversationEvent;
        use sha2::{Digest, Sha256};
        let _state = crate::test_env::state_dir_async().await;
        let tmp = tempfile::tempdir().unwrap();
        let home = std::env::var("HOME").unwrap();
        let config_path = std::path::PathBuf::from(&home).join(".grok/config.toml");
        let before = Sha256::digest(std::fs::read(&config_path).unwrap());
        let cli = crate::lane::session_store::resolve_cli(
            "grok",
            &[std::path::PathBuf::from(home).join(".grok/bin/grok")],
        );
        let version = std::process::Command::new(&cli)
            .arg("--version")
            .output()
            .unwrap();
        assert!(version.status.success());
        println!(
            "Grok version: {}",
            String::from_utf8_lossy(&version.stdout).trim()
        );
        let config = |session_id| AcpHostConfig {
            engine: AcpEngine::Grok,
            cwd: tmp.path().to_string_lossy().into_owned(),
            repo: "vptest-grok-settings-real".into(),
            lane: "lead".into(),
            lane_label: "lead".into(),
            session_key: 1,
            session_id,
            config_intent: vec![],
        };
        let mut host = AcpAgentHost::spawn(config(None)).unwrap();
        host.take_replay_port()
            .unwrap()
            .source
            .attach()
            .await
            .unwrap();
        let session = match host.session_init().unwrap() {
            ConversationEvent::SessionInit { session_id, .. } => session_id,
            _ => panic!("SessionInit"),
        };
        println!("Grok test session: {session}, cwd={}", tmp.path().display());
        // panic 時も、この試験で新しく作った会話だけを消す。
        struct Cleanup {
            cli: String,
            session: String,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let result = std::process::Command::new(&self.cli)
                    .args(["sessions", "delete", &self.session])
                    .output();
                eprintln!(
                    "Grok test session cleanup: {}",
                    result.as_ref().is_ok_and(|o| o.status.success())
                );
            }
        }
        let cleanup = Cleanup {
            cli: cli.clone(),
            session: session.clone(),
        };
        let initial = host.config_view().unwrap();
        println!(
            "Grok initial config: {}",
            serde_json::to_string(&initial).unwrap()
        );
        assert!(
            initial
                .options
                .iter()
                .any(|o| o.id == "model" && o.choices.iter().any(|c| c.value == "grok-4.5"))
        );
        assert!(initial.options.iter().any(|o|o.id=="reasoning_effort" && o.choices.iter().any(|c|c.value=="xhigh")));
        host.config_handle()
            .apply_grok(&GrokSettings {
                reasoning_effort: Some("xhigh".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        host.config_handle()
            .apply_grok(&GrokSettings {
                model: Some("grok-4.5".into()),

                ..Default::default()
            })
            .await
            .unwrap();
        let changed = host.config_view().unwrap();
        println!(
            "Grok changed config: {}",
            serde_json::to_string(&changed).unwrap()
        );
        assert_eq!(
            changed
                .options
                .iter()
                .find(|o| o.id == "model")
                .unwrap()
                .current_value,
            "grok-4.5"
        );
        assert_eq!(
            changed
                .options
                .iter()
                .find(|o| o.id == "reasoning_effort")
                .unwrap()
                .current_value,
            "high"
        );
        assert_eq!(
            changed.notice.unwrap().message,
            "effort を xhigh → high に揃えました"
        );
        let pid = host.pid();
        host.stop();
        drop(host);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let mut host = AcpAgentHost::spawn(config(Some(session.clone()))).unwrap();
        host.take_replay_port()
            .unwrap()
            .source
            .attach()
            .await
            .unwrap();
        assert_ne!(host.pid(), pid);
        assert!(
            matches!(host.session_init(),Some(ConversationEvent::SessionInit {session_id,..}) if session_id==session)
        );
        let loaded = host.config_view().unwrap();
        println!(
            "Grok loaded config (different process): {}",
            serde_json::to_string(&loaded).unwrap()
        );
        assert_eq!(
            loaded
                .options
                .iter()
                .find(|o| o.id == "model")
                .unwrap()
                .current_value,
            "grok-4.5"
        );
        assert_eq!(
            loaded
                .options
                .iter()
                .find(|o| o.id == "reasoning_effort")
                .unwrap()
                .current_value,
            "high"
        );
        // load の候補外 effort は attach 時に正規化される。notice は host の揮発状態。
        host.stop();
        drop(host);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let deleted = std::process::Command::new(&cli)
            .args(["sessions", "delete", &session])
            .output()
            .unwrap();
        assert!(deleted.status.success(), "試験会話を削除");
        std::mem::forget(cleanup);
        let after = Sha256::digest(std::fs::read(&config_path).unwrap());
        println!(
            "Grok config SHA256 before={} after={}; 1 session / 0 turns / 2 processes; deleted={session}",
            before
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            after.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        assert_eq!(before, after, "mako の Grok 設定は変更しない");
    }
}
