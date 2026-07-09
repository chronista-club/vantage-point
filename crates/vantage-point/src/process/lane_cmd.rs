//! Lane subcommand types — actor 経由で Lane 操作を実行する Cmd 型。
//!
//! (I-b, 2026-04-30): user 提案「Cmd にして tokio channel で recv、 CommandRunner で
//! 常時 N 動かす、 cmd type で queue 振り分け」 を実装。 in-process 直結 (2026-07-09) で
//! 配送は **`tokio::sync::mpsc` unbounded channel** になった (旧 Mailbox actor address
//! `lane-spawn@<project>` 経由は SP 再起動時の幽霊消費のため撤去、 詳細は
//! [`crate::process::lane_spawn_actor`] module doc)。
//! 各 Cmd の処理は actor 内の `tokio::sync::Semaphore::new(N)` で gate された
//! worker pool で並列実行 (= 内部 tokio worker pool、 Lane の performer とは別概念)。
//!
//! ## 関連
//!
//! - 設計 spec: memory `mem_1CaZiXoUVvZ4hSrYtVSW8R` (I-b design spark, 2026-04-30)
//! - Mailbox infra: VP-24 完了 (`capability/msgbox.rs`、 Router/Handle/Message)
//! - 計測 input: PR #229 (I-a) の `SP startup port resolved in {ms}ms` log
//!
//! ## Cmd type 別 queue (将来拡張)
//!
//! 「cmd の type によって、 動作 queue を振り分け」 (= user 提案) を VP では
//! **Cmd 種別ごとに別 channel + 別 tokio worker pool** で表現する方針。
//!
//! - lane spawn (本 Cmd): 重い Claude CLI 起動、 N=1 推奨 (rate-limit 安全)
//! - pane 操作 / PtySlot 終了 等 (将来): 別 channel + actor で N を調整
//!
//! 今 phase は lane spawn ([`LaneCmd::SpawnLane`]) のみ。 他 actor は別 sprint。
//!
//! ## serde derive
//!
//! `tag = "kind"` で discriminate、 各 variant の field は `snake_case` rename。
//! in-process channel 直結 (2026-07-09) 後は配送に serialize は不要だが、 型の能力として
//! derive を残す (wire debug / 将来の永続化余地)。 例:
//! ```json
//! {"kind": "spawn_lane", "project_id": "vantage-point", "name": "msg-test",
//!  "cwd": "/Users/.../lanes/vantage-point-msg-test", "stand": "echoes"}
//! ```

use serde::{Deserialize, Serialize};

// doc 11 PR-B: LaneStand enum 削除、 stand は String 化 (mise task 名 "echoes" / "shell" 等、
// PR-pre2 (VP-118) で "hd" → "echoes" rename)。

/// Lane に対する操作 Cmd。 [`LaneSpawnActor`](crate::process::lane_spawn_actor) が
/// in-process channel で recv し、 内部 Semaphore で gate された tokio worker pool で
/// 1 つずつ実行する。
///
/// 今 phase (I-b minimum) では `SpawnLane` のみ。 将来拡張 (`KillLane` /
/// `RestartLane` / `SwitchStand` 等) は別 sprint で variant 追加。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaneCmd {
    /// Performer Lane を spawn (= stand_spawner で PtySlot 起動 + LanePool insert)。
    ///
    /// **1 Performer = 1 SpawnLane Cmd** に分解して actor の channel に流し、 actor が Semaphore で
    /// gate しつつ並列処理する design。
    SpawnLane {
        /// LaneAddress.project の値 (= lane repo prefix と一致する project_id、
        /// `routes/lanes.rs::create_handler` の derivation と整合)
        project_id: String,
        /// Performer name (LaneAddress.name に入る)
        name: String,
        /// 起動 cwd (典型: `vp_data_dir()/lanes/<repo>-<name>/`)
        cwd: String,
        /// Stand 名 (`vp:stand:{name}` task の name 部分、 例: "echoes" / "shell" / "tmux")。
        /// doc 11 PR-B で String 化、 旧 LaneStand enum 廃止。
        /// PR-pre2 (VP-118) で "hd" → "echoes" rename。
        stand: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// serde round-trip: enum variant が `tag` 形式で安定 serialize されるか。
    /// Mailbox の `Message::with_payload` / `payload_as` の互換性を担保。
    #[test]
    fn lane_cmd_serde_round_trip_spawn_lane() {
        let cmd = LaneCmd::SpawnLane {
            project_id: "vantage-point".to_string(),
            name: "msg-test".to_string(),
            cwd: "/tmp/lanes/vantage-point-msg-test".to_string(),
            stand: "echoes".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        // tag は "kind"、 variant 名は snake_case (= "spawn_lane")
        assert!(json.contains(r#""kind":"spawn_lane""#));
        assert!(json.contains(r#""project_id":"vantage-point""#));
        // round-trip
        let restored: LaneCmd = serde_json::from_str(&json).unwrap();
        match restored {
            LaneCmd::SpawnLane {
                project_id,
                name,
                cwd: _,
                stand,
            } => {
                assert_eq!(project_id, "vantage-point");
                assert_eq!(name, "msg-test");
                assert_eq!(stand, "echoes");
            }
        }
    }
}
