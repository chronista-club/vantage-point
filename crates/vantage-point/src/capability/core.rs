//! Capability Core Trait (REQ-CAP-001)
//!
//! 全ての能力が実装する共通インターフェース。
//! JoJoスタンドの「能力」を表現し、Stand全体として協調動作する。
//!
//! ## 設計思想
//!
//! - **識別可能性**: 各能力はname, versionで一意に識別
//! - **ライフサイクル**: 初期化→稼働→停止のライフサイクルを管理
//! - **非同期**: 全ての操作はasyncで実行可能
//! - **イベント駆動**: EventBusを通じて能力間で通信

use crate::capability::params::CapabilityParams;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

// =============================================================================
// CapabilityError
// =============================================================================

/// Capability操作で発生しうるエラー
#[derive(Debug, Error)]
pub enum CapabilityError {
    /// 初期化失敗
    #[error("initialization failed: {0}")]
    InitializationFailed(String),

    /// 既に初期化済み
    #[error("capability already initialized")]
    AlreadyInitialized,

    /// 未初期化状態での操作
    #[error("capability not initialized")]
    NotInitialized,

    /// シャットダウン失敗
    #[error("shutdown failed: {0}")]
    ShutdownFailed(String),

    /// イベント処理エラー
    #[error("event handling error: {0}")]
    EventError(String),

    /// 設定エラー
    #[error("configuration error: {0}")]
    ConfigError(String),

    /// リソースエラー
    #[error("resource error: {0}")]
    ResourceError(String),

    /// タイムアウト
    #[error("operation timeout")]
    Timeout,

    /// その他のエラー
    #[error("{0}")]
    Other(String),
}

pub type CapabilityResult<T> = Result<T, CapabilityError>;

// =============================================================================
// CapabilityState
// =============================================================================

/// 能力の状態
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    /// 未初期化
    Uninitialized,
    /// 初期化中
    Initializing,
    /// 稼働中（アイドル）
    Idle,
    /// アクティブに動作中
    Active,
    /// 一時停止中
    Paused,
    /// エラー状態
    Error,
    /// シャットダウン中
    ShuttingDown,
    /// 停止済み
    Stopped,
}

impl fmt::Display for CapabilityState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uninitialized => write!(f, "Uninitialized"),
            Self::Initializing => write!(f, "Initializing"),
            Self::Idle => write!(f, "Idle"),
            Self::Active => write!(f, "Active"),
            Self::Paused => write!(f, "Paused"),
            Self::Error => write!(f, "Error"),
            Self::ShuttingDown => write!(f, "ShuttingDown"),
            Self::Stopped => write!(f, "Stopped"),
        }
    }
}

// =============================================================================
// CapabilityInfo
// =============================================================================

/// 能力のメタ情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityInfo {
    /// 能力の名前（一意識別子）
    pub name: String,
    /// バージョン（semver形式推奨）
    pub version: String,
    /// 説明
    pub description: String,
    /// 作者
    pub author: Option<String>,
    /// ホームページURL
    pub homepage: Option<String>,
    /// 能力タイプ（分類名）
    pub capability_type: String,
    /// 能力パラメータ（6パラメータ）
    pub params: CapabilityParams,
}

impl CapabilityInfo {
    /// 新しいCapabilityInfoを作成
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            description: description.into(),
            author: None,
            homepage: None,
            capability_type: "general".to_string(),
            params: CapabilityParams::balanced(),
        }
    }

    /// 作者を設定
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// ホームページを設定
    pub fn with_homepage(mut self, homepage: impl Into<String>) -> Self {
        self.homepage = Some(homepage.into());
        self
    }

    /// 能力タイプを設定
    pub fn with_type(mut self, capability_type: impl Into<String>) -> Self {
        self.capability_type = capability_type.into();
        self
    }

    /// パラメータを設定
    pub fn with_params(mut self, params: CapabilityParams) -> Self {
        self.params = params;
        self
    }

    /// 完全修飾名を取得（name@version形式）
    pub fn qualified_name(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

impl fmt::Display for CapabilityInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} v{}", self.name, self.version)?;
        if !self.description.is_empty() {
            write!(f, " - {}", self.description)?;
        }
        Ok(())
    }
}

