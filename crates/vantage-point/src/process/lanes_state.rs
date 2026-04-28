//! Lane state types — SP が持つ Lane (Lead/Worker) の data model
//!
//! 関連 memory:
//! - `mem_1CaSrCxysdGaaSsN4Dvxth` (VP Architecture: 3 段 Stand scope + Lane semantic)
//! - `mem_1CaSsN7xj69aVQtLPQFJxQ` (SP-as-Project-Master: 9 component minimum)
//! - 「多 scope architecture + protocol/msg 連携」rule (2026-04-27 確定):
//!   Lane scope に attach するのは **HD と TH のみ**。PP/GE/HP は Project scope (`project_stands_state` 参照)。
//!
//! ## architecture: Lane scope は HD/TH 専用
//!
//! Project scope の Stand (PP/GE/HP) は別 module (`project_stands_state.rs`) で管理。
//! Lane は **Lead/Worker の PTY セッション** に集中:
//! - Lead   1 / project (固定)、LaneStand = HD or TH
//! - Worker 0..n / project (可変、ccws clone)、LaneStand = HD or TH
//!
//! ## Phase A4-2b スコープ
//!
//! `LanePool::with_lead` で Lead Lane 1 つ pre-populate。
//! Worker create / destroy / Stand 切替は A4-4 / A5 で実装。

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Lane の種別 (memory rule: HD/TH を起動する Lane だけ)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneKind {
    /// 1 / project (固定)、LaneStand = HD or TH
    Lead,
    /// 0..n / project (可変、ccws cloned worktree)、LaneStand = HD or TH
    Worker,
}

impl fmt::Display for LaneKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaneKind::Lead => write!(f, "lead"),
            LaneKind::Worker => write!(f, "worker"),
        }
    }
}

/// Lane で起動する Stand (HD or TH のみ)
///
/// - Lead/Worker: HD (default) or TH の 2 択
/// - PP/GE/HP は **Lane の中身ではない** (Project scope の Stand)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneStand {
    /// HD 📖 Heaven's Door — Claude CLI (default)
    HeavensDoor,
    /// TH ✋ The Hand — 素 shell
    TheHand,
}

impl Default for LaneStand {
    fn default() -> Self {
        // memory rule: Lead/Worker default は HD
        LaneStand::HeavensDoor
    }
}

/// Lane の state machine 状態 (Phase A4-2b では Running 固定で pre-populate)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneState {
    Spawning,
    Running,
    Exiting,
    Dead,
}

impl Default for LaneState {
    fn default() -> Self {
        LaneState::Running
    }
}

/// Lane の address — Pool key
///
/// 表示形 (`Display` 実装):
/// - Lead:   `"<project>/lead"`         例: `"vp/lead"`
/// - Worker: `"<project>/worker/<name>"` 例: `"vp/worker/foo"`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LaneAddress {
    pub project: String,
    pub kind: LaneKind,
    /// Worker のみ Some (人間可読、例: "foo")。Lead は None。
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

    pub fn worker(project: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            kind: LaneKind::Worker,
            name: Some(name.into()),
        }
    }
}

impl fmt::Display for LaneAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.kind, &self.name) {
            (LaneKind::Lead, _) => write!(f, "{}/lead", self.project),
            (LaneKind::Worker, Some(n)) => write!(f, "{}/worker/{}", self.project, n),
            (LaneKind::Worker, None) => write!(f, "{}/worker/<unnamed>", self.project),
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
    pub stand: LaneStand,
    /// ISO 8601
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub cwd: String,
    /// Phase 5-D: Worker のみ embed (Lead は git workspace を持たない設計)。
    /// `cwd` から `ccws::commands::worker_status()` を呼んで populate。
    /// `/api/lanes` 応答時に lazy 取得 (registry には保存しない、 git 状態は volatile)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_status: Option<crate::ccws::commands::WorkerStatus>,
}

