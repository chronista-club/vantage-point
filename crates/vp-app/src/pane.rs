//! Sidebar 表示用 data model (Architecture v4 Process recursive 移行版)
//!
//! 旧版 (~ 2026-04-26) では Mac 由来の `Pane` / `PaneKind` (Agent/Canvas/Preview/Shell) を
//! RepoPaneState 内に持っていた。 Architecture v4 (mem_1CaTpCQH8iLJ2PasRcPjHv) で
//! **repo `/api/lanes` が SSOT** になったので、 vp-app local の Pane data model は撤去し、
//! このファイルは sidebar の accordion 状態 + widget payload + active selection だけを
//! 保持する役に絞った。
//!
//! ## sidebar 描画
//!
//! - Repo (= Runtime Process) accordion: `RepoPaneState`
//! - Lane (= Session Process / Main/Sub): `SidebarState.lanes_by_repo` (repo fetch 結果)
//! - Agent (= Agent process / Conversation/Shell/...): Lane の中身として並列 row
//!
//! つまり Pane は廃止、 階層は **Repo → Lane → Agent** に統一。
//!
//! ## active selection
//!
//! `SidebarState.active_lane_address` で 1 つだけ active な Lane を持つ。
//! 形式は Lane address の Display 表現 (`"<repo>/root"` / `"<repo>/sub/<name>"`)。

use serde::{Deserialize, Serialize};

// v1.0 柱 2 PR-1: ts-rs で Rust → TS 型を生成 (sidebar bundle 用)。
// `#[cfg_attr(test, ...)]` で test build 時のみ `TS` derive を効かせ、
// `cargo test -p vp-app` で `webview/src/generated/*.ts` を export する。
// 生成 path 詳細 → `pane.rs::ts_rs_export` test module 参照。
#[cfg(test)]
use ts_rs::TS;

/// repo 単位の sidebar accordion 状態 (Architecture v4: Process kind=Runtime)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct RepoPaneState {
    /// 正規化パス (HashMap key 兼)
    pub path: String,
    pub name: String,
    /// accordion 開閉状態 (default = 閉じる)
    #[serde(default)]
    pub expanded: bool,
    /// Process state (running/dead/spawning 等、 daemon fetch から merge される)
    /// sidebar JS が state badge 表示に使う
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// repo の listen port (Phase 2: Lane terminal connect で使う)。
    /// running 時のみ Some、 dead 時は None。 RepoInfo.port を merge して保持。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl RepoPaneState {
    /// 新規 repo state (accordion は閉じた状態で生成)
    pub fn new(path: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            name: name.into(),
            expanded: false,
            state: None, // ReposLoaded handler で fetch 後 merge
            port: None,  // 同上
        }
    }
}

/// sidebar 上部 widget slot に表示する widget の種類
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub enum WidgetKind {
    /// D — Activity / Agent Status (MVP)
    #[default]
    Activity,
    /// B — MsgBox / Inbox (P4 で実装、placeholder)
    Msgbox,
    /// C — Notes / Scratchpad (P4 で実装、placeholder)
    Notes,
}

/// hub の向こうに居る available node 1 件（`/api/health` の `hub_nodes[]` 要素）。
///
/// daemon 側 `HubNodeInfo` と同形。deserialize（`/api/health` 受け）と serialize
/// （sidebar への push）の両面で使うため中間 mapping を持たない。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct HubNode {
    /// daemon の identity（hostname 由来、hub registry の一意キー相当）
    pub handle: String,
    /// 位置独立 routing key `nd_xxx`（hub S2 前は空 = daemon 側が omit するため default で受ける）
    #[serde(default)]
    pub node_id: String,
    /// direct 到達 endpoint 候補数（hub S2 前は 0）
    #[serde(default)]
    pub endpoints_count: usize,
    /// hub との常駐接続が今生きているか（hub protocol v0.6.0、relay registry snapshot 由来）。
    /// false = registry には居るが relay は offline（stale / 切断中）。旧 daemon / 旧 hub は false。
    #[serde(default)]
    pub connected: bool,
}