// =============================================================================
// CapabilityEvent (能力間通信用イベント)
// =============================================================================

/// 能力間で送受信されるイベント
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityEvent {
    /// イベントタイプ（例: "midi.note_on", "agent.response"）
    pub event_type: String,
    /// 送信元能力名
    pub source: String,
    /// ペイロード（JSON値）
    pub payload: serde_json::Value,
    /// タイムスタンプ（Unix epoch ミリ秒）
    pub timestamp: u64,
}

impl CapabilityEvent {
    /// 新しいイベントを作成
    pub fn new(event_type: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            event_type: event_type.into(),
            source: source.into(),
            payload: serde_json::Value::Null,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }

    /// ペイロードを設定
    pub fn with_payload<T: Serialize>(mut self, payload: &T) -> Self {
        self.payload = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        self
    }

    /// イベントタイプがプレフィックスにマッチするか
    pub fn matches(&self, prefix: &str) -> bool {
        self.event_type.starts_with(prefix)
    }

    /// ペイロードを指定の型にデシリアライズ
    pub fn payload_as<T: for<'de> Deserialize<'de>>(&self) -> Option<T> {
        serde_json::from_value(self.payload.clone()).ok()
    }
}

// =============================================================================
// CapabilityContext (能力の実行コンテキスト)
// =============================================================================

/// 能力の実行時コンテキスト
///
/// 能力が初期化時・実行時に必要な情報やサービスへの参照を提供
pub struct CapabilityContext {
    /// イベント送信用チャンネル
    event_sender: Option<tokio::sync::mpsc::Sender<CapabilityEvent>>,
    /// 設定値（能力固有の設定を格納）
    config: serde_json::Value,
    /// 共有データストア（能力間でデータを共有）
    shared_data: Arc<tokio::sync::RwLock<serde_json::Map<String, serde_json::Value>>>,
}

impl CapabilityContext {
    /// 新しいコンテキストを作成（イベント送信なし）
    pub fn new() -> Self {
        Self {
            event_sender: None,
            config: serde_json::Value::Object(Default::default()),
            shared_data: Arc::new(tokio::sync::RwLock::new(Default::default())),
        }
    }

    /// イベント送信チャンネルを設定
    pub fn with_event_sender(mut self, sender: tokio::sync::mpsc::Sender<CapabilityEvent>) -> Self {
        self.event_sender = Some(sender);
        self
    }

    /// 設定を設定
    pub fn with_config(mut self, config: serde_json::Value) -> Self {
        self.config = config;
        self
    }

    /// イベントを発火（emit）
    pub async fn emit(&self, event: CapabilityEvent) -> CapabilityResult<()> {
        if let Some(sender) = &self.event_sender {
            sender
                .send(event)
                .await
                .map_err(|e| CapabilityError::EventError(e.to_string()))?;
        }
        Ok(())
    }

    /// 設定値を取得
    pub fn config(&self) -> &serde_json::Value {
        &self.config
    }

    /// 設定値を型付きで取得
    pub fn config_as<T: for<'de> Deserialize<'de>>(&self) -> Option<T> {
        serde_json::from_value(self.config.clone()).ok()
    }

    /// 共有データを取得
    pub async fn get_shared<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        let data = self.shared_data.read().await;
        data.get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// 共有データを設定
    pub async fn set_shared<T: Serialize>(&self, key: impl Into<String>, value: &T) {
        let mut data = self.shared_data.write().await;
        if let Ok(v) = serde_json::to_value(value) {
            data.insert(key.into(), v);
        }
    }
}

impl Default for CapabilityContext {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CapabilityContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapabilityContext")
            .field("has_event_sender", &self.event_sender.is_some())
            .field("config", &self.config)
            .finish()
    }
}

