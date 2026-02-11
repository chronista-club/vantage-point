//! Update Capability - オートアップデート機能
//!
//! GitHub Releasesを使用してバージョンチェックと更新を行うCapability。
//!
//! ## 機能
//!
//! - バージョンチェック（GitHub Releases API）
//! - 更新通知イベント発行
//! - バイナリダウンロード＆適用（Phase 2）
//!
//! ## 使用例
//!
//! ```ignore
//! let mut update = UpdateCapability::new();
//! update.initialize(&ctx).await?;
//!
//! // バージョンチェック
//! let result = update.check_update().await?;
//! if result.update_available {
//!     // 更新を適用
//!     let apply_result = update.apply_update(&result.release.unwrap()).await?;
//!     println!("更新完了: {:?}", apply_result);
//! }
//! ```

use crate::capability::core::{Capability, CapabilityContext, CapabilityError, CapabilityResult};
use crate::capability::{CapabilityEvent, CapabilityInfo, CapabilityState};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

/// GitHubリポジトリ情報（vp CLI）
const GITHUB_OWNER: &str = "chronista-club";
const GITHUB_REPO: &str = "vantage-point";

/// GitHubリポジトリ情報（VantagePoint.app）
const MAC_APP_GITHUB_OWNER: &str = "chronista-club";
const MAC_APP_GITHUB_REPO: &str = "vantage-point-mac";

/// VantagePoint.appのバンドルID
const MAC_APP_BUNDLE_ID: &str = "club.chronista.VantagePoint";

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

/// 更新適用結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateApplyResult {
    /// 成功したか
    pub success: bool,
    /// 更新前のバージョン
    pub previous_version: String,
    /// 更新後のバージョン
    pub new_version: String,
    /// バイナリパス
    pub binary_path: String,
    /// バックアップパス（ロールバック用）
    pub backup_path: Option<String>,
    /// メッセージ
    pub message: String,
    /// 再起動が必要か
    pub restart_required: bool,
}

/// Macアプリ更新チェック結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacAppUpdateCheckResult {
    /// 現在のバージョン
    pub current_version: String,
    /// 最新バージョン
    pub latest_version: String,
    /// 更新が利用可能か
    pub update_available: bool,
    /// リリース情報（更新がある場合）
    pub release: Option<ReleaseInfo>,
    /// アプリパス
    pub app_path: Option<String>,
}

/// Macアプリ更新適用結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MacAppUpdateApplyResult {
    /// 成功したか
    pub success: bool,
    /// 更新前のバージョン
    pub previous_version: String,
    /// 更新後のバージョン
    pub new_version: String,
    /// アプリパス
    pub app_path: String,
    /// バックアップパス（ロールバック用）
    pub backup_path: Option<String>,
    /// メッセージ
    pub message: String,
    /// 再起動が必要か
    pub restart_required: bool,
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
    /// GitHub Token（プライベートリポジトリ用）
    github_token: Option<String>,
}

impl UpdateCapability {
    /// 新しいUpdateCapabilityを作成
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(format!("vantage-point/{}", CURRENT_VERSION))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        // GitHub Token を取得（gh auth token または環境変数）
        let github_token = Self::get_github_token();

