//! Codex が公開する model / effort の組合せを扱う。モデル名の固定表は持たない。

use super::event::{CodexModel, CodexSelection};

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
