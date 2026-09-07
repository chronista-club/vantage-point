//! Main EventLoop + window lifecycle
//!
//! ## アーキテクチャ方針 (Mac 版と同等 + Creo UI 統一)
//!
//! 「ネイティブ層ベース + WebUI on top」のハイブリッド構成。
//! デザインシステムは **Creo UI** (mint-dark theme) を全ペインで共有。
//!
//! ```text
//! ┌─── tao ネイティブウィンドウ (native chrome, menu, tray) ──┐
//! │ ┌──────────┬───────────────────────────────────────┐ │
//! │ │ sidebar  │   main area (単一 wry WebView)          │ │
//! │ │ (Creo)   │   ┌─ pane-lane (xterm.js)─────┐   │ │
//! │ │ repo  │   ├─ pane-canvas (placeholder)─────┤   │ │
//! │ │ + Activ. │   ├─ pane-preview (iframe)─────────┤   │ │
//! │ │ widget   │   └─ pane-empty   (no selection)───┘   │ │
//! │ │ (~280px) │   active pane を kind 別に切替表示       │ │
//! │ └──────────┴───────────────────────────────────────┘ │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! - **ウィンドウ・メニュー・トレイ・レイアウト境界** は Rust (tao + muda + tray-icon)
//! - **sidebar** は wry WebView (accordion + Activity widget、VP-95)
//! - **main area** は単一 wry WebView (β 戦略、VP-100 Phase 2)。
//!   PaneKind 別の content を全部 mount しておき、`window.setActivePane` で表示切替
//! - **Creo UI tokens.css (mint-dark)** を各 WebView に inline して token 統一
//! - **γ-light readiness**: main area の slot rect を ResizeObserver 経由で Rust に
//!   push (`AppEvent::SlotRect`)、Phase 4+ で native overlay の `set_position` 同期に使用

/// sidebar IPC の解釈（state 遷移）。効果の実行は本 module の `run()`。
mod sidebar_ipc;

use std::time::Duration;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::{
    Rect, WebView, WebViewBuilder, dpi::LogicalPosition, dpi::LogicalSize as WryLogicalSize,
};

use crate::daemon::conn::{
    SharedDaemonConn, SubscriptionOutcome, daemon_repo_request, spawn_daemon_conn_manager,
};
use crate::daemon::pollers::{
    ActionsPersistPayload, fetch_repos_with_ports, resolve_active_repo_path,
    spawn_actions_persist_writer, spawn_activity_poller, spawn_lane_inbox_poller,
    spawn_menu_event_pump, spawn_processes_fetch, spawn_session_title_poller, spawn_sp_start,
};
use crate::daemon::subscriptions::{
    spawn_canvas_subscription, spawn_device_subscription, spawn_lanes_subscription,
};
use crate::daemon::wire::wire_fetch_payload;
use crate::events::AppEvent;
use crate::flows::repo_dialog::{
    resolve_default_repo_root, spawn_add_repo_picker, spawn_clone_repo, spawn_repo_root_picker,
};
use crate::pane::{RepoPaneState, SidebarState};
use crate::session_state::SessionState;
use crate::settings::Settings;
use crate::webview::editor_bridge::{fleet_dispatch_js, fleet_feedback_payload};
use crate::webview::main_area::{self, ActivePaneInfo, MAIN_AREA_HTML, SlotRect};
use crate::webview::push_main;
use crate::webview::push_sidebar::{self, push_sidebar_state};
use crate::webview::terminal_ipc;
use sidebar_ipc::handle_sidebar_ipc;

/// 起動時の window default size (LogicalPixel)。 with_inner_size と clamp 矯正後の値で
/// 共用するため定数化。
const DEFAULT_WINDOW_WIDTH: f64 = 1200.0;
const DEFAULT_WINDOW_HEIGHT: f64 = 800.0;

/// 最低 window size (LogicalPixel)。 sidebar 幅 280 (HTML 側 CSS `#sidebar-root` が司る) + 余裕ある main 領域 (820+) を
/// 構造的に確保。 これ未満になる window は使用に耐えないため、 OS の min 制約 (drag 防止)
/// と起動時 clamp (state restoration 後の矯正) の両方で下限として参照する。
const MIN_WINDOW_WIDTH: f64 = 1100.0;
const MIN_WINDOW_HEIGHT: f64 = 700.0;

/// 開発者モード判定 (起動時の初期値計算に使用、runtime 切替は menu 経由)
///
/// 優先順位 (1Password 風の挙動):
/// 1. `VP_DEVELOPER_MODE` env var が `1`/`true`/`yes`/`on` → 強制 ON
/// 2. `VP_DEVELOPER_MODE` env var が `0`/`false`/`no`/`off` → 強制 OFF
/// 3. Settings ファイル (`vp_config_dir()/vp-app.toml`) の `developer_mode` フィールド
/// 4. それ以外 (未設定) → `cfg!(debug_assertions)` (debug ビルドは ON、release は OFF)
///
/// 起動後の runtime 切替 (View → Developer Mode メニュー) は app.rs の event loop で
/// settings ファイルを更新しつつ、対応する menu item の状態を即時反映する。
fn initial_developer_mode(settings: &Settings) -> bool {
    if let Some(b) = developer_mode_env() {
        return b;
    }
    if let Some(b) = settings.developer_mode {
        return b;
    }
    cfg!(debug_assertions)
}

/// `VP_DEVELOPER_MODE` の解釈。`Some` = **env が実効値を固定している**（= 設定ページで
/// 変えても効かない）。
///
/// [`initial_developer_mode`] から切り出したのは、設定ページが「この toggle は今 env に
/// 固定されているか」を知る必要があるため（doc 59 P1）。受理する綴りの一覧を 2 箇所に
/// 書くと、片方だけ増やしたときに「env は効いているのに UI は編集可能に見える」がすぐ生える。
fn developer_mode_env() -> Option<bool> {
    let v = std::env::var("VP_DEVELOPER_MODE").ok()?;
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        // 綴り違いは「指定なし」に倒す（file / debug_assertions のフォールバックへ）。
        _ => None,
    }
}

/// Creo UI design tokens (CSS custom properties、mint-dark default)
///
/// <https://github.com/chronista-club/creo-ui> packages/web が source。
/// vp-app の 3 ペインすべてに inline して共通 token で描画する。
pub const CREO_TOKENS_CSS: &str = include_str!("../../assets/creo-tokens.css");

/// WebView 統合 (step 3a) 後の唯一の webview が `vp-asset://` で配信する asset。
/// `MAIN_AREA_HTML` を `app/index.html` で、SolidJS bundle 2 本を外部 script として配信
/// (doc 48 Phase 1 で inline → `<script src>` 化。`VP_WEBVIEW_DEV` 設定時は
/// `webview::assets::serve` の disk-read が baked より優先され、cargo build なしの HMR になる)。
///
/// ## なぜ with_html ではなく custom protocol か (統合 origin fix)
/// `with_html` で load した document は **about:blank = 不透明 (opaque) オリジン**になり、
/// `localStorage` 等 origin 依存 API が `SecurityError` を throw する。統合で sidebar bundle を
/// 同 document に inline した結果、`Shell()` が render 時に踏む `localStorage.getItem`
/// (タブ状態の永続) で sidebar bundle が boot 中に落ち、`<Shell/>` が mount されず sidebar が
/// 空になっていた。custom protocol で load すれば document origin = `vp-asset://app` の
/// 実オリジンになり、統合前 (sidebar が `vp-asset://app/sidebar.html` を load していた頃) と
/// 同じく localStorage が使える。
const MAIN_VIEW_ASSETS: &[(&str, &[u8], &str)] = &[
    (
        "app/index.html",
        MAIN_AREA_HTML.as_bytes(),
        "text/html; charset=utf-8",
    ),
    (
        "app/editor-host.bundle.js",
        main_area::EDITOR_HOST_BUNDLE_JS.as_bytes(),
        "application/javascript; charset=utf-8",
    ),
    (
        "app/sidebar.bundle.js",
        main_area::SIDEBAR_BUNDLE_JS.as_bytes(),
        "application/javascript; charset=utf-8",
    ),
];

/// Sidebar + Main area の bounds をウィンドウサイズから計算 (VP-100 Phase 2)
///
/// WebView 統合 (step 3a): sidebar + main を統合した 1 WebView を window 全面に張る。
/// sidebar(280px) | main の横分割は HTML 側 CSS flex (#app-shell) が司る。
fn update_pane_bounds(webview: &WebView, window_size: tao::dpi::PhysicalSize<u32>, scale: f64) {
    let logical = window_size.to_logical::<f64>(scale);
    let _ = webview.set_bounds(Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: WryLogicalSize::new(logical.width, logical.height).into(),
    });
}

/// WebView 統合 (step 3a): 統合 ipc_handler の dispatch 判定。
/// main (terminal / pane) IPC tag なら true、 sidebar IpcEnvelope tag (repo: / lane: 系)
/// なら false。 tag 集合は `terminal_ipc::handle_ipc_message` の match arm と一致 (disjoint)。
/// terminal の fall-through に頼ると sidebar tag を silent drop するため、 ここで明示判定する。
fn is_main_ipc_tag(body: &str) -> bool {
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

/// doc 50 §4.6 A6: 「どの session に xterm / 購読が要るか」の導出（実機で踏んだ穴の固定）。
///
/// 旧実装は lane 単位の `console_mode` / `pid` で gate していた。あれは「term になれるのは
/// root だけ」という制約下では正しかったが、A6 で非 root も term になれるので **root の mode で
/// lane 全体を切ると、非 root の住人が丸ごと落ちる**（2026-07-25 実機 dogfood で観測 —
/// pane は並ぶのに中身が来ない）。導出は registry の mode から行う、をここで固定する。
#[cfg(test)]
mod session_derivation_tests {
    use super::{
        forget_roster_push, remember_roster_push, roster_push_needed, session_list_payload,
        term_sessions_of,
    };
    use crate::daemon_wire::LaneInfo;

    /// doc 53 §11: snapshot の roster が **webview 契約の形**に写ること。
    ///
    /// 旧実装ではこの payload は `conversation_session_list` の ask 結果そのものだった。供給を
    /// snapshot に 1 本化した今、変換はこの純関数 1 箇所 — 形がずれると tab strip / pane grid /
    /// 名札が同時に壊れるので、**client が読む field を名指しで**固定する。
    ///
    /// （旧テスト `dropped_fetch_is_replayed_once_repo_resolves` が守っていた性質
    /// 「boot 窓で roster を取りこぼさない」は、供給が retained snapshot になったことで
    /// 構造的に消滅した — 取りこぼす対象の要求が存在しない。§8.6 の規律に従い、
    /// 代わりに新しい供給の契約をここで固定する。）
    #[test]
    fn session_list_payload_matches_webview_contract() {
        let lane = lane_with(
            16,
            serde_json::json!([
                {"key": 16, "agent": "claude", "mode": "gui",
                 "conversation": "conv-abc", "chat_capable": true, "image_capable": true},
                {"key": 24, "agent": "shell", "mode": "tui", "chat_capable": false},
            ]),
        );
        let sessions = lane.sessions.as_ref().expect("roster");
        let payload = session_list_payload("vp/root", sessions);

        assert_eq!(payload["lane"], "vp/root");
        assert_eq!(payload["focused"], 16, "focused は top-level にも出す");
        let entries = payload["sessions"].as_array().expect("sessions array");
        assert_eq!(entries.len(), 2);

        // root / focused は **entry ごとの bool** に展開する（webview はこの形で読む）。
        assert_eq!(entries[0]["key"], 16);
        assert_eq!(entries[0]["root"], true);
        assert_eq!(entries[0]["focused"], true);
        assert_eq!(entries[0]["mode"], "gui");
        assert_eq!(
            entries[0]["engine_session_id"], "conv-abc",
            "会話 id は engine_session_id という名で運ぶ（webview 契約）"
        );
        assert_eq!(
            entries[0]["chat_capable"], true,
            "能力表は server が SSOT — client に engine 名の分岐を作らない"
        );
        // ⚠️ 回帰固定（2026-08-30）: entry は手書きの写像なので、wire に足しただけでは
        // ここに現れない。`image_capable` を落として「server は true なのに client は false」
        // で貼り付け UI が出なかった実害があった。**能力 field は 1 つずつ assert する**。
        assert_eq!(
            entries[0]["image_capable"], true,
            "画像投入の能力表明も webview へ運ぶ（落とすと貼り付け UI が出ない）"
        );

        assert_eq!(entries[1]["key"], 24);
        assert_eq!(entries[1]["root"], false);
        assert_eq!(entries[1]["focused"], false);
        assert_eq!(entries[1]["mode"], "tui");
        assert!(
            entries[1]["engine_session_id"].is_null(),
            "会話 id 未発番（Draft / shell）は null"
        );
        assert_eq!(entries[1]["chat_capable"], false);
    }

    /// doc 53 §11: 定期 snapshot で roster を撃ち直さない指紋 gate の意味論。
    ///
    /// LanesLoaded は高頻度 event なので、値が同じなら push しない（毎回撃つと webview が
    /// roster を作り直して pane が無用に再配置される）。**replay 経路（`WebviewReady`）は
    /// この gate を通さない** — boot 窓で落ちた push を取り戻す唯一の機会で、そこで
    /// 「変化なし」と判断すると roster が永久に空のままになる（team-b 指摘 2026-07-25）。
    /// 呼び分けは実装側の構造（gate を呼ぶ / 呼ばない）で表す。
    #[test]
    fn roster_push_gate_fires_on_change_only() {
        let lane = lane_with(
            16,
            serde_json::json!([{"key": 16, "agent": "claude", "mode": "gui"}]),
        );
        let payload = session_list_payload("vp/root", lane.sessions.as_ref().expect("roster"));
        let mut last: std::collections::HashMap<String, String> = std::collections::HashMap::new();

        assert!(
            roster_push_needed(&last, "vp/root", &payload),
            "初回は push（指紋が無い）"
        );
        remember_roster_push(&mut last, "vp/root", &payload);
        assert!(
            !roster_push_needed(&last, "vp/root", &payload),
            "変化が無ければ push しない"
        );

        // session が 1 本増えた = roster の変化 → 撃つ。
        let grown = lane_with(
            16,
            serde_json::json!([
                {"key": 16, "agent": "claude", "mode": "gui"},
                {"key": 24, "agent": "shell", "mode": "tui"},
            ]),
        );
        let grown_payload =
            session_list_payload("vp/root", grown.sessions.as_ref().expect("roster"));
        assert!(
            roster_push_needed(&last, "vp/root", &grown_payload),
            "roster が変われば push する"
        );

        // lane が消えたら指紋も落とす（同名再作成で「変化なし」と誤判定しないため）。
        forget_roster_push(&mut last, "vp/root");
        assert!(
            roster_push_needed(&last, "vp/root", &payload),
            "lane 削除後は初回扱いに戻る"
        );
    }

    /// registry snapshot 付きの最小 LaneInfo（wire と同じ JSON 形で組む）。
    /// doc 53 R1: mode は sessions（registry snapshot）だけが運ぶ — 旧 console_mode field は退役。
    fn lane_with(root: u32, sessions: serde_json::Value) -> LaneInfo {
        serde_json::from_value(serde_json::json!({
            "address": {"kind": "root", "repo": "vp"},
            "sessions": {"root": root, "focused": root, "sessions": sessions},
        }))
        .expect("LaneInfo deserialize")
    }

    fn s(key: u32, mode: &str) -> serde_json::Value {
        serde_json::json!({"key": key, "agent": "claude", "mode": mode})
    }

    #[test]
    fn term_sessions_picks_every_tui_session_with_root_flag() {
        // root=16 が chat、非 root=19 が tui（2026-07-25 実機で踏んだ構成そのもの）。
        let lane = lane_with(16, serde_json::json!([s(16, "gui"), s(19, "tui")]));
        assert_eq!(
            term_sessions_of(&lane),
            vec![(19, false)],
            "root が chat でも非 root の term は拾う（lane ごと skip しない）"
        );

        // root も tui なら root フラグ付きで拾う。
        let lane = lane_with(16, serde_json::json!([s(16, "tui"), s(19, "tui")]));
        assert_eq!(term_sessions_of(&lane), vec![(16, true), (19, false)]);

        // 全部 chat なら term はゼロ = lane に xterm は要らない。
        let lane = lane_with(16, serde_json::json!([s(16, "gui")]));
        assert!(term_sessions_of(&lane).is_empty());
    }

    #[test]
    fn term_sessions_falls_back_to_root_for_legacy_wire() {
        // registry snapshot が無い旧 SP からの wire は root 1 枚に畳む（従来挙動）。
        let lane: LaneInfo = serde_json::from_value(serde_json::json!({
            "address": {"kind": "root", "repo": "vp"},
        }))
        .expect("LaneInfo deserialize");
        assert_eq!(term_sessions_of(&lane), vec![(1, true)]);
    }
}

/// terminal S4 (doc 27 §4.1): per-lane terminal session への command (WebView → repo)。
#[derive(Debug)]
enum TermCmd {
    /// keystroke (session, base64)。 canvas channel 上り request `terminal_write` で repo に送る。
    /// doc 50 §4.6 A6: `session` は宛先 slot（0 = 未指定 → repo が root に解決）。
    Write(u32, String),
    /// resize (session, cols, rows)。 `terminal_resize` で送る（0 = 未指定 → root）。
    Resize(u32, u16, u16),
}

/// terminal S4: 1 lane の terminal session handle (event loop が保持)。
///
/// map から remove すると `cmd_tx` が drop され、 session loop の `cmd_rx.recv()` が None を返して
/// 停止 → canvas channel drop → daemon 側 demand stop → repo pump stop
/// (= 購読者が消えたら pump を畳む、 S2 demand-driven production の出口)。
struct LaneTerminal {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<TermCmd>,
}

/// terminal S4: lane の terminal を Daemon "canvas" channel に乗せる per-lane session を spawn。
///
/// `lane_key` = `<repo>/root` 等 (`LaneAddressWire::key()`)。 Daemon :32000 の "canvas"
/// channel に `pattern: process/terminal/data/{lane_key}/out` で subscribe → Daemon demand 発火 →
/// repo pump start。 受信した PTY 出力は `AppEvent::TerminalOutput` で event loop に流し、 cmd_rx
/// 経由の write/resize は同 channel の上り request で repo に forward する (S3 bidirectional)。
fn spawn_terminal_session(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    conn: SharedDaemonConn,
    repo_path: String,
    lane_key: String,
) -> LaneTerminal {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    rt_handle.spawn(terminal_session_loop(
        proxy, conn, repo_path, lane_key, cmd_rx,
    ));
    LaneTerminal { cmd_tx }
}

/// "canvas" channel (terminal pattern) の購読 → 再購読を司る long-lived ループ
/// (F1b: 共有 connection 上の per-lane stream)。 `cmd_rx` は再接続を跨いで保持する
/// (= 切断中に積まれた write/resize は次接続で送れる)。 reconnect は共有 manager が所有するので
/// `wait_client` で接続を待ち、 give-up はしない (lane 消滅 = cmd_tx drop で AppClosing 終了)。
async fn terminal_session_loop(
    proxy: EventLoopProxy<AppEvent>,
    mut conn: SharedDaemonConn,
    repo_path: String,
    lane_key: String,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<TermCmd>,
) {
    loop {
        let client = match conn.wait_client().await {
            Some(c) => c,
            None => return, // app 終了
        };
        match run_terminal_session(&proxy, &repo_path, &lane_key, &client, &mut cmd_rx).await {
            // AppClosing = event loop 終了 or lane removed (cmd_tx drop) → session 終了。
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {}
            Err(e) => {
                tracing::warn!("terminal session error: lane={}: {}", lane_key, e);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// 1 回の terminal session: connect → `open_channel("canvas")` → subscribe(terminal pattern) →
/// recv (出力) / cmd (入力・resize) の select ループ。
///
/// 出力 `channel.recv()` と 上り `channel.request()` を同一 select! で扱う。 unison の `request` の
/// response は pending map で解決され `recv()` には来ず、 また `recv()` は内部 buffer 由来で
/// cancel-safe (= concurrent recv+request は control/repo-proxy で実証済) なので、 cmd 分岐で
/// recv future を drop しても出力欠落しない。
async fn run_terminal_session(
    proxy: &EventLoopProxy<AppEvent>,
    repo_path: &str,
    lane_key: &str,
    client: &unison::ProtocolClient,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TermCmd>,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    // F1b: 共有 connection 上に per-lane terminal 用 "gui" stream を開く (旧: lane ごと別 connect)。
    let channel = client
        .open_channel("gui")
        .await
        .map_err(|e| format!("open gui channel: {}", e))?;
    // 当該 lane の terminal topic を pattern 指定で subscribe (= demand を立てて repo pump を起こす)。
    let topic = format!("repo/terminal/data/{}/out", lane_key.replace('/', "~"));
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "repo_path": repo_path, "pattern": topic }),
        )
        .await
        .map_err(|e| format!("terminal subscribe handshake: {}", e))?;
    tracing::info!(
        "terminal session connected: lane={} topic={}",
        lane_key,
        topic
    );

    loop {
        tokio::select! {
            recvd = channel.recv() => {
                let msg = match recvd {
                    Ok(m) => m,
                    Err(_) => return Ok(SubscriptionOutcome::Disconnected),
                };
                if msg.msg_type != MessageType::Event || msg.method != "pane" {
                    continue;
                }
                let payload = match msg.payload_as_value() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // LaneTerminalOutput { lane, session, data(base64) }。 lane は subscription で
                // 確定済なので、session（doc 50 §4.6 A6 — 同 topic に複数 session が流れる）と
                // data を抜いて lane_key 付きで JS に渡す。session 欠落は 1（旧 sender 互換 =
                // RepoMessage 側の serde default と同値）。
                if let Some(data) = payload.get("data").and_then(|v| v.as_str())
                    && proxy
                        .send_event(AppEvent::TerminalOutput {
                            lane: lane_key.to_string(),
                            session: payload
                                .get("session")
                                .and_then(serde_json::Value::as_u64)
                                .and_then(|n| u32::try_from(n).ok())
                                .unwrap_or(1),
                            data: data.to_string(),
                        })
                        .is_err()
                {
                    return Ok(SubscriptionOutcome::AppClosing);
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(TermCmd::Write(session, data)) => {
                        // session=0 は「未指定」= repo が root に解決する（slot 系の規律）。
                        let mut payload = serde_json::json!({ "lane": lane_key, "data": data });
                        if session > 0 {
                            payload["session"] = serde_json::Value::from(session);
                        }
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "terminal_write",
                                &payload,
                            )
                            .await;
                    }
                    Some(TermCmd::Resize(session, cols, rows)) => {
                        let mut payload =
                            serde_json::json!({ "lane": lane_key, "cols": cols, "rows": rows });
                        if session > 0 {
                            payload["session"] = serde_json::Value::from(session);
                        }
                        // ⚠️ 応答は捨てる。repo 側は slot 未登録でも **intent を預かって登録時に
                        // 適用する**（`LanePool::desired_size`）ので、ここで retry する必要が無い。
                        // 2026-07-26 以前はそれが無く、この `let _` が「resize が落ちた」ことを
                        // 3 層にわたって不可視にしていた。
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "terminal_resize",
                                &payload,
                            )
                            .await;
                    }
                    // cmd_tx drop = lane removed → session 終了 (channel drop で demand stop)。
                    None => return Ok(SubscriptionOutcome::AppClosing),
                }
            }
        }
    }
}

// =============================================================================
// Conversation gui (doc 32): per-lane conversation session — 構造化イベント購読 + prompt 投入
// =============================================================================
//
// terminal session と同型だが **demand-driven**: lane reconcile には結合させず、
// ChatPane を開いた lane で初回 submit された時に lazy spawn する (repo 側 host の
// lazy モデルと一致)。subscribe → submit の順で走るため取りこぼしなし。

/// Conversation session への command (WebView → repo)。
#[derive(Debug)]
enum ConversationCmd {
    /// プロンプト投入。 canvas channel 上り request `conversation_submit` で repo に送る。
    /// session（doc 50 P2）: None = focused（repo 側 payload_session_key の後方互換）。
    Submit {
        reply: tokio::sync::oneshot::Sender<crate::conversation_submission::SubmitReply>,
        prompt: String,
        session: Option<u32>,
        /// 添付画像（chat 入力欄への貼り付け）。空 = text だけ。
        images: Vec<serde_json::Value>,
    },
    /// doc 35 PR1: PromptCard 回答。 canvas channel 上り request `conversation_respond` で repo に送る。
    Respond {
        request_id: String,
        answers: Option<serde_json::Value>,
        behavior: Option<String>,
        message: Option<String>,
        session: Option<u32>,
    },
    /// doc 35 §5 / PR2: 実行中 turn の中断。 canvas channel 上り request `conversation_interrupt` で repo へ。
    Interrupt { session: Option<u32> },
    /// doc 35 §2.5 / PR3: permission mode 切替。 canvas channel 上り request `conversation_set_permission_mode` で repo へ。
    SetPermissionMode { mode: String, session: Option<u32> },
}

/// 1 lane の conversation session handle (event loop が保持)。map から remove で cmd_tx drop → 停止。
struct LaneConversation {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<ConversationCmd>,
}

/// lane の conversation を Daemon "canvas" channel に乗せる per-lane session を spawn。
///
/// `repo/conversation/data/{lane_key}/event` を subscribe → repo host が emit する ConversationEvent を
/// `AppEvent::ConversationEvent` で event loop に流し、 cmd (submit) は同 channel の上り request
/// `conversation_submit` で repo に forward する (terminal session の gui 対応)。
fn spawn_conversation_session(
    rt_handle: &tokio::runtime::Handle,
    proxy: EventLoopProxy<AppEvent>,
    conn: SharedDaemonConn,
    repo_path: String,
    lane_key: String,
) -> LaneConversation {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    rt_handle.spawn(conversation_session_loop(
        proxy, conn, repo_path, lane_key, cmd_rx,
    ));
    LaneConversation { cmd_tx }
}

