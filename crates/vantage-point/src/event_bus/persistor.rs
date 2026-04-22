//! EventPersistor — Bus subscription → SurrealDB `event_log` INSERT の async task。
//!
//! fire-and-forget task としてイベント毎に event_log へ書き込む。
//! DB エラーは `tracing::warn` で log し drop (Bus は生存継続)。
//!
//! 本実装は Phase A skeleton — 実 INSERT 実装は `persist_one` の TODO に記述予定。

use std::sync::Arc;

use tokio::task::JoinHandle;

use super::bus::BusHandle;
use crate::creo::Event;
use crate::db::VpDb;

/// 永続化 task を spawn する factory。
pub struct EventPersistor {
    db: Arc<VpDb>,
}

/// spawn 済み task の handle。drop で task が detach される (要なら abort)。
pub struct PersistorHandle {
    pub task: JoinHandle<()>,
}

impl EventPersistor {
    /// Persistor を作成。Bus との結合は `spawn` 時に行う。
    pub fn new(db: Arc<VpDb>) -> Self {
        Self { db }
    }

    /// Bus を subscribe して event_log に書き続ける背景 task を起動。
    pub fn spawn(self, bus: BusHandle) -> PersistorHandle {
        let mut sub = bus.subscribe();
        let db = self.db.clone();
        let task = tokio::spawn(async move {
            loop {
                match sub.recv().await {
                    Ok(ev) => {
                        if let Err(e) = Self::persist_one(&db, &ev).await {
                            tracing::warn!(
                                error = ?e,
                                topic = %ev.topic,
                                "event_log persist failed"
                            );
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(dropped = n, "persistor lagged behind bus");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::info!("bus closed, persistor exiting");
                        break;
                    }
                }
            }
        });
        PersistorHandle { task }
    }

    /// 1 event を event_log に INSERT する。
    ///
    /// TODO (R1 Phase A 本実装):
    /// - `event_log` table に append-only で INSERT
    /// - `source.{stand,lane,project}` をそれぞれ column に展開
    /// - payload / ui は FLEXIBLE object として格納
    async fn persist_one(_db: &VpDb, _ev: &Event) -> anyhow::Result<()> {
        // TODO: R1 Phase A で実装 (VP-74 本 ticket の Day 2-3)
        // 例:
        //   db.client()
        //     .query("INSERT INTO event_log { event_id: $id, topic: $topic, ... }")
        //     .bind(...).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // TODO: publish → event_log に 1 行 (in-memory SurrealDB で test)
    // TODO: causation が保持される
    // TODO: DB down 時は warn して drop (bus は生き続ける)
    // TODO: lagged recv で drop カウント記録

    #[tokio::test]
    #[ignore = "Phase A 本実装で有効化"]
    async fn publish_persists_to_event_log() {
        // Phase A 本実装 (VP-74 Day 2-3) で unignore
    }
}