/// ACTIONS の 1 件（`/api/health` の `actions[]` 要素、doc 57 Phase 3）。
///
/// daemon 側 `CreoAction` と同形。deserialize（`/api/health` 受け）と serialize
/// （sidebar への push）の両面で使うため中間 mapping を持たない（[`HubNode`] と同じ流儀）。
///
/// ⚠️ `bucket` / `order` を enum や必須にしないのは、**creo の言い分をそのまま運ぶ**ため。
/// 空文字（creo の UI から手で tag を付けた memory 等）は webview の `normalizeActions` が
/// TODOs 末尾へ丸める — 正す場所を 1 箇所に閉じる。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct ActionItem {
    /// creo の memory id（`mem_xxx`）。Action の同一性はこれ 1 本。
    pub id: String,
    /// タイトル + 内容。1 行目がタイトル、2 行目以降が内容。
    #[serde(default)]
    pub text: String,
    /// 完了（creo の `status == "done"`）。
    #[serde(default)]
    pub done: bool,
    /// 区画（`metadata.vp.bucket`）。未設定は空文字。
    #[serde(default)]
    pub bucket: String,
    /// 区画内の並び（`metadata.vp.order`）。未設定は空文字。
    #[serde(default)]
    pub order: String,
}

/// Activity widget の payload
///
/// 5-10 秒間隔で Rust 側が `/api/health` (HTTP) + `repos/list` +
/// `registry.list` (Unison、doc 45 段 3) を fetch して更新、sidebar に push する。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct ActivitySnapshot {
    /// daemon 到達可否
    pub node_online: bool,
    /// `/api/health` から取得 (オフライン時 None)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub daemon_version: Option<String>,
    /// daemon の起動時刻 (ISO 8601、オフライン時 None)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub daemon_started_at: Option<String>,
    /// 登録 repo数
    pub repo_count: usize,
    /// 稼働中 process 数 (`registry.list`)
    pub running_repo_count: usize,
    /// chronista-hub federation 接続状態（`/api/health` の `hub`、Daemon 横に表示）。
    /// `"connected"` / `"connecting"` / `"disconnected"` / `"disabled"`、未取得 or 旧 daemon は空文字。
    #[serde(default)]
    pub hub: String,
    /// hub の向こうに居る available nodes（`/api/health` の `hub_nodes`、自 daemon 除外・dedup 済）。
    /// Hub 行の下に常時リスト表示する。未接続 / 旧 daemon は空。
    #[serde(default)]
    pub hub_nodes: Vec<HubNode>,
    /// hub 接続の credential 提示結果（`/api/health` の `hub_auth`）。
    /// `"credentialed"` / `"anonymous"`、未接続 / 旧 daemon は空文字。
    /// Hub 行の Login / Logout ボタン切替に使う（file でなく接続の真実）。
    #[serde(default)]
    pub hub_auth: String,
    /// 宛先ごとの credential 状態（`"hub"` / `"creo"` → `"valid"` | `"expired"` | `"none"`）。
    ///
    /// ⚠️ `hub_auth` と混同しないこと。あちらは **hub 接続がどう成立したか**なので hub を
    /// 切っていると何も分からない。こちらは local file の判定なので、**hub と無関係に
    /// 「creo にログイン済みか」が言える**（doc 57 Phase 2 の Creo ID 行が独立する根拠）。
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub auth_targets: std::collections::BTreeMap<String, String>,
    /// L1 lifecycle: repo presence map（repo path → `"connected"`|`"unregistered"`
    /// |`"unregistered"`、`/api/health` の `processes[]` 由来）。sidebar の repo 行が `proc.path`
    /// で引いて ●◐○ dot を描く。daemon-canonical（doc 27 §3.2 / Model Q）。
    #[serde(default)]
    pub presence: std::collections::HashMap<String, String>,
    /// in-app update: 新しい release が GitHub にあるか（`/api/health` の `update_available`）。
    /// sidebar Daemon widget の「更新する」ボタンの表示 gate。旧 daemon は false。
    #[serde(default)]
    pub update_available: bool,
    /// アイドルとみなすまでの分数（`/api/health` 由来、doc 59 P3）。
    /// **0 = 未取得**（旧 daemon / オフライン）で、webview 側が既定 5 分に倒す。
    #[serde(default)]
    pub idle_timeout_minutes: u64,
    /// 最新 release version（ボタン label「更新する ⤴ vX.Y.Z」用、未取得は None）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub latest_version: Option<String>,
    /// in-app update: 適用フロー実行中か。health 由来ではなく GUI local 状態
    /// （`AppEvent::UpdateFlowPhase`）で、「更新する」ボタンを「更新中…」表示に切り替える。
    #[serde(default)]
    pub update_applying: bool,
    /// ACTIONS（doc 57 Phase 3）— daemon が creo-memories から 30s poll で温めた一覧。
    /// sidebar 下部の ACTIONS 区画に出る。repo mode / 未取得 / 旧 daemon は空。
    #[serde(default)]
    pub actions: Vec<ActionItem>,
    /// ACTIONS の版。**内容が変わった時だけ**上がる。`0` = 未取得（旧 daemon もここに落ちる）。
    ///
    /// ⚠️ webview はこの値が**変わった時だけ**当てる。5s ごとの push で毎回当て直すと、
    /// 編集中の行が書き戻されて caret が飛ぶ。`u64` にしないのは ts-rs が `bigint` を吐いて
    /// JSON の number と噛み合わなくなるため。
    #[serde(default)]
    pub actions_rev: u32,
}

