//! Claude の settings — VP が spawn 時に注入する指定（Claude が所有する語彙）。
//!
//! 共有側（registry / RPC）は [`super::settings::EngineSettings::Claude`] の variant として
//! これを運ぶだけ。中身の意味（`--model` / `--effort` の語彙）は本 module が閉じる。
//! spawn への注入は gui host（`host.rs`）と tui（`agent_spawner::claude_command`）の 2 経路が
//! あり、どちらも [`ClaudeSettings::flag_pairs`] を並べるだけ（語彙の表を 2 か所に持たない）。

use super::engine::Choice;
#[cfg(test)]
use ts_rs::TS;

/// `--effort` が受ける値（`claude --help` 2026-09-21: low, medium, high, xhigh, max）。
/// 注入は shell 文字列にも乗るので、この表に無い値は書かない（injection 防壁）。
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Claude の「次にどう走らせるか」。全 field が `None` = engine 既定に委譲（何も注入しない）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct ClaudeSettings {
    /// `--model` に渡す id。None = engine 既定（注入しない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub model: Option<String>,
    /// `--effort` に渡す段。None = engine 既定（注入しない）。値は [`EFFORTS`] のいずれか。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub effort: Option<String>,
}

impl ClaudeSettings {
    /// 永続前の形式検査（`--model` / `--effort` 引数への injection 防壁）。
    pub fn validate_shape(&self) -> Result<(), String> {
        if let Some(m) = &self.model
            && !crate::lane::engine_model::is_valid_model(m)
        {
            return Err(format!("Claude の model 名が不正: {m:?}"));
        }
        if let Some(e) = &self.effort
            && !EFFORTS.contains(&e.as_str())
        {
            return Err(format!(
                "Claude の effort が不正: {e:?}（{}）",
                EFFORTS.join(" / ")
            ));
        }
        Ok(())
    }

    /// spawn に注入する `(flag, value)` の列。**形式外の値は黙って落とす**（validate_shape を
    /// 通っていない古い file / 手編集を spawn 時にも弾く二重防壁）。順序は model → effort。
    pub fn flag_pairs(&self) -> Vec<(&'static str, &str)> {
        let mut pairs = Vec::new();
        if let Some(m) = self
            .model
            .as_deref()
            .filter(|m| crate::lane::engine_model::is_valid_model(m))
        {
            pairs.push(("--model", m));
        }
        if let Some(e) = self.effort.as_deref().filter(|e| EFFORTS.contains(e)) {
            pairs.push(("--effort", e));
        }
        pairs
    }

    /// effort picker の選択肢（先頭 "" = Default = 注入しない）。model と同じ catalog 駆動 —
    /// client は並べるだけ。
    pub fn effort_choices() -> Vec<Choice> {
        std::iter::once(Choice {
            value: String::new(),
            label: "Default".to_string(),
        })
        .chain(EFFORTS.iter().map(|e| Choice {
            value: (*e).to_string(),
            label: (*e).to_string(),
        }))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_pairs_follow_the_settings_and_drop_invalid_values() {
        let s = ClaudeSettings {
            model: Some("claude-sonnet-5".into()),
            effort: Some("high".into()),
        };
        assert_eq!(
            s.flag_pairs(),
            vec![("--model", "claude-sonnet-5"), ("--effort", "high")]
        );
        assert!(ClaudeSettings::default().flag_pairs().is_empty());
        let bad = ClaudeSettings {
            model: Some("opus --dangerous".into()),
            effort: Some("ultra".into()),
        };
        assert!(bad.flag_pairs().is_empty(), "形式外は注入しない");
        assert!(bad.validate_shape().is_err());
    }

    #[test]
    fn effort_choices_are_default_plus_the_cli_vocabulary() {
        let values: Vec<String> = ClaudeSettings::effort_choices()
            .into_iter()
            .map(|c| c.value)
            .collect();
        assert_eq!(values, ["", "low", "medium", "high", "xhigh", "max"]);
    }
}
