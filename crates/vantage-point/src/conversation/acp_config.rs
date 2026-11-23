//! ACP Session Config Options の wire 型（doc 88 §6）— engine が出す「切り替えられる軸と候補」。
//!
//! 解釈（`configOptions` → 型、当て直しの要否）は共有 crate `agent-chat-acp` の `config` が持つ
//! （2026-11-20 に VP の純関数をそちらへ寄せた）。ここには **roster に載せる wire の型**と VP が補足する通知を置く:
//! webview の型（`webview/src/generated/`）を ts-rs で生成するには、derive を型の定義に付ける必要が
//! あり、他 crate の型には付けられない。engine が返す形は crate の型と同じで、[`From`] で写す。通知は VP host が足す。
//!
//! 候補の表は**engine が正**（VP は固定の表を持たない）。roster（`LaneSessionView.acp_config`）で
//! GUI に運び、engine 別の panel が並べる。engine 別の意図の形は [`super::agy_settings`]。

#[cfg(test)]
use ts_rs::TS;

/// 1 つの軸の候補 1 件。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct AcpConfigChoice {
    /// `set_config_option` に渡す値。
    pub value: String,
    /// 表示名。
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub description: Option<String>,
}

/// 切り替えられる軸 1 つ（ACP の `type: "select"`）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct AcpConfigOption {
    /// 軸の id（`model` / `thought_level` / `mode` …）。`set_config_option` の `configId`。
    pub id: String,
    /// 表示名（`Thinking Level` 等）。
    pub name: String,
    /// engine 上の現在値。
    pub current_value: String,
    /// 候補（group は平らにしてある）。
    pub choices: Vec<AcpConfigChoice>,
}

/// roster に載せる ACP engine の設定面（`LaneSessionView.acp_config`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct AcpConfigView {
    pub options: Vec<AcpConfigOption>,
    /// 直近の切替 / 当て直しの失敗（engine のエラー文）。次の成功で消える。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub error: Option<String>,
    /// 自動補正など、設定面に添える通知（VP host 内の揮発状態）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub notice: Option<AcpConfigNotice>,
}

/// engine 共通の設定通知。日時は通知を作った時だけ更新する。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct AcpConfigNotice {
    pub message: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub occurred_at_ms: u64,
}

impl From<agent_chat_acp::AcpConfigChoice> for AcpConfigChoice {
    fn from(c: agent_chat_acp::AcpConfigChoice) -> Self {
        Self {
            value: c.value,
            name: c.name,
            description: c.description,
        }
    }
}

impl From<agent_chat_acp::AcpConfigOption> for AcpConfigOption {
    fn from(o: agent_chat_acp::AcpConfigOption) -> Self {
        Self {
            id: o.id,
            name: o.name,
            current_value: o.current_value,
            choices: o.choices.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<agent_chat_acp::AcpConfigView> for AcpConfigView {
    fn from(v: agent_chat_acp::AcpConfigView) -> Self {
        Self {
            options: v.options.into_iter().map(Into::into).collect(),
            error: v.error,
            notice: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// crate の型と wire の形が同じ（写しで field を落とさない）。JSON にして比べるので、片方に
    /// field が増えたらここで落ちる。
    /// 落ちるべき壊し方: `From` で `description` や `error` を写し忘れる。
    #[test]
    fn mirrors_the_shared_crate_types_field_for_field() {
        let shared = agent_chat_acp::AcpConfigView {
            options: agent_chat_acp::config::parse_config_options(&serde_json::json!([
                {"id": "thought_level", "name": "Thinking Level", "type": "select",
                 "currentValue": "medium",
                 "options": [{"value": "low", "name": "Low", "description": "速い"},
                             {"value": "medium", "name": "Medium"}]}
            ]))
            .expect("configOptions"),
            error: Some("mode を plan にできません".to_string()),
        };
        let wire = AcpConfigView::from(shared.clone());
        assert_eq!(
            serde_json::to_value(&wire).unwrap(),
            serde_json::to_value(&shared).unwrap()
        );
        assert_eq!(
            wire.options[0].choices[0].description.as_deref(),
            Some("速い")
        );
    }

    #[test]
    fn export_acp_config_ts() {
        <AcpConfigView as TS>::export_all(&ts_rs::Config::from_env())
            .expect("AcpConfigView の TS export 失敗");
    }
}
