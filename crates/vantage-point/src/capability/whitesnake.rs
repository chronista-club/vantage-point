//! Whitesnake 🐍 — 汎用永続化レイヤー
//!
//! DISC（Data-Independent Serialized Container）として
//! VP の全状態を永続化する。Whitesnake が記憶を DISC に焼くように、
//! 任意の構造体を永続化コンテナに抽出（extract）・挿入（insert）する。
//!
//! ## レイヤー位置
//!
//! EventBus / Mailbox と同じインフラ層。Capability の下に位置し、
//! 各 Capability が Whitesnake を通じて状態を永続化する。
//!
//! ## 設計思想
//!
//! - **DISC = 永続化の最小単位**: namespace + key で一意に識別
//! - **Backend 抽象**: File / SurrealDB / Memory を差し替え可能
//! - **型安全**: `Disc<T>` で Serialize/Deserialize を保証
//! - **namespace 分離**: Stand ごとに独立した名前空間
//!
//! ## DISC 命名規則
//!
//! ```text
//! {namespace}/{key}
//!
//! 例:
//! paisley-park/pane/main        — PP メインペインの内容
//! paisley-park/layout           — Canvas レイアウト
//! mailbox/msg/{id}              — Mailbox メッセージ
//! heavens-door/session/{id}     — HD セッション状態
//! process/stand-status          — Stand ステータス一覧
//! ```

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::sync::RwLock;

// =============================================================================
// Disc — 永続化コンテナ
// =============================================================================

/// DISC（Data-Independent Serialized Container）
///
/// Whitesnake が抽出した「記憶のディスク」。
/// namespace + key で一意に識別され、任意の型を JSON としてシリアライズ保持する。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Disc {
    /// 名前空間（Stand ID など）
    pub namespace: String,
    /// キー（namespace 内で一意）
    pub key: String,
    /// シリアライズされたデータ（JSON）
    pub data: serde_json::Value,
    /// 保存時刻（Unix epoch ミリ秒）
    pub stored_at: u64,
    /// メタデータ（任意の追加情報）
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Disc {
    /// 新しい DISC を作成
    pub fn new(namespace: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            key: key.into(),
            data: serde_json::Value::Null,
            stored_at: now_millis(),
            metadata: HashMap::new(),
        }
    }

    /// データを型付きで抽出（extract = 読み取り）
    pub fn extract<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.data.clone()).map_err(Into::into)
    }

    /// データを型付きで挿入（insert = 書き込み）
    pub fn insert<T: Serialize>(mut self, value: &T) -> Result<Self> {
        self.data = serde_json::to_value(value)?;
        self.stored_at = now_millis();
        Ok(self)
    }

    /// メタデータを追加
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// フルパスを取得（namespace/key）
    pub fn path(&self) -> String {
        format!("{}/{}", self.namespace, self.key)
    }
}

impl fmt::Display for Disc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DISC[{}/{}]", self.namespace, self.key)
    }
}

// =============================================================================
// DiscBackend — 永続化バックエンド trait
// =============================================================================

/// 永続化バックエンドのインターフェース
///
/// File / SurrealDB / Memory を差し替え可能にする抽象レイヤー。
#[async_trait::async_trait]
pub trait DiscBackend: Send + Sync {
    /// DISC を保存（同じ namespace/key は上書き）
    async fn store(&self, disc: &Disc) -> Result<()>;

    /// DISC を取得
    async fn load(&self, namespace: &str, key: &str) -> Result<Option<Disc>>;

    /// namespace 配下の全 DISC を取得
    async fn list(&self, namespace: &str) -> Result<Vec<Disc>>;

    /// DISC を削除
    async fn remove(&self, namespace: &str, key: &str) -> Result<bool>;

    /// namespace 配下の全 DISC を削除
    async fn remove_all(&self, namespace: &str) -> Result<usize>;

    /// prefix に一致する DISC を取得
    async fn list_by_prefix(&self, namespace: &str, key_prefix: &str) -> Result<Vec<Disc>>;

