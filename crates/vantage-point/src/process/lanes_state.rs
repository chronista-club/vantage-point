//! Lane state types — SP が持つ Lane (Conductor/Performer) の data model
//!
//! 関連 memory:
//! - `mem_1CaSrCxysdGaaSsN4Dvxth` (VP Architecture: 3 段 Stand scope + Lane semantic)
//! - `mem_1CaSsN7xj69aVQtLPQFJxQ` (SP-as-Project-Master: 9 component minimum)
//! - **2026-04-27 rule** (旧):「Lane scope に attach するのは HD と TH のみ。PP/GE/HP は Project scope」
//!   → **doc 12 LSCM (VP-109、 2026-05-04) で明示的に supersede**。 LSCM では Layer container
//!   (World / Project / Lane) が必要な Stand を抱える composition モデルで、 各 Stand の居住可能
//!   Layer は doc 12 §9 catalog の「保持 layer pattern」 列が SSOT。
//! - PR-pre2 (VP-118 / 2026-05-04): HD → Echoes rename。
//! - PR-β-2 (VP-120 / 2026-05-04): PP を Project → Lane に物理移管 (`LaneCapabilities.paisley_park`)。
//! - PR-δ-2 (VP-136 / 2026-05-06): PP を `LaneStandRegistry` 経由 host へ rewire (`LaneCapabilities.registry`)。
//!
//! ## architecture (LSCM 確定 + PR-δ-2 後)
//!
//! Lane scope に host する Stand:
//! - Echoes 💬 (旧 HD) — Lane mise task PtySlot で立つ (= LaneCapabilities では host しない)
//! - The Hand 🤚 — Lane mise task PtySlot で立つ (= 同上)
//! - Paisley Park 🧭 — `LaneCapabilities.registry` 内 PaisleyParkStand (PR-δ-2 で trait-based host へ rewire、 Lane あたり 1 instance)
//! - Gold Experience 🌿 (planned PR-γ で Lane 移管予定、 LaneStand impl 追加)
//!
//! Project scope の Stand pool (`project_stands_state.rs`) は GE / HP のみ host (PR-β-2 後)。
//! Lane は **Conductor/Performer の PTY セッション + Stand container** に集中:
//! - Conductor 1 / project (固定)、stand = "echoes" / "shell" / "tmux"
//! - Performer 0..n / project (可変、lane clone)、stand 同上
//!
//! ## Phase A4-2b スコープ
//!
//! `LanePool::with_conductor` で Conductor Lane 1 つ pre-populate。
//! Performer create / destroy / Stand 切替は A4-4 / A5 で実装。

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Lane の位置独立な安定 id (I1、 doc 24 §7 / §10 Phase 2)。
///
/// path / port / PID に依存しない不変 handle。Lane の cwd が動こうと project が
/// rename されようと、 この id は変わらない (= 発端バグの path=identity を断つ種)。
///
/// **strangler 注意**: 現状この id は **pool key には使わない** (operative key は
/// [`LaneAddress`])。「id を持つが id で引かない」中間状態 — 後続 increment で徐々に
/// id へ寄せる土台。生成・永続は [`crate::lane::lane_id`]。
///
/// **format は意図的に opaque** (doc §12-E: format / 採番 / 衝突解決は連邦時 = Phase 3
/// まで決め打ちしない)。現状 UUID v7 (時刻順 sortable) で生成するが、 呼び手は中身に
/// 依存しないこと。serde は `transparent` で素の文字列として乗る (人にも読める wire)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LaneId(String);

impl LaneId {
    /// 新規 id を生成する (現状 UUID v7、 format は opaque)。
    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 空 id (legacy wire payload を `#[serde(default)]` で受けた時の値) 判定。
    /// `skip_serializing_if` と組で「空なら wire から省略」= 古 client と完全互換。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for LaneId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for LaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Lane の種別 (memory rule: HD/TH を起動する Lane だけ)
///
/// **互換注意 (conductor/performer rename 2026-06-07)**: serde は新名 `"conductor"`/
/// `"performer"` のみ受理する。旧名 `"lead"`/`"wing"` の後方互換受理は
/// [`LanePool::parse_address`] (address string path) と vp-app の `From<&LaneAddressWire>`
/// (wire IPC path) に**限定**される (LanePool は in-memory only で JSON 永続化しないため
/// この型直接の deserialize 経路は無い)。将来この型を JSON 永続化する経路を新設する場合は
/// `#[serde(alias = "lead")]` 等を追加すること。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneKind {
    /// 1 / project (固定)、LaneStand = HD or TH
    Conductor,
    /// 0..n / project (可変、lane cloned worktree)、LaneStand = HD or TH。
    Performer,
}

impl fmt::Display for LaneKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaneKind::Conductor => write!(f, "conductor"),
            LaneKind::Performer => write!(f, "performer"),
        }
    }
}

// `LaneStand` enum は doc 11 (PR-B) で削除。 stand 識別子は `String` に統一
// (例: "echoes" / "shell")。 tmux decoupling PR2 で stand script 層 (mise task) も廃止され、
// stand は `stand_spawner::build_stand_command` の Rust-native 分岐になった。
//
// wire format の legacy 名は `process::routes::lanes::migrate_legacy_stand` で
// 1 release の deprecation 期間 shim 経由で吸収 ("heavens_door" → "hd"、 "the_hand" → "shell")。
//
// `TmuxMode` / `TmuxLaneAddress` (Phase 1a の tmux session registry) は tmux decoupling PR2 で
// 退役 — lane の identity は `LaneAddress` ただ一つ、 process host は PtySlot ただ一つ
// (design doc §13)。

/// Lane の state machine 状態 (Phase A4-2b では Running 固定で pre-populate)
///
/// 注意: 「lane disk dir 存在 + Pane 不在」 は **Lane state ではなく `pid: None` で表現する** 設計。
/// Active/Inactive 概念は Project 集約 (sidebar 側 client-side computed) として扱い、 Lane state には混ぜない。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneState {
    Spawning,
    #[default]
    Running,
    Exiting,
    Dead,
}

/// Lane の **durable lifecycle** (doc 24 §4.6 — daemon 堅牢化の軽量 WAL)。
///
/// process liveness ([`LaneState`]) とは **別軸**: ground (worktree) の生成/破棄の lifecycle を
/// daemon-internal に追跡する (PtySlot の生死ではない)。 daemon-canonical で、 descriptor とは
/// 別 table (`lane_lifecycle`) に永続する (SP push が descriptor を round-trip して clobber する
/// のを避けるため)。
///
/// **intent-first bracket**: create は `Provisioning` を先に書く → worktree provision → `Ready`。
/// crash で `Provisioning` が残れば boot reconcile が ground 存在で heal (`Ready` or `Dead`)。
/// destroy-side (`Destroying`) は後続 increment。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneLifecycle {
    /// ground を provision 中 (intent 記録済、 external op in-flight)。
    Provisioning,
    /// ground 準備完了 (= 通常状態)。
    #[default]
    Ready,
    /// 失敗 / 外部削除で回収待ち (保持: inspection / `--resume` 可、 ground は当面残す)。
    Dead,
}

impl LaneLifecycle {
    /// db 永続用の文字列表現。
    pub fn as_str(&self) -> &'static str {
        match self {
            LaneLifecycle::Provisioning => "provisioning",
            LaneLifecycle::Ready => "ready",
            LaneLifecycle::Dead => "dead",
        }
    }

    /// db 文字列からの復元 (未知/`ready` は `Ready` に倒す = 安全側)。
    pub fn parse(s: &str) -> Self {
        match s {
            "provisioning" => LaneLifecycle::Provisioning,
            "dead" => LaneLifecycle::Dead,
            _ => LaneLifecycle::Ready,
        }
    }
}

/// Lane の address — Pool key
///
/// 表示形 (`Display` 実装):
/// - Conductor: `"<project>/conductor"`         例: `"vp/conductor"`
/// - Performer: `"<project>/performer/<name>"`  例: `"vp/performer/foo"`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LaneAddress {
    pub project: String,
    pub kind: LaneKind,
    /// Performer のみ Some (人間可読、例: "foo")。Conductor は None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl LaneAddress {
    pub fn conductor(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            kind: LaneKind::Conductor,
            name: None,
        }
    }

    pub fn performer(project: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            kind: LaneKind::Performer,
            name: Some(name.into()),
        }
    }

    // `tmux_session_name` / `tmux_session_prefix` (Phase 1a の deterministic tmux 名導出) は
    // tmux decoupling PR2 で退役。 lane の identity は Display 形 (`<project>/conductor` 等)
    // ただ一つ (design doc §13.2 — sanitize 形は tmux の「`/` 禁止」制約由来だった)。
}

impl fmt::Display for LaneAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.kind, &self.name) {
            (LaneKind::Conductor, _) => write!(f, "{}/conductor", self.project),
            (LaneKind::Performer, Some(n)) => write!(f, "{}/performer/{}", self.project, n),
            (LaneKind::Performer, None) => write!(f, "{}/performer/<unnamed>", self.project),
        }
    }
}

/// Phase 2 (Step E): エンティティ lifecycle の diff event を表現する generic ADT。
///
/// - `I` = identifier 型 (削除時のみ必要、 例: `LaneAddress`)
/// - `P` = payload 型 (add/update 時の full state、 例: `LaneInfo`)
///
/// SP の caller で event 発生 → AppState の broadcast channel に publish →
/// `spawn_world_uplink` の subscriber が QUIC registry channel で TheWorld に push、
/// 各 cache を realtime sync する primitive。
///
/// wire format: internally tagged JSON
/// ```json
/// {"kind": "add", "payload": {...}}
/// {"kind": "remove", "id": {...}}
/// {"kind": "update", "payload": {...}}
/// ```
///
/// QUIC channel は ordered (single connection) なので、 register snapshot → diff の順序保証あり。
/// 将来 `Diff<PaneId, PaneInfo>` / `Diff<StandKind, StandInfo>` 等の type alias で reuse 可能。
///
/// 関連 memory: Phase 1 完成 (mem_1Cac2YvnAhaVRCJemidtkx) の「残作業: Phase 2 Step E」に該当。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Diff<I, P> {
    /// 新規追加 (Lane spawn 完了 / Pane create 等、 full payload で挿入)
    Add { payload: P },
    /// 削除 (Lane destroy / Pane close 等、 id のみで identify)
    Remove { id: I },
    /// 更新 (state 変更 / pid 更新 / restart 完了 等、 full payload で replace)
    Update { payload: P },
}

/// Phase 2: Lane lifecycle 用の Diff alias。 SP の lane_pool 変更を TheWorld に伝える。
pub type LaneDiff = Diff<LaneAddress, LaneInfo>;

/// Phase 2 (Step E): SP の system 系 lifecycle event を 1 つの broadcast bus で配信。
///
/// caller (lane_spawn_actor / routes/* / lifecycle monitor / restart_lane 等) が
/// `state.system_event_tx.send(SystemEvent::*)` で publish、
/// `spawn_world_uplink` subscriber が QUIC registry channel 経由で TheWorld に流す。
///
/// scope ごとに variant 分け、 内部に該当 Diff を内包。 将来 Pane / Stand 等は
/// variant 追加で扱える central event bus pattern (Erlang event manager 風)。
///
/// wire format: internally tagged JSON で、 内側は Diff の `kind` も二重 tag:
/// ```json
/// {"scope": "lane", "kind": "add", "payload": {...}}
/// {"scope": "lane", "kind": "remove", "id": {...}}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
pub enum SystemEvent {
    /// Lane lifecycle diff (Phase 2 Step E)
    Lane(LaneDiff),
    // 将来 variant 追加候補:
    //   Pane(Diff<PaneId, PaneInfo>),       // Phase 7 (Pane Revival)
    //   Stand(Diff<StandKind, StandInfo>),  // 各 Stand の lifecycle
    //   Process(Diff<ProcessKey, RunningProcess>),  // Process registry diff
}

