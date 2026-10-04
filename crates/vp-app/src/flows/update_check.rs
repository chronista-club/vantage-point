//! 設定からの read-only 手動確認。適用・再起動は既存 update flow だけが行う。
//! 既存 daemon update API を利用し、GitHub 認証や配布元を GUI に複製しない。
use crate::generated::sidebar_ipc::UpdateCheckResult;

#[derive(serde::Deserialize)]
struct CheckedRelease {
    current_version: String,
    latest_version: String,
    update_available: bool,
    #[serde(default)]
    fresh_check: bool,
}

pub async fn check_now() -> UpdateCheckResult {
    let base = std::env::var("VP_DAEMON_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", crate::daemon::default_daemon_port()));
    match fetch(&base, env!("CARGO_PKG_VERSION")).await {
        Ok(result) => result,
        Err(error) => UpdateCheckResult {
            update_available: false,
            latest_version: None,
            error: Some(error),
        },
    }
}

async fn fetch(base: &str, current_version: &str) -> Result<UpdateCheckResult, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| "通信の準備に失敗しました。もう一度お試しください。".to_string())?;
    let mut url = reqwest::Url::parse(&format!("{}/api/update/check", base.trim_end_matches('/')))
        .map_err(|_| "daemon の接続先が正しくありません。".to_string())?;
    url.query_pairs_mut()
        .append_pair("force", "true")
        .append_pair("current_version", current_version);
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "daemon に接続できないか、確認がタイムアウトしました。".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "更新情報を取得できませんでした（HTTP {}）。時間をおいて再度お試しください。",
            response.status().as_u16()
        ));
    }
    let checked: CheckedRelease = response
        .json()
        .await
        .map_err(|_| "更新情報の応答を読み取れませんでした。".to_string())?;
    if !checked.fresh_check || checked.current_version != current_version {
        return Err(
            "daemon が手動確認に対応していません。アプリ更新後は daemon も再起動してください。"
                .to_string(),
        );
    }
    if checked.latest_version.is_empty() {
        return Err("更新情報にバージョンが含まれていません。".to_string());
    }
    Ok(UpdateCheckResult {
        update_available: checked.update_available,
        latest_version: Some(checked.latest_version),
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn serve(status: &str, body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", socket.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let task = tokio::spawn(async move {
            let (mut stream, _) = socket.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0; 1024];
                let count = stream.read(&mut buf).await.unwrap();
                bytes.extend_from_slice(&buf[..count]);
                if count == 0 || bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (url, task)
    }

    #[tokio::test]
    async fn requests_a_fresh_check_for_the_app_without_applying() {
        let (url, request) = serve(
            "200 OK",
            r#"{"current_version":"0.76.0","latest_version":"0.77.0","update_available":true,"fresh_check":true}"#,
        )
        .await;
        let result = fetch(&url, "0.76.0").await.unwrap();
        assert!(result.update_available);
        assert_eq!(result.latest_version.as_deref(), Some("0.77.0"));
        let request = request.await.unwrap();
        assert!(request.starts_with("GET /api/update/check?force=true&current_version=0.76.0 "));
    }

    #[tokio::test]
    async fn reports_http_and_invalid_response_errors_instead_of_latest() {
        for (status, body) in [
            ("503 Service Unavailable", "{}"),
            ("200 OK", "{}"),
            (
                "200 OK",
                r#"{"current_version":"0.70.0","latest_version":"0.77.0","update_available":true}"#,
            ),
        ] {
            let (url, request) = serve(status, body).await;
            assert!(fetch(&url, "0.76.0").await.is_err());
            request.await.unwrap();
        }
    }
}
