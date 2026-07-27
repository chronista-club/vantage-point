//! claude 固有の session helper（会話 id の検証 + transcript 探索）。
//!
//! ⚠️ **doc 40 で会話 id の SSOT は [`super::session_registry`]（`SessionEntry.conversation`）
//! に統合された**。かつて本 module が持っていた per-lane state file の store 役
//! （record / last / clear）は doc 40 PR-2 で退役した（one-shot migration で全 lane の会話 id を
//! registry へ移設済み。旧書き手 = hook 直書きは「root の label に追従しない」ラベル乖離バグの
//! 発生源だった — doc 40 §1-1。hook は repo への報告者に降格済み）。
//!
//! 本 module に残るのは claude 固有部だけ:
//! - [`is_valid_session_id`]: `--resume '<id>'` への injection 防壁（registry の write 側
//!   dispatch [`super::session_registry`] も使う）
//! - [`transcript_path`] / [`transcript_has_conversation`]: `~/.claude/projects` 走査
//!   （transcript replay 源の解決 / resume pre-flight の継続判定）。**存在と会話は別の問い**
//!   — replay は「file があるか」（`transcript_path`）、継続判定は「会話が成立したか」
//!   （`transcript_has_conversation`）を使う。

use std::path::{Path, PathBuf};

/// session id の正規形 (英数+ハイフン、 非空)。 `--resume '<id>'` の single-quote 埋め込みが
/// shell injection にならないための防壁（registry の write 側検証
/// `session_registry::is_valid_conversation` の claude arm も本関数を使う）。
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// claude の session transcript file path を引く（`~/.claude/projects/*/<id>.jsonl`）。
///
/// ⚠️ `projects` は **Claude Code 側の固有名**（外部 contract）— VP の project→repo rename の
/// 対象ではない。v0.56.0 の #940 が機械 rename で `repos` に巻き込み、resume pre-flight が
/// 全滅（transcript_exists 常 false → 全 spawn が fresh に倒れる）した実績がある。
///
/// claude は cwd 由来の encoded dir 名で session を分けるため、 全 project dir を走査する
/// （encoding 形式に依存しない = 堅牢）。 N は project 数（数百程度、 boot でなく切替 / attach 時のみ）。
/// 不正 id / home 不明 / 実体なしは None。
pub fn transcript_path(session_id: &str) -> Option<PathBuf> {
    transcript_path_in(&dirs::home_dir()?, session_id)
}

/// [`transcript_path`] の home 注入版（テストが実 `~/.claude` に依存しないため）。
fn transcript_path_in(home: &Path, session_id: &str) -> Option<PathBuf> {
    if !is_valid_session_id(session_id) {
        return None;
    }
    let projects = home.join(".claude").join("projects");
    let target = format!("{session_id}.jsonl");
    std::fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|e| e.path().join(&target))
        .find(|p| p.exists())
}

/// claude の session transcript が「継続に値する会話」を含むか（resume pre-flight /
/// F1/F2 guard の継続判定。旧 `transcript_exists` の後継）。
///
/// doc 33 C2: stale / phantom な cc_session id で resume すると headless claude が
/// "No conversation found" で即エラーになるため、継続できない id は resume に渡さず
/// fresh spawn に倒す。当初この判定は transcript の**実在**だったが、実在は「会話がある」の
/// 代理として漏れる: claude は発話ゼロの session でも meta 行だけの transcript
/// （mode / bridge-session / local-command 記録等）を書くことがある（2026-07-27 実測 —
/// meta-only な幻 pointer が F1/F2 guard を逆向きに作動させ、本物の会話への復帰を
/// KeptExisting で弾いた）。
///
/// 判定は「top-level `type:"assistant"` 行が 1 つでもあるか」= 最低 1 往復が成立した証明。
/// `type:"user"` は判定に使えない — local-command 記録（`/model` 等）も user 行として
/// 書かれるため。tui ⇄ gui 切替の live session は発話済み = assistant 行があるので継続する。
pub fn transcript_has_conversation(session_id: &str) -> bool {
    dirs::home_dir().is_some_and(|home| transcript_has_conversation_in(&home, session_id))
}

/// [`transcript_has_conversation`] の home 注入版（テストが実 `~/.claude` に依存しないため）。
fn transcript_has_conversation_in(home: &Path, session_id: &str) -> bool {
    transcript_path_in(home, session_id).is_some_and(|p| file_has_assistant_line(&p))
}

