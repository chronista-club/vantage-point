//! lane の帳簿値（値型、棚卸し 項目 9-1b で `state.rs` から分離）。
//!
//! `LaneInfo`（wire に載る descriptor）/ `LaneState` / `LaneLifecycle` / `Diff` / `SystemEvent` /
//! `LaneSessionsView`。disk / engine / lock を触らない。descriptor は帳簿の永続形で、slot は
//! in-memory な runtime 事実（doc 46 の境界）— 混ぜない。disk（session registry）と engine catalog を
//! 読んで値を完成させる投影は [`super::enrich`]（9-1c で分離）。

use serde::{Deserialize, Serialize};

use super::address::{LaneAddress, LaneId};
use crate::lane::session_registry::{SessionKey, SessionMode};

// doc 44 P2: `LaneKind`（Main / Sub）は撤去。
//
// D4「lane 自身は役割状態を持たない」— lane は全て対等になり、開発起点は
// [`super::address::ROOT_LANE_NAME`] の予約名（将来は Host が持つポインタ）で表される。
// 旧 kind の唯一の実質は「main は repo に 1 本・worktree を持たない」だが、
// それは **名前の一意性**（1 repo に同名 lane は 1 本）で既に表現されている。

/// Lane の state machine 状態 (Phase A4-2b では Running 固定で pre-populate)
///
/// 注意: 「lane disk dir 存在 + Pane 不在」 は **Lane state ではなく `pid: None` で表現する** 設計。
/// Active/Inactive 概念は Repo 集約 (sidebar 側 client-side computed) として扱い、 Lane state には混ぜない。
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
/// 別 table (`lane/lifecycle`) に永続する (repo push が descriptor を round-trip して clobber する
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

/// Phase 2 (Step E): エンティティ lifecycle の diff event を表現する generic ADT。
///
/// - `I` = identifier 型 (削除時のみ必要、 例: `LaneAddress`)
/// - `P` = payload 型 (add/update 時の full state、 例: `LaneInfo`)
///
/// caller で event 発生 → AppState の broadcast channel に publish → subscriber が
/// daemon 側 cache を realtime sync する primitive。
///
/// doc 44 P1 (fold-in): subscriber は旧「repo の QUIC registry push」から、repo 自身の
/// lanes publish task（`process/server.rs` の `publish_lanes`）に替わった。同一プロセスに
/// なったので push は daemon の集約 view への map 書き込みに退化している。
///
/// wire format: internally tagged JSON
/// ```json
/// {"kind": "add", "payload": {...}}
/// {"kind": "remove", "id": {...}}
/// {"kind": "update", "payload": {...}}
/// ```
///
/// QUIC channel は ordered (single connection) なので、 register snapshot → diff の順序保証あり。
/// 将来 `Diff<PaneId, PaneInfo>` / `Diff<ComponentKind, AgentInfo>` 等の type alias で reuse 可能。
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

/// Phase 2: Lane lifecycle 用の Diff alias。 repo の lane_pool 変更を daemon に伝える。
pub type LaneDiff = Diff<LaneAddress, LaneInfo>;