// =============================================================================
// Capability Trait (REQ-CAP-001)
// =============================================================================

/// Stand Capability トレイト
///
/// 全ての能力が実装する共通インターフェース。
///
/// ## 実装例
///
/// ```ignore
/// use async_trait::async_trait;
///
/// pub struct MyCapability {
///     state: CapabilityState,
/// }
///
/// #[async_trait]
/// impl Capability for MyCapability {
///     fn info(&self) -> CapabilityInfo {
///         CapabilityInfo::new("my-capability", "1.0.0", "My custom capability")
///     }
///
///     fn state(&self) -> CapabilityState {
///         self.state
///     }
///
///     async fn initialize(&mut self, ctx: &CapabilityContext) -> CapabilityResult<()> {
///         self.state = CapabilityState::Idle;
///         Ok(())
///     }
///
///     async fn shutdown(&mut self) -> CapabilityResult<()> {
///         self.state = CapabilityState::Stopped;
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait Capability: Send + Sync {
    // -------------------------------------------------------------------------
    // 識別情報 (name, version)
    // -------------------------------------------------------------------------

    /// 能力のメタ情報を取得
    fn info(&self) -> CapabilityInfo;

    /// 能力名を取得（ショートカット）
    fn name(&self) -> String {
        self.info().name
    }

    /// バージョンを取得（ショートカット）
    fn version(&self) -> String {
        self.info().version
    }

    // -------------------------------------------------------------------------
    // ライフサイクル (initialize, shutdown)
    // -------------------------------------------------------------------------

    /// 現在の状態を取得
    fn state(&self) -> CapabilityState;

    /// 能力を初期化
    ///
    /// ## 引数
    /// - `ctx`: 実行コンテキスト（イベント送信、設定、共有データ）
    ///
    /// ## エラー
    /// - `CapabilityError::InitializationFailed`: 初期化に失敗
    /// - `CapabilityError::AlreadyInitialized`: 既に初期化済み
    async fn initialize(&mut self, ctx: &CapabilityContext) -> CapabilityResult<()>;

    /// 能力をシャットダウン
    ///
    /// ## エラー
    /// - `CapabilityError::ShutdownFailed`: シャットダウンに失敗
    async fn shutdown(&mut self) -> CapabilityResult<()>;

    /// 能力を一時停止
    ///
    /// デフォルト実装は何もしない（オプション機能）
    async fn pause(&mut self) -> CapabilityResult<()> {
        Ok(())
    }

    /// 能力を再開
    ///
    /// デフォルト実装は何もしない（オプション機能）
    async fn resume(&mut self) -> CapabilityResult<()> {
        Ok(())
    }

    // -------------------------------------------------------------------------
    // イベント処理 (subscribe, emit)
    // -------------------------------------------------------------------------

    /// 購読するイベントタイプのパターンを取得
    ///
    /// 例: `["midi.*", "agent.response"]`
    ///
    /// デフォルトは空（何も購読しない）
    fn subscriptions(&self) -> Vec<String> {
        vec![]
    }

    /// イベントを処理
    ///
    /// EventBusから購読パターンにマッチしたイベントが配信される
    ///
    /// ## 引数
    /// - `event`: 受信したイベント
    /// - `ctx`: 実行コンテキスト
    ///
    /// デフォルト実装は何もしない
    async fn handle_event(
        &mut self,
        _event: &CapabilityEvent,
        _ctx: &CapabilityContext,
    ) -> CapabilityResult<()> {
        Ok(())
    }

    // -------------------------------------------------------------------------
    // 動的ダウンキャスト用
    // -------------------------------------------------------------------------

    /// Anyへの参照を取得（ダウンキャスト用）
    fn as_any(&self) -> &dyn Any;

    /// Anyへのミュータブル参照を取得（ダウンキャスト用）
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の簡易Capability実装
    struct TestCapability {
        state: CapabilityState,
        events_received: Vec<String>,
    }

    impl TestCapability {
        fn new() -> Self {
            Self {
                state: CapabilityState::Uninitialized,
                events_received: vec![],
            }
        }
    }

    #[async_trait]
    impl Capability for TestCapability {
        fn info(&self) -> CapabilityInfo {
            CapabilityInfo::new("test-capability", "0.1.0", "Test capability for unit tests")
        }

        fn state(&self) -> CapabilityState {
            self.state
        }

        async fn initialize(&mut self, _ctx: &CapabilityContext) -> CapabilityResult<()> {
            if self.state != CapabilityState::Uninitialized {
                return Err(CapabilityError::AlreadyInitialized);
            }
            self.state = CapabilityState::Idle;
            Ok(())
        }

        async fn shutdown(&mut self) -> CapabilityResult<()> {
            self.state = CapabilityState::Stopped;
            Ok(())
        }

        fn subscriptions(&self) -> Vec<String> {
            vec!["test.*".to_string()]
        }

        async fn handle_event(
            &mut self,
            event: &CapabilityEvent,
            _ctx: &CapabilityContext,
        ) -> CapabilityResult<()> {
            self.events_received.push(event.event_type.clone());
            Ok(())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[test]
    fn test_capability_info() {
        let cap = TestCapability::new();
        let info = cap.info();

        assert_eq!(info.name, "test-capability");
        assert_eq!(info.version, "0.1.0");
        assert_eq!(info.qualified_name(), "test-capability@0.1.0");
    }

    #[tokio::test]
    async fn test_capability_lifecycle() {
        let mut cap = TestCapability::new();
        let ctx = CapabilityContext::new();

        // 初期状態
        assert_eq!(cap.state(), CapabilityState::Uninitialized);

        // 初期化
        cap.initialize(&ctx).await.unwrap();
        assert_eq!(cap.state(), CapabilityState::Idle);

        // 二重初期化はエラー
        let result = cap.initialize(&ctx).await;
        assert!(matches!(result, Err(CapabilityError::AlreadyInitialized)));

        // シャットダウン
        cap.shutdown().await.unwrap();
        assert_eq!(cap.state(), CapabilityState::Stopped);
    }

    #[tokio::test]
    async fn test_capability_event_handling() {
        let mut cap = TestCapability::new();
        let ctx = CapabilityContext::new();

        cap.initialize(&ctx).await.unwrap();

        // イベント作成と処理
        let event = CapabilityEvent::new("test.action", "source");
        cap.handle_event(&event, &ctx).await.unwrap();

        assert_eq!(cap.events_received.len(), 1);
        assert_eq!(cap.events_received[0], "test.action");
    }

    #[test]
    fn test_capability_event() {
        let event = CapabilityEvent::new("midi.note_on", "midi-capability").with_payload(
            &serde_json::json!({
                "channel": 0,
                "note": 60,
                "velocity": 100
            }),
        );

        assert_eq!(event.event_type, "midi.note_on");
        assert_eq!(event.source, "midi-capability");
        assert!(event.matches("midi."));
        assert!(!event.matches("agent."));

        // ペイロード取得
        #[derive(Deserialize)]
        struct NoteOn {
            note: u8,
            velocity: u8,
        }
        let payload: NoteOn = event.payload_as().unwrap();
        assert_eq!(payload.note, 60);
        assert_eq!(payload.velocity, 100);
    }

    #[tokio::test]
    async fn test_capability_context_shared_data() {
        let ctx = CapabilityContext::new();

        // 共有データの設定と取得
        ctx.set_shared("key1", &"value1".to_string()).await;
        let value: Option<String> = ctx.get_shared("key1").await;
        assert_eq!(value, Some("value1".to_string()));

        // 存在しないキー
        let missing: Option<String> = ctx.get_shared("missing").await;
        assert!(missing.is_none());
    }

    #[test]
    fn test_capability_state_display() {
        assert_eq!(format!("{}", CapabilityState::Idle), "Idle");
        assert_eq!(format!("{}", CapabilityState::Active), "Active");
        assert_eq!(format!("{}", CapabilityState::Error), "Error");
    }
}