        Self {
            state: CapabilityState::Uninitialized,
            client,
            cached_release: None,
            last_check: None,
            github_token,
        }
    }

    /// GitHub Tokenを取得
    fn get_github_token() -> Option<String> {
        // 1. 環境変数から
        if let Ok(token) = std::env::var("GITHUB_TOKEN")
            && !token.is_empty()
        {
            return Some(token);
        }

        // 2. gh auth token から
        if let Ok(output) = std::process::Command::new("gh")
            .args(["auth", "token"])
            .output()
            && output.status.success()
        {
            let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }

        None
    }

    /// 現在のバージョンを取得
    pub fn current_version(&self) -> &str {
        CURRENT_VERSION
    }

    /// 更新をチェック
    pub async fn check_update(&mut self) -> CapabilityResult<UpdateCheckResult> {
        // キャッシュが5分以内なら再利用
        if let (Some(release), Some(last_check)) = (&self.cached_release, &self.last_check)
            && last_check.elapsed() < std::time::Duration::from_secs(300)
        {
            let update_available = is_newer_version(&release.version, CURRENT_VERSION);
            return Ok(UpdateCheckResult {
                current_version: CURRENT_VERSION.to_string(),
                latest_version: release.version.clone(),
                update_available,
                release: if update_available {
                    Some(release.clone())
                } else {
                    None
                },
            });
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
            release: if update_available {
                Some(release)
            } else {
                None
            },
        })
    }

    /// GitHub Releasesから最新リリースを取得
    async fn fetch_latest_release(&self) -> CapabilityResult<ReleaseInfo> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            GITHUB_OWNER, GITHUB_REPO
        );

        tracing::debug!(url = %url, "Fetching latest release");

        let mut request = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");

        // GitHub Tokenがあれば認証ヘッダを追加（プライベートリポジトリ対応）
        if let Some(ref token) = self.github_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request
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
        release
            .assets
            .iter()
            .find(move |a| a.name.contains(&target))
    }

    /// 更新を適用（ダウンロード→バックアップ→置換）
    pub async fn apply_update(&self, release: &ReleaseInfo) -> CapabilityResult<UpdateApplyResult> {
        // 1. プラットフォーム用アセットを検索
        let asset = self.find_platform_asset(release).ok_or_else(|| {
            CapabilityError::Other(format!(
                "No binary found for platform: {}",
                current_platform_target()
            ))
        })?;

        tracing::info!(
            version = %release.version,
            asset = %asset.name,
            size = asset.size,
            "Starting update download"
        );

        // 2. 現在のバイナリパスを取得
        let binary_path = find_current_binary()?;
        let backup_path = binary_path.with_extension("backup");

        // 3. バイナリをダウンロード（一時ファイルに）
        let temp_path = binary_path.with_extension("new");
        self.download_binary(asset, &temp_path).await?;

        // 4. 現在のバイナリをバックアップ
        if binary_path.exists() {
            tokio::fs::copy(&binary_path, &backup_path)
                .await
                .map_err(|e| CapabilityError::Other(format!("Failed to create backup: {}", e)))?;
            tracing::info!(backup = %backup_path.display(), "Created backup");
        }

        // 5. 新しいバイナリを配置
        match self.replace_binary(&temp_path, &binary_path).await {
            Ok(_) => {
                // 一時ファイルを削除
                let _ = tokio::fs::remove_file(&temp_path).await;

                tracing::info!(
                    version = %release.version,
                    path = %binary_path.display(),
                    "Update applied successfully"
                );

                Ok(UpdateApplyResult {
                    success: true,
                    previous_version: CURRENT_VERSION.to_string(),
                    new_version: release.version.clone(),
                    binary_path: binary_path.display().to_string(),
                    backup_path: Some(backup_path.display().to_string()),
                    message: format!(
                        "Updated from {} to {}. Restart required.",
                        CURRENT_VERSION, release.version
                    ),
                    restart_required: true,
                })
            }
            Err(e) => {
                // ロールバック
                tracing::error!(error = %e, "Update failed, rolling back");
                if backup_path.exists() {
                    let _ = tokio::fs::copy(&backup_path, &binary_path).await;
                    let _ = tokio::fs::remove_file(&backup_path).await;
                }
                let _ = tokio::fs::remove_file(&temp_path).await;

                Err(CapabilityError::Other(format!(
                    "Update failed (rolled back): {}",
                    e
                )))
            }
        }
    }

    /// バイナリをダウンロード
    async fn download_binary(&self, asset: &AssetInfo, dest: &PathBuf) -> CapabilityResult<()> {
        tracing::debug!(url = %asset.browser_download_url, "Downloading binary");

        let mut request = self
            .client
            .get(&asset.browser_download_url)
            .header("Accept", "application/octet-stream");

        // GitHub Tokenがあれば認証ヘッダを追加
        if let Some(ref token) = self.github_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request
            .send()
            .await
            .map_err(|e| CapabilityError::Other(format!("Download failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(CapabilityError::Other(format!(
                "Download failed: HTTP {}",
                response.status()
            )));
        }

        // ストリーミングダウンロード
        let bytes = response
            .bytes()
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to read response: {}", e)))?;

        // ファイルに書き込み
        let mut file = tokio::fs::File::create(dest)
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to create file: {}", e)))?;

        file.write_all(&bytes)
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to write file: {}", e)))?;

        file.flush()
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to flush file: {}", e)))?;

        tracing::info!(
            path = %dest.display(),
            size = bytes.len(),
            "Binary downloaded"
        );

        Ok(())
    }

    /// バイナリを置換（実行権限を設定）
    async fn replace_binary(&self, src: &PathBuf, dest: &PathBuf) -> CapabilityResult<()> {
        // コピー（moveだと異なるファイルシステム間で失敗する可能性がある）
        tokio::fs::copy(src, dest)
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to copy binary: {}", e)))?;

        // 実行権限を設定（Unix系のみ）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            tokio::fs::set_permissions(dest, perms)
                .await
                .map_err(|e| CapabilityError::Other(format!("Failed to set permissions: {}", e)))?;
        }

        Ok(())
    }

    /// ロールバックを実行
    pub async fn rollback(&self, backup_path: &str) -> CapabilityResult<()> {
        let backup = PathBuf::from(backup_path);
        if !backup.exists() {
            return Err(CapabilityError::Other("Backup file not found".to_string()));
        }

        let binary_path = find_current_binary()?;

        tokio::fs::copy(&backup, &binary_path)
            .await
            .map_err(|e| CapabilityError::Other(format!("Rollback failed: {}", e)))?;

        // 実行権限を設定（Unix系のみ）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            tokio::fs::set_permissions(&binary_path, perms)
                .await
                .map_err(|e| CapabilityError::Other(format!("Failed to set permissions: {}", e)))?;
        }

        tracing::info!(
            backup = %backup_path,
            binary = %binary_path.display(),
            "Rollback completed"
        );

        Ok(())
    }

    /// アプリケーションを再起動する
    ///
    /// # Arguments
    /// * `app_path` - 再起動するアプリのパス（.app bundleまたはバイナリ）
    /// * `delay_seconds` - 再起動までの遅延秒数（デフォルト: 1秒）
    ///
    /// # Note
    /// この関数を呼び出すと、現在のプロセスは終了されます。
    /// 呼び出し側が適切にクリーンアップを行ってから呼び出してください。
    pub async fn restart_app(app_path: &str, delay_seconds: u32) -> CapabilityResult<()> {
        let delay = delay_seconds.max(1); // 最低1秒の遅延

        tracing::info!(
            app_path = %app_path,
            delay = delay,
            "Scheduling app restart"
        );

        // 再起動スクリプトを生成
        // 遅延後にアプリを起動し、このプロセスは終了
        let script = format!(
            r#"
            sleep {delay}
            open "{app_path}"
            "#,
            delay = delay,
            app_path = app_path.replace('"', r#"\""#)
        );

        // バックグラウンドでスクリプトを実行
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // プロセスを切り離して実行（親プロセス終了後も継続）
        #[cfg(unix)]
        {
            unsafe {
                cmd.pre_exec(|| {
                    // 新しいセッションを作成（親プロセスから独立）
                    libc::setsid();
                    Ok(())
                });
            }
        }

        cmd.spawn().map_err(|e| {
            CapabilityError::Other(format!("Failed to spawn restart script: {}", e))
        })?;

        tracing::info!(
            "Restart script spawned, app will restart in {} seconds",
            delay
        );

        Ok(())
    }

    /// 現在実行中のvpバイナリを再起動する
    pub async fn restart_self(delay_seconds: u32) -> CapabilityResult<()> {
        let binary_path = find_current_binary()?;
        Self::restart_app(&binary_path.display().to_string(), delay_seconds).await
    }

    // =========================================================================
    // VantagePoint.app 更新機能
    // =========================================================================

    /// VantagePoint.app の更新をチェック
    ///
    /// # Arguments
    /// * `current_version` - 現在のアプリバージョン（Info.plistから取得）
    pub async fn check_mac_update(
        &mut self,
        current_version: &str,
    ) -> CapabilityResult<MacAppUpdateCheckResult> {
        let release = self.fetch_mac_latest_release().await?;
        let update_available = is_newer_version(&release.version, current_version);

        // アプリパスを検索
        let app_path = find_mac_app_path();

        Ok(MacAppUpdateCheckResult {
            current_version: current_version.to_string(),
            latest_version: release.version.clone(),
            update_available,
            release: if update_available {
                Some(release)
            } else {
                None
            },
            app_path: app_path.map(|p| p.display().to_string()),
        })
    }

    /// VantagePoint.app の最新リリースを取得
    async fn fetch_mac_latest_release(&self) -> CapabilityResult<ReleaseInfo> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/releases/latest",
            MAC_APP_GITHUB_OWNER, MAC_APP_GITHUB_REPO
        );

        tracing::debug!(url = %url, "Fetching latest Mac app release");

        let mut request = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");

        if let Some(ref token) = self.github_token {
            request = request.header("Authorization", format!("Bearer {}", token));
        }

        let response = request
            .send()
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to fetch Mac release: {}", e)))?;

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

    /// VantagePoint.app の更新を適用
    ///
    /// # Arguments
    /// * `release` - 適用するリリース情報
    /// * `current_version` - 現在のバージョン
    /// * `app_path` - 現在のアプリパス（省略時は自動検索）
    pub async fn apply_mac_update(
        &self,
        release: &ReleaseInfo,
        current_version: &str,
        app_path: Option<&str>,
    ) -> CapabilityResult<MacAppUpdateApplyResult> {
        // アプリパスを決定
        let app_path = match app_path {
            Some(p) => PathBuf::from(p),
            None => find_mac_app_path()
                .ok_or_else(|| CapabilityError::Other("VantagePoint.app not found".to_string()))?,
        };

        // .zipアセットを検索
        let zip_asset = release
            .assets
            .iter()
            .find(|a| a.name.ends_with(".zip") && a.name.contains("VantagePoint"))
            .ok_or_else(|| {
                CapabilityError::Other("No VantagePoint.app.zip asset found in release".to_string())
            })?;

        tracing::info!(
            current = current_version,
            new = release.version,
            app_path = %app_path.display(),
            asset = zip_asset.name,
            "Applying Mac app update"
        );

        // 一時ディレクトリを準備
        let temp_dir =
            std::env::temp_dir().join(format!("vantage-point-mac-update-{}", std::process::id()));
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to create temp dir: {}", e)))?;

        let zip_path = temp_dir.join(&zip_asset.name);
        let extract_dir = temp_dir.join("extracted");

        // ダウンロード
        self.download_binary(zip_asset, &zip_path).await?;

        // 展開
        self.extract_zip(&zip_path, &extract_dir).await?;

        // 展開されたアプリを検索
        let new_app_path = find_app_in_dir(&extract_dir).await.ok_or_else(|| {
            CapabilityError::Other("No .app bundle found in extracted archive".to_string())
        })?;

        // バックアップ
        let backup_path = app_path.with_extension("app.backup");
        if backup_path.exists() {
            tokio::fs::remove_dir_all(&backup_path).await.map_err(|e| {
                CapabilityError::Other(format!("Failed to remove old backup: {}", e))
            })?;
        }

        tracing::info!(
            src = %app_path.display(),
            dst = %backup_path.display(),
            "Backing up current app"
        );

        // 現在のアプリをバックアップ
        Self::copy_dir_recursive(&app_path, &backup_path)
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to backup app: {}", e)))?;

        // 現在のアプリを削除
        tokio::fs::remove_dir_all(&app_path)
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to remove current app: {}", e)))?;

        // 新しいアプリをコピー
        tracing::info!(
            src = %new_app_path.display(),
            dst = %app_path.display(),
            "Installing new app"
        );

        Self::copy_dir_recursive(&new_app_path, &app_path)
            .await
            .map_err(|e| {
                // ロールバック
                tracing::error!(error = %e, "Failed to install new app, rolling back");
                let _ = std::fs::remove_dir_all(&app_path);
                let _ = Self::copy_dir_sync(&backup_path, &app_path);
                CapabilityError::Other(format!("Failed to install app (rolled back): {}", e))
            })?;

        // 一時ファイルを削除
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;

        tracing::info!(
            previous = current_version,
            new = release.version,
            "Mac app update completed"
        );

        Ok(MacAppUpdateApplyResult {
            success: true,
            previous_version: current_version.to_string(),
            new_version: release.version.clone(),
            app_path: app_path.display().to_string(),
            backup_path: Some(backup_path.display().to_string()),
            message: format!(
                "Updated VantagePoint.app from {} to {}. Restart required.",
                current_version, release.version
            ),
            restart_required: true,
        })
    }

    /// ZIPファイルを展開
    async fn extract_zip(&self, zip_path: &PathBuf, dest: &PathBuf) -> CapabilityResult<()> {
        tokio::fs::create_dir_all(dest)
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to create extract dir: {}", e)))?;

        // unzipコマンドを使用（macOSに標準搭載）
        let output = tokio::process::Command::new("unzip")
            .arg("-q")
            .arg("-o")
            .arg(zip_path)
            .arg("-d")
            .arg(dest)
            .output()
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to run unzip: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CapabilityError::Other(format!("unzip failed: {}", stderr)));
        }

        tracing::debug!(
            zip = %zip_path.display(),
            dest = %dest.display(),
            "Extracted zip"
        );

        Ok(())
    }

    /// ディレクトリを再帰的にコピー（非同期）
    async fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf) -> std::io::Result<()> {
        tokio::fs::create_dir_all(dst).await?;

        let mut entries = tokio::fs::read_dir(src).await?;
        while let Some(entry) = entries.next_entry().await? {
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());

            if entry.file_type().await?.is_dir() {
                Box::pin(Self::copy_dir_recursive(&src_path, &dst_path)).await?;
            } else {
                tokio::fs::copy(&src_path, &dst_path).await?;
            }
        }

        Ok(())
    }

    /// ディレクトリを再帰的にコピー（同期）- ロールバック用
    fn copy_dir_sync(src: &PathBuf, dst: &PathBuf) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;

        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());

            if entry.file_type()?.is_dir() {
                Self::copy_dir_sync(&src_path, &dst_path)?;
            } else {
                std::fs::copy(&src_path, &dst_path)?;
            }
        }

        Ok(())
    }

    /// Macアプリのロールバック
    pub async fn rollback_mac_app(
        &self,
        backup_path: &str,
        app_path: &str,
    ) -> CapabilityResult<()> {
        let backup = PathBuf::from(backup_path);
        let app = PathBuf::from(app_path);

        if !backup.exists() {
            return Err(CapabilityError::Other("Backup not found".to_string()));
        }

        // 現在のアプリを削除
        if app.exists() {
            tokio::fs::remove_dir_all(&app).await.map_err(|e| {
                CapabilityError::Other(format!("Failed to remove current app: {}", e))
            })?;
        }

        // バックアップから復元
        Self::copy_dir_recursive(&backup, &app)
            .await
            .map_err(|e| CapabilityError::Other(format!("Failed to restore backup: {}", e)))?;

        tracing::info!(
            backup = backup_path,
            app = app_path,
            "Mac app rollback completed"
        );

        Ok(())
    }
}

