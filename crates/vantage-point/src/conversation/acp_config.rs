//! ACP Session Config Options の解釈（doc 88 §6）— engine が出す「切り替えられる軸と候補」。
//!
//! ACP の `session/new` / `session/load` / `session/set_config_option` の result と
//! `session/update` の `config_option_update` は、どれも `configOptions` 配列の**全体**を運ぶ
//! （実測 agy 1.5.9、2026-11-17）:
//!
//! ```json
//! {"id": "thought_level", "name": "Thinking Level", "category": "thought_level",
//!  "type": "select", "currentValue": "medium",
//!  "options": [{"value": "low", "name": "Low"}, {"value": "medium", "name": "Medium"}]}
//! ```
//!
//! 本 module はそれを VP の型（[`AcpConfigOption`]）へ写す純関数と、registry の意図のうち
//! 当て直しが要るものを選ぶ計算だけを持つ（data / calculations）。送信と状態は
//! [`super::acp_host`]、engine 別の意図の形は [`super::agy_settings`] が持つ。
//!
//! 候補の表は**engine が正**（VP は固定の表を持たない）。roster（`LaneSessionView.acp_config`）で
//! GUI に運び、engine 別の panel が並べる。

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
}

/// `configOptions` 配列を写す（純関数）。配列でなければ `None`。
///
/// - `type` が `select` 以外の軸は捨てる（VP は select しか描けない）
/// - 候補の group（`{name, options: [...]}`）は平らにする（ACP の grouped options）
/// - `value` の無い候補は捨てる。`name` が無ければ value で代用
pub fn parse_config_options(value: &serde_json::Value) -> Option<Vec<AcpConfigOption>> {
    let arr = value.as_array()?;
    Some(
        arr.iter()
            .filter(|opt| opt.get("type").and_then(|v| v.as_str()) == Some("select"))
            .filter_map(|opt| {
                let id = opt.get("id").and_then(|v| v.as_str())?.to_string();
                let name = opt
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&id)
                    .to_string();
                let current_value = opt
                    .get("currentValue")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let mut choices = Vec::new();
                for item in opt
                    .get("options")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    match item.get("options").and_then(|v| v.as_array()) {
                        // group: 中の候補を平らに並べる
                        Some(group) => choices.extend(group.iter().filter_map(parse_choice)),
                        None => choices.extend(parse_choice(item)),
                    }
                }
                Some(AcpConfigOption {
                    id,
                    name,
                    current_value,
                    choices,
                })
            })
            .collect(),
    )
}

/// 候補 1 件を写す（value が無ければ捨てる）。
fn parse_choice(item: &serde_json::Value) -> Option<AcpConfigChoice> {
    let value = item.get("value").and_then(|v| v.as_str())?.to_string();
    let name = item
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&value)
        .to_string();
    let description = item
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(AcpConfigChoice {
        value,
        name,
        description,
    })
}

/// `session/update` の `config_option_update` から `configOptions` を写す（純関数）。
pub fn parse_config_option_update(update: &serde_json::Value) -> Option<Vec<AcpConfigOption>> {
    if update.get("sessionUpdate").and_then(|v| v.as_str()) != Some("config_option_update") {
        return None;
    }
    parse_config_options(update.get("configOptions")?)
}

