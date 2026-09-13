//! Independent permission grants: display server-owned choices, never trust client profiles.
use serde::Deserialize;
use serde_json::{Value, json};

use super::event::CodexQuestion;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Profile {
    network: Option<Network>,
    file_system: Option<Files>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Network {
    enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Files {
    read: Option<Vec<String>>,
    write: Option<Vec<String>>,
    entries: Option<Vec<Value>>,
    glob_scan_max_depth: Option<u32>,
}

pub(super) struct Permissions {
    choices: Vec<(String, Value)>,
}

impl Permissions {
    pub fn parse(value: &Value) -> Result<Self, String> {
        let profile: Profile =
            serde_json::from_value(value.clone()).map_err(|_| "未対応または不正な権限形式です")?;
        let mut choices = Vec::new();
        if profile.network.is_some_and(|n| n.enabled == Some(true)) {
            choices.push((
                "ネットワークへの接続".into(),
                json!({"network":{"enabled":true}}),
            ));
        }
        if let Some(files) = profile.file_system {
            let mut labels = Vec::new();
            for (key, title, paths) in [
                ("read", "読み取り", &files.read),
                ("write", "書き込み", &files.write),
            ] {
                for path in paths.iter().flatten() {
                    if path.trim().is_empty() {
                        return Err("権限のパスが空です".into());
                    }
                    let label = format!("{title}: {path}");
                    labels.push(label.clone());
                    if files.entries.is_none() {
                        let mut grant = json!({"fileSystem":{"read":null,"write":null}});
                        grant["fileSystem"][key] = json!([path]);
                        choices.push((label, grant));
                    }
                }
            }
            if let Some(entries) = &files.entries {
                for entry in entries {
                    labels.push(entry_label(entry)?);
                }
                if let Some(depth) = files.glob_scan_max_depth {
                    labels.push(format!("パターン検索の最大深さ: {depth}"));
                }
                if !labels.is_empty() {
                    choices.push((
                        format!("ファイル権限（一組のルール）\n{}", labels.join("\n")),
                        json!({"fileSystem":value["fileSystem"]}),
                    ));
                }
            } else if files.glob_scan_max_depth.is_some() {
                return Err("パターン検索の設定には entries が必要です".into());
            }
        }
        if choices.is_empty() || choices.len() > 64 {
            return Err("選択可能な権限がないか、表示上限を超えています".into());
        }
        Ok(Self { choices })
    }

    pub fn questions(&self) -> Vec<CodexQuestion> {
        self.choices
            .iter()
            .enumerate()
            .map(|(i, (label, _))| (format!("p{i}"), label.clone()))
            .chain(std::iter::once(("scope".into(), "許可の期間".into())))
            .map(|(id, question)| CodexQuestion {
                id,
                question,
                header: String::new(),
                options: Vec::new(),
                is_secret: false,
            })
            .collect()
    }

    pub fn response(&self, answers: Option<&Value>) -> Result<Value, String> {
        let input = answers
            .and_then(Value::as_object)
            .ok_or("権限の選択がありません")?;
        let scope = input.get("scope").and_then(Value::as_str).unwrap_or("turn");
        if !matches!(scope, "turn" | "session")
            || input.get("scope").is_some_and(|v| !v.is_string())
        {
            return Err("不正な許可の期間です".into());
        }
        for (key, value) in input {
            if key != "scope"
                && (!(0..self.choices.len()).any(|i| *key == format!("p{i}"))
                    || !matches!(value.as_str(), Some("allow" | "deny")))
            {
                return Err("要求にない権限または不正な選択です".into());
            }
        }
        let mut grants = json!({});
        for (i, (_, grant)) in self.choices.iter().enumerate() {
            if input.get(&format!("p{i}")).and_then(Value::as_str) != Some("allow") {
                continue;
            }
            if let Some(network) = grant.get("network") {
                grants["network"] = network.clone();
            }
            if let Some(files) = grant.get("fileSystem") {
                if grants.get("fileSystem").is_none() {
                    grants["fileSystem"] = json!({"read":null,"write":null});
                }
                for (key, value) in files.as_object().expect("server-owned file grant") {
                    if let Some(paths) = value.as_array() {
                        if grants["fileSystem"][key].is_null() {
                            grants["fileSystem"][key] = json!([]);
                        }
                        grants["fileSystem"][key]
                            .as_array_mut()
                            .expect("grant array")
                            .extend(paths.clone());
                    } else if !value.is_null() {
                        grants["fileSystem"][key] = value.clone();
                    }
                }
            }
        }
        if grants.as_object().is_none_or(|g| g.is_empty()) {
            return Err("許可する項目を選んでください".into());
        }
        Ok(json!({"permissions":grants,"scope":scope}))
    }
}

fn entry_label(entry: &Value) -> Result<String, String> {
    known_fields(entry, &["access", "path"])?;
    let access = match entry["access"].as_str() {
        Some("read") => "読み取り",
        Some("write") => "書き込み",
        Some("deny") => "アクセス禁止",
        _ => return Err("未対応のファイルアクセス形式です".into()),
    };
    let path = &entry["path"];
    known_fields(
        path,
        match path["type"].as_str() {
            Some("path") => &["type", "path"],
            Some("glob_pattern") => &["type", "pattern"],
            Some("special") => &["type", "value"],
            _ => return Err("未対応のパス形式です".into()),
        },
    )?;
    let label = match path["type"].as_str() {
        Some("path") => path["path"].as_str().map(str::to_owned),
        Some("glob_pattern") => path["pattern"].as_str().map(|p| format!("パターン {p}")),
        Some("special") => {
            let value = &path["value"];
            known_fields(
                value,
                if value["kind"] == "project_roots" {
                    &["kind", "subpath"]
                } else {
                    &["kind"]
                },
            )?;
            let name = match value["kind"].as_str() {
                Some("root") => "ファイルシステム全体",
                Some("minimal") => "実行に必要な最小範囲",
                Some("project_roots") => "プロジェクトのルート",
                Some("tmpdir") => "一時ディレクトリ",
                Some("slash_tmp") => "/tmp",
                _ => return Err("未対応の特殊パスです".into()),
            };
            Some(match value.get("subpath").filter(|v| !v.is_null()) {
                Some(subpath) => {
                    format!("{name} / {}", subpath.as_str().ok_or("不正な相対パスです")?)
                }
                None => name.into(),
            })
        }
        _ => None,
    }
    .filter(|s| !s.trim().is_empty())
    .ok_or("未対応または空の権限パスです")?;
    Ok(format!("{access}: {label}"))
}

fn known_fields(value: &Value, keys: &[&str]) -> Result<(), String> {
    if value
        .as_object()
        .is_none_or(|o| o.keys().any(|k| !keys.contains(&k.as_str())))
    {
        return Err("未対応の権限フィールドが含まれています".into());
    }
    Ok(())
}
