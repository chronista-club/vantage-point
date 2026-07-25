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
//! `LanePool::with_root` で Conductor Lane 1 つ pre-populate。
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

// doc 44 P2: `LaneKind`（Conductor / Performer）は撤去。
//
// D4「lane 自身は役割状態を持たない」— lane は全て対等になり、開発起点は
// [`ROOT_LANE_NAME`] の予約名（将来は Host が持つポインタ）で表される。
// 旧 kind の唯一の実質は「conductor は project に 1 本・worktree を持たない」だが、
// それは **名前の一意性**（1 project に同名 lane は 1 本）で既に表現されている。

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

/// 開発起点 lane の予約名（doc 44 D4）。
///
/// 旧 `LaneKind::Conductor` の後継だが、**役割ではなく名前**である点が違う。
/// lane 側に「自分は conductor だ」という状態はなく、この名前を持つ lane が
/// たまたま開発起点である、という関係に退化した（P3 で Host のポインタに移る）。
///
/// この名前は `LaneAddress` の Display 形が旧 conductor と一致する（`<project>/root`）
/// ように選んである — 既存の永続 address / wire を無傷で引き継ぐため。
///
/// **定義は `vp-paths` が唯一**（2026-07-21）。vp-app が同名定数を独自に持っていて
/// 「同値でなければ address が食い違う」をコメントの約束で担保していたため、
/// 定義ごと共有 crate へ畳んだ。ここは re-export。
pub use vp_paths::ROOT_LANE_NAME;

/// Lane の address — Pool key
///
/// 表示形 (`Display` 実装): `"<project>/<name>"`  例: `"vp/root"` / `"vp/foo"`
///
/// doc 44 P2（フラット化）: 旧 `{ project, kind, name: Option<String> }` の 3-tuple から
/// **`{ project, name }` の 2-tuple** になった。旧構造は conductor だけ `name: None` という
/// 非対称を抱えており、それが「lane が役割を自意識する」構造の物理形だった（D4）。
///
/// ⚠️ performer の表示形が `<project>/performer/<name>` → `<project>/<name>` に変わる。
/// DB / session.json に残る旧形は [`LanePool::parse_address`] が受理して新形に正規化する
/// （lead/wing → root/performer の rename 時と同じ手当て）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LaneAddress {
    pub project: String,
    /// lane 名（人間可読、例: "foo"）。開発起点は [`ROOT_LANE_NAME`]。
    ///
    /// `default` は P2 以前に永続した descriptor を読むための互換。旧 `LaneAddress` は
    /// conductor だけ `name` を持たず（`skip_serializing_if` で省略）、DB の `lane.descriptor`
    /// にその形で入っている。既定値を予約名にすると、旧 conductor レコードは name 欠落 →
    /// `"root"`、旧 performer は `name: "foo"` がそのまま読める（余分な `kind` は
    /// unknown field として無視される）ので、**custom Deserialize なしで旧形が全部読める**。
    #[serde(default = "default_lane_name")]
    pub name: String,
}

/// [`LaneAddress::name`] の serde 既定値（P2 以前の永続 descriptor 互換、上記参照）。
fn default_lane_name() -> String {
    ROOT_LANE_NAME.to_string()
}

impl LaneAddress {
    /// 任意の lane を構築する（フラット化後の canonical な構築子）。
    pub fn new(project: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            name: name.into(),
        }
    }

    /// 開発起点 lane（予約名 [`ROOT_LANE_NAME`]）を構築する。
    pub fn root(project: impl Into<String>) -> Self {
        Self::new(project, ROOT_LANE_NAME)
    }

    /// 名前付き lane を構築する（旧 performer）。
    ///
    /// 旧 API 名を残しているのは呼び出し 100 箇所超の互換のため。フラット化後は
    /// [`Self::new`] と完全に同義で、「performer という種別」はもう存在しない。
    pub fn performer(project: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(project, name)
    }

    /// 開発起点 lane か（= 予約名を持つか）。
    pub fn is_root(&self) -> bool {
        self.name == ROOT_LANE_NAME
    }

    // `tmux_session_name` / `tmux_session_prefix` (Phase 1a の deterministic tmux 名導出) は
    // tmux decoupling PR2 で退役。 lane の identity は Display 形 (`<project>/<name>`)
    // ただ一つ (design doc §13.2 — sanitize 形は tmux の「`/` 禁止」制約由来だった)。
}

impl fmt::Display for LaneAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.project, self.name)
    }
}

/// Phase 2 (Step E): エンティティ lifecycle の diff event を表現する generic ADT。
///
/// - `I` = identifier 型 (削除時のみ必要、 例: `LaneAddress`)
/// - `P` = payload 型 (add/update 時の full state、 例: `LaneInfo`)
///
/// caller で event 発生 → AppState の broadcast channel に publish → subscriber が
/// World 側 cache を realtime sync する primitive。
///
/// doc 44 P1 (fold-in): subscriber は旧「SP の QUIC registry push」から、project 自身の
/// lanes publish task（`process/server.rs` の `publish_lanes`）に替わった。同一プロセスに
/// なったので push は World の集約 view への map 書き込みに退化している。
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
/// `state.system_event_tx.send(SystemEvent::*)` で publish、project の lanes publish task
/// (`publish_lanes`) が受けて World の集約 view を更新する（doc 44 P1 fold-in で
/// 旧 QUIC registry push から置き換わった）。
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
// `Lane(LaneDiff)` は 352 byte、`LanesReordered` は 0 byte で size 差が出る。
// Box 化はしない: 本 enum は broadcast channel (容量 64) を流れるだけで滞留せず、
// 最悪でも 22KB。対して `SystemEvent::Lane(Diff::*)` の構築点は複数あり、
// Box 化はそこ全部に `Box::new` を撒く割に得るものが無い。
#[allow(clippy::large_enum_variant)]
pub enum SystemEvent {
    /// Lane lifecycle diff (Phase 2 Step E)
    Lane(LaneDiff),
    /// 帳簿由来の **snapshot 投影**が変わった（並び順 / 開発起点 …、doc 44 §12）。
    ///
    /// **個々の lane は何も変わっていない**ので `Lane(Diff::*)` では表せない
    /// （Diff は per-lane の差分で、偽の Add/Update を流すと購読側が実在しない
    /// 変化に反応する）。snapshot 全体の性質なので独立 variant にする。
    ///
    /// ⚠️ 旧名 `LanesReordered` は「並び替え専用」に読めたため、**同じ性質の
    /// 起点変更で撃ち忘れ**が起きた（起点が 5s tick まで sidebar に載らなかった）。
    /// 帳簿が snapshot の見え方を変えたら、種類を問わずこれを撃つ。
    LanesProjectionChanged,
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
    // doc 44 P2: `kind` / `name` を撤去。どちらも `address` が持つ情報の複製で、
    // 真実源が 2 つある状態だった（`address.kind` / `address.name` と同値）。
    // kind は概念ごと消え、name は `address.name` が唯一の在処になる。
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
    /// doc 37: この lane の **active engine の** session id（claude=cc_session / codex=thread id /
    /// grok=ACP sessionId。shell は None）。Echoes 共通ヘッダの session chip 用（表示専用 —
    /// resume に使うのは registry の会話 id / `cc_session_id` 側）。doc 40: 供給は registry
    /// （root session の conversation）に一本化。serde default + skip で wire 後方互換。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_session_id: Option<String>,
    /// doc 39 P4-C: この lane の **root session の stand**（= slot に載る engine 種別）。Act I の
    /// session chip prefix の供給源（`stand` は lane 作成時固定なので cross-engine root では slot の
    /// engine と食い違う — chip が旧 engine の prefix で点く）。`engine_session_id` と同じ
    /// [`Self::refresh_engine_session_id`] で populate。root entry 不在は None = vp-app 側が従来の
    /// lane `stand` に fallback。serde default + skip で wire 後方互換。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_stand: Option<String>,
    /// doc 40 §3: lane の session 構造（registry snapshot — focused / root /
    /// sessions[{key, stand, conversation}]）。LaneInfo を「lane の完全な descriptor」に
    /// する一歩（cwd は既在、sessions が最後の外付けだった）— chip とタブの供給を同一
    /// snapshot に揃える土台。populate は [`Self::refresh_engine_session_id`]（enrich 供給点）。
    /// serde default + skip で wire 後方互換。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<crate::lane::session_registry::SessionRegistry>,
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
        // doc 39 P4-C: chip prefix は root session の stand（= slot の engine）で決める。lane 固定の
        // `self.stand` は cross-engine root で slot と食い違うため、root entry の stand を別 field で運ぶ。
        self.engine_stand = root.map(|s| s.stand.clone());
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
///
/// **doc 46 P5**: lane の住人（session）に紐づく入れ物は 3 つとも `(lane, session)` 粒度で持つ:
/// `pty_slots`（Act I の端末）/ `term_attaches`（その双子の Term grid）/ `chat_engines`（Act II）。
/// root（doc 39「座と化身」）が特別なのは **lane の代表**（mailbox / pid / Dead 判定 / 省略時の
/// 解決先）である点だけで、「端末を持てるのは root だけ」という制約はもう無い。
#[derive(Default)]
pub struct LanePool {
    lanes: HashMap<LaneAddress, LaneInfo>,
    /// A5-2: 各 slot の実 PtySlot (子 process と PTY を保持)
    ///
    /// key は **(lane, session_key) の 2 段 map**（doc 46 P5 — `chat_engines` と同型）。
    /// 旧実装は lane に 1 本だったため「Act I になれるのは root session だけ」という制約が
    /// あったが、それは lane の性質ではなく **slot の枚数**が作っていた制約だった。
    /// タプル key ではなく入れ子にしたのは、`chat_engines` と同じ入れ子の高さで
    /// 「1 session = 高々 1 エンジン」を検査できるため（lookup ごとの addr clone も不要）。
    ///
    /// spawn 失敗 / 未 spawn の Lane は entry なし (state=Dead で record される)。
    /// `Mutex` wrap は PtySlot が Send-only (内部 Box<dyn Write+Send> 等) で Sync でないため、
    /// AppState が `Arc<RwLock<LanePool>>` で thread-shared に必要
    pty_slots: HashMap<
        LaneAddress,
        HashMap<SessionKey, std::sync::Mutex<crate::daemon::pty_slot::PtySlot>>,
    >,
    /// Stage 1 (ADR-0001): 各 slot の Rust 側 alacritty Term<T> attach。
    ///
    /// ⚠️ **`pty_slots` の双子**。key の形も lifecycle も必ず一致させる
    /// （insert / remove / restart / dead 検出の全経路で対）。片方だけ動かすと
    /// 端末出力の経路が**コンパイラを通ったまま無音で壊れる**（doc 44 §11「1 辺が 2 仕事」型）。
    /// task は spawn_blocking で 1 slot = 1 task、 broadcast::Receiver を消費。
    term_attaches:
        HashMap<LaneAddress, HashMap<SessionKey, crate::terminal::term_attach::TermAttach>>,
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
    /// **エンジン排他の法**（doc 38 §2 で session 粒度 → doc 46 P5 で slot 側も session 粒度）:
    /// - **session 内は 1 会話 1 エンジン**: 同一 session に 2 つの host は立たない
    ///   （inner map の key 一意性 + [`Self::ensure_chat_engine`] の存在 check が保証）
    /// - **同一 session の slot と chat engine は排他**: `pty_slots[addr][key]` xor
    ///   `chat_engines[addr][key]`。P5 で両者が同じ入れ子の高さになったので、lane 全体では
    ///   なく**当該 session の有無**を直接 check する（[`Self::ensure_chat_engine`]）
    /// - **session 同士は独立**（doc 38 §2「lane 内の session 同士は独立」。console_mode
    ///   ガードは focused にのみ適用 — doc 38 落とし穴③）
    chat_engines: HashMap<LaneAddress, HashMap<SessionKey, ChatEngineSlot>>,
}

// chat engine の所有型（ChatEngineSlot / ChatHost）と engine 軸の語彙（EngineKind）は
// `crate::echoes::engine` に移設した（doc 37 — chat スタックを echoes module に閉じ、
// 他プロジェクトへ切り出せる形にする）。LanePool は所有と排他の「法」だけを担う。
use crate::echoes::{ChatEngineSlot, ChatHost, EngineKind};
// session 層の語彙（doc 38）。registry は disk が SSOT（LanePool は cache を持たない —
// 「状態の供給を 1 系統に」の原則。読みは毎回 registry file、書きは registry module 経由）。
use crate::lane::session_registry::{self, SessionAct, SessionKey};

/// [`LanePool::restart_lane`] の slot（engine）張り替えモード（doc 39 P2 — 旧 `fresh: bool` の昇格）。
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
/// focused かどうか。ガード分岐（focused のみ act ガード）と host 構築に使う。
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
    /// **この session の** Act（doc 50 §4.6 A6）。lane 単位 `console_mode`（= root cache）と
    /// 混同しないこと — A6 で「root=tui のまま非 root だけ chat」が正規の構成になったので、
    /// chat 経路の gate は root の act ではなく**当該 session の act** で判定する必要がある
    /// （root で gate すると非 root の chat に replay が届かない。team-b review 2026-07-25）。
    pub act: SessionAct,
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
    /// doc 39: この session が lane の root（Act I slot に立ち mailbox を名乗る）か。
    /// GUI は root タブの × を隠す（backend の「root は remove 不可」の UI 反映）。
    pub root: bool,
    /// doc 50 §4.6 A6: この session の Act（見え方）。GUI の roster（term / chat のどちらの
    /// Pane を生やすか）と名札の kind badge が読む **唯一の供給源**。旧 lane 単位 console_mode
    /// による roster 導出はこの field に置き換わった（term になれるのが root だけ、の制約は A6 で消滅）。
    pub act: SessionAct,
    /// この session を Chat（Act II）にできるか（doc 50 §4.6 A6 ②）。
    ///
    /// 能力表（[`crate::echoes::EngineKind::chat_capable`]）は **server が SSOT**。client は
    /// この bool を読むだけにして、engine 名の型分岐を GUI 側に持たせない（shell の chat host が
    /// 実装された日に、client を触らず badge が生えるのが正しい形）。
    /// 名札の kind badge はこれが false なら**押せる見た目を出さない** — 押しても server に
    /// 弾かれるだけの「行き止まり」を作らないため（`newPaneChoices` と同じ規律）。
    pub chat_capable: bool,
}

/// [`LanePool::slot_inventory`] の 1 要素 — lane が持つ PTY slot 1 枚分の view。
///
/// doc 46 P5 の「UI 以外の読み手」（doc 47 §7 成立条件②）。slot は lane に 1 枚ではなく
/// session ごとになったので、**何枚あるか**を UI を通さずに読めるようにしておく。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SlotInfo {
    /// この slot が化身している session の key。
    pub session: SessionKey,
    /// slot の子 process（login shell）の pid。
    pub pid: u32,
    /// 子 process が生きているか（non-blocking try_wait）。
    pub alive: bool,
    /// この session が lane の root（= lane の代表、doc 39）か。
    pub root: bool,
    /// Term grid（TermAttach）が張られているか。`pty_slots` の双子が欠けていれば false
    /// （= capture が空を返す状態。両者は必ず対で動くので、通常は常に true）。
    pub attached: bool,
}

