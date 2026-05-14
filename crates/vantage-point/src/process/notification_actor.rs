//! Notification bridge actor — `notify` mailbox から Msgbox Notification msg を受信し、
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
//! 既存挙動は完全互換 (= 通信経路 / msg flow / payload schema は不変)。
//!
//! ## 役割
//!
//! - Msgbox address `notify` の Notification msg を recv
//! - payload (`project` / `message` / `path`) を抽出 (= project_dir fallback あり)
//! - `crate::notify::post_cc_notification` で macOS DistributedNotification 配信
//!
//! ## shutdown
//!
//! `shutdown_token.cancelled()` で recv loop 終了 (= 既存挙動と完全互換)。
//!
//! ## 関連
//!
//! - VP-24 (Mailbox core) — original 設計
//! - VP-159 PR-3 — Service trait 形式登録 / PR-4b — SpawnableService + ActorRegistry 経由
//! - parent epic: VP-156 (Mailbox routing 統一)
//! - PR-2 同型 pattern: `AgentCapability` / `ProtocolCapability` (impl Stand)

use std::any::Any;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::capability::MsgboxStore;
use crate::capability::msgbox::MessageKind;
use crate::capability::msgbox_v2::WhitesnakeStore;
use crate::capability::stand_service::{LayerScope, Service, SpawnableService};

/// Notification bridge Service (= Msgbox `notify` → DistributedNotification)。
///
/// SP-local Service (= 1 Project per Process)。 `WhitesnakeStore` 経由で notify msg を
/// poll し、 macOS DistributedNotification に変換する。
///
/// ## VP-177 (Phase 3 PR-5) refactor
///
/// 旧: `register("notify")` Handle.recv() mpsc loop
/// 新: `WhitesnakeStore::claim("notify", "lead", ...)` polling (= 100ms interval)
///
/// `Option<WhitesnakeStore>` = None なら recv 経路なし、 shutdown 待ちで idle。
pub struct NotificationActor {
    /// VP-177: WhitesnakeStore (= Phase 3 PR-5 で mpsc Handle から rewire)
    msgbox_store: Option<WhitesnakeStore>,
    /// project root directory (= payload `path` field の fallback で使う)
    project_dir: String,
}

impl NotificationActor {
    /// 新しい `NotificationActor` を構築する。
    ///
    /// VP-177 (Phase 3 PR-5): 旧 `new(handle, project_dir)` → `new(store, project_dir)` に
    /// signature 変更。 caller (= server.rs) が `state.msgbox_store.clone()` を渡す。
    pub fn new(msgbox_store: Option<WhitesnakeStore>, project_dir: String) -> Self {
        Self {
            msgbox_store,
            project_dir,
        }
    }
}

impl Service for NotificationActor {
    fn actor_name(&self) -> &str {
        "notify"
    }

    fn layer_scope(&self) -> LayerScope {
        // SP-local Service (= 1 Project per Process、 cross-machine forward は msgbox_remote 経由)
        LayerScope::Project
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl SpawnableService for NotificationActor {
    /// recv loop を `tokio::spawn` で起動し、 `JoinHandle<()>` を返す。 `self` は consume される。
    ///
    /// shutdown_token.cancelled() で loop 終了、 channel close (= recv が None) でも終了。
    /// VP-159 PR-4b: 旧 `spawn(self, shutdown)` (= 戻り値なし) を `spawn_loop(self, shutdown)
    /// -> JoinHandle<()>` に統一、 ActorRegistry が JoinHandle を保持する path を開く。
    fn spawn_loop(self, shutdown: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            // VP-177: msgbox_store なし = recv 経路なし、 shutdown 待ち
            let Some(store) = self.msgbox_store.as_ref() else {
                tracing::info!("Notification bridge: msgbox_store なし、 shutdown 待ち");
                shutdown.cancelled().await;
                return;
            };
            let consumer_id = format!("notify-{}", std::process::id());
            tracing::info!("Notification bridge 起動 (= WhitesnakeStore claim polling)");
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        tracing::info!("Notification bridge: shutdown");
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        match store.claim("notify", "lead", &consumer_id).await {
                            Ok(Some(msg)) if msg.kind == MessageKind::Notification => {
                                let project = msg
                                    .payload
                                    .get("project")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| {
                                        self.project_dir
                                            .rsplit('/')
                                            .find(|s| !s.is_empty())
                                            .unwrap_or("unknown")
                                    })
                                    .to_string();
                                let message = msg
                                    .payload
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("完了")
                                    .to_string();
                                let path = msg
                                    .payload
                                    .get("path")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(&self.project_dir)
                                    .to_string();
                                crate::notify::post_cc_notification(&project, &message, &path);
                                // manual_ack でなければ即 mark_consumed (= mpsc Handle::recv 互換)
                                if !msg.manual_ack
                                    && let Err(e) = store.mark_consumed(&msg.id).await
                                {
                                    tracing::warn!("VP-177 notify mark_consumed failed (id={}): {}", msg.id, e);
                                }
                            }
                            Ok(Some(other_msg)) => {
                                // 非 Notification: claim 済なので即 release (= 他 consumer 不在だが念のため)
                                if let Err(e) = store.release_claim(&other_msg.id).await {
                                    tracing::warn!("VP-177 notify release_claim failed: {}", e);
                                }
                            }
                            Ok(None) => {} // 候補なし、 polling 継続
                            Err(e) => {
                                tracing::warn!("VP-177 notify claim failed: {}", e);
                            }
                        }
                    }
                }
            }
        })
    }
}