/// Sidebar 全体の state (sidebar webview に渡す)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct SidebarState {
    /// Runtime Process (= 旧 "repos") の list
    /// Architecture v4: mem_1CaTpCQH8iLJ2PasRcPjHv、JSON wire は serde alias で互換維持
    #[serde(alias = "projects")]
    pub processes: Vec<RepoPaneState>,
    /// 現在表示中の widget kind
    #[serde(default)]
    pub widget: WidgetKind,
    /// Activity widget payload (widget == Activity の時のみ有効)
    #[serde(default)]
    pub activity: ActivitySnapshot,
    /// repo_path → Lane list (repo `/api/lanes` から fetch)
    /// 関連 memory: mem_1CaSugEk1W2vr5TAdfDn5D (多 scope architecture)
    /// 起動時に再 fetch されるので disk persistence は実質意味薄いが、Serialize は維持
    #[serde(default)]
    pub lanes_by_repo: std::collections::HashMap<String, Vec<crate::client::LaneInfo>>,
    /// 現在 active な Lane の address (Display 形 `"<repo>/root"` 等)
    /// app 全体で 1 つだけ。 `lane:select` IPC で更新される。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_lane_address: Option<String>,
    /// doc 44 D4: repo_path → **開発起点 lane 名**（Repo Host の帳簿が解決した値）。
    ///
    /// `active_lane_address`（注視 = 今見ている lane）とは**別物**。D5 が明示的に分けており、
    /// 注視は click ごとに動くが起点は明示指定した時だけ動く。sidebar は起点 lane 行に
    /// star icon を出し、context menu から再指定できる。
    ///
    /// 値は lane **名**（address ではない）— 起点は repo ごとに 1 本なので
    /// `<repo>` 部分は key 側の path と冗長になる。
    ///
    /// ⚠️ `skip_serializing` にしてはいけない: この struct は webview への push payload
    /// でもあるので、skip すると Rust 側だけ更新されて sidebar に永久に届かない。
    /// 空の時だけ省く（`session_titles` 等と同じ扱い）。
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub origin_by_repo: std::collections::HashMap<String, String>,
    /// Phase 5-A: 現在 active な Repo-scope Agent kind
    /// (`"board"` / `"runner"` / `"devices"`)。
    /// `(repo_path, kind)` の tuple で repo ごとに区別。 app 全体で 1 つだけ active。
    /// `active_lane_address` と **mutually exclusive** ── どちらか一方が None。
    /// `stand:select` IPC で更新される。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_component: Option<ActiveComponent>,
    /// Currents セクションの repo 表示順 (path の順)。
    ///
    /// `SessionState.currents_order` から push される。 sidebar JS は Currents
    /// rendering 時に this list の順で並び替える (list に無い path は末尾、 daemon order 維持)。
    /// `None` なら順序指定なし (daemon registration 順)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currents_order: Option<Vec<String>>,
    /// Phase 5-D Sprint C P2.1: per-Lane HD notification unread count。
    /// Key: Lane address (Display 形 `"<repo>/root"` 等)、 Value: 未読 OSC 99 focus event 数。
    /// `OscNotification` event で increment、 `lane:select` で対応 Lane を 0 reset。
    /// disk persist 不要 (session 起動で 0 から)、 skip_serializing で軽量化。
    #[serde(default)]
    pub unread_notifications: std::collections::HashMap<String, u32>,
    /// Lane が cc の input 待ち状態かどうか (OSC 99 final-chunk 受信 = true)。
    /// Key: Lane address、 Value: true なら sidebar 行右端に黄 dot + pulse 表示。
    /// `OscNotification` event で `insert(addr, true)`、 `lane:select` で対応 Lane を `remove`。
    /// `unread_notifications` (履歴 count) と分離した「現在の活動状態」 表現。
    #[serde(default)]
    pub awaiting_input: std::collections::HashMap<String, bool>,
    /// Canvas (Board) 着信の per-Lane 未読 count (bug: canvas 可観測性 D)。
    /// Key: Lane address (`"<repo>/root"` 等)、 Value: 現在 active でない lane に
    /// show が着いた回数。 `CanvasMessage`(show) で increment、 `lane:select` (activate_lane) で
    /// 対応 Lane を 0 reset。 `unread_notifications` (HITL/OSC = 黄 dot) とは**別 sink** =
    /// sidebar で Canvas 専用 icon (Phosphor easel) を出し「用事」と「絵が届いた」の語彙を分ける。
    /// disk persist 不要 (起動で 0)。
    #[serde(default)]
    pub canvas_unread: std::collections::HashMap<String, u32>,
    /// VP-143: per-Lane の cc session display name (`/rename` で設定された custom-title)。
    /// Key: Lane address、 Value: title 文字列。
    /// `~/.claude/projects/<encoded-cwd>/<latest>.jsonl` の `custom-title` entry を polling
    /// で抽出して populate (`session_title` module + `session_title_poller`)。
    /// `/rename` 未実行 lane は entry なし → JS 側で branch 名 fallback。
    /// disk persist 不要 (起動時に再 resolve)、 skip_serializing で軽量化。
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub session_titles: std::collections::HashMap<String, String>,
    /// VP-147 PR-P2-3: per-Lane の mailbox inbox 状況。
    /// Key: Lane address (Display 形 `"<repo>/root"`)、 Value: [`MessageState`]。
    /// `spawn_lane_inbox_poller` (5s 間隔) が `AppEvent::ResolveLaneInboxes` を発火、
    /// main thread が active Lane に対して MessageState を populate する。
    /// JS 側は entry が存在する Lane に `.vp-message-icon` を render (Conversation icon の右隣)。
    /// disk persist 不要、 skip_serializing で軽量化。
    /// Phase 2 PR-P2-3 では unread_count / has_persistent / last_msg_ts は default 値、
    /// icon visibility のみ用途。 actual peek 値は後続 PR で backend API 経由で populate。
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub lane_inboxes: std::collections::HashMap<String, MessageState>,
    /// DeviceRegistry 🧲 接続中 device 一覧 (`devices.device_connected` / `disconnected` で更新)。
    /// JS 側は DeviceRegistry pane に device list を render する。 disk persist 不要 (起動時 0)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<DeviceSnapshot>,
    /// doc 30 §5-3 / lanes 購読 self-heal: per-repo の Daemon "lanes" channel 購読フェーズ。
    /// Key: repo_path。 Value は 3 値モデル (entry 有無 + 2 文字列):
    ///
    /// - entry なし (absent) = 初期 (購読開始〜初回 snapshot 未受信)。 `hintFor` は `📡 loading lanes…`
    /// - `"stalled"` = open / subscribe / 初回 snapshot が timeout (Daemon lanes channel 無応答 or QUIC 未接続)。 `LanesError` で挿入。 `hintFor` は `⚠️ lane 接続が停滞 — daemon restart で復帰`
    /// - `"ready"` = snapshot を 1 度でも受信 (`LanesLoaded` で挿入、 以後 entry は残る)。 lane 0 本なら `hintFor` は `📡 lane なし`
    ///
    /// stall→ready は復帰時の snapshot で上書きされ自動解消 (self-heal と連動)。 起動時は全 repo entry
    /// なしなので `skip_serializing_if = is_empty` で初期は wire に出ない (disk 非永続なので実害なし)。
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub lane_sub_state: std::collections::HashMap<String, String>,
}

