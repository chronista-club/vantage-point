//! vpcode の transcript store — **resume 用の正本**（VCP §9、mako 裁定 2026-08-22）。
//!
//! ## 位置づけ（[`super::replay_log`] との違い）
//!
//! | | replay_log | 本 store |
//! |---|---|---|
//! | 目的 | GUI 表示の replay 源（直近だけで十分） | resume の**正本**（完全性必須） |
//! | 中身 | ConversationEvent | **不透明 payload**（OpenAI 方言 messages — VP は解釈しない） |
//! | retention | 2MB 頭切り捨て | **切らない**（切ると tool 対が割れ server 400 / engine 拒否） |
//! | 書き手 | pump tap（配信流路） | [`super::vpcode_host`] が translator 手前で直接 append |
//!
//! 配信流路（pump / ConversationEvent / unison topic）に**乗せない** — 64KB 級 blob を
//! webview に流して捨てる無駄と「記録するが配信しない」特例の両方を、書き手の位置で
//! 構造的に回避する（wire thread 01a028f5 系の 3 者合意）。
//!
//! ## 行形式 = 封筒 `{id, prev, ts, payload}`
//!
//! payload は VCP `transcript` イベントの flush（messages 配列を含む JSON）を**そのまま**。
//! 方言防壁（§9「P2 で Anthropic 方言が入っても VP 無変更」）は payload の中身に適用され、
//! 封筒は VP の管轄。**1 行 = 1 flush** — flush は engine の対整合の保証単位なので、
//! 将来の分岐 / 巻き戻しの安全な分岐点になる（id/prev の chain はそのための土台）。
//!
//! ## resume の組み立て（VCP §4 hello.transcript）
//!
//! 封筒を剥いで **payload.messages を到着順に平坦連結した 1 つの配列**を返す
//! （封筒や flush 単位の入れ子は不可 / system role は engine が flush しない前提 —
//! 混入していたら engine が hello を診断付きで拒否する、二重防壁）。

use std::path::Path;

use serde::{Deserialize, Serialize};

/// store の名前空間 dir（[`super::jsonl_store`] の `dir` 引数）。
const STORE_DIR: &str = "vpcode_transcript";

/// 1 flush の封筒。`payload` は VCP transcript イベントの中身（不透明）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEnvelope {
    /// この flush の id（host 採番 — uuid v4）。
    pub id: String,
    /// 直前の封筒の id（先頭は None）。分岐 / 巻き戻しの土台となる chain。
    pub prev: Option<String>,
    /// 記録時刻（unix millis）。
    pub ts: u64,
    /// flush の中身（`messages` を含む JSON、OpenAI 方言 — VP は解釈しない）。
    pub payload: serde_json::Value,
}

/// 1 flush を封筒に包んで追記する。`prev` は呼び手（host）が持ち回る
/// （store は純粋な機構 — 採番と chain は host の状態、data/calculations/actions 分離）。
pub fn append_in(
    base: &Path,
    repo: &str,
    label: &str,
    envelope: &TranscriptEnvelope,
) -> std::io::Result<()> {
    super::jsonl_store::append_in(base, STORE_DIR, repo, label, envelope)
}

/// 全封筒を読む（壊れ行 skip は [`super::jsonl_store`] の機構）。
pub fn load_in(base: &Path, repo: &str, label: &str) -> Vec<TranscriptEnvelope> {
    super::jsonl_store::read_all_in(base, STORE_DIR, repo, label)
}

/// resume 用: 封筒を剥いで `payload.messages` を到着順に平坦連結した 1 配列を返す
/// （hello.transcript にそのまま入る形）。messages を持たない payload は skip
/// （additive 互換 — 将来 flush が別 field を運んでも resume を壊さない）。
pub fn load_messages_in(base: &Path, repo: &str, label: &str) -> Vec<serde_json::Value> {
    load_in(base, repo, label)
        .into_iter()
        .filter_map(|env| match env.payload.get("messages") {
            Some(serde_json::Value::Array(msgs)) => Some(msgs.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// 最後の封筒 id（host が spawn 時に chain を復元する用）。
pub fn last_id_in(base: &Path, repo: &str, label: &str) -> Option<String> {
    load_in(base, repo, label).into_iter().last().map(|e| e.id)
}

/// store を消す（fresh restart / session remove の破棄配線 — replay_log::clear と**対**で呼ぶ）。
pub fn clear_in(base: &Path, repo: &str, label: &str) -> std::io::Result<()> {
    super::jsonl_store::clear_in(base, STORE_DIR, repo, label)
}

// ---- 本番 base（vp_state_dir）での wrapper（replay_log と同じ構え）----

pub fn append(repo: &str, label: &str, envelope: &TranscriptEnvelope) -> std::io::Result<()> {
    append_in(&crate::config::vp_state_dir(), repo, label, envelope)
}

pub fn load_messages(repo: &str, label: &str) -> Vec<serde_json::Value> {
    load_messages_in(&crate::config::vp_state_dir(), repo, label)
}

pub fn last_id(repo: &str, label: &str) -> Option<String> {
    last_id_in(&crate::config::vp_state_dir(), repo, label)
}

pub fn clear(repo: &str, label: &str) -> std::io::Result<()> {
    clear_in(&crate::config::vp_state_dir(), repo, label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(id: &str, prev: Option<&str>, msgs: serde_json::Value) -> TranscriptEnvelope {
        TranscriptEnvelope {
            id: id.to_string(),
            prev: prev.map(str::to_string),
            ts: 1,
            payload: serde_json::json!({ "messages": msgs }),
        }
    }

    /// 封筒 append → messages 平坦連結（hello.transcript の形）と chain 復元。
    #[test]
    fn flatten_messages_in_arrival_order_and_restore_chain() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let m1 = serde_json::json!([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
        ]);
        let m2 = serde_json::json!([
            {"role": "assistant", "tool_calls": [{"id": "t1"}]},
            {"role": "tool", "tool_call_id": "t1", "content": "ok"},
        ]);
        append_in(tmp.path(), "vp", "main#2", &env("e1", None, m1)).expect("a1");
        append_in(tmp.path(), "vp", "main#2", &env("e2", Some("e1"), m2)).expect("a2");

        let flat = load_messages_in(tmp.path(), "vp", "main#2");
        assert_eq!(flat.len(), 4, "flush 2 本の messages が平坦に連結される");
        assert_eq!(flat[0]["role"], "user");
        assert_eq!(flat[3]["role"], "tool", "到着順が保たれる");
        // 封筒 / 入れ子は残らない（hello.transcript にそのまま入る形）
        assert!(flat.iter().all(|m| m.get("payload").is_none()));

        assert_eq!(
            last_id_in(tmp.path(), "vp", "main#2").as_deref(),
            Some("e2")
        );
    }

    /// messages を持たない payload は skip（additive 互換）+ clear の冪等。
    #[test]
    fn skips_payload_without_messages_and_clear_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let odd = TranscriptEnvelope {
            id: "x".into(),
            prev: None,
            ts: 1,
            payload: serde_json::json!({ "future_field": true }),
        };
        append_in(tmp.path(), "vp", "main", &odd).expect("a");
        assert!(load_messages_in(tmp.path(), "vp", "main").is_empty());
        clear_in(tmp.path(), "vp", "main").expect("clear");
        clear_in(tmp.path(), "vp", "main").expect("clear 冪等");
    }
}
