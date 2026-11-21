//! agy（Google Antigravity CLI）の settings — Model / Thinking Level / Mode の**意図**（doc 88 §6）。
//!
//! agy は ACP の Session Config Options（`configOptions` / `session/set_config_option`）で
//! 3 軸を切り替える。実測（agy 1.5.9、2026-11-17）で次が分かっている:
//!
//! - 軸の id は `model` / `thought_level` / `mode`。候補は engine が出す（thought_level の候補は
//!   model ごとに変わる — 不正値は `-32602 "Thinking level … is not offered for model …"`）
//! - **切り替えた値は `session/load` の後に残らない**（low → medium、accept-edits → default に戻る）
//!
//! そこで VP は「user が選んだ値」だけを registry（`SessionEntry.settings`）に残し、
//! [`super::acp_host::AcpAgentHost`] が session の確定後に `set_config_option` で当て直す。
//! 選んでいない軸は `None` = engine 既定に委ねる（VP は既定値を覚えない）。
//!
//! 候補の表は VP が持たない（engine の `configOptions` が正 — [`super::acp_config`]）。
//! ここが持つのは「どの軸に何を当てるか」と、その形式検査だけ。

#[cfg(test)]
use ts_rs::TS;

/// agy の session に永続される意図（registry の `{"agy": {...}}`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct AgySettings {
    /// configOption `model` の値（例: `gemini-4-argon`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub model: Option<String>,
    /// configOption `thought_level` の値（例: `low` / `medium` / `high`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub thought_level: Option<String>,
    /// configOption `mode` の値（例: `default` / `accept-edits` / `auto` / `plan`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub mode: Option<String>,
}

/// 値の長さの上限（実測の最長は `gemini-pro-orchestrator-g1-pro` の 30 文字）。
const MAX_VALUE_LEN: usize = 64;

impl AgySettings {
    /// 当てる順の `(config id, 値)` 列。**model を先に当てる** — thought_level の候補は
    /// model で変わるので、逆順だと新しい model に無い level を古い model に当てて弾かれる。
    pub fn config_pairs(&self) -> Vec<(String, String)> {
        [
            ("model", &self.model),
            ("thought_level", &self.thought_level),
            ("mode", &self.mode),
        ]
        .into_iter()
        .filter_map(|(id, value)| value.as_ref().map(|v| (id.to_string(), v.clone())))
        .collect()
    }

    /// config id の値を書き換えた意図を返す（知らない id は `None` = 永続しない）。
    pub fn with_value(&self, config_id: &str, value: &str) -> Option<Self> {
        let mut next = self.clone();
        let slot = match config_id {
            "model" => &mut next.model,
            "thought_level" => &mut next.thought_level,
            "mode" => &mut next.mode,
            _ => return None,
        };
        *slot = Some(value.to_string());
        Some(next)
    }

    /// 選んである軸を engine の現在値に揃えた意図を返す（user の切替の後に使う）。
    ///
    /// 実測（agy 1.5.9）で、model を替えると thought_level が既定に戻ることがある（軸の無い
    /// model を経由した時）。registry が古い値のままだと、panel の表示（engine の値）と食い違い、
    /// 次の spawn の当て直しで「勝手に変わる」ように見える。選んでいない軸（None）は触らない。
    /// engine に軸が無い時は残す（model によって thought_level の軸ごと消える — 戻した時に使う）。
    pub fn follow_engine(&self, options: &[super::acp_config::AcpConfigOption]) -> Self {
        let follow = |id: &str, value: &Option<String>| -> Option<String> {
            let value = value.as_ref()?;
            match options.iter().find(|o| o.id == id) {
                Some(o) if !o.current_value.is_empty() => Some(o.current_value.clone()),
                _ => Some(value.clone()),
            }
        };
        Self {
            model: follow("model", &self.model),
            thought_level: follow("thought_level", &self.thought_level),
            mode: follow("mode", &self.mode),
        }
    }

