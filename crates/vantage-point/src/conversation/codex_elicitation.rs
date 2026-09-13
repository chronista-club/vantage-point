//! MCP forms are rendered from a bounded schema; only validated typed content is returned.
use serde_json::{Value, json};

use super::event::{CodexElicitation, CodexElicitationField, CodexElicitationOption};

pub(super) struct Elicitation {
    pub view: CodexElicitation,
    validator: Option<jsonschema::Validator>,
}

pub(super) fn response(action: &str, content: Value) -> Value {
    json!({"action":action,"content":content,"_meta":null})
}

impl Elicitation {
    pub fn parse(params: &Value) -> Result<Self, String> {
        let mut view = CodexElicitation {
            server_name: params["serverName"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("MCP サーバー名がありません")?
                .into(),
            message: params["message"].as_str().unwrap_or_default().into(),
            url: None,
            fields: Vec::new(),
        };
        match params["mode"].as_str() {
            Some("url") => {
                let url = params["url"].as_str().ok_or("手続きの URL がありません")?;
                let parsed = url::Url::parse(url).map_err(|_| "不正な手続き URL です")?;
                if !matches!(parsed.scheme(), "http" | "https")
                    || parsed.host_str().is_none()
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                {
                    return Err("この手続き URL は開けません".into());
                }
                view.url = Some(url.into());
                Ok(Self {
                    view,
                    validator: None,
                })
            }
            Some("form" | "openai/form" | "openaiForm") => {
                let schema = &params["requestedSchema"];
                keys(
                    schema,
                    &[
                        "$schema",
                        "type",
                        "properties",
                        "required",
                        "title",
                        "description",
                        "additionalProperties",
                    ],
                )?;
                if schema["type"] != "object" {
                    return Err("MCP フォームは object 形式に対応しています".into());
                }
                let properties = schema["properties"]
                    .as_object()
                    .filter(|p| p.len() <= 32)
                    .ok_or("フォームの項目が不正か多すぎます")?;
                let required: Vec<String> = match schema.get("required") {
                    Some(value) => {
                        serde_json::from_value(value.clone()).map_err(|_| "必須項目が不正です")?
                    }
                    None => Vec::new(),
                };
                if required.iter().any(|name| !properties.contains_key(name)) {
                    return Err("表示できない必須項目があります".into());
                }
                for (name, field) in properties {
                    view.fields
                        .push(parse_field(name, field, required.contains(name))?);
                }
                // External references are unsupported by the UI and disabled at the dependency level.
                let validator = jsonschema::draft202012::options()
                    .should_validate_formats(true)
                    .should_ignore_unknown_formats(false)
                    .build(schema)
                    .map_err(|_| "フォームの制約が不正または未対応です")?;
                Ok(Self {
                    view,
                    validator: Some(validator),
                })
            }
            _ => Err(
                "この MCP 手続きの形式にはまだ対応していません。辞退またはキャンセルできます。"
                    .into(),
            ),
        }
    }

    pub fn accept(&self, answers: Option<&Value>) -> Result<Value, String> {
        let answers = answers
            .and_then(Value::as_object)
            .ok_or("回答がありません")?;
        if answers
            .keys()
            .any(|key| !self.view.fields.iter().any(|f| f.id == *key) && key != "action")
        {
            return Err("フォームにない項目が含まれています".into());
        }
        if answers.get("action").is_some_and(|v| v != "accept") {
            return Err("不正な MCP 応答です".into());
        }
        if self.view.url.is_some() {
            if answers.get("action") != Some(&json!("accept")) {
                return Err("手続きの完了を確認してください".into());
            }
            return Ok(response("accept", Value::Null));
        }
        let mut content = serde_json::Map::new();
        for field in &self.view.fields {
            let Some(value) = answers.get(&field.id) else {
                continue;
            };
            let raw = value
                .as_str()
                .filter(|v| v.len() <= 64 * 1024)
                .ok_or("回答の形式または長さが不正です")?;
            let value = if field.kind == "string" {
                json!(raw)
            } else {
                serde_json::from_str(raw)
                    .map_err(|_| format!("{} の入力形式を確認してください", field.title))?
            };
            content.insert(
                field
                    .id
                    .strip_prefix("field:")
                    .expect("server-owned field id")
                    .into(),
                value,
            );
        }
        let content = Value::Object(content);
        self.validator.as_ref().ok_or("検証できないフォームです")?.validate(&content)
            .map_err(|error| format!("回答がフォームの条件を満たしていません（{}）。必須項目・形式・範囲を確認してください。", error.instance_path()))?;
        Ok(response("accept", content))
    }
}

fn parse_field(
    name: &str,
    schema: &Value,
    required: bool,
) -> Result<CodexElicitationField, String> {
    let kind = schema["type"]
        .as_str()
        .ok_or("フォームの項目に型がありません")?;
    let mut allowed = vec!["type", "title", "description", "default"];
    allowed.extend_from_slice(match kind {
        "string" => &[
            "minLength",
            "maxLength",
            "format",
            "enum",
            "enumNames",
            "oneOf",
        ],
        "number" | "integer" => &["minimum", "maximum"],
        "boolean" => &[],
        "array" => &["items", "minItems", "maxItems", "uniqueItems"],
        _ => return Err("入れ子など、表示できないフォーム項目があります".into()),
    });
    keys(schema, &allowed)?;
    let options = if kind == "array" {
        keys(&schema["items"], &["type", "enum", "anyOf"])?;
        options(&schema["items"], "anyOf")?
    } else {
        options(schema, "oneOf")?
    };
    if kind == "array" && options.is_empty() {
        return Err("配列は選択肢のある項目に対応しています".into());
    }
    let mut description = schema["description"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    for (key, label) in [
        ("minimum", "最小値"),
        ("maximum", "最大値"),
        ("minLength", "最小文字数"),
        ("maxLength", "最大文字数"),
        ("minItems", "最小選択数"),
        ("maxItems", "最大選択数"),
        ("format", "形式"),
    ] {
        if let Some(value) = schema.get(key) {
            description.push_str(&format!(
                "\n{label}: {}",
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            ));
        }
    }
    let default_value = schema
        .get("default")
        .map(|v| {
            if kind == "string" {
                v.as_str().map(str::to_owned).ok_or("初期値の型が不正です")
            } else {
                Ok(v.to_string())
            }
        })
        .transpose()?;
    Ok(CodexElicitationField {
        id: format!("field:{name}"),
        title: schema["title"].as_str().unwrap_or(name).into(),
        description,
        kind: kind.into(),
        required,
        options,
        default_value,
    })
}

fn options(schema: &Value, titled_key: &str) -> Result<Vec<CodexElicitationOption>, String> {
    if schema.get("enum").is_some() && schema.get(titled_key).is_some() {
        return Err("複数の選択肢形式が指定されています".into());
    }
    let mut result = Vec::new();
    if let Some(values) = schema.get("enum") {
        let values = values
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 128)
            .ok_or("選択肢が不正か多すぎます")?;
        let names = schema
            .get("enumNames")
            .map(|v| {
                v.as_array()
                    .filter(|a| a.len() == values.len())
                    .ok_or("選択肢の表示名が不正です")
            })
            .transpose()?;
        for (i, value) in values.iter().enumerate() {
            let value = value.as_str().ok_or("選択肢は文字列に対応しています")?;
            let label = names
                .map(|n| n[i].as_str().ok_or("選択肢の表示名が不正です"))
                .transpose()?
                .unwrap_or(value);
            result.push(CodexElicitationOption {
                value: value.into(),
                label: label.into(),
            });
        }
    } else if let Some(values) = schema.get(titled_key) {
        for value in values
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 128)
            .ok_or("選択肢が不正か多すぎます")?
        {
            keys(value, &["const", "title"])?;
            let text = value["const"]
                .as_str()
                .ok_or("選択肢は文字列に対応しています")?;
            result.push(CodexElicitationOption {
                value: text.into(),
                label: value["title"].as_str().unwrap_or(text).into(),
            });
        }
    }
    Ok(result)
}

