//! CursorAgentHost — cursor-agent を lane 単位で turn-scoped 駆動する（Act II engine host）
//!
//! 機構は [`super::turn_host::TurnHost`] が全て持つ（queue / interrupt=kill / self-heal /
//! 偽 Error 回避 — cursor 初版 host（PR #776）からの抽出、経緯は turn_host の module doc）。
//! 本 module は cursor の差分 = コマンド構築・session 永続・表示名だけを [`CursorEngine`] として
//! 差す。stream 翻訳は [`super::cursor_translate`]。
//!
//! ## コマンド形（doc `cursor-engine.md` §Act II）
//!
//! ```text
//! cursor-agent -p "<prompt>" --output-format stream-json --stream-partial-output --trust [--resume '<chatId>']
//! ```
//!
//! - prompt は positional 値（`Command::arg` 渡し = shell 非経由なので injection 安全）
//! - `--stream-partial-output` の delta 判定は翻訳器側（cursor_translate の module doc）
//! - `--trust`: workspace trust の自動付与。headless（`-p`）は trust prompt を対話で出せず、
//!   cursor-agent 初見の workspace（新 lane / 未使用 repo）では "Workspace Trust Required" で
//!   即死する（2026-07-16 dogfood、bikeboy で実発生）。lane の cwd は user 自身が VP に登録した
//!   workspace なので自動 trust は claude（bypassPermissions）/ codex（full bypass）の姿勢と整合。
//!   Act I（TUI 床）は対話 prompt が出るため付けない = user 判断のまま
//! - record-from-init: `system/init` の session_id（= chatId）を `cursor_session` に永続。
//!   Act I（console 床）と同じ state file なので II ⇄ I の会話が `--resume` で継がれる

use super::cursor_translate::CursorTranslator;
use super::turn_host::{TurnEngine, TurnHost};

/// cursor-agent の [`TurnEngine`] 実装（状態なし、差分定義のみ）。
#[derive(Debug, Default)]
pub struct CursorEngine;

impl TurnEngine for CursorEngine {
    type Translator = CursorTranslator;

    fn translator(&self) -> Self::Translator {
        CursorTranslator::new()
    }

    fn command(&self, prompt: &str, resume: Option<&str>) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(crate::lane::cursor_session::cursor_cli_path());
        cmd.arg("-p")
            .arg(prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--stream-partial-output")
            // headless は trust prompt を出せない — 初見 workspace で即死するため自動 trust
            // （module doc の ⚠️ 参照。Act I の TUI は対話 prompt に委ねるので付けない）。
            .arg("--trust");
        if let Some(id) = resume {
            cmd.arg("--resume").arg(id);
        }
        cmd
    }

    fn record_session(&self, project: &str, lane: &str, id: &str) -> std::io::Result<()> {
        crate::lane::cursor_session::record(project, lane, id)
    }

    fn clear_session(&self, project: &str, lane: &str) -> std::io::Result<()> {
        crate::lane::cursor_session::clear(project, lane)
    }

    fn label(&self) -> &'static str {
        "cursor-agent"
    }
}

/// lane 単位の turn-scoped cursor-agent host（Act II）。
pub type CursorAgentHost = TurnHost<CursorEngine>;

#[cfg(test)]
mod tests {
    use super::*;

    /// コマンド形の固定（cursor-engine.md の実測形からの drift 検知）。
    #[test]
    fn command_shape_fresh_and_resume() {
        let engine = CursorEngine;
        let cmd = engine.command("hello", None);
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect();
        assert_eq!(
            args,
            vec![
                "-p",
                "hello",
                "--output-format",
                "stream-json",
                "--stream-partial-output",
                "--trust"
            ]
        );

        let cmd = engine.command("hi", Some("chat_42"));
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect();
        assert_eq!(
            args,
            vec![
                "-p",
                "hi",
                "--output-format",
                "stream-json",
                "--stream-partial-output",
                "--trust",
                "--resume",
                "chat_42"
            ]
        );
    }
}