    /// 永続前の形式検査。値は JSON-RPC で渡すので shell injection の経路は無いが、
    /// registry に壊れた値を残さない（engine に弾かれ続ける幻設定にしない）。
    pub fn validate_shape(&self) -> Result<(), String> {
        for (id, value) in self.config_pairs() {
            let ok = !value.is_empty()
                && value.len() <= MAX_VALUE_LEN
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if !ok {
                return Err(format!("agy の {id} の値が不正: {value:?}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(model: Option<&str>, level: Option<&str>, mode: Option<&str>) -> AgySettings {
        AgySettings {
            model: model.map(str::to_string),
            thought_level: level.map(str::to_string),
            mode: mode.map(str::to_string),
        }
    }

    /// model → thought_level → mode の順（level の候補は model で変わる — 実測の -32602）。
    /// 未選択の軸は出さない（engine 既定に委ねる）。
    #[test]
    fn config_pairs_apply_model_first_and_skip_unset() {
        let all = s(Some("claude-opus-5-5"), Some("high"), Some("plan"));
        assert_eq!(
            all.config_pairs(),
            vec![
                ("model".to_string(), "claude-opus-5-5".to_string()),
                ("thought_level".to_string(), "high".to_string()),
                ("mode".to_string(), "plan".to_string()),
            ]
        );
        assert_eq!(
            s(None, Some("low"), None).config_pairs(),
            vec![("thought_level".to_string(), "low".to_string())]
        );
        assert!(AgySettings::default().config_pairs().is_empty());
    }

    /// 知っている id だけ書き換える。他の軸は保つ。知らない id は None（永続しない）。
    #[test]
    fn with_value_updates_known_ids_only() {
        let base = s(Some("gemini-4-argon"), None, Some("plan"));
        assert_eq!(
            base.with_value("thought_level", "low"),
            Some(s(Some("gemini-4-argon"), Some("low"), Some("plan")))
        );
        assert_eq!(
            base.with_value("model", "gemini-3.8-flash"),
            Some(s(Some("gemini-3.8-flash"), None, Some("plan")))
        );
        assert_eq!(base.with_value("unknown_axis", "x"), None);
    }

    /// 形式検査: 実測の値（ハイフン / ドット入り）は通し、空・長すぎ・空白や quote 入りは弾く。
    #[test]
    fn validate_shape_accepts_measured_values_and_rejects_garbage() {
        assert!(
            s(
                Some("gemini-pro-orchestrator-g1-pro"),
                Some("medium"),
                Some("accept-edits")
            )
            .validate_shape()
            .is_ok()
        );
        assert!(
            s(Some("gemini-3.8-flash"), None, None)
                .validate_shape()
                .is_ok()
        );
        assert!(AgySettings::default().validate_shape().is_ok());
        assert!(s(Some(""), None, None).validate_shape().is_err());
        assert!(s(None, Some("high --x"), None).validate_shape().is_err());
        assert!(s(None, None, Some("plan'; rm")).validate_shape().is_err());
        assert!(
            s(Some(&"a".repeat(MAX_VALUE_LEN + 1)), None, None)
                .validate_shape()
                .is_err()
        );
    }

    /// 選んである軸は engine の現在値に揃える（model の切替で thought_level が既定に戻る、実測）。
    /// 選んでいない軸は触らない。engine に軸が無い時は残す（model によって軸ごと消える）。
    #[test]
    fn follow_engine_tracks_side_effects_of_a_switch() {
        let options: Vec<crate::conversation::acp_config::AcpConfigOption> =
            agent_chat_acp::config::parse_config_options(&serde_json::json!([
                {"id": "model", "type": "select", "currentValue": "gemini-3.8-flash",
                 "options": [{"value": "gemini-3.8-flash"}]},
                {"id": "thought_level", "type": "select", "currentValue": "medium",
                 "options": [{"value": "low"}, {"value": "medium"}]}
            ]))
            .unwrap()
            .into_iter()
            .map(Into::into)
            .collect();
        assert_eq!(
            s(Some("gemini-3.8-flash"), Some("low"), Some("plan")).follow_engine(&options),
            s(Some("gemini-3.8-flash"), Some("medium"), Some("plan")),
            "thought_level は engine の medium に揃う / mode は軸が無いので残す"
        );
        assert_eq!(
            s(Some("gemini-3.8-flash"), None, None).follow_engine(&options),
            s(Some("gemini-3.8-flash"), None, None),
            "選んでいない軸は None のまま（engine 既定に委ねる）"
        );
    }

    /// TS 型（webview の agy panel が組む `{agy: {...}}` の形）を export する。
    #[test]
    fn export_agy_settings_ts() {
        <AgySettings as TS>::export_all(&ts_rs::Config::from_env())
            .expect("AgySettings の TS export 失敗");
    }
}
