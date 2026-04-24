//! Capability Registry (REQ-CAP-002)
//!
//! 能力の登録・検索・管理を行うレジストリ。
//! Process内で複数の能力を管理し、ライフサイクルを制御する。
//!
//! ## 設計思想
//!
//! - **名前ベース管理**: 能力は一意の名前で識別
//! - **有効/無効制御**: 能力の動的な有効化・無効化
//! - **非同期対応**: 全操作はasync/awaitで実行可能

use crate::capability::core::{
    Capability, CapabilityContext, CapabilityError, CapabilityInfo, CapabilityResult,
    CapabilityState,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// =============================================================================
// CapabilityEntry (能力エントリ)
// =============================================================================

/// レジストリ内の能力エントリ
///
/// 能力本体に加え、有効/無効状態を管理
struct CapabilityEntry {
    /// 能力本体
    capability: Box<dyn Capability>,
    /// 有効フラグ
    enabled: bool,
}

// =============================================================================
// CapabilityRegistry
// =============================================================================

/// 能力のレジストリ
///
/// ## 使用例
///
/// ```ignore
/// let registry = CapabilityRegistry::new();
///
/// // 能力を登録
/// registry.register(Box::new(MidiCapability::new())).await?;
///
/// // 能力を取得
/// if let Some(midi) = registry.get("midi-capability").await {
///     println!("Found: {}", midi.info().name);
/// }
///
/// // 能力を無効化
/// registry.disable("midi-capability").await?;
/// ```
pub struct CapabilityRegistry {
    /// 登録済み能力
    capabilities: Arc<RwLock<HashMap<String, CapabilityEntry>>>,
    /// 共有コンテキスト
    context: Arc<CapabilityContext>,
}

impl CapabilityRegistry {
    /// 新しいレジストリを作成
    pub fn new() -> Self {
        Self {
            capabilities: Arc::new(RwLock::new(HashMap::new())),
            context: Arc::new(CapabilityContext::new()),
        }
    }

    /// コンテキストを指定してレジストリを作成
    pub fn with_context(context: CapabilityContext) -> Self {
        Self {
            capabilities: Arc::new(RwLock::new(HashMap::new())),
            context: Arc::new(context),
        }
    }

    // -------------------------------------------------------------------------
    // 登録・登録解除
    // -------------------------------------------------------------------------

    /// 能力を登録
    ///
    /// ## 引数
    /// - `capability`: 登録する能力
    ///
    /// ## エラー
    /// - 同名の能力が既に登録されている場合
    pub async fn register(&self, capability: Box<dyn Capability>) -> CapabilityResult<()> {
        let name = capability.info().name;
        let mut caps = self.capabilities.write().await;

        if caps.contains_key(&name) {
            return Err(CapabilityError::ConfigError(format!(
                "Capability '{}' is already registered",
                name
            )));
        }

        caps.insert(
            name.clone(),
            CapabilityEntry {
                capability,
                enabled: true,
            },
        );

        tracing::info!("Registered capability: {}", name);
        Ok(())
    }

    /// 能力を登録解除
    ///
    /// ## 引数
    /// - `name`: 登録解除する能力の名前
    ///
    /// ## 戻り値
    /// - 登録解除した能力（存在した場合）
    pub async fn unregister(&self, name: &str) -> Option<Box<dyn Capability>> {
        let mut caps = self.capabilities.write().await;
        caps.remove(name).map(|entry| {
            tracing::info!("Unregistered capability: {}", name);
            entry.capability
        })
    }

    // -------------------------------------------------------------------------
    // 検索・一覧取得
    // -------------------------------------------------------------------------

    /// 能力を名前で検索
    ///
    /// ## 引数
    /// - `name`: 検索する能力の名前
    ///
    /// ## 戻り値
    /// - 能力のメタ情報（存在する場合）
    pub async fn get(&self, name: &str) -> Option<CapabilityInfo> {
        let caps = self.capabilities.read().await;
        caps.get(name).map(|entry| entry.capability.info())
    }

    /// 能力が登録されているか確認
    pub async fn contains(&self, name: &str) -> bool {
        let caps = self.capabilities.read().await;
        caps.contains_key(name)
    }

    /// 登録済み能力を一覧取得
    ///
    /// ## 戻り値
    /// - 全登録済み能力のメタ情報
    pub async fn list(&self) -> Vec<CapabilityInfo> {
        let caps = self.capabilities.read().await;
        caps.values().map(|entry| entry.capability.info()).collect()
    }

    /// 有効な能力のみ一覧取得
    pub async fn list_enabled(&self) -> Vec<CapabilityInfo> {
        let caps = self.capabilities.read().await;
        caps.values()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.capability.info())
            .collect()
    }

    /// 全能力の自己診断レポートを収集 (2026-04-25 Stand 自己診断)
    pub async fn diagnose_all(&self) -> Vec<crate::capability::DiagnosticReport> {
        let caps = self.capabilities.read().await;
        caps.values()
            .map(|entry| entry.capability.diagnose())
            .collect()
    }

    /// 登録済み能力の数を取得
    pub async fn count(&self) -> usize {
        let caps = self.capabilities.read().await;
        caps.len()
    }

    // -------------------------------------------------------------------------
    // 有効/無効制御
    // -------------------------------------------------------------------------

    /// 能力を有効化
    ///
    /// ## 引数
    /// - `name`: 有効化する能力の名前
    ///
    /// ## エラー
    /// - 能力が見つからない場合
    pub async fn enable(&self, name: &str) -> CapabilityResult<()> {
        let mut caps = self.capabilities.write().await;

        if let Some(entry) = caps.get_mut(name) {
            entry.enabled = true;
            tracing::info!("Enabled capability: {}", name);
            Ok(())
        } else {
            Err(CapabilityError::ConfigError(format!(
                "Capability '{}' not found",
                name
            )))
        }
    }

    /// 能力を無効化
    ///
    /// ## 引数
    /// - `name`: 無効化する能力の名前
    ///
    /// ## エラー
    /// - 能力が見つからない場合
    pub async fn disable(&self, name: &str) -> CapabilityResult<()> {
        let mut caps = self.capabilities.write().await;

        if let Some(entry) = caps.get_mut(name) {
            entry.enabled = false;
            tracing::info!("Disabled capability: {}", name);
            Ok(())
        } else {
            Err(CapabilityError::ConfigError(format!(
                "Capability '{}' not found",
                name
            )))
        }
    }

    /// 能力が有効かどうか確認
    pub async fn is_enabled(&self, name: &str) -> Option<bool> {
        let caps = self.capabilities.read().await;
        caps.get(name).map(|entry| entry.enabled)
    }

    // -------------------------------------------------------------------------
    // ライフサイクル管理
    // -------------------------------------------------------------------------

    /// 全ての有効な能力を初期化
    pub async fn initialize_all(&self) -> Vec<CapabilityResult<String>> {
        let mut caps = self.capabilities.write().await;
        let mut results = Vec::new();

        for (name, entry) in caps.iter_mut() {
            if entry.enabled {
                match entry.capability.initialize(&self.context).await {
                    Ok(_) => {
                        tracing::info!("Initialized capability: {}", name);
                        results.push(Ok(name.clone()));
                    }
                    Err(e) => {
                        tracing::error!("Failed to initialize capability '{}': {}", name, e);
                        results.push(Err(e));
                    }
                }
            }
        }

        results
    }

    /// 全ての能力をシャットダウン
    pub async fn shutdown_all(&self) -> Vec<CapabilityResult<String>> {
        let mut caps = self.capabilities.write().await;
        let mut results = Vec::new();

        for (name, entry) in caps.iter_mut() {
            match entry.capability.shutdown().await {
                Ok(_) => {
                    tracing::info!("Shutdown capability: {}", name);
                    results.push(Ok(name.clone()));
                }
                Err(e) => {
                    tracing::error!("Failed to shutdown capability '{}': {}", name, e);
                    results.push(Err(e));
                }
            }
        }

        results
    }

    /// 特定の能力を初期化
    pub async fn initialize(&self, name: &str) -> CapabilityResult<()> {
        let mut caps = self.capabilities.write().await;

        if let Some(entry) = caps.get_mut(name) {
            if !entry.enabled {
                return Err(CapabilityError::ConfigError(format!(
                    "Capability '{}' is disabled",
                    name
                )));
            }
            entry.capability.initialize(&self.context).await
        } else {
            Err(CapabilityError::ConfigError(format!(
                "Capability '{}' not found",
                name
            )))
        }
    }

    /// 特定の能力をシャットダウン
    pub async fn shutdown(&self, name: &str) -> CapabilityResult<()> {
        let mut caps = self.capabilities.write().await;

        if let Some(entry) = caps.get_mut(name) {
            entry.capability.shutdown().await
        } else {
            Err(CapabilityError::ConfigError(format!(
                "Capability '{}' not found",
                name
            )))
        }
    }

    /// 能力の状態を取得
    pub async fn state(&self, name: &str) -> Option<CapabilityState> {
        let caps = self.capabilities.read().await;
        caps.get(name).map(|entry| entry.capability.state())
    }

    // -------------------------------------------------------------------------
    // 能力へのアクセス（高度な使用法）
    // -------------------------------------------------------------------------

    /// 能力にミュータブルアクセスして操作を実行
    ///
    /// ## 使用例
    ///
    /// ```ignore
    /// registry.with_capability_mut("midi", |cap| {
    ///     if let Some(midi) = cap.as_any_mut().downcast_mut::<MidiCapability>() {
    ///         midi.send_note_on(60, 100);
    ///     }
    /// }).await;
    /// ```
    pub async fn with_capability_mut<F, R>(&self, name: &str, f: F) -> Option<R>
    where
        F: FnOnce(&mut dyn Capability) -> R,
    {
        let mut caps = self.capabilities.write().await;
        caps.get_mut(name).map(|entry| f(entry.capability.as_mut()))
    }

    /// 能力に読み取りアクセスして操作を実行
    pub async fn with_capability<F, R>(&self, name: &str, f: F) -> Option<R>
    where
        F: FnOnce(&dyn Capability) -> R,
    {
        let caps = self.capabilities.read().await;
        caps.get(name).map(|entry| f(entry.capability.as_ref()))
    }
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::core::CapabilityInfo;
    use async_trait::async_trait;
    use std::any::Any;

    /// テスト用の簡易Capability実装
    struct TestCapability {
        name: String,
        state: CapabilityState,
    }

    impl TestCapability {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                state: CapabilityState::Uninitialized,
            }
        }
    }

    #[async_trait]
    impl Capability for TestCapability {
        fn info(&self) -> CapabilityInfo {
            CapabilityInfo::new(&self.name, "0.1.0", "Test capability")
        }

        fn state(&self) -> CapabilityState {
            self.state
        }

        async fn initialize(&mut self, _ctx: &CapabilityContext) -> CapabilityResult<()> {
            self.state = CapabilityState::Idle;
            Ok(())
        }

        async fn shutdown(&mut self) -> CapabilityResult<()> {
            self.state = CapabilityState::Stopped;
            Ok(())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[tokio::test]
    async fn test_register_and_get() {
        let registry = CapabilityRegistry::new();

        // 登録
        let cap = Box::new(TestCapability::new("test-cap"));
        registry.register(cap).await.unwrap();

        // 取得
        let info = registry.get("test-cap").await.unwrap();
        assert_eq!(info.name, "test-cap");
    }

    #[tokio::test]
    async fn test_register_duplicate() {
        let registry = CapabilityRegistry::new();

        let cap1 = Box::new(TestCapability::new("test-cap"));
        let cap2 = Box::new(TestCapability::new("test-cap"));

        registry.register(cap1).await.unwrap();
        let result = registry.register(cap2).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list() {
        let registry = CapabilityRegistry::new();

        registry
            .register(Box::new(TestCapability::new("cap-a")))
            .await
            .unwrap();
        registry
            .register(Box::new(TestCapability::new("cap-b")))
            .await
            .unwrap();
        registry
            .register(Box::new(TestCapability::new("cap-c")))
            .await
            .unwrap();

        let list = registry.list().await;
        assert_eq!(list.len(), 3);
    }

    #[tokio::test]
    async fn test_enable_disable() {
        let registry = CapabilityRegistry::new();

        registry
            .register(Box::new(TestCapability::new("test-cap")))
            .await
            .unwrap();

        // デフォルトは有効
        assert_eq!(registry.is_enabled("test-cap").await, Some(true));

        // 無効化
        registry.disable("test-cap").await.unwrap();
        assert_eq!(registry.is_enabled("test-cap").await, Some(false));

        // 有効化
        registry.enable("test-cap").await.unwrap();
        assert_eq!(registry.is_enabled("test-cap").await, Some(true));
    }

    #[tokio::test]
    async fn test_list_enabled() {
        let registry = CapabilityRegistry::new();

        registry
            .register(Box::new(TestCapability::new("cap-a")))
            .await
            .unwrap();
        registry
            .register(Box::new(TestCapability::new("cap-b")))
            .await
            .unwrap();

        registry.disable("cap-a").await.unwrap();

        let enabled = registry.list_enabled().await;
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "cap-b");
    }

    #[tokio::test]
    async fn test_unregister() {
        let registry = CapabilityRegistry::new();

        registry
            .register(Box::new(TestCapability::new("test-cap")))
            .await
            .unwrap();
        assert!(registry.contains("test-cap").await);

        let removed = registry.unregister("test-cap").await;
        assert!(removed.is_some());
        assert!(!registry.contains("test-cap").await);
    }

    #[tokio::test]
    async fn test_initialize_and_shutdown() {
        let registry = CapabilityRegistry::new();

        registry
            .register(Box::new(TestCapability::new("test-cap")))
            .await
            .unwrap();

        // 初期化
        registry.initialize("test-cap").await.unwrap();
        assert_eq!(
            registry.state("test-cap").await,
            Some(CapabilityState::Idle)
        );

        // シャットダウン
        registry.shutdown("test-cap").await.unwrap();
        assert_eq!(
            registry.state("test-cap").await,
            Some(CapabilityState::Stopped)
        );
    }

    #[tokio::test]
    async fn test_initialize_disabled() {
        let registry = CapabilityRegistry::new();

        registry
            .register(Box::new(TestCapability::new("test-cap")))
            .await
            .unwrap();
        registry.disable("test-cap").await.unwrap();

        let result = registry.initialize("test-cap").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_with_capability() {
        let registry = CapabilityRegistry::new();

        registry
            .register(Box::new(TestCapability::new("test-cap")))
            .await
            .unwrap();

        let name = registry
            .with_capability("test-cap", |cap| cap.info().name)
            .await;

        assert_eq!(name, Some("test-cap".to_string()));
    }
}
