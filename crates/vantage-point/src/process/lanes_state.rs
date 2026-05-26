//! Lane state types — SP が持つ Lane (Lead/Wing) の data model
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
//! Lane は **Lead/Wing の PTY セッション + Stand container** に集中:
//! - Lead 1 / project (固定)、stand = "echoes" / "shell" / "tmux"
//! - Wing 0..n / project (可変、lane clone)、stand 同上
//!
//! ## Phase A4-2b スコープ
//!
//! `LanePool::with_lead` で Lead Lane 1 つ pre-populate。
//! Wing create / destroy / Stand 切替は A4-4 / A5 で実装。

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Lane の種別 (memory rule: HD/TH を起動する Lane だけ)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneKind {
    /// 1 / project (固定)、LaneStand = HD or TH
    Lead,
    /// 0..n / project (可変、lane cloned worktree)、LaneStand = HD or TH。
    Wing,
}

impl fmt::Display for LaneKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaneKind::Lead => write!(f, "lead"),
            LaneKind::Wing => write!(f, "wing"),
        }
    }
}

// `LaneStand` enum は doc 11 (PR-B) で削除。 stand 識別子は `String` に統一、
// `mise tasks ls --json` で動的 discovery する設計に移行。
//   旧: enum { HeavensDoor, TheHand }
//   新: String (例: "hd" / "shell" / "tmux" / 任意の vp:stand:* task 名)
//
// wire format の legacy 名は `process::routes::lanes::migrate_legacy_stand` で
// 1 release の deprecation 期間 shim 経由で吸収 ("heavens_door" → "hd"、 "the_hand" → "shell")。

/// tmux session の起動 mode (Phase 1a: Lane → tmux registry の foundation)
///
/// `LaneInfo.tmux` の `mode` field で「実際に tmux で起動できたか」を区別する。
/// init_script は `tmux new-session -A -s ${SLUG} 'claude -c || claude' || (claude -c || claude)`
/// の形 (Option 1 inline cmd、idempotent)。tmux 不在環境などで外側 fallback に降格した
/// 場合は `PtySlotFallback` を立てる。
///
/// 関連 memory: vp_mailbox_monitor_agent_inbox + vp_lane_init_script (Phase 1)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TmuxMode {
    /// tmux session 起動 + claude 注入が成功 → send-keys で agent 間連携可能
    Tmux,
    /// tmux 不在 / 起動失敗 → 外側 `||` で素 claude にフォールバック
    PtySlotFallback,
}

/// Lane の tmux address (Phase 1a: deterministic derivation で agent が引ける)
///
/// session 名は `LaneAddress::tmux_session_name(stand)` で deterministic に決まる。
/// 例: lead@vantage-point (HD) → `"vp-vantage-point-lead-hd"`
///
/// `mode` で fallback 検出可能 (実 spawn 結果に基づき SP が populate)。
///
/// `stand` field は **どの Stand の tmux か** を表現 (1 Lane に複数 Stand を立てる
/// 将来想定: HD + TH 並立、 future custom Stand 等)。 Phase 1 MVP は通常 1 entry だが、
/// 型として複数対応可能。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmuxLaneAddress {
    /// Stand 名 (例: "hd" / "shell" / "tmux"、 doc 11 PR-B で String に変更)
    pub stand: String,
    pub session: String,
    pub mode: TmuxMode,
}

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