/// Lane の info (REST response 用 + 内部 registry の値)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneInfo {
    /// I1 (doc 24 §7): 位置独立な安定 id。生成・永続は [`crate::lane::lane_id`]。
    /// **まだ pool key には使わない** (operative key は `address`)。strangler の種。
    /// 旧 wire payload (id 欄なし) は `#[serde(default)]` で空 [`LaneId`] になり、
    /// `skip_serializing_if` で再び省略される (= 古 client と完全互換)。
    #[serde(default, skip_serializing_if = "LaneId::is_empty")]
    pub id: LaneId,
    pub address: LaneAddress,
    pub kind: LaneKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub state: LaneState,
    /// Stand 名 (例: "hd" / "shell" / "tmux"、 doc 11 PR-B で String に変更)
    pub stand: String,
    /// ISO 8601
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub cwd: String,
    /// Phase 5-D: Performer のみ embed (Conductor は git workspace を持たない設計)。
    /// `cwd` から `lane::commands::performer_status()` を呼んで populate。
    /// `/api/lanes` 応答時に lazy 取得 (registry には保存しない、 git 状態は volatile)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performer_status: Option<crate::lane::commands::PerformerStatus>,
    /// R3-b → doc 39 §3-1: この lane の **root session** の CC session id（wire 配送は常に
    /// root = lane の人格に解決する）。 registry には保存せず `/api/lanes` 応答時に root
    /// session の state file (`lane::cc_session`、 書き手は SessionStart/UserPromptSubmit hook)
    /// を lazy read する (`performer_status` と同じ前例)。 echoes の `--resume` 再利用と
    /// R3-c の `--bg` session 管理の土台。
    ///
    /// ⚠️ **claude 専用の契約**: delivery_actor（channel D）が `claude -p --resume <id>` に
    /// 使うため、他 engine の id を入れてはならない（root が非 claude session の場合、その
    /// label の cc_session store には書き手がいないため自然に None になる）。表示用の
    /// engine 横断 id は [`Self::engine_session_id`]（別契約）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc_session_id: Option<String>,
    /// doc 37: この lane の **active engine の** session id（claude=cc_session / cursor=chatId /
    /// codex=thread id。agy / shell は None）。Echoes 共通ヘッダの session chip 用（表示専用 —
    /// resume に使うのは registry の会話 id / `cc_session_id` 側）。doc 40: 供給は registry
    /// （root session の conversation）に一本化。serde default + skip で wire 後方互換。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_session_id: Option<String>,
    /// doc 40 §3: lane の session 構造（registry snapshot — focused / root /
    /// sessions[{key, stand, conversation}]）。LaneInfo を「lane の完全な descriptor」に
    /// する一歩（cwd は既在、sessions が最後の外付けだった）— chip とタブの供給を同一
    /// snapshot に揃える土台。populate は [`Self::refresh_engine_session_id`]（enrich 供給点）。
    /// serde default + skip で wire 後方互換。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<crate::lane::session_registry::SessionRegistry>,
    /// doc 33: Console のエンジンモード（Tui = PtySlot+claude TUI / Chat = EchoesAgentHost）。
    /// serde default = Tui で wire 後方互換。永続 SSOT は `lane::console_mode`（state file）、
    /// 本 field はその registry cache。vp-app は本 field で Dead-lane respawn 判定を gate する
    /// （chat lane の engine-less は正常状態 — #683 再演防止）。
    #[serde(default)]
    pub console_mode: crate::lane::console_mode::ConsoleMode,
    /// FSM 投影 (2026-07-11): dev-flow FSM (`flow::derive_flow_state`) の現在 state。
    /// **TheWorld が vp-app への snapshot 送信時に enrich する derive 値** — SP / lane_registry /
    /// db では常に `None` (derive できるものは store しない原則)。 source は wire store
    /// (latest msg + 未 ack needs_user) + performer_status で、 `vp flow progress` と同一判定。
    /// serde default + skip で旧 SP / 旧 client と wire 完全互換。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_state: Option<crate::flow::FlowState>,
}

impl LaneInfo {
    /// doc 37: active engine の session id を state file から lazy read して埋める
    /// （Echoes 共通ヘッダの session chip 用。表示専用の別契約 — [`Self::cc_session_id`] は
    /// claude resume 用でここでは触らない）。engine 対応表は `EngineKind` が SSOT。
    ///
    /// ⚠️ **lanes が World へ流れる供給点すべてで呼ぶこと**: ①`build_lanes_snapshot`
    /// （ask 経路 = MCP list_lanes / lanes_list）②uplink の agent_card（register payload）
    /// ③uplink の LaneDiff push（lanes/add|update）。供給が複数経路あるのは #683 と同じ地形で、
    /// 1 箇所だけ enrich すると「ask には出るが registry（= vp-app）には出ない」に化ける
    /// （2026-07-16 の Act I session chip 不点灯の根因）。1 lane 2 file read
    /// （session registry + session store、いずれも数百 byte）で軽微。
    pub fn refresh_engine_session_id(&mut self) {
        let lane_label = crate::process::stand_spawner::lane_label(&self.address);
        // doc 40 §5: 会話 id の SSOT = session registry を 1 回 load し、
        // - `engine_session_id`（chip）= root session の conversation（doc 39 P1: chip は
        //   lane の人格を映す。Act II のタブ表示は per-session 値 = #796 が担う）
        // - `cc_session_id`（channel D の claude 専用契約）= root が claude の時だけ同値
        //   （他 engine の id を混ぜない — 旧 field doc の不変条件を維持。旧実装の
        //   build_lanes_snapshot 個別 enrich は本 method に畳んだ = 供給点の実装差解消）
        // - `sessions` = registry snapshot 丸ごと（LaneInfo descriptor 完成、doc 40 §3）
        // registry file 不在（N=1 特殊ケース）は root=1 で従来と同一の読み先になる。
        // 旧 engine 別 store の 3-way dispatch は load 内の backfill bridge に移った。
        let reg =
            crate::lane::session_registry::load(&self.address.project, lane_label, &self.stand);
        let root = reg.sessions.iter().find(|s| s.key == reg.root);
        self.engine_session_id = root.and_then(|s| s.conversation.clone());
        self.cc_session_id = root
            .filter(|s| {
                matches!(
                    crate::echoes::EngineKind::from_stand(&s.stand),
                    Some(crate::echoes::EngineKind::Claude)
                )
            })
            .and_then(|s| s.conversation.clone());
        self.sessions = Some(reg);
    }
}

/// Lane Pool — Conductor/Performer registry
///
/// memory rule: Lane scope は HD/TH 専用。Project scope の Stand は別 module。
///
/// **A5-2 (mem_1CaTpCQH8iLJ2PasRcPjHv Architecture v4)**:
/// `pty_slots` で実 PTY (PtySlot) を保持。 Lane spawn 時に `stand_spawner::build_stand_command`
/// + `PtySlot::spawn` で実 process 起動、 結果を保持。 Drop で child process kill 保証。
#[derive(Default)]
pub struct LanePool {
    lanes: HashMap<LaneAddress, LaneInfo>,
    /// A5-2: 各 Lane の実 PtySlot (子 process と PTY を保持)
    /// spawn 失敗 / 未 spawn の Lane は entry なし (state=Dead で record される)
    /// `Mutex` wrap は PtySlot が Send-only (内部 Box<dyn Write+Send> 等) で Sync でないため、
    /// AppState が `Arc<RwLock<LanePool>>` で thread-shared に必要
    pty_slots: HashMap<LaneAddress, std::sync::Mutex<crate::daemon::pty_slot::PtySlot>>,
    /// Stage 1 (ADR-0001): 各 Lane の Rust 側 alacritty Term<T> attach。
    /// pty_slots と lifecycle 同期: with_conductor で spawn、 remove で drop abort。
    /// task は spawn_blocking で 1 Lane = 1 task、 broadcast::Receiver を消費。
    /// MVP: Conductor Lane のみ attach。 Performer spawn 経路 (insert_pty_slot) は別 PR で配線予定。
    term_attaches: HashMap<LaneAddress, crate::terminal::term_attach::TermAttach>,
    /// [`deliver_nudge`] の phase1→sleep→phase2 を **lane 単位で直列化**する async lock。
    /// 2-phase nudge は sleep 中 PtySlot lock を手放すため、直列化しないと同一 lane への並行
    /// nudge が text を interleave させ、連結された誤 command を submit してしまう（#674 で
    /// 2-phase に戻した際に再オープンした race、B1 #675 の default=command 化で頻度増）。
    /// 内部可変性で持つので `deliver_nudge` は `pool.read()` のまま get-or-insert できる。
    /// map は lane 数ぶんだけ増える（bounded、lane teardown での GC は未実装だが実害小）。
    nudge_locks: std::sync::Mutex<HashMap<LaneAddress, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    /// doc 33 → doc 38: Act II の chat engine スロット（host + pump）。
    ///
    /// key は (lane, session_key) の 2 段 map（doc 38 — 1 Lane = N session。session_key は
    /// VP 採番、registry の SSOT は [`crate::lane::session_registry`] = disk）。
    ///
    /// **エンジン排他の法**（doc 38 §2 で session 粒度に改定）:
    /// - **session 内は 1 会話 1 エンジン**: 同一 session に 2 つの host は立たない
    ///   （inner map の key 一意性 + [`Self::ensure_chat_engine`] の存在 check が保証）
    /// - **focused session は床（PtySlot）と排他**: `pty_slots` xor focused の chat slot
    ///   （mode 切替が旧エンジンを必ず落としてから遷移する — 従来の法の focused への限定）
    /// - **非 focused session は床と独立**（doc 38 §2「lane 内の session 同士は独立」。
    ///   console_mode ガードは focused にのみ適用 — doc 38 落とし穴③）
    chat_engines: HashMap<LaneAddress, HashMap<SessionKey, ChatEngineSlot>>,
}

// chat engine の所有型（ChatEngineSlot / ChatHost）と engine 軸の語彙（EngineKind）は
// `crate::echoes::engine` に移設した（doc 37 — chat スタックを echoes module に閉じ、
// 他プロジェクトへ切り出せる形にする）。LanePool は所有と排他の「法」だけを担う。
use crate::echoes::{ChatEngineSlot, ChatHost, EngineKind};
// session 層の語彙（doc 38）。registry は disk が SSOT（LanePool は cache を持たない —
// 「状態の供給を 1 系統に」の原則。読みは毎回 registry file、書きは registry module 経由）。
use crate::lane::session_registry::{self, SessionKey};

/// [`LanePool::restart_lane`] の床（engine）張り替えモード（doc 39 P2 — 旧 `fresh: bool` の昇格）。
///
/// 「素の engine で起動する」（spawn command の resume 回避）と「store を破棄する」は独立の軸。
/// 旧 bool は両者を束ねていたため、「新 root で bare 起動したいが store は無傷にしたい」
/// （Act I の ✨ New）が表現できなかった。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespawnMode {
    /// 会話を継ぐ（root session の store から resume）。旧 `fresh=false`。
    Resume,
    /// lane を素に戻す（全 session store + registry 破棄）。旧 `fresh=true` = sidebar の Reset lane。
    Reset,
    /// 素の engine で張り替え、store は破棄しない（doc 39 §4 Act I New — root 張り替え用）。
    Bare,
}

/// [`LanePool::resolve_chat_session`] の解決結果 — session key と、その engine（stand）・
/// focused かどうか。ガード分岐（focused のみ console_mode ガード）と host 構築に使う。
#[derive(Debug, Clone)]
pub struct ResolvedSession {
    /// 解決された session key（`None` 指定は focused に解決済み）。
    pub key: SessionKey,
    /// session の engine 種別（stand 名）。lane の stand でなく **session の** stand。
    pub stand: String,
    /// この session が現在 focused か。
    pub focused: bool,
    /// session の会話 id（doc 40: registry が SSOT — resolve 時の registry load から同梱。
    /// host 構築の resume 解決が別 store を読み直さないための持ち回り）。
    pub conversation: Option<String>,
}

/// [`LanePool::list_chat_sessions`] の 1 要素 — registry（永続）+ runtime（engine 生死）+
/// 会話 id（engine store）を突き合わせた GUI 向け view。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatSessionInfo {
    pub key: SessionKey,
    pub stand: String,
    /// engine の会話 id（cc_session 等の store から。Draft = None、doc 38 §1.1）。
    pub engine_session_id: Option<String>,
    /// chat host が現在生きているか（in-memory slot の有無）。
    pub live: bool,
    pub focused: bool,
    /// doc 39: この session が lane の root（床に化身し mailbox を名乗る）か。
    /// GUI は root タブの × を隠す（backend の「root は remove 不可」の UI 反映）。
    pub root: bool,
}

