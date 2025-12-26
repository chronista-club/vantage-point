//! Conductor - Paisley Park ライフサイクル管理
//!
//! Paisley Park (プロジェクトAgent) の登録・監視・管理を行うコンポーネント。
//! オーケストラの指揮者のように、複数の Paisley Park を統率する。
//!
//! ## 責務
//! - Paisley Park の登録/解除
//! - ハートビート監視
//! - エラー検出と自動再起動
//! - プロジェクト一覧管理

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Paisley Park のステータス
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaisleyStatus {
    /// 起動準備中
    Starting,
    /// アイドル状態
    Idle,
    /// タスク実行中
    Busy,
    /// エラー発生
    Error(String),
    /// 停止済み
    Stopped,
}

impl Default for PaisleyStatus {
    fn default() -> Self {
        Self::Idle
    }
}

/// Paisley Park の情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaisleyParkInfo {
    /// 一意識別子
    pub id: String,
    /// プロジェクトID
    pub project_id: String,
    /// プロジェクトパス
    pub project_path: String,
    /// 動的に割り当てられたポート
    pub port: u16,
    /// セッショントークン
    pub session_token: String,
    /// 現在のステータス
    pub status: PaisleyStatus,
    /// 最後のハートビート時刻
    #[serde(skip)]
    pub last_heartbeat: Option<Instant>,
    /// 登録時刻
    pub registered_at: u64,
}

/// Conductor イベント
#[derive(Debug, Clone)]
pub enum ConductorEvent {
    /// Paisley Park が登録された
    ParkRegistered { park_id: String },
    /// Paisley Park が解除された
    ParkUnregistered { park_id: String },
    /// Paisley Park のステータスが変化
    ParkStatusChanged {
        park_id: String,
        status: PaisleyStatus,
    },
    /// ハートビート失敗（応答なし）
    HeartbeatFailed { park_id: String },
}

/// Conductor - Paisley Park ライフサイクル管理
#[derive(Debug)]
pub struct Conductor {
    /// 登録済み Paisley Park
    parks: HashMap<String, PaisleyParkInfo>,
    /// イベント送信チャンネル
    event_tx: broadcast::Sender<ConductorEvent>,
    /// ハートビートタイムアウト（デフォルト30秒）
    heartbeat_timeout: Duration,
    /// 自動再起動の最大試行回数
    max_restart_attempts: u32,
}

impl Default for Conductor {
    fn default() -> Self {
        Self::new()
    }
}

impl Conductor {
    /// 新しい Conductor を作成
    pub fn new() -> Self {
        let (event_tx, _) = broadcast::channel(64);
        Self {
            parks: HashMap::new(),
            event_tx,
            heartbeat_timeout: Duration::from_secs(30),
            max_restart_attempts: 3,
        }
    }

    /// Paisley Park を登録
    ///
    /// # Returns
    /// - `Ok((park_id, session_token))` - 登録成功
    /// - `Err` - 登録失敗
    pub fn register(
        &mut self,
        project_id: String,
        project_path: String,
        port: u16,
    ) -> anyhow::Result<(String, String)> {
        // Park ID を生成 (project_id + timestamp)
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let park_id = format!("park-{}-{}", project_id, timestamp);

        // セッショントークンを生成
        let session_token = format!("token-{}", uuid::Uuid::new_v4());

        let info = PaisleyParkInfo {
            id: park_id.clone(),
            project_id,
            project_path,
            port,
            session_token: session_token.clone(),
            status: PaisleyStatus::Starting,
            last_heartbeat: Some(Instant::now()),
            registered_at: timestamp as u64,
        };

        self.parks.insert(park_id.clone(), info);

        // イベント発火
        let _ = self.event_tx.send(ConductorEvent::ParkRegistered {
            park_id: park_id.clone(),
        });

        tracing::info!("Paisley Park 登録完了: {}", park_id);
        Ok((park_id, session_token))
    }

    /// Paisley Park を解除
    pub fn unregister(&mut self, park_id: &str, _reason: Option<String>) -> bool {
        if self.parks.remove(park_id).is_some() {
            let _ = self.event_tx.send(ConductorEvent::ParkUnregistered {
                park_id: park_id.to_string(),
            });
            tracing::info!("Paisley Park 解除: {}", park_id);
            true
        } else {
            tracing::warn!("Paisley Park が見つかりません: {}", park_id);
            false
        }
    }

    /// ハートビートを受信
    pub fn heartbeat(&mut self, park_id: &str, status: PaisleyStatus) -> bool {
        if let Some(park) = self.parks.get_mut(park_id) {
            let old_status = park.status.clone();
            park.status = status.clone();
            park.last_heartbeat = Some(Instant::now());

            if old_status != status {
                let _ = self.event_tx.send(ConductorEvent::ParkStatusChanged {
                    park_id: park_id.to_string(),
                    status,
                });
            }
            true
        } else {
            false
        }
    }

    /// 登録済み Paisley Park 一覧を取得
    pub fn list_parks(&self) -> Vec<&PaisleyParkInfo> {
        self.parks.values().collect()
    }

    /// Paisley Park を取得
    pub fn get_park(&self, park_id: &str) -> Option<&PaisleyParkInfo> {
        self.parks.get(park_id)
    }

    /// プロジェクトIDで Paisley Park を検索
    pub fn find_by_project(&self, project_id: &str) -> Option<&PaisleyParkInfo> {
        self.parks.values().find(|p| p.project_id == project_id)
    }

    /// イベント受信チャンネルを取得
    pub fn subscribe(&self) -> broadcast::Receiver<ConductorEvent> {
        self.event_tx.subscribe()
    }

    /// ハートビートタイムアウトをチェック
    ///
    /// タイムアウトした Paisley Park の一覧を返す
    pub fn check_heartbeat_timeout(&self) -> Vec<String> {
        let now = Instant::now();
        self.parks
            .iter()
            .filter_map(|(id, info)| {
                if let Some(last) = info.last_heartbeat {
                    if now.duration_since(last) > self.heartbeat_timeout {
                        return Some(id.clone());
                    }
                }
                None
            })
            .collect()
    }

    /// 登録済み Paisley Park 数
    pub fn park_count(&self) -> usize {
        self.parks.len()
    }

    /// 最大再起動試行回数を取得
    pub fn max_restart_attempts(&self) -> u32 {
        self.max_restart_attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_unregister() {
        let mut conductor = Conductor::new();

        // 登録
        let result = conductor.register(
            "test-project".to_string(),
            "/path/to/project".to_string(),
            33001,
        );
        assert!(result.is_ok());

        let (park_id, _token) = result.unwrap();
        assert!(park_id.starts_with("park-test-project-"));

        // 一覧確認
        assert_eq!(conductor.park_count(), 1);

        // 解除
        assert!(conductor.unregister(&park_id, None));
        assert_eq!(conductor.park_count(), 0);
    }

    #[test]
    fn test_heartbeat() {
        let mut conductor = Conductor::new();

        let (park_id, _) = conductor
            .register("test".to_string(), "/path".to_string(), 33001)
            .unwrap();

        // ハートビート
        assert!(conductor.heartbeat(&park_id, PaisleyStatus::Busy));

        let park = conductor.get_park(&park_id).unwrap();
        assert_eq!(park.status, PaisleyStatus::Busy);
    }
}
