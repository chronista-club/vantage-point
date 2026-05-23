//! Claude Code の per-project trust state を pre-grant する。
//!
//! `vp lane new` (= wing 環境作成) のとき、 同じ dir で `claude` を起動した際の
//! folder trust dialog (`Quick safety check: Is this a project you trust?`) を
//! スキップさせる。 VP-managed lane は信頼領域なので毎回 user 承認は冗長。
//!
//! ## storage 仕様 (実証 2026-05-24)
//!
//! claude binary の `setPathTrusted` / `checkHasTrustDialogAccepted` は
//! **`~/.claude.json`** (= `$HOME/.claude.json`、 `~/.claude/` directory ではない)
//! の `projects[<absolute-path>].hasTrustDialogAccepted` を真偽値として参照する。
//! key は **canonical 絶対 path** (= macOS なら `fs::canonicalize` 後の `/private/...`
//! 等が必要、 symlink を解決した形)。
//!
//! `~/.claude/settings.json` の `trustedDirectories` は permission 系の別 list で、
//! folder trust dialog とは無関係 (実証で空振り確定)。
//!
//! ## concurrent write
//!
//! claude 自身も `~/.claude.json` を頻繁に書く (GH #3117 で concurrent write の
//! wipe bug が既知)。 競合を完全排除はできないが、 write を **tempfile + atomic
//! rename** で行うことで partial write による file 破壊を回避する。
//!
//! ## best-effort
//!
//! trust pre-grant が失敗しても wing 作成全体は失敗にしない (= 旧挙動で dialog が
//! 出るだけ)。 caller は `Err` を warn として表示するに留める。
//!
//! ## `vp lane rm` 側で cleanup しない理由
//!
//! lane dir が消えれば同じ path で claude を起動する経路自体が無くなるので、
//! 残った entry は無害 (claude も読まない)。 cleanup は ~/.claude.json への
//! 追加 write を必要とし concurrent write リスクを増やすだけで利得が無いため
//! 意図的に省略。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// `~/.claude.json` の場所を返す。
fn claude_json_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME 環境変数が未設定です".to_string())?;
    Ok(PathBuf::from(home).join(".claude.json"))
}

/// claude が新規 project entry に書く既定 key 群 (`hasTrustDialogAccepted: true` 含む)。
///
/// 実証時に観測した実在 entry を mirror。 過不足 key があっても claude 側は
/// missing key を default 扱いするので壊れない。
fn default_project_entry() -> Value {
    serde_json::json!({
        "allowedTools": [],
        "mcpContextUris": [],
        "mcpServers": {},
        "enabledMcpjsonServers": [],
        "disabledMcpjsonServers": [],
        "hasTrustDialogAccepted": true,
        "projectOnboardingSeenCount": 0,
        "hasClaudeMdExternalIncludesApproved": false,
        "hasClaudeMdExternalIncludesWarningShown": false,
    })
}

