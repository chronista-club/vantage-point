//! per-session の EchoesEvent 追記ログ（disk 永続、engine 非依存の replay 源）
//!
//! ## なぜ要るか（dogfood で発見した穴）
//!
//! Act II の会話復元（replay-on-attach）は claude では transcript(jsonl) を SSOT に再生する。
//! だが codex / grok は transcript を持たない（codex の rollout は形式移行中で直 parse は危険、
//! grok は ACP で会話 store を公開しない）。結果、これらの engine では demand_start の no_session path で
//! `ReplayStart` の冪等 clear だけが走り、**復元材料が無い**（lane 切替で会話が消える）。
//!
//! そこで **SP が配信した EchoesEvent を per-session に disk 記録**し、transcript を持たない
//! engine の replay 源にする。最初から disk 永続（in-memory では SP 再起動 / lane 切替で失われる）。
//!
//! ## 設計原則（session_store / session_registry と同系統）
//!
//! - 置き場: `vp_state_dir()/echoes_replay/<sanitize(project)>__<sanitize(label)>.jsonl`。
//!   `label` は [`crate::lane::session_registry::session_label`]（#1 = 素の lane 名、#2 以降
//!   `<lane>#<n>`）で、他 store（cc_sessions / echoes_sessions）と file 名規約が揃う
//! - **1 行 1 event の JSONL**（[`EchoesEvent`] を serde でそのまま）。壊れた行は読み時に skip
//!   （session_store の「壊れた値を渡さない」原則。partial 末尾行 = クラッシュ時の書きかけも黙って落とす）
//! - **claude は書かない**: transcript が SSOT なので二重化しない（tap は codex/grok のみ Some）
//! - 上限は turn 境界でのみ制御（[`truncate_if_needed_in`]、末尾側の完全な行を残して先頭から捨てる）
//! - 書き手 = `process::echoes_pump` の tap / user 発話は `process::unison_server` の submit 成功後。
//!   破棄は fresh restart / session remove で他 store と同じ場所に配線（`process::lanes_state`）

use std::io::Write;
use std::path::{Path, PathBuf};

use super::event::EchoesEvent;
use crate::lane::session_store::sanitize;

/// 1 session あたりのログ上限の目安（本番）。turn 境界の [`truncate_if_needed_in`] で
/// これを超えた分を古い方（先頭）から捨てる。会話の replay なので直近だけ残れば十分。
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// pump tap の宛先（transcript を持たない engine の replay 源への書き手）。
///
/// `label` は session の store label（[`crate::lane::session_registry::session_label`] の結果 =
/// #1 は素の lane 名、#2 以降 `<lane>#<n>`）。tap は codex / grok の session にのみ付く
/// （claude は None — module doc「claude は書かない」）。
#[derive(Debug, Clone)]
pub struct ReplayLogTap {
    pub project: String,
    pub label: String,
}

/// state base dir 配下の replay log file path（純関数、テスト用に base 注入）。
fn log_file_in(base: &Path, project: &str, label: &str) -> PathBuf {
    base.join("echoes_replay")
        .join(format!("{}__{}.jsonl", sanitize(project), sanitize(label)))
}

/// 1 event を JSONL 1 行として追記する（親 dir 自動作成）。
///
/// serialize 失敗（想定外）は `io::Error::other` に畳んで返す。呼び手（pump tap）は失敗を
/// warn するだけで配送は止めない（replay 記録は配送と独立）。
pub fn append_in(
    base: &Path,
    project: &str,
    label: &str,
    event: &EchoesEvent,
) -> std::io::Result<()> {
    let path = log_file_in(base, project, label);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())
}

/// 全行を読んで [`EchoesEvent`] 列に戻す。file 不在 / 全体が非 UTF-8 は空。
///
/// **壊れた行は skip**（JSON parse 不能 / 末尾の書きかけ partial 行）。replay 源が部分破損して
/// いても、読める分だけで会話を復元する方が「読めないから全滅」より復旧可能性が高い。
/// `read_to_string` + `str::lines()` は行 iterator の Err を挟まないので、1 行の破損で読みが
/// 止まらず（`Lines<BufReader>` の flatten 罠を回避）、partial 末尾行も 1 セグメントとして
/// filter_map で黙って落ちる。
pub fn load_in(base: &Path, project: &str, label: &str) -> Vec<EchoesEvent> {
    let Ok(content) = std::fs::read_to_string(log_file_in(base, project, label)) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<EchoesEvent>(line).ok())
        .collect()
}

