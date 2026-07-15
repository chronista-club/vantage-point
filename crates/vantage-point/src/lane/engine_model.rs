//! lane ごとの model 永続化（Act I TUI console + Act II chat engine 共有）
//!
//! lane 単位で claude の `--model` alias を永続する。**「この lane はこの model で走る」**
//! という単一の真実源として、両 Act の claude 起動が同じ file を読む:
//! - **Act I（TUI console / echoes stand）**: `build_stand_command` が spawn 時に読み、
//!   PtySlot にホストされる `claude … --model <alias>` の command line へ注入する
//!   （SP 再起動後の respawn でも自動で維持される）。
//! - **Act II（GUI chat engine / EchoesAgentHost）**: `ensure_chat_engine` が読む。
//!   切替は engine の drop → `--resume` 付き再 spawn で行うため、**会話コンテキストを
//!   保ったままモデルだけ替わる**（セッション進行中の切替 = CC の `/model` の VP 版）。
//!
//! [`super::console_mode`] と同型の 1 lane 1 file 1 行 state file。
//! - **書き手**: lane 作成時の `--model`（`create_performer_orchestrated` /
//!   `new_performer` / `fork_performer`、co-evolution #1）+ Act II の
//!   `console_set_model` dispatch（切替時。default へ戻す = file 削除）
//! - **読み手**: `build_stand_command`（Act I spawn）/ `ensure_chat_engine`（Act II spawn）
//! - 置き場: `vp_state_dir()/engine_models/<project>__<lane>`
//! - **未記録 = None = claude default**（`--model` を渡さない）

use std::path::{Path, PathBuf};

/// model 名として受理する形式（`--model` 引数に渡るため保守的に絞る）。
/// claude の model id / alias は英数と `.-_` だけで構成される（例: `claude-opus-4-8`）。
/// 先頭 `-` は弾く — `--model` の値が別の flag として解釈される余地を残さない。
pub fn is_valid_model(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
}

/// 明示 model（Some=優先）と既定（config knob）から、engine_model に記録する実効 model を返す。
/// 空白 / 未指定は `fallback`（= `Config::default_lane_model_or_opus()`）へ落とす。
/// performer 追加の全経路（mcp / cli / sidebar）が共有する解決規則。純粋 = テスト可能。
pub fn resolve_default(explicit: Option<&str>, fallback: &str) -> String {
    explicit
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// file 名に使えない文字を潰す（console_mode / cc_session と同一規則）。
fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '.' {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// state base dir 配下の model file path（純関数、テスト用に base 注入）。
pub fn model_file_in(base: &Path, project: &str, lane: &str) -> PathBuf {
    base.join("engine_models")
        .join(format!("{}__{}", sanitize(project), sanitize(lane)))
}

/// model を記録する（上書き、1 行）。形式外は Err（壊れた値を file に残さない）。
pub fn record_in(base: &Path, project: &str, lane: &str, model: &str) -> std::io::Result<()> {
    if !is_valid_model(model) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("model 名が不正: {model:?}"),
        ));
    }
    let path = model_file_in(base, project, lane);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, model)
}

/// 最後に記録された model を返す。無い / 形式外は None（= claude default で spawn）。
pub fn last_in(base: &Path, project: &str, lane: &str) -> Option<String> {
    let raw = std::fs::read_to_string(model_file_in(base, project, lane)).ok()?;
    let trimmed = raw.trim();
    is_valid_model(trimmed).then(|| trimmed.to_string())
}

/// 記録を消す（未記録なら no-op）。「default に戻す」と lane 削除 GC の両方で使う。
pub fn clear_in(base: &Path, project: &str, lane: &str) -> std::io::Result<()> {
    match std::fs::remove_file(model_file_in(base, project, lane)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// 本番 base（vp_state_dir）での record。
pub fn record(project: &str, lane: &str, model: &str) -> std::io::Result<()> {
    record_in(&crate::config::vp_state_dir(), project, lane, model)
}

/// 本番 base（vp_state_dir）での clear（default へ戻す / lane 削除経路から呼ぶ）。
pub fn clear(project: &str, lane: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base（vp_state_dir）での last。
pub fn last(project: &str, lane: &str) -> Option<String> {
    last_in(&crate::config::vp_state_dir(), project, lane)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_last_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // 未記録は None（= claude default）
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
        record_in(tmp.path(), "vp", "conductor", "claude-opus-4-8").expect("record");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("claude-opus-4-8")
        );
        // 上書き（最新が勝つ）
        record_in(tmp.path(), "vp", "conductor", "claude-fable-5").expect("record 2");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("claude-fable-5")
        );
    }

    #[test]
    fn clear_returns_to_default_and_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "conductor", "claude-sonnet-5").expect("record");
        clear_in(tmp.path(), "vp", "conductor").expect("clear");
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
        // 未記録の clear は no-op
        clear_in(tmp.path(), "vp", "conductor").expect("未記録の clear は Ok");
    }

    #[test]
    fn resolve_default_prefers_explicit_then_falls_back() {
        // 明示指定が最優先
        assert_eq!(
            resolve_default(Some("claude-sonnet-5"), "claude-opus-4-8"),
            "claude-sonnet-5"
        );
        // 未指定は fallback（= config knob / opus）
        assert_eq!(resolve_default(None, "claude-opus-4-8"), "claude-opus-4-8");
        // 空白 / 空文字は fallback（picker の '' や whitespace を default 扱い）
        assert_eq!(
            resolve_default(Some("   "), "claude-opus-4-8"),
            "claude-opus-4-8"
        );
        assert_eq!(
            resolve_default(Some(""), "claude-opus-4-8"),
            "claude-opus-4-8"
        );
        // 明示の前後空白は trim される
        assert_eq!(
            resolve_default(Some(" claude-fable-5 "), "claude-opus-4-8"),
            "claude-fable-5"
        );
    }

    #[test]
    fn rejects_garbage() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // 記録時に弾く（引数 injection 形は file に残さない）
        assert!(record_in(tmp.path(), "vp", "conductor", "opus --dangerous").is_err());
        assert!(record_in(tmp.path(), "vp", "conductor", "").is_err());
        // 先頭 `-` = `--model` の値が別 flag として解釈される余地
        assert!(record_in(tmp.path(), "vp", "conductor", "--resume").is_err());
        assert!(!is_valid_model("-x"));
        // file が直接壊されていても読み手が弾く
        let dir = tmp.path().join("engine_models");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vp__conductor"), "op us;rm -rf\n").unwrap();
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
    }
}
