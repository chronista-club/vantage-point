//! Gold Experience Requiem (GER) - 守護レイヤー
//!
//! JoJo's Bizarre Adventure Part 5 のジョルノ・ジョバーナのスタンド
//! 「真実にはたどり着けない」- 危険な変更を本番に到達させない
//!
//! ## 機能
//! - **Guardian**: 自動防御 - 危険な操作を検出してブロック
//! - **Revert**: ゼロに戻す - 任意時点への状態巻き戻し
//! - **Watch**: 無限ループ - バックグラウンド監視
//!
//! ## アーキテクチャ
//! GER は The World の守護レイヤーとして動作する。
//! The World が「世界（環境）」を統括し、GER がその世界を「守る」。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// スナップショット情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// スナップショット名
    pub name: String,
    /// 説明
    pub description: Option<String>,
    /// 作成日時
    pub created_at: DateTime<Utc>,
    /// スナップショットのパス
    pub path: PathBuf,
    /// メタデータ
    pub metadata: HashMap<String, String>,
}

/// Guardian ルール
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardianRule {
    /// ルール名
    pub name: String,
    /// パターン（glob形式）
    pub pattern: String,
    /// 有効/無効
    pub enabled: bool,
    /// アクション（block, warn, log）
    pub action: GuardianAction,
    /// 作成日時
    pub created_at: DateTime<Utc>,
}

/// Guardian アクション
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GuardianAction {
    /// 操作をブロック
    #[default]
    Block,
    /// 警告のみ
    Warn,
    /// ログ記録のみ
    Log,
}

/// Guardian ステータス
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardianStatus {
    /// 有効/無効
    pub enabled: bool,
    /// ルール数
    pub rule_count: usize,
    /// ブロック回数
    pub block_count: u64,
    /// 最終チェック日時
    pub last_check: Option<DateTime<Utc>>,
}

/// GER (Gold Experience Requiem) 設定
#[derive(Debug, Clone, Default)]
pub struct GerConfig {
    /// Requiem モード有効
    pub requiem_mode: bool,
    /// スナップショット保存ディレクトリ
    pub snapshot_dir: Option<PathBuf>,
    /// Guardian 有効
    pub guardian_enabled: bool,
}

/// Gold Experience Requiem
///
/// The World の守護レイヤーとして、自動防御とスナップショット管理を提供
pub struct GoldExperienceRequiem {
    /// 設定
    config: GerConfig,
    /// スナップショット一覧
    snapshots: Arc<RwLock<HashMap<String, Snapshot>>>,
    /// Guardian ルール
    guardian_rules: Arc<RwLock<Vec<GuardianRule>>>,
    /// Guardian 統計
    guardian_stats: Arc<RwLock<GuardianStats>>,
}

/// Guardian 統計
#[derive(Debug, Clone, Default)]
struct GuardianStats {
    enabled: bool,
    block_count: u64,
    warn_count: u64,
    last_check: Option<DateTime<Utc>>,
}

impl GoldExperienceRequiem {
    /// 新しい GER インスタンスを作成
    pub fn new(config: GerConfig) -> Self {
        Self {
            config,
            snapshots: Arc::new(RwLock::new(HashMap::new())),
            guardian_rules: Arc::new(RwLock::new(Vec::new())),
            guardian_stats: Arc::new(RwLock::new(GuardianStats::default())),
        }
    }

    /// Requiem モードを有効化
    pub async fn enable_requiem(&mut self) {
        self.config.requiem_mode = true;
        tracing::info!("GER: Requiem モード有効化「真実にはたどり着けない」");
    }

    // =========================================================================
    // スナップショット機能「時を止める」
    // =========================================================================

    /// スナップショットを作成
    pub async fn create_snapshot(
        &self,
        name: &str,
        description: Option<&str>,
        target_dir: &Path,
    ) -> Result<Snapshot> {
        let snapshot_dir = self.get_snapshot_dir()?;
        let snapshot_path = snapshot_dir.join(name);

        // ディレクトリ作成
        tokio::fs::create_dir_all(&snapshot_path).await?;

        // TODO: 実際のスナップショット処理（git stash、ファイルコピー等）
        // 現時点ではメタデータのみ保存

        let snapshot = Snapshot {
            name: name.to_string(),
            description: description.map(String::from),
            created_at: Utc::now(),
            path: snapshot_path.clone(),
            metadata: {
                let mut m = HashMap::new();
                m.insert("target".to_string(), target_dir.display().to_string());
                m
            },
        };

        // メタデータを保存
        let meta_path = snapshot_path.join("snapshot.json");
        let meta_json = serde_json::to_string_pretty(&snapshot)?;
        tokio::fs::write(&meta_path, meta_json).await?;

        // メモリに保存
        {
            let mut snapshots = self.snapshots.write().await;
            snapshots.insert(name.to_string(), snapshot.clone());
        }

        tracing::info!("GER: スナップショット作成「時よ止まれ」- {}", name);
        Ok(snapshot)
    }

    /// スナップショットから復元「ゼロに戻す」
    pub async fn restore_snapshot(&self, name: &str) -> Result<()> {
        let snapshots = self.snapshots.read().await;
        let snapshot = snapshots
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("スナップショット '{}' が見つかりません", name))?;

        // TODO: 実際の復元処理
        tracing::info!(
            "GER: スナップショット復元「ゼロに戻す」- {} ({})",
            name,
            snapshot.created_at
        );