/// Lane の address — Pool key
///
/// 表示形 (`Display` 実装):
/// - Lead: `"<project>/lead"`         例: `"vp/lead"`
/// - Wing: `"<project>/wing/<name>"`  例: `"vp/wing/foo"`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LaneAddress {
    pub project: String,
    pub kind: LaneKind,
    /// Wing のみ Some (人間可読、例: "foo")。Lead は None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl LaneAddress {
    pub fn lead(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            kind: LaneKind::Lead,
            name: None,
        }
    }

    pub fn wing(project: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            kind: LaneKind::Wing,
            name: Some(name.into()),
        }
    }

    /// Phase 1a: tmux session 名を deterministic に導出する。
    ///
    /// 形式: `vp-{project}-{lane_label}-{stand_short}` を sanitize ([A-Za-z0-9_-] 以外は '-')。
    /// - `vp-` prefix: VP 管理 session の owner mark (user 自前 session と確実に分離)
    /// - project: lexicographic sort で project 単位にまとまる (`tmux ls` 視認性 ↑)
    /// - lane_label: Lead → "lead"、Wing(Some(name)) → name、Wing(None) → "unnamed"
    /// - stand_short: HD → "hd"、TH → "th" (suffix なので将来 `gemini` / `opus` 等の追加も自然)
    ///
    /// 例:
    /// - `LaneAddress::lead("vantage-point")` + HD → `"vp-vantage-point-lead-hd"`
    /// - `LaneAddress::wing("vantage-point", "sub")` + HD → `"vp-vantage-point-sub-hd"`
    /// - `LaneAddress` の `.` `@` 等 → `-` に置換
    ///
    /// agent (Claude CLI on HD) はこの関数の戻り値を `tmux send-keys -t <session>` の
    /// 宛先として使う。`/api/lanes` の cache 値とも一致する (deterministic)。
    /// tmux session 名の stand を除いた prefix。形式 `vp-{project}-{lane_label}-`。
    ///
    /// stand を指定せず session を prefix 一致で引きたい用途（`vp directmsg` 等）に使う。
    /// sanitize は per-char なので、`tmux_session_prefix()` は必ず `tmux_session_name()` の
    /// 戻り値の prefix に一致する。
    pub fn tmux_session_prefix(&self) -> String {
        let lane_label: &str = match (&self.kind, self.name.as_deref()) {
            (LaneKind::Lead, _) => "lead",
            (LaneKind::Wing, Some(n)) => n,
            (LaneKind::Wing, None) => "unnamed",
        };
        sanitize_tmux_name(&format!("vp-{}-{}-", self.project, lane_label))
    }

    pub fn tmux_session_name(&self, stand_name: &str) -> String {
        // stand_name は `vp:stand:{name}` の name 部分そのまま (例: "hd" / "shell" / "tmux")。
        // 旧 enum dispatch (HeavensDoor → "hd"、 TheHand → "th") は廃止。
        // 旧 TH の "th" は wire format 上では "shell" に rename された (doc 11)。
        // prefix + sanitize 済 stand の連結 (sanitize は per-char なので分配可能)。
        format!(
            "{}{}",
            self.tmux_session_prefix(),
            sanitize_tmux_name(stand_name)
        )
    }
}

/// tmux session 名に使えない文字 ([A-Za-z0-9_-] 以外) を '-' に置換する。
fn sanitize_tmux_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

impl fmt::Display for LaneAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.kind, &self.name) {
            (LaneKind::Lead, _) => write!(f, "{}/lead", self.project),
            (LaneKind::Wing, Some(n)) => write!(f, "{}/wing/{}", self.project, n),
            (LaneKind::Wing, None) => write!(f, "{}/wing/<unnamed>", self.project),
        }
    }
}

/// Phase 2 (Step E): エンティティ lifecycle の diff event を表現する generic ADT。
///
/// - `I` = identifier 型 (削除時のみ必要、 例: `LaneAddress`)
/// - `P` = payload 型 (add/update 時の full state、 例: `LaneInfo`)
///
/// SP の caller で event 発生 → AppState の broadcast channel に publish →
/// `spawn_registry_keepalive` の subscriber が QUIC registry channel で TheWorld に push、
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
/// `spawn_registry_keepalive` subscriber が QUIC registry channel 経由で TheWorld に流す。
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

impl TmuxLaneAddress {
    /// Phase 1e: spawn 成功時の tmux address を生成する helper。
    ///
    /// `crate::tmux::is_tmux_available()` で mode を事前判定 (Phase 1e 設計確定軸 A):
    /// - tmux PATH 内 → `Tmux` mode (副舞台あり、 agent が `tmux send-keys -t {session}` で
    ///   他 Lane の HD に入力リレー可能)
    /// - tmux なし → `PtySlotFallback` mode (shell 内 `||` で素 LLM CLI に降格、 send-keys 不可)
    ///
    /// session 名は `addr.tmux_session_name(stand)` で deterministic 導出。
    pub fn for_spawn(addr: &LaneAddress, stand_name: &str) -> Self {
        let mode = if crate::tmux::is_tmux_available() {
            TmuxMode::Tmux
        } else {
            TmuxMode::PtySlotFallback
        };
        Self {
            stand: stand_name.to_string(),
            session: addr.tmux_session_name(stand_name),
            mode,
        }
    }
}

