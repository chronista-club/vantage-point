//! The World - 常駐コアプロセス
//!
//! JoJo's Bizarre Adventure Part 3 の DIO のスタンド "ザ・ワールド" にちなんだ命名。
//! 時間を止める能力のように、開発環境全体を統括・制御する中核プロセス。
//!
//! ## 責務
//! - **Conductor**: Paisley Park のライフサイクル管理
//! - **HTTP MCP Server**: AI Agent から呼び出されるツール群
//! - **View Server**: WebSocket による ViewPoint への配信
//! - **Vantage DB**: SurrealDB によるローカル永続化
//! - **GER (Gold Experience Requiem)**: 守護レイヤー（自動防御、スナップショット）
//! - **macOS Integration**: システムトレイ、通知
//!
//! ## アーキテクチャ
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │                    The World                         │
//! │                   [::1]:33000                        │
//! ├─────────────────────────────────────────────────────┤
//! │  ┌───────────────┐  ┌──────────────┐                │
//! │  │   Conductor   │  │  View Server │                │
//! │  │  (Paisley管理) │  │  (WebSocket) │                │
//! │  └───────────────┘  └──────────────┘                │
//! │  ┌───────────────┐  ┌──────────────┐                │
//! │  │  HTTP MCP     │  │  Vantage DB  │                │
//! │  │   Server      │  │  (SurrealDB) │                │
//! │  └───────────────┘  └──────────────┘                │
//! │  ┌─────────────────────────────────┐                │
//! │  │  GER (Gold Experience Requiem)  │                │
//! │  │  守護レイヤー「真実にはたどり着けない」 │                │
//! │  └─────────────────────────────────┘                │
//! └─────────────────────────────────────────────────────┘
//!           ▲                    ▲
//!           │ Unison Protocol    │ WebSocket
//!           ▼                    ▼
//!    ┌─────────────┐      ┌─────────────┐
//!    │Paisley Park │      │  ViewPoint │
//!    │  (動的Port) │      │  (Browser)  │
//!    └─────────────┘      └─────────────┘
//! ```

pub mod conductor;
pub mod db;
pub mod ge;
pub mod ger;
pub mod mcp;
pub mod midi;
pub mod server;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub use conductor::{Conductor, PaisleyParkInfo, PaisleyStatus};
pub use db::{ConversationRecord, ProjectRecord, SessionRecord, VantageDb, VantageDbConfig};
pub use ge::{
    GeConfig, GoldExperience, HealAction, HealResult, ProjectKind, ScaffoldResult, Template,
};
pub use ger::{
    GerConfig, GoldExperienceRequiem, GuardianAction, GuardianRule, GuardianStatus, Snapshot,
};
pub use server::WorldServer;

/// The World の固定ポート番号
pub const WORLD_PORT: u16 = 33000;

/// The World 設定
#[derive(Debug, Clone)]
pub struct WorldConfig {
    /// バインドするアドレス
    pub addr: SocketAddr,
    /// 静的ファイルディレクトリ
    pub static_dir: Option<std::path::PathBuf>,
    /// MIDI ポートパターン（Some で有効化）
    pub midi_port_pattern: Option<String>,
    /// デバッグモード
    pub debug: bool,
    /// Requiem モード（GER: 自動防御強化）
    pub requiem_mode: bool,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            addr: format!("[::1]:{}", WORLD_PORT).parse().unwrap(),
            static_dir: None,
            midi_port_pattern: None,
            debug: false,
            requiem_mode: false,
        }
    }
}

/// The World インスタンス
///
/// 常駐コアプロセスとして、Paisley Park の管理、View 配信、MCP Server を統括
pub struct TheWorld {
    /// 設定
    config: WorldConfig,
    /// Conductor (Paisley Park ライフサイクル管理)
    conductor: Arc<RwLock<Conductor>>,
    /// Vantage DB (ローカル永続化)
    db: Arc<RwLock<VantageDb>>,
    /// GER (Gold Experience Requiem) - 守護レイヤー
    ger: Arc<RwLock<GoldExperienceRequiem>>,
    /// キャンセルトークン
    cancel: CancellationToken,
}

impl TheWorld {
    /// 新しい The World インスタンスを作成
    pub fn new(config: WorldConfig) -> Self {
        let db_config = VantageDbConfig {
            debug: config.debug,
            ..Default::default()
        };

        let ger_config = GerConfig {
            requiem_mode: config.requiem_mode,
            ..Default::default()
        };

        Self {
            config,
            conductor: Arc::new(RwLock::new(Conductor::new())),
            db: Arc::new(RwLock::new(VantageDb::new(db_config))),
            ger: Arc::new(RwLock::new(GoldExperienceRequiem::new(ger_config))),
            cancel: CancellationToken::new(),
        }
    }

    /// The World を起動
    ///
    /// - Vantage DB 接続
    /// - GER 初期化（Requiem モード時は Guardian 有効化）
    /// - HTTP/WebSocket サーバー開始
    /// - Conductor 初期化
    /// - MCP Server 登録
    pub async fn start(&self) -> Result<()> {
        if self.config.requiem_mode {
            tracing::info!("The World 起動中... 「真実にはたどり着けない」(Requiem Mode)");
        } else {
            tracing::info!("The World 起動中... 「時よ止まれ」");
        }

        // Vantage DB 接続
        {
            let mut db = self.db.write().await;
            db.connect().await?;
        }

        // GER 初期化
        {
            let ger = self.ger.read().await;
            ger.load_snapshots().await?;

            if self.config.requiem_mode {
                drop(ger);
                let ger = self.ger.read().await;
                ger.enable_guardian().await;
            }
        }

        tracing::info!("Listening on {}", self.config.addr);

        // サーバー起動
        WorldServer::run(
            self.config.clone(),
            Arc::clone(&self.conductor),
            self.cancel.clone(),
        )
        .await
    }

    /// The World を停止
    pub async fn stop(&self) {
        tracing::info!("The World 停止中... 「そして時は動き出す」");

        // Vantage DB 切断
        {
            let mut db = self.db.write().await;
            let _ = db.disconnect().await;
        }

        self.cancel.cancel();
    }

    /// Conductor への参照を取得
    pub fn conductor(&self) -> Arc<RwLock<Conductor>> {
        Arc::clone(&self.conductor)
    }

    /// VantageDb への参照を取得
    pub fn db(&self) -> Arc<RwLock<VantageDb>> {
        Arc::clone(&self.db)
    }

    /// GER への参照を取得
    pub fn ger(&self) -> Arc<RwLock<GoldExperienceRequiem>> {
        Arc::clone(&self.ger)
    }
}

/// The World をバックグラウンドで起動
pub async fn run(config: WorldConfig) -> Result<()> {
    let world = TheWorld::new(config);
    world.start().await
}