        Ok(())
    }

    /// スナップショット一覧を取得
    pub async fn list_snapshots(&self) -> Vec<Snapshot> {
        let snapshots = self.snapshots.read().await;
        let mut list: Vec<_> = snapshots.values().cloned().collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }

    /// スナップショットディレクトリを取得
    fn get_snapshot_dir(&self) -> Result<PathBuf> {
        if let Some(ref dir) = self.config.snapshot_dir {
            return Ok(dir.clone());
        }

        // デフォルト: ~/.config/vantage/snapshots
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow::anyhow!("設定ディレクトリが見つかりません"))?;
        Ok(config_dir.join("vantage").join("snapshots"))
    }

    /// 保存済みスナップショットを読み込み
    pub async fn load_snapshots(&self) -> Result<()> {
        let snapshot_dir = self.get_snapshot_dir()?;
        if !snapshot_dir.exists() {
            return Ok(());
        }

        let mut entries = tokio::fs::read_dir(&snapshot_dir).await?;
        let mut snapshots = self.snapshots.write().await;

        while let Some(entry) = entries.next_entry().await? {
            let meta_path = entry.path().join("snapshot.json");
            if meta_path.exists() {
                if let Ok(content) = tokio::fs::read_to_string(&meta_path).await {
                    if let Ok(snapshot) = serde_json::from_str::<Snapshot>(&content) {
                        snapshots.insert(snapshot.name.clone(), snapshot);
                    }
                }
            }
        }

        tracing::debug!("GER: {} 個のスナップショットを読み込み", snapshots.len());
        Ok(())
    }

    // =========================================================================
    // Guardian 機能（自動防御）
    // =========================================================================

    /// Guardian を有効化
    pub async fn enable_guardian(&self) {
        let mut stats = self.guardian_stats.write().await;
        stats.enabled = true;
        tracing::info!("GER: Guardian 有効化「自動防御発動」");
    }

    /// Guardian を無効化
    pub async fn disable_guardian(&self) {
        let mut stats = self.guardian_stats.write().await;
        stats.enabled = false;
        tracing::info!("GER: Guardian 無効化");
    }

    /// Guardian のステータスを取得
    pub async fn guardian_status(&self) -> GuardianStatus {
        let stats = self.guardian_stats.read().await;
        let rules = self.guardian_rules.read().await;

        GuardianStatus {
            enabled: stats.enabled,
            rule_count: rules.len(),
            block_count: stats.block_count,
            last_check: stats.last_check,
        }
    }

    /// ルールを追加
    pub async fn add_rule(&self, name: &str, pattern: &str, action: GuardianAction) -> Result<()> {
        let rule = GuardianRule {
            name: name.to_string(),
            pattern: pattern.to_string(),
            enabled: true,
            action,
            created_at: Utc::now(),
        };

        let mut rules = self.guardian_rules.write().await;
        rules.push(rule);

        tracing::info!("GER: ルール追加 - {} ({})", name, pattern);
        Ok(())
    }

    /// ルール一覧を取得
    pub async fn list_rules(&self) -> Vec<GuardianRule> {
        let rules = self.guardian_rules.read().await;
        rules.clone()
    }

    /// 操作をチェック（Guardian）
    ///
    /// 危険な操作かどうかをチェックし、必要に応じてブロック
    pub async fn check_operation(&self, operation: &str, target: &str) -> Result<bool> {
        let stats = self.guardian_stats.read().await;
        if !stats.enabled {
            return Ok(true); // Guardian 無効時は常に許可
        }
        drop(stats);

        let rules = self.guardian_rules.read().await;
        for rule in rules.iter() {
            if !rule.enabled {
                continue;
            }

            // パターンマッチ（簡易的なglob）
            if Self::matches_pattern(&rule.pattern, target) {
                match rule.action {
                    GuardianAction::Block => {
                        let mut stats = self.guardian_stats.write().await;
                        stats.block_count += 1;
                        stats.last_check = Some(Utc::now());
                        tracing::warn!(
                            "GER: 操作ブロック「真実にはたどり着けない」- {} on {}",
                            operation,
                            target
                        );
                        return Ok(false);
                    }
                    GuardianAction::Warn => {
                        let mut stats = self.guardian_stats.write().await;
                        stats.warn_count += 1;
                        stats.last_check = Some(Utc::now());
                        tracing::warn!(
                            "GER: 警告 - {} on {} (rule: {})",
                            operation,
                            target,
                            rule.name
                        );
                    }
                    GuardianAction::Log => {
                        let mut stats = self.guardian_stats.write().await;
                        stats.last_check = Some(Utc::now());
                        tracing::info!(
                            "GER: ログ - {} on {} (rule: {})",
                            operation,
                            target,
                            rule.name
                        );
                    }
                }
            }
        }

        Ok(true)
    }

    /// 簡易パターンマッチ
    fn matches_pattern(pattern: &str, target: &str) -> bool {
        // 簡易実装: * をワイルドカードとして扱う
        if pattern == "*" {
            return true;
        }
        if pattern.starts_with('*') && pattern.ends_with('*') {
            let middle = &pattern[1..pattern.len() - 1];
            return target.contains(middle);
        }
        if pattern.starts_with('*') {
            let suffix = &pattern[1..];
            return target.ends_with(suffix);
        }
        if pattern.ends_with('*') {
            let prefix = &pattern[..pattern.len() - 1];
            return target.starts_with(prefix);
        }
        target == pattern
    }
}

impl Default for GoldExperienceRequiem {
    fn default() -> Self {
        Self::new(GerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_matching() {
        assert!(GoldExperienceRequiem::matches_pattern("*", "anything"));
        assert!(GoldExperienceRequiem::matches_pattern("*.rs", "main.rs"));
        assert!(GoldExperienceRequiem::matches_pattern(
            "src/*",
            "src/lib.rs"
        ));
        assert!(GoldExperienceRequiem::matches_pattern(
            "*secret*",
            "my_secret_file.txt"
        ));
        assert!(!GoldExperienceRequiem::matches_pattern("*.rs", "main.txt"));
    }
}