    /// prefix に一致する DISC を削除
    async fn remove_by_prefix(&self, namespace: &str, key_prefix: &str) -> Result<usize>;
}

// =============================================================================
// MemoryBackend — インメモリ実装（テスト用）
// =============================================================================

/// インメモリバックエンド（テスト・開発用）
#[derive(Debug, Default)]
pub struct MemoryBackend {
    store: RwLock<HashMap<String, Disc>>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// 保存されている DISC 数を取得
    pub async fn len(&self) -> usize {
        self.store.read().await.len()
    }

    /// ストアが空かどうか
    pub async fn is_empty(&self) -> bool {
        self.store.read().await.is_empty()
    }

    fn make_key(namespace: &str, key: &str) -> String {
        format!("{}/{}", namespace, key)
    }
}

#[async_trait::async_trait]
impl DiscBackend for MemoryBackend {
    async fn store(&self, disc: &Disc) -> Result<()> {
        let full_key = Self::make_key(&disc.namespace, &disc.key);
        self.store.write().await.insert(full_key, disc.clone());
        Ok(())
    }

    async fn load(&self, namespace: &str, key: &str) -> Result<Option<Disc>> {
        let full_key = Self::make_key(namespace, key);
        Ok(self.store.read().await.get(&full_key).cloned())
    }

    async fn list(&self, namespace: &str) -> Result<Vec<Disc>> {
        let prefix = format!("{}/", namespace);
        let store = self.store.read().await;
        Ok(store
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v.clone())
            .collect())
    }

    async fn remove(&self, namespace: &str, key: &str) -> Result<bool> {
        let full_key = Self::make_key(namespace, key);
        Ok(self.store.write().await.remove(&full_key).is_some())
    }

    async fn remove_all(&self, namespace: &str) -> Result<usize> {
        let prefix = format!("{}/", namespace);
        let mut store = self.store.write().await;
        let keys: Vec<String> = store
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        let count = keys.len();
        for key in keys {
            store.remove(&key);
        }
        Ok(count)
    }

    async fn list_by_prefix(&self, namespace: &str, key_prefix: &str) -> Result<Vec<Disc>> {
        let prefix = format!("{}/{}", namespace, key_prefix);
        let store = self.store.read().await;
        Ok(store
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v.clone())
            .collect())
    }

    async fn remove_by_prefix(&self, namespace: &str, key_prefix: &str) -> Result<usize> {
        let prefix = format!("{}/{}", namespace, key_prefix);
        let mut store = self.store.write().await;
        let keys: Vec<String> = store
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        let count = keys.len();
        for key in keys {
            store.remove(&key);
        }
        Ok(count)
    }
}

// =============================================================================
// FileBackend — ファイルベース実装
// =============================================================================

/// ファイルベースバックエンド（JSON ファイル永続化）
///
/// `{base_dir}/{namespace}/{key}.json` として保存。
#[derive(Debug)]
pub struct FileBackend {
    base_dir: PathBuf,
}

impl FileBackend {
    /// 新しい FileBackend を作成
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// VP のデフォルトディレクトリを使用
    pub fn default_dir() -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("vantage")
            .join("discs");
        Self::new(dir)
    }

    fn disc_path(&self, namespace: &str, key: &str) -> PathBuf {
        // key にスラッシュが含まれる場合、ディレクトリ階層にする
        self.base_dir.join(namespace).join(format!(
            "{}.json",
            key.replace('/', std::path::MAIN_SEPARATOR_STR)
        ))
    }

    fn namespace_dir(&self, namespace: &str) -> PathBuf {
        self.base_dir.join(namespace)
    }

    /// ディレクトリを再帰的にスキャンして DISC を収集する
    ///
    /// key にスラッシュが含まれる場合、サブディレクトリに保存されるため再帰が必要。
    fn collect_discs_recursive<'a>(
        dir: &'a PathBuf,
        discs: &'a mut Vec<Disc>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut entries = tokio::fs::read_dir(dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.is_dir() {
                    FileBackend::collect_discs_recursive(&path, discs).await?;
                } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Ok(json) = tokio::fs::read_to_string(&path).await
                        && let Ok(disc) = serde_json::from_str::<Disc>(&json)
                    {
                        discs.push(disc);
                    }
                }
            }
            Ok(())
        })
    }
}

