//! UI thread の可変 state — 旧 `run()` の閉包が捕まえていた 21 個の `let mut` を 1 struct に束ねる
//! （doc 60 §6 6-2、Codex 再レビュー ⑤）。resource は [`super::boot::Boot`]。
//!
//! 閉包は [`UiState`] を値で持つので、`&mut ui.sidebar_state` と `&mut ui.persist` は
//! 旧 local と同じく **disjoint な place borrow** として共存する。handler（`on_*`）は `&mut UiState`
//! を受け、helper（lane_view 等）は今どおり field を明示引数で受ける（`&mut UiState` を取る helper は
//! 呼び手の field borrow と衝突するので作らない）。
//!
//! field 名は旧 local 名をそのまま使う（移設の照合を「`ui.` / `boot.` を剥がすだけ」にするため。
//! rename は別 PR）。各 field の doc は旧宣言に付いていたコメントの移設。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use super::on_sidebar::DaemonSettings;
use super::persist::Persist;
use crate::lane::conversation::LaneConversation;
use crate::lane::terminal::LaneTerminal;
use crate::pane::SidebarState;
use crate::settings::Settings;
use crate::webview::main_area::SlotRect;

/// event loop の可変 state（所有者 = `run()` の閉包）。
pub(super) struct UiState {
    /// VP-100 follow-up: 永続設定 + 1Password 風 開発者モード切替（vp-app.toml）。
    pub(super) settings: Settings,
    /// この instance の session file の所有者（復元 cursor + 保存の 1 箇所、doc 60 §8）。
    /// WindowBuilder より前に load 済（window geometry の復元）。
    pub(super) persist: Persist,
    /// VP-95: sidebar 全体 state (repos + widget + activity)。
    pub(super) sidebar_state: SidebarState,
    /// in-app update: 適用フロー実行中フラグ（GUI local）。ActivitySnapshot は health poll で
    /// 定期上書きされるため、event loop 側で保持して毎回 snapshot に再適用する。
    pub(super) update_applying: bool,
    /// R sidebar の debug log tail の世代カウンタ（sidebar view modes、2026-08-01）。
    /// watch / unwatch のたびに進め、旧世代の tail thread は次の poll で自然に退場する
    /// （= 最後の watch が勝つ単一 tail。join も channel 後始末も不要 — debug_log.rs 参照）。
    pub(super) debuglog_watch_gen: Arc<AtomicU64>,
    /// board pane の boot 窓救済（doc 52 §10 wave 0、device 一覧と同型）: gui channel で届いた
    /// BoardUpdated を repo × lane で保持し、`AppEvent::WebviewReady` の replay で再配信する。
    /// retained BoardUpdated は bundle 評価前に届いて受け口不在で落ちるため、これが無いと
    /// reopen 時に board pane が出ない（live show まで空）。
    pub(super) board_snapshots: HashMap<String, HashMap<String, serde_json::Value>>,
    /// VP-100 follow-up (1Password 風): runtime 開発者モード state。
    pub(super) dev_mode: bool,
    /// 直近に daemon から引けた settings.kdl の値（doc 59 P3）。
    /// 保持するのは、folder picker のように **vp-app.toml だけを触る操作**の後でも
    /// daemon 側の表示を消さないため（毎回引き直すと overlay が一瞬空欄になる）。
    /// None のままなら「まだ引けていない / 接続できない」。
    pub(super) last_daemon_settings: Option<DaemonSettings>,
    /// 二重発火・購読の dedup と変化検知の指紋。
    pub(super) guards: Guards,
    /// OS window / focus / dock icon / geometry 保存の throttle。
    pub(super) win: WindowState,
    /// lane ごとの接続（terminal / conversation の session 登録簿）。
    pub(super) sessions: LaneSessions,
}

