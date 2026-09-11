//! Rust → sidebar bundle の投影（`window.vpSidebarDispatch` への typed push、`vp-sidebar.kdl` の event envelope）
//!
//! 旧 `app.rs` の `mod sidebar_js` + `push_sidebar_state`（棚卸し 項目 6 / 6-1 #3、2026-09-08。本文は順序付き diff で
//! 一致、差分は dedent / 可視性 / `sidebar_js::state` → `self::state`）。`DaemonSettings` / `settings_snapshot` は
//! app 側の関数（`developer_mode_env` / `flows::repo_dialog::resolve_default_repo_root`）を呼ぶ = state 側の
//! 計算なので `app/` に残す（doc 60 §2 の依存 rule: webview は app / flows を呼ばない）。
//! 投影だけを持ち、state 遷移は `app/` の責務（doc 60 §2）。
//!
//! sidebar bundle への押し込み（server → client）。
//!
//! ## なぜ [`crate::webview::push_main`] と別モジュールなのか
//!
//! webview は 1 document だが **bundle は 2 本**（`editor-host.bundle.js` /
//! `sidebar.bundle.js`）で、module state を共有できない。`dispatch.ts` の保留箱は main bundle
//! の中にあるので、sidebar 側の受け手をそこへ登録する術がない。**bundle が受け口の単位**
//! なので、sidebar は自分の受け口（`window.vpSidebarDispatch`）を持つ。
//!
//! SSOT は `schema/vp-sidebar.kdl`（request と同じ channel の event 側 = `IpcEventEnvelope`）。

use wry::WebView;

use crate::generated::sidebar_ipc::IpcEventEnvelope;
use crate::pane::SidebarState;

/// 生成 envelope を sidebar bundle の単一受け口 `window.vpSidebarDispatch` へ押し込む。
///
/// ⚠️ guard を残す理由は [`crate::webview::push_main`] と同じ — bundle 評価**前**に撃つ窓があり、
/// そこは JS が存在しないので保留箱にも積めない。sidebar の state は変化のたびに
/// 撃ち直されるので、その窓の取りこぼしは次の push で埋まる。
fn push(sidebar: &WebView, msg: &IpcEventEnvelope) {
    let json = match serde_json::to_string(msg) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!("sidebar push envelope の serialize に失敗: {e}");
            return;
        }
    };
    let script = format!("window.vpSidebarDispatch && window.vpSidebarDispatch({json})");
    if let Err(e) = sidebar.evaluate_script(&script) {
        tracing::warn!("vpSidebarDispatch script failed: {e}");
    }
}

/// sidebar の全 state を push する唯一の経路。
///
/// `state` の形の持ち主は Rust の [`crate::pane::SidebarState`]（ts-rs が TS 型を出す）。
/// envelope は「どの窓口へ届けるか」だけを型にし、中身はその 1 つの定義に委ねる。
pub(crate) fn state(sidebar: &WebView, state: &crate::pane::SidebarState) {
    let value = match serde_json::to_value(state) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("SidebarState serialize 失敗: {e}");
            return;
        }
    };
    push(
        sidebar,
        &IpcEventEnvelope::SidebarState(crate::generated::sidebar_ipc::SidebarState {
            state: value,
        }),
    );
}

/// daemon 接続失敗等の error 表示。
pub(crate) fn error(sidebar: &WebView, message: &str) {
    push(
        sidebar,
        &IpcEventEnvelope::SidebarError(crate::generated::sidebar_ipc::SidebarError {
            message: message.to_string(),
        }),
    );
}

/// + Add Sub の作成結果。`error` None = 成功（form を閉じる）。
pub(crate) fn sub_create_result(
    sidebar: &WebView,
    repo_path: String,
    name: String,
    error: Option<String>,
) {
    push(
        sidebar,
        &IpcEventEnvelope::SubCreateResult(crate::generated::sidebar_ipc::SubCreateResult {
            repo_path,
            name,
            error,
        }),
    );
}

/// + Add Sub の dropdown を populate する Agent 一覧。
pub(crate) fn stands_result(
    sidebar: &WebView,
    repo_path: String,
    agents: &[crate::daemon_wire::AgentInfo],
    error: Option<String>,
) {
    let agents = agents
        .iter()
        .filter_map(|s| match serde_json::to_value(s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("AgentInfo の serialize に失敗（この 1 件を省く）: {e}");
                None
            }
        })
        .collect();
    push(
        sidebar,
        &IpcEventEnvelope::AgentsResult(crate::generated::sidebar_ipc::AgentsResult {
            repo_path,
            agents,
            error,
        }),
    );
}

/// Wire inbox の履歴。
pub(crate) fn wire_result(sidebar: &WebView, payload: serde_json::Value) {
    push(
        sidebar,
        &IpcEventEnvelope::WireResult(crate::generated::sidebar_ipc::WireResult { payload }),
    );
}

/// 設定の確定値（doc 59 P1）。fetch / save / picker のいずれも**これ 1 本で終わる** —
/// client は楽観更新をしないので、保存失敗時の巻き戻しを持たなくてよい。
pub(crate) fn settings_result(
    sidebar: &WebView,
    result: crate::generated::sidebar_ipc::SettingsResult,
) {
    push(sidebar, &IpcEventEnvelope::SettingsResult(result));
}

/// SidebarState を sidebar webview に push（呼び手が多いので薄い別名を残す）。
pub(crate) fn push_sidebar_state(sidebar: &WebView, state: &SidebarState) {
    self::state(sidebar, state);
}