/// per-Lane mailbox inbox の summary state (VP-147 PR-P2-3)
///
/// sidebar UI で `.vp-message-icon` 表示と tooltip rendering に使う minimal struct。
/// Phase 2 PR-P2-3 (icon visibility) では default 値で populate、 actual 値は後続 PR で
/// backend peek API (`Router::inbox_state(actor, lane)`) を実装して populate する。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct MessageState {
    /// 未読 message 数 (= inbox queue 内の未消費 msg count)
    /// Phase 2 PR-P2-3 では default 0、 後続 PR で backend counter から populate。
    #[serde(default)]
    pub unread_count: u32,
    /// 永続化メッセージの存在 (= 永続 store に persist された未消費 msg があるか)
    /// Phase 2 PR-P2-3 では default false、 後続 PR で永続 store query から populate。
    #[serde(default)]
    pub has_persistent: bool,
    /// 最新 msg の timestamp (ISO 8601、 Display 用)
    /// Phase 2 PR-P2-3 では default None、 後続 PR で history tracker から populate。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_msg_ts: Option<String>,
}

/// Phase 5-A: Repo-scope component の active selection (sidebar の row click で発火)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct ActiveComponent {
    pub repo_path: String,
    /// `"board"` | `"runner"` | `"devices"`
    pub kind: String,
}

