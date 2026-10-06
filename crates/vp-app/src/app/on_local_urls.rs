//! 登録 URL の native 操作。repo/path と Lane registry を照合してから保存へ渡す。
use super::{boot::Boot, state::UiState};
use crate::events::AppEvent;
use crate::generated::sidebar_ipc::{LocalUrlsRequest, LocalUrlsResult};
use crate::local_urls::{Action, Prepared};

pub(super) fn request(ui: &UiState, boot: &Boot, request: LocalUrlsRequest) {
    let lane = ui
        .sidebar_state
        .lanes_by_repo
        .get(&request.path)
        .and_then(|lanes| {
            lanes
                .iter()
                .find(|lane| lane.address.key() == request.address)
        })
        .map(|lane| lane.address.name.clone());
    let action = serde_json::from_value::<Action>(request.payload);
    let (Some(lane), Ok(action)) = (lane, action) else {
        crate::webview::push_sidebar::local_urls_result(
            &boot.webview,
            LocalUrlsResult {
                req: request.req,
                payload: serde_json::Value::Null,
                error: Some("Lane または URL 操作を確認できません".into()),
            },
        );
        return;
    };
    let proxy = boot.proxy.clone();
    let runtime = boot.rt_handle.clone();
    boot.rt_handle.spawn(async move {
        let result = runtime
            .spawn_blocking(move || {
                crate::local_urls::prepare(
                    &vp_paths::vp_state_dir().join("lane-local-urls.json"),
                    &request.path,
                    &lane,
                    action,
                )
            })
            .await;
        let result = match result {
            Ok(Ok(Prepared::Entries(entries))) => Ok(serde_json::json!({"entries": entries})),
            Ok(Ok(Prepared::ProbeUrl(url))) => {
                Ok(serde_json::json!({"probe": crate::local_urls::probe(&url).await}))
            }
            Ok(Ok(Prepared::OpenUrl(url))) => {
                match runtime.spawn_blocking(move || webbrowser::open(&url)).await {
                    Ok(Ok(())) => Ok(serde_json::json!({})),
                    Ok(Err(e)) => Err(format!("ブラウザで開けませんでした: {e}")),
                    Err(e) => Err(e.to_string()),
                }
            }
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e.to_string()),
        };
        let (payload, error) = match result {
            Ok(v) => (v, None),
            Err(e) => (serde_json::Value::Null, Some(e)),
        };
        let _ = proxy.send_event(AppEvent::LocalUrlsResult(LocalUrlsResult {
            req: request.req,
            payload,
            error,
        }));
    });
}