impl std::fmt::Debug for LanePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // PtySlot は Debug 不可、 keys のみ表示
        f.debug_struct("LanePool")
            .field("lanes", &self.lanes)
            .field("pty_slots", &self.pty_slots.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl LanePool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Project 起動時に Conductor Lane を 1 つ pre-populate (Echoes default)
    ///
    /// **A5-2**: stand_spawner で command 構築 → PtySlot::spawn で実 process 起動。
    /// spawn 失敗時は graceful degrade (state=Dead、 pty_slots に entry なし) で
    /// SP 自体の起動継続性を担保。
    pub fn with_conductor(project_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        let project_id = project_id.into();
        let cwd = cwd.into();
        let mut pool = Self::new();
        let addr = LaneAddress::conductor(&project_id);
        // doc 11 PR-B: default stand は "echoes" 固定 (config.default_stand での per-user 化は
        // 後続 PR、 LanePool::with_conductor は config を持たないため)。
        // user 設定がある場合の経路は HTTP API / lane_spawn_actor 経由で stand を明示指定する。
        // PR-pre2 (VP-118): "hd" → "echoes" rename。 mise task `vp:stand:echoes` (旧 hd)。
        let stand_name = "echoes";

        // doc 33 §2: 永続 console_mode を boot で honor。chat の lane に PTY を立てない
        // （立てると echoes_submit がもう 1 本の engine を呼び、1 会話 2 エンジンになる）。
        let console_mode = crate::lane::console_mode::last(&project_id, "conductor")
            .unwrap_or(crate::lane::console_mode::ConsoleMode::Tui);

        let (state, pid) = if console_mode == crate::lane::console_mode::ConsoleMode::Chat {
            // Chat mode: engine-less で登録（EchoesAgentHost は初回 submit で lazy spawn）。
            // pid=None + state=Running は chat lane の正常形（vp-app は console_mode で
            // respawn 判定を gate する — doc 33 §3）。
            tracing::info!("Lane boot as chat mode (PTY skip): addr={}", addr);
            (LaneState::Running, None)
        } else {
            // tmux decoupling PR2: 床 (login shell) + claude 注入の Rust-native spawn (design §13)。
            // 旧 spawn_or_adopt (tmux session の adopt) は退役 — 重複 SP は DB-LOCK が spawn 前に
            // abort し (§13.3)、 claude は SP の子なので orphan session は存在しない。
            let cmd = crate::process::stand_spawner::build_stand_command(
                stand_name,
                &addr,
                std::path::Path::new(&cwd),
                false,
            );

            match crate::process::stand_spawner::spawn_stand(&cmd, 120, 48) {
                Ok((slot, term_rx)) => {
                    let pid = slot.pid();
                    tracing::info!(
                        "Lane spawned: addr={} stand={} program={} args={:?} pid={}",
                        addr,
                        stand_name,
                        cmd.program,
                        cmd.args,
                        pid
                    );
                    // Stage 1 (ADR-0001): PtySlot insert と TermAttach spawn を 1 関数に集約。
                    // term_rx は initial_rx (= reader_task start 前に取得) なので race フリー。
                    pool.insert_pty_slot(addr.clone(), slot, term_rx);
                    (LaneState::Running, Some(pid))
                }
                Err(e) => {
                    // graceful degrade: SP 自体は起動継続、 Lane は Dead で record
                    tracing::warn!(
                        "Lane spawn failed (graceful degrade to Dead): addr={} stand={} program={} cwd={} err={}",
                        addr,
                        stand_name,
                        cmd.program,
                        cwd,
                        e
                    );
                    (LaneState::Dead, None)
                }
            }
        };

        let info = LaneInfo {
            console_mode,
            // I1: conductor の安定 id を address (project, "conductor") で load_or_create
            id: crate::lane::lane_id::load_or_create(&project_id, "conductor"),
            address: addr.clone(),
            kind: LaneKind::Conductor,
            name: None,
            state,
            stand: stand_name.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            pid,
            cwd,
            // Conductor は git workspace 持たない (= project root が cwd)、 performer_status は None
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            flow_state: None,
        };
        pool.lanes.insert(addr, info);
        pool
    }

    /// Lane 一覧を **Conductor 先頭、 続いて Performer を生成順 (created_at 昇順)** で返す。
    ///
    /// 内部 `lanes` は `HashMap` のため iter 順は non-deterministic (process ごとに異なる
    /// hash seed)。 sidebar の表示要件 「Root/Conductor が一番上、 その下は生成時順」 を満たす
    /// ため、 list() で sort して contract に order を含める。
    ///
    /// `created_at` は ISO 8601 文字列 (UTC fixed) なので String::cmp で時刻順が取れる
    /// (lexicographic = chronological)。 同 ms 生成 (= populate ループ内連続 spawn) の
    /// tie-break は Lane name で安定 sort。 N≤10 想定で O(N log N) cost 無視可。
    ///
    /// 別案 (IndexMap で insertion order を data 構造に内蔵) は VP-issue 未起票。
    /// 「pty_slots も順序欲しい」「bulk import で order 崩れる」 等の動機が出たら再検討。
    pub fn list(&self) -> Vec<LaneInfo> {
        let mut v: Vec<LaneInfo> = self.lanes.values().cloned().collect();
        v.sort_by(|a, b| {
            use std::cmp::Ordering;
            match (a.kind, b.kind) {
                (LaneKind::Conductor, LaneKind::Performer) => Ordering::Less,
                (LaneKind::Performer, LaneKind::Conductor) => Ordering::Greater,
                _ => a.created_at.cmp(&b.created_at).then_with(|| {
                    a.name
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.name.as_deref().unwrap_or(""))
                }),
            }
        });
        v
    }

    pub fn get(&self, addr: &LaneAddress) -> Option<&LaneInfo> {
        self.lanes.get(addr)
    }

    pub fn insert(&mut self, info: LaneInfo) {
        self.lanes.insert(info.address.clone(), info);
    }

    /// Phase 3-A: 既に spawn 済の PtySlot を Lane address 紐付けで insert (Performer create で使う)。
    ///
    /// Stage 1 (ADR-0001): TermAttach も同期 spawn する。 `term_rx` は spawn_stand の
    /// 戻り値 (= broadcast::channel 作成と同時の initial_rx)、 reader_task が start する前に
    /// subscribe 済 = race フリー。 既存 entry があれば HashMap::insert で replace、
    /// 旧 TermAttach は Drop で handle.abort() (= restart 経路の再 attach に対応)。
    pub fn insert_pty_slot(
        &mut self,
        addr: LaneAddress,
        slot: crate::daemon::pty_slot::PtySlot,
        term_rx: tokio::sync::broadcast::Receiver<Vec<u8>>,
    ) {
        self.pty_slots
            .insert(addr.clone(), std::sync::Mutex::new(slot));
        // grid dims は PtySlot の初期 winsize (120x48、 spawn_stand 呼び出し側) と一致させる。
        // 不一致 (旧 80x24) だと headless (vp-app 未 attach) lane の capture が 80 桁で再 wrap
        // されて崩れる (PR2 実機検証で発見)。 client attach 後は resize_lane が両者を同期する。
        let term_attach = crate::terminal::term_attach::TermAttach::spawn(term_rx, 120, 48);
        self.term_attaches.insert(addr, term_attach);
    }

    pub fn remove(&mut self, addr: &LaneAddress) -> Option<LaneInfo> {
        // Phase 4-A: PtySlot も一緒に drop (= child kill 経由でプロセス停止)
        // PtySlot::Drop が child.kill() + child.wait() を呼ぶので zombie 防止。
        // Stage 1 (ADR-0001): TermAttach も同期 drop (JoinHandle::abort で task 終了)。
        // 順序: term_attaches → pty_slots → lanes (broadcast::Sender は pty_slots が保持)。
        self.term_attaches.remove(addr);
        self.pty_slots.remove(addr);
        // doc 33: chat engine も同時に drop（kill_on_drop + pump abort）。
        self.chat_engines.remove(addr);
        self.lanes.remove(addr)
    }

    pub fn count(&self) -> usize {
        self.lanes.len()
    }

    /// Phase 5-D: spawn_with_fallback の 800ms early-exit window を抜けた後で、
    ///   Lane の child process (例: `claude --continue`) が後で exit した場合の検知。
    ///
    /// ## 動作
    /// 1. 全 PtySlot の `is_alive()` (= non-blocking try_wait) を check
    /// 2. dead な Lane について:
    ///    - `LaneInfo.state` が既に Dead でなければ `LaneState::Dead` に更新
    ///    - `pty_slots` から entry を remove (Drop で child reap、 zombie 解消)
    /// 3. state transition した Lane の数を返す (caller が log 出力に使える)
    ///
    /// ## 関連 memory
    /// - vantage-point Atlas の Phase 5-D dogfooding bundle (unison-kdl で zombie 観測)
    /// - PtySlot::is_alive (`crates/vantage-point/src/daemon/pty_slot.rs`)
    pub fn detect_and_mark_dead(&mut self) -> usize {
        // step 1: dead な address を収集 (lock を持ったまま remove はできないので 2 段)
        let mut dead_addrs: Vec<LaneAddress> = Vec::new();
        for (addr, slot_mutex) in &self.pty_slots {
            if let Ok(mut slot) = slot_mutex.lock()
                && !slot.is_alive()
            {
                dead_addrs.push(addr.clone());
            }
        }

        // step 2: state 更新 + pty_slots から remove
        let mut transitioned = 0;
        for addr in dead_addrs {
            if let Some(info) = self.lanes.get_mut(&addr)
                && info.state != LaneState::Dead
            {
                tracing::warn!(
                    "Lane lifecycle: dead detected addr={} prev_state={:?} pid={:?}",
                    addr,
                    info.state,
                    info.pid
                );
                info.state = LaneState::Dead;
                transitioned += 1;
            }
            // TermAttach も同時に落とす (remove/restart_lane と順序統一)。 残すと Dead lane の
            // capture_lane が凍結した最終フレームを返し続ける (PR2 review B2)。
            self.term_attaches.remove(&addr);
            // PtySlot Drop で child.kill() + child.wait() = zombie 解消
            self.pty_slots.remove(&addr);
        }
        transitioned
    }

    /// Lane の Conductor Stand (= PtySlot の child process) を kill + 再 spawn する。
    ///
    /// 同 Lane の cwd / stand を維持したまま child process だけ作り直す。
    /// (例: HD Lane なら shell を立て直し → `claude --continue || claude` を再 inject)
    ///
    /// vp-app の WS connection は PR #218 (auto-reconnect) で透過的に新 PtySlot に
    /// attach し直す ─ pool の write lock を保持してる間は WS の read が queue され、
    /// release 後に新しい broadcast channel + scrollback を subscribe する。
    ///
    /// fresh restart の state 破棄（「lane を素に戻す」の実体、console_mode 非依存）。
    ///
    /// doc 38 落とし穴②「fresh が副を知らない」の再演防止で、対象は **registry 上の全 session**:
    /// - 各 engine の session store（cc / cursor / codex = resume の矢印。記録不在は no-op）
    /// - replay log（transcript を持たない engine の replay 源。残すと「New Session なのに
    ///   前の会話が replay される」嘘になる）
    /// - session registry 自体（既定形 N=1 へ — fresh 後の lane は「素の 1 session」）
    fn clear_fresh_lane_state(addr: &LaneAddress, default_stand: &str) -> anyhow::Result<()> {
        let lane_label = crate::process::stand_spawner::lane_label(addr).to_string();
        let reg = session_registry::load(&addr.project, &lane_label, default_stand);
        for s in &reg.sessions {
            let label = session_registry::session_label(&lane_label, s.key);
            crate::lane::cc_session::clear(&addr.project, &label).map_err(|e| {
                anyhow::anyhow!(
                    "fresh restart: cc_session の破棄に失敗（addr={addr}, session={}）: {e}",
                    s.key
                )
            })?;
            crate::lane::cursor_session::clear(&addr.project, &label).map_err(|e| {
                anyhow::anyhow!(
                    "fresh restart: cursor_session の破棄に失敗（addr={addr}, session={}）: {e}",
                    s.key
                )
            })?;
            crate::lane::codex_session::clear(&addr.project, &label).map_err(|e| {
                anyhow::anyhow!(
                    "fresh restart: codex_session の破棄に失敗（addr={addr}, session={}）: {e}",
                    s.key
                )
            })?;
            crate::echoes::replay_log::clear(&addr.project, &label).map_err(|e| {
                anyhow::anyhow!(
                    "fresh restart: replay log の破棄に失敗（addr={addr}, session={}）: {e}",
                    s.key
                )
            })?;
        }
        session_registry::clear(&addr.project, &lane_label).map_err(|e| {
            anyhow::anyhow!("fresh restart: session registry の破棄に失敗（addr={addr}）: {e}")
        })?;
        Ok(())
    }

    /// spawn 失敗時は LaneInfo.state を Dead にして error を返す (caller の責任で UI 通知)。
    ///
    /// `mode` は床（engine）の張り替え方（doc 39 P2 で 旧 `fresh: bool` から昇格 —
    /// 「素の engine で起動する」と「store を破棄する」は独立の軸で、New root は前者だけが要る）:
    /// - [`RespawnMode::Resume`]: 従来の restart（root session の store から `--resume` で会話を
    ///   継ぐ — tmux decoupling 後の継続性はこれが担う）
    /// - [`RespawnMode::Reset`]: lane を素に戻す（全 session store + registry 破棄 = 旧 fresh。
    ///   sidebar の Reset lane）
    /// - [`RespawnMode::Bare`]: 素の engine で張り替え、store は破棄しない（doc 39 §4 Act I の
    ///   ✨ New — 新 root は記録ゼロなので bare 起動が正、旧 session の会話は無傷でタブに残る）
    pub fn restart_lane(&mut self, addr: &LaneAddress, mode: RespawnMode) -> anyhow::Result<()> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        let cwd = info.cwd.clone();
        let stand = info.stand.clone();

        // Reset は「lane を素に戻す」= resume の矢印（全 session の engine store）を破棄する。
        // ⚠️ 破壊（engine drop / PtySlot kill）より先に fresh の前提を満たす。消せなければ
        // resume が残り fresh でなくなるので黙って成功にできないが、先に破壊してから bail
        // すると「死んだのに pid/state は旧値」の不整合が残る。この順序なら chat は
        // 「失敗したら何も遷移していない」を不変条件にでき、orchestrator の透過 retry が
        // 副作用なしの再試行になる。tui は spawn がその後失敗し得るが「素に戻したが立たず
        // Dead」で fresh の意図（旧会話を捨てる）と矛盾しない。
        //
        // 旧実装はこの破棄が chat 分岐の中にだけあり、tui lane の New Session（fresh）は
        // spawn command から --resume を落とすだけで pointer file が残った — restart 直後の
        // Diff::Update push が旧 id を運び、session chip が旧 id を映し続ける
        // （2026-07-17 解剖 / moody-blues 指摘の根治で mode 非依存に統一）。
        if mode == RespawnMode::Reset {
            Self::clear_fresh_lane_state(addr, &stand)?;
        }

        // doc 33: chat mode の lane の restart = chat engine の入れ替え（PTY は立てない）。
        // engine を drop するだけで、次の echoes_submit が新 engine を lazy spawn する。
        // fresh の意図は上の store 破棄が state で運ぶ（engine は lazy spawn なので
        // 「今 fresh に立て直す」対象が存在しない）:
        // - `ensure_chat_engine` の `cc_session::last` が None → --resume 無しで spawn
        //   → `EchoesAgentHost` が SessionInit で新 id を書き戻す（SSOT 復旧）
        // - transcript replay-on-attach も参照先を失う → 前の会話を映さない
        //   （消さないと「New Session なのに前の会話が出る」嘘になる）
        if info.console_mode == crate::lane::console_mode::ConsoleMode::Chat {
            // lane 単位の restart は全 session の engine を落とす（lazy respawn が resume で継ぐ）。
            self.chat_engines.remove(addr);
            if let Some(info) = self.lanes.get_mut(addr) {
                info.pid = None;
                info.state = LaneState::Running;
            }
            tracing::info!(
                "Lane restart (chat mode): engine drop、次 submit で再 spawn: {addr} mode={mode:?}"
            );
            return Ok(());
        }

        // step 1: 既存 PtySlot + TermAttach を drop (Drop で child.kill() + child.wait() = zombie 解消)。
        // tmux decoupling PR2: claude は PtySlot の子なので drop = 完全停止。 旧 step 1.5
        // (VP-131 の tmux kill_session) は tmux session という第 2 の生存木と共に消滅。
        // Stage 1 (ADR-0001): 順序は LanePool::remove と一致 (term_attaches → pty_slots、
        // broadcast::Sender は pty_slots が保持なので task は次 iter で Closed 検知して exit)。
        self.term_attaches.remove(addr);
        let _ = self.pty_slots.remove(addr);

        // step 2: 同 stand で respawn (bare 判定は builder に直接渡す — 旧 VP_FRESH env の後継)。
        // Reset / Bare とも「素の engine で起動」（resume/continue 回避）。差は store 破棄の有無
        // だけで、それは上の clear_fresh_lane_state 分岐が既に処理済み。
        let cmd = crate::process::stand_spawner::build_stand_command(
            &stand,
            addr,
            std::path::Path::new(&cwd),
            mode != RespawnMode::Resume,
        );
        match crate::process::stand_spawner::spawn_stand(&cmd, 120, 48) {
            Ok((slot, term_rx)) => {
                let pid = slot.pid();
                // Stage 1 (ADR-0001): PtySlot insert + TermAttach 再 spawn を集約。
                self.insert_pty_slot(addr.clone(), slot, term_rx);
                if let Some(info) = self.lanes.get_mut(addr) {
                    info.state = LaneState::Running;
                    info.pid = Some(pid);
                }
                tracing::info!(
                    "Lane restarted: addr={} stand={} program={} pid={}",
                    addr,
                    stand,
                    cmd.program,
                    pid
                );
                Ok(())
            }
            Err(e) => {
                if let Some(info) = self.lanes.get_mut(addr) {
                    info.state = LaneState::Dead;
                    info.pid = None;
                }
                tracing::warn!(
                    "Lane restart failed (state→Dead): addr={} stand={} err={}",
                    addr,
                    stand,
                    e
                );
                Err(e)
            }
        }
    }

    /// Display 形 (`"<project>/conductor"` / `"<project>/performer/<name>"`) をパースして LaneAddress を作る。
    /// vp-app の sidebar から `lane:select` IPC の address (= `lane_address_key`) を逆変換するために使う。
    pub fn parse_address(s: &str) -> Option<LaneAddress> {
        let parts: Vec<&str> = s.splitn(3, '/').collect();
        match parts.as_slice() {
            // 旧 "lead"/"wing" も受理 (conductor/performer rename 前の session.json / wire address 互換)
            [project, "conductor" | "lead"] if !project.is_empty() => {
                Some(LaneAddress::conductor(*project))
            }
            [project, "performer" | "wing", name] if !project.is_empty() && !name.is_empty() => {
                Some(LaneAddress::performer(*project, *name))
            }
            _ => None,
        }
    }

    /// lane の console 現在画面を text で返す（tmux decoupling: `capture-pane` の native 代替）。
    ///
    /// per-lane に張られた TermAttach（alacritty grid、`insert_pty_slot` で全 lane 配線済）から
    /// [`TermAttach::grid_text`](crate::terminal::term_attach::TermAttach::grid_text) を render。
    /// lane 不在 / attach 不在（spawn 失敗 = Dead 等）は None。
    pub fn capture_lane(&self, addr: &LaneAddress) -> Option<String> {
        self.term_attaches.get(addr).map(|t| t.grid_text())
    }

    /// 既存 Lane の PtySlot に新しい subscriber を追加 (PTY output を WS に流す等の用途)。
    /// `None` = address に対応する Lane が無い、 もしくは PtySlot が無い (state=Dead 等)。
    ///
    /// memory rule (mem_1CaTpCQH8iLJ2PasRcPjHv): Lane = Session Process。
    /// Phase 2 で vp-app が WS で attach する際、 既存 PtySlot に subscribe して
    /// 同じ PTY を複数 client が共有できる (broadcast channel ベース)。
    pub fn subscribe_output(
        &self,
        addr: &LaneAddress,
    ) -> Option<tokio::sync::broadcast::Receiver<Vec<u8>>> {
        let slot_mutex = self.pty_slots.get(addr)?;
        let slot = slot_mutex.lock().ok()?;
        Some(slot.subscribe_output())
    }

    /// [`Self::subscribe_output`] の replay 付き版 — 直近出力 snapshot + 購読を原子的に取得。
    ///
    /// terminal pump の attach 経路が使う（vp-app 再起動後の新 xterm に前回画面を復元する
    /// replay-on-attach）。 snapshot と receiver の境界は
    /// [`PtySlot::attach_output`](crate::daemon::pty_slot::PtySlot::attach_output) が保証する。
    pub fn attach_output(
        &self,
        addr: &LaneAddress,
    ) -> Option<(Vec<u8>, tokio::sync::broadcast::Receiver<Vec<u8>>)> {
        let slot_mutex = self.pty_slots.get(addr)?;
        let slot = slot_mutex.lock().ok()?;
        Some(slot.attach_output())
    }

    /// [`deliver_nudge`] の per-lane 直列化 lock を get-or-insert で返す。
    /// 同一 `addr` には常に同じ `Arc<Mutex>` を返し（→ 直列化が効く）、別 `addr` には別の lock を
    /// 返す（→ cross-lane は並行のまま）。`nudge_locks` の std Mutex は await を跨がず即 drop する。
    fn nudge_lock_handle(
        &self,
        addr: &LaneAddress,
    ) -> anyhow::Result<std::sync::Arc<tokio::sync::Mutex<()>>> {
        let mut locks = self
            .nudge_locks
            .lock()
            .map_err(|_| anyhow::anyhow!("nudge_locks mutex poisoned: {}", addr))?;
        Ok(locks
            .entry(addr.clone())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }

    /// 既存 Lane の PtySlot に input を書き込む (WS から user 入力を受けた時に使う)。
    /// `Mutex<PtySlot>` を lock するので、 broadcast 経路と直交して同期書込み。
    pub fn write_to_lane(&self, addr: &LaneAddress, data: &[u8]) -> anyhow::Result<()> {
        let slot_mutex = self
            .pty_slots
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane has no PtySlot: {}", addr))?;
        let mut slot = slot_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("PtySlot mutex poisoned: {}", addr))?;
        slot.write(data)
    }

    // =========================================================================
    // doc 33: Console engine slot（Act I/II 排他）
    //
    // 法: 1 lane = 高々 1 エンジン（pty_slots xor chat_engines）= 1 cc_session。
    // 排他は set_console_mode / ensure_chat_engine のみが engine を作る・壊すことで保証。
    // =========================================================================

    /// lane の console mode（registry cache）。lane 不在は None。
    pub fn console_mode(
        &self,
        addr: &LaneAddress,
    ) -> Option<crate::lane::console_mode::ConsoleMode> {
        self.lanes.get(addr).map(|i| i.console_mode)
    }

    /// session 指定を解決する（doc 38）: `None` = focused（省略時後方互換）、`Some(k)` は
    /// registry 上の実在を検証。戻り値は key + session の engine（stand）+ focused か。
    ///
    /// registry は disk が SSOT（毎回 file read）。submit 等の per-message 経路も通るが、
    /// 数百 byte の 1 file read で、既存の session_store 読み（cc_session::last 等）と同規模 —
    /// in-memory cache で供給が 2 系統に割れるリスクの方が大きい（doc 38 §5 原則）。
    /// lane/session 数が増えて実測で問題になったら spawn_blocking 化 / cache を再検討する。
    pub fn resolve_chat_session(
        &self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
    ) -> anyhow::Result<ResolvedSession> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        let lane_label = crate::process::stand_spawner::lane_label(addr);
        let reg = session_registry::load(&addr.project, lane_label, &info.stand);
        let key = session.unwrap_or(reg.focused);
        let entry = reg.sessions.iter().find(|s| s.key == key).ok_or_else(|| {
            anyhow::anyhow!("session が存在しません（addr={addr}, session={key}）")
        })?;
        Ok(ResolvedSession {
            key,
            stand: entry.stand.clone(),
            focused: key == reg.focused,
            conversation: entry.conversation.clone(),
        })
    }

    /// lane の session 一覧（registry + engine 生死 + 会話 id の突き合わせ view、doc 38）。
    pub fn list_chat_sessions(&self, addr: &LaneAddress) -> anyhow::Result<Vec<ChatSessionInfo>> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        let lane_label = crate::process::stand_spawner::lane_label(addr);
        let reg = session_registry::load(&addr.project, lane_label, &info.stand);
        let live = self.chat_engines.get(addr);
        Ok(reg
            .sessions
            .iter()
            .map(|s| {
                ChatSessionInfo {
                    key: s.key,
                    stand: s.stand.clone(),
                    // 会話 id は registry が SSOT（doc 40 §5 — 旧 3-way store dispatch は
                    // load 時の backfill bridge に畳まれた。Draft = None、doc 38 §1.1）。
                    engine_session_id: s.conversation.clone(),
                    live: live.is_some_and(|m| m.contains_key(&s.key)),
                    focused: s.key == reg.focused,
                    root: s.key == reg.root,
                }
            })
            .collect())
    }

    /// session を追加する（doc 38 Phase 2 の「+」の backend。`stand=None` は lane の stand）。
    /// engine は spawn しない（Draft のまま。focused eager は Phase 3、submit で lazy spawn）。
    pub fn create_chat_session(
        &mut self,
        addr: &LaneAddress,
        stand: Option<&str>,
        focus: bool,
    ) -> anyhow::Result<SessionKey> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        let stand = stand.unwrap_or(&info.stand);
        // 対応表は EngineKind が SSOT — 未知 stand の session は engine を一生持てないので入口で弾く。
        if EngineKind::from_stand(stand).is_none() {
            anyhow::bail!("未知の stand です（addr={addr}, stand={stand}）");
        }
        let lane_label = crate::process::stand_spawner::lane_label(addr);
        let key = session_registry::create(&addr.project, lane_label, &info.stand, stand, focus)
            .map_err(|e| anyhow::anyhow!("session 作成に失敗（addr={addr}）: {e}"))?;
        tracing::info!(
            "chat session create: addr={addr} session={key} stand={stand} focus={focus}"
        );
        Ok(key)
    }

    /// doc 39 §4: Act I の ✨ New の registry 部 — 新 session（現 root の stand を引き継ぐ）を
    /// 作り、root と focused を同時にそれへ向ける。床の張り替え（respawn）は caller が
    /// [`restart_lane_orchestrated`](crate::process::routes::lanes::restart_lane_orchestrated) を
    /// [`RespawnMode::Bare`] で呼ぶ（spawn の orchestration = retry / pump 付替 / Diff push は
    /// restart 経路に一元化 — 第 2 の spawn 経路を作らない）。
    ///
    /// mode=Tui 限定: chat lane（Act II）の New は既存の `create_chat_session`（新 Draft タブ）が
    /// 担う — 「今いる Act に出す」の分岐は vp-app が行い、backend は各動詞の整合だけ守る。
    pub fn prepare_new_root_session(&mut self, addr: &LaneAddress) -> anyhow::Result<SessionKey> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        if info.console_mode != crate::lane::console_mode::ConsoleMode::Tui {
            anyhow::bail!(
                "echoes_session_new_root は Act I（mode=tui）専用です（addr={addr}。chat lane の New は echoes_session_create）"
            );
        }
        let lane_label = crate::process::stand_spawner::lane_label(addr);
        // 新 session の engine は現 root の stand を引き継ぐ（doc 39 §1「engine は現 session を
        // 引き継ぎ」— lane の stand でなく root の stand。N=1 では両者は一致する）。
        let reg = session_registry::load(&addr.project, lane_label, &info.stand);
        let stand = reg
            .sessions
            .iter()
            .find(|s| s.key == reg.root)
            .map(|s| s.stand.clone())
            .unwrap_or_else(|| info.stand.clone());
        let key = session_registry::create_root(&addr.project, lane_label, &info.stand, &stand)
            .map_err(|e| anyhow::anyhow!("root session 作成に失敗（addr={addr}）: {e}"))?;
        tracing::info!(
            "new root session: addr={addr} session={key} stand={stand}（旧 root はタブに残存）"
        );
        Ok(key)
    }

    /// focused session を切り替える（registry 永続のみ。床への注入・eager spawn は Phase 3）。
    pub fn focus_chat_session(
        &mut self,
        addr: &LaneAddress,
        key: SessionKey,
    ) -> anyhow::Result<()> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        let is_chat = info.console_mode == crate::lane::console_mode::ConsoleMode::Chat;
        let lane_label = crate::process::stand_spawner::lane_label(addr);
        session_registry::focus(&addr.project, lane_label, &info.stand, key)
            .map_err(|e| anyhow::anyhow!("session focus に失敗（addr={addr}）: {e}"))?;
        // LaneInfo.pid は「focused session の代表値」— chat mode では切替に追随させる
        // （新 focused の engine が未 spawn なら None = chat-idle の正常形）。
        // Tui mode の pid は床（PTY）のものなので触らない。
        if is_chat {
            let pid = self
                .chat_engines
                .get(addr)
                .and_then(|m| m.get(&key))
                .and_then(|slot| slot.host.pid());
            if let Some(info) = self.lanes.get_mut(addr) {
                info.pid = pid;
            }
        }
        tracing::info!("chat session focus: addr={addr} session={key}");
        Ok(())
    }

    /// session を取り除く（doc 38 Phase 3 — tab を閉じる）。戻り値 = 新 focused key。
    ///
    /// - registry から除去（最後の 1 本は registry 側が拒否 — lane を素に戻すのは fresh restart）
    /// - 当該 session の engine slot を drop（走行中 turn は落ちる = 会話をやめる意思表示）
    /// - per-session の会話 id を全 engine store から破棄（key は再利用されないため残しても
    ///   概ね無害だが、session #1 の label = 素の lane 名は Act I 床の resume が読むため、
    ///   消さないと「閉じた会話が床で蘇る」嘘になる）。破棄失敗は warn（remove 自体は成立）
    /// - focused を取り除いた場合の focus 移動は registry が決める（残りの先頭）。
    ///   LaneInfo.pid は focused 代表値の規律で追随（[`Self::focus_chat_session`] と同じ）
    pub fn remove_chat_session(
        &mut self,
        addr: &LaneAddress,
        key: SessionKey,
    ) -> anyhow::Result<SessionKey> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        let is_chat = info.console_mode == crate::lane::console_mode::ConsoleMode::Chat;
        let stand = info.stand.clone();
        let lane_label = crate::process::stand_spawner::lane_label(addr).to_string();
        let new_focused = session_registry::remove(&addr.project, &lane_label, &stand, key)
            .map_err(|e| {
                anyhow::anyhow!("session remove に失敗（addr={addr}, session={key}）: {e}")
            })?;
        if let Some(slots) = self.chat_engines.get_mut(addr) {
            slots.remove(&key);
            if slots.is_empty() {
                self.chat_engines.remove(addr);
            }
        }
        let label = session_registry::session_label(&lane_label, key);
        for (store, res) in [
            (
                "cc_session",
                crate::lane::cc_session::clear(&addr.project, &label),
            ),
            (
                "cursor_session",
                crate::lane::cursor_session::clear(&addr.project, &label),
            ),
            (
                "codex_session",
                crate::lane::codex_session::clear(&addr.project, &label),
            ),
            // transcript を持たない engine の replay 源も破棄（cc_session と同じ理由: session #1 の
            // label = 素の lane 名は Act I 床の resume も読むため、残すと閉じた会話が蘇る）。
            (
                "replay_log",
                crate::echoes::replay_log::clear(&addr.project, &label),
            ),
        ] {
            if let Err(e) = res {
                tracing::warn!(
                    "session remove: {store} の破棄に失敗（addr={addr}, session={key}）: {e}"
                );
            }
        }
        if is_chat {
            let pid = self
                .chat_engines
                .get(addr)
                .and_then(|m| m.get(&new_focused))
                .and_then(|slot| slot.host.pid());
            if let Some(info) = self.lanes.get_mut(addr) {
                info.pid = pid;
            }
        }
        tracing::info!("chat session remove: addr={addr} session={key} → focused={new_focused}");
        Ok(new_focused)
    }

    /// chat engine の in-flight tail（disk にまだ載っていない増分 + commit 世代）。
    ///
    /// engine 未起動（chat-idle / Act I）は None = 継ぐものが無い。
    /// transcript replay がこれを後ろに継いで「生成中の message」まで復元する
    /// （[`crate::echoes::host`] の module doc）。`session=None` は focused。
    pub fn chat_in_flight(
        &self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
    ) -> Option<crate::echoes::InFlight> {
        let resolved = self.resolve_chat_session(addr, session).ok()?;
        self.chat_engines
            .get(addr)?
            .get(&resolved.key)
            .map(|slot| slot.host.in_flight())
    }

    /// chat engine の commit 世代のみ（transcript 読み後の検算用）。`session=None` は focused。
    pub fn chat_commit_seq(&self, addr: &LaneAddress, session: Option<SessionKey>) -> Option<u64> {
        let resolved = self.resolve_chat_session(addr, session).ok()?;
        self.chat_engines
            .get(addr)?
            .get(&resolved.key)
            .map(|slot| slot.host.commit_seq())
    }

    /// Console のエンジンモードを切り替える（doc 33 §2 の状態機械）。
    ///
    /// - → Chat: PtySlot + TermAttach を drop（claude TUI 停止）→ mode 永続 → engine-less
    ///   （EchoesAgentHost は初回 submit で lazy spawn）
    /// - → Tui: chat engine を drop（headless 停止）→ mode 永続 → PTY respawn
    ///   （`restart_lane` 再利用 = cc_session `--resume` で文脈継承）
    ///
    /// 同一 mode への切替は no-op。Chat が許されるのは stand="echoes" の lane のみ。
    pub fn set_console_mode(
        &mut self,
        addr: &LaneAddress,
        mode: crate::lane::console_mode::ConsoleMode,
    ) -> anyhow::Result<()> {
        use crate::lane::console_mode::ConsoleMode;
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        if info.console_mode == mode {
            return Ok(());
        }
        // Chat（Act II）は headless host を持つ engine の lane のみ（能力表明は EngineKind に
        // 一元化 — agy は Act I のみ、shell 等は engine なし。doc 37 §7.5「セル単位 readiness」）。
        // 未対応 engine を Chat に切替えて誤 spawn するのを型ではなくここで塞ぐ。
        if mode == ConsoleMode::Chat
            && !EngineKind::from_stand(&info.stand).is_some_and(EngineKind::chat_capable)
        {
            anyhow::bail!(
                "console mode Chat は Act II host を持つ engine の lane のみ（addr={}, stand={}）",
                addr,
                info.stand
            );
        }
        let lane_label = crate::process::stand_spawner::lane_label(addr).to_string();

        match mode {
            ConsoleMode::Chat => {
                // TUI engine 停止（PtySlot Drop = child kill + wait、restart_lane step1 と同順序）。
                self.term_attaches.remove(addr);
                let _ = self.pty_slots.remove(addr);
                if let Err(e) = crate::lane::console_mode::record(&addr.project, &lane_label, mode)
                {
                    tracing::warn!("console_mode 永続失敗（addr={addr}）: {e}");
                }
                if let Some(info) = self.lanes.get_mut(addr) {
                    info.console_mode = ConsoleMode::Chat;
                    info.pid = None;
                    info.state = LaneState::Running; // chat-idle は正常形（doc 33 §3）
                }
                tracing::info!("console mode → chat（TUI 停止、engine は submit で lazy）: {addr}");
                Ok(())
            }
            ConsoleMode::Tui => {
                // focused session の chat engine 停止（Drop = kill_on_drop + pump abort）。
                // doc 38 §2: 床（Act I）と排他なのは focused session だけ。非 focused の
                // session は床と独立に生き続ける（lane 内の session 同士は独立）。
                // N=1（registry file 不在）では focused=1 = 唯一の slot なので従来と同一挙動。
                let focused = session_registry::focused(&addr.project, &lane_label);
                if let Some(slots) = self.chat_engines.get_mut(addr) {
                    slots.remove(&focused);
                    if slots.is_empty() {
                        self.chat_engines.remove(addr);
                    }
                }
                if let Err(e) = crate::lane::console_mode::record(&addr.project, &lane_label, mode)
                {
                    tracing::warn!("console_mode 永続失敗（addr={addr}）: {e}");
                }
                if let Some(info) = self.lanes.get_mut(addr) {
                    info.console_mode = ConsoleMode::Tui;
                }
                // PTY respawn は restart_lane を再利用（--resume は build_stand_command が
                // cc_session から拾う = 会話継続）。
                tracing::info!("console mode → tui（headless 停止、PTY respawn）: {addr}");
                self.restart_lane(addr, RespawnMode::Resume)
            }
        }
    }

    /// chat engine を確保する（無ければ spawn + pump 起動）。`session=None` は focused。
    ///
    /// **法の番人**（doc 38 で session 粒度に改定）:
    /// - **focused session**: mode=Chat 以外では拒否（= PtySlot が生きたまま同一会話に headless を
    ///   立てる経路を型ではなくここで一元的に塞ぐ）。pty_slots 残存は不変条件違反として明示 Err
    /// - **非 focused session**: 床（Act I）と独立なので console_mode ガードを適用しない
    ///   （doc 38 落とし穴③ — ガードの流用は「Tui 中は副 session が動けない」という
    ///   意図しない制約の混入になる）。session 内の 1 会話 1 エンジンは存在 check が保証
    pub fn ensure_chat_engine(
        &mut self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
        topic_router: &std::sync::Arc<crate::process::topic_router::TopicRouter>,
    ) -> anyhow::Result<()> {
        use crate::lane::console_mode::ConsoleMode;
        let resolved = self.resolve_chat_session(addr, session)?;
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        if resolved.focused {
            if info.console_mode != ConsoleMode::Chat {
                // 呼び元は echoes_submit / echoes_nudge の両方（doc 34 channel E）— method 名は
                // 呼び元の ctx が名乗るので、ここでは要件だけ述べる。
                anyhow::bail!(
                    "chat engine には console mode=chat が必要（addr={}、現在 {:?}。console_set_mode で切替）",
                    addr,
                    info.console_mode
                );
            }
            if self.pty_slots.contains_key(addr) {
                anyhow::bail!(
                    "不変条件違反: mode=chat なのに PtySlot が残存（addr={}）",
                    addr
                );
            }
        }
        if self
            .chat_engines
            .get(addr)
            .is_some_and(|m| m.contains_key(&resolved.key))
        {
            return Ok(());
        }

        let lane_label = crate::process::stand_spawner::lane_label(addr).to_string();
        // session の store label（doc 38: session #1 = 素の lane 名で既存 file 互換、#2 以降は
        // `<lane>#<n>`）。host の config.lane にもこれを渡す = record-from-init が同じ per-session
        // slot に会話 id を書き戻す（session_store の key 拡張はこの 1 点で全 engine に効く）。
        let label = session_registry::session_label(&lane_label, resolved.key);
        // engine ごとに host を組む（対応表は EngineKind が SSOT。engine は **session の** stand —
        // lane と異なる engine の session を持てる、doc 38 §1）。
        let host = match EngineKind::from_stand(&resolved.stand) {
            Some(EngineKind::Codex) => {
                // codex: 常駐 RpcHost（`codex app-server` JSONL JSON-RPC、doc 41）。thread id は
                // registry の会話 id（doc 40 §5 — Act I と共有。書き戻しは host が registry 直結）。
                ChatHost::Codex(crate::echoes::CodexAgentHost::spawn(
                    crate::echoes::CodexRpcHostConfig {
                        cwd: info.cwd.clone(),
                        project: addr.project.clone(),
                        lane: label.clone(),
                        thread_id: resolved.conversation.clone(),
                    },
                )?)
            }
            Some(EngineKind::Claude) => {
                // claude: 常駐 stream-json host。resume は registry の会話 id（doc 40 §5）。
                // doc 33 C2: transcript が実在する id だけ resume に渡す（stale/phantom id で
                // "No conversation found" ハードエラーになるのを防ぐ = TUI の `|| claude` 相当）。
                let resume = resolved
                    .conversation
                    .clone()
                    .filter(|id| crate::lane::cc_session::transcript_exists(id));
                // Act II モデル切替: lane に永続された model を `--model` に渡す（未記録 = claude default）。
                // 切替（console_set_model）は record → engine 入替で行われ、resume と組むことで
                // 会話コンテキストを保ったままモデルだけ替わる。model は lane 単位（session 間で
                // 共有 — per-session 化は dogfood 後に判断）。
                let model = crate::lane::engine_model::last(&addr.project, &lane_label);
                ChatHost::Claude(crate::echoes::EchoesAgentHost::spawn(
                    crate::echoes::EchoesHostConfig {
                        cwd: info.cwd.clone(),
                        project: addr.project.clone(),
                        lane: label.clone(),
                        resume_session_id: resume,
                        model,
                        claude_cli_path: None,
                    },
                )?)
            }
            Some(EngineKind::Cursor | EngineKind::Agy) | None => {
                // cursor は Act II オミット（doc 39 §7、step 4 で TurnHost 系撤去。Act I の床は
                // 現役）。focused は mode=Chat ガード（set_console_mode）が上流で塞ぐので通常
                // 到達しない（belt-and-suspenders）。非 focused session はここが唯一の防壁。
                anyhow::bail!(
                    "stand '{}' は Act II chat host を持ちません（addr={}, session={}）",
                    resolved.stand,
                    addr,
                    resolved.key
                );
            }
        };
        // replay-log tap: transcript を持たない engine（codex）の session にだけ付ける。
        // claude は transcript が SSOT なので None（二重化しない）。tap は配信 event を per-session
        // に disk 記録し、demand_start の no_session path がそれを replay 源にする（doc — engine
        // 非依存 replay log）。
        let replay_tap = match EngineKind::from_stand(&resolved.stand) {
            Some(EngineKind::Codex) => Some(crate::echoes::replay_log::ReplayLogTap {
                project: addr.project.clone(),
                label: label.clone(),
            }),
            _ => None,
        };
        let pump = crate::process::echoes_pump::spawn_lane_echoes_pump(
            addr.to_string(),
            resolved.key,
            host.subscribe(),
            topic_router.clone(),
            replay_tap,
        );
        let pid = host.pid();
        // LaneInfo.pid / state は lane の代表 = focused session に紐づける（非 focused の
        // spawn は lane 表示を動かさない — sidebar の pid は focused の会話のもの）。
        if resolved.focused
            && let Some(info) = self.lanes.get_mut(addr)
        {
            info.pid = pid;
            info.state = LaneState::Running;
        }
        self.chat_engines
            .entry(addr.clone())
            .or_default()
            .insert(resolved.key, ChatEngineSlot { host, pump });
        tracing::info!(
            "chat engine start: addr={addr} session={} pid={pid:?}",
            resolved.key
        );
        Ok(())
    }

    /// 当該 session の chat slot を引く（`session=None` は focused。不在は Err）。
    /// submit / interrupt / respond / set_permission_mode の共通核。
    fn chat_slot(
        &self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
    ) -> anyhow::Result<&ChatEngineSlot> {
        let resolved = self.resolve_chat_session(addr, session)?;
        self.chat_engines
            .get(addr)
            .and_then(|m| m.get(&resolved.key))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "chat engine 未起動（addr={}, session={}）",
                    addr,
                    resolved.key
                )
            })
    }

    /// chat engine に prompt を投入する（`&self` — read lock 下で呼べる）。`session=None` は focused。
    pub async fn submit_chat(
        &self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
        prompt: &str,
    ) -> anyhow::Result<()> {
        self.chat_slot(addr, session)?.host.submit(prompt).await
    }

    /// doc 35 §5: 実行中 turn を中断する（stop ボタン / Esc）。submit_chat と同型（read lock 下で
    /// 呼べる — host が stdin Mutex で直列化）。engine 不在は Err（走行中 turn が無ければ何もしない）。
    pub async fn interrupt_chat(
        &self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
    ) -> anyhow::Result<()> {
        self.chat_slot(addr, session)?.host.interrupt().await
    }

    /// doc 35 §2.5 / PR3: permission mode を動的に切替える（承認モードへ opt-in / 素通しへ戻す）。
    /// submit_chat と同型（read lock 下）。engine 不在は Err。
    pub async fn set_permission_mode_chat(
        &self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
        mode: &str,
    ) -> anyhow::Result<()> {
        self.chat_slot(addr, session)?
            .host
            .set_permission_mode(mode)
            .await
    }

    /// chat engine の逆方向 `can_use_tool`（[`crate::echoes::EchoesEvent::Question`]）へ回答する
    /// （doc 35 PR1、`&self` — read lock 下で呼べる）。
    ///
    /// **ensure しない**: 応答対象 engine が居なければ Err。質問した engine が死んでいたら応答先が
    /// 無い（submit と違い会話を新規に立てても意味が無い = pending 質問はその engine に紐づく）。
    pub async fn respond_permission_chat(
        &self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
        request_id: &str,
        decision: crate::echoes::PermissionDecision,
    ) -> anyhow::Result<()> {
        self.chat_slot(addr, session)
            .map_err(|e| anyhow::anyhow!("{e} — 応答先が無い"))?
            .host
            .respond_permission(request_id, decision)
            .await
    }

    /// 当該 session の chat engine を落とす（submit 失敗時の self-heal 用。次の ensure で再 spawn）。
    /// `session=None` は focused。他 session の engine は巻き添えにしない（doc 38 §2「独立」）。
    pub fn drop_chat_engine(&mut self, addr: &LaneAddress, session: Option<SessionKey>) -> bool {
        let Ok(resolved) = self.resolve_chat_session(addr, session) else {
            return false;
        };
        let Some(slots) = self.chat_engines.get_mut(addr) else {
            return false;
        };
        let dropped = slots.remove(&resolved.key).is_some();
        if slots.is_empty() {
            self.chat_engines.remove(addr);
        }
        // pid は focused session の代表値なので、focused を落とした時だけ下ろす。
        if dropped
            && resolved.focused
            && let Some(info) = self.lanes.get_mut(addr)
        {
            info.pid = None;
        }
        dropped
    }

    /// 既存 Lane の PtySlot を resize する。
    /// Stage 1 (ADR-0001): TermAttach も並走 resize (= alacritty Term<T> grid を同期)。
    pub fn resize_lane(&self, addr: &LaneAddress, cols: u16, rows: u16) -> anyhow::Result<()> {
        let slot_mutex = self
            .pty_slots
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane has no PtySlot: {}", addr))?;
        let slot = slot_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("PtySlot mutex poisoned: {}", addr))?;
        slot.resize(cols, rows)?;
        // attach 不在 (= spawn 失敗 / 未配線 Performer 経路) は静かに skip
        if let Some(term_attach) = self.term_attaches.get(addr) {
            term_attach.resize(cols, rows);
        }
        Ok(())
    }
}