/// ログを消す（file 削除、不在は no-op）。fresh restart / session remove の破棄配線が呼ぶ。
pub fn clear_in(base: &Path, project: &str, label: &str) -> std::io::Result<()> {
    match std::fs::remove_file(log_file_in(base, project, label)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// file が `max_bytes` を超えていたら、**末尾側の完全な行だけ**を残して先頭から捨てる（rewrite）。
///
/// - 呼び出しは **turn 境界のみ**（[`EchoesEvent::TurnCompleted`] 書き込み後）。サイズ制御コストを
///   turn 頻度に抑える（append の度に走らせない）
/// - 切り口は必ず行境界（`\n` の直後）= 半端な JSON 断片を残さない
/// - 単一行が `max_bytes` を超える極端ケースでも、最低 1 行（最新の完全な行）は残す
pub fn truncate_if_needed_in(
    base: &Path,
    project: &str,
    label: &str,
    max_bytes: u64,
) -> std::io::Result<()> {
    let path = log_file_in(base, project, label);
    let len = match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if len <= max_bytes {
        return Ok(());
    }

    // 各行の開始 offset（0 と、末尾以外の `\n` の次の位置）を集める。append は必ず `\n` で
    // 終端するので、全 byte は完全な行の連なり = 半端な断片は残っていない前提で切れる。
    let data = std::fs::read(&path)?;
    let mut line_starts = vec![0usize];
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' && i + 1 < data.len() {
            line_starts.push(i + 1);
        }
    }
    // 末尾の行は必ず残す（単一行が上限超でも最新を捨てない）。そこから遡り、合計が max_bytes に
    // 収まる限り古い行も残す（= 最小の start を選ぶ = 直近を最大限保持）。
    let mut start = *line_starts.last().unwrap_or(&0);
    for &s in line_starts.iter().rev() {
        if (data.len() - s) as u64 <= max_bytes {
            start = s;
        } else {
            break;
        }
    }
    std::fs::write(&path, &data[start..])
}

// ---- 本番 base（vp_state_dir）での wrapper（session_store と同じ構え）----

/// 本番 base での append（pump tap / user 発話記録から呼ぶ）。
pub fn append(project: &str, label: &str, event: &EchoesEvent) -> std::io::Result<()> {
    append_in(&crate::config::vp_state_dir(), project, label, event)
}

/// 本番 base での load（demand_start の replay 源読みから呼ぶ）。
pub fn load(project: &str, label: &str) -> Vec<EchoesEvent> {
    load_in(&crate::config::vp_state_dir(), project, label)
}

/// 本番 base での clear（fresh restart / session remove から呼ぶ）。
pub fn clear(project: &str, label: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), project, label)
}

