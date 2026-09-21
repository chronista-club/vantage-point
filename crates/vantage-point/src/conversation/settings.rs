//! engine settings — 「次にどう走らせるか」を engine ごとの型で運ぶ束ね役。
//!
//! ## 原則（mako 裁定 2026-09-21）
//! - **共通の器は作らない**。model / effort の語彙は engine ごとに違い（名称も数も）、
//!   effort を持たない engine もある。共有側が知るのは「どの engine の settings か」だけ。
//! - **文字列で engine を分岐するのは最小限に**（`if agent == "codex"` を散らさない）。基本は
//!   variant の `match` で dispatch し、engine を足す / 消すときは compiler が網羅性で全分岐を指す。
//! - **engine の code は engine の module に閉じる**。中身の型は
//!   [`super::claude_settings`] / [`super::codex_settings`] / [`super::vpcode_catalog`] が所有し、
//!   本 module はそれを束ねて registry / RPC / GUI に運ぶ。
//!
//! settings 層で「engine X を消す」= X の settings module を消して variant を 1 つ消す。
//! 共有側に残るのは match の腕だけ（compiler が指す）。

use super::claude_settings::ClaudeSettings;
use super::codex_settings::CodexSelection;
use super::engine::EngineKind;
use super::vpcode_catalog::VpcodeSettings;
#[cfg(test)]
use ts_rs::TS;

/// session に永続される engine 別の設定（registry の `SessionEntry::settings`）。
///
/// serde は externally tagged（`{"claude": {...}}` / `{"codex": {...}}` / `{"vpcode": {...}}`）
/// — wire / disk 上で「どの engine の設定か」が key に出るので、engine を跨いだ誤適用が
/// 型で弾ける（[`Self::kind`] と entry の agent の一致検査）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
#[serde(rename_all = "snake_case")]
pub enum EngineSettings {
    Claude(ClaudeSettings),
    Codex(CodexSelection),
    Vpcode(VpcodeSettings),
}

impl EngineSettings {
    /// この settings が属する engine。
    pub fn kind(&self) -> EngineKind {
        match self {
            Self::Claude(_) => EngineKind::Claude,
            Self::Codex(_) => EngineKind::Codex,
            Self::Vpcode(_) => EngineKind::Vpcode,
        }
    }

    /// 永続前の形式検査（各 engine の validate に委譲）。
    pub fn validate_shape(&self) -> Result<(), String> {
        match self {
            Self::Claude(s) => s.validate_shape(),
            Self::Codex(s) => s.validate_shape(),
            Self::Vpcode(s) => s.validate_shape(),
        }
    }

    /// 「model だけ」の指定から engine の settings を組む（CLI `--model` / lane 作成の初期指定 /
    /// 旧 registry file の `model` field の移行）。`None` = その engine は VP から model 指定を
    /// 受けない（述語は [`EngineKind::takes_model_intent`] — ここの match と食い違わないよう
    /// test で固定）。
    pub fn from_model(kind: EngineKind, model: String) -> Option<Self> {
        match kind {
            EngineKind::Claude => Some(Self::Claude(ClaudeSettings {
                model: Some(model),
                ..Default::default()
            })),
            EngineKind::Vpcode => Some(Self::Vpcode(VpcodeSettings { model })),
            EngineKind::Codex | EngineKind::Grok | EngineKind::OpenCode => None,
        }
    }

    pub fn claude(&self) -> Option<&ClaudeSettings> {
        match self {
            Self::Claude(s) => Some(s),
            _ => None,
        }
    }

    pub fn codex(&self) -> Option<&CodexSelection> {
        match self {
            Self::Codex(s) => Some(s),
            _ => None,
        }
    }

    pub fn vpcode(&self) -> Option<&VpcodeSettings> {
        match self {
            Self::Vpcode(s) => Some(s),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn externally_tagged_by_engine() {
        let s = EngineSettings::Claude(ClaudeSettings {
            model: Some("claude-sonnet-5".into()),
            effort: None,
        });
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({"claude": {"model": "claude-sonnet-5"}})
        );
        let s = EngineSettings::Codex(CodexSelection {
            model: "gpt".into(),
            effort: "high".into(),
        });
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({"codex": {"model": "gpt", "effort": "high"}})
        );
        let back: EngineSettings =
            serde_json::from_value(serde_json::json!({"vpcode": {"model": "openai/gpt-oss-20b"}}))
                .unwrap();
        assert_eq!(back.kind(), EngineKind::Vpcode);
    }

    #[test]
    fn from_model_only_for_engines_that_take_a_model_intent() {
        assert!(EngineSettings::from_model(EngineKind::Claude, "m".into()).is_some());
        assert!(EngineSettings::from_model(EngineKind::Vpcode, "m".into()).is_some());
        assert!(EngineSettings::from_model(EngineKind::Codex, "m".into()).is_none());
        assert!(EngineSettings::from_model(EngineKind::Grok, "m".into()).is_none());
        // 述語と match が食い違わない（全 engine で一致）
        for kind in EngineKind::ALL {
            assert_eq!(
                EngineSettings::from_model(kind, "m".into()).is_some(),
                kind.takes_model_intent(),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn validate_shape_is_the_injection_guard() {
        let bad = EngineSettings::Claude(ClaudeSettings {
            model: Some("opus --dangerous".into()),
            effort: None,
        });
        assert!(bad.validate_shape().is_err());
        let ok = EngineSettings::Claude(ClaudeSettings::default());
        assert!(ok.validate_shape().is_ok());
    }

    /// TS 型（webview が IPC で組む `settings` の形）を export する。
    #[test]
    fn export_engine_settings_ts() {
        <EngineSettings as TS>::export_all(&ts_rs::Config::from_env())
            .expect("EngineSettings の TS export 失敗");
    }
}