/// Phase 2 (Step E): repo の system 系 lifecycle event を 1 つの broadcast bus で配信。
///
/// caller (lane_spawn_actor / lane_lifecycle / lifecycle monitor / restart_lane 等) が
/// `state.system_event_tx.send(SystemEvent::*)` で publish、repo の lanes publish task
/// (`publish_lanes`) が受けて daemon の集約 view を更新する（doc 44 P1 fold-in で
/// 旧 QUIC registry push から置き換わった）。
///
/// scope ごとに variant 分け、 内部に該当 Diff を内包。 将来 Pane / Agent 等は
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
    //   component(Diff<ComponentKind, AgentInfo>),  // 各機能 の lifecycle
    //   Process(Diff<ProcessKey, RunningRepo>),  // Process registry diff
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
    /// agent 名 (例: "hd" / "shell" / "tmux"、 doc 11 PR-B で String に変更)
    pub agent: String,
    /// ISO 8601
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub cwd: String,
    /// Phase 5-D: Sub のみ embed (Main は git workspace を持たない設計)。
    /// `cwd` から `lane::commands::sub_status()` を呼んで populate。
    /// `/api/lanes` 応答時に lazy 取得 (registry には保存しない、 git 状態は volatile)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_status: Option<crate::lane::commands::SubStatus>,
    /// R3-b → doc 39 §3-1: この lane の **root session** の CC session id（wire 配送は常に
    /// root = lane の人格に解決する）。 registry には保存せず `/api/lanes` 応答時に root
    /// session の state file (`lane::cc_session`、 書き手は SessionStart/UserPromptSubmit hook)
    /// を lazy read する (`sub_status` と同じ前例)。 conversation の `--resume` 再利用と
    /// R3-c の `--bg` session 管理の土台。
    ///
    /// ⚠️ **claude 専用の契約**: delivery_actor（channel D）が `claude -p --resume <id>` に
    /// 使うため、他 engine の id を入れてはならない（root が非 claude session の場合、その
    /// label の cc_session store には書き手がいないため自然に None になる）。表示用の
    /// engine 横断 id は [`Self::engine_session_id`]（別契約）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc_session_id: Option<String>,
    /// doc 37: この lane の **active engine の** session id（claude=cc_session / codex=thread id /
    /// grok=ACP sessionId。shell は None）。Conversation 共通ヘッダの session chip 用（表示専用 —
    /// resume に使うのは registry の会話 id / `cc_session_id` 側）。doc 40: 供給は registry
    /// （root session の conversation）に一本化。serde default + skip で wire 後方互換。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_session_id: Option<String>,
    /// doc 39 P4-C: この lane の **root session の agent**（= slot に載る engine 種別）。tui の
    /// session chip prefix の供給源（`agent` は lane 作成時固定なので cross-engine root では slot の
    /// engine と食い違う — chip が旧 engine の prefix で点く）。`engine_session_id` と同じ
    /// [`super::enrich::refresh_engine_session_id`] で populate。root entry 不在は None = vp-app 側が従来の
    /// lane `agent` に fallback。serde default + skip で wire 後方互換。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    /// doc 40 §3 → doc 53 §11: lane の session roster（**wire view**）。
    ///
    /// GUI の roster（pane 一覧の元）は**これ 1 本**で供給される（旧: `conversation_session_list`
    /// の fetch と snapshot の 2 本立てで、fetch は GUI 自身の動詞でしか撃たれないため
    /// **CLI / MCP 由来の session 変化が pane に出なかった** — doc 53 §11.1）。
    ///
    /// ⚠️ **disk 型（`SessionRegistry`）を直に載せない** — roster には `chat_capable` のような
    /// **導出値**が要り（能力表は server が SSOT = client に engine 名の分岐を作らない）、
    /// disk の永続形に runtime 由来の field を混ぜないため wire 専用型に分ける（§11.2 決定 3）。
    /// populate は [`super::enrich::refresh_engine_session_id`]（enrich 供給点）。
    /// serde default + skip で wire 後方互換。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<LaneSessionsView>,
    /// FSM 投影 (2026-07-11): dev-flow FSM (`flow::derive_flow_state`) の現在 state。
    /// **daemon が vp-app への snapshot 送信時に enrich する derive 値** — repo / lane_registry /
    /// db では常に `None` (derive できるものは store しない原則)。 source は wire store
    /// (latest msg + 未 ack needs_user) + sub_status で、 `vp flow progress` と同一判定。
    /// serde default + skip で旧 SP / 旧 client と wire 完全互換。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_state: Option<crate::flow::FlowState>,
}

/// lane の session roster の **wire view**（disk 型 `SessionRegistry` の投影 + 導出値）。
///
/// doc 53 §11: GUI の roster 供給はこれ 1 本（`LaneInfo.sessions`）。registry を丸ごと載せる
/// のではなく「client が roster を描くのに要るもの」だけを写し、能力（`chat_capable`）は
/// **server が導出**して載せる。`next`（採番カーソル）は client に読み手が無いので写さない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneSessionsView {
    /// lane の器（slot / mailbox）に化身する session。
    pub root: SessionKey,
    /// 現在 focus されている session。
    pub focused: SessionKey,
    /// session 一覧（生成順）。
    pub sessions: Vec<LaneSessionView>,
}

/// [`LaneSessionsView`] の 1 session。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneSessionView {
    pub key: SessionKey,
    /// engine 種別（agent 名）。
    pub agent: String,
    /// この session の Mode（見え方）。
    pub mode: SessionMode,
    /// engine の会話 id（registry が SSOT。Draft = None）。session chip / タブの表示用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// この session を Chat にできるか（能力表 = `EngineKind` が SSOT、server 導出）。
    /// 名札の kind badge がこれで gate する（押しても弾かれる行き止まりを作らない）。
    #[serde(default)]
    pub chat_capable: bool,
    /// user の投入に**画像**を混ぜられるか（chat 入力欄への貼り付け、2026-08-30）。
    /// client はこれが false の lane で貼り付け UI を出さない（chat_capable と同じ規律 —
    /// 押しても engine に無視されるだけの行き止まりを作らない）。
    #[serde(default)]
    pub image_capable: bool,
    /// この session の model 指定（registry の intent。None = engine 既定に委譲）。
    /// picker の「現在値」は engine 実測（session_init の header.model）が正で、
    /// こちらは「VP が spawn 時に何を注入するか」の側。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// model picker の選択肢（`EngineKind` catalog、server 導出 — client は並べるだけ）。
    /// **空 = VP からの model 切替なし**（client は read-only 表示 or 非表示に落とす —
    /// chat_capable と同じく「押しても弾かれる行き止まり」を server 表明で根絶する）。
    #[serde(default)]
    pub model_choices: Vec<crate::conversation::engine::Choice>,
    /// permission picker の選択肢（同上）。空 = 対話承認の概念なし（`set_permission_mode`
    /// が bail する engine — codex の approval_policy 等の別語彙は将来 catalog を足すだけ）。
    #[serde(default)]
    pub permission_choices: Vec<crate::conversation::engine::Choice>,
    /// 最終活動時刻 (epoch ms)。tui = PTY 出力 / gui = ConversationEvent の新しい方。
    /// None = 実体なし（Draft / 停止中）or 活動未観測。registry（disk）でなく
    /// **in-memory 実体からの enrich**（[`super::pool::LanePool::session_activity`]）なので
    /// [`super::enrich::sessions_view_from_registry`] 時点では常に None — 供給点（5s snapshot / LaneDiff push）が埋める。
    /// GUI は client 時計との差で「quiet N 分」を導く（閾値判定は載せない — 事実だけ運ぶ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<u64>,
}