// truncate は pump tap が base 注入版（`truncate_if_needed_in` + [`MAX_BYTES`]）で呼ぶ。
// turn 境界の内部処理なので本番 wrapper は設けない（YAGNI）。

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(text: &str) -> EchoesEvent {
        EchoesEvent::MessageChunk {
            text: text.to_string(),
        }
    }

    fn turn() -> EchoesEvent {
        EchoesEvent::TurnCompleted {
            session_id: "s".to_string(),
            cost_usd: None,
            context_tokens: None,
            context_window: None,
        }
    }

    /// append → load の roundtrip（順序保存）。
    #[test]
    fn append_and_load_roundtrip_preserves_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        append_in(tmp.path(), "vp", "conductor", &chunk("hello ")).expect("append 1");
        append_in(tmp.path(), "vp", "conductor", &chunk("world")).expect("append 2");
        append_in(tmp.path(), "vp", "conductor", &turn()).expect("append 3");

        let events = load_in(tmp.path(), "vp", "conductor");
        assert_eq!(events, vec![chunk("hello "), chunk("world"), turn()]);

        // 別 label は独立（混ざらない）。
        assert!(load_in(tmp.path(), "vp", "conductor#2").is_empty());
    }

    /// 壊れた行は skip し、読める行だけ返す（partial 末尾行も落ちる）。
    #[test]
    fn load_skips_corrupt_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        append_in(tmp.path(), "vp", "conductor", &chunk("good1")).expect("append");
        // 生 file に壊れた行を挟み込む。
        let path = log_file_in(tmp.path(), "vp", "conductor");
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open");
            f.write_all(b"not json at all\n").expect("write garbage");
            f.write_all(b"{\"kind\":\"message_chunk\",\"text\":\"good2\"}\n")
                .expect("write good2");
            // 末尾の書きかけ partial 行（改行なし）。
            f.write_all(b"{\"kind\":\"message_chunk\",\"text\":\"par")
                .expect("write partial");
        }
        let events = load_in(tmp.path(), "vp", "conductor");
        assert_eq!(
            events,
            vec![chunk("good1"), chunk("good2")],
            "壊れた行と partial 末尾行を除いて読める分だけ返す"
        );
    }

    /// clear は file を消し、冪等（未記録の clear も Ok）。load は空に戻る。
    #[test]
    fn clear_removes_and_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        append_in(tmp.path(), "vp", "conductor", &chunk("x")).expect("append");
        clear_in(tmp.path(), "vp", "conductor").expect("clear");
        assert!(load_in(tmp.path(), "vp", "conductor").is_empty());
        // 未記録の clear は no-op。
        clear_in(tmp.path(), "vp", "conductor").expect("二重 clear は Ok");
    }

    /// truncate: 上限超で末尾側の完全な行を残し、先頭から行境界で捨てる。
    #[test]
    fn truncate_keeps_trailing_complete_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // 各行 ~40 byte。10 本書いて上限を 100 byte にすると末尾の数本だけ残る。
        for i in 0..10 {
            append_in(
                tmp.path(),
                "vp",
                "conductor",
                &chunk(&format!("msg-{i:02}")),
            )
            .expect("append");
        }
        let before = load_in(tmp.path(), "vp", "conductor");
        assert_eq!(before.len(), 10);

        truncate_if_needed_in(tmp.path(), "vp", "conductor", 100).expect("truncate");
        let after = load_in(tmp.path(), "vp", "conductor");

        // 上限超で減っており、かつ全行が完全にパースできる（= 行境界で切れている）。
        assert!(after.len() < before.len(), "上限超過で古い行が捨てられる");
        assert!(!after.is_empty(), "最低 1 行は残る");
        // 残ったのは末尾側（最新）— 最後の event が保持される。
        assert_eq!(after.last(), Some(&chunk("msg-09")));
        // file 全体が max_bytes 以内。
        let path = log_file_in(tmp.path(), "vp", "conductor");
        assert!(std::fs::metadata(&path).expect("meta").len() <= 100);

        // 上限以内なら no-op（減らない）。
        let n = after.len();
        truncate_if_needed_in(tmp.path(), "vp", "conductor", 100).expect("truncate again");
        assert_eq!(load_in(tmp.path(), "vp", "conductor").len(), n);
        // 不在 file の truncate も Ok。
        truncate_if_needed_in(tmp.path(), "vp", "absent", 100).expect("不在は no-op");
    }

    /// 単一行が上限を超えても最新 1 行は残す（会話の最新を捨てない）。
    #[test]
    fn truncate_keeps_last_line_even_if_over_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        append_in(tmp.path(), "vp", "conductor", &chunk("old")).expect("append old");
        append_in(tmp.path(), "vp", "conductor", &chunk(&"z".repeat(500))).expect("append big");
        truncate_if_needed_in(tmp.path(), "vp", "conductor", 100).expect("truncate");
        let after = load_in(tmp.path(), "vp", "conductor");
        assert_eq!(after.len(), 1, "最新の 1 行だけ残る");
        assert_eq!(after[0], chunk(&"z".repeat(500)));
    }

    /// file 名は project / label を sanitize（`.` `/` → `-`、`#` は保持）した規約。
    #[test]
    fn file_name_sanitizes_and_lives_under_echoes_replay() {
        let p = log_file_in(Path::new("/base"), "creo.memories", "conductor#2");
        assert_eq!(
            p,
            Path::new("/base/echoes_replay/creo-memories__conductor#2.jsonl")
        );
        let p = log_file_in(Path::new("/base"), "a/b", "../evil");
        assert_eq!(p, Path::new("/base/echoes_replay/a-b__---evil.jsonl"));
    }
}