/// pure: parsed `~/.claude.json` の値と path key を受け取り、 trust 済 entry を
/// merge した新しい値を返す。
///
/// - 既存 entry あり → `hasTrustDialogAccepted` のみ `true` で上書き、 他 key 保持
/// - entry なし → [`default_project_entry`] を入れる
///
/// I/O を持たないので unit test 可能。
pub(crate) fn merge_trust_accepted(mut config: Value, key: &str) -> Result<Value, String> {
    let root = config
        .as_object_mut()
        .ok_or_else(|| "~/.claude.json の root が object ではありません".to_string())?;
    let projects = root
        .entry("projects".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !projects.is_object() {
        return Err("~/.claude.json の projects が object ではありません".to_string());
    }
    let projects_obj = projects.as_object_mut().unwrap();
    match projects_obj.get_mut(key) {
        Some(entry) if entry.is_object() => {
            entry
                .as_object_mut()
                .unwrap()
                .insert("hasTrustDialogAccepted".to_string(), Value::Bool(true));
        }
        _ => {
            projects_obj.insert(key.to_string(), default_project_entry());
        }
    }
    Ok(config)
}

/// `wing_dir` を claude の trust 済 project として `~/.claude.json` に登録する。
///
/// best-effort: 失敗しても呼び元 (`setup_wing` 等) を失敗にしない想定。
/// caller は `Err` を warn として表示する。
pub fn pre_grant_trust(wing_dir: &Path) -> Result<(), String> {
    let canonical = fs::canonicalize(wing_dir)
        .map_err(|e| format!("canonicalize {}: {e}", wing_dir.display()))?;
    let key = canonical
        .to_str()
        .ok_or_else(|| {
            format!(
                "canonical path が UTF-8 ではありません: {}",
                canonical.display()
            )
        })?
        .to_string();

    let path = claude_json_path()?;

    // 既存 ~/.claude.json を read (無ければ empty object から start)。
    let config: Value = if path.exists() {
        let bytes = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?
    } else {
        Value::Object(serde_json::Map::new())
    };

    let new_config = merge_trust_accepted(config, &key)?;
    write_atomic(&path, &new_config)?;
    Ok(())
}

/// `target` を同一 dir の tempfile に書いてから atomic rename する。
/// POSIX `rename(2)` は同一 filesystem 内で atomic なので、 claude 自身の
/// concurrent write と競合しても partial-write による file 破壊は避けられる
/// (完全な競合排除は不可、 競合時はどちらか片方の write が「丸ごと」勝つ)。
fn write_atomic(target: &Path, value: &Value) -> Result<(), String> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|e| format!("serialize claude.json: {e}"))?;
    let dir = target
        .parent()
        .ok_or_else(|| format!("親 dir が無い: {}", target.display()))?;
    let file_name = target
        .file_name()
        .ok_or_else(|| format!("file name 無し: {}", target.display()))?;
    // pid + nanos で同一 process 内・他 process との衝突を避ける tempfile 名。
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp_name = format!(
        ".{}.tmp.{}.{}",
        file_name.to_string_lossy(),
        std::process::id(),
        nanos
    );
    let temp_path = dir.join(temp_name);
    fs::write(&temp_path, &bytes)
        .map_err(|e| format!("write tempfile {}: {e}", temp_path.display()))?;
    if let Err(e) = fs::rename(&temp_path, target) {
        // 失敗時は temp 残骸を掃除 (best-effort)
        let _ = fs::remove_file(&temp_path);
        return Err(format!("rename to {}: {e}", target.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_into_empty_config() {
        let cfg = serde_json::json!({});
        let result = merge_trust_accepted(cfg, "/some/path").unwrap();
        assert_eq!(
            result["projects"]["/some/path"]["hasTrustDialogAccepted"],
            Value::Bool(true),
            "trust flag が立つ"
        );
        assert!(
            result["projects"]["/some/path"]["mcpServers"].is_object(),
            "default entry の他 key も入る (mcpServers)"
        );
    }

    #[test]
    fn merge_preserves_existing_entry() {
        // 既存 entry に独自 key がある場合、 hasTrustDialogAccepted のみ更新し他は保持
        let cfg = serde_json::json!({
            "projects": {
                "/some/path": {
                    "hasTrustDialogAccepted": false,
                    "customKey": "preserve me",
                    "allowedTools": ["existing"],
                }
            }
        });
        let result = merge_trust_accepted(cfg, "/some/path").unwrap();
        assert_eq!(
            result["projects"]["/some/path"]["hasTrustDialogAccepted"],
            Value::Bool(true),
            "false → true に更新"
        );
        assert_eq!(
            result["projects"]["/some/path"]["customKey"],
            Value::String("preserve me".to_string()),
            "他 key は保持"
        );
        assert_eq!(
            result["projects"]["/some/path"]["allowedTools"],
            serde_json::json!(["existing"]),
            "allowedTools も保持"
        );
    }

    #[test]
    fn merge_preserves_other_projects() {
        // 別 path の既存 entry は無傷
        let cfg = serde_json::json!({
            "projects": {
                "/keep/this": { "hasTrustDialogAccepted": true, "customKey": "keep" }
            }
        });
        let result = merge_trust_accepted(cfg, "/new/path").unwrap();
        assert_eq!(
            result["projects"]["/keep/this"]["customKey"],
            Value::String("keep".to_string()),
            "他 project は触らない"
        );
        assert_eq!(
            result["projects"]["/new/path"]["hasTrustDialogAccepted"],
            Value::Bool(true),
            "新 project entry が追加"
        );
    }

    #[test]
    fn merge_preserves_root_keys() {
        // root level の他 setting key は保持
        let cfg = serde_json::json!({
            "autoUpdates": true,
            "theme": "dark",
            "projects": {}
        });
        let result = merge_trust_accepted(cfg, "/p").unwrap();
        assert_eq!(result["autoUpdates"], Value::Bool(true));
        assert_eq!(result["theme"], Value::String("dark".to_string()));
        assert_eq!(
            result["projects"]["/p"]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );
    }

    #[test]
    fn merge_path_with_spaces() {
        // 空白入り path も key として正しく扱える (lane 実体の "Application Support" pattern)
        let cfg = serde_json::json!({});
        let key = "/Users/foo/Library/Application Support/vp/lanes/bar";
        let result = merge_trust_accepted(cfg, key).unwrap();
        assert_eq!(
            result["projects"][key]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );
    }

    #[test]
    fn merge_rejects_non_object_root() {
        let cfg = serde_json::json!([]);
        let result = merge_trust_accepted(cfg, "/p");
        assert!(result.is_err(), "root が array なら Err");
    }
}