/// `last_activity_at` を wire に載せる際の量子化粒度 (ms)。GUI の quiet 閾値（分単位）には
/// 十分細かく、snapshot 指紋（doc 44 §11.3）を無駄に乱さない下限。
const ACTIVITY_WIRE_GRANULARITY_MS: u64 = 60_000;

/// wire に載せる活動時刻の量子化（[`ACTIVITY_WIRE_GRANULARITY_MS`] へ切り下げ）。
pub(super) fn quantize_activity_ms(ms: u64) -> u64 {
    ms - ms % ACTIVITY_WIRE_GRANULARITY_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_info_worker_status_alias_rejected() {
        // `worker_status` serde alias 削除の回帰ガード。
        // 旧 SP が `worker_status` キーで送ってきても、 新 repo は sub_status: None として扱う
        // (= 情報損失は許容、 crash やパース失敗より優先)。
        // `#[serde(default)]` が残っているので unknown field は無視され None になる。
        let json = r#"{
            "address": {"repo": "vp", "kind": "sub", "name": "foo"},
            "kind": "sub",
            "name": "foo",
            "state": "running",
            "agent": "claude",
            "created_at": "2026-05-26T00:00:00Z",
            "cwd": "/tmp",
            "worker_status": {"branch": "main", "ahead": 0, "behind": 0, "is_merged": false, "has_changes": false}
        }"#;
        let info: LaneInfo = serde_json::from_str(json).expect("パース自体は成功する");
        assert!(
            info.sub_status.is_none(),
            "worker_status キーは sub_status に流れ込まない (alias 削除済)"
        );
    }

    /// tmux decoupling PR2: 旧 wire payload (tmux field 入り) が新 LaneInfo に decode できる
    /// （unknown field は serde が無視 = 旧 client / 旧 DB descriptor との後方互換）。
    #[test]
    fn lane_info_decodes_legacy_payload_with_tmux_field() {
        let legacy = r#"{
            "address": {"repo": "vp", "kind": "main"},
            "kind": "main",
            "state": "running",
            "agent": "claude",
            "created_at": "2026-05-01T00:00:00Z",
            "cwd": "/tmp",
            "tmux": [{"agent": "claude", "session": "vp-vp-root-conversation", "mode": "tmux"}]
        }"#;
        let info: LaneInfo = serde_json::from_str(legacy).expect("legacy payload decodes");
        assert_eq!(info.address, LaneAddress::root("vp"));
    }

    // Phase 2 (Step E) — Lane lifecycle diff push (SystemEvent + Diff<I, P>)
    #[test]
    fn lane_diff_add_serde_round_trip() {
        // Diff::Add { payload: LaneInfo } の wire 形式 + decode
        let info = LaneInfo {
            id: Default::default(),
            address: LaneAddress::sub("vp", "sub"),
            state: LaneState::Running,
            agent: "hd".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            pid: Some(12345),
            cwd: "/tmp".to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
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
        let addr = LaneAddress::sub("vp", "osc");
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
            agent: "hd".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            pid: None,
            cwd: "/tmp".to_string(),
            sub_status: None,
            cc_session_id: None,
            sessions: None,
            engine_session_id: None,
            agent_name: None,
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

    /// wire 量子化（doc 44 §11.3 — snapshot 指紋を活動のたびに乱さない）は分へ切り下げる。
    #[test]
    fn quantize_activity_ms_floors_to_minute() {
        assert_eq!(quantize_activity_ms(0), 0);
        assert_eq!(quantize_activity_ms(59_999), 0);
        assert_eq!(quantize_activity_ms(60_000), 60_000);
        assert_eq!(quantize_activity_ms(1_756_300_123_456), 1_756_300_080_000);
    }
}