/// DeviceRegistry 🧲 接続中 device の snapshot (sidebar webview に渡す)。
/// `devices.device_connected` / `disconnected` event で `apply_device_event` が更新する。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct DeviceSnapshot {
    /// CoreMIDI port の displayName (registry の一意キー)
    pub port_name: String,
    pub has_input: bool,
    pub has_output: bool,
    /// VP が今この port を**掴んでいるか**（艦隊スイッチ / parser 対応 / ROTO 常駐の合成）。
    /// 旧 daemon は field 不在 → false（安全側 = 掴んでいない扱い）。
    #[serde(default)]
    pub held: bool,
    /// 掴んでいない理由。`"released"`（`vp midi off` で譲渡中）/ `"unsupported"`（VP に parser が
    /// 無い = 最初から取り合っていない）/ `"idle"`。
    ///
    /// ⚠️ **bool に潰さない** — 「VP が邪魔している」と「元々関与していない」は user の次の
    /// 一手が正反対になる（前者は `vp midi on`、後者は何もしなくていい）。
    #[serde(default)]
    pub hold_reason: String,
    /// 最後にこの機材が**触られた**時刻（ISO 8601、秒精度）。`None` = 観測していない。
    ///
    /// ⚠️ **掴んでいない間は原理的に分からない**（listener が無い = 入力が届かない）。
    /// 譲渡中に「触られていない」と表示すると嘘になるので、`held == false` の間は
    /// 更新されないまま古い値が残る — 描画側はそれを「—（見ていない）」として扱う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_input_at: Option<String>,
}