/// conversation session の購読 → 再購読を司る long-lived ループ (terminal_session_loop と同型)。
async fn conversation_session_loop(
    proxy: EventLoopProxy<AppEvent>,
    mut conn: SharedDaemonConn,
    repo_path: String,
    lane_key: String,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<ConversationCmd>,
) {
    loop {
        let client = match conn.wait_client().await {
            Some(c) => c,
            None => return, // app 終了
        };
        match run_conversation_session(&proxy, &repo_path, &lane_key, &client, &mut cmd_rx).await {
            Ok(SubscriptionOutcome::AppClosing) => return,
            Ok(SubscriptionOutcome::Disconnected) => {}
            Err(e) => {
                tracing::warn!("conversation session error: lane={}: {}", lane_key, e);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// 1 回の conversation session: connect → `open_channel("canvas")` → subscribe(conversation pattern) →
/// recv (ConversationEvent) / cmd (submit) の select ループ (run_terminal_session と同型)。
async fn run_conversation_session(
    proxy: &EventLoopProxy<AppEvent>,
    repo_path: &str,
    lane_key: &str,
    client: &unison::ProtocolClient,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ConversationCmd>,
) -> Result<SubscriptionOutcome, String> {
    use unison::network::MessageType;

    let channel = client
        .open_channel("gui")
        .await
        .map_err(|e| format!("open gui channel (conversation): {}", e))?;
    let topic = format!(
        "repo/conversation/data/{}/event",
        lane_key.replace('/', "~")
    );
    channel
        .request::<serde_json::Value, serde_json::Value>(
            "subscribe",
            &serde_json::json!({ "repo_path": repo_path, "pattern": topic }),
        )
        .await
        .map_err(|e| format!("conversation subscribe handshake: {}", e))?;
    tracing::info!(
        "conversation session connected: lane={} topic={}",
        lane_key,
        topic
    );

    // subscribe 直後に engine 復活 + replay の demand を**毎回**明示的に撃つ。
    //
    // 背景: gui engine は demand-driven。本来は購読 0→1 を daemon の TopicRouter demand hook が
    // 検知して repo に conversation_demand_start を reverse-route し ensure_chat_engine で復活させる経路が
    // あるが、この edge は 2 つのレースで取りこぼされる:
    // (a) full restart 直後の「Daemon 復帰 / repo 再登録 / surface 再購読 / router 生成」の多者間レース
    //     （refire_active_demands の救済も順序に脆い）→ submit まで engine 不在（⚠/💤 固着）
    // (b) **前任 GUI の残留購読**: pkill された旧 GUI の QUIC 購読が cleanup される前に新 GUI が
    //     subscribe すると 1→2 で edge が立たず、demand が発火しない = **chat が空で始まる**
    //     （transcript replay 不発、2026-07-24 実測 — swap 連打で毎回再現）。
    // ここで forward request として明示的に撃つと forward_to_sp_control が **request 時に repo を
    // lookup** するためレースに強い。冪等なので二重発火は無害: ensure_chat_engine は既起動なら
    // no-op / transcript replay は ReplayStart の clear-prefix で収束する（自動 hook と重なっても
    // 一瞬の再描画のみ）。旧実装は「初回は自動 hook に任せる」と reconnect 限定にしていたが、
    // (b) は初回 attach でこそ起きるため撃ち分けをやめた。
    if let Err(e) = channel
        .request::<serde_json::Value, serde_json::Value>(
            "conversation_demand_start",
            &serde_json::json!({ "lane": lane_key }),
        )
        .await
    {
        tracing::warn!(
            "conversation attach demand_start 失敗（次 submit の self-heal に委ねる, lane={}）: {}",
            lane_key,
            e
        );
    }

    loop {
        tokio::select! {
            recvd = channel.recv() => {
                let msg = match recvd {
                    Ok(m) => m,
                    Err(_) => return Ok(SubscriptionOutcome::Disconnected),
                };
                if msg.msg_type != MessageType::Event || msg.method != "pane" {
                    continue;
                }
                let payload = match msg.payload_as_value() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // RepoMessage::ConversationEvent { lane, session, event } の生 JSON。 event と
                // session を抜いて lane_key 付きで JS に渡す (lane は subscription で確定済)。
                // doc 38 Phase 2: session を落とさず通す（旧 sender / N=1 では default 1）。
                if let Some(event) = payload.get("event") {
                    let session = payload
                        .get("session")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(1) as u32;
                    if proxy
                        .send_event(AppEvent::ConversationEvent {
                            lane: lane_key.to_string(),
                            event: event.clone(),
                            session,
                        })
                        .is_err()
                    {
                        return Ok(SubscriptionOutcome::AppClosing);
                    }
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(ConversationCmd::Submit { prompt, session, images, reply }) => {
                        if reply.is_closed() { continue; }
                        // session: None は JSON null になり、repo 側 payload_session_key が
                        // focused に解決する（旧 UI / 旧 SP との後方互換）。
                        let result = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "conversation_submit",
                                &serde_json::json!({
                                    "lane": lane_key, "prompt": prompt, "session": session,
                                    "images": images,
                                }),
                            )
                            .await;
                        let _ = reply.send(result);
                    }
                    Some(ConversationCmd::Respond { request_id, answers, behavior, message, session }) => {
                        // allow/deny のどちらの形も同 request に載せる（repo 側が behavior で分岐）。
                        let mut req = serde_json::json!({
                            "lane": lane_key, "request_id": request_id, "session": session,
                        });
                        if let Some(a) = answers {
                            req["answers"] = a;
                        }
                        if let Some(b) = behavior {
                            req["behavior"] = serde_json::Value::String(b);
                        }
                        if let Some(m) = message {
                            req["message"] = serde_json::Value::String(m);
                        }
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>("conversation_respond", &req)
                            .await;
                    }
                    Some(ConversationCmd::Interrupt { session }) => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "conversation_interrupt",
                                &serde_json::json!({ "lane": lane_key, "session": session }),
                            )
                            .await;
                    }
                    Some(ConversationCmd::SetPermissionMode { mode, session }) => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "conversation_set_permission_mode",
                                &serde_json::json!({
                                    "lane": lane_key, "mode": mode, "session": session,
                                }),
                            )
                            .await;
                    }
                    // cmd_tx が drop された = 呼び手が session を畳んだ（repo collapse /
                    // lane 削除 / app 終了）。**明示的に購読を降りてから**抜ける。
                    //
                    // ⚠️ channel を drop するだけでは daemon の demand は下がらない。
                    // daemon が `router.unsubscribe` を呼ぶのは受信ループが切れた時だけで、
                    // ここで黙って return すると「GUI は見ていないのに demand は立ったまま」
                    // になり engine の idle teardown が永久に発火しない（2026-08-29 実測:
                    // 畳んだ 7 repo の engine が 11 分経っても残っていた）。
                    None => {
                        let _ = channel
                            .request::<serde_json::Value, serde_json::Value>(
                                "unsubscribe",
                                &serde_json::json!({}),
                            )
                            .await;
                        tracing::info!(
                            "conversation session disconnected: lane={lane_key} (unsubscribed)"
                        );
                        return Ok(SubscriptionOutcome::AppClosing);
                    }
                }
            }
        }
    }
}

/// Phase 2.5 (per-Lane instance): main_view の JS API を呼ぶ helper 群。
/// xterm.js + WebSocket は **JS-side で per-Lane に管理** され、 Rust は thin trigger を出すだけ。
/// lane の term session（tui = mode "tui"）の (session, is_root) 一覧を返す。
///
/// doc 50 §4.6 A6: xterm は (lane, session) ごとなので、boot / lane 選択の経路は
/// 「この lane にどの term pane が要るか」をここで解決する。registry snapshot（`sessions`）が
/// 無い旧 SP からの wire は root=1 の 1 枚に畳む（従来挙動 = lane に xterm 1 枚）。
fn term_sessions_of(lane: &crate::daemon_wire::LaneInfo) -> Vec<(u32, bool)> {
    match &lane.sessions {
        Some(reg) if !reg.sessions.is_empty() => reg
            .sessions
            .iter()
            .filter(|s| s.mode != "gui")
            .map(|s| (s.key, s.key == reg.root))
            .collect(),
        // registry 不在（boot 窓の placeholder / N=1 特殊ケース）: root 1 枚（tui）に畳む。
        _ => vec![(1, true)],
    }
}

/// roster を push すべきか（前回渡した値と違うか）。純関数 = テスト可能。
///
/// doc 53 §11: LanesLoaded は定期 snapshot でも走る高頻度 event なので、**変化した lane だけ**
/// 撃つ（毎回撃つと webview が roster を作り直して pane が無用に再配置される）。
///
/// ⚠️ **replay 経路（`WebviewReady`）はこの gate を通さない** — bundle ロード前の
/// `evaluate_script` は無言 no-op になるのに指紋だけ残るため、gate を共有すると boot 窓で
/// 落ちた 1 回目を永久に取り戻せない（team-b 指摘 2026-07-25）。
fn roster_push_needed(
    last: &std::collections::HashMap<String, String>,
    addr: &str,
    payload: &serde_json::Value,
) -> bool {
    last.get(addr) != Some(&payload.to_string())
}

/// [`roster_push_needed`] の対 — 撃った値を覚える。
fn remember_roster_push(
    last: &mut std::collections::HashMap<String, String>,
    addr: &str,
    payload: &serde_json::Value,
) {
    last.insert(addr.to_string(), payload.to_string());
}

/// 消えた lane の指紋を落とす（同名再作成で「変化なし」と誤判定しないため）。
fn forget_roster_push(last: &mut std::collections::HashMap<String, String>, addr: &str) {
    last.remove(addr);
}

/// roster を webview へ渡す（push envelope `console:session_list` → `vp:conversation-sessions`）。
///
/// doc 53 §11: 呼び手は LanesLoaded の 1 箇所だけ（旧実装は動詞ごとの再取得 7 箇所から
/// 撃っていた — 供給路が 2 本ある構造そのものだった）。
fn push_session_list(webview: &wry::WebView, lane: &str, payload: &serde_json::Value) {
    push_main::console_session_list(webview, lane, payload.clone());
}

/// doc 53 §11: lane snapshot の roster を webview の session 一覧 payload に写す（純関数）。
///
/// **roster の供給はこの 1 本**。旧実装は `conversation_session_list` の ask 結果を流していたが、
/// その fetch は「lane を開いた時 / GUI 自身が動詞を撃った後 / boot 窓の再送」でしか走らず、
/// **CLI・MCP 由来の session 変化が pane grid に出なかった**（doc 53 §11.1）。snapshot は
/// server が動詞の末尾で push する（`emit_lane_update`）ので、誰が起こした変化でも届く。
///
/// payload の形は webview 契約（`console.ts` の `ConversationSessionListPayload`）そのまま — 供給路を差し替える
/// だけで消費側（tab strip / pane grid / 名札）は無改造。`root` / `focused` は entry の
/// bool に展開する（webview は entry ごとの flag で読む）。
/// ⚠️ **entry は手書きの写像**。`LaneSessionEntryWire` に field を足しても
/// **ここに書かない限り webview へ届かない**（rustc は何も言わない = 静かに落ちる）。
/// 2026-08-30 に `image_capable` を roster と wire mirror に足したのにこの 1 箇所を忘れ、
/// 「server は true を返しているのに client では false」で貼り付け UI が出なかった。
fn session_list_payload(
    lane: &str,
    sessions: &crate::daemon_wire::LaneSessionsWire,
) -> serde_json::Value {
    let entries: Vec<serde_json::Value> = sessions
        .sessions
        .iter()
        .map(|s| {
            serde_json::json!({
                "key": s.key,
                "agent": s.agent,
                "engine_session_id": s.conversation,
                "focused": s.key == sessions.focused,
                "root": s.key == sessions.root,
                "mode": s.mode,
                "chat_capable": s.chat_capable,
                "image_capable": s.image_capable,
                "model": s.model,
                "model_choices": s.model_choices,
                "permission_choices": s.permission_choices,
            })
        })
        .collect();
    serde_json::json!({ "lane": lane, "focused": sessions.focused, "sessions": entries })
}

/// lane address から root session key を引く（snapshot 由来。不明は 1 = 従来の既定）。
///
/// lane 名しか手元に無い経路（mode 切替の適用など）が root の xterm を ensure するのに使う。
fn root_session_of(sidebar_state: &crate::pane::SidebarState, lane: &str) -> u32 {
    sidebar_state
        .lanes_by_repo
        .values()
        .flatten()
        .find(|l| l.address.key() == lane)
        .and_then(|l| l.sessions.as_ref().map(|r| r.root))
        .unwrap_or(1)
}

/// Architecture v4: sidebar の active selection に応じて main area の表示 kind を切替。
///
/// Phase 5-A 拡張: Lane と component が **mutually exclusive** な active 軸として扱われる。
/// 優先順位:
///   1. `active_component` Some → kind = "board" / "runner" / "devices"
///   2. `active_lane_address` Some → kind = "lane"、 pane_id = Lane address
///   3. 両方 None → kind=None で empty placeholder
///
/// Lane address ごとの terminal 接続は per-Lane xterm.js (Phase 2.5) が JS-side で管理。
/// root session の mode（"tui" | "gui"）を wire の `sessions`（registry snapshot）から導出する。
///
/// doc 53 R1: 旧 `console_mode` field（root mode の投影）は退役 — **client 側の導出はこの
/// 1 関数に閉じる**（読み手 3 系統: chat 判定 / respawn gate / header 差分。R4 で pane 一覧
/// 配信に差し替える時の改修点もここ 1 箇所）。sessions 欠落（boot 窓の placeholder 等）は
/// "tui"（旧 serde default と同値）に倒す。
fn root_mode_of(lane: &crate::daemon_wire::LaneInfo) -> &str {
    lane.sessions
        .as_ref()
        .and_then(|reg| reg.sessions.iter().find(|s| s.key == reg.root))
        .map(|s| s.mode.as_str())
        .unwrap_or("tui")
}

/// 指定 lane address が gui (root mode="gui") かを `lanes_by_repo` から引く。
///
/// 未知 address (LanesLoaded 未着 等) は false (= tui 扱い) に倒す。 chat lane は
/// engine-less (pid=None) が正常形なので、 pid では判定できない — sessions（registry
/// snapshot）由来の root mode が真実源（doc 53 R1）。
fn lane_is_chat(state: &SidebarState, address: &str) -> bool {
    state
        .lanes_by_repo
        .values()
        .flatten()
        .find(|l| l.address.key() == address)
        .map(|l| root_mode_of(l) == "gui")
        .unwrap_or(false)
}

/// doc 38 §4.2: `conversation_session_list` payload（`{focused, sessions:[{key, agent, focused, ...}]}`）
/// から focused session の agent を引く。New Session の chat 分岐で「現 focused と同じ engine の
/// 新 Draft を作る」ために使う。`focused` フラグ優先 → `focused` key 一致 → 先頭 の順で解決し、
/// 取れなければ None（backend が lane 既定 agent を使うため送らなくてよい）。純粋 = テスト可能。
fn focused_session_agent(payload: &serde_json::Value) -> Option<String> {
    let sessions = payload.get("sessions").and_then(|v| v.as_array())?;
    let focused_key = payload.get("focused").and_then(|v| v.as_u64());
    sessions
        .iter()
        .find(|s| s.get("focused").and_then(|v| v.as_bool()) == Some(true))
        .or_else(|| {
            focused_key.and_then(|k| {
                sessions
                    .iter()
                    .find(|s| s.get("key").and_then(|v| v.as_u64()) == Some(k))
            })
        })
        .or_else(|| sessions.first())
        .and_then(|s| s.get("agent").and_then(|v| v.as_str()).map(str::to_string))
}

#[cfg(test)]
mod header_lane_fields_changed_tests {
    use super::header_lane_fields_changed;
    use crate::daemon_wire::LaneInfo;

    /// 最小 LaneInfo（全 field serde default）に engine_session_id だけ与える。
    fn lane(engine_session_id: Option<&str>) -> LaneInfo {
        serde_json::from_value(serde_json::json!({
            "address": {"kind": "root", "repo": "vp"},
            "engine_session_id": engine_session_id,
        }))
        .expect("LaneInfo deserialize")
    }

    /// 供給 push 根治: session chip の供給源（engine_session_id）の変化と消灯を検知する。
    #[test]
    fn detects_engine_session_id_change() {
        assert!(header_lane_fields_changed(
            &lane(Some("old")),
            &lane(Some("new"))
        ));
        assert!(header_lane_fields_changed(&lane(Some("old")), &lane(None)));
    }

    /// 変化なしは false（LanesLoaded は高頻度 loop event — setActivePane を無駄打ちしない）。
    #[test]
    fn unchanged_is_false() {
        assert!(!header_lane_fields_changed(
            &lane(Some("same")),
            &lane(Some("same"))
        ));
        assert!(!header_lane_fields_changed(&lane(None), &lane(None)));
    }
}

#[cfg(test)]
mod focused_session_stand_tests {
    use super::focused_session_agent;

    /// focused フラグ付き session の agent を引く（doc 38 §4.2 New Session の chat 分岐）。
    #[test]
    fn picks_stand_of_focused_flagged_session() {
        let payload = serde_json::json!({
            "focused": 2,
            "sessions": [
                {"key": 1, "agent": "claude", "focused": false},
                {"key": 2, "agent": "codex", "focused": true},
            ]
        });
        assert_eq!(focused_session_agent(&payload).as_deref(), Some("codex"));
    }

    /// focused フラグが無ければ `focused` key と一致する session に落ちる。
    #[test]
    fn falls_back_to_focused_key() {
        let payload = serde_json::json!({
            "focused": 3,
            "sessions": [
                {"key": 1, "agent": "claude"},
                {"key": 3, "agent": "grok"},
            ]
        });
        assert_eq!(focused_session_agent(&payload).as_deref(), Some("grok"));
    }

    /// どちらも決まらなければ先頭 session の agent（安全側 = とにかく作れる）。
    #[test]
    fn falls_back_to_first_session() {
        let payload = serde_json::json!({
            "sessions": [{"key": 1, "agent": "claude"}, {"key": 2, "agent": "codex"}]
        });
        assert_eq!(focused_session_agent(&payload).as_deref(), Some("claude"));
    }

    /// sessions が空 / 欠落なら None（backend の lane 既定 agent に委ねる）。
    #[test]
    fn returns_none_when_no_sessions() {
        assert_eq!(focused_session_agent(&serde_json::json!({})), None);
        assert_eq!(
            focused_session_agent(&serde_json::json!({"sessions": []})),
            None
        );
    }
}

/// lane を conversation topic に attach する（`terminal_sessions` の対）。
///
/// 購読 0→1 が daemon の demand hook を撃ち、chat session を持つ lane では repo が
/// **transcript replay**（過去会話）を返す。これが無いと conversation topic は非 retained
/// なので「submit するまで ChatView が空」になる（app 再起動で会話が消えたように見える）。
/// idempotent — 既に session があれば no-op。
///
/// doc 58 ②-a: 旧実装は「chat session を持つ lane だけ」に張っていたが、名簿の
/// now-line（`vp now` → NowLine event）は **TUI lane からも** conversation topic に
/// 流れてくるため、gate を外して全 lane に張る。chat 専用の副作用（transcript replay /
/// eager engine spawn）は repo 側 `handle_conversation_demand_start` の session-mode gate
/// が守る — TUI session の demand は `not_chat` で graceful no-op（ReplayStart も出ない）。
fn ensure_conversation_attach(
    address: &str,
    sidebar_state: &SidebarState,
    conversation_sessions: &mut std::collections::HashMap<String, LaneConversation>,
    rt_handle: &tokio::runtime::Handle,
    proxy: &EventLoopProxy<AppEvent>,
    daemon_conn: &SharedDaemonConn,
) {
    // 購読は lane 単位で全 session の event を運ぶ（doc 50 §4.6 A6 — root=tui + 非 root=chat
    // の構成も 1 本で拾う）。mode による撃ち分けは repo 側 gate に委譲済み（上記 doc）。
    let Some(repo_path) = resolve_repo_path_for_lane(sidebar_state, address) else {
        return; // repo 未解決 (LanesLoaded 未着) — 後続の LanesLoaded で再評価される
    };
    // 「Lane タイトルが見えている Lane だけ生きている」（mako 2026-08-28）: accordion を
    // 畳んだ repo の lane は名簿に出ないので、購読も外す。購読 1→0 が daemon の demand hook
    // を撃ち、repo 側 `conversation_demand_stop` が暇な engine を寝かせる。
    //
    // ⚠️ **detach は now-line も止める**（NowLine は conversation topic を通るため）。
    // 畳んだ repo の「今」は名簿ごと見えないので、モデル上これで正しい（mako 裁定）。
    // 副作用: 開き直した直後は now-line が空欄（NowLine は replay 対象外 = 揮発の自己申告、
    // conversation_pump の doc）。次の `vp now` で埋まる。
    if !repo_is_expanded(sidebar_state, &repo_path) {
        if conversation_sessions.remove(address).is_some() {
            tracing::info!("conversation detach (repo collapsed): {}", address);
        }
        return;
    }
    if conversation_sessions.contains_key(address) {
        return;
    }
    tracing::info!("conversation attach (chat lane): {}", address);
    let session = spawn_conversation_session(
        rt_handle,
        proxy.clone(),
        daemon_conn.clone(),
        repo_path,
        address.to_string(),
    );
    conversation_sessions.insert(address.to_string(), session);
}

fn push_active_view(main_view: &WebView, state: &SidebarState) {
    let info = if let Some(agent) = state.active_component.as_ref() {
        ActivePaneInfo {
            kind: Some(agent.kind.as_str()),
            pane_id: None,
            preview_url: None,
            chat: false,
            // 非 lane pane (Agent) は Conversation ヘッダの lane 情報を持たない。
            cwd: None,
            branch: None,
            lane_name: None,
            session_id: None,
            agent: None,
        }
    } else if let Some(addr) = state.active_lane_address.as_deref() {
        // Conversation 共通ヘッダ用: active lane の LaneInfo から cwd / branch を引く。cwd は
        // address (pane_id) から導出できない唯一の lane 情報なので、setActivePane に相乗り
        // させて運ぶ (新しい配信チャネルは増やさない)。branch は sub のみ (安価に取れる時)。
        let lane = state
            .lanes_by_repo
            .values()
            .flatten()
            .find(|l| l.address.key() == addr);
        ActivePaneInfo {
            kind: Some("lane"),
            pane_id: Some(addr),
            preview_url: None,
            // doc 33: chat lane は xterm を持たない (ChatView が内容)。 これを JS に伝えないと
            // showLane が「xterm 無し = 内容無し」と誤判定し placeholder が ChatView を覆う。
            chat: lane_is_chat(state, addr),
            cwd: lane.map(|l| l.cwd.as_str()).filter(|c| !c.is_empty()),
            branch: lane
                .and_then(|l| l.sub_status.as_ref())
                .and_then(|p| p.branch.as_deref()),
            // doc 44 P2: 旧 `LaneInfo.name` は複製 field で **常に None** だった（JS は addr
            // 短縮名に fallback していた）。フラット化で `address.name` が唯一の在処になり、
            // 常に実体を持つのでヘッダにそのまま供給できる。
            lane_name: lane.map(|l| l.address.name.as_str()),
            // tui の session chip はこの相乗りが唯一の供給路（gui は event が上書き）。
            session_id: lane.and_then(|l| l.engine_session_id.as_deref()),
            // doc 39 P4-C: chip prefix は root session の engine（agent_name）を優先する
            // （cross-engine root で slot の engine を正しく映す）。無ければ lane 固定の agent に fallback。
            agent: lane
                .map(|l| l.agent_name.as_deref().unwrap_or(l.agent.as_str()))
                .filter(|st| !st.is_empty()),
        }
    } else {
        ActivePaneInfo {
            kind: None,
            pane_id: None,
            preview_url: None,
            chat: false,
            cwd: None,
            branch: None,
            lane_name: None,
            session_id: None,
            agent: None,
        }
    };
    let script = main_area::build_set_active_pane_script(&info);
    if let Err(e) = main_view.evaluate_script(&script) {
        tracing::warn!("main setActivePane 失敗: {}", e);
    }
}

/// LanesLoaded の snapshot 差し替えで、active lane の Conversation ヘッダに載る field が変わったか。
///
/// `push_active_view` 再発行の gate（供給 push 根治）。LanesLoaded は loop event で頻発する
/// ため毎回撃つと setActivePane が noise になる — header が実際に読む field（session chip /
/// cwd / branch / lane 名 / agent / Mode 初期値）に変化がある時だけ true を返す。
fn header_lane_fields_changed(
    prev: &crate::daemon_wire::LaneInfo,
    next: &crate::daemon_wire::LaneInfo,
) -> bool {
    prev.engine_session_id != next.engine_session_id
        || prev.cwd != next.cwd
        || prev.agent != next.agent
        // doc 39 P4-C: chip prefix は agent_name（root session の engine）で決まるため、
        // その変化（cross-engine root 切替）でも header を再 push する。
        || prev.agent_name != next.agent_name
        || prev.address.name != next.address.name
        // doc 53 R1: root mode の変化（mode 切替 / root 付け替え）は sessions から導出して比較。
        || root_mode_of(prev) != root_mode_of(next)
        || prev
            .sub_status
            .as_ref()
            .and_then(|p| p.branch.as_deref())
            != next
                .sub_status
                .as_ref()
                .and_then(|p| p.branch.as_deref())
}

/// Lane address (Display 形 `"<repo>/root"` 等) から所属 repo path を逆引きする。
///
/// `lanes_by_repo` (= repo_path → LaneInfo list) を走査し、 `address.key()` が一致する
/// lane を持つ repo の path を返す。 `lane:select` 経路 (= JS から path を受け取る) の鏡像で、
/// focus 経路は address しか持たないためここで path を解決する。 一致なしは None。
fn resolve_repo_path_for_lane(state: &SidebarState, address: &str) -> Option<String> {
    state
        .lanes_by_repo
        .iter()
        .find(|(_path, lanes)| lanes.iter().any(|l| l.address.key() == address))
        .map(|(path, _)| path.clone())
}

/// repo の accordion が開いているか（= その repo の lane タイトルが名簿に出ているか）。
///
/// conversation 購読の gate（[`ensure_conversation_attach`]）が使う。**未知の repo は
/// 開いている扱い**に倒す — 判断材料が無い時に「畳んでいる」と決めつけると、
/// snapshot 未着の窓で購読を落として engine を寝かせてしまう（fail-open）。
fn repo_is_expanded(state: &SidebarState, repo_path: &str) -> bool {
    state
        .processes
        .iter()
        .find(|p| p.path == repo_path)
        .is_none_or(|p| p.expanded)
}

