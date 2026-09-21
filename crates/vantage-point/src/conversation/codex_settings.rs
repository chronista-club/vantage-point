//! Codex の settings — model / effort の組合せ（Codex が所有する語彙）。
//!
//! 候補は app-server の `model/list` から動的に引く（モデル名の固定表は持たない）。
//! 共有側（registry / RPC）は [`super::settings::EngineSettings::Codex`] の variant として
//! これを運ぶだけで、中身の意味（effort の語彙、model ごとの候補）は本 module が閉じる。

#[cfg(test)]
use ts_rs::TS;

/// Codex の「次の送信を何で走らせるか」— model と effort の組。
/// effort の値は app-server が名乗る文字列そのもの（VP 側に列挙は持たない）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct CodexSelection {
    pub model: String,
    pub effort: String,
}

impl CodexSelection {
    /// 永続前の形式検査（`is_valid_model` は `--model` 引数系と同じ injection 防壁）。
    pub fn validate_shape(&self) -> Result<(), String> {
        if !crate::lane::engine_model::is_valid_model(&self.model) {
            return Err(format!("Codex の model 名が不正: {:?}", self.model));
        }
        if self.effort.is_empty() || self.effort.len() > 64 {
            return Err(format!("Codex の effort が不正: {:?}", self.effort));
        }
        Ok(())
    }
}

/// `model/list` の 1 候補（GUI の picker に並ぶ形）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct CodexModel {
    pub model: String,
    pub label: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
}

pub(super) fn parse_page(
    value: &serde_json::Value,
) -> Result<(Vec<CodexModel>, Option<String>), String> {
    let data = value["data"]
        .as_array()
        .ok_or("model/list の候補がありません")?;
    if data.len() > 256 {
        return Err("model/list の候補が上限を超えています".into());
    }
    let mut models = Vec::new();
    for item in data {
        if item["hidden"].as_bool() == Some(true) {
            continue;
        }
        let text = |key: &str| -> Result<String, String> {
            item[key]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 512)
                .map(str::to_owned)
                .ok_or_else(|| format!("model/list の {key} が不正です"))
        };
        let model = text("model")?;
        if !crate::lane::engine_model::is_valid_model(&model) {
            continue;
        }
        let label = text("displayName")?;
        let default_effort = text("defaultReasoningEffort")?;
        let values = item["supportedReasoningEfforts"]
            .as_array()
            .ok_or("effort の候補がありません")?;
        if values.len() > 32 {
            return Err("effort の候補が上限を超えています".into());
        }
        let mut efforts = Vec::new();
        for value in values {
            let effort = value["reasoningEffort"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 64)
                .ok_or("effort の候補が不正です")?
                .to_owned();
            if !efforts.contains(&effort) {
                efforts.push(effort);
            }
        }
        if !efforts.contains(&default_effort) {
            return Err("既定 effort が候補に含まれていません".into());
        }
        models.push(CodexModel {
            model,
            label,
            efforts,
            default_effort,
        });
    }
    let cursor = match value.get("nextCursor") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) if !s.is_empty() && s.len() <= 4096 => Some(s.clone()),
        _ => return Err("model/list の cursor が不正です".into()),
    };
    Ok((models, cursor))
}

pub(super) fn validate(models: &[CodexModel], selection: &CodexSelection) -> Result<(), String> {
    let model = models
        .iter()
        .find(|m| m.model == selection.model)
        .ok_or("選択したモデルを現在の候補で確認できません。モデルを選び直してください。")?;
    if !model.efforts.contains(&selection.effort) {
        return Err("選択したモデルではその effort を使えません。選び直してください。".into());
    }
    Ok(())
}
