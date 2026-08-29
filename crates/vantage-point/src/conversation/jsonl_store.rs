//! per-session JSONL 追記 store の共有実装（型総称）— replay_log / vpcode_transcript の共通核。
//!
//! ## なぜ切り出したか（doc: vpcode transcript 裁定 2026-08-22）
//!
//! vpcode の transcript 保存（resume 正本、封筒 JSONL）は [`super::replay_log`]（GUI replay 源）
//! と**意味論が違う**（retention: 正本は切らない / 配信: pump に乗せない）が、**実装の資産**
//! （file 名規約 / 追記 / 壊れ行 skip の読み）は同一。ここに型総称で切り出し、両者が
//! dir 名と行型だけ変えて使う — 意味論は各 module、機構はここ 1 箇所。
//!
//! ⚠️ **truncate はここに置かない**。切り詰めは replay_log の意味論（「表示は直近だけで十分」）
//! であって機構ではない — 正本 store に誤って混ざらないよう、意味論側に残す。

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::lane::session_store::sanitize;

/// state base dir 配下の store file path（純関数、テスト用に base 注入）。
/// `dir` が store の名前空間（`"conversation_replay"` / `"vpcode_transcript"`）。
pub fn file_in(base: &Path, dir: &str, repo: &str, label: &str) -> PathBuf {
    base.join(dir)
        .join(format!("{}__{}.jsonl", sanitize(repo), sanitize(label)))
}

/// 1 値を JSONL 1 行として追記する（親 dir 自動作成）。
///
/// serialize 失敗（想定外）は `io::Error::other` に畳んで返す。
pub fn append_in<T: serde::Serialize>(
    base: &Path,
    dir: &str,
    repo: &str,
    label: &str,
    value: &T,
) -> std::io::Result<()> {
    let path = file_in(base, dir, repo, label);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(value).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())
}

/// 全行を読んで値の列に戻す。file 不在 / 全体が非 UTF-8 は空。
///
/// **壊れた行は skip**（JSON parse 不能 / 末尾の書きかけ partial 行）。部分破損でも
/// 読める分だけで復元する方が「読めないから全滅」より復旧可能性が高い。
pub fn read_all_in<T: serde::de::DeserializeOwned>(
    base: &Path,
    dir: &str,
    repo: &str,
    label: &str,
) -> Vec<T> {
    let path = file_in(base, dir, repo, label);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<T>(line).ok())
        .collect()
}

/// store file を削除する（不在は成功扱い = 冪等）。
pub fn clear_in(base: &Path, dir: &str, repo: &str, label: &str) -> std::io::Result<()> {
    let path = file_in(base, dir, repo, label);
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Row {
        n: u32,
    }

    /// append → read の往復 + 壊れ行 skip + clear 冪等（型総称の機構だけを固定する —
    /// 意味論のテストは各利用側にある）。
    #[test]
    fn roundtrip_skip_broken_and_idempotent_clear() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        append_in(tmp.path(), "t_store", "vp", "main#2", &Row { n: 1 }).expect("a1");
        append_in(tmp.path(), "t_store", "vp", "main#2", &Row { n: 2 }).expect("a2");
        // 壊れ行（書きかけ partial）を混ぜる
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(file_in(tmp.path(), "t_store", "vp", "main#2"))
                .expect("open");
            f.write_all(b"{\"n\": 3").expect("partial");
        }
        let rows: Vec<Row> = read_all_in(tmp.path(), "t_store", "vp", "main#2");
        assert_eq!(rows, vec![Row { n: 1 }, Row { n: 2 }], "壊れ行は skip");
        clear_in(tmp.path(), "t_store", "vp", "main#2").expect("clear");
        clear_in(tmp.path(), "t_store", "vp", "main#2").expect("clear 冪等");
        let empty: Vec<Row> = read_all_in(tmp.path(), "t_store", "vp", "main#2");
        assert!(empty.is_empty());
    }
}
