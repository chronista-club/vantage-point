//! process ops — file watch / process runner / ruby の Unison method handler（owner は `process_runner` と `file_watcher`、doc 61）。
//!
//! watch_file / unwatch_file / process_run / process_stop / process_inject / process_list / ruby_eval / ruby_run /
//! ruby_stop（ruby_list は process_list と同一なので受付側で `handle_process_list` を再利用）。payload から
//! field を抜いて owner を呼ぶ薄い adapter。受付は `unison_server::dispatch_repo_method`。

use serde::{Deserialize, Serialize};

use super::state::RepoState;

/// UnwatchFile リクエストのペイロード
#[derive(Debug, Serialize, Deserialize)]
struct UnwatchFileRequest {
    pane_id: String,
}

/// watch_file メソッドのハンドラー
pub(crate) async fn handle_watch_file(
    state: &RepoState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let config: crate::file_watcher::WatchConfig = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid watch_file payload: {}", e))?;

    let pane_id = config.pane_id.clone();

    state
        .file_watchers
        .lock()
        .await
        .start_watch(config, state.hub.clone())
        .map_err(|e| format!("watch_file 開始失敗: {}", e))?;

    Ok(serde_json::json!({"status": "ok", "pane_id": pane_id}))
}

/// unwatch_file メソッドのハンドラー
pub(crate) async fn handle_unwatch_file(
    state: &RepoState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let req: UnwatchFileRequest = serde_json::from_value(payload)
        .map_err(|e| format!("Invalid unwatch_file payload: {}", e))?;

    state.file_watchers.lock().await.stop_watch(&req.pane_id);

    Ok(serde_json::json!({"status": "ok", "pane_id": req.pane_id}))
}

// =============================================================================
// ProcessRunner ハンドラー
// =============================================================================

/// プロセス起動
pub(crate) async fn handle_process_run(
    state: &RepoState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: crate::repo::process_runner::RunParams =
        serde_json::from_value(payload).map_err(|e| format!("パラメータ不正: {}", e))?;
    let process_id = crate::repo::process_runner::process_run(
        &state.process_registry,
        &params,
        &state.repo_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({"status": "ok", "process_id": process_id}))
}

/// プロセス停止
pub(crate) async fn handle_process_stop(
    state: &RepoState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let process_id = payload["process_id"]
        .as_str()
        .ok_or_else(|| "process_id が必要です".to_string())?;
    crate::repo::process_runner::process_stop(&state.process_registry, process_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// コード注入
pub(crate) async fn handle_process_inject(
    state: &RepoState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let params: crate::repo::process_runner::InjectParams =
        serde_json::from_value(payload).map_err(|e| format!("パラメータ不正: {}", e))?;
    crate::repo::process_runner::process_inject(&state.process_registry, &params).await?;
    Ok(serde_json::json!({"status": "ok"}))
}

/// プロセス一覧
pub(crate) async fn handle_process_list(state: &RepoState) -> Result<serde_json::Value, String> {
    let processes = state.process_registry.lock().await.list();
    Ok(serde_json::json!({"status": "ok", "processes": processes}))
}

// L0 portless Group B-3: 旧 SP HTTP `/api/ruby/*` を repo-proxy ask に移管。 HTTP handler と同じ
// `process_runner::ruby_*` core を呼ぶ薄い adapter (payload からフィールド抽出)。 ruby_list は
// `process_registry.list()` = `handle_process_list` と同一なので dispatch 側で再利用する。

/// ruby_eval: 短命 Ruby 実行
pub(crate) async fn handle_ruby_eval(
    state: &RepoState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = payload.get("code").and_then(|v| v.as_str());
    let file = payload.get("file").and_then(|v| v.as_str());
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let r =
        crate::repo::process_runner::ruby_eval(code, file, pane_id, &state.repo_dir, &state.hub)
            .await?;
    Ok(serde_json::json!({
        "status": "ok",
        "stdout": r.stdout,
        "stderr": r.stderr,
        "exit_code": r.exit_code,
        "elapsed_ms": r.elapsed_ms,
    }))
}

/// ruby_run: 長命 Ruby daemon 起動
pub(crate) async fn handle_ruby_run(
    state: &RepoState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let code = payload.get("code").and_then(|v| v.as_str());
    let file = payload.get("file").and_then(|v| v.as_str());
    let name = payload.get("name").and_then(|v| v.as_str());
    let pane_id = payload
        .get("pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let process_id = crate::repo::process_runner::ruby_run(
        &state.process_registry,
        code,
        file,
        name,
        pane_id,
        &state.repo_dir,
        &state.hub,
    )
    .await?;
    Ok(serde_json::json!({"status": "ok", "process_id": process_id}))
}

/// ruby_stop: Ruby daemon 停止
pub(crate) async fn handle_ruby_stop(
    state: &RepoState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let process_id = payload["process_id"]
        .as_str()
        .ok_or_else(|| "process_id が必要です".to_string())?;
    crate::repo::process_runner::ruby_stop(&state.process_registry, process_id).await?;
    Ok(serde_json::json!({"status": "ok"}))
}