impl std::fmt::Debug for LanePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // PtySlot は Debug 不可、 keys のみ表示（doc 46 P5 で (lane, session) の 2 段になった）
        let slots: Vec<(&LaneAddress, Vec<&SessionKey>)> = self
            .pty_slots
            .iter()
            .map(|(addr, sessions)| (addr, sessions.keys().collect()))
            .collect();
        f.debug_struct("LanePool")
            .field("lanes", &self.lanes)
            .field("pty_slots", &slots)
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
    pub fn with_root(project_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        let project_id = project_id.into();
        let cwd = cwd.into();
        let mut pool = Self::new();
        let addr = LaneAddress::root(&project_id);
        // doc 11 PR-B: default stand は "echoes" 固定 (config.default_stand での per-user 化は
        // 後続 PR、 LanePool::with_root は config を持たないため)。
        // user 設定がある場合の経路は HTTP API / lane_spawn_actor 経由で stand を明示指定する。
        // PR-pre2 (VP-118): "hd" → "echoes" rename。 mise task `vp:stand:echoes` (旧 hd)。
        let stand_name = "echoes";

        // doc 54 §8-11: conductor の**初回作成**（registry file 不在 = 一度も仕込みを持って
        // いない）は既定レンズを書く。with_root は毎 boot 呼ばれるので、file 不在を生成契機と
        // みなす — 以降の boot は既存 file を honor（= user の act 切替が boot で戻らない）。
        if !session_registry::exists(&project_id, "root") {
            let act = session_registry::default_act_for_stand(stand_name);
            if let Err(e) = session_registry::set_root_act(&project_id, "root", stand_name, act) {
                tracing::warn!(
                    "conductor 既定レンズの永続失敗（Tui 相当で継続）: project={project_id} err={e}"
                );
            }
        }
        // doc 47 §4: root session の act を boot で honor。chat の lane に PTY を立てない
        // （立てると echoes_submit がもう 1 本の engine を呼び、1 会話 2 エンジンになる）。
        // doc 53 R1: これは spawn 判断の入力としての registry 直読（投影に書き戻さない）。
        let root_act = session_registry::root_act(&project_id, "root");

        let (state, pid) = if root_act == SessionAct::Chat {
            // Chat mode: engine-less で登録（EchoesAgentHost は初回 submit で lazy spawn）。
            // pid=None + state=Running は chat lane の正常形（vp-app は sessions 由来の act で
            // respawn 判定を gate する — doc 33 §3 / doc 53 R1）。
            tracing::info!("Lane boot as chat mode (PTY skip): addr={}", addr);
            (LaneState::Running, None)
        } else {
            // tmux decoupling PR2: slot (login shell) + claude 注入の Rust-native spawn (design §13)。
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
                    // session=None = root（boot で立つ slot は lane の代表、doc 39）。
                    pool.insert_pty_slot(addr.clone(), None, slot, term_rx);
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
            // I1: conductor の安定 id を address (project, "root") で load_or_create
            id: crate::lane::lane_id::load_or_create(&project_id, "root"),
            address: addr.clone(),
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
            engine_stand: None,
            flow_state: None,
        };
        pool.lanes.insert(addr.clone(), info);
        // doc 50 §4.6 A6: **非 root の term session（act=Tui）も boot で復元する**。
        //
        // A6 で「非 root が term である」は registry に永続する一級の状態になったが、boot で
        // slot を立てるのは root だけだった。そのため World / project 再起動のあと（dogfood の
        // `VP_SWAP_RESTART_DAEMON=1` は毎回これ）**pane は出るのに中身が空で無反応**になる —
        // roster は registry から導出されるので pane は現れ、slot だけが居ない状態
        // （team-b review 2026-07-25 score 78。実機でも観測していたが原因を追っていなかった）。
        // root と同じ「前回状態キープ」の規律で eager に復元する。
        pool.restore_term_slots(&addr);
        pool
    }

    /// registry 上の **非 root の `act=Tui` session** に slot を立て直す（boot 復元）。
    ///
    /// root は呼び手が既に立てている（[`Self::with_root`] / lane spawn）。ここは「前回 term
    /// だった非 root」を戻す担当。失敗は warn で継続（1 枚立たなくても lane 全体は使える —
    /// graceful degrade。`open_slot_for_session` 側の法の check もそのまま効く）。
    ///
    /// ⚠️ **sync + 重い**: 1 session あたり `spawn_stand` の 800ms sync sleep がかかる。
    /// async 文脈から呼ぶなら `spawn_blocking` で隔離すること（`lane_spawn_actor` はそうして
    /// いる）。`with_root` からの呼び出しは `server.rs` が「起動 1 回だけなので軽微」と
    /// 受容している経路だが、その前提は **root 1 枚**のときのもの — 本関数は非 root 数に
    /// 比例して伸びるので、蓄積した term が増えたら起動が目に見えて遅くなる
    /// （team-b 4 回目 2026-07-25 の指摘。今は N が小さいので据え置き）。
    pub(crate) fn restore_term_slots(&mut self, addr: &LaneAddress) {
        let Some(info) = self.lanes.get(addr) else {
            return;
        };
        let lane_label = crate::process::stand_spawner::lane_label(addr).to_string();
        let reg = session_registry::load(&addr.project, &lane_label, &info.stand);
        let targets: Vec<SessionKey> = reg
            .sessions
            .iter()
            .filter(|s| s.key != reg.root && s.act == SessionAct::Tui)
            .map(|s| s.key)
            .collect();
        for key in targets {
            match self.open_slot_for_session(addr, key) {
                Ok(pid) => tracing::info!(
                    "term slot restored: addr={addr} session={key} pid={pid}（boot 復元）"
                ),
                Err(e) => tracing::warn!(
                    "term slot の boot 復元に失敗（pane は空で出る）: addr={addr} session={key}: {e}"
                ),
            }
        }
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
            // doc 44 P2: 旧 kind 比較の後継。開発起点（予約名）を先頭に置く要件は
            // 表示順の話であって lane の役割分岐ではないので、名前の判定で足りる。
            match (a.address.is_root(), b.address.is_root()) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => a
                    .created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.address.name.cmp(&b.address.name)),
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

    /// slot 側の session 指定を解決する（doc 46 P5）。
    ///
    /// ⚠️ **`None` の既定は root**（doc 39「座と化身」= lane の器に化身するのは root session）。
    /// chat 系（[`Self::resolve_chat_session`]）の `None` = focused と**意図的に違う**:
    /// slot は lane の設備で、その代表は focused ではなく root だから。
    ///
    /// registry は disk が SSOT なので毎回 file read（`resolve_chat_session` と同じ規律）。
    /// 呼び出し頻度が最も高いのは `write_to_lane`（keystroke ごと）だが、数百 byte の
    /// 1 file read で、既に走っている QUIC 往復に対して無視できる。実測で問題になったら
    /// 「lane に slot が 1 枚しかない時だけ短絡」ではなく（root 以外の 1 枚を root と
    /// 誤答するため）、resolve 済み key を wire で運ぶ方向で直す。
    fn slot_session(addr: &LaneAddress, session: Option<SessionKey>) -> SessionKey {
        session.unwrap_or_else(|| {
            session_registry::root(
                &addr.project,
                crate::process::stand_spawner::lane_label(addr),
            )
        })
    }

    /// lane の root session key（`slot_session(addr, None)` の公開版）。
    ///
    /// 「session 省略 = root」の解決規則（`payload_session_key` の doc）を **呼び手側から
    /// 明示的に引ける**ようにするもの。session 明示が必須な新経路（`session_set_act` 等）に
    /// 対して「root を指したい」と書けるのが用途。
    // 読み手: `restart_lane_orchestrated` が pump の張り直し範囲（root の slot だけ差し替わる）を
    // 決めるのに使う + テスト。slot_session は private なので、これが無いと外から root を
    // 名指しできない。
    pub fn root_session_key(addr: &LaneAddress) -> SessionKey {
        Self::slot_session(addr, None)
    }

    /// Phase 3-A: 既に spawn 済の PtySlot を (lane, session) 紐付けで insert。
    ///
    /// `session=None` は root（[`Self::slot_session`]）。boot / restart / performer spawn の
    /// 既存経路はすべて lane の代表 slot を立てるので `None` を渡す。
    ///
    /// ⚠️ **ここは配線であって門番ではない**（法の check は持たない — 既存 entry があれば
    /// 黙って replace する）。非 root session に slot を立てる入口は
    /// [`Self::open_slot_for_session`] で、法（1 session = 高々 1 エンジン）の check は
    /// そちらに置いてある。
    ///
    /// Stage 1 (ADR-0001): TermAttach も同期 spawn する。 `term_rx` は spawn_stand の
    /// 戻り値 (= broadcast::channel 作成と同時の initial_rx)、 reader_task が start する前に
    /// subscribe 済 = race フリー。 既存 entry があれば HashMap::insert で replace、
    /// 旧 TermAttach は Drop で handle.abort() (= restart 経路の再 attach に対応)。
    pub fn insert_pty_slot(
        &mut self,
        addr: LaneAddress,
        session: Option<SessionKey>,
        slot: crate::daemon::pty_slot::PtySlot,
        term_rx: tokio::sync::broadcast::Receiver<Vec<u8>>,
    ) {
        let key = Self::slot_session(&addr, session);
        self.pty_slots
            .entry(addr.clone())
            .or_default()
            .insert(key, std::sync::Mutex::new(slot));
        // grid dims は PtySlot の初期 winsize (120x48、 spawn_stand 呼び出し側) と一致させる。
        // 不一致 (旧 80x24) だと headless (vp-app 未 attach) lane の capture が 80 桁で再 wrap
        // されて崩れる (PR2 実機検証で発見)。 client attach 後は resize_lane が両者を同期する。
        let term_attach = crate::terminal::term_attach::TermAttach::spawn(term_rx, 120, 48);
        // ⚠️ pty_slots の双子。同じ key で必ず対に insert する（片方だけだと capture が
        // 無音で空になる / 逆に Dead slot の凍結画面が残り続ける）。
        self.term_attaches
            .entry(addr)
            .or_default()
            .insert(key, term_attach);
    }

    /// 1 枚の slot（+ 双子の TermAttach）を落とす。戻り値 = 実際に落ちたか。
    /// 空になった inner map は outer から除く（`chat_engines` と同じ規律 — 空殻を残さない）。
    fn drop_slot(&mut self, addr: &LaneAddress, key: SessionKey) -> bool {
        // 順序: term_attaches → pty_slots（broadcast::Sender は pty_slots が保持。先に
        // Sender を落とすと attach task が Closed を見る前に消えるため、attach を先に畳む）。
        if let Some(attaches) = self.term_attaches.get_mut(addr) {
            attaches.remove(&key);
            if attaches.is_empty() {
                self.term_attaches.remove(addr);
            }
        }
        let Some(slots) = self.pty_slots.get_mut(addr) else {
            return false;
        };
        let dropped = slots.remove(&key).is_some();
        if slots.is_empty() {
            self.pty_slots.remove(addr);
        }
        dropped
    }

    pub fn remove(&mut self, addr: &LaneAddress) -> Option<LaneInfo> {
        // Phase 4-A: PtySlot も一緒に drop (= child kill 経由でプロセス停止)
        // PtySlot::Drop が child.kill() + child.wait() を呼ぶので zombie 防止。
        // Stage 1 (ADR-0001): TermAttach も同期 drop (JoinHandle::abort で task 終了)。
        // 順序: term_attaches → pty_slots → lanes (broadcast::Sender は pty_slots が保持)。
        //
        // doc 46 P5: lane ごと消えるので **全 session の slot** を drop する（outer entry を
        // 落とせば inner map ごと Drop = 各 PtySlot の child kill が走る）。
        self.term_attaches.remove(addr);
        self.pty_slots.remove(addr);
        // doc 33: chat engine も同時に drop（kill_on_drop + pump abort）。
        self.chat_engines.remove(addr);
        self.lanes.remove(addr)
    }

    // 要確認（audit 2026-07-18、先行実装の可能性）: LanePool の debug/metrics helper。
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.lanes.len()
    }

    /// Phase 5-D: spawn_with_fallback の 800ms early-exit window を抜けた後で、
    ///   Lane の child process (例: `claude --continue`) が後で exit した場合の検知。
    ///
    /// ## 動作
    /// 1. 全 PtySlot の `is_alive()` (= non-blocking try_wait) を check
    /// 2. dead な slot について:
    ///    - **root session の slot が死んだ時だけ** `LaneInfo.state` を `LaneState::Dead` に更新
    ///    - `pty_slots` / `term_attaches` から当該 slot を remove (Drop で child reap、 zombie 解消)
    /// 3. state transition した Lane の数を返す (caller が log 出力に使える)
    ///
    /// ## doc 46 P5: Dead 判定は root slot だけが持つ
    /// slot が session ごとになったので「slot が 1 枚死んだ」と「lane が死んだ」は別の事実に
    /// なった。**lane の代表は root**（doc 39「座と化身」— mailbox を名乗り、sidebar の
    /// pid/state を代表するのも root）なので、
    /// - root slot の死 → lane を Dead に落とす（従来と同じ。UI の respawn 動線が要る）
    /// - 非 root slot の死 → **その slot を畳むだけ**。lane は Running のまま
    ///   （同居している他の住人が生きているのに lane 全体を Dead と呼ぶのは嘘になる）
    ///
    /// ## 関連 memory
    /// - vantage-point Atlas の Phase 5-D dogfooding bundle (unison-kdl で zombie 観測)
    /// - PtySlot::is_alive (`crates/vantage-point/src/daemon/pty_slot.rs`)
    pub fn detect_and_mark_dead(&mut self) -> usize {
        // step 1: dead な (lane, session) を収集 (lock を持ったまま remove はできないので 2 段)
        let mut dead_slots: Vec<(LaneAddress, SessionKey)> = Vec::new();
        for (addr, slots) in &self.pty_slots {
            for (key, slot_mutex) in slots {
                if let Ok(mut slot) = slot_mutex.lock()
                    && !slot.is_alive()
                {
                    dead_slots.push((addr.clone(), *key));
                }
            }
        }

        // step 2: state 更新 + slot（+ 双子の TermAttach）を remove
        let mut transitioned = 0;
        for (addr, key) in dead_slots {
            // root 解決は disk read なので、死んだ slot がある lane のぶんだけ（= 稀）。
            let is_root = key == Self::slot_session(&addr, None);
            if is_root
                && let Some(info) = self.lanes.get_mut(&addr)
                && info.state != LaneState::Dead
            {
                tracing::warn!(
                    "Lane lifecycle: dead detected addr={} session={} prev_state={:?} pid={:?}",
                    addr,
                    key,
                    info.state,
                    info.pid
                );
                info.state = LaneState::Dead;
                transitioned += 1;
            }
            if !is_root {
                tracing::info!(
                    "slot lifecycle: 非 root slot が終了 addr={addr} session={key}（lane は Running のまま）"
                );
            }
            // TermAttach も同時に落とす (remove/restart_lane と順序統一)。 残すと Dead slot の
            // capture_lane が凍結した最終フレームを返し続ける (PR2 review B2)。
            // PtySlot Drop で child.kill() + child.wait() = zombie 解消
            self.drop_slot(&addr, key);
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
    /// fresh restart の state 破棄（「lane を素に戻す」の実体、act 非依存）。
    ///
    /// doc 38 落とし穴②「fresh が副を知らない」の再演防止で、対象は **registry 上の全 session**:
    /// - replay log（transcript を持たない engine の replay 源。残すと「New Session なのに
    ///   前の会話が replay される」嘘になる — session 単位に消す）
    /// - session registry 自体（既定形 N=1 へ — fresh 後の lane は「素の 1 session」。会話 id は
    ///   registry の SSOT なので registry clear で全 session の resume の矢印が消える。doc 40 PR-2 で
    ///   旧 cc/codex_sessions store が退役したため、per-session の store 破棄は不要になった）
    fn clear_fresh_lane_state(addr: &LaneAddress, default_stand: &str) -> anyhow::Result<()> {
        let lane_label = crate::process::stand_spawner::lane_label(addr).to_string();
        let reg = session_registry::load(&addr.project, &lane_label, default_stand);
        for s in &reg.sessions {
            let label = session_registry::session_label(&lane_label, s.key);
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
    /// `mode` は slot（engine）の張り替え方（doc 39 P2 で 旧 `fresh: bool` から昇格 —
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
        // doc 53 R1: 分岐用の act は fresh 破棄より**前**に読む（旧 root cache は破棄前の値を
        // 保持していた = その挙動を維持）。Reset は registry を既定形に戻すため、破棄後に
        // 読むと既定（Tui）に化けて **chat lane の Reset が PTY を立ててしまう**。
        let root_act = self.root_act(addr);

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
            // doc 46 P5: Reset は registry を既定形（N=1）へ戻す = 非 root session が
            // **registry 上から消える**。その slot を残すと「もう存在しない session の
            // 端末」が生き続ける（orphan）ので、Reset に限り全 slot を畳む。
            // Resume / Bare は registry 無傷なので、張り替えるのは root の slot だけ（後述）。
            self.term_attaches.remove(addr);
            self.pty_slots.remove(addr);
            // doc 50 §4.6 A6: **term の PTY replay file も消す**（Reset = 素に戻す）。
            //
            // ⚠️ **順序が要**: `PtySlot::drop` は最終 flush で replay を disk に書き戻すので、
            // 上の `clear_fresh_lane_state`（= 破壊より前）に混ぜると消したそばから復活する。
            // だからここ = `pty_slots.remove` の**直後**に置く。
            //
            // 消さないと **ghost replay**: registry を消すと採番が N=1 に戻り、次に作る session
            // は同じ key を再利用する → `build_stand_command_for_session` が同じ file path を
            // 返す → `load_replay_seed` が旧画面を新 console の scrollback に seed する =
            // 「Reset したはずの画面」が蘇る。`clear_replay_in` の doc は同じ機序を **lane
            // 再作成**について警告していたが、Reset は経路が別で漏れていた（root は以前から
            // replay を持つので pre-existing、A6 で非 root も持つようになり範囲が広がった。
            // team-b 5 回目 2026-07-25）。best-effort（消せなくても console は live で動く）。
            let lane_label = crate::process::stand_spawner::lane_label(addr);
            if let Err(e) = crate::daemon::pty_slot::clear_replay_in(
                &crate::config::vp_state_dir(),
                &addr.project,
                lane_label,
            ) {
                tracing::warn!(
                    "fresh restart: term replay の破棄に失敗（ghost replay の恐れ）: addr={addr}: {e}"
                );
            }
        }

        // doc 33: chat mode の lane の restart = chat engine の入れ替え（PTY は立てない）。
        // engine を drop するだけで、次の echoes_submit が新 engine を lazy spawn する。
        // fresh の意図は上の registry 破棄が state で運ぶ（engine は lazy spawn なので
        // 「今 fresh に立て直す」対象が存在しない）:
        // - `ensure_chat_engine` の resolve で registry の会話 id が None → --resume 無しで spawn
        //   → `EchoesAgentHost` が SessionInit で新 id を registry に書き戻す（SSOT 復旧）
        // - transcript replay-on-attach も参照先を失う → 前の会話を映さない
        //   （消さないと「New Session なのに前の会話が出る」嘘になる）
        // doc 53 R1: 分岐の問いは「root session の act」= intent。投影ではなく registry を直読する
        // （root 付け替え直後の restart で古い投影を読む事故 — doc 50 §4.7 の 15 例目 — が構造的に消える）。
        // 読むのは冒頭（fresh 破棄前）— 上の `root_act` 捕捉コメント参照。
        if root_act == SessionAct::Chat {
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
        // Stage 1 (ADR-0001): 順序は LanePool::drop_slot と一致 (term_attaches → pty_slots、
        // broadcast::Sender は pty_slots が保持なので task は次 iter で Closed 検知して exit)。
        //
        // doc 46 P5: 張り替えるのは **root session の slot** だけ（step 2 の
        // build_stand_command が root entry で engine / resume を決めるのと同じ主語）。
        // 同居している非 root slot は独立の住人なので巻き添えにしない。
        let root_key = Self::slot_session(addr, None);
        self.drop_slot(addr, root_key);

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
                // session=None = root（張り替えたのは root の slot）。
                self.insert_pty_slot(addr.clone(), None, slot, term_rx);
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

    /// Display 形 (`"<project>/root"` / `"<project>/performer/<name>"`) をパースして LaneAddress を作る。
    /// vp-app の sidebar から `lane:select` IPC の address (= `lane_address_key`) を逆変換するために使う。
    pub fn parse_address(s: &str) -> Option<LaneAddress> {
        let parts: Vec<&str> = s.splitn(3, '/').collect();
        match parts.as_slice() {
            // 旧 "lead" は開発起点の旧名 (conductor rename 前の session.json / wire address 互換)。
            [project, "lead"] if !project.is_empty() => Some(LaneAddress::root(*project)),
            // canonical: "<project>/<name>" (doc 44 P2 フラット化後)
            [project, name] if !project.is_empty() && !name.is_empty() => {
                Some(LaneAddress::new(*project, *name))
            }
            // 旧 3 分節形 "<project>/performer/<name>" (P2 以前の永続 address / wire) を
            // 新形に正規化して受理する。lead/wing → root/performer の rename 時と同じ手当て
            // で、DB (`lane` / `lane_lifecycle` の address 列) と session.json を無傷で引き継ぐ。
            [project, "performer" | "wing", name] if !project.is_empty() && !name.is_empty() => {
                Some(LaneAddress::new(*project, *name))
            }
            _ => None,
        }
    }

    /// slot の console 現在画面を text で返す（tmux decoupling: `capture-pane` の native 代替）。
    ///
    /// slot ごとに張られた TermAttach（alacritty grid、`insert_pty_slot` で配線済）から
    /// [`TermAttach::grid_text`](crate::terminal::term_attach::TermAttach::grid_text) を render。
    /// `session=None` は root（doc 46 P5 — 省略時は lane の代表 slot）。
    /// lane 不在 / attach 不在（spawn 失敗 = Dead 等）は None。
    pub fn capture_lane(&self, addr: &LaneAddress, session: Option<SessionKey>) -> Option<String> {
        let key = Self::slot_session(addr, session);
        self.term_attaches
            .get(addr)
            .and_then(|m| m.get(&key))
            .map(|t| t.grid_text())
    }

    /// lane が持つ slot の session key 一覧（昇順）。lane 不在 / slot ゼロは空。
    ///
    /// doc 46 P5 の「UI 以外の読み手」その 1（doc 47 §7 成立条件②）。
    /// `lane_capture` の失敗理由 / `lanes_list` の enrich から読まれる。
    pub fn slot_sessions(&self, addr: &LaneAddress) -> Vec<SessionKey> {
        let mut keys: Vec<SessionKey> = self
            .pty_slots
            .get(addr)
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default();
        keys.sort_unstable();
        keys
    }

    /// lane が持つ slot の一覧 view（session / pid / 生死 / root か / attach 有無）。
    ///
    /// doc 46 P5 の「UI 以外の読み手」その 2 — `vp lane slots` / `lane_slots` ask が読む。
    /// **書いたものが誰にも読まれない状態で出荷しない**ための消費側（`LaneId` の轍を踏まない）。
    pub fn slot_inventory(&self, addr: &LaneAddress) -> Vec<SlotInfo> {
        let root = Self::slot_session(addr, None);
        let Some(slots) = self.pty_slots.get(addr) else {
            return Vec::new();
        };
        let mut out: Vec<SlotInfo> = slots
            .iter()
            .map(|(key, slot_mutex)| {
                // Mutex は内部可変性なので `&self` のまま is_alive（try_wait）を撃てる。
                // lock 失敗（poisoned）は「読めない = 生死不明」なので alive=false に倒す。
                let (pid, alive) = match slot_mutex.lock() {
                    Ok(mut slot) => (slot.pid(), slot.is_alive()),
                    Err(_) => (0, false),
                };
                SlotInfo {
                    session: *key,
                    pid,
                    alive,
                    root: *key == root,
                    attached: self
                        .term_attaches
                        .get(addr)
                        .is_some_and(|m| m.contains_key(key)),
                }
            })
            .collect();
        out.sort_by_key(|s| s.session);
        out
    }

    /// 既存 Lane の PtySlot に新しい subscriber を追加 (PTY output を WS に流す等の用途)。
    /// `None` = address に対応する Lane が無い、 もしくは PtySlot が無い (state=Dead 等)。
    ///
    /// memory rule (mem_1CaTpCQH8iLJ2PasRcPjHv): Lane = Session Process。
    /// Phase 2 で vp-app が WS で attach する際、 既存 PtySlot に subscribe して
    /// 同じ PTY を複数 client が共有できる (broadcast channel ベース)。
    // 要確認（audit 2026-07-18、先行実装の可能性）: Phase 2 WS attach 用の先行 API。
    #[allow(dead_code)]
    pub fn subscribe_output(
        &self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
    ) -> Option<tokio::sync::broadcast::Receiver<Vec<u8>>> {
        let key = Self::slot_session(addr, session);
        let slot_mutex = self.pty_slots.get(addr)?.get(&key)?;
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
        session: Option<SessionKey>,
    ) -> Option<(Vec<u8>, tokio::sync::broadcast::Receiver<Vec<u8>>)> {
        let key = Self::slot_session(addr, session);
        let slot_mutex = self.pty_slots.get(addr)?.get(&key)?;
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

    /// 既存 slot の PtySlot に input を書き込む (WS から user 入力を受けた時に使う)。
    /// `Mutex<PtySlot>` を lock するので、 broadcast 経路と直交して同期書込み。
    /// `session=None` は root（doc 46 P5 — 省略時は lane の代表 slot）。
    pub fn write_to_lane(
        &self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
        data: &[u8],
    ) -> anyhow::Result<()> {
        let key = Self::slot_session(addr, session);
        let slot_mutex = self
            .pty_slots
            .get(addr)
            .and_then(|m| m.get(&key))
            .ok_or_else(|| anyhow::anyhow!("Lane has no PtySlot: {} (session={})", addr, key))?;
        let mut slot = slot_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("PtySlot mutex poisoned: {} (session={})", addr, key))?;
        slot.write(data)
    }

    // =========================================================================
    // Console engine slot
    //
    // **法: 1 session = 高々 1 エンジン**（0 = Draft / 1 = PTY か headless の一方）。
    // 禁止したいのは同一 session に PTY と headless が同居する状態 — 会話 id は 1 つなのに
    // 書き手が 2 本になる（= 1 会話 2 エンジン）。
    //
    // lane は N session を持つ（doc 38）。**doc 46 P5 で `pty_slots` も `(lane, session)` に
    // なった**ので、「Act I になれるのは root session だけ」という制約は外れた — それは
    // lane の性質ではなく **slot の枚数**が作っていた制約だった。root が特別なのは
    // 「lane の代表（器に化身する = mailbox / pid / Dead 判定）」であることだけで、
    // 端末を持てる資格の話ではなくなった（doc 39「座と化身」）。
    //
    // これで法は型と同じ高さで検査できる: `pty_slots[addr][key]` と `chat_engines[addr][key]`
    // の**同一 key に両方が居ないこと**（= 1 session に書き手が 2 本にならない）。
    //
    // 排他は set_session_act / ensure_chat_engine（chat engine 側）と
    // [`LanePool::open_slot_for_session`]（slot 側）の 3 箇所だけが engine を作ることで保証。
    // `insert_pty_slot` 自体は「配線」であって門番ではない（boot / restart / performer spawn の
    // root 経路は mode ガードを通った後に呼ぶ）。**非 root slot を立てる入口は
    // `open_slot_for_session` 1 つ**で、そこに「同 session に chat engine が居ないこと」の
    // check を置いた（逆方向は ensure_chat_engine が持つ = 両向きが揃った）。
    //
    // ⚠️ 旧記述「1 lane = 高々 1 エンジン（pty_slots xor chat_engines）= 1 cc_session」は
    // doc 33（Act I/II 排他）時代のもので、`chat_engines` が session ごとの map になった
    // doc 38 の時点で**既に事実と違っていた**。型は正しく、この「法」の宣言だけが古かった。
    // =========================================================================

    /// **root session の act**（doc 53 R1 — 旧 `LaneInfo.console_mode` 投影の置き換え）。
    ///
    /// 投影を持たず registry（SSOT）を直読する。理由は乖離の構造的排除で、投影時代は
    /// root を動かす動詞が同期を 1 つ忘れるたびに「PTY が永久に立たない」「1 会話 2 engine」に
    /// 化けていた（doc 50 §4.7 の 15 例目）。読み手は restart / focus / remove / capture 案内 /
    /// nudge で、いずれも**人間の操作頻度**（disk 1 file、数百 byte）。
    ///
    /// ⚠️ **これは intent（何であるべきか）であって実体ではない**。「今 PTY に打てるか」を
    /// 訊きたいなら [`Self::has_slot`] 系（実体）を見ること — 混ぜると boot 窓と死んだ slot で
    /// 誤る（doc 53 §8.3）。
    pub fn root_act(&self, addr: &LaneAddress) -> SessionAct {
        let lane_label = crate::process::stand_spawner::lane_label(addr);
        session_registry::root_act(&addr.project, lane_label)
    }

    /// lane が pool に実在するか（旧 `console_mode(addr).is_none()` の流用を明示化、doc 53 §8.4）。
    pub fn contains(&self, addr: &LaneAddress) -> bool {
        self.lanes.contains_key(addr)
    }

    /// doc 46 P5 **producer**: 新しい console を 1 枚立てる（非 root session に slot を立てる
    /// production 経路）。戻り値 = 採番した session key と slot の pid。
    ///
    /// ## なぜ「session の採番」と一体なのか（doc 46 §1.5）
    ///
    /// Pane（console 1 枚）と session は **1:1** で、**Pane は必ず新しい session id で始まる**。
    /// 「既存の会話をもう 1 枚開く」はしない（どちらが真かが曖昧になる）ので、この動詞は
    /// 「session 採番 → その session に slot」を 1 つにしている。Act=Chat 固定の
    /// [`Self::create_chat_session`] の Act I 版に当たる。
    ///
    /// ## 決めごと
    ///
    /// - Act は **Tui**（console だから）。root session の act は**見ない** — Act は session の
    ///   属性（doc 46 §1.4、P4 で移設済）なので、root=chat の lane にも console を 1 枚足せる
    /// - **focused は動かさない**（`focus=false`）。focused は chat 系動詞（submit / interrupt）の
    ///   既定の宛先で、そこを PTY を持つ session に向けると次の submit が法（1 session = 高々 1
    ///   エンジン）で拒否される。console の注視は Pane 側の話（doc 46 §1.6 = client 所有）
    /// - engine は **明示 > root からの引き継ぎ**（[`Self::prepare_new_root_session`] と同じ規律）
    /// - **root は動かさない** — mailbox `agent@<lane>` / pid / Dead 判定の代表は root のまま
    ///   （doc 40 §4-1 の据え置き。同居人は「読み書きできる console」であって mailbox の主ではない）
    pub fn open_new_slot(
        &mut self,
        addr: &LaneAddress,
        stand_override: Option<&str>,
    ) -> anyhow::Result<(SessionKey, u32)> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        let lane_stand = info.stand.clone();
        let lane_label = crate::process::stand_spawner::lane_label(addr).to_string();
        let reg = session_registry::load(&addr.project, &lane_label, &lane_stand);
        // engine: 明示指定 > 現 root の stand（doc 46 P2 要件 4 と同じ選び方）。
        let stand = match stand_override.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => reg
                .sessions
                .iter()
                .find(|s| s.key == reg.root)
                .map(|s| s.stand.clone())
                .unwrap_or_else(|| lane_stand.clone()),
        };
        // 未知 stand は入口で弾く。`build_stand_command` は未知名を shell 層へ graceful に
        // 落とすので、通すと「engine が起動しない console」が黙って生まれる（行き止まりの
        // Pane を作らない = doc 46 §5.2 と同じ判断）。`"shell"` は engine 無しが意図なので通す。
        if EngineKind::from_stand(&stand).is_none() && stand != "shell" {
            anyhow::bail!(
                "engine が未知の stand では console を立てられません（addr={addr}, stand={stand}）。\
                 engine を明示指定してください（echoes / codex / grok / opencode / shell）"
            );
        }
        // doc 47 §4: console なので Act は Tui。focus は動かさない（上の決めごと）。
        let key = session_registry::create(
            &addr.project,
            &lane_label,
            &lane_stand,
            &stand,
            SessionAct::Tui,
            false,
        )
        .map_err(|e| anyhow::anyhow!("session 作成に失敗（addr={addr}）: {e}"))?;

        match self.open_slot_for_session(addr, key) {
            Ok(pid) => {
                tracing::info!(
                    "console slot open: addr={addr} session={key} stand={stand} pid={pid}"
                );
                Ok((key, pid))
            }
            Err(e) => {
                // 採番した session は **誰も指していない**（root でも focused でもない）ので、
                // slot が立たなければ存在価値がない。registry を元に戻してから失敗を返す
                // = 「失敗したら何も遷移していない」（`prepare_new_root_session` は root を
                // 動かすので registry 先行が正だが、こちらは逆 — 主体の違いで判断が分かれる）。
                if let Err(cleanup) =
                    session_registry::remove(&addr.project, &lane_label, &lane_stand, key)
                {
                    tracing::warn!(
                        "console slot open 失敗後の session 巻き戻しに失敗（Draft が残る）: addr={addr} session={key}: {cleanup}"
                    );
                }
                Err(e)
            }
        }
    }

    /// 1 つの session に console slot を立てる核（[`Self::open_new_slot`] の spawn 部）。
    ///
    /// **法の番人（slot 側）**: 「1 session = 高々 1 エンジン」の逆向き check をここに置く
    /// （chat engine 側は [`Self::ensure_chat_engine`]）。将来「畳んだ Pane の console を
    /// 開き直す」導線が来ても、slot を立てる経路はここ 1 本に通すこと。
    ///
    /// 拒否するもの:
    /// 1. registry に居ない session — 実在しない住人の端末を作らない
    /// 2. Act=Chat の session — その session は headless engine の器（両方は持てない）
    /// 3. 同 session に **chat engine** が居る — 1 会話 2 エンジン（法の直接違反）
    /// 4. 同 session に **既に slot** が居る — `insert_pty_slot` は黙って replace = 走行中の
    ///    console を無言で殺すので、入口で断る（張り替えたいなら restart 経路）
    fn open_slot_for_session(
        &mut self,
        addr: &LaneAddress,
        key: SessionKey,
    ) -> anyhow::Result<u32> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        let cwd = info.cwd.clone();
        let lane_stand = info.stand.clone();
        let lane_label = crate::process::stand_spawner::lane_label(addr).to_string();

        let reg = session_registry::load(&addr.project, &lane_label, &lane_stand);
        let entry = reg.sessions.iter().find(|s| s.key == key).ok_or_else(|| {
            anyhow::anyhow!("session が存在しません（addr={addr}, session={key}）")
        })?;
        if entry.act == SessionAct::Chat {
            anyhow::bail!(
                "Act II（chat）の session には console を立てられません（addr={addr}, session={key}）"
            );
        }
        if self
            .chat_engines
            .get(addr)
            .is_some_and(|m| m.contains_key(&key))
        {
            anyhow::bail!(
                "不変条件違反: 同一 session に PTY slot（Act I）と chat engine（Act II）は同居できません（addr={addr}, session={key}）"
            );
        }
        if self
            .pty_slots
            .get(addr)
            .is_some_and(|m| m.contains_key(&key))
        {
            anyhow::bail!(
                "その session には既に console があります（addr={addr}, session={key}。張り替えは restart）"
            );
        }

        // engine / resume id / VP_SESSION_KEY は **この session** の entry から決まる
        // （root 決め打ちにしない = doc 46 §3 の producer blocker の解）。
        let cmd = crate::process::stand_spawner::build_stand_command_for_session(
            &lane_stand,
            addr,
            std::path::Path::new(&cwd),
            false,
            Some(key),
        );
        let (slot, term_rx) = crate::process::stand_spawner::spawn_stand(&cmd, 120, 48)?;
        let pid = slot.pid();
        self.insert_pty_slot(addr.clone(), Some(key), slot, term_rx);
        Ok(pid)
    }

    /// session 指定を解決する（doc 38）: `None` = focused（省略時後方互換）、`Some(k)` は
    /// registry 上の実在を検証。戻り値は key + session の engine（stand）+ focused か。
    ///
    /// registry は disk が SSOT（毎回 file read）。submit 等の per-message 経路も通るが、
    /// 数百 byte の 1 file read で軽微 — in-memory cache で供給が 2 系統に割れるリスクの方が
    /// 大きい（doc 38 §5 原則。doc 40 PR-2 の in-memory authoritative 化は別 process reader
    /// との整合都合で見送り = disk read を SSOT のまま維持）。
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
            // doc 50 §4.6 A6: chat 経路の gate はこの値で行う（root cache では非 root を弾く）。
            act: entry.act,
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
                    // doc 50 §4.6 A6: GUI の roster / kind badge の供給源（registry が SSOT）。
                    act: s.act,
                    // 能力表は server が SSOT（client に engine 名の分岐を持たせない）。
                    chat_capable: EngineKind::from_stand(&s.stand)
                        .is_some_and(EngineKind::chat_capable),
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
        // doc 47 §4: chat session の Act は Chat（root は動かさないので slot に化身しない）。
        let key = session_registry::create(
            &addr.project,
            lane_label,
            &info.stand,
            stand,
            SessionAct::Chat,
            focus,
        )
        .map_err(|e| anyhow::anyhow!("session 作成に失敗（addr={addr}）: {e}"))?;
        tracing::info!(
            "chat session create: addr={addr} session={key} stand={stand} focus={focus}"
        );
        Ok(key)
    }

    /// doc 39 §4: Act I の ✨ New の registry 部 — 新 session（現 root の stand を引き継ぐ）を
    /// 作り、root と focused を同時にそれへ向ける。slot の張り替え（respawn）は caller が
    /// [`restart_lane_orchestrated`](crate::process::routes::lanes::restart_lane_orchestrated) を
    /// [`RespawnMode::Bare`] で呼ぶ（spawn の orchestration = retry / pump 付替 / Diff push は
    /// restart 経路に一元化 — 第 2 の spawn 経路を作らない）。
    ///
    /// 新 root は**既定レンズ**で立つ（doc 54 §3.1: chat_capable な engine は Chat、shell 等は
    /// Tui。旧「必ず Act I」は 2026-07-25 に撤回）。Act II 内の「新しい会話」タブは従来どおり
    /// `create_chat_session`（新 Draft タブ）が担う — 「どこに出すか」の分岐は vp-app が行い、
    /// backend は各動詞の整合だけ守る。旧 lane 単位 act の gate は A6 で撤去済み（下記）。
    /// `stand_override` は doc 46 P2 要件 4（Engine を選んで新コンソールを作る）。
    /// `None` なら従来どおり**現 root の engine を引き継ぐ**（doc 39 §1）。
    pub fn prepare_new_root_session(
        &mut self,
        addr: &LaneAddress,
        stand_override: Option<&str>,
    ) -> anyhow::Result<SessionKey> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        // doc 50 §4.6 A6: `switch_root` と同じ理由で lane 単位 act の gate を撤去した
        // （root の付け替えは act と直交する。act が session の属性になった今「chat lane」は無い）。
        // 新 root は bare engine の slot で立つので、旧 root が chat だった場合も成立する。
        let lane_label = crate::process::stand_spawner::lane_label(addr);
        // 新 session の engine は現 root の stand を引き継ぐ（doc 39 §1「engine は現 session を
        // 引き継ぎ」— lane の stand でなく root の stand。N=1 では両者は一致する）。
        let reg = session_registry::load(&addr.project, lane_label, &info.stand);
        // doc 46 P2: 明示指定があればそれを使う。無い時だけ現 root を引き継ぐ。
        let stand = match stand_override.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => reg
                .sessions
                .iter()
                .find(|s| s.key == reg.root)
                .map(|s| s.stand.clone())
                .unwrap_or_else(|| info.stand.clone()),
        };
        // doc 54 §3.1（2026-07-25）: 新 root は**既定レンズ**で立つ — chat_capable な engine は
        // Chat（VP 自前の ChatView が既定の面）、shell 等は Tui（定義）。旧「新 root は必ず
        // Act I（Tui）」は安定性の都合による暫定だった。
        let act = session_registry::default_act_for_stand(&stand);
        let key =
            session_registry::create_root(&addr.project, lane_label, &info.stand, &stand, act)
                .map_err(|e| anyhow::anyhow!("root session 作成に失敗（addr={addr}）: {e}"))?;
        // doc 53 R1: 旧「投影の同期」はここに在った。root の act の読み手が registry 直読に
        // なったので、同期すべき cache が存在しない（乖離の 15 例目が構造的に消えた）。
        tracing::info!(
            "new root session: addr={addr} session={key} stand={stand}（旧 root はタブに残存）"
        );
        Ok(key)
    }

    /// doc 39 P3: Root 切替 picker の registry 部 — root（と focused）を既存 session へ
    /// 向け替える。slot の張り替え（対象 session の store で resume）は caller が
    /// [`restart_lane_orchestrated`](crate::process::routes::lanes::restart_lane_orchestrated) を
    /// [`RespawnMode::Resume`] で呼ぶ（`prepare_new_root_session` と同じ「第 2 の spawn 経路を
    /// 作らない」規律）。lane 単位 act の gate が無いのも同様（A6 で撤去、下記）。
    pub fn prepare_switch_root_session(
        &mut self,
        addr: &LaneAddress,
        key: SessionKey,
    ) -> anyhow::Result<()> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        // doc 50 §4.6 A6: 旧実装は「root の act が tui でなければ拒否」だった
        // （メッセージ: 「chat lane の切替は echoes_session_focus」）。**A6 で撤去**:
        //
        // - あの gate が実際に見ていたのは「lane 全体が Act I か」で、旧・lane 単位 mode 時代の
        //   区別。act が session の属性になった今、「chat lane」という概念自体が無い
        // - root は「**誰が lane の代表か**」（slot / mailbox `agent@<lane>` の主、doc 39）で、
        //   act（見え方）とは**直交**する。root=chat のまま非 root の term を代表にしたい、は
        //   正当な要求（A6 が root=chat + 非 root=tui を正規の構成にしたので普通に起きる）
        // - 当初 tui 限定にしたのは「最初は tui しか安定していなかったから」（mako 2026-07-25）
        //
        // 残る制限は engine の有無だけ（下の EngineKind check）— root は mailbox の主なので、
        // engine を持たない shell が `agent@<lane>` を名乗るのは意味を持たない。
        let lane_label = crate::process::stand_spawner::lane_label(addr);
        // doc 39 P4-B: slot の respawn（restart_lane → build_stand_command）は root session の stand で
        // engine を決めるようになった（P4-A）ため、cross-engine の Root 切替は安全になった（選んだ
        // 会話の engine がそのまま slot に立つ）。P3 の同 engine ガードは解き、**未知 / 撤去済み stand**
        // （legacy cursor 等 = `EngineKind::from_stand` が None）のみ拒否する — それらは shell 層に
        // 落ちて engine が立たず resume も効かない = 選んだ会話に戻れない誤配送になるため。
        let reg = session_registry::load(&addr.project, lane_label, &info.stand);
        let entry = reg.sessions.iter().find(|s| s.key == key).ok_or_else(|| {
            anyhow::anyhow!("session が存在しません（addr={addr}, session={key}）")
        })?;
        if crate::echoes::EngineKind::from_stand(&entry.stand).is_none() {
            anyhow::bail!(
                "engine が未知の session への root 切替は未対応です（addr={addr}, session={key}: stand={} は shell 層のみで engine を持たない）",
                entry.stand
            );
        }
        session_registry::set_root(&addr.project, lane_label, &info.stand, key)
            .map_err(|e| anyhow::anyhow!("root 切替に失敗（addr={addr}, session={key}）: {e}"))?;
        // doc 53 R1: 旧「投影の同期」は不要になった（root の act は registry 直読 — 付け替えの
        // 直後でも restart_lane は常に新 root の act を見る）。
        tracing::info!("switch root session: addr={addr} session={key}（旧 root はタブに残存）");
        Ok(())
    }

    /// focused session を切り替える（registry 永続のみ。slot への注入・eager spawn は Phase 3）。
    pub fn focus_chat_session(
        &mut self,
        addr: &LaneAddress,
        key: SessionKey,
    ) -> anyhow::Result<()> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        // doc 53 R1: 問いは「LaneInfo.pid の意味」（chat engine の pid か PTY の pid か）= root の act。
        let is_chat = self.root_act(addr) == SessionAct::Chat;
        let lane_label = crate::process::stand_spawner::lane_label(addr);
        session_registry::focus(&addr.project, lane_label, &info.stand, key)
            .map_err(|e| anyhow::anyhow!("session focus に失敗（addr={addr}）: {e}"))?;
        // LaneInfo.pid は「focused session の代表値」— chat mode では切替に追随させる
        // （新 focused の engine が未 spawn なら None = chat-idle の正常形）。
        // Tui mode の pid は slot（PTY）のものなので触らない。
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

    /// session を取り除く（doc 38 Phase 3 — pane を閉じる）。戻り値 = 新 focused key。
    ///
    /// - registry から除去（最後の 1 本は registry 側が拒否 — lane を素に戻すのは fresh restart）
    /// - 当該 session の **engine slot（chat）と PtySlot（term）の両方**を drop
    ///   （走行中 turn は落ちる = 会話をやめる意思表示）。doc 50 §4.6 A6 で term pane にも ✕ が
    ///   出たので、chat 側だけ畳むと **registry からは消えたのに PTY が生き残る**（孤児 slot、
    ///   誰も読まない console が CPU と会話 id を抱えたまま残る）。名前は chat 由来だが
    ///   実体は「session を閉じる」— 1 往復路の終わりなので、その化身は種類を問わず畳む
    /// - 会話 id は registry の SSOT（doc 40）— session を registry から取り除いた時点で消えるので
    ///   個別の破棄は不要（doc 40 PR-2 で旧 cc/codex_sessions store は退役済み）
    /// - replay log（transcript を持たない engine の replay 源）は session 単位に破棄する（残すと
    ///   「閉じた session の会話が slot で蘇る」嘘になる）。破棄失敗は warn（remove 自体は成立）
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
        // doc 53 R1: 問いは focus_chat_session と同じ「LaneInfo.pid の意味」= root の act。
        let is_chat = self.root_act(addr) == SessionAct::Chat;
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
        // doc 50 §4.6 A6: term 側の化身（PtySlot + 双子の TermAttach）も畳む。閉じた session の
        // PTY を残すと、registry から消えているのに login shell / engine が生き続ける（孤児）。
        // `drop_slot` は不在なら no-op なので chat session でも安全に通せる。
        self.drop_slot(addr, key);
        // 会話 id は registry から entry を取り除いた時点で消える（doc 40 SSOT）。transcript を
        // 持たない engine の replay 源だけ session 単位に破棄する（残すと閉じた会話が slot で蘇る）。
        let label = session_registry::session_label(&lane_label, key);
        if let Err(e) = crate::echoes::replay_log::clear(&addr.project, &label) {
            tracing::warn!(
                "session remove: replay_log の破棄に失敗（addr={addr}, session={key}）: {e}"
            );
        }
        // term 側の replay（PTY 画面）も同じく session 単位で捨てる。A6 で非 root も replay を
        // disk に持つようになったため、閉じても消さないと**孤児 file が溜まり続ける**
        // （key 再利用は Reset だけなので ghost replay には直結しないが、純粋な leak。
        // team-b 10 回目 2026-07-25）。chat 側と対称に並べる。
        crate::daemon::pty_slot::clear_replay_session(&addr.project, &lane_label, key);
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

    /// 指定 session の Act（見え方）を切り替える（doc 50 §4.6 A6 — 旧 `set_console_mode` を
    /// root 専用から session 一般へ広げたもの。session = Pane の kind badge が任意 pane を
    /// 切り替える経路）。切替 = resume handoff（1 往復路 = Active 化身 高々 1 の遷移）。
    ///
    /// - → Chat: その session の PtySlot + TermAttach を drop（TUI 停止）→ act 永続 → engine-less
    ///   （`EchoesAgentHost` は demand_start / 初回 submit で lazy spawn。replay は handler が撃つ）
    /// - → Tui: その session の chat engine を drop（headless 停止）→ act 永続 → PtySlot respawn
    ///   （root は `restart_lane` 再利用 = `--resume` で文脈継承 / 非 root は `open_slot_for_session`）
    ///
    /// root を指定した場合も特別な追随処理は無い（doc 53 R1 — boot spawn / nudge 配送の
    /// 読み手は registry を直読するので、下の act 永続だけで全読み手に届く。旧 root cache
    /// `info.console_mode` は退役）。`info.pid` / `info.state` の代表値だけ追随させる。
    /// 同一 act への切替は no-op。Chat が許されるのは **その session の stand** が chat_capable な
    /// engine のときのみ（root 決め打ちにしない）。
    pub fn set_session_act(
        &mut self,
        addr: &LaneAddress,
        session: SessionKey,
        act: SessionAct,
    ) -> anyhow::Result<()> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        let lane_label = crate::process::stand_spawner::lane_label(addr).to_string();
        let default_stand = info.stand.clone();
        let root_key = Self::slot_session(addr, None);
        let is_root = session == root_key;

        // その session の現 act / stand を registry（disk SSOT）から引く。
        let reg = session_registry::load(&addr.project, &lane_label, &default_stand);
        let entry = reg
            .sessions
            .iter()
            .find(|s| s.key == session)
            .ok_or_else(|| {
                anyhow::anyhow!("session が存在しません（addr={addr}, session={session}）")
            })?;
        if entry.act == act {
            return Ok(());
        }
        let session_stand = entry.stand.clone();

        // Chat（Act II）は headless host を持つ engine のみ（能力表明は EngineKind に一元化）。
        // その session の stand で判定する（root 決め打ちにしない = 非 root は engine が違い得る）。
        if act == SessionAct::Chat
            && !EngineKind::from_stand(&session_stand).is_some_and(EngineKind::chat_capable)
        {
            anyhow::bail!(
                "Act II（chat）は Act II host を持つ engine の session のみ（addr={addr}, session={session}, stand={session_stand}）"
            );
        }

        // act 永続（→tui では open_slot_for_session が act=Tui を前提に読むため、遷移前に永続する）。
        if let Err(e) = session_registry::set_session_act(
            &addr.project,
            &lane_label,
            &default_stand,
            session,
            act,
        ) {
            tracing::warn!("session act の永続失敗（addr={addr}, session={session}）: {e}");
        }

        match act {
            SessionAct::Chat => {
                // その session の TUI engine 停止（PtySlot Drop = child kill + wait）。
                self.drop_slot(addr, session);
                if is_root && let Some(info) = self.lanes.get_mut(addr) {
                    // doc 53 R1: root の act は registry 直読になった（上で永続済み）ので
                    // cache 更新は無い。pid / state は「lane の代表値」なので追随させる。
                    info.pid = None;
                    info.state = LaneState::Running; // chat-idle は正常形（doc 33 §3）
                }
                tracing::info!(
                    "session act → chat（TUI 停止、engine は demand/submit で lazy）: addr={addr} session={session}"
                );
                Ok(())
            }
            SessionAct::Tui => {
                // その session の chat engine 停止（Drop = kill_on_drop + pump abort）。
                if let Some(slots) = self.chat_engines.get_mut(addr) {
                    slots.remove(&session);
                    if slots.is_empty() {
                        self.chat_engines.remove(addr);
                    }
                }
                if is_root {
                    // doc 53 R1: 旧 root cache 更新は不要（restart_lane が registry を直読する。
                    // act は上で永続済みなので、ここから見える registry は常に新しい）。
                    // root の PTY respawn は restart_lane を再利用（--resume は
                    // build_stand_command が cc_session から拾う = 会話継続）。
                    tracing::info!("session act → tui（root、headless 停止、PTY respawn）: {addr}");
                    self.restart_lane(addr, RespawnMode::Resume)
                } else {
                    // 非 root は open_slot_for_session で PtySlot を起立（act=Tui 永続済が前提）。
                    tracing::info!(
                        "session act → tui（非 root、headless 停止、slot 起立）: addr={addr} session={session}"
                    );
                    self.open_slot_for_session(addr, session).map(|_pid| ())
                }
            }
        }
    }

    /// chat engine を確保する（無ければ spawn + pump 起動）。`session=None` は focused。
    ///
    /// **法の番人**（doc 38 で session 粒度 → doc 46 P5 で slot 側も session 粒度）:
    /// - **同一 session に PTY slot と chat engine を同居させない**（focused かどうかに依らず
    ///   全 session に適用）。P5 で `pty_slots` が session ごとになったので、旧「lane に
    ///   PtySlot が残存」という lane 全体の近似ではなく **`pty_slots[addr][key]` の有無**を
    ///   直接検査できる。これが「1 session = 高々 1 エンジン」の実体
    /// - **act=chat 以外の session では拒否**（= 生きた Act I console を暗黙に殺さないための
    ///   入口ガード）。判定は **その session の act**（doc 50 §4.6 A6）。
    ///
    /// > ⚠️ 旧実装は `resolved.focused && info.console_mode != Chat`（= lane 単位 root cache を
    /// > focused の時だけ見る）だった。A6 で「root=tui のまま非 root だけ chat」が正規の構成に
    /// > なったので、**その非 root を focus すると chat engine の起動が拒否される**（root が tui
    /// > だから）。逆に非 focused は素通りしていた。session ごとに act を持つ今は、focused の
    /// > 特例（doc 38 落とし穴③ = lane 単位ガードが副 session を縛る問題への対処）も不要 —
    /// > 自分の act で判定すれば「Tui 中は副 session が動けない」は構造的に起きない。
    pub fn ensure_chat_engine(
        &mut self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
        topic_router: &std::sync::Arc<crate::process::topic_router::TopicRouter>,
    ) -> anyhow::Result<()> {
        let resolved = self.resolve_chat_session(addr, session)?;
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        if resolved.act != SessionAct::Chat {
            // 呼び元は echoes_submit / echoes_nudge の両方（doc 34 channel E）— method 名は
            // 呼び元の ctx が名乗るので、ここでは要件だけ述べる。
            anyhow::bail!(
                "chat engine には act=chat が必要（addr={}, session={}、現在 {:?}。session_set_act で切替）",
                addr,
                resolved.key,
                resolved.act
            );
        }
        // doc 46 P5: 法（1 session = 高々 1 エンジン）を **当該 session の slot 有無**で直接見る。
        // 旧実装は lane 全体（`pty_slots.contains_key(addr)`）を focused の時だけ見ていた —
        // slot が lane に 1 枚だった時代の近似で、非 root slot が立ち得る今は
        // 「別 session の slot が居るから focused の chat を拒否」/「自分の slot が居るのに
        // 非 focused だから通す」の両方向に嘘をつく。
        if self
            .pty_slots
            .get(addr)
            .is_some_and(|m| m.contains_key(&resolved.key))
        {
            anyhow::bail!(
                "不変条件違反: 同一 session に PTY slot（Act I）と chat engine（Act II）は同居できません（addr={}, session={}）",
                addr,
                resolved.key
            );
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
                        lane_label: lane_label.clone(),
                        session_key: resolved.key,
                        thread_id: resolved.conversation.clone(),
                    },
                )?)
            }
            Some(EngineKind::Grok) => {
                // grok: 常駐 AcpAgentHost（`grok agent stdio` = ACP、doc 42）。sessionId は
                // registry の会話 id（registry-native — 旧 store なし）。
                ChatHost::Grok(crate::echoes::AcpAgentHost::spawn(
                    crate::echoes::AcpHostConfig {
                        engine: crate::echoes::AcpEngine::Grok,
                        cwd: info.cwd.clone(),
                        project: addr.project.clone(),
                        lane: label.clone(),
                        lane_label: lane_label.clone(),
                        session_key: resolved.key,
                        session_id: resolved.conversation.clone(),
                    },
                )?)
            }
            Some(EngineKind::OpenCode) => {
                // opencode: grok と同じ常駐 AcpAgentHost（`opencode acp` = 同 ACP、doc 43）。
                // engine パラメタだけが違う。sessionId は registry の会話 id（registry-native）。
                ChatHost::OpenCode(crate::echoes::AcpAgentHost::spawn(
                    crate::echoes::AcpHostConfig {
                        engine: crate::echoes::AcpEngine::OpenCode,
                        cwd: info.cwd.clone(),
                        project: addr.project.clone(),
                        lane: label.clone(),
                        lane_label: lane_label.clone(),
                        session_key: resolved.key,
                        session_id: resolved.conversation.clone(),
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
                        lane_label: lane_label.clone(),
                        session_key: resolved.key,
                        resume_session_id: resume,
                        model,
                        claude_cli_path: None,
                    },
                )?)
            }
            None => {
                // engine を持たない stand（shell / 撤去済み cursor・agy / 未知）。focused は
                // act=Chat ガード（set_session_act の chat_capable check）が上流で塞ぐので
                // 通常到達しない（belt-and-suspenders）。非 focused session はここが唯一の防壁。
                anyhow::bail!(
                    "stand '{}' は Act II chat host を持ちません（addr={}, session={}）",
                    resolved.stand,
                    addr,
                    resolved.key
                );
            }
        };
        // replay-log tap: transcript を持たない engine（codex / grok / opencode）の session にだけ
        // 付ける。claude は transcript が SSOT なので None（二重化しない）。tap は配信 event を
        // per-session に disk 記録し、demand_start の no_session path がそれを replay 源にする
        // （doc — engine 非依存 replay log）。⚠️ この判定は unison_server の reader / writer と
        // replay_log.rs の doc と 4 点セット（片側更新は dead-write を生む、#807 教訓 / doc 43 §5）。
        let replay_tap = match EngineKind::from_stand(&resolved.stand) {
            Some(EngineKind::Codex | EngineKind::Grok | EngineKind::OpenCode) => {
                Some(crate::echoes::replay_log::ReplayLogTap {
                    project: addr.project.clone(),
                    label: label.clone(),
                })
            }
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

    /// 既存 slot の PtySlot を resize する。`session=None` は root。
    /// Stage 1 (ADR-0001): TermAttach も並走 resize (= alacritty Term<T> grid を同期)。
    pub fn resize_lane(
        &self,
        addr: &LaneAddress,
        session: Option<SessionKey>,
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<()> {
        let key = Self::slot_session(addr, session);
        let slot_mutex = self
            .pty_slots
            .get(addr)
            .and_then(|m| m.get(&key))
            .ok_or_else(|| anyhow::anyhow!("Lane has no PtySlot: {} (session={})", addr, key))?;
        let slot = slot_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("PtySlot mutex poisoned: {} (session={})", addr, key))?;
        slot.resize(cols, rows)?;
        // attach 不在 (= spawn 失敗 / 未配線 Performer 経路) は静かに skip
        if let Some(term_attach) = self.term_attaches.get(addr).and_then(|m| m.get(&key)) {
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
///
/// ## 宛先 slot（doc 46 P5）
/// `session=None` は root — wire mailbox `agent@<lane>` を名乗るのは root session だから
/// （doc 39）。明示指定は「lane の中の特定の住人に話しかける」経路（`vp lane nudge --session`）。
pub async fn deliver_nudge(
    pool: &std::sync::Arc<tokio::sync::RwLock<LanePool>>,
    addr: &LaneAddress,
    session: Option<SessionKey>,
    text: &str,
) -> anyhow::Result<()> {
    // 同一 lane への並行 nudge を直列化する per-lane lock を get-or-insert（内部可変性なので
    // read guard で足りる。handle 取得後は pool read lock を手放す）。
    let nudge_lock = pool.read().await.nudge_lock_handle(addr)?;
    // この guard を phase1→sleep→phase2 の間ずっと保持し、同一 lane の他 nudge を待たせる。
    let _serialized = nudge_lock.lock().await;

    // phase 1: text 本体（末尾 CR/LF は落として単一行の paste にする）
    let body = text.trim_end_matches(['\r', '\n']);
    pool.read()
        .await
        .write_to_lane(addr, session, body.as_bytes())?;
    // paste 判定を跨ぐ猶予（best-effort nudge なので体感遅延にならない範囲）
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // phase 2: Enter(CR) 単独 → 独立 keystroke として submit
    pool.read().await.write_to_lane(addr, session, b"\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// lane を PTY / engine 無しで pool に置く（restart_lane の chat 分岐は早期 return する
    /// ので spawn 不要）。mode は registry（SSOT）に書く — doc 53 R1 で pool cache は退役し、
    /// 読み手（root_act 直読）と同じ経路をテストも通る。⚠️ 呼び手は `test_env::state_dir`
    /// guard 必須（registry は vp_state_dir() を読み書きする）。
    fn insert_lane(pool: &mut LanePool, addr: &LaneAddress, mode: SessionAct) {
        let lane_label = crate::process::stand_spawner::lane_label(addr);
        crate::lane::session_registry::set_root_act(&addr.project, lane_label, "echoes", mode)
            .expect("test registry へ root act を書けること");
        pool.insert(LaneInfo {
            id: Default::default(),
            address: addr.clone(),
            state: LaneState::Running,
            stand: "echoes".to_string(),
            created_at: "2026-07-10T00:00:00Z".to_string(),
            pid: None,
            cwd: "/tmp".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
            flow_state: None,
        });
    }

    /// chat mode の lane を pool に置く（従来 helper の互換形）。
    fn insert_chat_lane(pool: &mut LanePool, addr: &LaneAddress) {
        insert_lane(pool, addr, SessionAct::Chat);
    }

    /// doc 54 §8-11: conductor の初回作成（registry 不在）は既定レンズ（Chat）で立ち、
    /// PTY を立てない（engine-less / Running = chat-idle の正常形）。2 回目以降の boot は
    /// 既存 registry を honor する — 生成の既定であって毎 boot の強制ではない（壊し方②:
    /// これが「不在なら書く」の gate 無しだと、user の act 切替が boot のたびに Chat へ戻る）。
    #[tokio::test]
    async fn with_root_first_boot_defaults_to_chat_and_does_not_rewrite_existing() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vptest-chatdefault");

        // 初回: registry 不在 → 既定レンズ Chat（PTY spawn なし = テストでも安全に通る）。
        let pool = LanePool::with_root("vptest-chatdefault", "/tmp");
        assert_eq!(
            pool.root_act(&addr),
            SessionAct::Chat,
            "初回作成は既定レンズ Chat（われわれの ChatView）"
        );
        let info = pool.get(&addr).expect("conductor 登録");
        assert_eq!(info.pid, None, "chat boot は engine-less（PTY を立てない）");
        assert_eq!(info.state, LaneState::Running, "chat-idle は正常形");
        drop(pool);

        // 2 回目の boot: 既存 registry（session #2 を足して user の痕跡を付ける）を
        // with_root が書き換えないこと — 「不在なら書く」の gate の証明。
        session_registry::create(
            "vptest-chatdefault",
            "root",
            "echoes",
            "echoes",
            SessionAct::Tui,
            false,
        )
        .expect("user が session #2 を追加");
        let pool = LanePool::with_root("vptest-chatdefault", "/tmp");
        assert_eq!(
            session_registry::load("vptest-chatdefault", "root", "echoes")
                .sessions
                .len(),
            2,
            "既存 registry は初回 gate の外 = 上書き・再初期化されない"
        );
        assert_eq!(pool.root_act(&addr), SessionAct::Chat, "root の act も無傷");
    }

    /// doc 33 → doc 39 P2: chat lane の restart は `RespawnMode` で意味が割れる。
    /// - Resume → 会話 id を残す（次 spawn が `--resume` で会話を継ぐ）
    /// - Bare   → 会話 id を残す（素の engine で張り替えるが registry は無傷 — 新 root 用）
    /// - Reset  → 会話 id を捨てる（素の新規 session + replay も前会話を映さない）
    ///
    /// engine は lazy spawn なので「立て直す対象」がその場に無く、意図は state
    /// (registry の会話 id の有無) でしか運べない。 その 1 点をここで固定する。
    #[test]
    fn chat_restart_clears_conversation_only_when_fresh() {
        // session registry は vp_state_dir() = $XDG_STATE_HOME/vp を読む。 crate 唯一のロック下で
        // tempdir に向け、 guard の drop で復元する。
        let _state = crate::test_env::state_dir();

        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);

        // root(#1) の会話 id を記録（doc 40: SSOT は registry）。
        let root_conv = || {
            crate::lane::session_registry::load("vp", "root", "echoes")
                .sessions
                .iter()
                .find(|s| s.key == 1)
                .and_then(|s| s.conversation.clone())
        };
        crate::lane::session_registry::set_conversation(
            "vp",
            "root",
            "echoes",
            1,
            Some("old-session-id"),
        )
        .expect("record conversation");

        // Resume: 会話を継ぐので記録は残る
        pool.restart_lane(&addr, RespawnMode::Resume)
            .expect("chat restart");
        assert_eq!(
            root_conv().as_deref(),
            Some("old-session-id"),
            "Resume restart は resume の矢印を保つ"
        );

        // Bare（doc 39 P2）: 素の engine で張り替えるが registry は破棄しない
        pool.restart_lane(&addr, RespawnMode::Bare)
            .expect("bare chat restart");
        assert_eq!(
            root_conv().as_deref(),
            Some("old-session-id"),
            "Bare restart は会話 id を無傷に保つ（新 root 用 — 旧会話をタブに残す）"
        );

        // Reset: 素の新規 session にするため記録を捨てる（registry ごと N=1 へ）
        pool.restart_lane(&addr, RespawnMode::Reset)
            .expect("reset chat restart");
        assert_eq!(root_conv(), None, "Reset restart は resume の矢印を捨てる");

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
        let addr = LaneAddress::root("vp");
        crate::lane::session_registry::set_conversation("vp", "root", "echoes", 1, Some("old-id"))
            .expect("record conversation");
        LanePool::clear_fresh_lane_state(&addr, "echoes").expect("clear");
        assert_eq!(
            crate::lane::session_registry::load("vp", "root", "echoes").sessions[0].conversation,
            None,
            "mode に依らず fresh 破棄で会話 id（registry）が消える"
        );
    }

    /// doc 38 落とし穴②: fresh restart は registry 上の**全 session**の会話 id を消し、
    /// registry も既定形（N=1）へ戻す。focused（や旧来の #1）だけ消すと副 session が
    /// resume され「New Session なのに前の会話が出る」嘘になる — その再演をここで塞ぐ。
    #[test]
    fn chat_fresh_restart_clears_all_sessions_and_registry() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);

        // session #2（codex）を追加し、#1 / #2 の両方に会話 id を registry に記録する。
        let k2 = pool
            .create_chat_session(&addr, Some("codex"), false)
            .expect("create session");
        assert_eq!(k2, 2);
        crate::lane::session_registry::set_conversation("vp", "root", "echoes", 1, Some("cc-id-1"))
            .expect("record #1");
        crate::lane::session_registry::set_conversation(
            "vp",
            "root",
            "echoes",
            2,
            Some("0199-codex-id"),
        )
        .expect("record #2");
        // 副 session（codex）の replay 源にも会話を仕込む — fresh はこれも捨てるべき。
        crate::echoes::replay_log::append(
            "vp",
            "root#2",
            &crate::echoes::EchoesEvent::MessageChunk {
                text: "old codex reply".to_string(),
            },
        )
        .expect("replay log append #2");

        pool.restart_lane(&addr, RespawnMode::Reset)
            .expect("reset chat restart");

        assert!(
            crate::echoes::replay_log::load("vp", "root#2").is_empty(),
            "副 session (#2) の replay 源も消える（残すと New Session なのに前会話が replay される）"
        );
        // registry ごと既定形（N=1）へ戻る = 全 session の会話 id が道連れに消える（doc 40 SSOT）。
        let reg = crate::lane::session_registry::load("vp", "root", "echoes");
        assert_eq!(reg.sessions.len(), 1, "registry は既定形（N=1）へ戻る");
        assert_eq!(reg.focused, 1);
        assert_eq!(reg.sessions[0].conversation, None, "#1 の会話 id も消える");
    }

    /// Reset は **term の PTY replay file** も消すこと（ghost replay の封じ、team-b 5 回目）。
    ///
    /// registry を消すと採番が N=1 に戻るので、次に作る session は**同じ key を再利用**する。
    /// replay file が残っていると同じ path を seed に読み、「Reset したはずの画面」が新しい
    /// console に蘇る（`clear_replay_in` の doc が lane 再作成について警告していた機序。Reset は
    /// 経路が別で漏れていた）。root は以前から replay を持つので pre-existing、A6 で非 root も
    /// 持つようになり範囲が広がった。
    ///
    /// **順序も固定する**: 掃除が破壊より前だと `PtySlot::drop` の最終 flush が書き戻して無効化
    /// される。だから slot は `replay_path` **付き**で立て、実際に出力させてから Reset する
    /// （`replay_path` なしの test slot だと Drop が何も書かず、順序を間違えても通ってしまう）。
    #[cfg(unix)]
    #[tokio::test]
    async fn reset_wipes_term_replay_and_only_after_slots_are_dropped() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);
        if let Some(info) = pool.lanes.get_mut(&addr) {
            info.stand = "shell".to_string(); // engine を注入しない = restart の spawn が軽い
        }
        // 非 root の term session（A6 で replay を持つようになった側）を registry に足す。
        session_registry::create("vp", "root", "shell", "shell", SessionAct::Tui, false)
            .expect("非 root term session");

        let file_of = |session: SessionKey| {
            crate::daemon::pty_slot::replay_file_path_session(
                &addr.project,
                crate::process::stand_spawner::lane_label(&addr),
                session,
            )
        };
        let (root_file, mate_file) = (file_of(1), file_of(2));

        // 各 slot に**固有の目印を出力させる**。Drop の最終 flush でこれが disk に書かれるので、
        // 掃除の順序を間違えると目印が生き残る = 罠が検出される。
        let (root_slot, _root_rx) =
            spawn_test_slot_with_replay("printf PRE_RESET_ROOT; cat", &root_file).await;
        let (mate_slot, _mate_rx) =
            spawn_test_slot_with_replay("printf PRE_RESET_MATE; cat", &mate_file).await;
        pool.insert_pty_slot(addr.clone(), Some(1), root_slot, _root_rx.resubscribe());
        pool.insert_pty_slot(addr.clone(), Some(2), mate_slot, _mate_rx.resubscribe());

        pool.restart_lane(&addr, RespawnMode::Reset)
            .expect("reset restart");

        // 見るのは「file が消えたか」ではなく「**旧画面が残っていないか**」。Reset 後に立て直した
        // root slot が自分の出力を同じ path に flush するのは正常なので、存在だけ見ると誤判定する
        // （[[verify-the-cleanup-not-just-the-disappearance]]）。
        let has = |path: &std::path::Path, needle: &str| {
            std::fs::read(path)
                .map(|b| String::from_utf8_lossy(&b).contains(needle))
                .unwrap_or(false)
        };
        assert!(
            !has(&root_file, "PRE_RESET_ROOT"),
            "root の旧画面が残っている（Reset 後 root は必ず key=1 = 同じ path を再利用する。\
             掃除が PtySlot::drop より前だと最終 flush で書き戻される）"
        );
        assert!(
            !has(&mate_file, "PRE_RESET_MATE"),
            "非 root の旧画面が残っている（Reset で採番が N=1 に戻り、次の session も key=2 になる）"
        );
    }

    /// **root を付け替えても replay file の身元が動かない**こと（team-b 6 回目 score 92）。
    ///
    /// 初版は root だけ lane 単位の旧名 file を使っていた = file の身元が **role**（誰が root か）
    /// に紐づいていた。A6 が「非 root も term になれる / 旧 root は付け替え後もタブに残る」を
    /// 正規にしたので、この命名は 2 つの壊れ方を生む:
    ///
    /// - **①内容の混入**: 新 root は spawn 時に `is_root=true` になり旧名 file を seed する →
    ///   *別 session の画面*が新 root の console に出る（Reset の ghost replay より実害が大きい —
    ///   「消したはずの自分の画面」ではなく「他人の会話」が出る）
    /// - **②同一 file の奪い合い**: 旧 root の slot は付け替えでは畳まれない（「同居人は独立の
    ///   住人」= 意図的）。その slot の `replay_path` は spawn 時に焼き込んだ旧名のままなので、
    ///   新旧 2 本の**生きた** slot が同じ file を 3s ごとに上書きし合う
    ///
    /// 身元を session に紐づけると両方が構造的に消える（旧 root は自分の file を持ち続け、
    /// 新 root は自分の file を読む）。ここでは path の不変条件として固定する — 実 spawn は
    /// engine の PTY を立てることになり CI に置けないため。
    #[test]
    fn switching_root_does_not_move_any_replay_file_identity() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);
        let lane_label = crate::process::stand_spawner::lane_label(&addr);
        let path_of = |session: SessionKey| {
            crate::daemon::pty_slot::replay_file_path_session(&addr.project, lane_label, session)
        };

        // engine 持ちの非 root session を足し、root=1 時点の両者の path を覚える。
        session_registry::create("vp", "root", "echoes", "echoes", SessionAct::Tui, false)
            .expect("非 root session");
        let (p1_before, p2_before) = (path_of(1), path_of(2));
        assert_ne!(p1_before, p2_before, "session ごとに別 file");

        // root を #2 へ付け替える。
        pool.prepare_switch_root_session(&addr, 2)
            .expect("root 付け替え");
        assert_eq!(session_registry::root("vp", "root"), 2, "root が動いた");

        // **どちらの file も動かない** = 内容の混入も奪い合いも起きない。
        assert_eq!(
            path_of(1),
            p1_before,
            "旧 root(#1) の file は付け替え後も同じ（生存 slot が書き続ける先が変わらない）"
        );
        assert_eq!(
            path_of(2),
            p2_before,
            "新 root(#2) は自分の file を読む（旧 root の画面を seed しない）"
        );
        assert_ne!(path_of(1), path_of(2), "付け替え後も衝突しない");
    }

    /// doc 38 落とし穴③: console_mode ガードは focused session にのみ適用される。
    /// - focused の ensure は Tui mode で「mode=chat が必要」で弾かれる（従来どおり）
    /// - 非 focused の ensure は mode ガードを**通過**し、engine 能力の防壁まで到達して弾かれる
    ///   = ガードが session 経路に流用されていない証跡
    ///
    /// sweep 6.5 以降、現行 engine（claude/codex/grok）は全て chat 対応なので、能力防壁
    /// （`None` arm = "Act II chat host を持ちません"）に到達させるには engine を持たない
    /// legacy/未知 stand が要る。ここでは撤去済み "cursor" の legacy session を registry へ
    /// 直接注入する（`create_chat_session` は未知 stand を入口で弾くため直書き。legacy stand の
    /// graceful degradation = shell のみ・chat 不可、の証跡も兼ねる）。
    #[tokio::test]
    async fn console_mode_guard_applies_only_to_focused_session() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);
        let router = std::sync::Arc::new(crate::process::topic_router::TopicRouter::new());

        // 非 focused の legacy stand（撤去済み "cursor"）session を registry に直接注入する
        // （focused は #1=echoes のまま）。
        let lane_label = crate::process::stand_spawner::lane_label(&addr);
        let k = crate::lane::session_registry::create(
            &addr.project,
            lane_label,
            "echoes",
            "cursor",
            SessionAct::Chat,
            false,
        )
        .expect("create legacy session");

        // focused（#1、省略時）は act ガードで弾かれる（doc 50 §4.6 A6 で文言が
        // `console mode=chat` → `act=chat` / 案内 verb が `session_set_act` に変わった）。
        let err = pool
            .ensure_chat_engine(&addr, None, &router)
            .expect_err("Tui act の focused ensure は Err");
        assert!(
            err.to_string().contains("act=chat が必要"),
            "focused は act ガード: {err}"
        );

        // 非 focused は mode ガードを通過し、engine 能力の防壁で弾かれる（legacy stand = host なし）。
        let err = pool
            .ensure_chat_engine(&addr, Some(k), &router)
            .expect_err("legacy stand session の ensure は Err");
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
        let addr = LaneAddress::root("vp");
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
    /// tab を閉じたのに slot（Act I）で会話が蘇る嘘（session #1 の bare label）をここで固定する。
    #[test]
    fn remove_chat_session_drops_slot_and_conversation_ids() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);

        // #2(codex) を focused で追加し、両 session に会話 id を registry に記録
        let k2 = pool
            .create_chat_session(&addr, Some("codex"), true)
            .expect("create #2");
        crate::lane::session_registry::set_conversation("vp", "root", "echoes", 1, Some("cc-id-1"))
            .expect("record #1");
        crate::lane::session_registry::set_conversation(
            "vp",
            "root",
            "echoes",
            2,
            Some("0199-codex-id"),
        )
        .expect("record #2");
        // #2（codex）の replay 源にも会話を仕込む — close で消えるべき。
        crate::echoes::replay_log::append(
            "vp",
            "root#2",
            &crate::echoes::EchoesEvent::MessageChunk {
                text: "codex reply".to_string(),
            },
        )
        .expect("replay log append #2");
        // term 側の replay（PTY 画面）も置いておく — A6 で非 root も持つようになった側。
        let term_replay =
            crate::daemon::pty_slot::replay_file_path_session(&addr.project, "root", 2);
        std::fs::create_dir_all(term_replay.parent().expect("parent")).expect("mkdir");
        std::fs::write(&term_replay, b"old screen").expect("write term replay");

        // focused(#2) を remove → focus は #1 へ、#2 の会話 id は registry entry ごと消える
        let focused = pool.remove_chat_session(&addr, k2).expect("remove #2");
        assert_eq!(focused, 1);
        let reg = crate::lane::session_registry::load("vp", "root", "echoes");
        assert!(
            reg.sessions.iter().all(|s| s.key != 2),
            "閉じた session (#2) は registry から消える = 会話 id も道連れ（doc 40 SSOT）"
        );
        assert!(
            !term_replay.exists(),
            "閉じた session の **term replay file** も破棄される（残すと孤児 file が溜まる。\
             team-b 10 回目 2026-07-25）: {term_replay:?}"
        );
        assert!(
            crate::echoes::replay_log::load("vp", "root#2").is_empty(),
            "閉じた session の replay 源も破棄される（slot で会話が蘇る嘘を防ぐ）"
        );
        assert_eq!(
            reg.sessions
                .iter()
                .find(|s| s.key == 1)
                .and_then(|s| s.conversation.as_deref()),
            Some("cc-id-1"),
            "残る session (#1) の会話 id は無傷"
        );

        // 最後の 1 本は取り除けない（fresh restart が正道）
        assert!(pool.remove_chat_session(&addr, 1).is_err());
    }

    /// doc 50 §4.6 A6: **非 root の term session は boot で復元される**（再起動を越える）。
    ///
    /// team-b review 2026-07-25（score 78）: A6 で「非 root が term」は registry に永続する一級の
    /// 状態になったが、boot で slot を立てるのは root だけだった。World / project 再起動のあと
    /// （dogfood の `VP_SWAP_RESTART_DAEMON=1` は毎回これ）**pane は出るのに中身が空で無反応**に
    /// なる — roster は registry から導出されるので pane は現れ、slot だけが居ない。
    ///
    /// 「registry に act=Tui の非 root が居る状態で lane を初めて触る」= 再起動後の主経路を再現する。
    #[tokio::test]
    async fn boot_restores_non_root_term_slots() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");

        // 再起動前の registry を模す: root(#1)=chat + 非 root=tui（A6 の正規構成）。
        let lane_label = crate::process::stand_spawner::lane_label(&addr);
        session_registry::set_root_act("vp", lane_label, "echoes", SessionAct::Chat)
            .expect("root を chat に");
        let term_key =
            session_registry::create("vp", lane_label, "echoes", "shell", SessionAct::Tui, false)
                .expect("非 root term session");

        // lane を初めて触る（= pool に entry が無い状態からの登録 + 復元）。
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);
        pool.restore_term_slots(&addr);

        assert!(
            pool.slot_sessions(&addr).contains(&term_key),
            "非 root の term session に slot が立つ（無いと pane が空で無反応になる）: got={:?}",
            pool.slot_sessions(&addr)
        );
        // root（chat）には slot を立てない — chat は engine-less が正常形（doc 33 §3）。
        assert!(
            !pool.slot_sessions(&addr).contains(&1),
            "root=chat には PTY を立てない（1 会話 2 エンジンを作らない）"
        );
    }

    /// doc 50 §4.6 A6: **term session を閉じたら PtySlot も畳む**（孤児 slot を作らない）。
    ///
    /// A6 で term pane にも名札の ✕ が出た。`remove_chat_session` は名前どおり chat_engines しか
    /// 畳んでいなかったので、term を閉じると **registry からは消えるのに PTY が生き残る**
    /// （誰も読まない console が会話 id ごと残る）。「1 往復路の終わり」なので化身は種類を問わず畳む。
    #[tokio::test]
    async fn remove_session_drops_pty_slot_too() {
        // PtySlot::spawn は reader task を立てるので tokio runtime が要る（async test）。
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);

        // 非 root の term session を 1 本足す（slot 付き）。
        let key = pool
            .create_chat_session(&addr, Some("echoes"), false)
            .expect("create session");
        let (slot, rx) = crate::daemon::pty_slot::PtySlot::spawn(
            &std::env::temp_dir().to_string_lossy(),
            "/bin/sh",
            &["-c".to_string(), "cat".to_string()],
            &[],
            80,
            24,
            None,
        )
        .expect("spawn slot");
        pool.insert_pty_slot(addr.clone(), Some(key), slot, rx);
        assert!(
            pool.slot_sessions(&addr).contains(&key),
            "前提: term session に slot が居る"
        );

        pool.remove_chat_session(&addr, key).expect("remove");

        assert!(
            !pool.slot_sessions(&addr).contains(&key),
            "session を閉じたら PtySlot も畳まれる（孤児 PTY を残さない）"
        );
    }

    /// list_chat_sessions は registry の session + 会話 id（registry SSOT）+ focused を突き合わせる。
    #[test]
    fn list_chat_sessions_joins_registry_and_conversations() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);

        pool.create_chat_session(&addr, Some("codex"), false)
            .expect("create");
        crate::lane::session_registry::set_conversation("vp", "root", "echoes", 1, Some("cc-id-1"))
            .expect("record");

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

    /// doc 39 P4-B: Root 切替に残る制限は **既知 engine 限定**だけ（cross-engine は respawn が root
    /// stand に追従する P4-A で安全になり解禁。未知 / 撤去済み stand は shell 層に落ちるため拒否の
    /// まま。"hd"/"echoes" の旧名差は同 engine 扱い）。doc 50 §4.6 A6 で「Tui 限定」は撤去 —
    /// root=chat のまま非 root を代表に立てられることを本テストが固定する。
    #[test]
    fn switch_root_validates_engine_and_moves_root() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);

        // 同 engine（旧名 "hd" = claude）の #2 → 切替 OK、root/focused が動く
        session_registry::create("vp", "root", "echoes", "hd", SessionAct::Chat, false)
            .expect("create #2");
        pool.prepare_switch_root_session(&addr, 2)
            .expect("同 engine（旧名差）への切替は通る");
        let reg = session_registry::load("vp", "root", "echoes");
        assert_eq!(reg.root, 2);
        assert_eq!(reg.focused, 2);

        // cross-engine（codex）の #3 → P4 で解禁（通る、root/focused が動く）
        session_registry::create("vp", "root", "echoes", "codex", SessionAct::Chat, false)
            .expect("create #3");
        pool.prepare_switch_root_session(&addr, 3)
            .expect("cross-engine（codex）への切替は P4 で通る");
        let reg = session_registry::load("vp", "root", "echoes");
        assert_eq!(reg.root, 3, "root は codex session #3 へ");
        assert_eq!(reg.focused, 3);

        // 未知 / 撤去済み stand（cursor）の #4 → Err（shell 層に落ちるため拒否のまま）
        session_registry::create("vp", "root", "echoes", "cursor", SessionAct::Chat, false)
            .expect("create #4");
        let err = pool
            .prepare_switch_root_session(&addr, 4)
            .expect_err("未知 engine は拒否");
        assert!(err.to_string().contains("engine が未知"), "err={err}");

        // 不在 key → Err
        assert!(pool.prepare_switch_root_session(&addr, 99).is_err());

        // doc 50 §4.6 A6: **root が chat でも付け替えできる**（旧 Tui gate は撤去）。
        // root は「誰が lane の代表か」で act（見え方）とは直交する — root=chat のまま
        // 別 session を代表にしたいのは正当な要求（当初 tui 限定にしたのは「最初は tui しか
        // 安定していなかったから」= mako 2026-07-25）。残る制限は engine の有無だけ。
        let chat = LaneAddress::performer("vp", "chatty");
        insert_chat_lane(&mut pool, &chat);
        session_registry::create("vp", "chatty", "echoes", "echoes", SessionAct::Chat, false)
            .expect("chatty に #2 を作る");
        pool.prepare_switch_root_session(&chat, 2)
            .expect("root=chat でも engine を持つ session への付け替えは通る");
        assert_eq!(
            session_registry::root("vp", "chatty"),
            2,
            "root が移っている"
        );
    }

    /// root を付け替えたら、[`LanePool::restart_lane`] の分岐述語（`root_act` = registry 直読）
    /// が**即座に新 root の act を返す**こと。
    ///
    /// 旧実装はこの述語が `LaneInfo.console_mode`（投影 cache）で、書き手が同期を 1 つ
    /// 忘れると root=chat → 非 root(tui) 付け替えで **PtySlot が永久に立たない** /
    /// 逆向きで **1 会話 2 engine** になった（doc 50 §4.7 の 15 例目）。doc 53 R1 で投影を
    /// 廃止し registry 直読になった — 本テストは「付け替え → 読み手が古い答えを見る」の
    /// 再演を、読み手と同じ経路（`pool.root_act`）で塞ぎ続ける（§8.6: 同じ性質の言い直し）。
    #[test]
    fn moving_root_updates_the_restart_predicate() {
        let _state = crate::test_env::state_dir();
        let addr = LaneAddress::performer("vp", "proj");
        let mut pool = LanePool::new();

        // root=chat の lane に、engine 持ちの非 root tui session を足す。
        insert_chat_lane(&mut pool, &addr);
        assert_eq!(pool.root_act(&addr), SessionAct::Chat);
        session_registry::create("vp", "proj", "echoes", "echoes", SessionAct::Tui, false)
            .expect("非 root tui session");

        // switch_root: 代表が tui になった = restart_lane の述語も即 Tui（PTY を立てる側）。
        pool.prepare_switch_root_session(&addr, 2)
            .expect("root=chat から tui session への付け替え");
        assert_eq!(
            pool.root_act(&addr),
            SessionAct::Tui,
            "root が tui に移ったら述語も Tui（Chat のままだと PTY が立たない）"
        );

        // 逆向き: chat session を代表にしたら述語も Chat（PTY を張って 2 engine にしない）。
        session_registry::create("vp", "proj", "echoes", "echoes", SessionAct::Chat, false)
            .expect("chat session");
        pool.prepare_switch_root_session(&addr, 3)
            .expect("tui root から chat session への付け替え");
        assert_eq!(
            pool.root_act(&addr),
            SessionAct::Chat,
            "root が chat に移ったら述語も Chat（Tui のままだと 1 会話 2 engine）"
        );

        // new_root: 新 root は既定レンズで立つ（doc 54 §3.1 — echoes は chat_capable → Chat）。
        pool.prepare_new_root_session(&addr, None)
            .expect("chat root からの New");
        assert_eq!(
            pool.root_act(&addr),
            SessionAct::Chat,
            "新 root は既定レンズ（Chat）で立つ = 述語も Chat"
        );

        // 特例（壊し方①）: shell を明示指定した新 root は Tui（chat レンズには映す会話が無い
        // = 定義。既定 Chat を一律にすると shell root が挙動不能になる）。
        pool.prepare_new_root_session(&addr, Some("shell"))
            .expect("shell 指定の New");
        assert_eq!(
            pool.root_act(&addr),
            SessionAct::Tui,
            "shell の新 root は Tui（default_act_for_stand の定義側）"
        );
    }

    #[test]
    fn lane_address_display_is_flat() {
        // doc 44 P2: 表示形は `<project>/<name>` 一本。開発起点は予約名なので旧形と一致する。
        assert_eq!(LaneAddress::root("vp").to_string(), "vp/root");
        assert_eq!(LaneAddress::performer("vp", "foo").to_string(), "vp/foo");
    }

    // deliver_nudge の並行 interleave 防止 (#674 race) の要は「同一 lane が同じ lock を共有し、
    // 別 lane は別 lock を持つ」こと。PTY 無しで検証できる直列化 invariant をここで固定する。
    #[test]
    fn nudge_lock_is_stable_per_lane_and_distinct_across_lanes() {
        let pool = LanePool::new();
        let a = LaneAddress::root("proj-a");
        let b = LaneAddress::root("proj-b");

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
        let addr = LaneAddress::root("proj");
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
        // Phase 1: LanePool::with_root は内部で PtySlot::spawn → tokio::task::spawn_blocking する。
        // 純 sync test だと runtime が無くて panic するので #[tokio::test] にする。
        let pool = LanePool::with_root("vp", "/tmp");
        assert_eq!(pool.count(), 1);
        let lanes = pool.list();
        assert_eq!(lanes.len(), 1);
        assert!(lanes[0].address.is_root());
        assert_eq!(lanes[0].stand, "echoes"); // default は "echoes" (PR-pre2 で "hd" → "echoes" rename)
    }

    /// doc 44 P2: 旧 `LaneKind` の serde テスト 2 本（snake_case / "worker" 拒否）は型ごと撤去。
    /// 代わりに固定すべきは「**P2 以前に永続した descriptor が読めること**」になった。
    #[test]
    fn legacy_lane_address_deserializes() {
        // 旧 conductor: name 省略 + kind field あり → 予約名に落ちる
        let conductor: LaneAddress =
            serde_json::from_str(r#"{"project":"vp","kind":"root"}"#).unwrap();
        assert_eq!(conductor, LaneAddress::root("vp"));
        assert!(conductor.is_root());

        // 旧 performer: name あり + kind field は unknown として無視される
        let performer: LaneAddress =
            serde_json::from_str(r#"{"project":"vp","kind":"performer","name":"foo"}"#).unwrap();
        assert_eq!(performer, LaneAddress::new("vp", "foo"));
        assert!(!performer.is_root());

        // 新形（kind なし）
        let flat: LaneAddress = serde_json::from_str(r#"{"project":"vp","name":"bar"}"#).unwrap();
        assert_eq!(flat, LaneAddress::new("vp", "bar"));
    }

    /// 旧 3 分節 address 文字列（`<project>/performer/<name>`）が新形に正規化されること。
    #[test]
    fn legacy_address_string_normalizes() {
        assert_eq!(
            LanePool::parse_address("vp/performer/foo").unwrap(),
            LaneAddress::new("vp", "foo")
        );
        assert_eq!(
            LanePool::parse_address("vp/wing/foo").unwrap(),
            LaneAddress::new("vp", "foo")
        );
        // 旧 conductor 名 "lead" も予約名に寄る
        assert_eq!(
            LanePool::parse_address("vp/lead").unwrap(),
            LaneAddress::root("vp")
        );
        // 新形はそのまま
        assert_eq!(
            LanePool::parse_address("vp/foo").unwrap(),
            LaneAddress::new("vp", "foo")
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
        let conductor = LanePool::parse_address("vp/root").unwrap();
        assert_eq!(conductor, LaneAddress::root("vp"));

        let performer = LanePool::parse_address("vp/performer/foo").unwrap();
        assert_eq!(performer, LaneAddress::performer("vp", "foo"));

        // CJK / kebab-case project name も通る
        let conductor2 = LanePool::parse_address("vantage-point/root").unwrap();
        assert_eq!(conductor2, LaneAddress::root("vantage-point"));

        // doc 44 P2: `vp/foo` は「未知 kind」ではなく **name が foo の lane** になった。
        assert_eq!(
            LanePool::parse_address("vp/foo").unwrap(),
            LaneAddress::new("vp", "foo")
        );

        // 不正
        assert!(LanePool::parse_address("vp").is_none()); // / 無し
        assert!(LanePool::parse_address("/root").is_none()); // project 空
        assert!(LanePool::parse_address("vp/").is_none()); // name 空
        assert!(LanePool::parse_address("vp/performer/").is_none()); // 旧形の name 空
        // 旧 "worker" token は受理しない（3 分節の互換は performer/wing のみ）
        assert!(LanePool::parse_address("vp/worker/foo").is_none());

        // 後方互換: root/performer rename 前の "lead"/"wing" address も受理する
        // (既存 session.json の active lane / 既存 wire address を orphan にしないため)
        assert_eq!(
            LanePool::parse_address("vp/lead").unwrap(),
            LaneAddress::root("vp")
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
            "address": {"project": "vp", "kind": "root"},
            "kind": "root",
            "state": "running",
            "stand": "echoes",
            "created_at": "2026-05-01T00:00:00Z",
            "cwd": "/tmp",
            "tmux": [{"stand": "echoes", "session": "vp-vp-root-echoes", "mode": "tmux"}]
        }"#;
        let info: LaneInfo = serde_json::from_str(legacy).expect("legacy payload decodes");
        assert_eq!(info.address, LaneAddress::root("vp"));
    }

    // ========================================================================
    // Phase 2 (Step E) — Lane lifecycle diff push (SystemEvent + Diff<I, P>)
    // ========================================================================

    #[test]
    fn lane_diff_add_serde_round_trip() {
        // Diff::Add { payload: LaneInfo } の wire 形式 + decode
        let info = LaneInfo {
            id: Default::default(),
            address: LaneAddress::performer("vp", "sub"),
            state: LaneState::Running,
            stand: "hd".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            pid: Some(12345),
            cwd: "/tmp".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
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
            id: Default::default(),
            address: LaneAddress::root("vp"),
            state: LaneState::Running,
            stand: "hd".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            pid: None,
            cwd: "/tmp".to_string(),
            performer_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            engine_stand: None,
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

    // =========================================================================
    // doc 46 P5 — slot は (lane, session) key（端末の複数枚化）
    // =========================================================================

    /// テスト用の PtySlot を 1 枚 spawn する（`sh -c <cmd>`、replay 永続なし）。
    /// - `"cat"`: 入力待ちで生き続ける（生きた slot）
    /// - `"exit 0"`: 即終了する（Dead 検出の対象）
    #[cfg(unix)]
    fn spawn_test_slot(
        cmd: &str,
    ) -> (
        crate::daemon::pty_slot::PtySlot,
        tokio::sync::broadcast::Receiver<Vec<u8>>,
    ) {
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        crate::daemon::pty_slot::PtySlot::spawn(
            &cwd,
            "/bin/sh",
            &["-c".to_string(), cmd.to_string()],
            &[],
            80,
            24,
            None,
        )
        .expect("PTY spawn")
    }

    /// `spawn_test_slot` の **replay を disk に永続する**版（本番の term slot と同じ形）。
    ///
    /// `replay_path` を持つ slot は `PtySlot::drop` が最終 flush で file を**書き戻す**。
    /// 掃除の順序（破壊の前か後か）を検証するテストは、この形でないと**罠を検出できない**
    /// （`None` の slot だと Drop が何も書かないので、順序を間違えても test が通る —
    /// 実際に一度それで取り逃した）。出力を 1 回 recv して buffer が埋まるのを待つ。
    #[cfg(unix)]
    async fn spawn_test_slot_with_replay(
        cmd: &str,
        replay: &std::path::Path,
    ) -> (
        crate::daemon::pty_slot::PtySlot,
        tokio::sync::broadcast::Receiver<Vec<u8>>,
    ) {
        let cwd = std::env::temp_dir().to_string_lossy().to_string();
        let (slot, mut rx) = crate::daemon::pty_slot::PtySlot::spawn(
            &cwd,
            "/bin/sh",
            &["-c".to_string(), cmd.to_string()],
            &[],
            80,
            24,
            Some(replay.to_path_buf()),
        )
        .expect("PTY spawn");
        // 出力が replay buffer に入るまで待つ（Drop の final flush が空を書かないように）。
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await;
        (slot, rx)
    }

    /// 当該 slot が終了する（is_alive=false になる）まで待つ。
    #[cfg(unix)]
    async fn wait_until_slot_dead(pool: &LanePool, addr: &LaneAddress, key: SessionKey) {
        for _ in 0..60 {
            if pool
                .slot_inventory(addr)
                .iter()
                .any(|s| s.session == key && !s.alive)
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("slot が終了しない (session={key})");
    }

    /// **P5 の本体**: 1 つの lane に 2 session ぶんの slot が同居し、write / attach / capture が
    /// 互いに独立であること。旧実装（lane に 1 本）では 2 枚目の insert が 1 枚目を replace して
    /// いたので、この test は型が変わったことの直接の証跡になる。
    ///
    /// ⚠️ `term_attaches`（`pty_slots` の双子）まで見るのが要点 — 片方だけ re-key しても
    /// コンパイルは通るので、capture が「別 slot の画面」を返さないことで対を固定する。
    #[cfg(unix)]
    #[tokio::test]
    async fn slots_coexist_per_session_and_stay_independent() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);

        // root(#1) と 非 root(#2) にそれぞれ生きた slot を立てる。
        let (slot1, rx1) = spawn_test_slot("cat");
        pool.insert_pty_slot(addr.clone(), Some(1), slot1, rx1);
        let (slot2, rx2) = spawn_test_slot("cat");
        pool.insert_pty_slot(addr.clone(), Some(2), slot2, rx2);

        assert_eq!(
            pool.slot_sessions(&addr),
            vec![1, 2],
            "2 枚の slot が同居する（旧実装は 2 枚目が 1 枚目を replace していた）"
        );
        let inv = pool.slot_inventory(&addr);
        assert_eq!(inv.len(), 2);
        assert!(inv[0].root, "#1 は root（registry 不在 = root=1 の既定形）");
        assert!(!inv[1].root, "#2 は非 root");
        assert!(inv.iter().all(|s| s.alive && s.attached));
        assert_ne!(inv[0].pid, inv[1].pid, "別プロセスとして立っている");

        // 出力の購読も slot ごとに独立に取れる（attach_output の re-key）。
        let (_replay1, mut out1) = pool.attach_output(&addr, Some(1)).expect("attach #1");
        let (_replay2, mut out2) = pool.attach_output(&addr, Some(2)).expect("attach #2");

        // #1 にだけ書く → #1 の broadcast にだけ届く。
        pool.write_to_lane(&addr, Some(1), b"vp-slot-one\n")
            .expect("write #1");
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), out1.recv())
            .await
            .expect("#1 の出力が来ない (timeout)")
            .expect("#1 broadcast closed");
        assert!(
            String::from_utf8_lossy(&got).contains("vp-slot-one"),
            "書いた slot の出力に現れる: {:?}",
            String::from_utf8_lossy(&got)
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), out2.recv())
                .await
                .is_err(),
            "書いていない slot には何も届かない（slot 間の独立）"
        );

        // 双子（TermAttach）も同じ key で引けている = capture が slot ごとに割れる。
        let mut captured = String::new();
        for _ in 0..60 {
            captured = pool.capture_lane(&addr, Some(1)).expect("capture #1");
            if captured.contains("vp-slot-one") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            captured.contains("vp-slot-one"),
            "#1 の Term grid に書いた文字が乗る: {captured:?}"
        );
        assert!(
            !pool
                .capture_lane(&addr, Some(2))
                .expect("capture #2")
                .contains("vp-slot-one"),
            "#2 の Term grid には混ざらない（term_attaches も session key で割れている）"
        );

        // session 省略 = root（#1）に解決される（chat 系の「省略 = focused」と違う）。
        assert!(
            pool.capture_lane(&addr, None)
                .expect("capture root")
                .contains("vp-slot-one"),
            "session 省略は root（#1）の画面"
        );
    }

    /// 法の番人（doc 46 P5 で session 粒度に精密化）: **同一 session に PTY slot と
    /// chat engine は同居できない**。旧実装は lane 全体の `pty_slots` 有無を focused の時だけ
    /// 見ていたので、この 2 つの判定（当該 session / 他 session）は区別できなかった。
    #[cfg(unix)]
    #[tokio::test]
    async fn same_session_cannot_hold_both_pty_slot_and_chat_engine() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        // act ガードを通し、PTY 排他の判定だけを裸で見る。
        // ⚠️ doc 50 §4.6 A6 で act ガードは **registry の session ごとの act** を見るように
        // なった（旧: lane cache の console_mode）。`insert_lane` は cache しか設定しないので、
        // registry 側にも Chat を書く必要がある（registry が SSOT = doc 47 §4）。
        insert_lane(&mut pool, &addr, SessionAct::Chat);
        let lane_label = crate::process::stand_spawner::lane_label(&addr);
        session_registry::set_root_act(&addr.project, lane_label, "echoes", SessionAct::Chat)
            .expect("root の act を chat に");
        let router = std::sync::Arc::new(crate::process::topic_router::TopicRouter::new());

        // 非 focused 側の比較対象として、engine を持たない legacy stand の session #2 を作る
        // （engine spawn を実際に走らせないための足場 — 既存 test と同じ手筋）。
        let k2 = session_registry::create(
            &addr.project,
            lane_label,
            "echoes",
            "cursor",
            SessionAct::Chat,
            false,
        )
        .expect("create #2");

        // focused/root の #1 に slot を立てる（= 不変条件違反の状態を作る）。
        let (slot, rx) = spawn_test_slot("cat");
        pool.insert_pty_slot(addr.clone(), Some(1), slot, rx);

        let err = pool
            .ensure_chat_engine(&addr, Some(1), &router)
            .expect_err("同一 session の chat engine は拒否される");
        assert!(
            err.to_string().contains("同一 session に PTY slot"),
            "法の番人が session 粒度で弾く: {err}"
        );

        // 別 session（#2）は slot を持たないので PTY 排他には触れず、engine 能力の防壁まで進む
        // = 「lane に slot があるから全部ダメ」という lane 全体の近似ではないことの証跡。
        let err = pool
            .ensure_chat_engine(&addr, Some(k2), &router)
            .expect_err("legacy stand は engine を持てない");
        assert!(
            err.to_string().contains("Act II chat host を持ちません"),
            "他 session は PTY 排他を通過して能力防壁に到達する: {err}"
        );

        // 逆向き: #2 に slot を立てると、#2 の ensure も PTY 排他で先に弾かれる。
        let (slot2, rx2) = spawn_test_slot("cat");
        pool.insert_pty_slot(addr.clone(), Some(k2), slot2, rx2);
        let err = pool
            .ensure_chat_engine(&addr, Some(k2), &router)
            .expect_err("slot を持つ session は非 focused でも拒否");
        assert!(
            err.to_string().contains("同一 session に PTY slot"),
            "非 focused でも同一 session の同居は禁じる: {err}"
        );
    }

    /// lifecycle（doc 46 P5 の決定）: **lane を Dead にするのは root slot の死だけ**。
    /// 非 root slot が死んでも、その slot を畳むだけで lane は Running のまま
    /// （lane の代表は root — 同居人が 1 人倒れただけで場を閉じない）。
    #[cfg(unix)]
    #[tokio::test]
    async fn only_root_slot_death_marks_lane_dead() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);

        // root(#1) = 生存、#2 = 即終了。
        let (slot1, rx1) = spawn_test_slot("cat");
        pool.insert_pty_slot(addr.clone(), Some(1), slot1, rx1);
        let (slot2, rx2) = spawn_test_slot("exit 0");
        pool.insert_pty_slot(addr.clone(), Some(2), slot2, rx2);
        wait_until_slot_dead(&pool, &addr, 2).await;

        assert_eq!(
            pool.detect_and_mark_dead(),
            0,
            "非 root slot の死では lane state を動かさない"
        );
        assert_eq!(
            pool.get(&addr).expect("lane").state,
            LaneState::Running,
            "lane は Running のまま（root は生きている）"
        );
        assert_eq!(
            pool.slot_sessions(&addr),
            vec![1],
            "死んだ slot だけが畳まれる"
        );
        assert!(
            pool.capture_lane(&addr, Some(2)).is_none(),
            "双子（TermAttach）も一緒に消える = Dead slot の凍結画面が残らない"
        );

        // root(#1) を死ぬ slot に張り替える → 今度は lane が Dead になる。
        let (dying, rx) = spawn_test_slot("exit 0");
        pool.insert_pty_slot(addr.clone(), Some(1), dying, rx);
        wait_until_slot_dead(&pool, &addr, 1).await;
        assert_eq!(
            pool.detect_and_mark_dead(),
            1,
            "root slot の死は lane の死（従来どおり）"
        );
        assert_eq!(pool.get(&addr).expect("lane").state, LaneState::Dead);
        assert!(pool.slot_sessions(&addr).is_empty());
    }

    /// `remove(addr)` は lane ごと消えるので **全 session の slot と双子**が残らないこと。
    /// 「消えたか」でなく「残っていないか」を見る（主対象だけ見ると後始末を見落とす）。
    #[cfg(unix)]
    #[tokio::test]
    async fn remove_lane_drops_every_session_slot_and_twin() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);
        for key in [1, 2, 3] {
            let (slot, rx) = spawn_test_slot("cat");
            pool.insert_pty_slot(addr.clone(), Some(key), slot, rx);
        }
        assert_eq!(pool.slot_sessions(&addr), vec![1, 2, 3]);

        pool.remove(&addr);

        assert!(
            !pool.pty_slots.contains_key(&addr),
            "pty_slots に lane の entry が残らない"
        );
        assert!(
            !pool.term_attaches.contains_key(&addr),
            "双子の term_attaches にも残らない（片側だけ消すと凍結画面が生き残る）"
        );
        assert!(pool.slot_sessions(&addr).is_empty());
        assert!(pool.get(&addr).is_none());
    }

    /// restart の意味論（doc 46 P5 の決定）その 1: `Resume` / `Bare` が張り替えるのは
    /// **root slot だけ**。同居している非 root slot は巻き添えにしない
    /// （step 2 の `build_stand_command` が root entry で engine / resume を決めるのと同じ主語）。
    ///
    /// lane の stand を `"shell"` にしてあるのは、restart が実 spawn を伴うため
    /// （`"echoes"` だと login shell に claude を type-ahead 注入してしまう）。
    #[cfg(unix)]
    #[tokio::test]
    async fn resume_restart_respawns_root_slot_only() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);
        if let Some(info) = pool.lanes.get_mut(&addr) {
            info.stand = "shell".to_string(); // engine を注入しない slot（login shell のみ）
        }
        for key in [1, 2] {
            let (slot, rx) = spawn_test_slot("cat");
            pool.insert_pty_slot(addr.clone(), Some(key), slot, rx);
        }
        let pid_of = |pool: &LanePool, key: SessionKey| {
            pool.slot_inventory(&addr)
                .into_iter()
                .find(|s| s.session == key)
                .map(|s| s.pid)
        };
        let (root_before, mate_before) = (pid_of(&pool, 1), pid_of(&pool, 2));

        pool.restart_lane(&addr, RespawnMode::Resume)
            .expect("resume restart");

        assert_eq!(
            pool.slot_sessions(&addr),
            vec![1, 2],
            "同居人（#2）は restart を生き延びる"
        );
        assert_ne!(
            pid_of(&pool, 1),
            root_before,
            "root(#1) の slot は張り替わる"
        );
        assert_eq!(
            pid_of(&pool, 2),
            mate_before,
            "非 root(#2) の slot はそのまま（別の住人の restart に巻き込まれない）"
        );
        assert!(
            pool.capture_lane(&addr, Some(2)).is_some(),
            "双子（TermAttach）も #2 のぶんは残る"
        );
    }

    /// テスト用の「偽 chat engine」を当該 session に据える（法の番人・逆向きの検証用）。
    ///
    /// 要るのは `chat_engines[addr][key]` に entry が居るという**事実だけ**なので、claude の
    /// 代わりに `/bin/cat` を spawn する（引数を解さず即終了するが、map に居ることは変わらない）。
    /// 本物の engine を要求すると claude CLI 必須のテストになり CI で回せない。
    ///
    /// ⚠️ `EchoesAgentHost::spawn` は claude path で `--forward-subagent-text` 対応を probe し
    /// **プロセス内 OnceLock に cache** する。cat は「illegal/unrecognized option」を吐く =
    /// 判定文言（"unknown option"）を含まないので cache 値は本物 claude と同じ `true` に落ちる
    /// （= 同一 test binary の `--ignored` 実機テストを歪めない）。
    #[cfg(unix)]
    fn insert_fake_chat_engine(pool: &mut LanePool, addr: &LaneAddress, key: SessionKey) {
        let host = crate::echoes::EchoesAgentHost::spawn(crate::echoes::EchoesHostConfig {
            cwd: std::env::temp_dir().to_string_lossy().to_string(),
            project: addr.project.clone(),
            lane: "fake-engine".to_string(),
            lane_label: "fake-engine".to_string(),
            session_key: key,
            resume_session_id: None,
            model: None,
            claude_cli_path: Some("/bin/cat".to_string()),
        })
        .expect("偽 engine の spawn");
        pool.chat_engines.entry(addr.clone()).or_default().insert(
            key,
            crate::echoes::ChatEngineSlot {
                host: crate::echoes::ChatHost::Claude(host),
                pump: tokio::spawn(async {}),
            },
        );
    }

    /// **P5 producer の本体**（doc 46 §3 の宿題）: 非 root session に console を 1 枚立てる。
    ///
    /// 見るのは「2 枚目が `vp lane slots`（= `slot_inventory`）に出るか」と、
    /// **lane の代表（root）が 1 mm も動いていないか**の 2 点。後者が崩れると
    /// mailbox / pid / Dead 判定の主語が同居人に移る（doc 40 §4-1 の据え置き事項）。
    #[cfg(unix)]
    #[tokio::test]
    async fn open_new_slot_adds_a_second_console_without_moving_root() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);

        // 既存の root slot（boot 経路が立てたもの相当）。
        let (slot, rx) = spawn_test_slot("cat");
        pool.insert_pty_slot(addr.clone(), Some(1), slot, rx);
        let root_pid = pool.slot_inventory(&addr)[0].pid;

        // producer: engine を明示して 1 枚追加（"shell" = claude を注入しない console）。
        let (key, pid) = pool
            .open_new_slot(&addr, Some("shell"))
            .expect("console slot が立つ");
        assert_eq!(
            key, 2,
            "新しい session が採番される（既存の再利用ではない）"
        );

        let inv = pool.slot_inventory(&addr);
        assert_eq!(inv.len(), 2, "`vp lane slots` に 2 枚出る: {inv:?}");
        assert_eq!(inv[1].session, 2);
        assert_eq!(inv[1].pid, pid, "戻り値の pid が一覧の pid と一致する");
        assert!(inv[1].alive, "立てた console は生きている");
        assert!(
            inv[1].attached,
            "双子（TermAttach）も張られている = capture できる"
        );
        assert!(!inv[1].root, "同居人であって lane の代表ではない");
        assert_eq!(inv[0].pid, root_pid, "root slot は張り替えられていない");
        assert!(
            pool.capture_lane(&addr, Some(2)).is_some(),
            "`vp lane capture --session 2` で読める"
        );

        // registry: 新 session は Act=Tui の同居人。root / focused は動かない。
        let reg = session_registry::load("vp", "root", "echoes");
        assert_eq!(reg.root, 1, "root は動かない（mailbox の主は root のまま）");
        assert_eq!(
            reg.focused, 1,
            "focused も動かない（chat 動詞の宛先を奪わない）"
        );
        let entry = reg
            .sessions
            .iter()
            .find(|s| s.key == key)
            .expect("registry に新 session");
        assert_eq!(entry.act, SessionAct::Tui, "console なので Act I");
        assert_eq!(entry.stand, "shell", "明示した engine で立つ");
        assert_eq!(
            pool.get(&addr).expect("lane").state,
            LaneState::Running,
            "lane の state は代表（root）のもの — 同居人の追加で動かない"
        );
    }

    /// **法の番人・逆向き**（doc 46 P5 の宿題の中核）: chat engine が居る session に slot は
    /// 立てられない。`ensure_chat_engine` が持つ「slot が居たら chat を拒否」の鏡像で、
    /// 両向き揃って初めて「1 session = 高々 1 エンジン」が法として閉じる。
    ///
    /// 併せて slot 側の入口が断る残り 3 つ（Act=Chat の session / 既に console がある session /
    /// registry に居ない session）も同じ場所で固定する。
    #[cfg(unix)]
    #[tokio::test]
    async fn open_slot_refuses_when_the_session_already_has_an_engine() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);

        // #2: Act=Tui の同居人 session（producer が作るのと同じ形）に chat engine を据える。
        let k2 = session_registry::create("vp", "root", "echoes", "shell", SessionAct::Tui, false)
            .expect("create #2");
        insert_fake_chat_engine(&mut pool, &addr, k2);
        let err = pool
            .open_slot_for_session(&addr, k2)
            .expect_err("engine が居る session に console は立てられない");
        assert!(
            err.to_string().contains("同居できません"),
            "法（1 session = 高々 1 エンジン）で断る: {err}"
        );

        // #3: Act=Chat の session（headless engine の器）にも立てない。
        let k3 =
            session_registry::create("vp", "root", "echoes", "echoes", SessionAct::Chat, false)
                .expect("create #3");
        let err = pool
            .open_slot_for_session(&addr, k3)
            .expect_err("Act II の session に console は立てられない");
        assert!(err.to_string().contains("Act II"), "Act で断る: {err}");

        // 既に console がある session は無言で殺さず断る（insert_pty_slot は replace するため）。
        let (slot, rx) = spawn_test_slot("cat");
        pool.insert_pty_slot(addr.clone(), Some(1), slot, rx);
        let err = pool
            .open_slot_for_session(&addr, 1)
            .expect_err("走行中 console の上書きは断る");
        assert!(
            err.to_string().contains("既に console"),
            "既存 slot を守る: {err}"
        );

        // registry に居ない session の端末は作らない。
        let err = pool
            .open_slot_for_session(&addr, 99)
            .expect_err("実在しない session の console は作らない");
        assert!(
            err.to_string().contains("session が存在しません"),
            "実在チェック: {err}"
        );
    }

    /// 入口が弾くもの（`open_new_slot` 側）と、spawn 失敗時の巻き戻し。
    ///
    /// 採番した session は root でも focused でもない = **誰も指していない**ので、slot が
    /// 立たなければ registry に残す理由がない（残すと「console の無い幽霊タブ」になる）。
    #[cfg(unix)]
    #[tokio::test]
    async fn open_new_slot_rejects_unknown_engine_and_rolls_back_failed_spawn() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_lane(&mut pool, &addr, SessionAct::Tui);

        // 未知 engine は入口で断る（通すと shell 層に落ちて「engine の無い console」が黙って建つ）。
        let err = pool
            .open_new_slot(&addr, Some("opus-xhigh"))
            .expect_err("未知 stand は拒否");
        assert!(
            err.to_string().contains("engine が未知"),
            "行き止まりの console を作らない: {err}"
        );
        assert_eq!(
            session_registry::load("vp", "root", "echoes")
                .sessions
                .len(),
            1,
            "弾いた入力で session を採番しない"
        );

        // slot が立たなかった時の巻き戻し。実 spawn の失敗（shell 不在 / PTY 枯渇）は環境依存で
        // test から再現できないので、**同じ失敗経路**（`open_slot_for_session` の Err）を入口
        // guard で作る: 次に採番される key(#2) に orphan slot を置いておくと「既に console」で断られる。
        let (orphan, rx) = spawn_test_slot("cat");
        pool.insert_pty_slot(addr.clone(), Some(2), orphan, rx);
        let err = pool
            .open_new_slot(&addr, Some("shell"))
            .expect_err("slot が立たなければ Err");
        assert!(
            err.to_string().contains("既に console"),
            "失敗理由がそのまま返る: {err}"
        );
        assert_eq!(
            session_registry::load("vp", "root", "echoes")
                .sessions
                .len(),
            1,
            "失敗したら何も遷移していない（幽霊 session を残さない）"
        );
        assert_eq!(
            pool.slot_sessions(&addr),
            vec![2],
            "既存 slot（この test では orphan）は無傷 — 巻き戻しは registry 側だけ"
        );
    }

    /// restart の意味論その 2: `Reset` は registry を既定形（N=1）へ戻す = 非 root session が
    /// registry から消えるので、その slot を残すと「もう存在しない session の端末」になる。
    /// → **全 slot** を畳む。chat lane で回すのは早期 return 前の畳み込みを裸で見るため。
    #[cfg(unix)]
    #[tokio::test]
    async fn reset_restart_drops_every_slot() {
        let _state = crate::test_env::state_dir_async().await;
        let addr = LaneAddress::root("vp");
        let mut pool = LanePool::new();
        insert_chat_lane(&mut pool, &addr);
        session_registry::create("vp", "root", "echoes", "codex", SessionAct::Chat, false)
            .expect("create #2");
        for key in [1, 2] {
            let (slot, rx) = spawn_test_slot("cat");
            pool.insert_pty_slot(addr.clone(), Some(key), slot, rx);
        }

        pool.restart_lane(&addr, RespawnMode::Reset)
            .expect("reset restart");

        assert!(
            pool.slot_sessions(&addr).is_empty(),
            "Reset は registry を N=1 に戻すので全 slot を畳む（orphan slot を残さない）"
        );
        assert!(!pool.term_attaches.contains_key(&addr), "双子も残らない");
        assert_eq!(
            session_registry::load("vp", "root", "echoes")
                .sessions
                .len(),
            1,
            "registry も既定形へ（slot を畳む根拠）"
        );
    }
}
