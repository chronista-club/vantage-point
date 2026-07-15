//! CodexAgentHost — codex を lane 単位で turn-scoped 駆動する（Act II engine host）
//!
//! 機構は [`super::turn_host::TurnHost`] が全て持つ（queue / interrupt=kill / self-heal /
//! 偽 Error 回避）。本 module は codex の差分 = コマンド構築・session 永続・表示名だけを
//! [`CodexEngine`] として差す。stream 翻訳は [`super::codex_translate`]。
//!
//! ## コマンド形（codex-cli 0.144.4 実測、doc 37 §7）
//!
//! - 初回 turn: `codex exec --json --dangerously-bypass-approvals-and-sandbox
//!   --skip-git-repo-check -- "<prompt>"`
//! - 継続 turn: `codex exec resume <UUID> …（同 flags）… -- "<prompt>"`
//!
//! 判断のポイント:
//! - **`--dangerously-bypass-approvals-and-sandbox`**: claude の `bypassPermissions` / cursor の
//!   all-tools 相当。`-s danger-full-access` 単体では承認プロンプトが残り、headless で承認 UI が
//!   無いため許可待ち error 化する（[[echoes-act2-parity]] の罠）ので使わない。
//! - **`--` 区切り**: prompt が `-` 始まりでも positional に確定させる（clap、実測でパース確認済）。
//! - **`--skip-git-repo-check`**: codex は既定で git repo 外の実行を拒む。lane cwd はほぼ git
//!   repo だが、非 git プロジェクトの Echoes でも落ちないよう外す。
//! - **resume は UUID 指名**（`thread.started` を record-from-init）。`--last` は claude
//!   `--continue` と同型の「最新」曖昧性があるため使わない。存在しない id の resume は実測で
//!   「no rollout found」即エラー終了 = TurnHost の self-heal（記録破棄 → resume 無し再実行）が
//!   そのまま効く。

use super::codex_translate::CodexTranslator;
use super::turn_host::{TurnEngine, TurnHost};

/// codex の [`TurnEngine`] 実装（状態なし、差分定義のみ）。
#[derive(Debug, Default)]
pub struct CodexEngine;

impl TurnEngine for CodexEngine {
    type Translator = CodexTranslator;

    fn translator(&self) -> Self::Translator {
        CodexTranslator::new()
    }

    fn command(&self, prompt: &str, resume: Option<&str>) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(crate::lane::codex_session::codex_cli_path());
        cmd.arg("exec");
        if let Some(id) = resume {
            cmd.arg("resume").arg(id);
        }
        cmd.arg("--json")
            .arg("--dangerously-bypass-approvals-and-sandbox")
            .arg("--skip-git-repo-check")
            .arg("--")
            .arg(prompt);
        cmd
    }

    fn record_session(&self, project: &str, lane: &str, id: &str) -> std::io::Result<()> {
        crate::lane::codex_session::record(project, lane, id)
    }

    fn clear_session(&self, project: &str, lane: &str) -> std::io::Result<()> {
        crate::lane::codex_session::clear(project, lane)
    }

    fn label(&self) -> &'static str {
        "codex"
    }
}

/// lane 単位の turn-scoped codex host（Act II）。
pub type CodexAgentHost = TurnHost<CodexEngine>;

#[cfg(test)]
mod tests {
    use super::*;

    /// コマンド形の固定（doc 37 §7 の実測形からの drift 検知）。
    #[test]
    fn command_shape_fresh_and_resume() {
        let engine = CodexEngine;
        let cmd = engine.command("hi there", None);
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect();
        assert_eq!(
            args,
            vec![
                "exec",
                "--json",
                "--dangerously-bypass-approvals-and-sandbox",
                "--skip-git-repo-check",
                "--",
                "hi there"
            ]
        );

        let cmd = engine.command("-leading-dash", Some("0196-abc"));
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect();
        assert_eq!(
            args,
            vec![
                "exec",
                "resume",
                "0196-abc",
                "--json",
                "--dangerously-bypass-approvals-and-sandbox",
                "--skip-git-repo-check",
                "--",
                "-leading-dash"
            ],
            "`--` 区切りで leading-dash prompt も positional に確定する"
        );
    }
}