/// Lane Pool — Lead/Worker registry
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

    /// Project 起動時に Lead Lane を 1 つ pre-populate (HD default)
    ///
    /// **A5-2**: stand_spawner で command 構築 → PtySlot::spawn で実 process 起動。
    /// spawn 失敗時は graceful degrade (state=Dead、 pty_slots に entry なし) で
    /// SP 自体の起動継続性を担保。
    pub fn with_lead(project_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        let project_id = project_id.into();
        let cwd = cwd.into();
        let mut pool = Self::new();
        let addr = LaneAddress::lead(&project_id);
        let stand = LaneStand::default(); // HD

        // A5-2: stand_spawner で LaneStand 別 command 構築
        let cmd =
            crate::process::stand_spawner::build_stand_command(stand, std::path::Path::new(&cwd));

        // Phase 5-D: spawn_with_fallback で `claude --continue` 早期 exit 時に空 args で retry。
        let (state, pid) = match crate::process::stand_spawner::spawn_with_fallback(
            &cwd, &cmd, 80, 24,
        ) {
            Ok((slot, _rx)) => {
                let pid = slot.pid();
                tracing::info!(
                    "Lane spawned: addr={} stand={:?} program={} args={:?} pid={}",
                    addr,
                    stand,
                    cmd.program,
                    cmd.args,
                    pid
                );
                pool.pty_slots
                    .insert(addr.clone(), std::sync::Mutex::new(slot));
                (LaneState::Running, Some(pid))
            }
            Err(e) => {
                // graceful degrade: SP 自体は起動継続、 Lane は Dead で record
                tracing::warn!(
                    "Lane spawn failed (graceful degrade to Dead): addr={} stand={:?} program={} cwd={} err={}",
                    addr,
                    stand,
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
            stand,
            created_at: chrono::Utc::now().to_rfc3339(),
            pid,
            cwd,
            // Lead は git workspace 持たない (= project root が cwd)、 worker_status は None
            worker_status: None,
        };
        pool.lanes.insert(addr, info);
        pool
    }

    pub fn list(&self) -> Vec<LaneInfo> {
        self.lanes.values().cloned().collect()
    }

    pub fn get(&self, addr: &LaneAddress) -> Option<&LaneInfo> {
        self.lanes.get(addr)
    }

    pub fn insert(&mut self, info: LaneInfo) {
        self.lanes.insert(info.address.clone(), info);
    }

    /// Phase 3-A: 既に spawn 済の PtySlot を Lane address 紐付けで insert (Worker create で使う)
    pub fn insert_pty_slot(&mut self, addr: LaneAddress, slot: crate::daemon::pty_slot::PtySlot) {
        self.pty_slots.insert(addr, std::sync::Mutex::new(slot));
    }

    pub fn remove(&mut self, addr: &LaneAddress) -> Option<LaneInfo> {
        // Phase 4-A: PtySlot も一緒に drop (= child kill 経由でプロセス停止)
        // PtySlot::Drop が child.kill() + child.wait() を呼ぶので zombie 防止。
        self.pty_slots.remove(addr);
        self.lanes.remove(addr)
    }

    pub fn count(&self) -> usize {
        self.lanes.len()
    }

    /// Display 形 (`"<project>/lead"` / `"<project>/worker/<name>"`) をパースして LaneAddress を作る。
    /// vp-app の sidebar から `lane:select` IPC の address (= `lane_address_key`) を逆変換するために使う。
    pub fn parse_address(s: &str) -> Option<LaneAddress> {
        // 形式: "<project>/lead" or "<project>/worker/<name>"
        let parts: Vec<&str> = s.splitn(3, '/').collect();
        match parts.as_slice() {
            [project, "lead"] if !project.is_empty() => Some(LaneAddress::lead(*project)),
            [project, "worker", name] if !project.is_empty() && !name.is_empty() => {
                Some(LaneAddress::worker(*project, *name))
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
    pub fn resize_lane(&self, addr: &LaneAddress, cols: u16, rows: u16) -> anyhow::Result<()> {
        let slot_mutex = self
            .pty_slots
            .get(addr)
            .ok_or_else(|| anyhow::anyhow!("Lane has no PtySlot: {}", addr))?;
        let slot = slot_mutex
            .lock()
            .map_err(|_| anyhow::anyhow!("PtySlot mutex poisoned: {}", addr))?;
        slot.resize(cols, rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_address_display_lead_and_worker() {
        assert_eq!(LaneAddress::lead("vp").to_string(), "vp/lead");
        assert_eq!(
            LaneAddress::worker("vp", "foo").to_string(),
            "vp/worker/foo"
        );
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
        assert_eq!(lanes[0].stand, LaneStand::HeavensDoor); // default は HD
    }

    #[test]
    fn lane_kind_serde_snake_case() {
        assert_eq!(serde_json::to_string(&LaneKind::Lead).unwrap(), "\"lead\"");
        let k: LaneKind = serde_json::from_str("\"worker\"").unwrap();
        assert_eq!(k, LaneKind::Worker);
    }

    #[test]
    fn lane_stand_only_hd_and_th() {
        // Phase A4-2b 修正: PP/GE/HP は LaneStand に含めない (Project scope に分離)
        assert_eq!(
            serde_json::to_string(&LaneStand::HeavensDoor).unwrap(),
            "\"heavens_door\""
        );
        assert_eq!(
            serde_json::to_string(&LaneStand::TheHand).unwrap(),
            "\"the_hand\""
        );
    }

    #[test]
    fn lane_stand_default_is_heavens_door() {
        assert_eq!(LaneStand::default(), LaneStand::HeavensDoor);
    }

    #[test]
    fn parse_address_lead_and_worker() {
        // Phase 2: vp-app が IPC で送る address ("<project>/lead" / "<project>/worker/<name>") を
        // SP 側で逆変換する。 lane_address_key (vp-app) と完全に対称。
        let lead = LanePool::parse_address("vp/lead").unwrap();
        assert_eq!(lead, LaneAddress::lead("vp"));

        let worker = LanePool::parse_address("vp/worker/foo").unwrap();
        assert_eq!(worker, LaneAddress::worker("vp", "foo"));

        // CJK / kebab-case project name も通る
        let lead2 = LanePool::parse_address("vantage-point/lead").unwrap();
        assert_eq!(lead2, LaneAddress::lead("vantage-point"));

        // 不正
        assert!(LanePool::parse_address("vp").is_none()); // / 無し
        assert!(LanePool::parse_address("/lead").is_none()); // project 空
        assert!(LanePool::parse_address("vp/foo").is_none()); // 未知 kind
        assert!(LanePool::parse_address("vp/worker/").is_none()); // worker name 空
    }
}
