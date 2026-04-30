//! Lane subcommand types — Mailbox actor 経由で Lane 操作を実行する Cmd 型。
//!
//! (I-b, 2026-04-30): user 提案「Cmd にして tokio channel で recv、 CommandRunner で
//! 常時 N 動かす、 cmd type で queue 振り分け」 を VP の **Mailbox actor address**
//! (例: `lane-spawn@<project>`) + Mailbox `Message::with_payload` で表現。
//! 各 Cmd の処理は actor 内の `tokio::sync::Semaphore::new(N)` で gate された
//! worker pool で並列実行。
//!
//! ## 関連
//!
//! - 設計 spec: memory `mem_1CaZiXoUVvZ4hSrYtVSW8R` (I-b design spark, 2026-04-30)
//! - Mailbox infra: VP-24 完了 (`capability/msgbox.rs`、 Router/Handle/Message)
//! - 計測 input: PR #229 (I-a) の `SP startup port resolved in {ms}ms` log
//!
//! ## Cmd type 別 actor address (将来拡張)
//!
//! 「cmd の type によって、 動作 queue を振り分け」 (= user 提案) を VP では
//! **actor address ごとに別 mailbox + 別 worker pool** で表現する。
//!
//! - `lane-spawn@<project>`: 重い Claude CLI 起動、 N=1 推奨 (rate-limit 安全)
//! - `pane-tmux@<project>`: tmux 操作 (将来)、 N=多並列可能
//! - `pane-kill@<project>`: PtySlot 終了 (将来)、 graceful 待機が必要
//!
//! 今 phase (I-b minimum) は `lane-spawn@<project>` のみ。 他 actor は別 sprint。
//!
//! ## Wire format
//!
//! `Message::with_payload(&cmd)` で JSON serialize される。 `tag = "kind"` で
//! discriminate、 各 variant の field は `snake_case` rename。 例:
//! ```json
//! {"kind": "spawn_lane", "project_id": "vantage-point", "name": "msg-test",
//!  "cwd": "/Users/.../ccws/vantage-point-msg-test", "stand": "heavens_door"}
//! ```

use serde::{Deserialize, Serialize};

use super::lanes_state::LaneStand;

/// Lane に対する操作 Cmd。 Mailbox actor (`lane-spawn@<project>`) が recv し、
/// 内部 Semaphore で gate された worker pool で 1 つずつ実行する。
///
/// 今 phase (I-b minimum) では `SpawnLane` のみ。 将来拡張 (`KillLane` /
/// `RestartLane` / `SwitchStand` 等) は別 sprint で variant 追加。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaneCmd {
    /// Worker Lane を spawn (= stand_spawner で PtySlot 起動 + LanePool insert)。
    ///
    /// 旧 `LanePool::populate_workers_from_disk` が同期 loop で呼んでいた spawn を、
    /// **1 Worker = 1 SpawnLane Cmd** に分解して Mailbox actor に流す。 actor が
    /// Semaphore で gate しつつ並列処理する design。
    SpawnLane {
        /// LaneAddress.project の値 (= ccws repo prefix と一致する project_id、
        /// `routes/lanes.rs::create_handler` の derivation と整合)
        project_id: String,
        /// Worker name (LaneAddress.name に入る)
        name: String,
        /// 起動 cwd (典型: `~/.local/share/ccws/<repo>-<name>/`)
        cwd: String,
        /// LaneStand (HD or TH、 default は HD)
        stand: LaneStand,
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
            cwd: "/tmp/ccws/vantage-point-msg-test".to_string(),
            stand: LaneStand::HeavensDoor,
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
                assert_eq!(stand, LaneStand::HeavensDoor);
            }
        }
    }
}
