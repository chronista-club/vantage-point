//! Update Capability - オートアップデート機能
//!
//! GitHub Releasesを使用してバージョンチェックと更新を行うCapability。
//!
//! ## 機能
//!
//! - バージョンチェック（GitHub Releases API）
//! - 更新通知イベント発行
//! - バイナリダウンロード（Phase 2以降）
//!
//! ## 使用例
//!
//! ```ignore
//! let mut update = UpdateCapability::new();
//! update.initialize(&ctx).await?;
//!
//! // バージョンチェック
//! if let Some(release) = update.check_update().await? {
//!     println!("新バージョン: {}", release.version);
//! }
//! ```

use crate::capability::core::{
    Capability, CapabilityContext, CapabilityError, CapabilityResult,
};
use crate::capability::{CapabilityEvent, CapabilityInfo, CapabilityState};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::any::Any;

/// GitHubリポジトリ情報
const GITHUB_OWNER: &str = "chronista-club";
const GITHUB_REPO: &str = "vantage-point";

/// 現在のバージョン
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// リリース情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    /// バージョン（v0.2.3 など）
    pub version: String,
    /// リリースタグ
    pub tag_name: String,
    /// リリース名
    pub name: Option<String>,
    /// リリースノート
    pub body: Option<String>,
    /// 公開日時
    pub published_at: Option<String>,
    /// HTML URL
    pub html_url: String,
    /// アセット情報
    pub assets: Vec<AssetInfo>,
}

/// アセット情報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetInfo {
    /// ファイル名
    pub name: String,
    /// ダウンロードURL
    pub browser_download_url: String,
    /// ファイルサイズ（バイト）
    pub size: u64,
    /// コンテンツタイプ
    pub content_type: String,
}

/// 更新チェック結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheckResult {
    /// 現在のバージョン
    pub current_version: String,
    /// 最新バージョン
    pub latest_version: String,
    /// 更新が利用可能か
    pub update_available: bool,
    /// リリース情報（更新がある場合）
    pub release: Option<ReleaseInfo>,
}

/// GitHub Releases APIレスポンス
#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    published_at: Option<String>,
    html_url: String,
    assets: Vec<GitHubAsset>,
}

/// GitHub Assetレスポンス
#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    content_type: String,
}

/// Update Capability
pub struct UpdateCapability {
    /// 現在の状態
    state: CapabilityState,
    /// HTTPクライアント
    client: reqwest::Client,
    /// キャッシュされた最新リリース情報
    cached_release: Option<ReleaseInfo>,
    /// 最終チェック時刻
    last_check: Option<std::time::Instant>,
}

impl UpdateCapability {
    /// 新しいUpdateCapabilityを作成
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(format!("vantage-point/{}", CURRENT_VERSION))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        Self {
            state: CapabilityState::Uninitialized,
            client,
            cached_release: None,
            last_check: None,
        }
    }

    /// 現在のバージョンを取得
    pub fn current_version(&self) -> &str {
        CURRENT_VERSION
    }

    /// 更新をチェック
    pub async fn check_update(&mut self) -> CapabilityResult<UpdateCheckResult> {
        // キャッシュが5分以内なら再利用
        if let (Some(release), Some(last_check)) = (&self.cached_release, &self.last_check) {
            if last_check.elapsed() < std::time::Duration::from_secs(300) {
                let update_available = is_newer_version(&release.version, CURRENT_VERSION);
                return Ok(UpdateCheckResult {
                    current_version: CURRENT_VERSION.to_string(),
                    latest_version: release.version.clone(),
                    update_available,
                    release: if update_available { Some(release.clone()) } else { None },
                });
            }
        }

        // GitHub Releases APIを呼び出し
        let release = self.fetch_latest_release().await?;
        let update_available = is_newer_version(&release.version, CURRENT_VERSION);

        // キャッシュを更新
        self.cached_release = Some(release.clone());
        self.last_check = Some(std::time::Instant::now());

        Ok(UpdateCheckResult {
            current_version: CURRENT_VERSION.to_string(),
            latest_version: release.version.clone(),
            update_available,
            release: if update_available { Some(release) } else { None },
        })
    }

    /// GitHub Releasesから最新リリースを取得
    async fn fetch_latest_release(&self) -> CapabilityResult<ReleaseInfo> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            GITHUB_OWNER, GITHUB_REPO
        );

        tracing::debug!(url = %url, "Fetching latest release");

        let response = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to fetch release: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(CapabilityError::Other(format!(
                "GitHub API error: {} - {}",
                status, body
            )));
        }

        let github_release: GitHubRelease = response
            .json()
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to parse release: {}", e)))?;

        // バージョン番号を抽出（v0.2.3 → 0.2.3）
        let version = github_release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&github_release.tag_name)
            .to_string();

        Ok(ReleaseInfo {
            version,
            tag_name: github_release.tag_name,
            name: github_release.name,
            body: github_release.body,
            published_at: github_release.published_at,
            html_url: github_release.html_url,
            assets: github_release
                .assets
                .into_iter()
                .map(|a| AssetInfo {
                    name: a.name,
                    browser_download_url: a.browser_download_url,
                    size: a.size,
                    content_type: a.content_type,
                })
                .collect(),
        })
    }

    /// 現在のプラットフォーム用のアセットを検索
    pub fn find_platform_asset<'a>(&self, release: &'a ReleaseInfo) -> Option<&'a AssetInfo> {
        let target = current_platform_target();
        release.assets.iter().find(move |a| a.name.contains(&target))
    }
}

