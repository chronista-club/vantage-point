//! 受動の取得 — 会話中に流れてくる rate limit の通知を [`UsageService`] に流し込む口。
//!
//! 会話 engine の host（`conversation/host.rs` の Claude stdout ポンプ、`conversation/codex_host.rs`
//! の notification 分岐）は repo runtime の奥にあり、daemon の state を引き回していない。
//! そこで daemon 起動時に 1 度だけ [`install`] し、host は関数を呼ぶだけにする
//! （`vpcode_catalog` の process-global cache と同じ割り切り）。
//!
//! - install 前（test・usage を使わない構成）は**何もしない**
//! - 毎行呼ばれるので、関係ない行は文字列の包含だけで素通しする（JSON を読まない）
//! - `Weak` で持つ: daemon が service を捨てたら、ここから延命しない

use std::sync::{Arc, OnceLock, Weak};

use serde_json::Value;

use super::model::UsageEngine;
use super::parse;
use super::service::UsageService;

static SINK: OnceLock<Weak<UsageService>> = OnceLock::new();

/// daemon 起動時に 1 度だけ呼ぶ（2 度目以降は無視）。
pub fn install(service: &Arc<UsageService>) {
    let _ = SINK.set(Arc::downgrade(service));
}

fn sink() -> Option<Arc<UsageService>> {
    SINK.get().and_then(Weak::upgrade)
}

/// Claude の stream-json 1 行。`rate_limit_event` で使用率を持つものだけ反映する。
pub fn observe_claude_line(line: &str) {
    if !line.contains("rate_limit_event") {
        return;
    }
    let Some(service) = sink() else {
        return;
    };
    let Ok(event) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if let Some(window) = parse::claude_rate_limit_window(&event) {
        service.apply_partial(UsageEngine::Claude, vec![window], None);
    }
}

/// Codex app-server の notification 1 つ。`account/rateLimits/updated` だけ反映する。
pub fn observe_codex_notification(method: &str, params: &Value) {
    if method != "account/rateLimits/updated" {
        return;
    }
    let Some(service) = sink() else {
        return;
    };
    if let Some((windows, plan)) = parse::codex_rate_limits_update(params) {
        service.apply_partial(UsageEngine::Codex, windows, plan);
    }
}
