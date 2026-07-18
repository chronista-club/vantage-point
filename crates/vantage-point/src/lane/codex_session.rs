//! lane ごとの Codex thread id 永続化（`cc_session` の codex 版）
//!
//! codex（OpenAI Codex CLI）の会話単位は **thread**（id は UUID）。
//! - `codex` = 対話 TUI 起動 / `codex resume '<id>'` = TUI の指名 resume
//! - `codex exec --json` = headless 1 turn（`thread.started` が thread_id を stream に載せる）
//! - `codex exec resume '<id>' --json` = headless の指名 resume
//!
//! **id 先取り手段（create-chat 相当）が無い**ため、採番は常に
//! record-from-init（Act II turn の `thread.started` を [`crate::echoes::codex_host`] が
//! 書き戻す）に依る。Act I（TUI 床）は同じ state file を読んで `codex resume '<id>'` で継ぐ
//! （= Act I ⇄ II の会話共有、doc 37 §7）。`--last`（最新 picker）は claude `--continue` の
//! dashboard 罠と同型の「最新」曖昧性があるため使わない。
//!
//! - 置き場: `vp_state_dir()/codex_sessions/<project>__<lane>`（1 lane 1 file 1 行 = thread id）
//! - 共通機構（record / last / clear + 検証防壁）は [`super::session_store`] に委譲

use std::path::{Path, PathBuf};

use super::session_store::SessionStore;

/// thread id の正規形（英数 + ハイフン、非空 = UUID を包含）。
///
/// `resume '<id>'` の single-quote 埋め込みが shell injection にならないための防壁。
/// 書き手（record-from-init）は `thread.started` の UUID を記録するので通常は常に真。
pub(crate) fn is_valid_thread_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

const STORE: SessionStore = SessionStore::new("codex_sessions", is_valid_thread_id);

/// state base dir 配下の thread file path（純関数、テスト用に base 注入）。
pub fn thread_file_in(base: &Path, project: &str, lane: &str) -> PathBuf {
    STORE.file_in(base, project, lane)
}

/// thread id を記録する（上書き、1 行）。形式外は書かずに Ok（session_store の共通原則）。
pub fn record_in(base: &Path, project: &str, lane: &str, thread_id: &str) -> std::io::Result<()> {
    STORE.record_in(base, project, lane, thread_id)
}

/// 最後に記録された thread id を返す（無い / 形式外は None）。
pub fn last_in(base: &Path, project: &str, lane: &str) -> Option<String> {
    STORE.last_in(base, project, lane)
}

/// 記録を消す（未記録なら no-op）。fresh（"New Session"）の state 側表現。
pub fn clear_in(base: &Path, project: &str, lane: &str) -> std::io::Result<()> {
    STORE.clear_in(base, project, lane)
}

/// 本番 base（vp_state_dir）での record（codex_host の record-from-init から呼ぶ）。
pub fn record(project: &str, lane: &str, thread_id: &str) -> std::io::Result<()> {
    record_in(&crate::config::vp_state_dir(), project, lane, thread_id)
}

/// 本番 base（vp_state_dir）での clear（fresh restart から呼ぶ）。
pub fn clear(project: &str, lane: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), project, lane)
}

/// 本番 base（vp_state_dir）での last（Act I spawn / Act II ensure から呼ぶ）。
pub fn last(project: &str, lane: &str) -> Option<String> {
    last_in(&crate::config::vp_state_dir(), project, lane)
}

/// codex の実行パスを解決する（launchd の細い PATH 対策、`session_store::resolve_cli` 委譲）。
///
/// brew cask（`/opt/homebrew/bin/codex`）が主経路。Act II（[`crate::echoes::codex_host`]）の
/// turn spawn が使うため crate 内公開。
pub(crate) fn codex_cli_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    super::session_store::resolve_cli(
        "codex",
        &[
            PathBuf::from("/opt/homebrew/bin/codex"),
            PathBuf::from(format!("{home}/.local/bin/codex")),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// dir 名が codex_sessions であること（他 engine の store と混ざらない）。
    #[test]
    fn file_lives_under_codex_sessions() {
        let p = thread_file_in(Path::new("/base"), "vp", "conductor");
        assert_eq!(p, Path::new("/base/codex_sessions/vp__conductor"));
    }

    /// thread id 検証: UUID 形は通し、injection 形 / `_` は拒否（cc_session と同規則）。
    #[test]
    fn thread_id_validation_is_uuid_shaped() {
        assert!(is_valid_thread_id("0196f9a2-1234-4abc-9def-0123456789ab"));
        assert!(!is_valid_thread_id(""));
        assert!(!is_valid_thread_id("has_underscore"));
        assert!(!is_valid_thread_id("a'; rm -rf /"));
        // 書き読み roundtrip（共通機構が効いていることの smoke）。
        let tmp = tempfile::tempdir().expect("tempdir");
        record_in(tmp.path(), "vp", "conductor", "0196-abc").expect("record");
        assert_eq!(
            last_in(tmp.path(), "vp", "conductor").as_deref(),
            Some("0196-abc")
        );
        clear_in(tmp.path(), "vp", "conductor").expect("clear");
        assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
    }
}
