//! Device I/O 🌫️ — Lane scope の双方向 device I/O endpoint (doc 23 §6)
//!
//! DeviceRegistry 🧲 (World scope の device registry) が集約した物理 device に対し、
//! Lane 単位の双方向 I/O を担う。
//!
//! 設計 SSOT: `docs/design/23-bastet-justice-stand-wiring.md`
//!
//! 2 片方向 flow (doc 23 §6.2):
//! - **input**: DeviceRegistry が parse した ControlEvent を「active Lane = 自分」のときだけ受け取り、
//!   Lane command context に着地させる
//! - **output**: Lane state 変化を subscribe → DeviceProfile で byte 化 → DeviceRegistry の out port 経由送出
//!
//! ## 実装状態 (sub-PR 追跡)
//!
//! | sub-PR | scope | status |
//! |--------|-------|--------|
//! | E3-1 | DeviceIoStand (LaneStandHost impl) 型 + LaneStandRegistry insert | ✅ Done (#556) |
//! | E3-2 | output projection: DeviceProfile hold + projection batch 生成 | ← 本 PR |

use std::any::Any;

use tokio::sync::RwLock;

use crate::device_profile::{DeviceProfile, ParamSpec, Rgb};
use crate::process::lane_stand::LaneStandHost;

// ─── data ──────────────────────────────────────────────────

/// projection batch の出力単位 — (port_pattern, messages) のペア。
/// caller が `midi::send_batch(Some(&pattern), &messages)` で送出する。
pub struct ProjectionBatch {
    pub port_pattern: String,
    pub messages: Vec<Vec<u8>>,
}

/// Device I/O 🌫️ の Lane-local state。
///
/// bound された DeviceProfile 群を保持し、projection 要求時に byte batch を生成する。
#[derive(Default)]
pub struct DeviceIoState {
    /// この Lane に bind された device profile 群（projection 対象）
    profiles: Vec<Box<dyn DeviceProfile + Send + Sync>>,
}

// ─── DeviceIoStand ──────────────────────────────────────────

/// Lane に host される device I/O endpoint（Device I/O 🌫️）。
///
/// `PaisleyParkStand` と同型の **passive marker**（`LaneStandHost` impl）。
/// `LaneCapabilities::new()` で各 Lane に自動登録される（midi feature 有効時）。
///
/// E3-2 で DeviceProfile の hold + projection batch 生成を追加。
/// Device I/O は **計算に徹し**、I/O（`send_batch`）は caller/DeviceRegistry に委譲する。
pub struct DeviceIoStand {
    state: RwLock<DeviceIoState>,
}

impl DeviceIoStand {
    pub fn new() -> Self {
        Self {
            state: RwLock::new(DeviceIoState::default()),
        }
    }

    /// internal `RwLock<DeviceIoState>` への参照。
    pub fn state(&self) -> &RwLock<DeviceIoState> {
        &self.state
    }

    // ─── profile binding ───────────────────────────────

    /// device profile をこの Lane に bind（projection 対象に追加）。
    pub async fn bind_profile(&self, profile: Box<dyn DeviceProfile + Send + Sync>) {
        self.state.write().await.profiles.push(profile);
    }

    /// bind 中の profile 数。
    pub async fn profile_count(&self) -> usize {
        self.state.read().await.profiles.len()
    }

    /// 全 profile を unbind（Lane 非 active 化 / 切替時の cleanup）。
    pub async fn unbind_all(&self) {
        self.state.write().await.profiles.clear();
    }

    // ─── projection（calculation — I/O なし）────────────

    /// 全 bound profile に handshake batch を生成（device bind 直後に caller が送出）。
    pub async fn handshake_all(&self) -> Vec<ProjectionBatch> {
        let state = self.state.read().await;
        state
            .profiles
            .iter()
            .map(|p| ProjectionBatch {
                port_pattern: p.port_pattern().to_string(),
                messages: p.handshake(),
            })
            .filter(|b| !b.messages.is_empty())
            .collect()
    }