fn keys(value: &Value, allowed: &[&str]) -> Result<(), String> {
    if value
        .as_object()
        .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
    {
        return Err("未対応のフォーム制約が含まれています".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_form_constraints_and_enum_values_are_preserved() {
        let params = json!({"serverName":"test","mode":"openai/form","message":"設定",
            "requestedSchema":{"type":"object","properties":{
                "choice":{"type":"string","oneOf":[{"const":"raw-a","title":"表示 A"},{"const":"raw-b","title":"表示 B"}]},
                "tags":{"type":"array","minItems":1,"maxItems":2,"items":{"anyOf":[{"const":"x","title":"X"},{"const":"y","title":"Y"}]}},
                "email":{"type":"string","format":"email","maxLength":100}},"required":["choice","tags","email"]}});
        let form = Elicitation::parse(&params).unwrap();
        let invalid = json!({"field:choice":"raw-a","field:tags":"[\"x\"]","field:email":"secret-invalid-email"});
        let error = form.accept(Some(&invalid)).unwrap_err();
        assert!(!error.contains("secret-invalid-email"));
        let result = form.accept(Some(&json!({"field:choice":"raw-b","field:tags":"[\"x\",\"y\"]","field:email":"a@example.com"}))).unwrap();
        assert_eq!(
            result["content"],
            json!({"choice":"raw-b","tags":["x","y"],"email":"a@example.com"})
        );
        assert!(
            form.accept(Some(
                &json!({"field:choice":"表示 A","field:tags":"[]","field:email":"a@example.com"})
            ))
            .is_err()
        );
        let mut unsupported = params;
        unsupported["requestedSchema"]["properties"]["email"] =
            json!({"$ref":"file:///private/secret"});
        assert!(Elicitation::parse(&unsupported).is_err());
    }
}