/// transcript file に top-level `type:"assistant"` の行があるか。行は JSON parse で確認する
/// — attachment 行には任意テキスト（file 内容等）が埋まるため、部分文字列一致では偽陽性になり、
/// 書式（空白）変化には偽陰性になる。最初の assistant 行で early-return するので、実会話の
/// transcript なら先頭数行で確定する（meta-only の幻は小さいので全読みでも軽い）。
fn file_has_assistant_line(path: &Path) -> bool {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .any(|line| {
            serde_json::from_str::<serde_json::Value>(&line)
                .ok()
                .and_then(|v| {
                    v.get("type")
                        .and_then(|t| t.as_str().map(|s| s == "assistant"))
                })
                .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// claude 版の会話 id 検証規則: 英数 + ハイフンのみ（`_` / 空 / injection 形は不可）。
    /// `--resume '<id>'` の single-quote 埋め込み防壁の核。
    #[test]
    fn session_id_validation_rejects_underscore_and_injection() {
        assert!(is_valid_session_id("good-id"));
        assert!(is_valid_session_id("94427c81-1234-4abc"));
        assert!(!is_valid_session_id(""), "空は不可");
        assert!(!is_valid_session_id("has_underscore"), "_ は不可");
        assert!(!is_valid_session_id("bad id'; rm"), "quote 破りは reject");
    }

    /// transcript 置き場は **Claude Code の `~/.claude/projects/`**（外部 contract）。
    /// #940 の project→repo 機械 rename がここを `repos` に巻き込み、resume pre-flight が
    /// 全滅した回帰の再発防止 — この `projects` は VP の語彙変更に追従させないこと。
    #[test]
    fn transcript_path_scans_claude_projects_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude/projects/-Users-x-repos-y");
        std::fs::create_dir_all(&dir).unwrap();
        let id = "94427c81-aaaa-4abc-bbbb-000000000000";
        std::fs::write(dir.join(format!("{id}.jsonl")), "{}").unwrap();

        let found =
            transcript_path_in(tmp.path(), id).expect("projects 配下の transcript が引ける");
        assert!(found.ends_with(format!("-Users-x-repos-y/{id}.jsonl")));
        // 実体なし / 不正 id は None（resume に渡さず fresh に倒す既存規約）
        assert!(transcript_path_in(tmp.path(), "no-such-id").is_none());
        assert!(transcript_path_in(tmp.path(), "bad_id").is_none());
    }

    /// 「実在」と「会話」は別の問い（2026-07-27 の幻 pointer 逆転の再発防止）。
    /// claude は発話ゼロでも meta-only transcript を書くことがあり、local-command 記録
    /// （`/model` 等）は `type:"user"` で入る — どちらも継続の証明にならない。
    /// 継続判定 true の条件は top-level `type:"assistant"` 行の存在のみ。
    #[test]
    fn has_conversation_requires_an_assistant_line() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude/projects/-Users-x-repos-y");
        std::fs::create_dir_all(&dir).unwrap();
        let write = |id: &str, body: &str| {
            std::fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
        };

        // 幻: meta + local-command の user 行のみ（2026-07-27 に実在した形そのまま）
        let phantom = "11111111-aaaa-4abc-bbbb-000000000000";
        write(
            phantom,
            concat!(
                "{\"type\":\"mode\",\"mode\":\"normal\"}\n",
                "{\"type\":\"user\",\"message\":{\"content\":\"<command-name>/model</command-name>\"}}\n",
            ),
        );
        assert!(
            !transcript_has_conversation_in(tmp.path(), phantom),
            "meta-only（発話ゼロ）は継続対象でない"
        );

        // attachment 行に "type":"assistant" 文字列が埋まっていても偽陽性しない（JSON parse が正）
        let embedded = "22222222-aaaa-4abc-bbbb-000000000000";
        write(
            embedded,
            "{\"type\":\"attachment\",\"content\":\"{\\\"type\\\":\\\"assistant\\\"}\"}\n",
        );
        assert!(
            !transcript_has_conversation_in(tmp.path(), embedded),
            "埋め込み文字列は会話でない"
        );

        // 本物: assistant 行が 1 つあれば true（最低 1 往復の成立）
        let real = "33333333-aaaa-4abc-bbbb-000000000000";
        write(
            real,
            concat!(
                "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
                "{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n",
            ),
        );
        assert!(transcript_has_conversation_in(tmp.path(), real));

        // 実体なしは false（旧 transcript_exists の規約を引き継ぐ）
        assert!(!transcript_has_conversation_in(tmp.path(), "no-such-id"));
    }
}