/// Active Lane を切替える — 全副作用を 1 箇所に集約（Simplicity 原則）。
///
/// sidebar click / switch_lane (QUIC) / auto-select の 3 入口すべてがこの関数を呼ぶ。
/// 副作用:
///   1. `sidebar_state.active_lane_address` + `active_component` (排他 clear)
///   2. `session_state` 永続化
///   3. notification / awaiting_input reset
///   4. sidebar UI push (`sidebar:state`)
///   5. main area push (`setActivePane` → `showLane`)
///   6. dead lane respawn
#[allow(clippy::too_many_arguments)]
fn activate_lane(
    address: &str,
    sidebar_state: &mut SidebarState,
    session_state: &mut crate::session_state::SessionState,
    webview: &wry::WebView,
    lane_respawn_triggered: &mut std::collections::HashSet<String>,
    rt_handle: &tokio::runtime::Handle,
    respawn_proxy: &EventLoopProxy<AppEvent>,
) {
    // 1. State
    sidebar_state.active_lane_address = Some(address.to_string());
    if sidebar_state.active_component.is_some() {
        sidebar_state.active_component = None;
    }

    // 2. Session persistence
    session_state.active_lane_address = Some(address.to_string());
    session_state.save();

    // 3. Notification reset (同 lane click 連打でも badge を消す)
    sidebar_state.unread_notifications.remove(address);
    sidebar_state.awaiting_input.remove(address);
    // canvas 着信 badge (D) も active 化で消す (unread_notifications と同 lifecycle)。
    sidebar_state.canvas_unread.remove(address);

    // 4-6. UI push + dead lane respawn
    // BUG#3: 旧実装は push_active_view / respawn を `view_changed` (active_lane_address が
    // 変わった時だけ) に gate していたが、 address 一致だが main area 未表示 / pump 未成立
    // (restart 直後の楽観反映 × canonical desync) の状態で同一 lane を再 click すると no-op に
    // なり「切り替えられない」。 setActivePane → showLane は冪等 (setWantedLane 撤去済で WS 付替
    // churn 無し)、 respawn は triggered set で dedup 済なので、 view_changed に依らず毎回実行して
    // desync を確定的に解消する。 activate_lane の 3 caller (初回 auto-select は
    // active_lane_address.is_none() gate で 1 回 / switch_lane / sidebar click) はいずれも
    // genuine activation で高頻度発火しないため、 毎回 push しても focus 奪取 flood は起きない。
    push_sidebar_state(webview, sidebar_state);
    push_active_view(webview, sidebar_state);
    // doc 50 §4.6 A6: lane 単位 console_mode の同期は退役。表示（roster + focus）は World B の
    // `applyLaneView` が lane 切替を契機に開き、顔ぶれは session 一覧 × 各 session の mode から
    // 導出される（見え方は session の属性なので、lane 単位の mode を送る意味が無くなった）。
    maybe_respawn_dead_lane(
        address,
        sidebar_state,
        lane_respawn_triggered,
        rt_handle,
        respawn_proxy,
    );
}

/// オンデマンド respawn: active にしようとする lane が Dead (pid:null) なら repo に restart_lane を
/// 発火して蘇らせる。 lane (main / sub) の Conversation プロセスが死ぬと repo の lifecycle monitor は
/// Dead を検知するだけで auto-respawn しない (server.rs の設計判断) ため、 user が lane を
/// 開いた時点でオンデマンドに復活させる。 これが無いと「一度死んだ lane は手動 restart するまで
/// Conversation が出ない」状態になる (= 全 repo で console 非表示の真因)。
///
/// dedup: `triggered` set で同一 lane の連打を防ぐ (LanesLoaded は loop event で頻発するため必須)。
/// 解除タイミングは 2 つ: (a) lane が Running に戻った時 caller が `triggered.remove` する、
/// (b) restart_lane が失敗した時 `AppEvent::LaneRespawnFailed` 経由で caller が `triggered.remove`
/// する (= 失敗が永続 suppression にならないようにする、 Moody Blues Issue #1)。
fn maybe_respawn_dead_lane(
    addr: &str,
    state: &SidebarState,
    triggered: &mut std::collections::HashSet<String>,
    rt_handle: &tokio::runtime::Handle,
    proxy: &EventLoopProxy<AppEvent>,
) {
    // addr の lane を lanes_by_repo から探し、 所属 repo path と pid を取得。
    let entry = state.lanes_by_repo.iter().find_map(|(path, lanes)| {
        lanes
            .iter()
            .find(|l| l.address.key() == addr)
            .map(|l| (path.clone(), l.pid, root_mode_of(l).to_string()))
    });
    let Some((repo_path, pid, root_mode)) = entry else {
        return; // lane 未知 (まだ LanesLoaded 来てない等) — 後続の LanesLoaded で再評価される
    };
    if pid.is_some() {
        return; // Running、 respawn 不要
    }
    // doc 33 §3: chat lane は engine-less (pid=None) が正常形。
    // respawn 対象は「root mode=tui かつ pid=None」のみ（chat lane を殺しに行かない — #683
    // 再演防止。mode は sessions 由来 — doc 53 R1）。
    if root_mode == "gui" {
        return;
    }
    // dedup: 既に respawn 進行中なら skip
    if !triggered.insert(addr.to_string()) {
        return;
    }
    // F6③: 旧 DaemonRpcClient.restart_lane (repo 直結 reqwest) を daemon repo-proxy ask
    // (lane_restart) に移管。 repo port 解決は不要 (Daemon :32000 固定 + repo_path handshake)、
    // 旧「port 未解決 skip」分岐も消滅。 失敗時の trigger 解除は LaneRespawnFailed 経路に一本化。
    let addr_owned = addr.to_string();
    let proxy = proxy.clone();
    tracing::info!("auto-respawn dead lane (on-demand): addr={}", addr_owned);
    rt_handle.spawn(async move {
        // auto-respawn は Dead lane の復活なので会話を継ぐ (fresh=false)。
        let payload = serde_json::json!({ "address": &addr_owned, "fresh": false });
        match daemon_repo_request(
            crate::daemon::default_daemon_port(),
            &repo_path,
            "lane_restart",
            payload,
        )
        .await
        {
            Ok(_) => {
                // 成功時は LanesLoaded で Running 検出時に triggered から解除される。
                tracing::info!("auto-respawn lane_restart ok: {}", addr_owned);
            }
            Err(e) => {
                tracing::warn!("auto-respawn lane_restart failed: {}: {}", addr_owned, e);
                // 失敗を event loop に通知して triggered を解除する (永続 suppression 回避)。
                // これが無いと repo クラッシュ等で全 retry 失敗した lane は vp-app 再起動まで
                // auto-respawn 対象外になってしまう (Moody Blues Issue #1)。
                let _ = proxy.send_event(AppEvent::LaneRespawnFailed {
                    address: addr_owned,
                });
            }
        }
    });
}

/// window の現在の geometry + 表示モードを SessionState に write-through する (doc 30 §3.4a / §6.1)。
///
/// - **通常ウィンドウ**: 位置・サイズ・monitor・`display_mode=Windowed` を `set_window_geometry`。
/// - **全画面**: `inner_size()` は fullscreen frame を返し windowed 座標を潰すため、 `set_display_mode`
///   で mode + monitor のみ更新し、 直前の windowed 座標を保持する (全画面解除で元の窓サイズに戻せる)。
///
/// `save()` は呼ばない (caller が open flag 等とまとめて save する)。 outer_position 取得失敗時は
/// windowed 座標を更新できないので geometry を触らず返る (mode 更新は全画面時のみで別経路)。
fn persist_window_geometry(session_state: &mut SessionState, window: &tao::window::Window) {
    let monitor_name = window.current_monitor().and_then(|m| m.name());
    if window.fullscreen().is_some() {
        session_state.set_display_mode(crate::session_state::DisplayMode::Fullscreen, monitor_name);
        return;
    }
    let scale = window.scale_factor();
    match window.outer_position() {
        Ok(pos) => {
            let inner = window.inner_size().to_logical::<f64>(scale);
            let logical_pos = pos.to_logical::<f64>(scale);
            session_state.set_window_geometry(crate::session_state::WindowGeometry {
                width: inner.width,
                height: inner.height,
                x: logical_pos.x,
                y: logical_pos.y,
                monitor: monitor_name,
                display_mode: crate::session_state::DisplayMode::Windowed,
            });
        }
        Err(e) => {
            tracing::warn!("outer_position() 取得失敗 (geometry save skip): {}", e);
        }
    }
}

/// daemon 側（settings.kdl）から取れた設定。**取れなかった場合と未設定を区別する**ため
/// `Option` を包んでいる（doc 59 P3）。
///
/// `None` = daemon に届かなかった（オフライン / 旧 binary）。この時 UI は該当区画を
/// 「daemon に接続すると編集できます」に落とす — 空欄を編集可能に見せると、押しても
/// 保存できない**行き止まり**になる。
#[derive(Debug, Clone, Default)]
struct DaemonSettings {
    log_level: Option<String>,
    idle_timeout_minutes: Option<i64>,
    /// 既定 agent × model（doc 59 P4）。**組で保持する** — 別々に持つと
    /// 「codex なのに claude の model」を UI 側で再構成できてしまう。
    default_agent: Option<String>,
    default_model: Option<String>,
    /// 実効 agent が model 指定を受け付けるか。**daemon が判定した結果**をそのまま持つ
    /// （vp-app は engine の能力表明を知らない = 一覧を複製しない）。
    default_agent_takes_model: bool,
}

/// 設定 overlay へ返す確定値を組み立てる（doc 59 P1 + P3）。
///
/// `developer_mode` は **実効値**（env > vp-app.toml > `debug_assertions` の解決後）を渡す —
/// event loop が持っている `dev_mode` がその値なので、それをそのまま映す。
/// `resolved_repo_root` は明示値が無いときに実際に使われるパスで、入力欄の placeholder に
/// なる（「空欄だが実際はここ」を見せるため）。
///
/// ⚠️ **真実源が 2 つある**面なので、それぞれの持ち主を分けて扱う:
/// - `vp-app.toml`（GUI 固有）= developer_mode / default_repo_root — この関数が同期で読む
/// - `settings.kdl`（好み、daemon 所有）= log_level / idle_timeout — `daemon` 引数で渡る
fn settings_snapshot(
    settings: &Settings,
    sidebar_state: &SidebarState,
    dev_mode: bool,
    daemon: Option<&DaemonSettings>,
) -> crate::generated::sidebar_ipc::SettingsResult {
    crate::generated::sidebar_ipc::SettingsResult {
        developer_mode: dev_mode,
        developer_mode_locked: developer_mode_env().is_some(),
        default_repo_root: settings.default_repo_root.clone(),
        resolved_repo_root: resolve_default_repo_root(settings, sidebar_state)
            .map(|p| p.display().to_string()),
        daemon_reachable: daemon.is_some(),
        log_level: daemon.and_then(|d| d.log_level.clone()),
        idle_timeout_minutes: daemon.and_then(|d| d.idle_timeout_minutes),
        default_agent: daemon.and_then(|d| d.default_agent.clone()),
        default_model: daemon.and_then(|d| d.default_model.clone()),
        // daemon が判定した結果をそのまま流す。UI はこれが false なら model 欄を出さない
        // （codex は VP から model を渡さない = 押しても効かない欄を並べない）。
        default_agent_takes_model: daemon.is_some_and(|d| d.default_agent_takes_model),
    }
}

/// lane を「入力待ち（要注意）」として記録し、sidebar の unread count / 黄 dot を更新する。
///
/// active lane（今まさに見ている lane）は即読扱いで skip する（見ている lane に dot を出さない）。
/// これは通知の**単一 sink** で、2 つのソースがここに合流する。tui は OSC 99/9/777
/// notification（`AppEvent::OscNotification`、xterm が parse）。gui は
/// `ConversationEvent::turn_completed`（headless stream-json は Notification hook を発火しないため、
/// stream `result` 由来の turn_completed が「Claude が返し終えた＝入力待ち」の唯一のシグナル。
/// memory echoes-act2-notification-signal 参照）。
/// `source` はログ用ラベル（`"osc:notification"` / `"gui:turn_completed"` 等）。
fn mark_lane_awaiting_input(
    lane: &str,
    source: &str,
    sidebar_state: &mut SidebarState,
    webview: &WebView,
) {
    if sidebar_state.active_lane_address.as_deref() == Some(lane) {
        tracing::debug!("{source} skip (active lane): lane={lane}");
        return;
    }
    let count = sidebar_state
        .unread_notifications
        .entry(lane.to_string())
        .or_insert(0);
    *count += 1;
    // 「入力待ち」 = 行右端に黄 dot。 active 切替で reset される。
    sidebar_state.awaiting_input.insert(lane.to_string(), true);
    tracing::info!("{source} lane={lane} unread={}", *count);
    push_sidebar_state(webview, sidebar_state);
}

/// lane に Canvas (board) show が着信したことを sidebar の canvas_unread に計上する。
///
/// `mark_lane_awaiting_input` (HITL/OSC = 黄 dot) とは**別 sink**。Canvas 着信は sidebar 行に
/// Canvas 専用 icon (Phosphor easel) として出し、「用事(黄 dot)」と「絵が届いた(easel)」の
/// 語彙を分離する (bug: canvas 可観測性 D、show 偽 success の viewer 文脈対策)。
/// active lane（今見ている lane）宛の show は panel 側 (pp-overlay auto-open) で解決するので、
/// ここでは badge を出さない（呼び出し側で active 判定済だが二重防御で skip）。
fn mark_lane_canvas_unread(lane: &str, sidebar_state: &mut SidebarState, webview: &WebView) {
    if sidebar_state.active_lane_address.as_deref() == Some(lane) {
        return;
    }
    let count = sidebar_state
        .canvas_unread
        .entry(lane.to_string())
        .or_insert(0);
    *count += 1;
    tracing::info!("canvas:show lane={lane} canvas_unread={}", *count);
    push_sidebar_state(webview, sidebar_state);
}

/// address だけから lane の workdir を引く（code pane 用）。
///
/// main bundle（CodePane.tsx）は sidebar と違い `lanes_by_repo` の repo_path を
/// 持たないため、address（`LaneAddressWire::key()` = daemon 発行の canonical）を
/// 全 repo に対して探す。key は repo 名を含む合成キーなので全 repo 走査でも
/// 衝突しない（`<repo>/lane/<name>` — 同名 lane が別 repo に居ても key が違う）。
fn lookup_lane_cwd_by_address(state: &SidebarState, address: &str) -> Option<std::path::PathBuf> {
    state
        .lanes_by_repo
        .values()
        .flatten()
        .find(|l| l.address.key() == address)
        .map(|l| std::path::PathBuf::from(&l.cwd))
}

// R-0 (`docs/design/11-vp-app-refactor.md` § 3.0a / `mem_1CaaaDoXHZvhR46ZfLN6jx`):
//   旧 `lane_address_key(&LaneAddressWire) -> String` 関数は `lane_address.rs::LaneAddressWire::key()`
//   メソッドに移管 (G2 解消、 3 重実装の 1 元化)。 caller は `wire.key()` で同等の文字列を取れる。