/// VantagePoint.appのパスを検索
fn find_mac_app_path() -> Option<PathBuf> {
    let search_paths = [
        // 標準的なアプリケーションフォルダ
        "/Applications/VantagePoint.app",
        // ユーザーのアプリケーションフォルダ
        &format!(
            "{}/Applications/VantagePoint.app",
            std::env::var("HOME").unwrap_or_default()
        ),
        // ビルド出力（開発用）
        &format!(
            "{}/.build/release/VantagePoint.app",
            std::env::var("HOME").unwrap_or_default()
        ),
    ];

    for path in search_paths {
        let p = PathBuf::from(path);
        if p.exists() && p.is_dir() {
            return Some(p);
        }
    }

    None
}

/// ディレクトリ内の.appバンドルを検索
async fn find_app_in_dir(dir: &PathBuf) -> Option<PathBuf> {
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().map(|e| e == "app").unwrap_or(false) && path.is_dir() {
            return Some(path);
        }
    }

    None
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
        tracing::info!(version = CURRENT_VERSION, "UpdateCapability initialized");

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
            Some((parts[0].parse().ok()?, parts[1].parse().ok()?, 0))
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

/// 現在実行中のバイナリのパスを取得
fn find_current_binary() -> CapabilityResult<PathBuf> {
    // 1. 現在の実行ファイルパスを取得
    if let Ok(exe_path) = std::env::current_exe() {
        // シンボリックリンクを解決
        if let Ok(canonical) = exe_path.canonicalize() {
            return Ok(canonical);
        }
        return Ok(exe_path);
    }

    // 2. which vp で探す
    if let Ok(output) = std::process::Command::new("which").arg("vp").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }

    // 3. よく使われるパスを探す
    let candidates = [
        dirs::home_dir().map(|h| h.join(".cargo/bin/vp")),
        Some(PathBuf::from("/usr/local/bin/vp")),
        Some(PathBuf::from("/opt/homebrew/bin/vp")),
    ];

    for candidate in candidates.into_iter().flatten() {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(CapabilityError::Other(
        "Could not find vp binary path".to_string(),
    ))
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
