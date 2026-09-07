//! 統合 ipc_handler の **dispatch 判定** — main（terminal / pane）IPC か sidebar IPC か。
//!
//! main 側の tag 集合（allowlist）は `terminal_ipc::handle_ipc_message` の match arm と対。
//! 片側だけ更新すると sidebar IPC へ流れて「unknown variant」で silent drop になる
//! （2026-07-16 の「+」無反応 regression）。新 tag は **両方**に足す。
//! app/mod.rs から移設（棚卸し 項目 6 / 6-1 #9、2026-09-08）。

/// WebView 統合 (step 3a): 統合 ipc_handler の dispatch 判定。
/// main (terminal / pane) IPC tag なら true、 sidebar IpcEnvelope tag (repo: / lane: 系)
/// なら false。 tag 集合は `terminal_ipc::handle_ipc_message` の match arm と一致 (disjoint)。
/// terminal の fall-through に頼ると sidebar tag を silent drop するため、 ここで明示判定する。
pub(crate) fn is_main_ipc_tag(body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    matches!(
        v.get("t").and_then(|t| t.as_str()),
        Some(
            // webview が受け口を全部生やした合図。Rust はこれを受けて現在の状態を丸ごと
            // 撃ち直す（旧 `lanes:ensure-all` / `bastet:devices_fetch` / `board:demand` の
            // 3 本はここに畳んだ）。allowlist 漏れは「起動直後だけ何も出ない」になる。
            "ready"
                | "term:write"
                | "term:resize"
                | "copy"
                | "paste:request"
                | "debug"
                | "osc:notification"
                | "slot:rect"
                | "board:delete"
                | "board:clear"
                // cursor server 昇格（doc 52 §5）: thumbnail click / scrollback の注視 → repo。
                // allowlist 漏れは sidebar IPC へ silent drop = click しても注視が同期されない
                | "board:cursor"
                | "console"
                | "open-url"
                | "conversation:submit"
                | "conversation:respond"
                | "conversation:interrupt"
                | "conversation:set_permission_mode"
                // doc 38 Phase 2: session tab strip の tag。webview/terminal_ipc.rs に match arm を
                // 足すだけでは届かない — この allowlist に無い tag は sidebar IPC に流れて
                // 「unknown variant」で捨てられる（2026-07-16 dogfood で「+」無反応の根因）。
                // 旧 `echoes:sessions_fetch` は doc 53 §11 で退役（roster は snapshot が運ぶ）。
                | "conversation:session_create"
                | "conversation:session_focus"
                // doc 38 Phase 3: session tab の × による close（allowlist 漏れは sidebar IPC へ
                // 流れて silent drop = 「×無反応」regression。tests でも固定）。
                | "conversation:session_remove"
                | "conversation:agents_fetch"
                // replay demand（2026-07-24）: 消費者主導 demand。allowlist 漏れは sidebar IPC へ
                // 流れて silent drop = 「chat が空のまま」regression（webview/terminal_ipc.rs の arm と対）
                | "conversation:demand_start"
                // doc 50 §4.6 A6: 名札 kind badge の Mode 切替（session 明示）。漏れると
                // sidebar IPC へ流れて silent drop = 「badge を押しても変身しない」regression。
                // 旧 lane 単位 `console:set_mode` は同 A6 で退役（見え方は session の属性）。
                | "session:set_mode"
                | "console:new_session"
                // doc 39 P3: Root 切替 picker（allowlist 漏れは sidebar IPC へ流れて
                // silent drop = 「picker 無反応」になる — session tab 4 tag と同じ罠）
                | "console:switch_root"
                | "conversation:set_model"
                // ink（対話面, doc 52 §3）: 送信の snapshot 要求。漏れると sidebar IPC へ流れて
                // 「unknown variant ink:snapshot」で silent drop = 送信しても画像が飛ばない
                | "ink:snapshot"
                // R sidebar の debug log（sidebar view modes, 2026-08-01）: tail の購読開始/停止。
                // 漏れると sidebar IPC へ流れて silent drop = 「開いても永久に空」regression
                | "debuglog:watch"
                | "debuglog:unwatch"
                // shell layout（L sidebar | main | R sidebar の形）: drag / form 切替 / R 開閉の
                // 確定時に webview が送る。漏れると sidebar IPC へ流れて silent drop =
                // 「ドラッグしても次回起動で戻る」regression（他の tag と同じ罠）
                | "shell:layout"
                // code pane（コードブラウザ P1、CodePane.tsx 発）。漏れると sidebar IPC へ
                // 流れて silent drop = 「tree が永久に空 / file 無反応」regression
                | "code:list"
                | "code:read"
        )
    )
}

#[cfg(test)]
mod ipc_tag_tests {
    use super::is_main_ipc_tag;

    /// doc 38 Phase 2/3 の session tab tag が main webview IPC として dispatch されること。
    /// webview/terminal_ipc.rs の match arm と本 allowlist は**両方**更新が要る（片側更新だと
    /// sidebar IPC に落ちて silent drop — 2026-07-16 の「+」無反応 regression の固定。
    /// Phase 3 の `conversation:session_remove` も同じ理由で allowlist に載せた）。
    #[test]
    fn session_tab_tags_route_to_main_ipc() {
        for t in [
            "conversation:session_create",
            "conversation:session_focus",
            "conversation:session_remove",
            "conversation:agents_fetch",
            // 消費者主導 replay demand（2026-07-24 — 漏れは「chat が空のまま」）
            "conversation:demand_start",
            // doc 39 P3: Root 切替 picker（ヘッダ chip dropdown）
            "console:switch_root",
            // webview の誕生合図。Rust の replay 一式がここに畳んである（旧 catch-up pull
            // 3 本の後継）。漏れると「起動直後だけ console も pane も空」になる
            "ready",
            // ink（対話面, doc 52 §3）: 送信の snapshot 要求（漏れは「送っても画像が飛ばない」）
            "ink:snapshot",
            // cursor server 昇格（doc 52 §5 計器盤）: 漏れは「click しても注視が同期されない」
            "board:cursor",
            // doc 50 §4.6 A6: 名札 kind badge の Mode 切替（漏れは「押しても変身しない」）
            "session:set_mode",
            // R sidebar の debug log（漏れは「開いても永久に空」）
            "debuglog:watch",
            "debuglog:unwatch",
            // shell layout（漏れは「ドラッグしても次回起動で戻る」）
            "shell:layout",
            // code pane（コードブラウザ）: 漏れは「pane を開いても tree が永久に空」/
            // 「file を押しても何も出ない」
            "code:list",
            "code:read",
        ] {
            let msg = format!(r#"{{"t":"{t}","lane":"vp/root"}}"#);
            assert!(
                is_main_ipc_tag(&msg),
                "{t} は main IPC に振り分けられるべき（sidebar に流すと unknown variant で drop）"
            );
        }
        // sidebar 系 tag は従来どおり main に取られない（disjoint の維持）。
        assert!(!is_main_ipc_tag(r#"{"t":"lane:select","lane":"x"}"#));
        assert!(!is_main_ipc_tag(r#"{"t":"agents:fetch","path":"x"}"#));
    }
}