/// App のエントリポイント
pub fn run() -> anyhow::Result<()> {
    // R-1 (`docs/design/11-vp-app-refactor.md` § 3.1 / `mem_1CaaaDoXHZvhR46ZfLN6jx`):
    //   tracing init を `crate::log_init::init_tracing()` に切り出し済。
    //   filter resilience (PR #235) + appender + KdlFormatter wiring + 起動ログを内包。
    let _log = crate::log_init::init_tracing();

    // VP-192: 旧 config/data パスからの冪等なデータ移行 (Settings/SessionState 読み込み前)
    vp_paths::migrate_legacy_paths();

    // ink（対話面, doc 52 §3）: 送信済み snapshot は ephemeral だが disk に残るので、起動時に
    // 7 日超を掃除する（「消し手のないファイルを作らない」— terminal replay disk leak の轍）。
    crate::webview::ink_snapshot::prune_old(Duration::from_secs(7 * 24 * 3600));

    // Windows taskbar の identity。 **window を作る前**に設定する必要がある
    // (既存 window の AUMID は後から変えられない)。 非 Windows は no-op。
    crate::icon::set_app_user_model_id();

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();

    // 根治: vp-app 共有 Tokio runtime (multi-thread)。
    //
    // tao の event_loop は macOS main thread を専有し、 closure 内には Tokio
    // runtime context が無いため、 bare `tokio::spawn` を呼ぶと
    // 「no reactor running」 panic で即死する (= 過去事故、 board 永続化 #456241e 等)。
    //
    // 全 async work はここで作る共有 runtime の `Handle::spawn` に乗せる。
    // closure / helper 関数には `rt_handle.clone()` を move-capture で配る。
    // `tokio::spawn` 直書きは `crates/vp-app/.clippy.toml` の
    // `disallowed-methods` で compile-time block。
    //
    // `_rt` は `run()` の戻りまで生存させる (= drop すると runtime が止まる)。
    let _rt = tokio::runtime::Runtime::new()?;
    let rt_handle = _rt.handle().clone();

    // VP-100 follow-up: 永続設定 + 1Password 風 開発者モード切替
    let mut settings = Settings::load();
    let initial_dev_mode = initial_developer_mode(&settings);
    tracing::info!("Settings: developer_mode = {} (initial)", initial_dev_mode);

    // メニューバー (View → Developer Mode / Open Developer Tools を含む) + トレイ
    let menu_handles = crate::menu::build_menu_bar(initial_dev_mode);
    let _menu = menu_handles.menu.clone();
    // macOS: NSApp に menu を attach、 accelerator (Cmd+N 等) を NSApplication menu hotkey 化。
    // これを呼ばないと MenuItem::new() の accelerator が NSResponder chain で発火しない。
    // 既存の PredefinedMenuItem (close_window/undo/copy 等) は muda 内部で auto-attach されるが、
    // user-defined MenuItem は明示の init_for_nsapp が要る。
    #[cfg(target_os = "macos")]
    {
        // muda 0.17: Menu::init_for_nsapp() でメニューバーに attach
        menu_handles.menu.init_for_nsapp();
    }
    let open_devtools_item = menu_handles.open_devtools_item;
    let reload_webview_item = menu_handles.reload_webview_item;
    let menu_ids = menu_handles.ids;
    let _tray = match crate::tray::build_tray() {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!("トレイ初期化失敗 (無効化): {}", e);
            None
        }
    };

    // muda の MenuEvent を main loop に橋渡しする pump を起動
    spawn_menu_event_pump(&rt_handle, event_loop.create_proxy());

    // F1b (doc 27 §3.4.4): vp-app → Daemon :32000 の全 persistent session を 1 QUIC connection に
    // 集約する共有ハンドル。 manager task が connect/reconnect を一手に所有し、 各 session
    // (device/lanes/canvas/terminal) は `wait_client` で得た共有 client に open_channel する。
    // event loop closure が move capture するので、 closure 内の spawn は `daemon_conn.clone()` を渡す。
    let daemon_conn = spawn_daemon_conn_manager(&rt_handle, crate::daemon::default_daemon_port());

    // フィードバック方向 (doc 49 LE-19): webview の場の状態 → daemon-device 上り event。
    // watch = latest-wins (webview が throttle 済みでも Rust 側で自然に coalesce)。
    // 送り手 = ipc_handler の "fleet:feedback" 分岐 / 受け手 = device session の sender task。
    let (fleet_feedback_tx, fleet_feedback_rx) =
        tokio::sync::watch::channel(serde_json::Value::Null);

    // ACTIONS の永続化 (doc 57 Phase 4)。同じく watch = latest-wins。
    // 送り手 = `handle_sidebar_ipc` の `actions:persist` 分岐 / 受け手 = 下の debounce task。
    let (actions_persist_tx, actions_persist_rx) =
        tokio::sync::watch::channel::<Option<ActionsPersistPayload>>(None);
    spawn_actions_persist_writer(&rt_handle, actions_persist_rx, daemon_conn.clone());

    // DeviceRegistry 🧲 device event を daemon (daemon-device channel) から購読する (daemon に 1 本)。
    // canvas/lanes は per-repo だが device は machine scope (= daemon singleton) なので起動時 1 回。
    spawn_device_subscription(
        &rt_handle,
        event_loop.create_proxy(),
        daemon_conn.clone(),
        fleet_feedback_rx,
    );

    // vp-app instance index 判定 (= multi-window 復元)。 per-instance file load に先立って
    // 必要なので session_state より前に確定する。
    // `VP_APP_INSTANCE` (= "0", "1", ...) が instance 番号。 未設定 / "0" = primary。
    let instance_index: usize = std::env::var("VP_APP_INSTANCE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let is_primary = instance_index == 0;
    tracing::info!(
        "vp-app boot: instance_index={} (= {})",
        instance_index,
        if is_primary { "primary" } else { "secondary" }
    );

    // session_state を WindowBuilder より前に load して、 window geometry (= 前回終了時の
    // position + size + monitor) を起動時に復元できるようにする。 per-instance 分離後は
    // **自分の instance file** (`session.json` / `session.<N>.json`) を読む。 `mut` で keep し、
    // 後段で active_lane_address / repos / currents_order 等の mutate + save にも使う。
    let mut session_state = SessionState::load(instance_index);
    // この instance window を「開いている」 と記録する (= 次回 primary 起動時の auto-spawn
    // signal)。 clean close (`CloseRequested`) で `open=false` に上書きするので、 明示的に
    // 閉じた window は復活せず、 kill された window は復元される。
    session_state.set_open(true);
    session_state.save();

    // PR #458: invalid geometry (= MIN 未満 / NaN / Inf) は None に fallback。
    // per-instance 分離後は自分の file の geometry を使う。
    let restored_geometry = session_state.window_geometry().cloned();

    // 最低サイズ + 起動時 size 強制矯正 — sidebar (固定 280px) 圧縮 bug の構造的防御。
    //
    // 1. `with_min_inner_size`: sidebar 幅 (280) + 余裕ある main 領域を構造的に確保する OS
    //    レベル下限 (NSWindow.setMinSize)。 手動 drag による narrow 化を防ぐ。
    // 2. 起動時 clamp: macOS state restoration は `applicationDidFinishLaunching` 後の
    //    async phase で `restorableState` を frame に反映するため、 build 直後の同期
    //    `inner_size()` チェックは race する (#428 Moody Blues Issue #1 で発覚)。
    //    EventLoop が走り始めた**最初の Resized event** (= restoration 適用後) で
    //    min 未満を検出して `set_inner_size(DEFAULT)` で force-resize する経路に移行。
    //    詳細は event loop の Resized handler 側コメント。
    // 3. window geometry 復元: `session_state.window_geometry` Some なら前回の size +
    //    position を apply (= 個別位置)。 None なら default。 monitor 復元は EventLoop
    //    走り始め後に `available_monitors()` で確認、 disconnect されてれば primary 内に clamp。
    let mut builder = WindowBuilder::new()
        .with_title("Vantage Point")
        .with_min_inner_size(LogicalSize::new(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT));
    // window icon (Windows: titlebar + taskbar / Linux: WM)。 Windows は exe に焼いた icon
    // resource が主役だが、 window 単位の icon を明示しておくと起動経路によらず確実に出る。
    // mac は dock icon (icon::set_app_icon) が担当で window icon の概念が無いため素通り。
    if let Some((rgba, w, h)) = crate::icon::icon_rgba(256)
        && let Ok(icon) = tao::window::Icon::from_rgba(rgba, w, h)
    {
        builder = builder.with_window_icon(Some(icon));
    }
    if let Some(geom) = &restored_geometry {
        builder = builder
            .with_inner_size(LogicalSize::new(geom.width, geom.height))
            .with_position(LogicalPosition::new(geom.x, geom.y));
    } else {
        builder = builder.with_inner_size(LogicalSize::new(
            DEFAULT_WINDOW_WIDTH,
            DEFAULT_WINDOW_HEIGHT,
        ));
    }
    let window = builder.build(&event_loop)?;

    // 表示モード復元 (doc 30 §6.1): windowed 座標で build した後、 保存が Fullscreen なら全画面化する。
    // windowed frame を base に残すことで全画面解除時に元の窓サイズへ戻せる。 monitor 精密指定は
    // EventLoop 走行後の `available_monitors()` race を避け、 current monitor (= 復元位置の display) で
    // 全画面化する (`Borderless(None)`)。 monitor 相対の厳密復元は doc 30 §6.2 の将来課題。
    if restored_geometry
        .as_ref()
        .is_some_and(|g| g.display_mode == crate::session_state::DisplayMode::Fullscreen)
    {
        tracing::info!(
            "session restore [instance={}]: 全画面モードを復元",
            instance_index
        );
        window.set_fullscreen(Some(tao::window::Fullscreen::Borderless(None)));
    }

    // primary 起動時、 前回「開いていた」 secondary instance (= `session.<N>.json` で
    // open==true、 N≥1) を **child process として auto-spawn** する。 これで「複数 window を
    // 開いて再起動 → 全 window 復元」 が動く。 明示的に閉じた (= clean close で open=false)
    // instance は復活しない ─ per-instance file 分離 + open flag 管理によって、 共有 1 file
    // 時代の「close しても slot が残り再 spawn される」 bug を根治した。
    //
    // 子は `VP_APP_INSTANCE=<idx>` で自分の file を read する。
    // spawn 失敗は warn して continue (= primary 起動は阻害しない)。
    if is_primary {
        let to_spawn = SessionState::open_secondary_indices();
        if !to_spawn.is_empty() {
            match std::env::current_exe() {
                Ok(exe) => {
                    for idx in to_spawn {
                        match std::process::Command::new(&exe)
                            .env("VP_APP_INSTANCE", idx.to_string())
                            .spawn()
                        {
                            Ok(child) => tracing::info!(
                                "auto-spawned secondary instance (pid={}, instance_index={})",
                                child.id(),
                                idx
                            ),
                            Err(e) => tracing::warn!(
                                "auto-spawn secondary (instance={}) failed (起動は継続): {}",
                                idx,
                                e
                            ),
                        }
                    }
                }
                Err(e) => tracing::warn!("current_exe() 失敗 (auto-spawn skip): {}", e),
            }
        }
    }

    // Terminal backend: daemon を auto-launch (down なら `vp` binary を spawn)。
    let proxy = event_loop.create_proxy();
    // Phase 2.5 (per-Lane instance): startup の placeholder PTY 接続は撤去。
    // Lane が出現するまで main area は empty placeholder ("No Lane selected") のみ。
    // ただし daemon の auto-launch だけは継続 (sidebar の Activity widget や
    // /api/daemon/repos 取得に必要)。
    let _ = proxy; // 旧 spawn_shell / connect_daemon_terminal で proxy を消費していた、 互換用に残す
    let node_url = std::env::var("VP_DAEMON_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", crate::daemon::default_daemon_port()));
    if let Err(e) = crate::daemon::launcher::ensure_daemon_ready(&node_url) {
        tracing::warn!(
            "daemon auto-launch 失敗 (continue with offline state): {}",
            e
        );
    }

    // daemon から repo list を非同期 fetch (起動初回)
    spawn_processes_fetch(&rt_handle, event_loop.create_proxy(), daemon_conn.clone());
    // VP-95: Activity widget の定期更新 (5s 間隔)
    spawn_activity_poller(&rt_handle, event_loop.create_proxy(), daemon_conn.clone());
    // VP-143: cc session display name (custom-title) の 5s 周期 resolve
    spawn_session_title_poller(&rt_handle, event_loop.create_proxy());
    // VP-147 PR-P2-3: per-Lane mailbox inbox 状況の 5s 周期 resolve (sidebar message icon 用 signal)
    spawn_lane_inbox_poller(&rt_handle, event_loop.create_proxy());

    // WebView 統合 (step 3a): sidebar + main を 1 WebView (1 DOM, CSS flex) に統合。
    // sidebar.bundle.js は vp-asset://app/sidebar.bundle.js の外部 script として load される
    // (doc 48 Phase 1 で inline → 外部化。#sidebar-root に mount)。
    // 旧 2 WebView (cross-WebView IPC bridge で keyboard を 2 往復させていた) を廃し、
    // sidebar↔main の event / state が同一 DOM 内で直接流れる。
    let sidebar_ipc_proxy = event_loop.create_proxy();
    let ipc_proxy = event_loop.create_proxy();
    // DevTools は compile 時 always 有効。menu の「Open Developer Tools」から
    // `webview.open_devtools()` を呼ぶかで runtime 制御 (本番ビルドでも切替可)。
    // echo probe trigger (Unison 北極星 step 2/3): VP_UNISON_ECHO_CERT が set なら
    // webview load 前に cert を global へ注入する。 entry.tsx が load 時に検出して
    // window.vpUnisonEcho を auto-run し、 結果は console bridge 経由で app.kdl.log に出る
    // (= agent が DevTools なしで round-trip を観測する経路)。 未 set なら空 script で no-op。
    let echo_init = std::env::var("VP_UNISON_ECHO_CERT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|cert| {
            format!(
                "window.__VP_ECHO_CERT__ = {};",
                serde_json::to_string(&cert).unwrap_or_else(|_| "\"\"".into())
            )
        })
        .unwrap_or_default();
    let webview = WebViewBuilder::new()
        // 統合 origin fix: with_html (about:blank = 不透明オリジン) だと localStorage が
        // SecurityError を throw し sidebar bundle が boot 中に落ちる。custom protocol で
        // 実オリジン (vp-asset://app) を与え、MAIN_AREA_HTML を app/index.html として配信する。
        .with_custom_protocol("vp-asset".to_string(), move |id, request| {
            crate::webview::assets::serve(id, request, MAIN_VIEW_ASSETS)
        })
        .with_initialization_script(&echo_init)
        .with_url("vp-asset://app/index.html")
        .with_bounds(Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: WryLogicalSize::new(DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT).into(),
        })
        .with_devtools(true)
        .with_ipc_handler(move |req| {
            // 統合 ipc dispatch: t 値で明示分岐 (sidebar tag と main tag は disjoint)。
            // main tag (terminal / pane 系) → terminal、 それ以外 (sidebar IpcEnvelope:
            // repo: / lane: 系) → SidebarIpc。 terminal の fall-through に頼らない。
            let body = req.body();
            // fleet feedback (LE-19) は event loop を経由せず watch へ直行 (高頻度 + 状態量)
            if let Some(fb) = fleet_feedback_payload(body) {
                let _ = fleet_feedback_tx.send(fb);
                return;
            }
            if is_main_ipc_tag(body) {
                terminal_ipc::handle_ipc_message(body, &ipc_proxy);
            } else {
                let _ = sidebar_ipc_proxy.send_event(AppEvent::SidebarIpc(body.to_string()));
            }
        })
        .with_focused(true)
        .build_as_child(&window)?;

    tracing::info!("メインウィンドウ作成 (sidebar + main を 1 WebView に統合)");

    // 起動直後の bounds 明示同期 — 「下部が空く」 bug の構造的 fix。
    // WebView の初期 `with_bounds` は DEFAULT_WINDOW_HEIGHT (800) 固定なので、 復元 geometry が
    // DEFAULT より大きい (= 前回 window を縦に広げていた) 場合、 起動後に `WindowEvent::Resized`
    // が発火しない限り content が 800px のまま下部が黒く空く。 macOS は `with_inner_size` で
    // born した window に初回 Resized を出さないことがあるため、 ここで実 inner_size に
    // 明示同期して初回 paint から content view を全面に張る (Resized handler と idempotent)。
    update_pane_bounds(&webview, window.inner_size(), window.scale_factor());

    // Phase 2.x-d: 旧 single-PTY 経路 (`xterm_ready` / `pending` / `PENDING_MAX`) は撤去。
    // per-Lane instance + browser-native WebSocket では各 Lane の xterm.js が独立に
    // WS から bytes を受けるので、 Rust 側で buffer / flush 同期する必要が無い。
    // VP-95: sidebar 全体 state (repos + widget + activity)
    let mut sidebar_state = SidebarState::default();
    // in-app update: 適用フロー実行中フラグ（GUI local）。ActivitySnapshot は health poll で
    // 定期上書きされるため、event loop 側で保持して毎回 snapshot に再適用する。
    let mut update_applying = false;
    // session_state は WindowBuilder 上で既に load 済 (= window geometry を先に必要)。
    // 直前 active Lane を初回 LanesLoaded で復元するための pending 値。
    // 1 度復元したら None にして、 後続 LanesLoaded で再復元しないように。
    let mut pending_session_active_lane: Option<String> = session_state.active_lane_address.clone();
    // SidebarState に currents_order を即反映 (renderRepos がこの順で並べる)
    sidebar_state.currents_order = session_state.currents_order.clone();
    // VP-100 γ-light: pane_id → slot rect。Phase 2 では蓄積するだけ、Phase 4+ で
    // native overlay の `set_position` 同期に使う。
    let mut slot_rects: std::collections::HashMap<String, SlotRect> =
        std::collections::HashMap::new();
    // repo auto-spawn: 1 セッションで同じ repo を二重 trigger しないための guard。
    // path をキーにする (repo_name は重複しうる、 path は正規化済 unique)。
    // daemon 側でも `Process already running` で弾かれるが、 無駄な POST を避ける。
    let mut repo_spawn_triggered: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // オンデマンド respawn: active にする lane が Dead (pid:null) の時に restart_lane を 1 回だけ
    // 発火するための guard。 lane address をキーにする。 lane が Running に戻ったら (LanesLoaded で
    // pid あり検出時) entry を解除し、 再度 Dead 化した時に再 respawn できるようにする。
    let mut lane_respawn_triggered: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // maybe_respawn_dead_lane の async restart_lane が失敗した時に event loop へ
    // 通知を返し lane_respawn_triggered を解除するための proxy (永続 suppression 回避)。
    let respawn_proxy = event_loop.create_proxy();
    // R sidebar の debug log tail の世代カウンタ（sidebar view modes、2026-08-01）。
    // watch / unwatch のたびに進め、旧世代の tail thread は次の poll で自然に退場する
    // （= 最後の watch が勝つ単一 tail。join も channel 後始末も不要 — debug_log.rs 参照）。
    let debuglog_watch_gen: std::sync::Arc<std::sync::atomic::AtomicU64> =
        std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    // wiremsg Stage 1: per-repo の "lanes" Unison 購読を 1 本だけ張るための guard。
    // path をキーにする。F1b: 購読は共有 connection に追従して give-up しないので、 一度
    // spawn したら app 終了まで張りっぱなし (= guard から除去されない)。
    let mut lanes_sub_active: std::collections::HashSet<String> = std::collections::HashSet::new();
    // wiremsg Stage 2: per-repo の "gui" Unison 購読 guard (lanes_sub_active と同型)。
    let mut canvas_sub_active: std::collections::HashSet<String> = std::collections::HashSet::new();
    // board pane の boot 窓救済（doc 52 §10 wave 0、device 一覧と同型）: gui channel で届いた
    // BoardUpdated を repo × lane で保持し、`AppEvent::WebviewReady` の replay で再配信する。
    // retained BoardUpdated は bundle 評価前に届いて受け口不在で落ちるため、これが無いと
    // reopen 時に board pane が出ない（live show まで空）。
    let mut board_snapshots: std::collections::HashMap<
        String,
        std::collections::HashMap<String, serde_json::Value>,
    > = std::collections::HashMap::new();
    // terminal S4: per-lane terminal session registry (lane key → LaneTerminal)。
    // LanesLoaded で live lane に対し start、 消えた lane / app 終了で stop (= map から remove)。
    let mut terminal_sessions: std::collections::HashMap<String, LaneTerminal> =
        std::collections::HashMap::new();
    // Conversation gui (doc 32): per-lane conversation session registry (lane key → LaneConversation)。
    // terminal と違い demand-driven: ConversationSubmit の初回で lazy spawn (reconcile 非結合)。
    let mut conversation_sessions: std::collections::HashMap<String, LaneConversation> =
        std::collections::HashMap::new();
    // doc 53 §11: lane ごとに **最後に webview へ渡した roster** の指紋。定期 snapshot で
    // 同じ roster を撃ち直して pane を作り直さないための変化検知（`header_lane_fields_changed`
    // と同じ「変化時のみ push」の規律）。
    //
    // ⚠️ 旧実装にはここに **取りこぼした fetch の保留箱**（`pending_session_fetch`）が在った。
    // 供給が fetch 1 本だった時代、boot 直後の要求が repo 未解決で捨てられると
    // 「pane も名札も出ない」になり、再試行の契機が無かったため箱で救っていた。
    // 供給が snapshot（retained + 変化時 push）に一本化された今、取りこぼしという状態自体が
    // 存在しない（doc 53 §6.5.2 が予言した「供給路を 1 本にすれば要らない」）。
    let mut last_roster_push: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // VP-100 follow-up (1Password 風): runtime 開発者モード state
    let mut dev_mode = initial_dev_mode;
    // 直近に daemon から引けた settings.kdl の値（doc 59 P3）。
    // 保持するのは、folder picker のように **vp-app.toml だけを触る操作**の後でも
    // daemon 側の表示を消さないため（毎回引き直すと overlay が一瞬空欄になる）。
    // None のままなら「まだ引けていない / 接続できない」。
    let mut last_daemon_settings: Option<DaemonSettings> = None;
    // repo:add 等の async 操作で event loop に repo list 再 fetch を kick するための proxy
    let async_action_proxy = event_loop.create_proxy();

    // 起動時 size clamp 用 once-flag。 macOS state restoration の `restorableState` は
    // EventLoop 起動後の async phase で frame に反映され、 初回の `WindowEvent::Resized`
    // として届く。 この flag が false のうちに来た Resized が「restoration 適用直後」と
    // みなして min 制約と照合し、 必要なら force-resize する (#428 Moody Blues Issue #1)。
    // PR #458 fix: 保存 geometry を復元した path では起動時 clamp を skip。
    // 復元値 (with_inner_size apply 済) を macOS state restoration race 由来の小 size で
    // 上書きしないため、 復元 path 中は最初の Resized event を「正常な user-driven resize」
    // 扱いにする。 default path (= restored_geometry None) では従来通り clamp logic を走らせる。
    let mut initial_size_clamp_done = restored_geometry.is_some();

    // PR #459 throttled save: window resize / move 中も 500ms throttle で session save。
    // CloseRequested の force save に依存しない (= `ge app:stop` の SIGTERM kill や crash
    // でも直近 state が persistent)。 dogfood で「ge app で再起動すると save 走らない」
    // bug を解消。
    const GEOMETRY_SAVE_THROTTLE: std::time::Duration = std::time::Duration::from_millis(500);
    let mut last_geometry_save = std::time::Instant::now() - std::time::Duration::from_secs(1);

    // dock app icon (portal favicon) の再アサート用。 bare binary は .app bundle が無いため
    // macOS が launch 完了時に generic icon を被せ、 run() 前 (window.build 直後) の
    // setApplicationIconImage を上書きする。 event loop 開始後 ~1.5s 間 set_app_icon() を
    // 呼び続けて (WaitUntil で loop を起こす) portal icon を定着させる。 .dmg 版は冪等。
    let icon_launch_at = std::time::Instant::now();
    let mut icon_settled = false;

    // Model B (focus = 操舵ポインタ): この vp-app instance が OS の key window かを追跡する。
    // multi-window は別プロセス (VP_APP_INSTANCE = primary 0 / secondary N) なので、ROTO の
    // switch_lane broadcast は全 instance の "canvas" 購読に届く。両 window が一斉に切り替わるのを
    // 防ぐため、**focused instance だけ**が switch_lane を適用する (B-local self-filter)。
    // with_focused(true) で起動するので初期値は true。
    let mut is_focused = true;
    // Model B #2: 直近 daemon に報告した active_lane。 focus が高速に flip しても
    // 同じ lane への重複報告 (= reqwest::Client 新規構築 + 無駄 POST) を抑止する。
    let mut last_focus_reported_lane: Option<String> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // launch settle まで dock icon を再設定 (bare binary 対策)。 settle 後は通常の Wait。
        if !icon_settled {
            crate::icon::set_app_icon();
            if icon_launch_at.elapsed() < std::time::Duration::from_millis(1500) {
                *control_flow = ControlFlow::WaitUntil(
                    std::time::Instant::now() + std::time::Duration::from_millis(150),
                );
            } else {
                icon_settled = true;
            }
        }

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                tracing::info!("Window close requested");
                // この window は **明示的に閉じられた** → 次回 primary 起動時に auto-respawn
                // しないよう自 instance file に open=false を記録する。 強制 kill (= SIGTERM /
                // crash) では CloseRequested が来ないので open=true のまま残り、 復元される。
                session_state.set_open(false);
                // window geometry + 表示モード (position/size/monitor/fullscreen) も自 instance file に
                // save。 起動時に WindowBuilder + set_fullscreen で apply されて前回の配置に復元される。
                persist_window_geometry(&mut session_state, &window);
                if let Some(g) = session_state.window_geometry() {
                    tracing::info!(
                        "session save [instance={}]: window geometry ({}x{} @ {},{}, monitor={:?}, mode={:?}), open=false",
                        instance_index,
                        g.width,
                        g.height,
                        g.x,
                        g.y,
                        g.monitor.as_deref(),
                        g.display_mode
                    );
                }
                // open=false (+ geometry) を確実に書き出す (outer_position 失敗でも open は残す)。
                session_state.save();
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } => {
                let scale = window.scale_factor();
                // 初回 Resized = macOS state restoration 適用後の frame。 min 未満なら
                // force-resize して default に揃える (#428 Moody Blues Issue #1 fix)。
                // 2 回目以降は user resize / clamp 由来の通常 resize として update_pane_bounds 走らせる。
                if !initial_size_clamp_done {
                    initial_size_clamp_done = true;
                    let logical = size.to_logical::<f64>(scale);
                    if logical.width < MIN_WINDOW_WIDTH || logical.height < MIN_WINDOW_HEIGHT {
                        tracing::info!(
                            "vp-app: 起動時 window size ({}x{}) が min 未満 → {}x{} に矯正",
                            logical.width,
                            logical.height,
                            DEFAULT_WINDOW_WIDTH,
                            DEFAULT_WINDOW_HEIGHT
                        );
                        window.set_inner_size(LogicalSize::new(
                            DEFAULT_WINDOW_WIDTH,
                            DEFAULT_WINDOW_HEIGHT,
                        ));
                        // set_inner_size → 後続 Resized event で update_pane_bounds が正しく走る。
                        // この event は restoration の小 size なので bounds 更新 skip。
                        return;
                    }
                }
                update_pane_bounds(&webview, size, scale);
                // PR #459 throttled save: resize 中も 500ms throttle で geometry + 表示モードを save。
                // 全画面 enter/exit も Resized を撃つので、 helper 内の fullscreen 判定で mode が追従する。
                let now = std::time::Instant::now();
                if now.duration_since(last_geometry_save) > GEOMETRY_SAVE_THROTTLE {
                    last_geometry_save = now;
                    persist_window_geometry(&mut session_state, &window);
                    session_state.save();
                }
            }
            Event::WindowEvent {
                event: WindowEvent::Moved(_),
                ..
            } => {
                // PR #459 throttled save: window 移動中も 500ms throttle で geometry + 表示モードを save。
                // Resized と pair (= drag による size 変更だけでなく位置変更も capture)。
                let now = std::time::Instant::now();
                if now.duration_since(last_geometry_save) > GEOMETRY_SAVE_THROTTLE {
                    last_geometry_save = now;
                    persist_window_geometry(&mut session_state, &window);
                    session_state.save();
                }
            }
            // Model B (focus = 操舵ポインタ): focus 状態を追跡する。OS の key window は全プロセス間で
            // 1 つだけなので、ちょうど 1 つの instance が is_focused=true になる。これにより ROTO の
            // switch_lane broadcast を「今見ている window」だけが適用し、focus を切り替えるだけで
            // 操舵対象 window が移る (seamless)。
            Event::WindowEvent {
                event: WindowEvent::Focused(focused),
                ..
            } => {
                is_focused = focused;
                tracing::debug!("window focus changed: is_focused={}", focused);
                // Model B #2: focus を得た瞬間、 この window の display lane を daemon canonical の
                // active_lane に報告する。 daemon active_lane が focused window に追従 → ROTO LCD
                // follows focus (#4) が「active_lane を映すだけ」 で自動成立する。 focus-loss (false)
                // は無視 ── 次に focus を得た window が上書きするため (lane 未選択 window も skip)。
                if focused
                    && let Some(address) = sidebar_state.active_lane_address.clone()
                    && last_focus_reported_lane.as_deref() != Some(address.as_str())
                    && let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &address)
                {
                    // 重複報告抑止: 報告する lane を記録してから spawn。 同 lane への
                    // 連続 focus event は上の guard で弾かれ、 RPC は lane 切替時のみ。
                    last_focus_reported_lane = Some(address.clone());
                    let conn = daemon_conn.clone();
                    rt_handle.spawn(async move {
                        let result = match conn.control().await {
                            Ok(control) => control.set_active_lane(path, address).await,
                            Err(e) => Err(e),
                        };
                        if let Err(e) = result {
                            tracing::warn!("focus→set_active_lane failed: {}", e);
                        }
                    });
                }
            }
            // Phase 4-paste-fix: clipboard.readText の webview permission 問題への fallback。
            // IPC `paste:request` を Rust が受けて arboard で読み取り、 ここで JS に inject。
            Event::UserEvent(AppEvent::PasteText(text)) => {
                if text.is_empty() {
                    tracing::debug!("PasteText empty (clipboard 空 or 取得失敗)、 skip");
                } else {
                    // escape は envelope の serde_json 化に含まれる（Phase review fix #3 の
                    // 「手書き escape は null byte / surrogate を見落とす」は、payload ごと
                    // JSON にすることで構造的に解消）。
                    push_main::deliver_paste(&webview, &text);
                }
            }
            Event::UserEvent(AppEvent::OscNotification { lane, code: _ }) => {
                // Phase 5-D Sprint C P2.1: per-Lane HD notification（tui / OSC 由来）。
                // active lane は即読 skip。共通 sink（gui の turn_completed と合流）。
                mark_lane_awaiting_input(&lane, "osc:notification", &mut sidebar_state, &webview);
            }
            Event::UserEvent(AppEvent::ResolveSessionTitles) => {
                // VP-143 → doc 58 ②-c: 全 lane の **session ごと**に cc custom-title を resolve
                // → diff → sidebar に push。poller (`spawn_session_title_poller`) が 5s 間隔で
                // tick を送ってここに来る。resolve は read-only file I/O なので main thread
                // blocking は無視できる範囲。
                //
                // 鍵は `{address}#{session}`（webview 側 `sessionNowKey` と同形 — now-line と
                // 同じ語彙で session を指す）。conversation id を持つ session は jsonl を直接
                // 特定（相部屋で他人の title を拾わない）、持たない session（Draft / TUI の
                // 旧 wire）は root に限り従来の cwd 推定に fallback する（新規に嘘を増やさない
                // — 非 root の cwd 推定は「最新 mtime = root の会話」とぶつかりやすい）。
                let mut changed = false;
                let mut current_keys: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for lanes in sidebar_state.lanes_by_repo.values() {
                    for lane in lanes {
                        let address = lane.address.key();
                        let cwd = std::path::Path::new(&lane.cwd);
                        // sessions registry 欠落（旧 wire / boot 窓）は root=1 の 1 session とみなす
                        let entries: Vec<(u32, Option<String>, bool)> = match &lane.sessions {
                            Some(reg) if !reg.sessions.is_empty() => reg
                                .sessions
                                .iter()
                                .map(|e| (e.key, e.conversation.clone(), e.key == reg.root))
                                .collect(),
                            _ => vec![(1, None, true)],
                        };
                        for (key, conversation, is_root) in entries {
                            let map_key = format!("{address}#{key}");
                            current_keys.insert(map_key.clone());
                            let resolved = match conversation.as_deref() {
                                Some(conv) => {
                                    crate::lane::title::resolve_title_for_conversation(cwd, conv)
                                }
                                None if is_root => {
                                    crate::lane::title::resolve_title_for_cwd(cwd)
                                }
                                None => None,
                            };
                            let prev = sidebar_state.session_titles.get(&map_key).cloned();
                            match (resolved, prev) {
                                (Some(new_title), Some(old)) if old == new_title => {}
                                (None, None) => {}
                                (Some(new_title), _) => {
                                    sidebar_state.session_titles.insert(map_key, new_title);
                                    changed = true;
                                }
                                (None, Some(_)) => {
                                    sidebar_state.session_titles.remove(&map_key);
                                    changed = true;
                                }
                            }
                        }
                    }
                }
                // 既に消えた lane の stale entry 掃除
                let stale: Vec<String> = sidebar_state
                    .session_titles
                    .keys()
                    .filter(|k| !current_keys.contains(k.as_str()))
                    .cloned()
                    .collect();
                for k in stale {
                    sidebar_state.session_titles.remove(&k);
                    changed = true;
                }
                if changed {
                    push_sidebar_state(&webview, &sidebar_state);
                }
            }
            Event::UserEvent(AppEvent::ResolveLaneInboxes) => {
                // VP-147 PR-P2-3: 全 lane の mailbox inbox 状況を resolve → sidebar に push。
                //  poller (`spawn_lane_inbox_poller`) が 5s 間隔で tick を送ってここに来る。
                //  Phase 2 (icon visibility のみ) では default MessageState を populate して
                //  sidebar UI で `.vp-message-icon` 表示の signal とする。 backend peek API
                //  + 永続 store query は後続 PR で実装、 actual 値で MessageState を populate。
                use crate::pane::MessageState;
                let mut changed = false;
                let mut current_keys: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for lanes in sidebar_state.lanes_by_repo.values() {
                    for lane in lanes {
                        let address = lane.address.key();
                        current_keys.insert(address.clone());
                        // Phase 2 placeholder: default MessageState (= unread_count 0、 has_persistent false)
                        // 既存 entry が無い (= 初回 tick or 新規 lane) 場合のみ insert、 上書きしない。
                        // TODO 後続 PR (Phase 2.5): backend peek API を叩いて actual 値で update。
                        //   `Vacant` ガードを `entry().and_modify(|s| *s = fetched).or_insert_with(...)`
                        //   に書き換えて、 既存 entry の `unread_count` 等を refresh する。 現状 (Phase 2)
                        //   は actual 値が無いので Vacant のみ insert で sufficient (icon visibility のみ)。
                        if let std::collections::hash_map::Entry::Vacant(e) =
                            sidebar_state.lane_inboxes.entry(address)
                        {
                            e.insert(MessageState::default());
                            changed = true;
                        }
                    }
                }
                // 既に消えた lane の stale entry 掃除
                let stale: Vec<String> = sidebar_state
                    .lane_inboxes
                    .keys()
                    .filter(|k| !current_keys.contains(k.as_str()))
                    .cloned()
                    .collect();
                for k in stale {
                    sidebar_state.lane_inboxes.remove(&k);
                    changed = true;
                }
                if changed {
                    push_sidebar_state(&webview, &sidebar_state);
                }
            }
            Event::UserEvent(AppEvent::ReposLoaded(repos)) => {
                // 既存 SidebarState とマージ:
                //  - 同じ path があれば既存 state を維持 (expanded / panes / active 保持)
                //  - 新規は RepoPaneState::new (Main Agent 1 つ)
                //  - サーバから消えた repo は除外
                //
                // VP-101 follow-up: register 後の auto-expand。
                // auto-select は LanesLoaded 側で扱う (Architecture v4: 真の selection unit は Lane)。
                // 「prev (旧 sidebar_state.processes) には port があった、 新 repos には port が無い」
                // 形の merge は port を不用意に消すので、 sidebar_state の port は新側 (port_by_name 反映済)
                // で上書きされる。 retroactive ensureLane (= 後段) で None→Some 遷移を補う。
                let prev: std::collections::HashMap<String, RepoPaneState> = sidebar_state
                    .processes
                    .drain(..)
                    .map(|p| (p.path.clone(), p))
                    .collect();
                let is_initial_load = prev.is_empty();
                // Phase A4-3b: drain 前に (path → port) を retain して fetch task に渡す
                let repo_ports: Vec<(String, Option<u16>)> = repos
                    .iter()
                    .map(|p| (p.path.clone(), p.port))
                    .collect();
                // Model Q: daemon canonical の active lane (presence、 boot 復元用)。
                // 注: app の active_lane_address は単一 global (pane.rs) なので、 daemon の
                // per-repo active のうち **order 先頭の 1 つ**を採用する (意図的な単純化、
                // doc 24 §12-H)。 repo ごとに最後の active を復元する per-repo 化は
                // Phase 3 (app 側を per-repo active に拡張、 daemon は既に per-repo 保持)。
                let daemon_active_lane: Option<String> =
                    repos.iter().find_map(|p| p.active_lane.clone());
                sidebar_state.processes = repos
                    .into_iter()
                    .map(|p| {
                        // RepoInfo.state / .port を RepoPaneState に merge
                        // (sidebar JS が processStateMark で 🟢/🔴 badge 表示に使う、
                        //  port は Phase 2 で lane:select 時の WS 接続先決定に使う)
                        let state_str = p.state.as_str().to_string();
                        let port = p.port;
                        let mut pane_state = if let Some(existing) = prev.get(&p.path) {
                            existing.clone()
                        } else {
                            // 新規 repo の expanded 解決:
                            //   1. session_state に saved 値があれば最優先 (vp-app 再起動の復元)
                            //   2. 上記 None かつ session 中の追加 (= 初回 fetch ではない) なら auto-expand
                            //   3. 初回 fetch の新規は閉じた状態
                            let mut s = RepoPaneState::new(p.path.clone(), p.name.clone());
                            s.expanded = session_state
                                .repo_expanded(&p.path)
                                .unwrap_or(!is_initial_load);
                            s
                        };
                        pane_state.state = Some(state_str);
                        pane_state.port = port;
                        pane_state
                    })
                    .collect();
                // Phase 1 (doc 24): currents_order を daemon の repo_order (= fetch 順) の
                // mirror にする。これで currents_order は独立 SSOT ではなく canonical の派生となり、
                // JS resolveRepoOrder は実質 passthrough（sidebar = daemon = ROTO = CLI で一致）。
                sidebar_state.currents_order =
                    Some(repo_ports.iter().map(|(path, _)| path.clone()).collect());
                // Model Q: 初回 load で active lane を daemon canonical から復元 (session.json でなく daemon が源)。
                if is_initial_load
                    && let Some(addr) = daemon_active_lane
                {
                    sidebar_state.active_lane_address = Some(addr.clone());
                    session_state.active_lane_address = Some(addr);
                }
                // wiremsg: 各 repo の repo の Unison channel を購読する (per-repo 1 本ずつ)。
                // - Stage 1: "lanes" channel → sidebar Lane ツリー
                // - Stage 2: "canvas" channel → main area の Board body
                // retained topic なので接続直後に現スナップショットが届き、以降変化のたび
                // push される。設計: creo-memories mem_1CbA198fsHJsoKpu2jDUCv。
                for (path, _port) in &repo_ports {
                    // L0 SP-portless: lanes / canvas とも Daemon :32000 の集約 channel から購読する
                    // (repo 直結を剥がす)。 どちらも daemon 側で per-repo に集約済
                    // (lanes=lane_registry / canvas=TopicRouter) なので repo port 不問 = repo が down
                    // (port=None) でも「前回の続き」を表示でき、 port None→Some race で購読が始まらない
                    // 旧 gating の穴も解消する。 repo 復帰時は register / canvas push で各 channel が更新。
                    if lanes_sub_active.insert(path.clone()) {
                        spawn_lanes_subscription(
                            &rt_handle,
                            async_action_proxy.clone(),
                            path.clone(),
                            daemon_conn.clone(),
                        );
                    }
                    if canvas_sub_active.insert(path.clone()) {
                        spawn_canvas_subscription(
                            &rt_handle,
                            async_action_proxy.clone(),
                            path.clone(),
                            daemon_conn.clone(),
                        );
                    }
                }
                // terminal S4: ensureLane / terminal session は repo port に依存しなくなった
                // (xterm transport は Daemon "canvas" channel)。 port None→Some race のための
                // retroactive ensureLane block は撤去 — lane の出現/消滅は LanesLoaded reconcile
                // が SSOT として扱う (= ensureLane + terminal session start/stop)。
                // Phase 2.x-b: dead-respawn fix — repo が "running" になった時点で
                // repo_spawn_triggered から path を外す。 これで次に dead に落ちた時、
                // user が re-expand すれば再度 spawn が trigger される。
                // 注意: spawn 進行中 (state=="spawning") は外さない、 一連の spawn cycle が完了
                // (= "running") した時のみ。 こうすれば spawn 中の重複 POST も防げる。
                for proc in &sidebar_state.processes {
                    if proc.state.as_deref() == Some("running")
                        && repo_spawn_triggered.remove(&proc.path)
                    {
                        tracing::debug!(
                            "sp_spawn_triggered cleared (running): {}",
                            proc.path
                        );
                    }
                }
                push_sidebar_state(&webview, &sidebar_state);
            }
            // Phase A4-3b: repo の Lane fetch 結果を sidebar_state に反映
            Event::UserEvent(AppEvent::LanesLoaded {
                repo_path,
                lanes,
                origin,
            }) => {
                // doc 44 D4: 開発起点を反映する。**`None` は上書きしない** — snapshot に
                // 起点が載っていなかっただけで「起点が無い」ではないので、前回値を保つ
                // （既定値に落とすと ⭐ が明滅する）。
                if let Some(origin) = origin {
                    sidebar_state
                        .origin_by_repo
                        .insert(repo_path.clone(), origin);
                }
                // ループする event なので log omit (= LanesLoaded push と pair で noise 源)。
                // Architecture v4: active_lane_address が未設定なら最初の Lane を auto-select。
                // 「初回起動 → Main Lane が main area に出る」UX を Lane SSOT で保つ。
                //
                // 例外: secondary instance (Cmd+N で spawn = `instance_index != 0`) の場合は
                // auto-select を skip。 元 vp-app が既に同 lane の terminal WS を持ってる事が多く、
                // 衝突して両方の console が壊れるため。 Secondary は user が手動 lane 選択する前提。
                let is_secondary = instance_index != 0;
                // session 復元優先: pending_session_active_lane が今回の lanes に含まれれば、
                // auto-select-first より先にそれを採用 (vp-app 再起動時に直前 active を維持)。
                let session_match: Option<String> = pending_session_active_lane
                    .as_ref()
                    .filter(|saved| {
                        lanes
                            .iter()
                            .any(|l| &l.address.key() == *saved)
                    })
                    .cloned();
                // F.8 B Convergent: auto-select は pid あり (= Active = Pane 起動済) な Lane のみ対象。
                //  Dead Lane (pid:null、 spawn 失敗) を選ぶと WS 確立先が無く「lane not found」 reconnect ループに陥る。
                //  Active Lane が 1 件も無ければ auto-select はスキップ (user 明示選択を待つ)。
                let first_active = lanes.iter().find(|l| l.pid.is_some());
                let auto_select = !is_secondary
                    && sidebar_state.active_lane_address.is_none()
                    && session_match.is_none()
                    && first_active.is_some();
                let first_addr = if let Some(saved) = session_match {
                    // session 復元: 1 度限り、 復元済 marker として pending を消費
                    pending_session_active_lane = None;
                    tracing::info!("session 復元: active_lane = {}", saved);
                    Some(saved)
                } else if auto_select {
                    first_active.map(|l| l.address.key())
                } else {
                    None
                };
                let path_key = repo_path.clone();
                // Phase 2.5: prev lanes との diff で「消えた Lane」 を判定 → removeLane 発行
                let removed_addrs: Vec<String> = sidebar_state
                    .lanes_by_repo
                    .get(&path_key)
                    .map(|prev| {
                        let new_set: std::collections::HashSet<String> = lanes
                            .iter()
                            .map(|l| l.address.key())
                            .collect();
                        prev.iter()
                            .map(|l| l.address.key())
                            .filter(|addr| !new_set.contains(addr))
                            .collect()
                    })
                    .unwrap_or_default();
                for addr in &removed_addrs {
                    tracing::info!("Lane removed (LanesLoaded diff): {}", addr);
                    push_main::remove_lane(&webview, addr);
                    // terminal S4: 消えた lane の terminal session を停止 (= map から remove で
                    // cmd_tx drop → canvas channel close → Daemon demand stop → repo pump stop)。
                    terminal_sessions.remove(addr);
                    // conversation session も対で停止（terminal_sessions と同寿命）。remove が無いと
                    // 削除済 lane の購読 task が demand を立てたまま永久残留する。
                    conversation_sessions.remove(addr);
                    // VP-147 PR-P2-3 Moody Blues fix #1: lane delete 検出時に lane_inboxes
                    // も即時 cleanup (= 5s polling tick 待たずに stale state 解消)。
                    sidebar_state.lane_inboxes.remove(addr);
                }
                // 供給 push 根治（session chip 凍結、2026-07-17）: この snapshot で active lane の
                // header 相当 field（engine_session_id 等）が変わったかを差し替え前に判定して
                // おく。従来は cache 更新のみで setActivePane を撃ち直さず、lane を選び直すまで
                // Conversation ヘッダが旧値で凍結した。LanesLoaded は高頻度 loop event なので、
                // 変化時のみ（下の push）に絞る。
                let active_header_refresh = sidebar_state
                    .active_lane_address
                    .as_deref()
                    .and_then(|addr| {
                        let prev = sidebar_state
                            .lanes_by_repo
                            .get(&path_key)?
                            .iter()
                            .find(|l| l.address.key() == addr)?;
                        let next = lanes.iter().find(|l| l.address.key() == addr)?;
                        Some(header_lane_fields_changed(prev, next))
                    })
                    .unwrap_or(false);
                sidebar_state.lanes_by_repo.insert(repo_path, lanes);
                // 購読フェーズを "ready" に (= snapshot を 1 度でも受けた)。 stalled から復帰した場合も
                // ここで解消。 absent(初期 loading) / stalled と区別して hintFor が lane 0本 を
                // 「📡 lane なし」 と正しく出せる (doc 30 §5-3)。
                sidebar_state
                    .lane_sub_state
                    .insert(path_key.clone(), "ready".to_string());
                // terminal S4: per-lane instance — repo port には依存しない (xterm transport は
                // Daemon "canvas" channel)。 live lane (pid あり) ごとに ensureLane (JS xterm 作成) +
                // terminal session start (Daemon 購読 → demand → repo pump)。 どちらも idempotent。
                if let Some(lanes_for_proj) = sidebar_state.lanes_by_repo.get(&path_key) {
                    for lane in lanes_for_proj {
                        // doc 50 §4.6 A6: gate は **term session が 1 つでもあるか**。
                        //
                        // ⚠️ 旧 gate は `pid.is_none() || console_mode == "gui"` だった。あれは
                        //    「term になれるのは root だけ」という制約下では正しかった（root が
                        //    chat なら lane に xterm は要らない）。A6 で非 root も term になれる
                        //    ので、**root が chat でも非 root の term** が居うる — lane ごと skip
                        //    すると、その term に xterm も購読も作られず「pane は並ぶが真っ黒」に
                        //    なる（2026-07-25 実機 dogfood で観測。pid も root slot の pid なので
                        //    root=chat では None に見え、二重に間違う）。
                        //    lane 単位の判断はやめ、registry の mode から導出する。
                        let terms = term_sessions_of(lane);
                        if terms.is_empty() {
                            continue;
                        }
                        // Running に戻った lane は respawn guard を解除 (再 Dead 化時に再 respawn 可能に)。
                        let addr_str = lane.address.key();
                        lane_respawn_triggered.remove(&addr_str);
                        // term session ごとに xterm を用意する（PtySlot 不在なら pump が張れない
                        // だけで graceful — Dead lane は別途 on-demand respawn が拾う）。
                        for (session, is_root) in terms {
                            push_main::ensure_lane(&webview, &addr_str, session, is_root);
                        }
                        // terminal session 未起動なら start (idempotent)。
                        terminal_sessions
                            .entry(addr_str.clone())
                            .or_insert_with(|| {
                                spawn_terminal_session(
                                    &rt_handle,
                                    async_action_proxy.clone(),
                                    daemon_conn.clone(),
                                    path_key.clone(),
                                    addr_str.clone(),
                                )
                            });
                    }
                }
                if let Some(addr) = first_addr {
                    tracing::info!("auto-select first lane: {}", addr);
                    activate_lane(
                        &addr,
                        &mut sidebar_state,
                        &mut session_state,
                        &webview,
                        &mut lane_respawn_triggered,
                        &rt_handle,
                        &respawn_proxy,
                    );
                } else {
                    push_sidebar_state(&webview, &sidebar_state);
                }
                // 供給 push 根治: active lane の header field が変わった snapshot でだけ
                // setActivePane を再発行（webview の LaneHeader ctx 層が新値に追従する）。
                if active_header_refresh {
                    push_active_view(&webview, &sidebar_state);
                }
                // conversation topic への attach（chat は → demand → transcript replay、TUI は
                // now-line のみ流れる軽い購読）。doc 58 ②-a で active 限定 → **全 lane** に拡大 —
                // 名簿は背景 lane の「今なにを」も見せるため。LanesLoaded は lane snapshot 到着の
                // たび走るので、新 lane / 起動直後の session 復元もここで確実に拾える（冪等）。
                let all_addrs: Vec<String> = sidebar_state
                    .lanes_by_repo
                    .values()
                    .flatten()
                    .map(|l| l.address.key().to_string())
                    .collect();
                for addr in all_addrs {
                    ensure_conversation_attach(
                        &addr,
                        &sidebar_state,
                        &mut conversation_sessions,
                        &rt_handle,
                        &async_action_proxy,
                        &daemon_conn,
                    );
                }
                // doc 53 §11: **roster の供給点はここ 1 本**（旧 `conversation_session_list` fetch は
                // 退役）。snapshot は server が動詞の末尾で push する（`emit_lane_update`）ので、
                // GUI 自身が起こした変化も CLI / MCP 由来の変化も同じ道で届く。
                //
                // 変化した lane だけ push する（LanesLoaded は定期 snapshot でも走る高頻度 event。
                // 毎回撃つと webview が roster を作り直して pane が無用に再配置される）。
                // 判定の規律は上の `active_header_refresh` と同型 = 「変化時のみ push」。
                if let Some(lanes_for_proj) = sidebar_state.lanes_by_repo.get(&path_key) {
                    for lane in lanes_for_proj {
                        let Some(sessions) = lane.sessions.as_ref() else {
                            continue;
                        };
                        let addr = lane.address.key();
                        let payload = session_list_payload(&addr, sessions);
                        if !roster_push_needed(&last_roster_push, &addr, &payload) {
                            continue;
                        }
                        remember_roster_push(&mut last_roster_push, &addr, &payload);
                        push_session_list(&webview, &addr, &payload);
                    }
                }
                // 消えた lane の指紋も落とす（同名再作成で「変化なし」と誤判定しないため）。
                for addr in &removed_addrs {
                    forget_roster_push(&mut last_roster_push, addr);
                }
            }
            // webview が「受け口を全部生やした」と名乗った（`entry.tsx` の `t:"ready"`）。
            //
            // ## これは catch-up ではなく **replay**
            //
            // bundle 評価前に Rust が撃った押し込みは、受け口 (`window.vpDispatch`) が居ないので
            // 届かない。ここで**現在の状態を丸ごと撃ち直す**ことでそれを埋める。全部 idempotent /
            // 全量置き換えなので、二重に撃っても壊れない（level 駆動）。
            //
            // ⚠️ 以前は同じことを **feature ごとの pull 3 本**（`lanes:ensure-all` /
            // `bastet:devices_fetch` / `board:demand`）でやっていた。面を足すたびに pull を 1 本
            // 足す形で、しかも**その面が install された後**に撃つ順序制約が JS 側に散っていた。
            // 「webview が生まれた」という事実は 1 つなので、signal も 1 本に畳んである。
            // 新しい面を足したら **ここに replay を 1 行足す**（新しい IPC tag は要らない）。
            Event::UserEvent(AppEvent::WebviewReady) => {
                // terminal S4: JS xterm instance の catch-up 再発行のみ (repo port 不要)。
                // terminal session 自体は LanesLoaded reconcile が管理するのでここでは触らない。
                for (_repo_path, lanes) in sidebar_state.lanes_by_repo.clone().iter() {
                    for lane in lanes {
                        // doc 50 §4.6 A6: gate は term session の有無（LanesLoaded と同じ規則 —
                        // lane 単位の pid / mode で切ると root=chat の lane の非 root term が
                        // 落ちる）。ensureLane は idempotent なので catch-up で撃ち直してよい。
                        let addr_str = lane.address.key();
                        for (session, is_root) in term_sessions_of(lane) {
                            push_main::ensure_lane(&webview, &addr_str, session, is_root);
                        }
                        // doc 53 §11: **roster も同じ窓で落ちる**（team-b 指摘 2026-07-25）。
                        //
                        // roster が push 型になった以上、`ensure_lane` / `push_active_view` /
                        // device 一覧と同じ boot race を持つ: bundle **評価前**の押し込みは
                        // 受け口（`window.vpDispatch`）が居ないので届かないのに、Rust 側は
                        // 「送った」として指紋を残す → その lane の roster が**実際に変わるまで
                        // 二度と push されない**（tab strip / pane grid / picker が空のまま）。
                        // 供給が fetch だった頃は「JS が能動的に取りに行く」ので原理的に
                        // 起きなかった窓 — 供給路を変えたことの随伴。
                        //
                        // ここは JS が ready を名乗った後なので、**指紋を無視して撃ち直す**
                        // （送った値は同じなので指紋の更新は不要）。
                        if let Some(sessions) = lane.sessions.as_ref() {
                            push_session_list(
                                &webview,
                                &addr_str,
                                &session_list_payload(&addr_str, sessions),
                            );
                        }
                        // **terminal の replay も同じ窓で落ちる**（2026-07-26 実測）。
                        //
                        // 上のコメントが列挙する「同じ boot race を持つもの」に terminal が
                        // 並んでいなかった。実測した時刻:
                        //   02:11:42.886  replay が client に到着（= evaluate_script が撃たれる）
                        //   02:11:43.284  bundle init complete（**0.4 秒後**）
                        // `window.vpTerminal` が未定義の間の `evaluate_script` は silent no-op で、
                        // terminal の replay は**一度きり**なので二度と来ない → console が黒いまま。
                        //
                        // ここは JS が ready を名乗った後なので、demand を撃ち直して replay を
                        // 取り直す（server 側は `terminal_demand_start` → `reconcile_lane` で
                        // 冪等 — 既に張られていれば pump は kept、replay だけが流れ直す）。
                        if !term_sessions_of(lane).is_empty()
                            && let Some(path) =
                                resolve_repo_path_for_lane(&sidebar_state, &addr_str)
                        {
                            let port = crate::daemon::default_daemon_port();
                            let lane_for_req = addr_str.clone();
                            rt_handle.spawn(async move {
                                if let Err(e) = daemon_repo_request(
                                    port,
                                    &path,
                                    "terminal_demand_start",
                                    // `replay: true` = 「画面を持っていないので流し直して」。
                                    // 準備前に届いた replay を捨てているので、server 側の
                                    // 「変化なし」判定を明示要求で越える（doc 53 §6.5.0）。
                                    serde_json::json!({ "lane": lane_for_req, "replay": true }),
                                )
                                .await
                                {
                                    tracing::debug!(
                                        "WebviewReady: terminal demand 再要求に失敗（次の契機で再試行）: {e}"
                                    );
                                }
                            });
                        }
                    }
                }
                // 現在 active な Lane を再度 show する (lane-empty placeholder を解除する保険)
                if let Some(addr) = sidebar_state.active_lane_address.clone() {
                    let is_chat = lane_is_chat(&sidebar_state, &addr);
                    push_main::show_lane(&webview, Some(&addr), is_chat);
                    // 起動 race で silent drop されるのは ensureLane だけではない。 auto-select の
                    // activate_lane が撃つ setActivePane も同じ窓で落ちるが、これが JS 側の
                    // 「active lane」を埋める唯一の経路 — showLane だけ再発行しても JS の active
                    // lane は null のままなので、lane 文脈を要する操作が「active lane 不明」で
                    // 早期 return する。冪等なので毎回再発行して JS 側 state を確定させる。
                    //
                    // doc 50 §4.6 A6: lane 単位 mode の catch-up は退役（lane 単位 mode が
                    // 消滅）。roster の catch-up は上の lane ループが撃つ（doc 53 §11 — push 型に
                    // なって以降、この経路にも roster が要る）。
                    push_active_view(&webview, &sidebar_state);
                }
                // 計器盤: daemon-device の接続時 snapshot は bundle ロード前に届いて落ちている
                // （sidebar の Devices badge は state 再 push で生きるが pane だけ空、2026-07-23
                // 実機で確認）。保持済み state から全量で撃ち直す。
                push_main::render_devices(&webview, &sidebar_state.devices);
                // shell (L|main|R) の形: 保存があれば復元する。無ければ撃たない
                // （webview の既定値が残る = 既定を 2 箇所に書かない）。
                // ⚠️ 撃った/撃たなかったを**両方**残す。「行が無い」は「保存が無かった」とも
                // 「ここに来ていない」とも読めてしまい、実機の切り分けで 1 往復損する
                // （2026-08-06 に実際に損した）。
                match session_state.shell_layout().cloned() {
                    Some(layout) => {
                        tracing::info!("shell layout 復元: {layout:?}");
                        push_main::shell_layout(&webview, &layout);
                    }
                    None => tracing::info!("shell layout 復元: 保存なし（既定のまま）"),
                }
                // 掲示板: retained BoardUpdated も同じ窓で落ちる（doc 52 §10 wave 0）。
                // 保持分を撃ち直す（落ちたままだと reopen で board pane が出ず、次の live show
                // まで空のまま）。
                //
                // ⚠️ **全 repo 分を撃つ**。active repo だけに絞っていた旧実装は、bundle 再評価後に
                // 別 repo へ切り替えると board が空のままだった（webview は `(repo, lane)` で
                // 箱を持つので、届いていない repo の箱は作られない）。message には repo が
                // stamp 済なので、まとめて配っても混ざらない。
                for boards in board_snapshots.values() {
                    for message in boards.values() {
                        push_main::board_message(&webview, message.clone());
                    }
                }
                // LanesLoaded のたびに follow up 発火する loop event のため log omit。
            }
            Event::UserEvent(AppEvent::LanesError {
                repo_path,
                message,
            }) => {
                tracing::warn!(
                    "AppEvent::LanesError: repo={} message={}",
                    repo_path,
                    message
                );
                // repo 接続失敗 / lanes channel stall — lanes_by_repo は更新しない (前回値を保持) が、
                // 購読フェーズを "stalled" に倒して UI に surface する (doc 30 §5-3)。 hintFor が
                // `📡 loading lanes…` ではなく「⚠️ lane 接続が停滞 — restart で復帰」を出す。 復帰時の
                // snapshot 受信 (LanesLoaded) で "ready" に上書きされて自動解消する (self-heal と連動)。
                sidebar_state
                    .lane_sub_state
                    .insert(repo_path, "stalled".to_string());
                push_sidebar_state(&webview, &sidebar_state);
            }
            // オンデマンド respawn の restart_lane が失敗した lane を guard から解除する。
            // 解除しておくと、 次に同 lane を active にした (or LanesLoaded for Dead の) 時点で
            // 再 respawn を試行できる (= repo クラッシュ後の復帰でも auto-respawn が効く)。
            // 即ループにはならない: クリック起点は user 操作、 起動時 first_addr は active 設定後
            // None になるため LanesLoaded loop event での連続発火は起きない。
            Event::UserEvent(AppEvent::LaneRespawnFailed { address }) => {
                if lane_respawn_triggered.remove(&address) {
                    tracing::info!("auto-respawn guard 解除 (restart 失敗): {}", address);
                }
            }
            Event::UserEvent(AppEvent::InkSnapshot { rect }) => {
                // ink（対話面, doc 52 §3）: board pane（#ink-stage）を WKWebView.takeSnapshot で
                // PNG 化する。保存先 dir は active lane の flat key で分ける（board と同じ空間）。
                // completion（main thread）は InkSnapshotReady で event loop に戻す（proxy.clone）。
                let lane_key = sidebar_state
                    .active_lane_address
                    .as_deref()
                    .map(crate::webview::ink_snapshot::lane_key_from_address)
                    .unwrap_or_else(|| "main".to_string());
                match crate::webview::ink_snapshot::snapshot_path(&lane_key) {
                    Ok(out_path) => {
                        let ready_proxy = proxy.clone();
                        crate::webview::ink_snapshot::take_snapshot(
                            &webview,
                            rect,
                            out_path,
                            move |path, error| {
                                let _ = ready_proxy
                                    .send_event(AppEvent::InkSnapshotReady { path, error });
                            },
                        );
                    }
                    Err(e) => {
                        let _ = proxy.send_event(AppEvent::InkSnapshotReady {
                            path: None,
                            error: Some(format!("snapshot 保存先の作成に失敗: {e}")),
                        });
                    }
                }
            }
            Event::UserEvent(AppEvent::InkSnapshotReady { path, error }) => {
                // ink: snapshot 完了/失敗を webview に返す（ink.ts が会話へ一行 + 画像を送る）。
                // 成功と失敗で受け手の振る舞いが別なので event も 2 本（schema 参照）。
                match path {
                    Some(p) => push_main::ink_snapshot(&webview, p),
                    None => push_main::ink_snapshot_error(&webview, error.unwrap_or_default()),
                }
            }
            Event::UserEvent(AppEvent::ShellLayout {
                sidebar_width,
                right_sidebar_width,
                sidebar_form,
                right_sidebar_open,
            }) => {
                // shell の形（幅 / full-slim / R 開閉）を **この instance の** session file に保存。
                // 「window をどう開いていたか」なので window_geometry と同じ箱に入れる。
                // ⚠️ 値の検証は `set_shell_layout` の clamp が持つ（webview の値を信用しない）。
                use crate::session_state::{ShellLayout, SidebarForm};
                session_state.set_shell_layout(ShellLayout {
                    sidebar_width,
                    right_sidebar_width,
                    // 未知の形は full に倒す（版ズレで「開けない sidebar」を作らない）
                    sidebar_form: if sidebar_form == "slim" {
                        SidebarForm::Slim
                    } else {
                        SidebarForm::Full
                    },
                    right_sidebar_open,
                });
                session_state.save();
            }
            Event::UserEvent(AppEvent::DebugLogWatch { source }) => {
                // R sidebar の debug log（sidebar view modes）: 世代を進めて旧 tail を退場させ、
                // 新しい tail thread を起こす（最後の watch が勝つ = 単一 tail）。
                use std::sync::atomic::Ordering;
                let generation = debuglog_watch_gen.fetch_add(1, Ordering::Relaxed) + 1;
                match crate::debug_log::log_path(&source) {
                    Some(path) => {
                        crate::debug_log::spawn_tail(
                            source,
                            path,
                            generation,
                            debuglog_watch_gen.clone(),
                            proxy.clone(),
                        );
                    }
                    None => tracing::warn!("debuglog:watch の未知 source: {source}"),
                }
            }
            Event::UserEvent(AppEvent::DebugLogUnwatch) => {
                // 世代を進めるだけで tail は次の poll で止まる（見ていない log は読まない）。
                use std::sync::atomic::Ordering;
                debuglog_watch_gen.fetch_add(1, Ordering::Relaxed);
            }
            Event::UserEvent(AppEvent::DebugLogChunk {
                source,
                reset,
                lines,
                generation,
            }) => {
                // tail thread からの行群を R sidebar へ。退場直前の旧世代 thread が送った
                // 残 chunk はここで棄てる（新 backlog の後に旧行が 1 回混ざる race の封じ）。
                // stream なので replay は持たない（次の watch が毎回 backlog から始まる）。
                use std::sync::atomic::Ordering;
                if generation == debuglog_watch_gen.load(Ordering::Relaxed) {
                    push_main::debuglog_lines(&webview, &source, reset, lines);
                }
            }
            Event::UserEvent(AppEvent::DeviceEvent { payload }) => {
                tracing::debug!("🧲 device event: {}", payload);
                // Phase 2: device 一覧を registry 更新 → sidebar (Devices badge) + main area
                // (DeviceRegistry pane の device list) の両方に push。
                if crate::pane::apply_device_event(&mut sidebar_state.devices, &payload) {
                    push_sidebar_state(&webview, &sidebar_state);
                    push_main::render_devices(&webview, &sidebar_state.devices);
                }
                // fleet 配線 (doc 49 LE-19): 操作入力 (control_event) は webview の mapping
                // registry へ fire-and-forget 転送。受け手 (window.vpFleet) は gallery-panes.tsx。
                if let Some(js) = fleet_dispatch_js(&payload)
                    && let Err(e) = webview.evaluate_script(&js)
                {
                    tracing::warn!("fleet dispatch: evaluate_script 失敗: {}", e);
                }
            }
            Event::UserEvent(AppEvent::EditorEval { js, resp }) => {
                // doc 48 Phase 2: editor bridge の webview 評価。結果 (wry が JSON 文字列化
                // した評価値) を canvas session 側へ返す。受信側は timeout で打ち切るので
                // callback が遅れて発火しても送信は無害 (受け手 drop 済なら send Err → 無視)。
                if let Err(e) = webview.evaluate_script_with_callback(&js, move |result| {
                    let _ = resp.send(result);
                }) {
                    tracing::warn!("editor bridge: evaluate_script 失敗: {}", e);
                }
            }
            Event::UserEvent(AppEvent::CanvasMessage {
                repo_path,
                message,
            }) => {
                // wiremsg Stage 2: repo の "canvas" channel から受信した RepoMessage。
                // active repo の分のみ main area の Board body に転送する
                // （**board_updated だけは例外** — 下記）。
                // active 判定: active_lane_address の repo segment == repo_path の basename。
                let active_repo = sidebar_state
                    .active_lane_address
                    .as_deref()
                    .and_then(|addr| addr.split('/').next());
                let msg_repo = std::path::Path::new(&repo_path)
                    .file_name()
                    .and_then(|s| s.to_str());
                // ⚠️ **board_updated には送信元 repo を stamp する**（repo 側の BoardUpdated は
                // 持っていない）。board の同一性は `(repo, lane)` の対で、全 repo の root lane が
                // 同じ `'main'` を名乗る。repo を落として webview に渡していた旧実装は
                // 13 repo が 1 つの箱を奪い合い、「board 行を持たない repo に切り替えると前の
                // repo の board が出たまま」になっていた（2026-08-04 根治）。
                let mut message = message;
                let is_board_update = message.get("type").and_then(|t| t.as_str())
                    == Some("board_updated")
                    && message.get("scope").and_then(|s| s.as_str()) == Some("lane");
                if is_board_update
                    && let Some(proj) = msg_repo
                    && let Some(obj) = message.as_object_mut()
                {
                    obj.insert(
                        "repo".to_string(),
                        serde_json::Value::String(proj.to_string()),
                    );
                }
                // board pane の boot 窓救済（doc 52 §10 wave 0）: BoardUpdated を repo × lane で
                // 保持する。`AppEvent::WebviewReady` の replay で再配信し、retained が bundle 評価前に
                // 落ちた分を埋める。lane 欠落 = main（board-handler の flat key と一致）。
                //
                // ⚠️ scope=="lane" のみ buffer する（消費側 board-handler.ts `applyBoardUpdated` の
                //   `if (msg.scope !== 'lane') return` と対称にする）。退役済み scope="proj" の孤児行も
                //   seed_boards が無条件 broadcast し、board_key() で proj も main lane も
                //   broadcast_lane=None → lane_key="main" に衝突する。scope guard が無いと、行順
                //   次第で proj 孤児が本物の lane board を上書きし、replay が「JS が捨てる死んだ
                //   message」を配って boot 窓 regression が再発する（team-b review 2026-07-24）。
                if is_board_update
                    && let Some(proj) = msg_repo
                {
                    let lane_key = message
                        .get("lane")
                        .and_then(|l| l.as_str())
                        .unwrap_or("main")
                        .to_string();
                    board_snapshots
                        .entry(proj.to_string())
                        .or_default()
                        .insert(lane_key, message.clone());
                }
                // B1 + cross-project: switch_lane は board content ではなく active Lane 切替コマンド。
                // active を「変える」コマンドなので、active repo guard の **外**で処理する
                // （別 repo の repo から来た switch_lane こそ通す）。送信元 repo の repo
                // (= msg_repo) の lane を activate し、sidebar / main area を追随させる。
                if message.get("type").and_then(|t| t.as_str()) == Some("switch_lane") {
                    if let (Some(repo), Some(token)) = (
                        msg_repo,
                        message.get("lane").and_then(|l| l.as_str()),
                    ) {
                        // token → lane address（形式は `address_from_lane_token` の 1 箇所）
                        let address = crate::lane_address::address_from_lane_token(repo, token);
                        // Model B (focus = 操舵ポインタ): switch_lane は全 instance に broadcast される
                        // が、適用するのは **focused instance だけ**。非 focus の window はこの event を
                        // 無視し、自分の lane に park されたまま (= 2 window が別々の lane を同時に見られる)。
                        if is_focused {
                            activate_lane(
                                &address,
                                &mut sidebar_state,
                                &mut session_state,
                                &webview,
                                &mut lane_respawn_triggered,
                                &rt_handle,
                                &respawn_proxy,
                            );
                            // gui: chat lane なら conversation topic に attach（→ transcript replay）。
                            ensure_conversation_attach(
                                &address,
                                &sidebar_state,
                                &mut conversation_sessions,
                                &rt_handle,
                                &async_action_proxy,
                                &daemon_conn,
                            );
                        } else {
                            tracing::debug!(
                                "switch_lane skip (not focused): address={}",
                                address
                            );
                        }
                    }
                } else if is_board_update || (active_repo.is_some() && active_repo == msg_repo) {
                    // ⚠️ **board_updated は active repo でなくても流す**。webview が `(repo, lane)` で
                    // 箱を分けるようになったので、全 repo 分を持たせておけば repo 切替が
                    // 「キーを差し替えるだけ」で済む（撃ち直しの経路が要らない）。
                    // active repo に絞っていた旧実装は、切替先の board を **一度も届けない**まま
                    // 前の repo の箱を見せていた（bug の後半）。
                    // board 以外（switch_lane を除く content）は従来どおり active repo のみ。
                    match serde_json::to_value(&message) {
                        Ok(json) => push_main::board_message(&webview, json),
                        Err(e) => {
                            tracing::warn!("CanvasMessage serialize 失敗: {}", e);
                        }
                    }
                }

                // board 着信 badge: show が現在 active でない lane に着いたら sidebar に
                // canvas_unread を計上する。別 repo / 別 lane（同 repo だが別 lane）の両ケースを
                // 1 箇所で拾う（上の forward guard とは独立）。active lane 宛の show は board pane 側
                // （board-handler.ts の presence → lane-panes、doc 52 §10 wave 0）で解決する。
                if message.get("type").and_then(|t| t.as_str()) == Some("show")
                    && let Some(repo) = msg_repo
                {
                    let token = message
                        .get("lane")
                        .and_then(|l| l.as_str())
                        .unwrap_or(crate::lane_address::ROOT_LANE_NAME);
                    // token → lane address（switch_lane と同じ helper を通す）。
                    let address = crate::lane_address::address_from_lane_token(repo, token);
                    if sidebar_state.active_lane_address.as_deref() != Some(address.as_str()) {
                        mark_lane_canvas_unread(&address, &mut sidebar_state, &webview);
                    }
                }
            }
            // terminal S4 (doc 27 §4.1): per-lane terminal session 由来の PTY 出力を当該 lane の
            // xterm に inject する。 data は base64 (JS 側で decode → term.write)。
            Event::UserEvent(AppEvent::TerminalOutput {
                lane,
                session,
                data,
            }) => {
                // doc 50 §4.6 A6: 同 lane の複数 xterm に振り分けるため session を第 2 引数で渡す
                // （push envelope `console:event` と同じ形）。
                let script = format!(
                    "window.vpTerminal && window.vpTerminal.handleOutput({}, {}, {})",
                    serde_json::to_string(&lane).unwrap_or_else(|_| "\"\"".into()),
                    session,
                    serde_json::to_string(&data).unwrap_or_else(|_| "\"\"".into()),
                );
                if let Err(e) = webview.evaluate_script(&script) {
                    tracing::warn!("vpTerminal.handleOutput 失敗 (lane={}): {}", lane, e);
                }
            }
            // terminal S4: xterm onData → 当該 lane の terminal session に渡す (上り request)。
            Event::UserEvent(AppEvent::TerminalWrite {
                lane,
                session,
                data,
            }) => {
                vp_paths::term_trace("A:app-dispatch(b64)", &lane, data.as_bytes());
                if let Some(term) = terminal_sessions.get(&lane) {
                    let _ = term.cmd_tx.send(TermCmd::Write(session, data));
                }
            }
            // terminal S4: xterm resize → 当該 lane の terminal session に渡す (上り request)。
            Event::UserEvent(AppEvent::TerminalResize {
                lane,
                session,
                cols,
                rows,
            }) => {
                if let Some(term) = terminal_sessions.get(&lane) {
                    let _ = term.cmd_tx.send(TermCmd::Resize(session, cols, rows));
                }
            }
            // Conversation gui (doc 32): repo から受信した構造化イベントを当該 lane の Console pane に渡す。
            Event::UserEvent(AppEvent::ConversationEvent {
                lane,
                event,
                session,
            }) => {
                // doc 38 Phase 2: 第 3 引数 session（VP 採番 key）を渡す。console.ts が focused
                // 判定に使い、chatview が背景 session の stream を焦点会話へ混ぜないよう filter する。
                push_main::console_event(&webview, &lane, event.clone(), session);
                // 路 A（memory echoes-act2-notification-signal）: gui の完了/エラーを tui の
                // OSC 通知と同じ sink に流す。headless stream-json は Notification hook を発火しない
                // ため、turn_completed（stream `result` 由来）が「Claude が返し終えた＝入力待ち」の
                // 唯一のシグナル。question（PR1）/ permission_request（PR3 tool 承認 + PR4 plan の
                // ExitPlanMode）は engine が turn を pause して人の判断を待つ明示的 HITL なので、同じ
                // awaiting_input 機構で conn-hitl（magenta diamond）を点灯する（doc 35 §4 の契約）。
                // active lane は helper が即読 skip し、切替（activate_lane）で reset される。
                if let Some(kind) = event.get("kind").and_then(|k| k.as_str())
                    && (kind == "turn_completed"
                        || kind == "error"
                        || kind == "question"
                        || kind == "permission_request")
                {
                    mark_lane_awaiting_input(
                        &lane,
                        &format!("gui:{kind}"),
                        &mut sidebar_state,
                        &webview,
                    );
                }
            }
            // Conversation gui: ChatPane の submit → 当該 lane の conversation session に渡す。
            // demand-driven: 未起動なら lazy spawn (subscribe → submit の順で取りこぼしなし)。
            Event::UserEvent(AppEvent::ConversationSubmit { lane, prompt, session: chat_session, images, request_id }) => {
                let session = conversation_sessions.entry(lane.clone()).or_insert_with(|| {
                    // repo_path は active repo から解決 (conversation pane = active lane 前提)。
                    let repo_path =
                        resolve_active_repo_path(&sidebar_state).unwrap_or_default();
                    spawn_conversation_session(
                        &rt_handle,
                        async_action_proxy.clone(),
                        daemon_conn.clone(),
                        repo_path,
                        lane.clone(),
                    )
                });
                let (reply, result) = tokio::sync::oneshot::channel();
                let proxy = async_action_proxy.clone();
                rt_handle.spawn(async move {
                    let event = crate::conversation_submission::await_submit_result(&request_id, result).await;
                    let _ = proxy.send_event(AppEvent::ConversationEvent {
                        lane,
                        session: chat_session.unwrap_or(1),
                        event,
                    });
                });
                let _ = session
                    .cmd_tx
                    .send(ConversationCmd::Submit { prompt, session: chat_session, images, reply });
            }
            // Conversation gui HITL (doc 35 PR1): PromptCard の回答 → 当該 lane の conversation session へ。
            // 質問は submit 済み engine 由来なので session は既存のはずだが、防御的に lazy spawn。
            Event::UserEvent(AppEvent::ConversationRespond {
                lane,
                request_id,
                answers,
                behavior,
                message,
                session: chat_session,
            }) => {
                let session = conversation_sessions.entry(lane.clone()).or_insert_with(|| {
                    let repo_path =
                        resolve_active_repo_path(&sidebar_state).unwrap_or_default();
                    spawn_conversation_session(
                        &rt_handle,
                        async_action_proxy.clone(),
                        daemon_conn.clone(),
                        repo_path,
                        lane.clone(),
                    )
                });
                let _ = session.cmd_tx.send(ConversationCmd::Respond {
                    request_id,
                    answers,
                    behavior,
                    message,
                    session: chat_session,
                });
            }
            // doc 35 §5 / PR2: 実行中 turn の中断を当該 lane の conversation session に渡す。
            // interrupt は走行中 turn 前提なので session が居るはず（lazy spawn しない）。
            Event::UserEvent(AppEvent::ConversationInterrupt { lane, session: chat_session }) => {
                if let Some(session) = conversation_sessions.get(&lane) {
                    let _ = session
                        .cmd_tx
                        .send(ConversationCmd::Interrupt { session: chat_session });
                } else {
                    tracing::warn!("conversation:interrupt skip — session 未起動 (lane={lane})");
                }
            }
            // doc 35 §2.5 / PR3: permission mode 切替を当該 lane の conversation session に渡す。
            Event::UserEvent(AppEvent::ConversationSetPermissionMode {
                lane,
                mode,
                session: chat_session,
            }) => {
                if let Some(session) = conversation_sessions.get(&lane) {
                    let _ = session
                        .cmd_tx
                        .send(ConversationCmd::SetPermissionMode { mode, session: chat_session });
                } else {
                    tracing::warn!("conversation:set_permission_mode skip — session 未起動 (lane={lane})");
                }
            }
            // doc 50 §4.6 A6: 名札 kind badge からの Mode 切替（session 明示）。repo の
            // `session_set_mode` に forward し、成功したら SessionModeApplied で表示を追従させる。
            Event::UserEvent(AppEvent::SessionSetMode { lane, session, mode }) => {
                // repo は対象 lane 自身から逆引き（#705 のレース教訓 — repo 応答待ちの間に
                // active lane が変わり得るため resolve_active_repo_path は使わない）。
                let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!("session:set_mode skip — lane の repo 解決失敗 (lane={lane})");
                    return;
                };
                let proxy = async_action_proxy.clone();
                let (lane_for_js, mode_for_js) = (lane.clone(), mode.clone());
                rt_handle.spawn(async move {
                    match daemon_repo_request(
                        crate::daemon::default_daemon_port(),
                        &path,
                        "session_set_mode",
                        serde_json::json!({ "lane": lane, "session": session, "mode": mode }),
                    )
                    .await
                    {
                        Ok(_) => {
                            tracing::info!(
                                "session_set_mode ok: lane={lane_for_js} session={session} mode={mode_for_js}"
                            );
                            let _ = proxy.send_event(AppEvent::SessionModeApplied {
                                lane: lane_for_js,
                                session,
                                mode: mode_for_js,
                            });
                        }
                        Err(e) => tracing::warn!(
                            "session_set_mode 失敗 (lane={lane_for_js} session={session}): {e}"
                        ),
                    }
                });
            }
            // doc 50 §4.6 A6: session_set_mode 成功後、WebView に mode を反映する。
            //
            // **replay はここでは撃たない**（S2 と対）— World B が新しい kind の pane を mount し、
            // その pane が購読を張ってから demand を撃つ（購読前 replay は非 retained topic で
            // 落ちる順序 race）。ここは「mode が変わった」事実を JS に渡すだけに徹する。
            Event::UserEvent(AppEvent::SessionModeApplied { lane, session, mode }) => {
                let is_tui = mode == "tui";
                let is_root = root_session_of(&sidebar_state, &lane) == session;
                // 手元 snapshot（registry の投影）を即時更新する。lanes snapshot の反映は 5s
                // periodic 頼みで stale が残るため（ConsoleModeApplied と同じ理由）、mode を
                // 読む後続（term_sessions_of / attach gate / activate_lane）が旧値を見ないようにする。
                // doc 53 R1: 更新は sessions の 1 箇所だけ — 読み手（lane_is_chat / respawn
                // gate / header 差分）は sessions から root mode を導出するので、この 1 書きで
                // 全読み手に届く（旧「root なら lane 単位 mode 投影も更新」は退役）。
                for lanes in sidebar_state.lanes_by_repo.values_mut() {
                    if let Some(l) = lanes.iter_mut().find(|l| l.address.key() == lane)
                        && let Some(reg) = l.sessions.as_mut()
                        && let Some(e) = reg.sessions.iter_mut().find(|s| s.key == session)
                    {
                        e.mode = mode.clone();
                    }
                }
                push_sidebar_state(&webview, &sidebar_state);
                // xterm の起立 / 撤去（World A は instance 管理に徹し、顔ぶれの決定は上位が持つ）。
                if is_tui {
                    push_main::ensure_lane(&webview, &lane, session, is_root);
                    // 購読が無いと新 PtySlot の出力が届かない（terminal topic は非 retained）。
                    // demand 0→1 が repo の pump 張り直し + replay を撃つ。idempotent。
                    match resolve_repo_path_for_lane(&sidebar_state, &lane) {
                        Some(path) => {
                            terminal_sessions.entry(lane.clone()).or_insert_with(|| {
                                spawn_terminal_session(
                                    &rt_handle,
                                    async_action_proxy.clone(),
                                    daemon_conn.clone(),
                                    path,
                                    lane.clone(),
                                )
                            });
                        }
                        None => tracing::warn!(
                            "session:mode_applied — lane の repo 解決失敗、terminal session を張れず (lane={lane})"
                        ),
                    }
                    // ⚠️ **xterm の container を active 化しないと見えない**（`.lane-pane` は
                    // display:none が既定で、`.active` が付いて初めて描かれる）。chat から戻って
                    // 新しく作った instance は非 active のままなので、これが無いと「名札は出るのに
                    // 中身が真っ黒」になる（2026-07-25 実機で踏んだ — 旧 ConsoleModeApplied が
                    // 持っていた 1 行を S6 の撤去時に移植し忘れていた）。
                    // showLane は active 化に加えて rAF 2 段で fit / sendResize / focus まで行う。
                    // 順序: ensure_lane より後（instance が無いと active 化できない）。
                    //
                    // repo 応答待ちの間に別 lane へ移っていたら表示は奪わない（mode は手元 snapshot に
                    // 反映済みなので、戻った時に正しい顔ぶれで開く）。
                    if sidebar_state.active_lane_address.as_deref() == Some(lane.as_str()) {
                        push_main::show_lane(&webview, Some(&lane), false);
                    }
                } else {
                    // →chat: その session の xterm を畳む（PtySlot は repo 側で drop 済）。
                    push_main::remove_lane_session(&webview, &lane, session);
                    // tui→II の対称: conversation topic への購読を確保する（初回 chat 化で張られる）。
                    // 上の手元 snapshot 反映が先に要る（attach の gate が mode を読む）。
                    ensure_conversation_attach(
                        &lane,
                        &sidebar_state,
                        &mut conversation_sessions,
                        &rt_handle,
                        &async_action_proxy,
                        &daemon_conn,
                    );
                    // **Reborn ⊃ replay の実体**（doc 50 §4.6 ① / §4.7 逸脱②）: 切替のたび
                    // transcript を読み直す。
                    //
                    // ⚠️ `ensure_conversation_attach` に任せてはいけない — あれは購読ハンドル
                    // （`conversation_sessions`、**lane 単位**）が既にあれば no-op で、購読は lane 削除まで
                    // 残る。つまり chat→tui→chat の 2 回目以降は attach が発火せず、**tui で
                    // 進めた分が chat に出ない**（A6 が根治すると宣言した当の症状が別の理由で再現する。
                    // team-b review 2026-07-25 の指摘で発覚）。購読を落として張り直す案は採らない —
                    // 購読は lane 単位で**他の chat session の live stream も運んでいる**ため、
                    // 落とすと巻き添えになる。gate を経由しない明示 demand が正しい形。
                    //
                    // demand は session を明示する（replay は session 単位 — `conversation_demand_start`
                    // の None は focused に解決されるので、非 focused な pane を切り替えた時に
                    // 別会話を読んでしまう）。
                    if let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &lane) {
                        let proxy = async_action_proxy.clone();
                        let lane_for_log = lane.clone();
                        rt_handle.spawn(async move {
                            if let Err(e) = daemon_repo_request(
                                crate::daemon::default_daemon_port(),
                                &path,
                                "conversation_demand_start",
                                serde_json::json!({ "lane": lane_for_log, "session": session }),
                            )
                            .await
                            {
                                tracing::warn!(
                                    "conversation_demand_start（mode 切替後）失敗 (session={session}): {e}"
                                );
                            }
                            let _ = &proxy; // 応答は topic 経由で届く（ここでは event を投げない）
                        });
                    }
                }
                push_main::console_mode_applied(&webview, &lane, session, &mode);
            }
            // 新セッション開始（✨ New ボタン）。doc 39 §4「New は今いる Mode に出す」で分岐する:
            //  - chat lane（gui）: 「新 Draft session を作って focus」。旧会話はタブに残る
            //    （タブモデルの自然形 = 前回状態キープの延長）。
            //  - tui lane（tui）: lane_slot_new = slot を**足す**だけ（root 不変、A6 ③）。
            //    ⚠️ 旧実装の conversation_session_new_root（root 張り替え）は A6 で撤去 — root を
            //    動かすのは chip picker（console:switch_root）と「New Root Conversation」
            //    （sidebar context menu、doc 39 §8.4）の明示操作のみ。
            Event::UserEvent(AppEvent::ConsoleNewSession { lane, engine, mode }) => {
                // repo は対象 lane 自身から逆引き（#705 のレース教訓 — repo 応答待ちの間に
                // active lane が変わり得るため resolve_active_repo_path は使わない）。
                let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!("console:new_session skip — lane の repo 解決失敗 (lane={lane})");
                    return;
                };
                let port = crate::daemon::default_daemon_port();
                // doc 46 P2 要件 4: Mode は**明示指定を優先**し、無ければ lane の現 Mode を継ぐ。
                // 未知の値（typo 等）は継承に倒す — 「指定したのに黙って別の Mode で作られた」より
                // 「指定が効かなかった」方が気付きやすい。
                let want_chat = match mode.as_deref() {
                    Some("gui") => true,
                    Some("tui") => false,
                    _ => lane_is_chat(&sidebar_state, &lane),
                };
                if want_chat {
                    // doc 38 §4.2: chat lane は「新 Draft session を作って focus」。
                    rt_handle.spawn(async move {
                        // 1. engine を決める。doc 46 P2 要件 4 の**明示指定があればそれを使い**、
                        //    無い時だけ現 focused を継ぐ（従来挙動）。指定がある場合は
                        //    session_list の往復ごと省ける。
                        let agent = match engine {
                            Some(e) => Some(e),
                            None => match daemon_repo_request(
                                port,
                                &path,
                                "conversation_session_list",
                                serde_json::json!({ "lane": &lane }),
                            )
                            .await
                            {
                                Ok(payload) => focused_session_agent(&payload),
                                Err(e) => {
                                    tracing::warn!(
                                        "conversation_session_list（new_session 前）失敗 (lane={lane}): {e}"
                                    );
                                    None
                                }
                            },
                        };
                        // 2. 新 Draft session を作って focus（focus は明示 true）。
                        let mut create = serde_json::json!({ "lane": &lane, "focus": true });
                        if let Some(s) = &agent {
                            create["agent"] = serde_json::Value::String(s.clone());
                        }
                        if let Err(e) =
                            daemon_repo_request(port, &path, "conversation_session_create", create).await
                        {
                            tracing::warn!("console:new_session（chat）session_create 失敗 (lane={lane}): {e}");
                            return;
                        }
                        tracing::info!("console:new_session ok（chat, new draft）: lane={lane}");
                        // 3. roster（tab strip / focusedOf）の更新は **snapshot が運ぶ**
                        //    （doc 53 §11。server の `emit_lane_update` → LanesLoaded）。
                        //
                        //    ⚠️ 旧実装はここで一覧を取り直し「demand_start より先に送る」順序を
                        //    守っていた。その理由（session filter が旧 focused のまま replay_start を
                        //    落とす）は **A6 で消えている** — event は focused で捨てず session ごとの
                        //    store に振り分けるようになった（doc 50 §4.3 #2 / `foldEvent`）。
                        //    roster が数十 ms 遅れて届いても、表示先が切り替わるのが僅かに遅れるだけで
                        //    event は落ちない。
                        // 4. demand_start で新 focused（Draft）の replay を発火。no_session path でも
                        //    ReplayStart/End が届いて会話がクリアされる（doc 38 §4.2）。
                        if let Err(e) = daemon_repo_request(
                            port,
                            &path,
                            "conversation_demand_start",
                            serde_json::json!({ "lane": &lane }),
                        )
                        .await
                        {
                            tracing::warn!("conversation_demand_start（new_session 後）失敗 (lane={lane}): {e}");
                        }
                    });
                } else {
                    // tui（tui）: doc 50 §4.6 A6 ③ — chat 分岐と**対称**に「新 session を作って
                    // 台に並べる」だけ。新 term pane が tiling に入場し、既存 pane は無傷。
                    //
                    // ⚠️ 旧実装は `conversation_session_new_root`（新 session + **root 張り替え** + slot
                    // bare respawn）だった。あれは「xterm が lane に 1 枚」制約下では正しい適応
                    // （新しい console を見せる唯一の方法が root の付け替えだった）が、A6 で制約が
                    // 外れた今は「勝手に root を動かす副作用」に意味が反転する。root の付け替えは
                    // `console:switch_root`（root picker）の明示操作に一本化した。
                    rt_handle.spawn(async move {
                        // engine の明示指定は backend まで通す（無ければ lane の agent を継ぐ）。
                        let mut payload = serde_json::json!({ "lane": &lane });
                        if let Some(e) = &engine {
                            payload["agent"] = serde_json::Value::String(e.clone());
                        }
                        match daemon_repo_request(port, &path, "lane_slot_new", payload).await {
                            Ok(res) => {
                                let session = res.get("session").and_then(serde_json::Value::as_u64);
                                tracing::info!(
                                    "console:new_session ok（tui, new slot）: lane={lane} session={session:?}"
                                );
                                // roster（新 term pane が生える元）の更新は snapshot が運ぶ
                                // （doc 53 §11）。root は動いていないので会話 clear も送らない。
                            }
                            Err(e) => tracing::warn!("console:new_session 失敗 (lane={lane}): {e}"),
                        }
                    });
                }
            }
            // doc 39 P3: Root 切替 picker — 既存 session へ root を向け替え（slot = Resume respawn）。
            // 後続は new_root（ConsoleSessionRenewed = clear）と違い conversation_demand_start:
            // 対象 session には既存の会話があるため、clear でなく transcript replay で追従させる
            //（conversation_session_focus chain と同じ規律）。
            Event::UserEvent(AppEvent::ConsoleSwitchRoot { lane, session }) => {
                let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!(
                        "console:switch_root skip — lane の repo 解決失敗 (lane={lane})"
                    );
                    return;
                };
                let port = crate::daemon::default_daemon_port();
                rt_handle.spawn(async move {
                    let payload = serde_json::json!({ "lane": &lane, "session": session });
                    match daemon_repo_request(port, &path, "conversation_session_switch_root", payload)
                        .await
                    {
                        Ok(_) => {
                            tracing::info!(
                                "console:switch_root ok: lane={lane} session={session}"
                            );
                            // roster（tab strip / picker）の更新は snapshot が運ぶ（doc 53 §11）。
                            // 旧実装が守っていた「replay より先に一覧」の順序は A6 で不要に
                            // なっている（event は session ごとの store に振り分けられる —
                            // `ConsoleNewSession` の chat 分岐のコメント参照）。
                            if let Err(e) = daemon_repo_request(
                                port,
                                &path,
                                "conversation_demand_start",
                                serde_json::json!({ "lane": &lane }),
                            )
                            .await
                            {
                                tracing::warn!(
                                    "conversation_demand_start（switch_root 後）失敗 (lane={lane}): {e}"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!("console:switch_root 失敗 (lane={lane} session={session}): {e}")
                        }
                    }
                });
            }
            // gui モデル切替: conversation_set_model で repo に forward（fire & forget、
            // session 単位）。適用の視覚確認は新 engine の session_init が header.model を
            // 更新することで得る。
            Event::UserEvent(AppEvent::ConversationSetModel {
                lane,
                session,
                model,
            }) => {
                let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!(
                        "conversation:set_model skip — lane の repo 解決失敗 (lane={lane})"
                    );
                    return;
                };
                rt_handle.spawn(async move {
                    let payload =
                        serde_json::json!({ "lane": &lane, "session": session, "model": model });
                    match daemon_repo_request(
                        crate::daemon::default_daemon_port(),
                        &path,
                        "conversation_set_model",
                        payload,
                    )
                    .await
                    {
                        Ok(_) => tracing::info!(
                            "conversation:set_model ok: lane={lane} session={session}"
                        ),
                        Err(e) => tracing::warn!(
                            "conversation:set_model 失敗 (lane={lane} session={session}): {e}"
                        ),
                    }
                });
            }
            // doc 53 §11: 旧 `ConversationSessionsFetch`（webview → ask `conversation_session_list`）は退役。
            // roster の供給は lanes snapshot 1 本（LanesLoaded の push）— fetch は GUI 自身の
            // 動詞でしか撃たれず、CLI / MCP 由来の session 変化が pane に出なかった。
            //
            // doc 38 Phase 2: 「+」からの新 session 作成。focus は送らない = backend 既定 true。
            // 作成後に一覧を取り直して tab strip に新 session を即反映（1 task で直列）。
            Event::UserEvent(AppEvent::ConversationSessionCreate { lane, agent }) => {
                let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!("conversation:session_create skip — lane の repo 解決失敗 (lane={lane})");
                    return;
                };
                rt_handle.spawn(async move {
                    let mut create = serde_json::json!({ "lane": &lane });
                    if let Some(s) = &agent {
                        create["agent"] = serde_json::Value::String(s.clone());
                    }
                    // doc 53 §11: 動詞を撃つだけ。roster の更新は server の `emit_lane_update`
                    // → lanes snapshot → LanesLoaded で届く（旧: ここで一覧を取り直していた）。
                    if let Err(e) = daemon_repo_request(
                        crate::daemon::default_daemon_port(),
                        &path,
                        "conversation_session_create",
                        create,
                    )
                    .await
                    {
                        tracing::warn!("conversation_session_create 失敗 (lane={lane}): {e}");
                    }
                });
            }
            // doc 38 Phase 2: session tab click による focused 切替。focus → 一覧再取得 →
            // demand_start（新 focused の transcript replay 発火）の順で直列に。
            Event::UserEvent(AppEvent::ConversationDemandStart { lane }) => {
                // 消費者主導の replay demand（2026-07-24）: webview が renderer を張った直後に
                // 届く。attach 時 demand（run_conversation_session）の boot 窓取りこぼしを埋める第 2 弾
                //（冪等 — ensure_chat_engine は既起動 no-op / replay は clear-prefix で収束）。
                let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!("conversation:demand_start skip — lane の repo 解決失敗 (lane={lane})");
                    return;
                };
                rt_handle.spawn(async move {
                    if let Err(e) = daemon_repo_request(
                        crate::daemon::default_daemon_port(),
                        &path,
                        "conversation_demand_start",
                        serde_json::json!({ "lane": &lane }),
                    )
                    .await
                    {
                        tracing::warn!("conversation_demand_start（webview 発）失敗 (lane={lane}): {e}");
                    }
                });
            }
            Event::UserEvent(AppEvent::ConversationSessionFocus { lane, session }) => {
                let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!("conversation:session_focus skip — lane の repo 解決失敗 (lane={lane})");
                    return;
                };
                rt_handle.spawn(async move {
                    if let Err(e) = daemon_repo_request(
                        crate::daemon::default_daemon_port(),
                        &path,
                        "conversation_session_focus",
                        serde_json::json!({ "lane": &lane, "session": session }),
                    )
                    .await
                    {
                        tracing::warn!(
                            "conversation_session_focus 失敗 (lane={lane} session={session}): {e}"
                        );
                        return;
                    }
                    // tab strip の focused 確定は snapshot が運ぶ（doc 53 §11 — server の
                    // `conversation_session_focus` が末尾で `emit_lane_update` を撃つ）。
                    // 新 focused の transcript replay を発火（session 省略 = focused に解決）。
                    // 応答は使わない（replay は topic 経由で ReplayStart として届く）。エラーは warn のみ。
                    if let Err(e) = daemon_repo_request(
                        crate::daemon::default_daemon_port(),
                        &path,
                        "conversation_demand_start",
                        serde_json::json!({ "lane": &lane }),
                    )
                    .await
                    {
                        tracing::warn!("conversation_demand_start（focus 後）失敗 (lane={lane}): {e}");
                    }
                });
            }
            // doc 38 Phase 3: session tab の × による close。remove → 一覧再取得 →
            // demand_start（除去後の新 focused の会話を replay）の順で直列に（focus 切替と同型）。
            // 最後の 1 本は backend が Err で拒否する（GUI も × は 2 本以上でしか出さない = 多重防御）。
            Event::UserEvent(AppEvent::ConversationSessionRemove { lane, session }) => {
                let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!("conversation:session_remove skip — lane の repo 解決失敗 (lane={lane})");
                    return;
                };
                let port = crate::daemon::default_daemon_port();
                rt_handle.spawn(async move {
                    if let Err(e) = daemon_repo_request(
                        port,
                        &path,
                        "conversation_session_remove",
                        serde_json::json!({ "lane": &lane, "session": session }),
                    )
                    .await
                    {
                        // 最後の 1 本の拒否含む（Err）— 一覧はそのまま（GUI は変化なし）。
                        tracing::warn!(
                            "conversation_session_remove 失敗 (lane={lane} session={session}): {e}"
                        );
                        return;
                    }
                    // 除去後の roster / focused は snapshot が運ぶ（doc 53 §11）。
                    // 除去後の新 focused の transcript replay を発火（session 省略 = focused に解決）。
                    if let Err(e) = daemon_repo_request(
                        port,
                        &path,
                        "conversation_demand_start",
                        serde_json::json!({ "lane": &lane }),
                    )
                    .await
                    {
                        tracing::warn!("conversation_demand_start（remove 後）失敗 (lane={lane}): {e}");
                    }
                });
            }
            // doc 38 Phase 2: 「+」menu の engine 選択肢を埋める agents 一覧取得。
            // 既存 + Add Sub と同じ agents_list を再利用（doc 38 §3 の作成 UX）。
            Event::UserEvent(AppEvent::AgentsFetch { lane, req }) => {
                let Some(path) = resolve_repo_path_for_lane(&sidebar_state, &lane) else {
                    tracing::warn!("conversation:agents_fetch skip — lane の repo 解決失敗 (lane={lane})");
                    return;
                };
                let proxy = async_action_proxy.clone();
                rt_handle.spawn(async move {
                    match daemon_repo_request(
                        crate::daemon::default_daemon_port(),
                        &path,
                        "agents_list",
                        serde_json::json!({}),
                    )
                    .await
                    {
                        Ok(payload) => {
                            // doc 47 §6: 要求元の相関 id をそのまま応答へ載せ替える。
                            let _ =
                                proxy.send_event(AppEvent::Agents { lane, payload, req });
                        }
                        Err(e) => {
                            tracing::warn!("conversation:agents_fetch の agents_list 失敗 (lane={lane}): {e}")
                        }
                    }
                });
            }
            // doc 38 Phase 2: agents_list の結果を「+」menu へ push back。
            // doc 47 §6: 第 3 引数 = 要求元の相関 id。共有 bus の購読側はこれで振り分ける。
            Event::UserEvent(AppEvent::Agents { lane, payload, req }) => {
                push_main::console_stands(&webview, &lane, payload, req);
            }
            Event::UserEvent(AppEvent::BoardMutate { method, body }) => {
                // board モデル (2026-07-15): WebView の board mutate（thumbnail ✕ / Clear ボタン）を
                // daemon repo-proxy ask で active repo の repo に forward する。 repo が DB を更新して
                // BoardUpdated(retained) を broadcast し、 canvas channel 経由で webview の board が
                // 更新される（webview は truth を持たず repo の反映を待つ view）。 active repo 解決
                // 失敗は silent skip。
                let Some(path) = resolve_active_repo_path(&sidebar_state) else {
                    tracing::debug!("board mutate skip — active repo 解決失敗");
                    return;
                };
                rt_handle.spawn(async move {
                    match daemon_repo_request(
                        crate::daemon::default_daemon_port(),
                        &path,
                        &method,
                        body,
                    )
                    .await
                    {
                        Ok(_) => tracing::debug!("board mutate ({}) → Daemon OK", method),
                        Err(e) => tracing::warn!("board mutate ({}) 失敗: {}", method, e),
                    }
                });
            }
            Event::UserEvent(AppEvent::ReposError(msg)) => {
                push_sidebar::error(&webview, &msg);
            }
            // R5 Sub create flow: spawn_blocking thread からの結果を sidebar に push back。
            // success → form を閉じる + addSubOpen から削除。
            // error → form 下に inline error 表示 + form は開いたまま (再 submit 可能)。
            Event::UserEvent(AppEvent::SubCreateResult {
                repo_path,
                name,
                error,
            }) => {
                push_sidebar::sub_create_result(&webview, repo_path, name, error);
            }
            Event::UserEvent(AppEvent::AgentsResult {
                repo_path,
                agents,
                error,
            }) => {
                // doc 11 PR-C: + Add Sub form の dropdown を populate するための push back。
                push_sidebar::stands_result(&webview, repo_path, &agents, error);
            }
            // ===== code pane（コードブラウザ P1）=====
            // demand（CodeList / CodeRead）は blocking I/O を spawn_blocking に
            // 逃し、結果 event で main thread に戻して push する（旧 File Explorer と同型）。
            Event::UserEvent(AppEvent::CodeList { lane }) => {
                match lookup_lane_cwd_by_address(&sidebar_state, &lane) {
                    Some(cwd) => {
                        let proxy = async_action_proxy.clone();
                        rt_handle.spawn_blocking(move || {
                            let (entries, truncated) = crate::webview::file_explorer::list_entries(&cwd);
                            let _ = proxy.send_event(AppEvent::CodeEntriesResult {
                                lane,
                                entries,
                                truncated,
                            });
                        });
                    }
                    None => {
                        tracing::warn!("code:list: lane cwd unknown for address={lane} (skip)");
                    }
                }
            }
            Event::UserEvent(AppEvent::CodeRead { lane, rel_path }) => {
                match lookup_lane_cwd_by_address(&sidebar_state, &lane) {
                    Some(cwd) => {
                        let proxy = async_action_proxy.clone();
                        rt_handle.spawn_blocking(move || {
                            let payload = crate::webview::file_explorer::read_file(&cwd, &rel_path);
                            let _ = proxy.send_event(AppEvent::CodeFileResult {
                                lane,
                                rel_path,
                                payload,
                            });
                        });
                    }
                    None => {
                        tracing::warn!("code:read: lane cwd unknown for address={lane} (skip)");
                    }
                }
            }
            Event::UserEvent(AppEvent::CodeEntriesResult {
                lane,
                entries,
                truncated,
            }) => {
                push_main::code_entries(&webview, &lane, &entries, truncated);
            }
            Event::UserEvent(AppEvent::CodeFileResult {
                lane,
                rel_path,
                payload,
            }) => {
                push_main::code_file(&webview, &lane, &rel_path, &payload);
            }
            // Wire inbox (doc 34 §4 V1): fetch 結果を sidebar の vpWire 受け口へ push back。
            Event::UserEvent(AppEvent::WireHistoryResult { address, payload }) => {
                tracing::debug!("wire history 受領 (address={address})");
                push_sidebar::wire_result(&webview, payload);
            }
            Event::UserEvent(AppEvent::ActivityUpdate(snap)) => {
                sidebar_state.activity = snap;
                // 適用中フラグは GUI local（health 由来ではない）ので poll 上書きから守る。
                sidebar_state.activity.update_applying = update_applying;
                push_sidebar_state(&webview, &sidebar_state);
            }
            Event::UserEvent(AppEvent::UpdateFlowPhase(applying)) => {
                update_applying = applying;
                sidebar_state.activity.update_applying = applying;
                push_sidebar_state(&webview, &sidebar_state);
            }
            Event::UserEvent(AppEvent::SettingsRepoRootPicked(path)) => {
                // キャンセル (None) は**書かない**（既存値を保持）。ただし overlay の表示は
                // 現実に合わせたいので、選ばれた / 選ばれなかったに関わらず確定値を返す。
                if let Some(p) = path {
                    settings.default_repo_root = Some(p);
                    if let Err(e) = settings.save() {
                        tracing::warn!("Settings 保存失敗: {e}");
                    }
                }
                // picker は vp-app.toml しか触らないので daemon 側は引き直さない
                // （`last_daemon_settings` に前回の結果が残っている）。
                push_sidebar::settings_result(
                    &webview,
                    settings_snapshot(
                        &settings,
                        &sidebar_state,
                        dev_mode,
                        last_daemon_settings.as_ref(),
                    ),
                );
            }
            Event::UserEvent(AppEvent::SettingsDaemonFetched(fetched)) => {
                // daemon 側（settings.kdl）が揃ったので、vp-app.toml 側と合流させて
                // **1 回だけ** push する。`None` = 接続できなかった（UI は該当区画を
                // 「daemon に接続すると編集できます」に落とす）。
                last_daemon_settings = fetched.map(|v| {
                    let text = |k: &str| {
                        v.get(k)
                            .and_then(|x| x.as_str())
                            .map(str::to_string)
                    };
                    DaemonSettings {
                        log_level: text("log_level"),
                        idle_timeout_minutes: v
                            .get("idle_timeout_minutes")
                            .and_then(|x| x.as_i64()),
                        default_agent: text("default_agent"),
                        default_model: text("default_model"),
                        default_agent_takes_model: v
                            .get("default_agent_takes_model")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false),
                    }
                });
                push_sidebar::settings_result(
                    &webview,
                    settings_snapshot(
                        &settings,
                        &sidebar_state,
                        dev_mode,
                        last_daemon_settings.as_ref(),
                    ),
                );
            }
            Event::UserEvent(AppEvent::SidebarIpc(msg)) => {
                // VP-100 follow-up: repo:add / repo:clone は async picker → API → ReposLoaded ルート
                // (state 直接 mutate しないので handle_sidebar_ipc の前で分岐)
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&msg) {
                    match parsed.get("t").and_then(|v| v.as_str()) {
                        Some("repo:add") => {
                            let initial_dir =
                                resolve_default_repo_root(&settings, &sidebar_state);
                            spawn_add_repo_picker(
                                async_action_proxy.clone(),
                                initial_dir,
                                rt_handle.clone(),
                                daemon_conn.clone(),
                            );
                            return;
                        }
                        Some("process:clone") => {
                            let url = parsed
                                .get("url")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if url.is_empty() {
                                tracing::warn!("process:clone with empty url");
                                return;
                            }
                            let target_override = parsed
                                .get("target_dir")
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .map(std::path::PathBuf::from);
                            let default_root =
                                resolve_default_repo_root(&settings, &sidebar_state);
                            spawn_clone_repo(
                                async_action_proxy.clone(),
                                url,
                                default_root,
                                target_override,
                                rt_handle.clone(),
                                daemon_conn.clone(),
                            );
                            return;
                        }
                        _ => {}
                    }
                }
                let outcome = handle_sidebar_ipc(&msg, &mut sidebar_state, &mut session_state);
                // Lane activation — activate_lane() が全副作用を処理
                if let Some(addr) = outcome.activate_lane {
                    activate_lane(
                        &addr,
                        &mut sidebar_state,
                        &mut session_state,
                        &webview,
                        &mut lane_respawn_triggered,
                        &rt_handle,
                        &respawn_proxy,
                    );
                    // gui: chat lane なら conversation topic に attach（→ transcript replay）。
                    ensure_conversation_attach(
                        &addr,
                        &sidebar_state,
                        &mut conversation_sessions,
                        &rt_handle,
                        &async_action_proxy,
                        &daemon_conn,
                    );
                } else {
                    if outcome.changed {
                        push_sidebar_state(&webview, &sidebar_state);
                    }
                    if outcome.active_changed {
                        push_active_view(&webview, &sidebar_state);
                    }
                }
                // Architecture v4: dead な repo が expand されたら repo を auto-spawn。
                // dedup: 同 session で同じ path を 2 回呼ばない (daemon 側でも弾かれるが
                // 余計な POST を避ける)。
                if let Some((name, path)) = outcome.repo_spawn_request {
                    if repo_spawn_triggered.insert(path.clone()) {
                        tracing::info!(
                            "repo auto-spawn 要求 (accordion expand trigger): name={} path={}",
                            name,
                            path
                        );
                        spawn_sp_start(
                            &rt_handle,
                            async_action_proxy.clone(),
                            name,
                            path,
                            daemon_conn.clone(),
                        );
                    } else {
                        tracing::debug!("repo auto-spawn skip (既 trigger): {}", path);
                    }
                }
                // Phase 5-D fix: accordion 閉じた → dedup HashSet から path を release。
                //  spawn 失敗で entry が居残ったまま user が collapse → expand すれば確実に retry。
                if let Some(path) = outcome.repo_spawn_release
                    && repo_spawn_triggered.remove(&path)
                {
                    tracing::info!(
                        "repo auto-spawn dedup released (accordion collapse): {}",
                        path
                    );
                }
                // 「見えている Lane だけ生きている」: accordion の開閉で購読を張り直す。
                // 全 lane を回すのは LanesLoaded の再評価と同じ形（冪等・数十 lane 規模）。
                if outcome.conversation_reattach {
                    let all_addrs: Vec<String> = sidebar_state
                        .lanes_by_repo
                        .values()
                        .flatten()
                        .map(|l| l.address.key().to_string())
                        .collect();
                    for addr in all_addrs {
                        ensure_conversation_attach(
                            &addr,
                            &sidebar_state,
                            &mut conversation_sessions,
                            &rt_handle,
                            &async_action_proxy,
                            &daemon_conn,
                        );
                    }
                }
                // Phase 5-C: Process restart 要求 (sidebar の 🔄 button から)。
                // 全 async work は shared runtime (rt_handle) 経由 — bare `tokio::spawn` は禁止
                // (.clippy.toml で compile gate)、 tao event loop closure に runtime context が
                // 無いので必ず `rt_handle.spawn` を使う。
                if let Some(repo_name) = outcome.restart_process_request {
                    let proxy = async_action_proxy.clone();
                    let conn = daemon_conn.clone();
                    rt_handle.spawn(async move {
                        // doc 45 段 3: 旧 `POST /api/daemon/processes/{name}/restart` を
                        // Unison `daemon-control.repos/restart` に差し替え。 接続先は共有
                        // QUIC connection (port 解決は conn manager が持つ)。
                        let control = match conn.control().await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("restart_process: {}", e);
                                return;
                            }
                        };
                        match control.restart_process(&repo_name).await {
                            Ok(()) => {
                                tracing::info!("restart_process OK: {}", repo_name);
                                // 完了 → repos 再 fetch → sidebar state badge 更新。
                                // 必ず `fetch_repos_with_ports` 経由 (= runtime port merge)
                                // で送る。 list_repos() だけだと restart 直後に全 repo の
                                // port が None で潰れ、 後続 LanesLoaded で ensureLane が
                                // 全件 skip され main terminal が消失する。
                                if let Ok(repos) = fetch_repos_with_ports(&control).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ReposLoaded(repos));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "restart_process failed for {}: {}",
                                    repo_name,
                                    e
                                );
                            }
                        }
                    });
                }
                // Process stop 要求 (repo context menu の Stop repo から)。
                if let Some(repo_name) = outcome.stop_process_request {
                    let proxy = async_action_proxy.clone();
                    let conn = daemon_conn.clone();
                    rt_handle.spawn(async move {
                        let control = match conn.control().await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("stop_process: {}", e);
                                return;
                            }
                        };
                        match control.stop_process(&repo_name).await {
                            Ok(()) => {
                                tracing::info!("stop_process OK: {}", repo_name);
                                // 完了 → repos 再 fetch → 停止 state を sidebar に反映。
                                // restart と同じく `fetch_repos_with_ports` 経由で
                                // 他 repo の runtime port を保つ。
                                if let Ok(repos) = fetch_repos_with_ports(&control).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ReposLoaded(repos));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "stop_process failed for {}: {}",
                                    repo_name,
                                    e
                                );
                            }
                        }
                    });
                }
                // Repo delete 要求 (repo context menu の Delete repo から、
                // UI で 2-click 確認済)。 daemon の remove_repo は稼働中 repo があると
                // エラーになるため、 先に stop → grace → remove と chain する
                // (restart_process が capability 内でやっているのと同じ順序)。
                if let Some((repo_name, repo_path)) = outcome.delete_repo_request {
                    let proxy = async_action_proxy.clone();
                    let conn = daemon_conn.clone();
                    rt_handle.spawn(async move {
                        let control = match conn.control().await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("delete_repo: {}", e);
                                return;
                            }
                        };
                        // stop は best-effort: repo が未起動 (= 停止中) なら
                        // 「No running Process」 エラーが返るが、 続行して remove する。
                        match control.stop_process(&repo_name).await {
                            Ok(()) => {
                                tracing::info!("delete: stop_process OK: {}", repo_name);
                                // shutdown 伝播 + port release を待つ grace period
                                tokio::time::sleep(std::time::Duration::from_millis(500))
                                    .await;
                            }
                            Err(e) => {
                                tracing::info!(
                                    "delete: stop_process skipped for {} (continuing): {}",
                                    repo_name,
                                    e
                                );
                            }
                        }
                        match control.remove_repo(&repo_path).await {
                            Ok(()) => {
                                tracing::info!("remove_repo OK: {}", repo_path);
                                // 完了 → repos 再 fetch → sidebar から除去。
                                // 削除対象以外の repo の runtime port を保つため
                                // `fetch_repos_with_ports` 経由で送る。
                                if let Ok(repos) = fetch_repos_with_ports(&control).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ReposLoaded(repos));
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "remove_repo failed for {}: {}",
                                    repo_path,
                                    e
                                );
                            }
                        }
                    });
                }
                // Phase 1 (doc 24): repo 並び替えを daemon の repo_order に永続化する。
                // restart/stop と同じ「操作 → re-fetch → ReposLoaded」パターン。成功後の
                // ReposLoaded で currents_order が canonical 順に reconcile される。
                if let Some(order) = outcome.reorder_request {
                    let proxy = async_action_proxy.clone();
                    let conn = daemon_conn.clone();
                    rt_handle.spawn(async move {
                        let control = match conn.control().await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!("reorder_repos: {}", e);
                                return;
                            }
                        };
                        match control.reorder_repos(order).await {
                            Ok(()) => {
                                tracing::info!("reorder_repos OK");
                                // 完了 → repos 再 fetch → canonical 順で sidebar reconcile。
                                if let Ok(repos) = fetch_repos_with_ports(&control).await {
                                    let _ =
                                        proxy.send_event(AppEvent::ReposLoaded(repos));
                                }
                            }
                            Err(e) => {
                                tracing::warn!("reorder_repos failed: {}", e);
                            }
                        }
                    });
                }
                // Model Q: active lane を daemon canonical に永続 (fire-and-forget、 optimistic 適用済)。
                if let Some((repo_path, address)) = outcome.set_active_lane_request {
                    let conn = daemon_conn.clone();
                    rt_handle.spawn(async move {
                        let result = match conn.control().await {
                            Ok(control) => control.set_active_lane(repo_path, address).await,
                            Err(e) => Err(e),
                        };
                        if let Err(e) = result {
                            tracing::warn!("set_active_lane failed: {}", e);
                        }
                    });
                }
                // Phase 4-A: Sub Lane 削除要求 (sidebar の × button から)
                if let Some((repo_path, address)) = outcome.delete_lane_request {
                    // F6②: 旧 DaemonRpcClient.delete_lane (repo 直結 reqwest) を daemon repo-proxy
                    // ask (lane_delete) に移管。 repo port 解決は不要になり repo_path を handshake で渡す。
                    // JS-side からも先 removeLane を呼ぶ (= xterm 即時 dispose、 server 反映は
                    // repo の "lanes" topic snapshot 経由で sidebar に届く)。
                    push_main::remove_lane(&webview, &address);
                    rt_handle.spawn(async move {
                        let payload = serde_json::json!({ "address": &address });
                        match daemon_repo_request(
                            crate::daemon::default_daemon_port(),
                            &repo_path,
                            "lane_delete",
                            payload,
                        )
                        .await
                        {
                            Ok(_) => {
                                tracing::info!(
                                    "Lane deleted: repo={} address={}",
                                    repo_path,
                                    address
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "lane_delete failed: repo={} address={}: {}",
                                    repo_path,
                                    address,
                                    e
                                );
                            }
                        }
                    });
                }
                // Lane Main Agent restart 要求 (sidebar の restart icon → confirm dialog から)
                if let Some((repo_path, address, fresh)) = outcome.restart_lane_request {
                    // F6③: 旧 DaemonRpcClient.restart_lane (repo 直結 reqwest) を daemon repo-proxy
                    // ask (lane_restart) に移管。 repo port 解決は不要、 repo_path を handshake で渡す。
                    rt_handle.spawn(async move {
                        let payload = serde_json::json!({ "address": &address, "fresh": fresh });
                        match daemon_repo_request(
                            crate::daemon::default_daemon_port(),
                            &repo_path,
                            "lane_restart",
                            payload,
                        )
                        .await
                        {
                            Ok(_) => {
                                // 新 pid / state は repo の "lanes" topic snapshot で購読側に push され、
                                // 端末は canvas channel demand 経由で新 PtySlot に再 attach し直す。
                                tracing::info!(
                                    "Lane restarted: repo={} address={}",
                                    repo_path,
                                    address
                                );
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "lane_restart failed: repo={} address={}: {}",
                                    repo_path,
                                    address,
                                    e
                                );
                            }
                        }
                    });
                }
                // doc 39 §8.4 提案 2: 新しい root conversation（sidebar lane 行の context menu）。
                // backend が新 session を採番して root に向ける。旧 root の pane / 会話は残る
                //（= Reset Lane との違い）。反映は lanes snapshot / session list が運ぶので
                // 楽観更新しない。
                if let Some((repo_path, address)) = outcome.new_root_request {
                    rt_handle.spawn(async move {
                        match daemon_repo_request(
                            crate::daemon::default_daemon_port(),
                            &repo_path,
                            "conversation_session_new_root",
                            serde_json::json!({ "lane": &address }),
                        )
                        .await
                        {
                            Ok(res) => {
                                let session =
                                    res.get("session").and_then(serde_json::Value::as_u64);
                                tracing::info!(
                                    "new root conversation: repo={repo_path} lane={address} session={session:?}"
                                );
                            }
                            Err(e) => tracing::warn!(
                                "conversation_session_new_root failed: repo={repo_path} lane={address}: {e}"
                            ),
                        }
                    });
                }
                // doc 44 D4/D5: 開発起点の再指定 (sidebar lane 行の context menu から)。
                // Host の帳簿のポインタを書き換えるだけ — cwd も active lane も engine も動かない。
                // 反映は次の lanes snapshot の `origin` で戻る（楽観更新しない = 帳簿が真実源）。
                if let Some((repo_path, address)) = outcome.set_origin_request {
                    rt_handle.spawn(async move {
                        // 帳簿は lane **名**で受ける（起点は repo ごとに 1 本なので
                        // address の `<repo>` 部分は冗長）。address からは末尾を取る。
                        let lane_name = address.rsplit('/').next().unwrap_or("").to_string();
                        if lane_name.is_empty() {
                            tracing::warn!("lane_origin_set: address から lane 名を取れない: {address}");
                            return;
                        }
                        let payload = serde_json::json!({ "lane": lane_name });
                        match daemon_repo_request(
                            crate::daemon::default_daemon_port(),
                            &repo_path,
                            "lane_origin_set",
                            payload,
                        )
                        .await
                        {
                            Ok(_) => tracing::info!(
                                "開発起点を変更: repo={} lane={}",
                                repo_path,
                                lane_name
                            ),
                            Err(e) => tracing::warn!(
                                "lane_origin_set failed: repo={} lane={}: {}",
                                repo_path,
                                lane_name,
                                e
                            ),
                        }
                    });
                }
                // doc 44 §12: lane の並び順を帳簿に保存する（sidebar の DnD）。
                // address 列を lane 名の列に畳んでから投げる（帳簿は lane 名で受け、
                // 境界で lane_id に解決する — 起点と同じ規律）。
                if let Some((repo_path, order)) = outcome.reorder_lanes_request {
                    rt_handle.spawn(async move {
                        let names: Vec<String> = order
                            .iter()
                            .filter_map(|a| a.rsplit('/').next())
                            .filter(|n| !n.is_empty())
                            .map(|n| n.to_string())
                            .collect();
                        if names.is_empty() {
                            tracing::warn!("lane_order_set: address 列から lane 名を取れない");
                            return;
                        }
                        let payload = serde_json::json!({ "order": names });
                        match daemon_repo_request(
                            crate::daemon::default_daemon_port(),
                            &repo_path,
                            "lane_order_set",
                            payload,
                        )
                        .await
                        {
                            Ok(_) => tracing::info!(
                                "lane の並び順を保存: repo={} count={}",
                                repo_path,
                                names.len()
                            ),
                            Err(e) => tracing::warn!(
                                "lane_order_set failed: repo={}: {}",
                                repo_path,
                                e
                            ),
                        }
                    });
                }
                // Phase 3-A: Sub Lane 作成要求 (sidebar の + Add Sub から)
                // 投げ先は Daemon (:32000) の `daemon-control.lanes/create` 1 本 (repo port 解決は不要、
                // set_active_lane / reorder と同じ daemon-command パターン)。
                // doc 44 §9.4: daemon 側はそこで自前の provision をせず repo runtime の
                // lane 作成 core に委譲する — worktree も PtySlot も**この 1 往復で揃う**。
                // 旧構成は descriptor だけ作って PtySlot を lane_watcher の到達に賭けており、
                // 「+ で作った lane だけ engine 指定が別経路で伝わる」等の経路差が生じていた。
                // doc 11 PR-C: agent 指定 を tuple 4 番目に保持 (None なら daemon-side default)。
                if let Some((repo_path, name, branch, agent)) = outcome.add_sub_request {
                    let proxy = async_action_proxy.clone();
                    let name_clone = name.clone();
                    let branch_clone = branch.clone();
                    let stand_clone = agent.clone();
                    let path_clone = repo_path.clone();
                    let conn = daemon_conn.clone();
                    rt_handle.spawn(async move {
                        let control = match conn.control().await {
                            Ok(c) => c,
                            Err(e) => {
                                let msg = e.to_string();
                                tracing::warn!("create_sub_lane: {}", msg);
                                let _ = proxy.send_event(AppEvent::SubCreateResult {
                                    repo_path: path_clone,
                                    name: name_clone,
                                    error: Some(msg),
                                });
                                return;
                            }
                        };
                        match control
                            .create_sub_lane(
                                &path_clone,
                                &name_clone,
                                branch_clone.as_deref(),
                                stand_clone.as_deref(),
                            )
                            .await
                        {
                            Ok(()) => {
                                tracing::info!(
                                    "Sub Lane created (daemon): repo={} name={} branch={:?}",
                                    path_clone,
                                    name_clone,
                                    branch_clone
                                );
                                // 応答が返った時点で lane は既に spawn 済（doc 44 §9.4）。
                                // sidebar への反映は "lanes" topic snapshot の push を待つ
                                // （楽観更新しない = 真実源は 1 つ、doc 44 §10.3 と同じ規律）。
                                // R5: 成功通知を sidebar に push back (form を閉じる)
                                let _ = proxy.send_event(AppEvent::SubCreateResult {
                                    repo_path: path_clone,
                                    name: name_clone,
                                    error: None,
                                });
                            }
                            Err(e) => {
                                // R5: 失敗通知を sidebar に push back (form 下に inline error 表示)。
                                // doc 45 段 3 以降は Unison の error 慣習 (VP-163) に従い
                                // "daemon-control.lanes/create: <daemon 側の理由>" が返る
                                // (旧 HTTP の "... HTTP 500: {json}" より読める)。 そのまま流す。
                                let msg = format!("{}", e);
                                tracing::warn!(
                                    "create_sub_lane failed: repo={} name={}: {}",
                                    path_clone,
                                    name_clone,
                                    msg
                                );
                                let _ = proxy.send_event(AppEvent::SubCreateResult {
                                    repo_path: path_clone,
                                    name: name_clone,
                                    error: Some(msg),
                                });
                            }
                        }
                    });
                }

                // doc 11 PR-C / F6④: 利用可能 Agent 一覧 fetch 要求 (sidebar の + Add Sub 開閉から)。
                // 旧 SP 直結 (client.list_agents) を撤去し daemon repo-proxy ask (`agents_list`) に移管。
                // repo port 解決が消滅し、 surface は Daemon :32000 だけを知れば済む (L1 portless 前進)。
                if let Some(repo_path) = outcome.list_stands_request {
                    let proxy = async_action_proxy.clone();
                    rt_handle.spawn(async move {
                        let (agents, error) = match daemon_repo_request(
                            crate::daemon::default_daemon_port(),
                            &repo_path,
                            "agents_list",
                            serde_json::json!({}),
                        )
                        .await
                        {
                            // repo は {agents:[...]} を返す。 agents 配列だけ Vec<AgentInfo> に deserialize。
                            Ok(v) => {
                                let agents = v
                                    .get("agents")
                                    .and_then(|s| {
                                        serde_json::from_value::<Vec<crate::daemon_wire::AgentInfo>>(
                                            s.clone(),
                                        )
                                        .ok()
                                    })
                                    .unwrap_or_default();
                                tracing::debug!(
                                    "agents listed: repo={} count={}",
                                    repo_path,
                                    agents.len()
                                );
                                (agents, None)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "agents_list failed: repo={}: {}",
                                    repo_path,
                                    e
                                );
                                (Vec::new(), Some(e))
                            }
                        };
                        let _ = proxy.send_event(AppEvent::AgentsResult {
                            repo_path,
                            agents,
                            error,
                        });
                    });
                }

                // Wire inbox (doc 34 §4 V1): Daemon "wire" channel への read-only fetch
                // (ack 要求は「ack → 再 fetch」に畳む)。 async I/O なので tokio task に逃し、
                // 結果は AppEvent::WireHistoryResult で event loop に戻して
                // window.vpWire.handleResult へ push back する。
                // fetch と ack は別 IPC で、 IpcEnvelope の単一 variant match により同一
                // outcome で両立しない — 単純な合成で足りる (ack は「ack → 再 fetch」に畳む)。
                let wire_req = outcome
                    .wire_ack_request
                    .map(|(addr, id)| (addr, Some(id)))
                    .or_else(|| outcome.wire_fetch_request.map(|a| (a, None)));
                if let Some((address, ack_id)) = wire_req {
                    let proxy = async_action_proxy.clone();
                    let conn = daemon_conn.clone();
                    rt_handle.spawn(async move {
                        let payload = wire_fetch_payload(conn, address.clone(), ack_id).await;
                        let _ =
                            proxy.send_event(AppEvent::WireHistoryResult { address, payload });
                    });
                }

                // in-app update: sidebar footer の「更新する」ボタン click 要求。
                // native 確認ダイアログ → self-update → daemon restart → relaunch を
                // 専用スレッドで起動する（event loop = main thread は塞がない）。
                // on_phase は AppEvent 経由で event loop に戻し、「更新中…」表示に使う。
                if let Some(version) = outcome.update_apply_request {
                    let phase_proxy = proxy.clone();
                    crate::flows::update::spawn_update_flow(version, move |applying| {
                        let _ = phase_proxy.send_event(AppEvent::UpdateFlowPhase(applying));
                    });
                }

                // 設定 overlay（doc 59 P1）。fetch / save は最後に必ず「確定値を push back」で
                // 合流する — client が楽観更新をしないので、**真実は vp-app.toml 1 本**で決まり、
                // 保存失敗時の巻き戻しを client に持たせなくてよい。
                let settings_saved = outcome.settings_save_request.is_some();
                // daemon 側（settings.kdl）へ中継する分。**vp-app は書かない** — 書き手を
                // daemon 唯一にしてある（doc 59 §3）ので、ここは payload を組むだけ。
                let mut daemon_payload = serde_json::Map::new();
                if let Some(save) = outcome.settings_save_request {
                    // ⚠️ `None` の field は**不変**（「変えた分だけ送る」契約）。
                    if let Some(dev) = save.developer_mode {
                        dev_mode = dev;
                        open_devtools_item.set_enabled(dev);
                        reload_webview_item.set_enabled(dev);
                        settings.developer_mode = Some(dev);
                    }
                    if let Some(root) = save.default_repo_root {
                        // 空文字 = **未設定に戻す**（推定へのフォールバックを復活させる）。
                        // 消し方を別 UI にしないための約束 — 入力欄を空にすれば戻る。
                        let trimmed = root.trim();
                        settings.default_repo_root =
                            (!trimmed.is_empty()).then(|| trimmed.to_string());
                    }
                    if let Err(e) = settings.save() {
                        tracing::warn!("Settings 保存失敗: {e}");
                    }
                    if let Some(level) = save.log_level {
                        daemon_payload.insert("log_level".into(), serde_json::json!(level));
                    }
                    if let Some(minutes) = save.idle_timeout_minutes {
                        daemon_payload
                            .insert("idle_timeout_minutes".into(), serde_json::json!(minutes));
                    }
                    if let Some(agent) = save.default_agent {
                        daemon_payload.insert("default_agent".into(), serde_json::json!(agent));
                    }
                    if let Some(model) = save.default_model {
                        daemon_payload.insert("default_model".into(), serde_json::json!(model));
                    }
                }
                if outcome.settings_pick_repo_root_request {
                    // rfd は blocking なので専用スレッド → 結果は
                    // `AppEvent::SettingsRepoRootPicked` で戻る（そこで保存 + push back）。
                    let initial = resolve_default_repo_root(&settings, &sidebar_state);
                    spawn_repo_root_picker(async_action_proxy.clone(), initial);
                }
                if outcome.settings_fetch_request || settings_saved {
                    // ⚠️ **書いてから読む**を 1 つの task に閉じ込める。2 本に分けると
                    // 「保存より先に読み終えて古い値を表示する」順序が生まれる。
                    // 読めたら `SettingsDaemonFetched` で戻り、そこで vp-app.toml 側と
                    // 合流させて 1 回だけ push する。
                    let conn = daemon_conn.clone();
                    let ev_proxy = proxy.clone();
                    let payload = (!daemon_payload.is_empty())
                        .then_some(serde_json::Value::Object(daemon_payload));
                    rt_handle.spawn(async move {
                        let fetched = match conn.control().await {
                            Ok(control) => {
                                if let Some(p) = payload
                                    && let Err(e) = control.settings_set(p).await
                                {
                                    tracing::warn!("settings/set 失敗: {e}");
                                }
                                control.settings_get().await.ok()
                            }
                            Err(e) => {
                                tracing::debug!("settings: daemon に接続できない: {e}");
                                None
                            }
                        };
                        let _ = ev_proxy.send_event(AppEvent::SettingsDaemonFetched(fetched));
                    });
                }
                if outcome.daemon_restart_request {
                    // ⚠️ **全 repo = 全 lane の claude が落ちる**（doc 44 P1 fold-in）。
                    // 確認ダイアログは flow 側（rfd が blocking なので専用スレッド）。
                    crate::daemon::restart::spawn_daemon_restart();
                }
                // Hub 行の Login / Logout ボタン click 要求。blocking フロー（browser OAuth
                // 待ち / 確認ダイアログ / CLI spawn）を blocking pool で実行し、成功したら
                // `daemon-control.hub/reconnect` で daemon の hub 接続に credential 変化を
                // 即反映する（= 押した結果が数秒後の health poll で Hub 行に現れる）。
                // ACTIONS の永続化要求（doc 57 Phase 4）。watch は latest-wins なので、
                // 打鍵ごとに来ても debounce task が静まった 1 回だけを daemon へ撃つ。
                if let Some(payload) = outcome.actions_persist_request {
                    let _ = actions_persist_tx.send(Some(payload));
                }
                if outcome.auth_login_request.is_some() || outcome.auth_logout_request.is_some() {
                    let login_target = outcome.auth_login_request.clone();
                    let logout_target = outcome.auth_logout_request.clone();
                    let conn = daemon_conn.clone();
                    let rt = rt_handle.clone();
                    rt_handle.spawn(async move {
                        let flow = rt.spawn_blocking(move || match login_target {
                            Some(t) => crate::flows::auth::run_login_blocking(&t),
                            None => crate::flows::auth::run_logout_blocking(
                                logout_target.as_deref().unwrap_or(""),
                            ),
                        });
                        // false = キャンセル / 失敗 / 二重起動 → credentials 不変なので反映不要。
                        if !matches!(flow.await, Ok(true)) {
                            return;
                        }
                        match conn.control().await {
                            Ok(control) => {
                                if let Err(e) = control.hub_reconnect().await {
                                    tracing::warn!(
                                        "auth flow: hub/reconnect 要求に失敗（次の自然な再接続で反映される）: {}",
                                        e
                                    );
                                }
                            }
                            Err(e) => tracing::warn!(
                                "auth flow: daemon 接続に失敗（hub/reconnect 未送信）: {}",
                                e
                            ),
                        }
                    });
                }
            }
            // VP-100 γ-light: ResizeObserver からの slot 矩形通知を蓄積。
            // Phase 4+ で native overlay の `set_position` 同期に使う。
            Event::UserEvent(AppEvent::SlotRect {
                pane_id,
                kind,
                rect,
            }) => {
                if let Some(id) = pane_id {
                    slot_rects.insert(id.clone(), rect);
                    tracing::trace!("slot:rect kind={} pane={} rect={:?}", kind, id, rect);
                } else {
                    tracing::trace!("slot:rect kind={} (no pane_id) rect={:?}", kind, rect);
                }
            }
            // VP-100 follow-up: muda メニュー項目クリック処理
            //
            // ⚠️ **"Developer Mode" の toggle は設定ページへ移設した**（doc 59 P1）。ここに
            // 残るのは dev_mode で gate される 2 項目で、gate 自体の切替は
            // `settings:save` の arm が担う（両 item の `set_enabled` もそちら）。
            //  - "Open Developer Tools" → dev_mode == true なら webview.open_devtools()
            Event::UserEvent(AppEvent::MenuClicked(id)) => {
                if id == menu_ids.new_window {
                    // Cmd+N: 新規 vp-app process を spawn = 新しい MainWindow が独立 process で立つ。
                    // 同 EventLoop に重ねるのではなく fork-style で別 process 化することで、
                    // state 干渉ゼロ + crash isolation + multi-instance 並行開発が可能に。
                    // daemon (port 32000) は process 横断 shared なので repos 一覧は同期。
                    //
                    // instance index を明示採番する (= 旧 bug 修正)。 採番しないと子は
                    // 全員 instance 0 相当に落ち、 `session.0.json` を共有して per-window state
                    // (active_lane / geometry) を互いに clobber していた。 採番直後に open=true で
                    // 予約 save しておくと、 連打 (= 複数 Cmd+N) でも次の採番が同 index を避ける
                    // (= race 防止)。
                    let new_idx = SessionState::next_free_secondary_index();
                    let mut reserved = SessionState::load(new_idx);
                    reserved.set_open(true);
                    reserved.save();
                    match std::env::current_exe() {
                        Ok(exe) => {
                            match std::process::Command::new(&exe)
                                // 子 process は auto-select を skip ── 元 vp-app と active_lane
                                // が衝突して両方の terminal WS が壊れるのを防ぐ。
                                // 起動後 user が手動で lane 選択するまで main_area は empty。
                                .env("VP_APP_INSTANCE", new_idx.to_string())
                                .spawn()
                            {
                                Ok(child) => {
                                    tracing::info!(
                                        "Cmd+N: spawned new vp-app process (pid={}, instance_index={})",
                                        child.id(),
                                        new_idx
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Cmd+N: failed to spawn new process at {}: {}",
                                        exe.display(),
                                        e
                                    );
                                    // spawn 失敗 → 予約した open=true を解放 (= 次回 primary 起動の
                                    // auto-spawn が存在しない secondary を起こすのを防ぐ)。
                                    reserved.set_open(false);
                                    reserved.save();
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Cmd+N: current_exe() failed: {}", e);
                            // 同上: spawn に至らなかったので予約を解放。
                            reserved.set_open(false);
                            reserved.save();
                        }
                    }
                } else if id == menu_ids.open_file {
                    // File menu → "Code Browser": code pane の toggle を webview に要求。
                    //
                    // menu click は OS-level で発火するため、 Pane (terminal / Canvas) focus 中
                    // でも到達する (これが要件の本質)。 active lane の有無は webview 側
                    // （code-view.ts — lane 不在なら no-op）に委譲し、 Rust に判定を持たない
                    // （旧 File Explorer は Rust 側で active 判定していたが、 判定が 2 箇所に
                    // なる & sidebar_state を menu 経路が読む結合が残るため一本化した）。
                    tracing::info!("File menu: code pane toggle 要求");
                    push_main::code_toggle(&webview);
                } else if id == menu_ids.open_devtools {
                    if dev_mode {
                        webview.open_devtools();
                        tracing::info!("DevTools open");
                    } else {
                        tracing::warn!("Open DevTools clicked but dev_mode=false (gated)");
                    }
                } else if id == menu_ids.reload_webview {
                    // doc 48 Phase 1: HMR loop の reload 側。VP_WEBVIEW_DEV 設定時は
                    // reload で *.bundle.js が disk から fresh に取り直される。
                    if dev_mode {
                        if let Err(e) = webview.evaluate_script("location.reload()") {
                            tracing::warn!("Reload WebView 失敗: {}", e);
                        } else {
                            tracing::info!("Reload WebView (location.reload)");
                        }
                    } else {
                        tracing::warn!("Reload WebView clicked but dev_mode=false (gated)");
                    }
                } else {
                    tracing::debug!("MenuClicked: 未処理の id = {:?}", id);
                }
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod main_view_asset_tests {
    //! 統合 WebView (step 3a) の単一 HTML が vp-asset:// で配信でき、SolidJS bundle を
    //! 外部 script (vp-asset://app/*.bundle.js) として参照・配信できること (doc 48 Phase 1)。
    //! Bundle font / serve handler のテストは `webview::assets` module 側に分離。
    use super::*;

    /// `MAIN_VIEW_ASSETS` で統合 HTML が `vp-asset://app/index.html` から取れる。
    #[test]
    fn main_view_html_servable_via_vp_asset() {
        let html =
            crate::webview::assets::lookup_asset("vp-asset://app/index.html", MAIN_VIEW_ASSETS);
        assert!(html.is_some(), "index.html not lookupable");
        let (bytes, ct) = html.unwrap();
        assert_eq!(ct, "text/html; charset=utf-8");
        assert_eq!(bytes, MAIN_AREA_HTML.as_bytes());
    }

    /// 統合 HTML が sidebar mount point を持ち、bundle を外部 script として参照する
    /// (doc 48 Phase 1: inline → `<script src>` 化。相対 src は page origin
    /// `vp-asset://app/` により `app/*.bundle.js` に解決される)。
    #[test]
    fn main_area_html_references_external_bundles() {
        assert!(
            MAIN_AREA_HTML.contains(r#"id="sidebar-root""#),
            "統合 HTML に #sidebar-root mount point がない"
        );
        assert!(
            MAIN_AREA_HTML.contains(r#"<script src="editor-host.bundle.js"></script>"#),
            "統合 HTML が editor-host bundle を外部 script 参照していない"
        );
        assert!(
            MAIN_AREA_HTML.contains(r#"<script src="sidebar.bundle.js"></script>"#),
            "統合 HTML が sidebar bundle を外部 script 参照していない"
        );
    }

    /// 外部化した bundle が `vp-asset://app/*.bundle.js` から配信できる
    /// (baked 経路 = `VP_WEBVIEW_DEV` 未設定時の prod 挙動)。
    #[test]
    fn bundles_servable_via_vp_asset() {
        for (path, marker) in [
            ("vp-asset://app/sidebar.bundle.js", "[vp-sidebar] booting"),
            ("vp-asset://app/editor-host.bundle.js", "EditorHost"),
        ] {
            let asset = crate::webview::assets::lookup_asset(path, MAIN_VIEW_ASSETS);
            assert!(asset.is_some(), "{path} not lookupable");
            let (bytes, ct) = asset.unwrap();
            assert_eq!(ct, "application/javascript; charset=utf-8");
            assert!(
                String::from_utf8_lossy(bytes).contains(marker),
                "{path} の中身に marker `{marker}` が無い (bundle 生成物が想定と違う)"
            );
        }
    }
}