#[async_trait::async_trait]
impl DiscBackend for FileBackend {
    async fn store(&self, disc: &Disc) -> Result<()> {
        let path = self.disc_path(&disc.namespace, &disc.key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(disc)?;
        tokio::fs::write(&path, json).await?;
        Ok(())
    }

    async fn load(&self, namespace: &str, key: &str) -> Result<Option<Disc>> {
        let path = self.disc_path(namespace, key);
        if !path.exists() {
            return Ok(None);
        }
        let json = tokio::fs::read_to_string(&path).await?;
        let disc: Disc = serde_json::from_str(&json)?;
        Ok(Some(disc))
    }

    async fn list(&self, namespace: &str) -> Result<Vec<Disc>> {
        let dir = self.namespace_dir(namespace);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        // key にスラッシュが含まれる場合、サブディレクトリに保存されるため再帰スキャン
        let mut discs = Vec::new();
        FileBackend::collect_discs_recursive(&dir, &mut discs).await?;
        Ok(discs)
    }

    async fn remove(&self, namespace: &str, key: &str) -> Result<bool> {
        let path = self.disc_path(namespace, key);
        if path.exists() {
            tokio::fs::remove_file(&path).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn remove_all(&self, namespace: &str) -> Result<usize> {
        let dir = self.namespace_dir(namespace);
        if !dir.exists() {
            return Ok(0);
        }
        // サブディレクトリを含む全 DISC を収集してから削除
        let discs = self.list(namespace).await?;
        let count = discs.len();
        for disc in &discs {
            let path = self.disc_path(&disc.namespace, &disc.key);
            if path.exists() {
                tokio::fs::remove_file(&path).await?;
            }
        }
        Ok(count)
    }

    async fn list_by_prefix(&self, namespace: &str, key_prefix: &str) -> Result<Vec<Disc>> {
        let discs = self.list(namespace).await?;
        Ok(discs
            .into_iter()
            .filter(|d| d.key.starts_with(key_prefix))
            .collect())
    }

    async fn remove_by_prefix(&self, namespace: &str, key_prefix: &str) -> Result<usize> {
        let discs = self.list_by_prefix(namespace, key_prefix).await?;
        let mut count = 0;
        for disc in &discs {
            if self.remove(namespace, &disc.key).await? {
                count += 1;
            }
        }
        Ok(count)
    }
}

// =============================================================================
// Whitesnake — メインインターフェース
// =============================================================================

/// Whitesnake 🐍 — 永続化マネージャー
///
/// `Disc` の CRUD を提供し、Backend を通じて永続化する。
/// 各 Capability / モジュールはこのハンドルを通じて状態を保存・復元する。
#[derive(Clone)]
pub struct Whitesnake {
    backend: Arc<dyn DiscBackend>,
}

impl Whitesnake {
    /// 新しい Whitesnake インスタンスを作成
    pub fn new(backend: Arc<dyn DiscBackend>) -> Self {
        Self { backend }
    }

    /// インメモリバックエンドで作成（テスト用）
    pub fn in_memory() -> Self {
        Self::new(Arc::new(MemoryBackend::new()))
    }

    /// ファイルバックエンドで作成（デフォルトディレクトリ）
    pub fn file_backed() -> Self {
        Self::new(Arc::new(FileBackend::default_dir()))
    }

    /// ポート別ディレクトリのファイルバックエンドで作成
    ///
    /// 複数プロセスが同一の namespace/key に同時書き込みするのを防ぐ。
    /// Process は port で一意なので、`discs/{port}/` 配下を専用領域として使う。
    pub fn file_backed_for_port(port: u16) -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("vantage")
            .join("discs")
            .join(port.to_string());
        Self::new(Arc::new(FileBackend::new(dir)))
    }

    /// カスタムディレクトリのファイルバックエンドで作成
    pub fn file_backed_at(dir: impl Into<PathBuf>) -> Self {
        Self::new(Arc::new(FileBackend::new(dir)))
    }

    // ─── DISC 操作 ──────────────────────────────────

    /// 値を DISC に抽出して保存（extract = Whitesnake が記憶を DISC に焼く）
    pub async fn extract<T: Serialize>(&self, namespace: &str, key: &str, value: &T) -> Result<()> {
        let disc = Disc::new(namespace, key).insert(value)?;
        self.backend.store(&disc).await
    }

    /// メタデータ付きで DISC に抽出
    pub async fn extract_with_metadata<T: Serialize>(
        &self,
        namespace: &str,
        key: &str,
        value: &T,
        metadata: HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        let mut disc = Disc::new(namespace, key).insert(value)?;
        disc.metadata = metadata;
        self.backend.store(&disc).await
    }

    /// DISC からデータを挿入して復元（insert = DISC を差し込んで記憶を戻す）
    pub async fn insert<T: DeserializeOwned>(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<T>> {
        match self.backend.load(namespace, key).await? {
            Some(disc) => Ok(Some(disc.extract()?)),
            None => Ok(None),
        }
    }

    /// DISC を生のまま取得
    pub async fn load_disc(&self, namespace: &str, key: &str) -> Result<Option<Disc>> {
        self.backend.load(namespace, key).await
    }

    /// namespace 配下の全 DISC を取得
    pub async fn list_discs(&self, namespace: &str) -> Result<Vec<Disc>> {
        self.backend.list(namespace).await
    }

    /// prefix に一致する DISC を取得
    pub async fn list_by_prefix(&self, namespace: &str, key_prefix: &str) -> Result<Vec<Disc>> {
        self.backend.list_by_prefix(namespace, key_prefix).await
    }

    /// DISC を削除
    pub async fn remove(&self, namespace: &str, key: &str) -> Result<bool> {
        self.backend.remove(namespace, key).await
    }

    /// namespace 配下の全 DISC を削除（Lane 切断時のクリーンアップ等）
    pub async fn remove_all(&self, namespace: &str) -> Result<usize> {
        self.backend.remove_all(namespace).await
    }

    /// prefix に一致する DISC を削除
    pub async fn remove_by_prefix(&self, namespace: &str, key_prefix: &str) -> Result<usize> {
        self.backend.remove_by_prefix(namespace, key_prefix).await
    }
}

impl fmt::Debug for Whitesnake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Whitesnake").finish()
    }
}

// =============================================================================
// ユーティリティ
// =============================================================================

/// 現在時刻を Unix epoch ミリ秒で取得
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_disc_extract_insert() {
        // DISC に値を焼いて取り出す
        let disc = Disc::new("test", "greeting").insert(&"hello").unwrap();
        let value: String = disc.extract().unwrap();
        assert_eq!(value, "hello");
    }