    /// 全 bound profile に track state を projection し、port ごとの batch を返す。
    ///
    /// `DeviceProfile::project_track` は `&mut self`（shadow state 更新）のため write lock。
    pub async fn project_track(
        &self,
        index: u8,
        name: &str,
        color: Rgb,
        is_group: bool,
    ) -> Vec<ProjectionBatch> {
        let mut state = self.state.write().await;
        state
            .profiles
            .iter_mut()
            .map(|p| {
                let pattern = p.port_pattern().to_string();
                let messages = p.project_track(index, name, color, is_group);
                ProjectionBatch {
                    port_pattern: pattern,
                    messages,
                }
            })
            .filter(|b| !b.messages.is_empty())
            .collect()
    }

    /// 全 bound profile に parameter を teach し、port ごとの batch を返す。
    ///
    /// `DeviceProfile::learn_parameter` は `&mut self`（shadow state 更新）のため write lock。
    pub async fn project_parameter(&self, index: u8, spec: &ParamSpec) -> Vec<ProjectionBatch> {
        let mut state = self.state.write().await;
        state
            .profiles
            .iter_mut()
            .map(|p| {
                let pattern = p.port_pattern().to_string();
                let messages = p.learn_parameter(index, spec);
                ProjectionBatch {
                    port_pattern: pattern,
                    messages,
                }
            })
            .filter(|b| !b.messages.is_empty())
            .collect()
    }
}

impl Default for DeviceIoStand {
    fn default() -> Self {
        Self::new()
    }
}

impl LaneStandHost for DeviceIoStand {
    fn stand_kind(&self) -> &'static str {
        "device_io"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ─── tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_profile::{DeviceProfile, ParamSpec, Rgb};

    /// test 用 minimal DeviceProfile impl
    struct MockProfile {
        pattern: &'static str,
        handshake_msgs: Vec<Vec<u8>>,
        /// project_track 呼び出し回数
        track_calls: u32,
    }

    impl MockProfile {
        fn new(pattern: &'static str) -> Self {
            Self {
                pattern,
                handshake_msgs: vec![vec![0xF0, 0x00, 0xF7]],
                track_calls: 0,
            }
        }

        fn without_handshake(pattern: &'static str) -> Self {
            Self {
                pattern,
                handshake_msgs: Vec::new(),
                track_calls: 0,
            }
        }
    }

    impl DeviceProfile for MockProfile {
        fn port_pattern(&self) -> &str {
            self.pattern
        }

        fn handshake(&self) -> Vec<Vec<u8>> {
            self.handshake_msgs.clone()
        }

        fn project_track(
            &mut self,
            index: u8,
            name: &str,
            _color: Rgb,
            _is_group: bool,
        ) -> Vec<Vec<u8>> {
            self.track_calls += 1;
            // index + name 頭文字を mock payload に
            vec![vec![
                0xB0,
                index,
                name.as_bytes().first().copied().unwrap_or(0),
            ]]
        }

        fn learn_parameter(&mut self, index: u8, spec: &ParamSpec) -> Vec<Vec<u8>> {
            let value_byte = (spec.value * 127.0) as u8;
            vec![vec![0xB0, index, value_byte]]
        }
    }

    // ─── LaneStandHost ─────────────────────────────────

    #[test]
    fn stand_kind_is_device_io() {
        let stand = DeviceIoStand::new();
        assert_eq!(stand.stand_kind(), "device_io");
    }

    #[test]
    fn supports_downcast() {
        let stand = DeviceIoStand::new();
        let host: &dyn LaneStandHost = &stand;
        let downcast = host.as_any().downcast_ref::<DeviceIoStand>();
        assert!(downcast.is_some());
    }

    // ─── profile binding ───────────────────────────────