/// registry の意図の 1 軸を engine に当てる必要があるか（純関数）。
///
/// 当て直しは **1 軸ずつ、直前の応答で更新した configOptions で**判断する（host の
/// `AcpState::next_reapply`）。実測（agy 1.5.9）で、model を替えると thought_level の軸が
/// 消えたり（orchestrator / gpt-oss）、値が既定に戻ったりするため、先に全体を計画すると外れる。
///
/// - engine が出していない軸は当てない（model によって軸が無い / agy が軸を減らした）
/// - 現在値と同じなら当てない
/// - 候補に無い値でも当てる（engine に判断させ、弾かれたら error に残す）
pub fn needs_apply(options: &[AcpConfigOption], config_id: &str, value: &str) -> bool {
    options
        .iter()
        .find(|o| o.id == config_id)
        .is_some_and(|o| o.current_value != value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 実測 wire（agy 1.5.9 の session/new result の configOptions、候補は一部省略）。
    fn measured() -> serde_json::Value {
        serde_json::json!([
            {"id": "model", "name": "Model", "category": "model", "type": "select",
             "currentValue": "gemini-4-argon",
             "options": [{"value": "gemini-4-argon", "name": "Gemini 4 Argon"},
                         {"value": "claude-opus-5-5", "name": "Claude Opus 5.5"}]},
            {"id": "thought_level", "name": "Thinking Level", "category": "thought_level",
             "type": "select", "currentValue": "medium",
             "options": [{"value": "low", "name": "Low"}, {"value": "medium", "name": "Medium"},
                         {"value": "high", "name": "High"}]},
            {"id": "mode", "name": "Mode", "category": "mode", "type": "select",
             "currentValue": "default",
             "options": [{"value": "default", "name": "Default",
                          "description": "Ask before edits and commands, per your permission settings."},
                         {"value": "plan", "name": "Plan",
                          "description": "Plan carefully before executing a task."}]}
        ])
    }

    #[test]
    fn parses_measured_agy_config_options() {
        let opts = parse_config_options(&measured()).expect("配列");
        assert_eq!(
            opts.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            ["model", "thought_level", "mode"]
        );
        assert_eq!(opts[1].name, "Thinking Level");
        assert_eq!(opts[1].current_value, "medium");
        assert_eq!(
            opts[1]
                .choices
                .iter()
                .map(|c| c.value.as_str())
                .collect::<Vec<_>>(),
            ["low", "medium", "high"]
        );
        assert_eq!(
            opts[2].choices[1].description.as_deref(),
            Some("Plan carefully before executing a task.")
        );
        assert_eq!(opts[0].choices[1].name, "Claude Opus 5.5");
    }

    /// ACP の grouped options は平らにする。select 以外の軸・value の無い候補は捨てる。
    /// name 欠落は value で代用。配列でなければ None。
    #[test]
    fn flattens_groups_and_drops_unrenderable() {
        let raw = serde_json::json!([
            {"id": "model", "type": "select", "currentValue": "b",
             "options": [{"name": "G1", "options": [{"value": "a", "name": "A"}, {"value": "b"}]},
                         {"name": "no value"}]},
            {"id": "flag", "type": "boolean", "currentValue": true}
        ]);
        let opts = parse_config_options(&raw).expect("配列");
        assert_eq!(opts.len(), 1, "boolean の軸は捨てる");
        assert_eq!(opts[0].name, "model", "name 欠落は id で代用");
        assert_eq!(
            opts[0]
                .choices
                .iter()
                .map(|c| (c.value.as_str(), c.name.as_str()))
                .collect::<Vec<_>>(),
            [("a", "A"), ("b", "b")]
        );
        assert_eq!(parse_config_options(&serde_json::json!({})), None);
    }

    #[test]
    fn parses_config_option_update_notification() {
        let update = serde_json::json!({
            "sessionUpdate": "config_option_update",
            "configOptions": measured()
        });
        assert_eq!(
            parse_config_option_update(&update).map(|o| o.len()),
            Some(3)
        );
        let other = serde_json::json!({"sessionUpdate": "usage_update", "used": 1, "size": 2});
        assert_eq!(parse_config_option_update(&other), None);
    }

    /// 当てる要否: engine に無い軸・現在値と同じものは当てない。候補に無い値は当てる
    /// （engine に判断させる）。
    #[test]
    fn needs_apply_skips_missing_axes_and_equal_values() {
        let opts = parse_config_options(&measured()).unwrap();
        assert!(needs_apply(&opts, "model", "claude-opus-5-5"));
        assert!(
            !needs_apply(&opts, "thought_level", "medium"),
            "現在値と同じ"
        );
        assert!(!needs_apply(&opts, "gone_axis", "x"), "engine に無い軸");
        assert!(
            needs_apply(&opts, "thought_level", "xhigh"),
            "候補に無い値も当てる"
        );
    }

    #[test]
    fn export_acp_config_ts() {
        <AcpConfigView as TS>::export_all(&ts_rs::Config::from_env())
            .expect("AcpConfigView の TS export 失敗");
    }
}