/// 二重発火・購読の dedup と変化検知の指紋。⚠️ daemon 再起動では reset されない
/// （共有 connection の再接続と次の Loaded に追随する設計。doc 60 §8 の台帳）。
pub(super) struct Guards {
    /// repo auto-spawn: 1 セッションで同じ repo を二重 trigger しないための guard。
    /// path をキーにする (repo_name は重複しうる、 path は正規化済 unique)。
    /// daemon 側でも `Process already running` で弾かれるが、 無駄な POST を避ける。
    pub(super) repo_spawn_triggered: HashSet<String>,
    /// オンデマンド respawn: active にする lane が Dead (pid:null) の時に restart_lane を 1 回だけ
    /// 発火するための guard。 lane address をキーにする。 lane が Running に戻ったら (LanesLoaded で
    /// pid あり検出時) entry を解除し、 再度 Dead 化した時に再 respawn できるようにする。
    pub(super) lane_respawn_triggered: HashSet<String>,
    /// wiremsg Stage 1: per-repo の "lanes" Unison 購読を 1 本だけ張るための guard。
    /// path をキーにする。F1b: 購読は共有 connection に追従して give-up しないので、 一度
    /// spawn したら app 終了まで張りっぱなし (= guard から除去されない)。
    pub(super) lanes_sub_active: HashSet<String>,
    /// wiremsg Stage 2: per-repo の "gui" Unison 購読 guard (lanes_sub_active と同型)。
    pub(super) canvas_sub_active: HashSet<String>,
    /// doc 53 §11: lane ごとに **最後に webview へ渡した roster** の指紋。定期 snapshot で
    /// 同じ roster を撃ち直して pane を作り直さないための変化検知（`header_lane_fields_changed`
    /// と同じ「変化時のみ push」の規律）。
    ///
    /// ⚠️ 旧実装にはここに **取りこぼした fetch の保留箱**（`pending_session_fetch`）が在った。
    /// 供給が fetch 1 本だった時代、boot 直後の要求が repo 未解決で捨てられると
    /// 「pane も名札も出ない」になり、再試行の契機が無かったため箱で救っていた。
    /// 供給が snapshot（retained + 変化時 push）に一本化された今、取りこぼしという状態自体が
    /// 存在しない（doc 53 §6.5.2 が予言した「供給路を 1 本にすれば要らない」）。
    pub(super) last_roster_push: HashMap<String, String>,
}

/// OS window / focus / dock icon / geometry 保存の throttle。
pub(super) struct WindowState {
    /// 起動時 size clamp 用 once-flag。 macOS state restoration の `restorableState` は
    /// EventLoop 起動後の async phase で frame に反映され、 初回の `WindowEvent::Resized`
    /// として届く。 この flag が false のうちに来た Resized が「restoration 適用直後」と
    /// みなして min 制約と照合し、 必要なら force-resize する (#428 Moody Blues Issue #1)。
    /// PR #458 fix: 保存 geometry を復元した path では起動時 clamp を skip。
    /// 復元値 (with_inner_size apply 済) を macOS state restoration race 由来の小 size で
    /// 上書きしないため、 復元 path 中は最初の Resized event を「正常な user-driven resize」
    /// 扱いにする。 default path (= restored_geometry None) では従来通り clamp logic を走らせる。
    pub(super) initial_size_clamp_done: bool,
    /// 危険 D（doc 60 §8、b-6 = 実測）: 復元経路では最初の Resized を user 操作扱いで書き通す。
    /// その size が復元 geometry と違うか（= macOS の restorableState が別の frame を当てているか）を
    /// 1 nightly 分 log で観測する。`Some((w, h))` = boot 時の復元値、まだ最初の Resized を見ていない。
    /// ⚠️ 基準は boot で clone した値を持つ — `session.window_geometry()` は直前の `record_window`
    /// で live の frame に上書きされ得るので、log 時に読むと同語反復になる。
    pub(super) restore_first_resized: Option<(f64, f64)>,
    /// dock app icon (portal favicon) の再アサート用。 bare binary は .app bundle が無いため
    /// macOS が launch 完了時に generic icon を被せ、 run() 前 (window.build 直後) の
    /// setApplicationIconImage を上書きする。 event loop 開始後 ~1.5s 間 set_app_icon() を
    /// 呼び続けて (WaitUntil で loop を起こす) portal icon を定着させる。 .dmg 版は冪等。
    pub(super) icon_launch_at: Instant,
    /// 上記の settle 済 flag。
    pub(super) icon_settled: bool,
    /// Model B (focus = 操舵ポインタ): この vp-app instance が OS の key window かを追跡する。
    /// multi-window は別プロセス (VP_APP_INSTANCE = primary 0 / secondary N) なので、ROTO の
    /// switch_lane broadcast は全 instance の "canvas" 購読に届く。両 window が一斉に切り替わるのを
    /// 防ぐため、**focused instance だけ**が switch_lane を適用する (B-local self-filter)。
    /// with_focused(true) で起動するので初期値は true。
    pub(super) is_focused: bool,
    /// Model B #2: 直近 daemon に報告した active_lane。 focus が高速に flip しても
    /// 同じ lane への重複報告 (= reqwest::Client 新規構築 + 無駄 POST) を抑止する。
    pub(super) last_focus_reported_lane: Option<String>,
    /// VP-100 γ-light: pane_id → slot rect。Phase 2 では蓄積するだけ、Phase 4+ で
    /// native overlay の `set_position` 同期に使う。
    pub(super) slot_rects: HashMap<String, SlotRect>,
}