impl Default for UpdateCapability {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Capability for UpdateCapability {
    fn info(&self) -> CapabilityInfo {
        CapabilityInfo::new(
            "update-capability",
            CURRENT_VERSION,
            "オートアップデート - GitHub Releasesからの更新チェック",
        )
    }

    fn state(&self) -> CapabilityState {
        self.state
    }

    async fn initialize(&mut self, _ctx: &CapabilityContext) -> CapabilityResult<()> {
        if self.state != CapabilityState::Uninitialized {
            return Err(CapabilityError::AlreadyInitialized);
        }

        self.state = CapabilityState::Initializing;

        // 起動時にバージョンチェック（バックグラウンドで）
        tracing::info!(
            version = CURRENT_VERSION,
            "UpdateCapability initialized"
        );

        self.state = CapabilityState::Idle;
        Ok(())
    }

    async fn shutdown(&mut self) -> CapabilityResult<()> {
        self.state = CapabilityState::Stopped;
        tracing::info!("UpdateCapability shutdown");
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec!["update.*".to_string()]
    }

    async fn handle_event(
        &mut self,
        event: &CapabilityEvent,
        _ctx: &CapabilityContext,
    ) -> CapabilityResult<()> {
        // update.check イベントで更新チェックを実行
        if event.event_type == "update.check" {
            let _ = self.check_update().await;
        }

        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// バージョン比較（semver）
/// latest が current より新しければ true
fn is_newer_version(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<(u32, u32, u32)> {
        let v = v.strip_prefix('v').unwrap_or(v);
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() >= 3 {
            Some((
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].split('-').next()?.parse().ok()?,
            ))
        } else if parts.len() == 2 {
            Some((
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                0,
            ))
        } else {
            None
        }
    };

    match (parse(latest), parse(current)) {
        (Some((l_major, l_minor, l_patch)), Some((c_major, c_minor, c_patch))) => {
            (l_major, l_minor, l_patch) > (c_major, c_minor, c_patch)
        }
        _ => false,
    }
}

/// 現在のプラットフォームのターゲット名を取得
fn current_platform_target() -> String {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return "aarch64-apple-darwin".to_string();

    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return "x86_64-apple-darwin".to_string();

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return "x86_64-unknown-linux-gnu".to_string();

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return "x86_64-pc-windows-msvc".to_string();

    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    return "unknown".to_string();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_version() {
        // 新しいバージョン
        assert!(is_newer_version("0.3.0", "0.2.2"));
        assert!(is_newer_version("0.2.3", "0.2.2"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
        assert!(is_newer_version("v0.3.0", "0.2.2"));

        // 同じバージョン
        assert!(!is_newer_version("0.2.2", "0.2.2"));
        assert!(!is_newer_version("v0.2.2", "0.2.2"));

        // 古いバージョン
        assert!(!is_newer_version("0.2.1", "0.2.2"));
        assert!(!is_newer_version("0.1.0", "0.2.2"));
    }

    #[test]
    fn test_update_capability_new() {
        let cap = UpdateCapability::new();
        assert_eq!(cap.state(), CapabilityState::Uninitialized);
        assert_eq!(cap.current_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn test_current_platform_target() {
        let target = current_platform_target();
        assert!(!target.is_empty());
        // macOS ARM
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(target, "aarch64-apple-darwin");
    }
}