    #[tokio::test]
    async fn test_disc_path() {
        let disc = Disc::new("paisley-park", "pane/main");
        assert_eq!(disc.path(), "paisley-park/pane/main");
    }

    #[tokio::test]
    async fn test_disc_metadata() {
        let disc =
            Disc::new("test", "key").with_metadata("content_type", serde_json::json!("markdown"));
        assert_eq!(
            disc.metadata.get("content_type"),
            Some(&serde_json::json!("markdown"))
        );
    }

    #[tokio::test]
    async fn test_whitesnake_extract_and_insert() {
        let ws = Whitesnake::in_memory();

        // extract: 値を DISC に焼く
        ws.extract("pp", "pane/main", &"# Hello World")
            .await
            .unwrap();

        // insert: DISC から値を戻す
        let value: Option<String> = ws.insert("pp", "pane/main").await.unwrap();
        assert_eq!(value, Some("# Hello World".to_string()));
    }

    #[tokio::test]
    async fn test_whitesnake_list_discs() {
        let ws = Whitesnake::in_memory();

        ws.extract("pp", "pane/main", &"Main content")
            .await
            .unwrap();
        ws.extract("pp", "pane/side", &"Side content")
            .await
            .unwrap();
        ws.extract("hd", "session/abc", &"HD session")
            .await
            .unwrap();

        // namespace 単位で一覧
        let pp_discs = ws.list_discs("pp").await.unwrap();
        assert_eq!(pp_discs.len(), 2);

        let hd_discs = ws.list_discs("hd").await.unwrap();
        assert_eq!(hd_discs.len(), 1);
    }