/// nudge 配送: lane の PtySlot に text を流し込み、続けて Enter(`\r`) を**単独**送って submit させる。
///
/// ## なぜ 2-phase（text → 50ms → 単独 `\r`）か
/// claude の TUI は入力の burst を **paste として検出**し、`text + \r` を 1 回で write すると
/// 末尾 `\r` を「改行(literal newline)」として paste に飲み込み **submit しない**（プロンプトに
/// 残り、人が RETURN を押す羽目になる）。text を write → 50ms 空けて `\r` を**別 write** すると
/// `\r` が独立した read = Enter keystroke として届き submit される（design doc §12）。
///
/// ⚠️ #663 で「PtySlot 直結後は paste-wrap 主体が消えたので 1-write で OK」と畳んだが、その
/// §13.6c 実機検証は login shell 側で、claude TUI の paste 検出経路を踏んでいなかった。実運用で
/// 「command が届くが自動読み込みされず手動 RETURN が要る」regression になったため 2-phase に戻す。
/// **1-write へ再度畳まないこと**（claude TUI の paste 判定に依存する submit のため）。
///
/// PtySlot の lock は各 write ごとに `read().await` で都度取り即 drop し、間の sleep は無 lock で
/// 行う（await 跨ぎで guard を保持しない）。 in-process nudge（`AppState::nudge_lane`）と
/// World→SP proxy（`lane_nudge`）の双方から呼ばれる共通 sink（submit 意味論を 1 箇所に集約）。
///
/// ## 並行 nudge の直列化（#674 の race を塞ぐ）
/// phase 間の sleep 中は PtySlot lock を手放すため、同一 lane へ並行に走る 2 本の `deliver_nudge`
/// が phase1 の text を interleave させると、`text_A` + `text_B` が連結された誤 command が
/// submit されうる（B1 #675 で CC の素の `wire_send` が default=command 化し、delivery-loop の
/// 即時 re-nudge で同一 recipient への近接 nudge が起きやすくなった）。lane 単位の async lock
/// （[`LanePool::nudge_locks`]）で phase1→sleep→phase2 全体を直列化してこれを防ぐ。lock は
/// lane 単位なので別 lane への nudge は並行のまま（cross-lane の head-of-line blocking なし）。
pub async fn deliver_nudge(
    pool: &std::sync::Arc<tokio::sync::RwLock<LanePool>>,
    addr: &LaneAddress,
    text: &str,
) -> anyhow::Result<()> {
    // 同一 lane への並行 nudge を直列化する per-lane lock を get-or-insert（内部可変性なので
    // read guard で足りる。handle 取得後は pool read lock を手放す）。
    let nudge_lock = pool.read().await.nudge_lock_handle(addr)?;
    // この guard を phase1→sleep→phase2 の間ずっと保持し、同一 lane の他 nudge を待たせる。
    let _serialized = nudge_lock.lock().await;

    // phase 1: text 本体（末尾 CR/LF は落として単一行の paste にする）
    let body = text.trim_end_matches(['\r', '\n']);
    pool.read().await.write_to_lane(addr, body.as_bytes())?;
    // paste 判定を跨ぐ猶予（best-effort nudge なので体感遅延にならない範囲）
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // phase 2: Enter(CR) 単独 → 独立 keystroke として submit
    pool.read().await.write_to_lane(addr, b"\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// lane を PTY / engine 無しで pool に置く（restart_lane の chat 分岐は早期 return する
    /// ので spawn 不要）。mode を注入できる（doc 38: focused / 非 focused でガードが割れる）。
    fn insert_lane(
        pool: &mut LanePool,
        addr: &LaneAddress,
        mode: crate::lane::console_mode::ConsoleMode,
    ) {
        pool.insert(LaneInfo {
            console_mode: mode,
            id: Default::default(),
            address: addr.clone(),
            kind: LaneKind::Conductor,
            name: None,
            state: LaneState::Running,
            stand: "echoes".to_string(),
            created_at: "2026-07-10T00:00:00Z".to_string(),
            pid: None,
            cwd: "/tmp".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            flow_state: None,
        });
    }

    /// chat mode の lane を pool に置く（従来 helper の互換形）。
    fn insert_chat_lane(pool: &mut LanePool, addr: &LaneAddress) {
        insert_lane(pool, addr, crate::lane::console_mode::ConsoleMode::Chat);
    }

    /// doc 33 → doc 39 P2: chat lane の restart は `RespawnMode` で意味が割れる。
    /// - Resume → cc_session を残す（次 spawn が `--resume` で会話を継ぐ）
    /// - Bare   → cc_session を残す（素の engine で張り替えるが store は無傷 — 新 root 用）
    /// - Reset  → cc_session を捨てる（素の新規 session + replay も前会話を映さない）
    ///
    /// engine は lazy spawn なので「立て直す対象」がその場に無く、意図は state
    /// (cc_session の有無) でしか運べない。 その 1 点をここで固定する。
    #[test]
    fn chat_restart_clears_cc_session_only_when_fresh() {
        // cc_session は vp_state_dir() = $XDG_STATE_HOME/vp を読む。 crate 唯一のロック下で
        // tempdir に向け、 guard の drop で復元する。
        let _state = crate::test_env::state_dir();

        let addr = LaneAddress::conductor("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);

        // Resume: 会話を継ぐので記録は残る
        crate::lane::cc_session::record("vp", "conductor", "old-session-id").expect("record");
        pool.restart_lane(&addr, RespawnMode::Resume)
            .expect("chat restart");
        assert_eq!(
            crate::lane::cc_session::last("vp", "conductor").as_deref(),
            Some("old-session-id"),
            "Resume restart は resume の矢印を保つ"
        );

        // Bare（doc 39 P2）: 素の engine で張り替えるが store は破棄しない
        pool.restart_lane(&addr, RespawnMode::Bare)
            .expect("bare chat restart");
        assert_eq!(
            crate::lane::cc_session::last("vp", "conductor").as_deref(),
            Some("old-session-id"),
            "Bare restart は store を無傷に保つ（新 root 用 — 旧会話をタブに残す）"
        );

        // Reset: 素の新規 session にするため記録を捨てる
        pool.restart_lane(&addr, RespawnMode::Reset)
            .expect("reset chat restart");
        assert_eq!(
            crate::lane::cc_session::last("vp", "conductor"),
            None,
            "Reset restart は resume の矢印を捨てる"
        );

        // chat 分岐は PTY を立てない = engine-less (pid=None) のまま Running が正常形
        let info = pool.get(&addr).expect("lane");
        assert_eq!(info.pid, None);
        assert_eq!(info.state, LaneState::Running);
    }

    /// moody-blues 指摘の根治（2026-07-17）: fresh の store 破棄は console_mode に依らない。
    /// 旧実装は chat 分岐内にだけあり、tui lane の New Session は spawn command から
    /// --resume を落とすだけで pointer file が残った — restart 直後の Diff::Update push が
    /// 旧 id を運び、session chip が旧 id を映し続けた。破棄の実体（`clear_fresh_lane_state`）
    /// を直接固定する（tui 分岐の restart_lane 全体は実 PTY spawn を伴うため unit test 対象外）。
    #[test]
    fn fresh_clear_wipes_stores_regardless_of_console_mode() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::conductor("vp");
        crate::lane::cc_session::record("vp", "conductor", "old-id").expect("record");
        LanePool::clear_fresh_lane_state(&addr, "echoes").expect("clear");
        assert_eq!(
            crate::lane::cc_session::last("vp", "conductor"),
            None,
            "mode に依らず fresh 破棄で pointer が消える"
        );
    }

    /// doc 38 落とし穴②: fresh restart は registry 上の**全 session**の会話 id を消し、
    /// registry も既定形（N=1）へ戻す。focused（や旧来の #1）だけ消すと副 session が
    /// resume され「New Session なのに前の会話が出る」嘘になる — その再演をここで塞ぐ。
    #[test]
    fn chat_fresh_restart_clears_all_sessions_and_registry() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::conductor("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);

        // session #2（codex）を追加し、#1 / #2 の両方に会話 id を記録する。
        let k2 = pool
            .create_chat_session(&addr, Some("codex"), false)
            .expect("create session");
        assert_eq!(k2, 2);
        crate::lane::cc_session::record("vp", "conductor", "cc-id-1").expect("record #1");
        crate::lane::codex_session::record("vp", "conductor#2", "0199-codex-id")
            .expect("record #2");
        // 副 session（codex）の replay 源にも会話を仕込む — fresh はこれも捨てるべき。
        crate::echoes::replay_log::append(
            "vp",
            "conductor#2",
            &crate::echoes::EchoesEvent::MessageChunk {
                text: "old codex reply".to_string(),
            },
        )
        .expect("replay log append #2");

        pool.restart_lane(&addr, RespawnMode::Reset)
            .expect("reset chat restart");

        assert_eq!(
            crate::lane::cc_session::last("vp", "conductor"),
            None,
            "session #1 の会話 id が消える"
        );
        assert_eq!(
            crate::lane::codex_session::last("vp", "conductor#2"),
            None,
            "副 session (#2) の会話 id も消える — fresh は全 session を知る"
        );
        assert!(
            crate::echoes::replay_log::load("vp", "conductor#2").is_empty(),
            "副 session (#2) の replay 源も消える（残すと New Session なのに前会話が replay される）"
        );
        let reg = crate::lane::session_registry::load("vp", "conductor", "echoes");
        assert_eq!(reg.sessions.len(), 1, "registry は既定形（N=1）へ戻る");
        assert_eq!(reg.focused, 1);
    }

    /// doc 38 落とし穴③: console_mode ガードは focused session にのみ適用される。
    /// - focused の ensure は Tui mode で「mode=chat が必要」で弾かれる（従来どおり）
    /// - 非 focused の ensure は mode ガードを**通過**し、engine 能力（agy = Act II host なし）
    ///   まで到達して弾かれる = ガードが session 経路に流用されていない証跡
    #[tokio::test]
    async fn console_mode_guard_applies_only_to_focused_session() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::conductor("vp");
        let mut pool = LanePool::new();
        insert_lane(
            &mut pool,
            &addr,
            crate::lane::console_mode::ConsoleMode::Tui,
        );
        let router = std::sync::Arc::new(crate::process::topic_router::TopicRouter::new());

        // 非 focused の agy session を作る（focused は #1 のまま）。
        let k = pool
            .create_chat_session(&addr, Some("agy"), false)
            .expect("create agy session");

        // focused（#1、省略時）は mode ガードで弾かれる。
        let err = pool
            .ensure_chat_engine(&addr, None, &router)
            .expect_err("Tui mode の focused ensure は Err");
        assert!(
            err.to_string().contains("console mode=chat が必要"),
            "focused は mode ガード: {err}"
        );

        // 非 focused は mode ガードを通過し、engine 能力の防壁で弾かれる。
        let err = pool
            .ensure_chat_engine(&addr, Some(k), &router)
            .expect_err("agy session の ensure は Err");
        assert!(
            err.to_string().contains("Act II chat host を持ちません"),
            "非 focused は mode ガードを通過して能力防壁に到達する: {err}"
        );
    }

    /// session 解決の基本則: 省略 = focused、実在しない key は Err、未知 stand の create は Err。
    /// registry file 不在（N=1 特殊ケース）でも focused=1 に解決される。
    #[test]
    fn resolve_chat_session_defaults_and_validates() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::conductor("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);

        // registry file 不在 = N=1 特殊ケース（focused=1、stand は lane の stand）。
        let r = pool.resolve_chat_session(&addr, None).expect("resolve");
        assert_eq!((r.key, r.stand.as_str(), r.focused), (1, "echoes", true));

        // 実在しない session key は Err（黙って focused に落とさない）。
        assert!(pool.resolve_chat_session(&addr, Some(9)).is_err());

        // 未知 stand の session は作れない（engine を一生持てない）。
        assert!(
            pool.create_chat_session(&addr, Some("nonsense"), true)
                .is_err()
        );

        // create(focus=true) で focused が移り、省略解決も追随する。
        let k = pool
            .create_chat_session(&addr, Some("codex"), true)
            .expect("create");
        let r = pool.resolve_chat_session(&addr, None).expect("resolve");
        assert_eq!((r.key, r.stand.as_str(), r.focused), (k, "codex", true));
        // 旧 #1 は非 focused として引き続き解決できる。
        let r1 = pool
            .resolve_chat_session(&addr, Some(1))
            .expect("resolve #1");
        assert!(!r1.focused);
        assert_eq!(r1.stand, "echoes");
    }

    /// doc 38 Phase 3: session remove — engine slot drop + 会話 id 破棄 + focused fallback。
    /// tab を閉じたのに床（Act I）で会話が蘇る嘘（session #1 の bare label）をここで固定する。
    #[test]
    fn remove_chat_session_drops_slot_and_conversation_ids() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::conductor("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);

        // #2(codex) を focused で追加し、両 session に会話 id を記録
        let k2 = pool
            .create_chat_session(&addr, Some("codex"), true)
            .expect("create #2");
        crate::lane::cc_session::record("vp", "conductor", "cc-id-1").expect("record #1");
        crate::lane::codex_session::record("vp", "conductor#2", "0199-codex-id")
            .expect("record #2");
        // #2（codex）の replay 源にも会話を仕込む — close で消えるべき。
        crate::echoes::replay_log::append(
            "vp",
            "conductor#2",
            &crate::echoes::EchoesEvent::MessageChunk {
                text: "codex reply".to_string(),
            },
        )
        .expect("replay log append #2");

        // focused(#2) を remove → focus は #1 へ、#2 の会話 id は消える
        let focused = pool.remove_chat_session(&addr, k2).expect("remove #2");
        assert_eq!(focused, 1);
        assert_eq!(
            crate::lane::codex_session::last("vp", "conductor#2"),
            None,
            "閉じた session の会話 id は破棄される"
        );
        assert!(
            crate::echoes::replay_log::load("vp", "conductor#2").is_empty(),
            "閉じた session の replay 源も破棄される（床で会話が蘇る嘘を防ぐ）"
        );
        assert_eq!(
            crate::lane::cc_session::last("vp", "conductor").as_deref(),
            Some("cc-id-1"),
            "残る session (#1) の会話 id は無傷"
        );

        // 最後の 1 本は取り除けない（fresh restart が正道）
        assert!(pool.remove_chat_session(&addr, 1).is_err());
    }

    /// list_chat_sessions は registry + 会話 id（engine store）+ focused を突き合わせる。
    /// session label（#1 = 素の lane 名 / #2 = `<lane>#2`）で読むことをここで固定する。
    #[test]
    fn list_chat_sessions_joins_registry_and_stores() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::conductor("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);

        pool.create_chat_session(&addr, Some("codex"), false)
            .expect("create");
        crate::lane::cc_session::record("vp", "conductor", "cc-id-1").expect("record");

        let sessions = pool.list_chat_sessions(&addr).expect("list");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].key, 1);
        assert_eq!(sessions[0].engine_session_id.as_deref(), Some("cc-id-1"));
        assert!(sessions[0].focused);
        assert!(!sessions[0].live, "engine 未 spawn は live=false");
        assert_eq!(sessions[1].key, 2);
        assert_eq!(sessions[1].stand, "codex");
        assert_eq!(
            sessions[1].engine_session_id, None,
            "Draft session は会話 id を持たない（doc 38 §1.1）"
        );
    }

    #[test]
    fn lane_address_display_conductor_and_performer() {
        assert_eq!(LaneAddress::conductor("vp").to_string(), "vp/conductor");
        assert_eq!(
            LaneAddress::performer("vp", "foo").to_string(),
            "vp/performer/foo"
        );
    }

    // deliver_nudge の並行 interleave 防止 (#674 race) の要は「同一 lane が同じ lock を共有し、
    // 別 lane は別 lock を持つ」こと。PTY 無しで検証できる直列化 invariant をここで固定する。
    #[test]
    fn nudge_lock_is_stable_per_lane_and_distinct_across_lanes() {
        let pool = LanePool::new();
        let a = LaneAddress::conductor("proj-a");
        let b = LaneAddress::conductor("proj-b");

        let a1 = pool.nudge_lock_handle(&a).unwrap();
        let a2 = pool.nudge_lock_handle(&a).unwrap();
        let b1 = pool.nudge_lock_handle(&b).unwrap();

        // 同一 lane → 同じ Arc<Mutex>（ptr 一致）。これがないと 2 本の deliver_nudge が
        // 別々の lock を取り直列化が効かず、phase1 の text が interleave する。
        assert!(
            std::sync::Arc::ptr_eq(&a1, &a2),
            "同一 lane は同じ nudge lock を共有すべき"
        );
        // 別 lane → 別 lock。cross-lane の head-of-line blocking を避ける。
        assert!(
            !std::sync::Arc::ptr_eq(&a1, &b1),
            "別 lane は別の nudge lock を持つべき"
        );
    }

    // 同一 lane の 2 本目の deliver_nudge が 1 本目の critical section 完了まで待たされること
    // （＝直列化が実際に効く）を、per-lane lock の hold 経由で確認する。
    #[tokio::test]
    async fn nudge_lock_serializes_same_lane() {
        let pool = LanePool::new();
        let addr = LaneAddress::conductor("proj");
        let lock = pool.nudge_lock_handle(&addr).unwrap();

        // 1 本目が critical section を保持中は、同 lane の 2 本目 (try_lock) は取れない。
        let held = lock.lock().await;
        let second = pool.nudge_lock_handle(&addr).unwrap();
        assert!(
            second.try_lock().is_err(),
            "同一 lane の nudge は先行 critical section 完了まで待つべき"
        );
        drop(held);
        assert!(
            second.try_lock().is_ok(),
            "先行 nudge 完了後は次の nudge が進める"
        );
    }

    #[tokio::test]
    async fn lane_pool_with_conductor_pre_populates_one_lane() {
        // Phase 1: LanePool::with_conductor は内部で PtySlot::spawn → tokio::task::spawn_blocking する。
        // 純 sync test だと runtime が無くて panic するので #[tokio::test] にする。
        let pool = LanePool::with_conductor("vp", "/tmp");
        assert_eq!(pool.count(), 1);
        let lanes = pool.list();
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].kind, LaneKind::Conductor);
        assert_eq!(lanes[0].stand, "echoes"); // default は "echoes" (PR-pre2 で "hd" → "echoes" rename)
    }

    #[test]
    fn lane_kind_serde_snake_case() {
        assert_eq!(
            serde_json::to_string(&LaneKind::Conductor).unwrap(),
            "\"conductor\""
        );
        assert_eq!(
            serde_json::to_string(&LaneKind::Performer).unwrap(),
            "\"performer\""
        );
    }

    #[test]
    fn lane_kind_serde_worker_rejected() {
        // Worker → Performer rename 完結後: legacy `"worker"` は serde alias から外れた。
        // `#[serde(alias = "worker")]` 削除の回帰ガード。
        // "worker" が LaneKind として deserialize されると旧 SP wire から届いた
        // stale payload を黙って受理してしまう — それを防ぐ。
        let result: Result<LaneKind, _> = serde_json::from_str("\"worker\"");
        assert!(
            result.is_err(),
            "\"worker\" は LaneKind として受理されてはならない (alias 削除済)"
        );
    }

    #[test]
    fn lane_info_worker_status_alias_rejected() {
        // `worker_status` serde alias 削除の回帰ガード。
        // 旧 SP が `worker_status` キーで送ってきても、 新 SP は performer_status: None として扱う
        // (= 情報損失は許容、 crash やパース失敗より優先)。
        // `#[serde(default)]` が残っているので unknown field は無視され None になる。
        let json = r#"{
            "address": {"project": "vp", "kind": "performer", "name": "foo"},
            "kind": "performer",
            "name": "foo",
            "state": "running",
            "stand": "echoes",
            "created_at": "2026-05-26T00:00:00Z",
            "cwd": "/tmp",
            "worker_status": {"branch": "main", "ahead": 0, "behind": 0, "is_merged": false, "has_changes": false}
        }"#;
        let info: LaneInfo = serde_json::from_str(json).expect("パース自体は成功する");
        assert!(
            info.performer_status.is_none(),
            "worker_status キーは performer_status に流れ込まない (alias 削除済)"
        );
    }

    #[test]
    fn parse_address_conductor_and_performer() {
        let conductor = LanePool::parse_address("vp/conductor").unwrap();
        assert_eq!(conductor, LaneAddress::conductor("vp"));

        let performer = LanePool::parse_address("vp/performer/foo").unwrap();
        assert_eq!(performer, LaneAddress::performer("vp", "foo"));

        // CJK / kebab-case project name も通る
        let conductor2 = LanePool::parse_address("vantage-point/conductor").unwrap();
        assert_eq!(conductor2, LaneAddress::conductor("vantage-point"));

        // 不正
        assert!(LanePool::parse_address("vp").is_none()); // / 無し
        assert!(LanePool::parse_address("/conductor").is_none()); // project 空
        assert!(LanePool::parse_address("vp/foo").is_none()); // 未知 kind
        assert!(LanePool::parse_address("vp/performer/").is_none()); // performer name 空
        // 旧 "worker" token は受理しない
        assert!(LanePool::parse_address("vp/worker/foo").is_none());

        // 後方互換: conductor/performer rename 前の "lead"/"wing" address も受理する
        // (既存 session.json の active lane / 既存 wire address を orphan にしないため)
        assert_eq!(
            LanePool::parse_address("vp/lead").unwrap(),
            LaneAddress::conductor("vp")
        );
        assert_eq!(
            LanePool::parse_address("vp/wing/bar").unwrap(),
            LaneAddress::performer("vp", "bar")
        );
    }

    /// tmux decoupling PR2: 旧 wire payload (tmux field 入り) が新 LaneInfo に decode できる
    /// （unknown field は serde が無視 = 旧 client / 旧 DB descriptor との後方互換）。
    #[test]
    fn lane_info_decodes_legacy_payload_with_tmux_field() {
        let legacy = r#"{
            "address": {"project": "vp", "kind": "conductor"},
            "kind": "conductor",
            "state": "running",
            "stand": "echoes",
            "created_at": "2026-05-01T00:00:00Z",
            "cwd": "/tmp",
            "tmux": [{"stand": "echoes", "session": "vp-vp-conductor-echoes", "mode": "tmux"}]
        }"#;
        let info: LaneInfo = serde_json::from_str(legacy).expect("legacy payload decodes");
        assert_eq!(info.address, LaneAddress::conductor("vp"));
    }

    // ========================================================================
    // Phase 2 (Step E) — Lane lifecycle diff push (SystemEvent + Diff<I, P>)
    // ========================================================================

    #[test]
    fn lane_diff_add_serde_round_trip() {
        // Diff::Add { payload: LaneInfo } の wire 形式 + decode
        let info = LaneInfo {
            console_mode: Default::default(),
            id: Default::default(),
            address: LaneAddress::performer("vp", "sub"),
            kind: LaneKind::Performer,
            name: Some("sub".to_string()),
            state: LaneState::Running,
            stand: "hd".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            pid: Some(12345),
            cwd: "/tmp".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            flow_state: None,
        };
        let diff: LaneDiff = Diff::Add {
            payload: info.clone(),
        };
        let json = serde_json::to_string(&diff).unwrap();
        assert!(json.contains("\"kind\":\"add\""), "got: {}", json);
        assert!(json.contains("\"payload\""), "got: {}", json);

        let restored: LaneDiff = serde_json::from_str(&json).unwrap();
        match restored {
            Diff::Add { payload } => {
                assert_eq!(payload.address, info.address);
            }
            _ => panic!("expected Diff::Add"),
        }
    }

    #[test]
    fn lane_diff_remove_serde_round_trip() {
        // Diff::Remove { id: LaneAddress } で id のみ送る wire 形式
        let addr = LaneAddress::performer("vp", "osc");
        let diff: LaneDiff = Diff::Remove { id: addr.clone() };
        let json = serde_json::to_string(&diff).unwrap();
        assert!(json.contains("\"kind\":\"remove\""), "got: {}", json);
        assert!(json.contains("\"id\""), "got: {}", json);

        let restored: LaneDiff = serde_json::from_str(&json).unwrap();
        match restored {
            Diff::Remove { id } => assert_eq!(id, addr),
            _ => panic!("expected Diff::Remove"),
        }
    }

    #[test]
    fn system_event_lane_serde_flattens_inner_diff() {
        // SystemEvent::Lane(LaneDiff) の wire 形式は
        // {"scope": "lane", "kind": "add", "payload": {...}} のように
        // outer scope tag + inner Diff tag が同 level に flatten される。
        let info = LaneInfo {
            console_mode: Default::default(),
            id: Default::default(),
            address: LaneAddress::conductor("vp"),
            kind: LaneKind::Conductor,
            name: None,
            state: LaneState::Running,
            stand: "hd".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            pid: None,
            cwd: "/tmp".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            flow_state: None,
        };
        let event = SystemEvent::Lane(Diff::Add {
            payload: info.clone(),
        });
        let json = serde_json::to_string(&event).unwrap();
        // outer: scope tag
        assert!(
            json.contains("\"scope\":\"lane\""),
            "scope tag missing, got: {}",
            json
        );
        // inner: Diff::kind tag が同 level に flatten される (serde internally tagged の挙動)
        assert!(
            json.contains("\"kind\":\"add\""),
            "inner kind missing, got: {}",
            json
        );

        let restored: SystemEvent = serde_json::from_str(&json).unwrap();
        match restored {
            SystemEvent::Lane(Diff::Add { payload }) => {
                assert_eq!(payload.address, info.address);
            }
            _ => panic!("expected SystemEvent::Lane(Diff::Add)"),
        }
    }
}
