//! event handler: **terminal**（PTY 出力の投影と keystroke / resize の上り、paste）。
//!
//! 旧 `run()` の match arm を fn ごとに移したもの（doc 60 §6 6-2 PR-3、2026-09-08）。本体は arm の
//! 中身を 12 空白 dedent しただけ（照合は `sed 's/^            //'`）。arm の前に付いていたコメントは
//! fn の doc に。`run()` 側は routing table（`AppEvent::X => on_terminal::x(&mut ui, &boot, …)`）。
//!
//! 触る state: `ui.sessions.terminal_sessions`。resource: `boot.webview`。

use super::boot::Boot;
use super::state::UiState;
use crate::lane::terminal::TermCmd;
use crate::webview::push_main;

/// Phase 4-paste-fix: clipboard.readText の webview permission 問題への fallback。
/// IPC `paste:request` を Rust が受けて arboard で読み取り、 ここで JS に inject。
pub(super) fn paste_text(_ui: &mut UiState, boot: &Boot, text: String) {
    if text.is_empty() {
        tracing::debug!("PasteText empty (clipboard 空 or 取得失敗)、 skip");
    } else {
        // escape は envelope の serde_json 化に含まれる（Phase review fix #3 の
        // 「手書き escape は null byte / surrogate を見落とす」は、payload ごと
        // JSON にすることで構造的に解消）。
        push_main::deliver_paste(&boot.webview, &text);
    }
}

/// terminal S4 (doc 27 §4.1): per-lane terminal session 由来の PTY 出力を当該 lane の
/// xterm に inject する。 data は base64 (JS 側で decode → term.write)。
pub(super) fn terminal_output(
    _ui: &mut UiState,
    boot: &Boot,
    lane: String,
    session: u32,
    data: String,
) {
    // doc 50 §4.6 A6: 同 lane の複数 xterm に振り分けるため session を第 2 引数で渡す
    // （push envelope `console:event` と同じ形）。
    let script = format!(
        "window.vpTerminal && window.vpTerminal.handleOutput({}, {}, {})",
        serde_json::to_string(&lane).unwrap_or_else(|_| "\"\"".into()),
        session,
        serde_json::to_string(&data).unwrap_or_else(|_| "\"\"".into()),
    );
    if let Err(e) = boot.webview.evaluate_script(&script) {
        tracing::warn!("vpTerminal.handleOutput 失敗 (lane={}): {}", lane, e);
    }
}

/// terminal S4: xterm onData → 当該 lane の terminal session に渡す (上り request)。
pub(super) fn terminal_write(
    ui: &mut UiState,
    _boot: &Boot,
    lane: String,
    session: u32,
    data: String,
) {
    vp_paths::term_trace("A:app-dispatch(b64)", &lane, data.as_bytes());
    if let Some(term) = ui.sessions.terminal_sessions.get(&lane) {
        let _ = term.cmd_tx.send(TermCmd::Write(session, data));
    }
}

/// terminal S4: xterm resize → 当該 lane の terminal session に渡す (上り request)。
pub(super) fn terminal_resize(
    ui: &mut UiState,
    _boot: &Boot,
    lane: String,
    session: u32,
    cols: u16,
    rows: u16,
) {
    if let Some(term) = ui.sessions.terminal_sessions.get(&lane) {
        let _ = term.cmd_tx.send(TermCmd::Resize(session, cols, rows));
    }
}
