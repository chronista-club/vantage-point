//! Msgbox Registry — TheWorld registry への actor 登録 (wing actor discovery)
//!
//! wiremsg R5-3 で旧 msgbox の **forward 系** (`RemoteRoutingClient` / `http_forward` /
//! `/api/msgbox/remote_deliver`) は撤去済。 残るのは TheWorld registry への
//! register / unregister と認証 token の helper のみ。
//!
//! これらは VP Process 起動 / 停止時に actor → port マッピングを TheWorld の
//! `MsgboxRegistry` に同期するための薄い HTTP client で、 wing actor discovery
//! (= `msg_directory` 等) の基盤として存続する (R5-4 territory)。
//!
//! ## Auth
//!
//! `registry_token()` が `VP_REGISTRY_TOKEN` 環境変数を読む。 register HTTP には
//! まだ Bearer を付与していないが、 token helper は将来の register auth 用に保持。

use std::time::Duration;

/// 認証トークン形式
///
/// TheWorld registry が発行 / 受信側が検証する Bearer token。
/// 環境変数 `VP_REGISTRY_TOKEN` から取得。
/// 未設定の場合は空 token = auth 無効（development デフォルト）。
pub fn registry_token() -> Option<String> {
    std::env::var("VP_REGISTRY_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
}

// =============================================================================
// TheWorld registry への register/unregister（Process startup/shutdown）
// =============================================================================

/// 単一 actor を TheWorld registry に register
pub async fn register_actor_to_world(
    world_port: u16,
    project_name: &str,
    self_port: u16,
    actor: &str,
) -> anyhow::Result<()> {
    let url = format!("http://[::1]:{}/api/world/msgbox/register", world_port);
    let body = serde_json::json!({
        "actor": actor,
        "project_name": project_name,
        "port": self_port,
    });

    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?
        .post(&url)
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("register failed: HTTP {} - {}", status, body);
    }
    Ok(())
}

/// 一括 register（Process 起動時）
///
/// 各 actor の register に失敗しても他は試す。失敗 actor 名のリストを返す。
pub async fn register_actors_to_world(
    world_port: u16,
    project_name: &str,
    self_port: u16,
    actors: &[String],
) -> Vec<String> {
    let mut failed = Vec::new();
    for actor in actors {
        if let Err(e) = register_actor_to_world(world_port, project_name, self_port, actor).await {
            tracing::warn!("Registry: register '{}' to TheWorld failed: {}", actor, e);
            failed.push(actor.clone());
        }
    }
    failed
}

/// Process（port）配下の全 actor を TheWorld registry から一括 unregister
///
/// Process 停止時に呼ぶ。失敗してもログ出すだけ（shutdown を止めない）。
pub async fn unregister_process_from_world(world_port: u16, self_port: u16) -> anyhow::Result<()> {
    let url = format!(
        "http://[::1]:{}/api/world/msgbox/unregister-process",
        world_port
    );
    let body = serde_json::json!({ "port": self_port });

    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?
        .post(&url)
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("unregister failed: HTTP {} - {}", status, body);
    }
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_token_from_env() {
        // env var 未設定時は None
        unsafe {
            std::env::remove_var("VP_REGISTRY_TOKEN");
        }
        assert!(registry_token().is_none());

        // 空文字列も None 扱い
        unsafe {
            std::env::set_var("VP_REGISTRY_TOKEN", "");
        }
        assert!(registry_token().is_none());

        // セット時は Some
        unsafe {
            std::env::set_var("VP_REGISTRY_TOKEN", "test-token-123");
        }
        assert_eq!(registry_token(), Some("test-token-123".to_string()));

        // クリーンアップ
        unsafe {
            std::env::remove_var("VP_REGISTRY_TOKEN");
        }
    }
}
