//! Notification bridge actor — `notify@<project>` wire address に届いた message を受信し、
//! macOS DistributedNotification に変換する Service actor。
//!
//! ## 設計 (VP-159 PR-3 → PR-4b、 2026-05-11)
//!
//! - **PR-3**: VP-24 で導入された Msgbox "notify" actor の inline 実装 (旧 `server.rs:196-247`) を
//!   struct 化、 `Service` trait に形式登録 (= ECS 純度回復、 actor を struct で表現)。
//! - **PR-4b**: `SpawnableService` super-trait を impl (= `spawn(self, shutdown)` →
//!   `spawn_loop(self, shutdown) -> JoinHandle<()>` に統一)、 caller は `server.rs` で
//!   `ActorRegistry::spawn_service` 経由に集約 (= JoinHandle を ActorRegistry が保持、
//!   PR-5 supervisor 統一の foundation)。
//!
//! ## wiremsg R4 (group B 移行、 2026-05-21) — recv path を wire accumulation に rewire
//!
//! 旧 `WhitesnakeStore.claim("notify", "conductor", ...)` の 100ms polling を廃止し、
//! wire accumulation (`WiremsgStore`) の per-agent cursor recv に切替。 actor は
//! `notify@<project>` を wire address として `WiremsgStore::recv` し、 `WireNotifier`
//! の long-poll で起床する (= `handle_wire_recv` と同型の取りこぼし防止プロトコル)。
//!
//! - 旧 msgbox は `claim → mark_consumed` の destructive 消費だったが、 wire は
//!   per-agent cursor 前進 (非破壊) なので mark_consumed / release_claim は不要。
//! - 旧 `MessageKind::Notification` での型区別は廃止 — `notify@<project>` 宛は全て
//!   notification 扱い、 body から `project` / `message` / `path` を抽出する。
//! - producer は `wire_send(to=["notify@<project>"], ...)` (= agent / cross-process
//!   forward 経由)。 旧 `msg_send(to="notify")` 経路は R5 で msgbox 撤去とともに廃止。
//!
//! ## 役割
//!
//! - wire address `notify@<project>` 宛 message を recv
//! - body (`project` / `message` / `path`) を抽出 (= project_dir fallback あり)
//! - `crate::notify::post_cc_notification` で macOS DistributedNotification 配信
//!
//! ## shutdown
//!
//! `shutdown_token.cancelled()` で recv loop 終了。
//!
//! ## 関連
//!
//! - VP-24 (Mailbox core) — original 設計
//! - VP-159 PR-3 — Service trait 形式登録 / PR-4b — SpawnableService + ActorRegistry 経由
//! - wiremsg 再設計 R4 — group B (本 actor + `lane_spawn_actor`) を wire accumulation に移行
//! - parent epic: VP-156 (Mailbox routing 統一)

use std::any::Any;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::capability::stand_service::{LayerScope, Service, SpawnableService};
use crate::capability::{WireNotifier, WiremsgStore};

/// notify recv の fallback poll 間隔。 wire_send / cross-process forward 受信は
/// 必ず `WireNotifier::notify` を呼ぶので通常は即時起床する。 この sleep は
/// notify を取りこぼした場合の安全網 (= 最悪でもこの間隔で recv が回る)。
const IDLE_POLL: Duration = Duration::from_secs(5);

/// Notification bridge Service (= wire `notify@<project>` → DistributedNotification)。
///
/// SP-local Service (= 1 Project per Process)。 `WiremsgStore` の per-agent cursor recv で
/// notify message を取得し、 macOS DistributedNotification に変換する。
///
/// `wiremsg_store = None` (= vpdb 未接続) なら recv 経路なし、 shutdown 待ちで idle。
pub struct NotificationActor {
    /// wiremsg R4: wire accumulation store (= 旧 `WhitesnakeStore` から rewire)
    wiremsg_store: Option<WiremsgStore>,
    /// wiremsg R4: long-poll 起床用 in-process notifier (`AppState` と共有)
    wire_notifier: WireNotifier,
    /// wire address の project segment (`notify@<project>`)
    project: String,
    /// project root directory (= body `path` field の fallback で使う)
    project_dir: String,
}

impl NotificationActor {
    /// 新しい `NotificationActor` を構築する。
    ///
    /// wiremsg R4: 旧 `new(store, project_dir)` → `new(wiremsg_store, wire_notifier,
    /// project, project_dir)` に signature 変更。 caller (= server.rs) が wire 系を渡す。
    pub fn new(
        wiremsg_store: Option<WiremsgStore>,
        wire_notifier: WireNotifier,
        project: String,
        project_dir: String,
    ) -> Self {
        Self {
            wiremsg_store,
            wire_notifier,
            project,
            project_dir,
        }
    }
}

impl Service for NotificationActor {
    fn actor_name(&self) -> &str {
        "notify"
    }

    fn layer_scope(&self) -> LayerScope {
        // SP-local Service (= 1 Project per Process、 cross-machine forward は wire_remote 経由)
        LayerScope::Project
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl SpawnableService for NotificationActor {
    /// recv loop を `tokio::spawn` で起動し、 `JoinHandle<()>` を返す。 `self` は consume される。
    ///
    /// shutdown_token.cancelled() で loop 終了。 VP-159 PR-4b で `spawn_loop` に統一、
    /// ActorRegistry が JoinHandle を保持する。
    fn spawn_loop(self, shutdown: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            let Self {
                wiremsg_store,
                wire_notifier,
                project,
                project_dir,
            } = self;

            // wiremsg_store なし = recv 経路なし、 shutdown 待ち
            let Some(store) = wiremsg_store else {
                tracing::info!("Notification bridge: wiremsg_store なし、 shutdown 待ち");
                shutdown.cancelled().await;
                return;
            };
            let address = format!("notify@{}", project);
            tracing::info!(
                "Notification bridge 起動 (= wire accumulation recv、 address={})",
                address
            );

            loop {
                if shutdown.is_cancelled() {
                    break;
                }
                // 取りこぼし防止プロトコル (WireNotifier doc 参照):
                // handle → notified future を store.recv の前に生成する。
                let notify = wire_notifier.handle(&address).await;
                let notified = notify.notified();

                let msgs = match store.recv(&address).await {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("notify wire recv failed: {}", e);
                        tokio::select! {
                            _ = shutdown.cancelled() => break,
                            _ = tokio::time::sleep(IDLE_POLL) => {}
                        }
                        continue;
                    }
                };

                if msgs.is_empty() {
                    // 未読なし → notify (= wire_send) か fallback timeout まで待機
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = notified => {}
                        _ = tokio::time::sleep(IDLE_POLL) => {}
                    }
                    continue;
                }

                for msg in &msgs {
                    post_notification(&msg.body, &project_dir);
                }
            }
            tracing::info!("Notification bridge: shutdown");
        })
    }
}

/// wire message の body から通知 field を抽出し、 DistributedNotification を配信する。
///
/// body は `{ project, message, path }` の JSON object 想定。 各 field 欠落時は
/// 旧 msgbox 実装と同じ fallback (project_dir から project 名導出 / "完了" / project_dir)。
fn post_notification(body: &serde_json::Value, project_dir: &str) {
    let project = body
        .get("project")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            project_dir
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or("unknown")
        })
        .to_string();
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("完了")
        .to_string();
    let path = body
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or(project_dir)
        .to_string();
    crate::notify::post_cc_notification(&project, &message, &path);
}