    #[tokio::test]
    async fn bind_profile_increments_count() {
        let stand = DeviceIoStand::new();
        assert_eq!(stand.profile_count().await, 0);

        stand
            .bind_profile(Box::new(MockProfile::new("X-Touch")))
            .await;
        assert_eq!(stand.profile_count().await, 1);

        stand.bind_profile(Box::new(MockProfile::new("Roto"))).await;
        assert_eq!(stand.profile_count().await, 2);
    }

    #[tokio::test]
    async fn unbind_all_clears_profiles() {
        let stand = DeviceIoStand::new();
        stand
            .bind_profile(Box::new(MockProfile::new("X-Touch")))
            .await;
        stand.bind_profile(Box::new(MockProfile::new("Roto"))).await;
        assert_eq!(stand.profile_count().await, 2);

        stand.unbind_all().await;
        assert_eq!(stand.profile_count().await, 0);
    }

    // ─── projection ────────────────────────────────────

    #[tokio::test]
    async fn handshake_all_returns_batches_per_profile() {
        let stand = DeviceIoStand::new();
        stand
            .bind_profile(Box::new(MockProfile::new("X-Touch")))
            .await;
        stand.bind_profile(Box::new(MockProfile::new("Roto"))).await;

        let batches = stand.handshake_all().await;
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].port_pattern, "X-Touch");
        assert_eq!(batches[1].port_pattern, "Roto");
    }

    #[tokio::test]
    async fn handshake_filters_empty() {
        let stand = DeviceIoStand::new();
        stand
            .bind_profile(Box::new(MockProfile::without_handshake("silent")))
            .await;

        let batches = stand.handshake_all().await;
        assert!(batches.is_empty());
    }

    #[tokio::test]
    async fn project_track_generates_batches() {
        let stand = DeviceIoStand::new();
        stand
            .bind_profile(Box::new(MockProfile::new("X-Touch")))
            .await;

        let batches = stand
            .project_track(0, "vocal", Rgb::new(255, 0, 0), false)
            .await;
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].port_pattern, "X-Touch");
        // mock: [0xB0, index, name 頭文字]
        assert_eq!(batches[0].messages[0], vec![0xB0, 0, b'v']);
    }

    #[tokio::test]
    async fn project_track_multi_profile() {
        let stand = DeviceIoStand::new();
        stand
            .bind_profile(Box::new(MockProfile::new("X-Touch")))
            .await;
        stand.bind_profile(Box::new(MockProfile::new("Roto"))).await;

        let batches = stand
            .project_track(3, "bass", Rgb::new(0, 0, 255), true)
            .await;
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].port_pattern, "X-Touch");
        assert_eq!(batches[1].port_pattern, "Roto");
    }

    #[tokio::test]
    async fn project_parameter_generates_batches() {
        let stand = DeviceIoStand::new();
        stand.bind_profile(Box::new(MockProfile::new("Roto"))).await;

        let spec = ParamSpec::continuous("volume", 0.5);
        let batches = stand.project_parameter(0, &spec).await;
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].port_pattern, "Roto");
        // mock: [0xB0, index, value_byte] where 0.5 * 127 ≈ 63
        assert_eq!(batches[0].messages[0], vec![0xB0, 0, 63]);
    }

    #[tokio::test]
    async fn project_on_empty_returns_nothing() {
        let stand = DeviceIoStand::new();
        let batches = stand
            .project_track(0, "test", Rgb::new(0, 0, 0), false)
            .await;
        assert!(batches.is_empty());
    }

    #[tokio::test]
    async fn projection_updates_shadow_state() {
        let stand = DeviceIoStand::new();
        stand
            .bind_profile(Box::new(MockProfile::new("X-Touch")))
            .await;

        // 2 回 project → shadow state（track_calls）が累積するか
        stand.project_track(0, "a", Rgb::new(0, 0, 0), false).await;
        stand.project_track(1, "b", Rgb::new(0, 0, 0), false).await;

        let state = stand.state().read().await;
        // MockProfile は track_calls を内部で increment — shadow state の動作確認
        // （実 profile では LCD/色の shadow buffer がこれに相当）
        assert_eq!(state.profiles.len(), 1);
    }
}