/// lane ごとの接続（daemon/conn を握る session の登録簿）。
pub(super) struct LaneSessions {
    /// terminal S4: per-lane terminal session registry (lane key → LaneTerminal)。
    /// LanesLoaded で live lane に対し start、 消えた lane / app 終了で stop (= map から remove)。
    pub(super) terminal_sessions: HashMap<String, LaneTerminal>,
    /// Conversation gui (doc 32): per-lane conversation session registry (lane key → LaneConversation)。
    /// terminal と違い demand-driven: ConversationSubmit の初回で lazy spawn (reconcile 非結合)。
    pub(super) conversation_sessions: HashMap<String, LaneConversation>,
}

impl UiState {
    /// boot 直後の初期 state。`persist` の session から currents_order の写しを取る。
    pub(super) fn new(
        settings: Settings,
        persist: Persist,
        initial_dev_mode: bool,
        restored_geometry: Option<(f64, f64)>,
    ) -> Self {
        // Phase 2.x-d: 旧 single-PTY 経路 (`xterm_ready` / `pending` / `PENDING_MAX`) は撤去。
        // per-Lane instance + browser-native WebSocket では各 Lane の xterm.js が独立に
        // WS から bytes を受けるので、 Rust 側で buffer / flush 同期する必要が無い。
        // SidebarState に currents_order を即反映 (renderRepos がこの順で並べる)
        let sidebar_state = SidebarState {
            currents_order: persist.session.currents_order.clone(),
            ..SidebarState::default()
        };
        Self {
            settings,
            persist,
            sidebar_state,
            update_applying: false,
            debuglog_watch_gen: Arc::new(AtomicU64::new(0)),
            board_snapshots: HashMap::new(),
            dev_mode: initial_dev_mode,
            last_daemon_settings: None,
            guards: Guards {
                repo_spawn_triggered: HashSet::new(),
                lane_respawn_triggered: HashSet::new(),
                lanes_sub_active: HashSet::new(),
                canvas_sub_active: HashSet::new(),
                last_roster_push: HashMap::new(),
            },
            win: WindowState {
                initial_size_clamp_done: restored_geometry.is_some(),
                restore_first_resized: restored_geometry,
                icon_launch_at: Instant::now(),
                icon_settled: false,
                is_focused: true,
                last_focus_reported_lane: None,
                slot_rects: HashMap::new(),
            },
            sessions: LaneSessions {
                terminal_sessions: HashMap::new(),
                conversation_sessions: HashMap::new(),
            },
        }
    }
}