/// Lane の info (REST response 用 + 内部 registry の値)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneInfo {
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
    /// Phase 5-D: Wing のみ embed (Lead は git workspace を持たない設計)。
    /// `cwd` から `lane::commands::wing_status()` を呼んで populate。
    /// `/api/lanes` 応答時に lazy 取得 (registry には保存しない、 git 状態は volatile)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wing_status: Option<crate::lane::commands::WingStatus>,
    /// Phase 1a: Lane に attach した Stand ごとの tmux session address (deterministic)。
    /// SP push 経由で TheWorld cache に流れる (agent から `/api/lanes` で resolve)。
    ///
    /// **複数 entry 想定**: 1 Lane に複数 Stand を並立させる将来 (HD + TH、 future custom Stand) に
    /// 対応するため `Vec`。 Phase 1 MVP は通常 0 or 1 entry。 順序は spawn 順を表現。
    /// `panes` の論理 ID 管理は tmux 側に委譲 (1 session 内 split は tmux native 機能)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tmux: Vec<TmuxLaneAddress>,
}

/// Lane Pool — Lead/Wing registry
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
    /// pty_slots と lifecycle 同期: with_lead で spawn、 remove で drop abort。
    /// task は spawn_blocking で 1 Lane = 1 task、 broadcast::Receiver を消費。
    /// MVP: Lead Lane のみ attach。 Wing spawn 経路 (insert_pty_slot) は別 PR で配線予定。
    term_attaches: HashMap<LaneAddress, crate::terminal::term_attach::TermAttach>,
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

    /// Project 起動時に Lead Lane を 1 つ pre-populate (Echoes default)
    ///
    /// **A5-2**: stand_spawner で command 構築 → PtySlot::spawn で実 process 起動。
    /// spawn 失敗時は graceful degrade (state=Dead、 pty_slots に entry なし) で
    /// SP 自体の起動継続性を担保。
    pub fn with_lead(project_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        let project_id = project_id.into();
        let cwd = cwd.into();
        let mut pool = Self::new();
        let addr = LaneAddress::lead(&project_id);
        // doc 11 PR-B: default stand は "echoes" 固定 (config.default_stand での per-user 化は
        // 後続 PR、 LanePool::with_lead は config を持たないため)。
        // user 設定がある場合の経路は HTTP API / lane_spawn_actor 経由で stand を明示指定する。
        // PR-pre2 (VP-118): "hd" → "echoes" rename。 mise task `vp:stand:echoes` (旧 hd)。
        let stand_name = "echoes";

        let cmd = crate::process::stand_spawner::build_stand_command(
            stand_name,
            &addr,
            std::path::Path::new(&cwd),
        );

        // Phase 5-D: spawn_with_fallback で `claude --continue` 早期 exit 時に空 args で retry。
        // PR-D: cwd は cmd.cwd (install root) に集約、 引数からは削除。
        let (state, pid) = match crate::process::stand_spawner::spawn_with_fallback(&cmd, 120, 48) {
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
        };

        let info = LaneInfo {
            address: addr.clone(),
            kind: LaneKind::Lead,
            name: None,
            state,
            stand: stand_name.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            pid,
            cwd,
            // Lead は git workspace 持たない (= project root が cwd)、 wing_status は None
            wing_status: None,
            // Phase 1e: spawn 成功時のみ tmux address を populate
            // (spawn 失敗 = Dead → 空 Vec で副舞台不在 signal)
            tmux: if matches!(state, LaneState::Running) {
                vec![TmuxLaneAddress::for_spawn(&addr, stand_name)]
            } else {
                Vec::new()
            },
        };
        pool.lanes.insert(addr, info);
        pool
    }

    /// Lane 一覧を **Lead 先頭、 続いて Wing を生成順 (created_at 昇順)** で返す。
    ///
    /// 内部 `lanes` は `HashMap` のため iter 順は non-deterministic (process ごとに異なる
    /// hash seed)。 sidebar の表示要件 「Root/Lead が一番上、 その下は生成時順」 を満たす
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
                (LaneKind::Lead, LaneKind::Wing) => Ordering::Less,
                (LaneKind::Wing, LaneKind::Lead) => Ordering::Greater,
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

    /// Phase 3-A: 既に spawn 済の PtySlot を Lane address 紐付けで insert (Wing create で使う)。
    ///
    /// Stage 1 (ADR-0001): TermAttach も同期 spawn する。 `term_rx` は spawn_with_fallback の
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
        let term_attach = crate::terminal::term_attach::TermAttach::spawn(term_rx, 80, 24);
        self.term_attaches.insert(addr, term_attach);
    }

    pub fn remove(&mut self, addr: &LaneAddress) -> Option<LaneInfo> {
        // Phase 4-A: PtySlot も一緒に drop (= child kill 経由でプロセス停止)
        // PtySlot::Drop が child.kill() + child.wait() を呼ぶので zombie 防止。
        // Stage 1 (ADR-0001): TermAttach も同期 drop (JoinHandle::abort で task 終了)。
        // 順序: term_attaches → pty_slots → lanes (broadcast::Sender は pty_slots が保持)。
        self.term_attaches.remove(addr);
        self.pty_slots.remove(addr);
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
            // PtySlot Drop で child.kill() + child.wait() = zombie 解消
            self.pty_slots.remove(&addr);
        }
        transitioned
    }

    /// Lane の Lead Stand (= PtySlot の child process) を kill + 再 spawn する。
    ///
    /// 同 Lane の cwd / stand を維持したまま child process だけ作り直す。
    /// (例: HD Lane なら shell を立て直し → `claude --continue || claude` を再 inject)
    ///
    /// vp-app の WS connection は PR #218 (auto-reconnect) で透過的に新 PtySlot に
    /// attach し直す ─ pool の write lock を保持してる間は WS の read が queue され、
    /// release 後に新しい broadcast channel + scrollback を subscribe する。
    ///
    /// spawn 失敗時は LaneInfo.state を Dead にして error を返す (caller の責任で UI 通知)。
    pub fn restart_lane(&mut self, addr: &LaneAddress) -> anyhow::Result<()> {
        let info = self
            .lanes
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane not found: {}", addr))?;
        let cwd = info.cwd.clone();
        let stand = info.stand.clone();
        // VP-131 fix: tmux session 名を事前に capture して step 1.5 で kill する。
        // PtySlot drop だけだと mise が起動した detached tmux session が tmux server
        // 配下で生存継続、 同名 session 残存のまま respawn すると `tmux new-session` 衝突で
        // mise task `vp:stand:<name>` が即 fail (= `[lost tty] / ERROR task failed`)。
        // VP-124 Phase 1 で `delete_lane_orchestrated` には同型 fix 済、 restart path も補完。
        let tmux_sessions: Vec<String> = info.tmux.iter().map(|t| t.session.clone()).collect();

        // step 1: 既存 PtySlot + TermAttach を drop (Drop で child.kill() + child.wait() = zombie 解消)。
        // Stage 1 (ADR-0001): 順序は LanePool::remove と一致 (term_attaches → pty_slots、
        // broadcast::Sender は pty_slots が保持なので task は次 iter で Closed 検知して exit)。
        // step 2 が Err でも term_attaches に死んだ entry が残らない (= lifecycle clean)。
        self.term_attaches.remove(addr);
        let _ = self.pty_slots.remove(addr);

        // step 1.5 (VP-131): tmux session を kill して name 衝突を避ける (best-effort、
        // 不存在 / 既 kill 済は false 戻り = no-op)。 PtySlot drop と respawn の間に挟むことで
        // `tmux new-session -d -s <name>` が衝突せず succeed する。
        for session in &tmux_sessions {
            let killed = crate::tmux::kill_session(session);
            if killed {
                tracing::info!("restart_lane: tmux session killed: {}", session);
            } else {
                tracing::debug!(
                    "restart_lane: tmux session kill returned false (already gone?): {}",
                    session
                );
            }
        }

        // step 2: 同 stand で respawn (Phase 5-D: spawn_with_fallback で early-exit retry)
        // doc 11 PR-B: stand は String 化 (旧 enum 廃止)
        let cmd = crate::process::stand_spawner::build_stand_command(
            &stand,
            addr,
            std::path::Path::new(&cwd),
        );
        match crate::process::stand_spawner::spawn_with_fallback(&cmd, 120, 48) {
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

    /// Display 形 (`"<project>/lead"` / `"<project>/wing/<name>"`) をパースして LaneAddress を作る。
    /// vp-app の sidebar から `lane:select` IPC の address (= `lane_address_key`) を逆変換するために使う。
    pub fn parse_address(s: &str) -> Option<LaneAddress> {
        let parts: Vec<&str> = s.splitn(3, '/').collect();
        match parts.as_slice() {
            [project, "lead"] if !project.is_empty() => Some(LaneAddress::lead(*project)),
            [project, "wing", name] if !project.is_empty() && !name.is_empty() => {
                Some(LaneAddress::wing(*project, *name))
            }
            _ => None,
        }
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

    /// Phase 2.x-c: scrollback 付きで attach する。
    /// 戻り値: `(rx, initial_bytes)` ── initial_bytes を WS Binary で先送してから rx で継続。
    pub fn subscribe_with_scrollback(
        &self,
        addr: &LaneAddress,
    ) -> Option<(tokio::sync::broadcast::Receiver<Vec<u8>>, Vec<u8>)> {
        let slot_mutex = self.pty_slots.get(addr)?;
        let slot = slot_mutex.lock().ok()?;
        Some(slot.subscribe_with_scrollback())
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
        // attach 不在 (= spawn 失敗 / 未配線 Wing 経路) は静かに skip
        if let Some(term_attach) = self.term_attaches.get(addr) {
            term_attach.resize(cols, rows);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_address_display_lead_and_wing() {
        assert_eq!(LaneAddress::lead("vp").to_string(), "vp/lead");
        assert_eq!(LaneAddress::wing("vp", "foo").to_string(), "vp/wing/foo");
    }

    #[tokio::test]
    async fn lane_pool_with_lead_pre_populates_one_lane() {
        // Phase 1: LanePool::with_lead は内部で PtySlot::spawn → tokio::task::spawn_blocking する。
        // 純 sync test だと runtime が無くて panic するので #[tokio::test] にする。
        let pool = LanePool::with_lead("vp", "/tmp");
        assert_eq!(pool.count(), 1);
        let lanes = pool.list();
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].kind, LaneKind::Lead);
        assert_eq!(lanes[0].stand, "echoes"); // default は "echoes" (PR-pre2 で "hd" → "echoes" rename)
    }

    #[test]
    fn lane_kind_serde_snake_case() {
        assert_eq!(serde_json::to_string(&LaneKind::Lead).unwrap(), "\"lead\"");
        assert_eq!(serde_json::to_string(&LaneKind::Wing).unwrap(), "\"wing\"");
    }

    #[test]
    fn parse_address_lead_and_wing() {
        let lead = LanePool::parse_address("vp/lead").unwrap();
        assert_eq!(lead, LaneAddress::lead("vp"));

        let wing = LanePool::parse_address("vp/wing/foo").unwrap();
        assert_eq!(wing, LaneAddress::wing("vp", "foo"));

        // CJK / kebab-case project name も通る
        let lead2 = LanePool::parse_address("vantage-point/lead").unwrap();
        assert_eq!(lead2, LaneAddress::lead("vantage-point"));

        // 不正
        assert!(LanePool::parse_address("vp").is_none()); // / 無し
        assert!(LanePool::parse_address("/lead").is_none()); // project 空
        assert!(LanePool::parse_address("vp/foo").is_none()); // 未知 kind
        assert!(LanePool::parse_address("vp/wing/").is_none()); // wing name 空
        // 旧 "worker" token は受理しない
        assert!(LanePool::parse_address("vp/worker/foo").is_none());
    }

    // ========================================================================
    // Phase 1a — Lane → tmux session address resolution
    // ========================================================================

    #[test]
    fn tmux_session_name_lead_hd() {
        // Lead Lane + Heaven's Door → "vp-{project}-lead-hd"
        let addr = LaneAddress::lead("vantage-point");
        assert_eq!(addr.tmux_session_name("hd"), "vp-vantage-point-lead-hd");
    }

    #[test]
    fn tmux_session_name_wing_hd() {
        // Wing(name) + HD → "vp-{project}-{name}-hd"
        let addr = LaneAddress::wing("vantage-point", "sub");
        assert_eq!(addr.tmux_session_name("hd"), "vp-vantage-point-sub-hd");
    }

    #[test]
    fn tmux_session_name_lead_shell() {
        // doc 11 PR-B: 旧 TH stand → "shell" stand 名に rename (mise task `vp:stand:shell`)
        // tmux session 名も "vp-{project}-lead-shell" に変わる (旧: "-th")。
        let addr = LaneAddress::lead("vp");
        assert_eq!(addr.tmux_session_name("shell"), "vp-vp-lead-shell");
    }

    #[test]
    fn tmux_session_name_sanitize_special_chars() {
        // project 名に '.' 等の tmux session 名で escape 必要な文字 → '-' に置換
        let addr = LaneAddress {
            project: "creo.memories".to_string(),
            kind: LaneKind::Lead,
            name: None,
        };
        assert_eq!(addr.tmux_session_name("hd"), "vp-creo-memories-lead-hd");
    }

    #[test]
    fn tmux_session_name_unnamed_wing_fallback() {
        // Wing(name=None) は仕様上想定外だが defensive に "unnamed" にフォールバック
        let addr = LaneAddress {
            project: "vp".to_string(),
            kind: LaneKind::Wing,
            name: None,
        };
        assert_eq!(addr.tmux_session_name("hd"), "vp-vp-unnamed-hd");
    }

    #[test]
    fn tmux_lane_address_serde_snake_case_mode_and_stand() {
        // doc 11 PR-B: stand は String 化、 wire format は task 名そのまま ("hd" / "shell" 等)。
        let t = TmuxLaneAddress {
            stand: "hd".to_string(),
            session: "vp-vp-lead-hd".to_string(),
            mode: TmuxMode::Tmux,
        };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"mode\":\"tmux\""), "got: {}", json);
        assert!(json.contains("\"stand\":\"hd\""), "got: {}", json);

        let fb = TmuxLaneAddress {
            stand: "shell".to_string(),
            session: "vp-vp-lead-shell".to_string(),
            mode: TmuxMode::PtySlotFallback,
        };
        let json2 = serde_json::to_string(&fb).unwrap();
        assert!(
            json2.contains("\"mode\":\"pty_slot_fallback\""),
            "got: {}",
            json2
        );
        assert!(json2.contains("\"stand\":\"shell\""), "got: {}", json2);
    }

    #[test]
    fn lane_info_tmux_field_empty_vec_serde_omitted() {
        // tmux が空 Vec なら skip_serializing_if で wire 形式から省略 → 古 client と互換
        let info = LaneInfo {
            address: LaneAddress::lead("vp"),
            kind: LaneKind::Lead,
            name: None,
            state: LaneState::Running,
            stand: "hd".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            pid: None,
            cwd: "/tmp".to_string(),
            wing_status: None,
            tmux: Vec::new(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(
            !json.contains("\"tmux\""),
            "tmux should be omitted when empty: {}",
            json
        );
    }

    #[test]
    fn lane_info_tmux_field_multi_stand_serde_round_trip() {
        // Phase 1a multi-stand 設計: 1 Lane が 複数 Stand 並立 (HD + TH) で
        // 複数 tmux session を持つケースを serde round-trip で検証
        let original = LaneInfo {
            address: LaneAddress::lead("vp"),
            kind: LaneKind::Lead,
            name: None,
            state: LaneState::Running,
            stand: "hd".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            pid: None,
            cwd: "/tmp".to_string(),
            wing_status: None,
            tmux: vec![
                TmuxLaneAddress {
                    stand: "hd".to_string(),
                    session: "vp-vp-lead-hd".to_string(),
                    mode: TmuxMode::Tmux,
                },
                TmuxLaneAddress {
                    stand: "shell".to_string(),
                    session: "vp-vp-lead-shell".to_string(),
                    mode: TmuxMode::Tmux,
                },
            ],
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: LaneInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.tmux.len(), 2);
        assert_eq!(restored.tmux[0].stand, "hd");
        assert_eq!(restored.tmux[1].stand, "shell");
        assert_eq!(restored.tmux[0].session, "vp-vp-lead-hd");
        assert_eq!(restored.tmux[1].session, "vp-vp-lead-shell");
    }

    // ========================================================================
    // Phase 2 (Step E) — Lane lifecycle diff push (SystemEvent + Diff<I, P>)
    // ========================================================================

    #[test]
    fn lane_diff_add_serde_round_trip() {
        // Diff::Add { payload: LaneInfo } の wire 形式 + decode
        let info = LaneInfo {
            address: LaneAddress::wing("vp", "sub"),
            kind: LaneKind::Wing,
            name: Some("sub".to_string()),
            state: LaneState::Running,
            stand: "hd".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            pid: Some(12345),
            cwd: "/tmp".to_string(),
            wing_status: None,
            tmux: vec![TmuxLaneAddress {
                stand: "hd".to_string(),
                session: "vp-vp-sub-hd".to_string(),
                mode: TmuxMode::Tmux,
            }],
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
                assert_eq!(payload.tmux.len(), 1);
            }
            _ => panic!("expected Diff::Add"),
        }
    }

    #[test]
    fn lane_diff_remove_serde_round_trip() {
        // Diff::Remove { id: LaneAddress } で id のみ送る wire 形式
        let addr = LaneAddress::wing("vp", "osc");
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
            address: LaneAddress::lead("vp"),
            kind: LaneKind::Lead,
            name: None,
            state: LaneState::Running,
            stand: "hd".to_string(),
            created_at: "2026-05-01T00:00:00Z".to_string(),
            pid: None,
            cwd: "/tmp".to_string(),
            wing_status: None,
            tmux: Vec::new(),
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