    #[tokio::test]
    async fn test_whitesnake_remove() {
        let ws = Whitesnake::in_memory();

        ws.extract("pp", "pane/main", &"content").await.unwrap();
        assert!(ws.remove("pp", "pane/main").await.unwrap());

        let value: Option<String> = ws.insert("pp", "pane/main").await.unwrap();
        assert!(value.is_none());
    }

    #[tokio::test]
    async fn test_whitesnake_remove_all_namespace() {
        let ws = Whitesnake::in_memory();

        ws.extract("lane-a", "pane/1", &"a1").await.unwrap();
        ws.extract("lane-a", "pane/2", &"a2").await.unwrap();
        ws.extract("lane-b", "pane/1", &"b1").await.unwrap();

        // Lane A のクリーンアップ（Lane 切断時の想定）
        let removed = ws.remove_all("lane-a").await.unwrap();
        assert_eq!(removed, 2);

        // Lane B は無傷
        let remaining = ws.list_discs("lane-b").await.unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[tokio::test]
    async fn test_whitesnake_list_by_prefix() {
        let ws = Whitesnake::in_memory();

        ws.extract("pp", "pane/main", &"main").await.unwrap();
        ws.extract("pp", "pane/side", &"side").await.unwrap();
        ws.extract("pp", "layout", &"{}").await.unwrap();

        // pane/ プレフィックスで絞り込み
        let panes = ws.list_by_prefix("pp", "pane/").await.unwrap();
        assert_eq!(panes.len(), 2);
    }

    #[tokio::test]
    async fn test_whitesnake_remove_by_prefix() {
        let ws = Whitesnake::in_memory();

        ws.extract("mailbox", "msg/001", &"msg1").await.unwrap();
        ws.extract("mailbox", "msg/002", &"msg2").await.unwrap();
        ws.extract("mailbox", "config", &"cfg").await.unwrap();

        // msg/ だけ削除
        let removed = ws.remove_by_prefix("mailbox", "msg/").await.unwrap();
        assert_eq!(removed, 2);

        // config は残る
        let remaining = ws.list_discs("mailbox").await.unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[tokio::test]
    async fn test_whitesnake_overwrite() {
        let ws = Whitesnake::in_memory();

        ws.extract("pp", "pane/main", &"v1").await.unwrap();
        ws.extract("pp", "pane/main", &"v2").await.unwrap();

        let value: Option<String> = ws.insert("pp", "pane/main").await.unwrap();
        assert_eq!(value, Some("v2".to_string()));
    }

    #[tokio::test]
    async fn test_whitesnake_complex_types() {
        let ws = Whitesnake::in_memory();

        #[derive(Debug, Serialize, Deserialize, PartialEq)]
        struct PaneState {
            content: String,
            title: Option<String>,
        }

        let state = PaneState {
            content: "# Hello".to_string(),
            title: Some("Main".to_string()),
        };

        ws.extract("pp", "pane/main", &state).await.unwrap();
        let restored: Option<PaneState> = ws.insert("pp", "pane/main").await.unwrap();
        assert_eq!(restored, Some(state));
    }

    #[tokio::test]
    async fn test_file_backend_roundtrip() {
        // 一時ディレクトリでテスト
        let tmp = tempfile::tempdir().unwrap();
        let ws = Whitesnake::file_backed_at(tmp.path());

        ws.extract("pp", "pane/main", &"file backed content")
            .await
            .unwrap();

        let value: Option<String> = ws.insert("pp", "pane/main").await.unwrap();
        assert_eq!(value, Some("file backed content".to_string()));

        // ファイルが実際に作成されている
        let path = tmp.path().join("pp").join("pane/main.json");
        assert!(path.exists() || tmp.path().join("pp").exists());
    }
}
