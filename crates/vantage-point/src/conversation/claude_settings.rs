//! Claude の settings — VP が spawn 時に注入する指定（Claude が所有する語彙）。
//!
//! 共有側（registry / RPC）は [`super::settings::EngineSettings::Claude`] の variant として
//! これを運ぶだけ。中身の意味（`--model` の語彙）は本 module が閉じる。

#[cfg(test)]
use ts_rs::TS;

/// Claude の「次にどう走らせるか」。全 field が `None` = engine 既定に委譲（何も注入しない）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct ClaudeSettings {
    /// `--model` に渡す id。None = engine 既定（注入しない）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub model: Option<String>,
}

impl ClaudeSettings {
    /// 永続前の形式検査（`--model` 引数への injection 防壁）。
    pub fn validate_shape(&self) -> Result<(), String> {
        if let Some(m) = &self.model
            && !crate::lane::engine_model::is_valid_model(m)
        {
            return Err(format!("Claude の model 名が不正: {m:?}"));
        }
        Ok(())
    }
}