/// device event payload (wire `DeviceEvent` の生 JSON、 tag="kind") を device 一覧に適用する
/// 純粋計算。 変更があれば true (= caller が push_sidebar_state する)。
///
/// - `device_connected`: 同名 port を置換して追加 (重複させず has_input/output を最新化)
/// - `device_disconnected`: 同名 port を削除 (対象 port 不在は変更なしで false)
/// - それ以外 (`control_event` 等) / port_name 欠落: 無視 (false)
pub fn apply_device_event(devices: &mut Vec<DeviceSnapshot>, payload: &serde_json::Value) -> bool {
    match payload.get("kind").and_then(|v| v.as_str()) {
        Some("device_connected") => {
            let Some(port_name) = payload.get("port_name").and_then(|v| v.as_str()) else {
                return false;
            };
            // ⚠️ 置換の前に「触られた時刻」を拾っておく。retain-then-push は snapshot 全体を
            // 作り直すので、持ち越さないと **hold 状態が変わるたびに操作履歴が消える**
            // （スイッチを 1 回叩くだけで全機材の「最後に触った」が飛ぶ）。
            let carried = devices
                .iter()
                .find(|d| d.port_name == port_name)
                .and_then(|d| d.last_input_at.clone());
            devices.retain(|d| d.port_name != port_name);
            devices.push(DeviceSnapshot {
                port_name: port_name.to_string(),
                has_input: payload
                    .get("has_input")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                has_output: payload
                    .get("has_output")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                held: payload
                    .get("held")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                hold_reason: payload
                    .get("hold_reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                last_input_at: carried,
            });
            true
        }
        // 機材が触られた（knob / pad / fader）。**秒精度で刻み、同じ秒なら push しない** —
        // ROTO の knob は連続 CC なので、素通しすると 1 操作で数十回 SidebarState を撃つ。
        Some("control_event") => {
            let Some(port_name) = payload.get("port_name").and_then(|v| v.as_str()) else {
                return false;
            };
            let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
            let Some(dev) = devices.iter_mut().find(|d| d.port_name == port_name) else {
                return false;
            };
            if dev.last_input_at.as_deref() == Some(now.as_str()) {
                return false; // 同じ秒 = 既に出ている情報。撃たない
            }
            dev.last_input_at = Some(now);
            true
        }
        Some("device_disconnected") => {
            let Some(port_name) = payload.get("port_name").and_then(|v| v.as_str()) else {
                return false;
            };
            let before = devices.len();
            devices.retain(|d| d.port_name != port_name);
            devices.len() != before
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_state_starts_collapsed() {
        let p = RepoPaneState::new("/path", "demo");
        assert_eq!(p.path, "/path");
        assert_eq!(p.name, "demo");
        assert!(!p.expanded);
        assert!(p.state.is_none());
    }

    #[test]
    fn sidebar_state_serializes_round_trip() {
        let mut s = SidebarState::default();
        s.processes.push(RepoPaneState::new("/a", "alpha"));
        s.activity.node_online = true;
        s.activity.repo_count = 1;
        s.active_lane_address = Some("alpha/root".into());
        let json = serde_json::to_string(&s).unwrap();
        let parsed: SidebarState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.processes.len(), 1);
        assert!(parsed.activity.node_online);
        assert_eq!(parsed.widget, WidgetKind::Activity);
        assert_eq!(parsed.active_lane_address.as_deref(), Some("alpha/root"));
    }

    #[test]
    fn sidebar_state_accepts_legacy_projects_alias() {
        // 旧 disk persistence (key="projects") から読めること
        let json = r#"{"projects":[{"path":"/x","name":"x","expanded":true}]}"#;
        let parsed: SidebarState = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.processes.len(), 1);
        assert_eq!(parsed.processes[0].path, "/x");
        assert!(parsed.processes[0].expanded);
    }

    /// v1.0 柱 2 PR-1: ts-rs export が `webview/src/generated/` に `.ts` を吐くこと。
    ///
    /// `#[ts(export)]` を付けた型は ts-rs が自動 export test を生成するので、
    /// `cargo test -p vp-app` を回せば binding は必ず最新化される。 本 test は
    /// 「sidebar push の SSOT 型 = `SidebarState`」 の export 結果が実在することの
    /// regression guard ── 出力先 path が壊れたら即座に気付ける。
    #[test]
    fn ts_rs_export_sidebar_state_generates_file() {
        // ts-rs の自動生成 export test を明示的に呼んでファイルを書き出す。
        // ts-rs 12 で `export_all` に Config が必須化。`from_env()` で `.cargo/config.toml` の
        // `TS_RS_EXPORT_DIR`（= crates/vp-app）を効かせ、自動 export test と同じ出力先に揃える。
        <SidebarState as TS>::export_all(&ts_rs::Config::from_env())
            .expect("SidebarState binding export 失敗");
        let manifest = env!("CARGO_MANIFEST_DIR");
        let generated =
            std::path::Path::new(manifest).join("webview/src/generated/SidebarState.ts");
        assert!(
            generated.exists(),
            "SidebarState.ts が生成されていない: {}",
            generated.display()
        );
    }

    #[test]
    fn apply_device_event_connect_disconnect() {
        use serde_json::json;
        let mut devs: Vec<DeviceSnapshot> = Vec::new();
        // connect → 追加
        assert!(apply_device_event(
            &mut devs,
            &json!({"kind":"device_connected","port_name":"ROTO","has_input":true,"has_output":true})
        ));
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].port_name, "ROTO");
        // 同名 connect → 重複させず置換 (has_output が更新される)
        assert!(apply_device_event(
            &mut devs,
            &json!({"kind":"device_connected","port_name":"ROTO","has_input":true,"has_output":false})
        ));
        assert_eq!(devs.len(), 1);
        assert!(!devs[0].has_output);
        // 別 device → 追加
        assert!(apply_device_event(
            &mut devs,
            &json!({"kind":"device_connected","port_name":"LPD8","has_input":true,"has_output":false})
        ));
        assert_eq!(devs.len(), 2);
        // disconnect → 削除
        assert!(apply_device_event(
            &mut devs,
            &json!({"kind":"device_disconnected","port_name":"ROTO"})
        ));
        assert_eq!(devs.len(), 1);
        assert_eq!(devs[0].port_name, "LPD8");
        // 不在 disconnect → 変更なし (false)
        assert!(!apply_device_event(
            &mut devs,
            &json!({"kind":"device_disconnected","port_name":"NOPE"})
        ));
        // control_event → 「最後に触られた」を刻む（旧: 無視）。
        assert!(apply_device_event(
            &mut devs,
            &json!({"kind":"control_event","port_name":"LPD8","event":{}})
        ));
        assert!(devs[0].last_input_at.is_some(), "触られた時刻が入る");
        // ⚠️ **同じ秒では撃たない**。ROTO の knob は連続 CC なので、素通しすると
        // 1 操作で SidebarState を数十回 push することになる。
        assert!(
            !apply_device_event(
                &mut devs,
                &json!({"kind":"control_event","port_name":"LPD8","event":{}})
            ),
            "同一秒の再入力は変化として扱わない"
        );
        // 未知 port の control_event は無視（一覧に居ないものは刻めない）
        assert!(!apply_device_event(
            &mut devs,
            &json!({"kind":"control_event","port_name":"NOPE","event":{}})
        ));
        // port_name 欠落 → 無視 (false)
        assert!(!apply_device_event(
            &mut devs,
            &json!({"kind":"device_connected"})
        ));
    }

    /// ⚠️ **hold 状態の更新で「最後に触られた」を消さない**。
    /// `device_connected` は retain-then-push で entry を作り直すので、持ち越しを忘れると
    /// スイッチを 1 回叩くだけで全機材の操作履歴が飛ぶ。
    #[test]
    fn hold_state_update_keeps_last_input() {
        use serde_json::json;
        let mut devs = vec![];
        assert!(apply_device_event(
            &mut devs,
            &json!({"kind":"device_connected","port_name":"LPD8","has_input":true,"has_output":true,"held":true,"hold_reason":"listener"})
        ));
        assert!(apply_device_event(
            &mut devs,
            &json!({"kind":"control_event","port_name":"LPD8","event":{}})
        ));
        let touched = devs[0].last_input_at.clone();
        assert!(touched.is_some());

        // `vp midi off` → hold 状態だけが変わる再配信
        assert!(apply_device_event(
            &mut devs,
            &json!({"kind":"device_connected","port_name":"LPD8","has_input":true,"has_output":true,"held":false,"hold_reason":"released"})
        ));
        assert_eq!(devs[0].hold_reason, "released");
        assert!(!devs[0].held);
        assert_eq!(devs[0].last_input_at, touched, "操作履歴は持ち越す");
    }
}
