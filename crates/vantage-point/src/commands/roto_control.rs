//! roto_control — ROTO-CONTROL の双方向 control loop（CLI / daemon 共有）
//!
//! `vp midi roto control`（前景・secs 制限）と Bastet 常駐（持続・自動再接続）が
//! **同一の control loop body を共有**するための module。doc 23 の Bastet（device 接続所有 @ World）
//! に control loop を載せる際、loop を二重実装しないための注入境界を定義する。
//!
//! ## 構造
//! - `LaneSource` / `SwitchSink`: loop が外界に触れる 2 点を trait で注入
//!   - CLI: `QuicLaneSource`(world-process QUIC) + `QuicSwitchSink`(per-SP QUIC)
//!   - daemon: `InProcessLaneSource`(build_world_lanes 直読み) + `QuicSwitchSink`(SP 越境は QUIC)
//! - `roto_control_loop`: keepalive(autorespond) + LCD projection + nav→switch を回す共有 body
//! - `RotoSessionBracket`(nostos `AsyncBracket`) + `RotoHealDriver`(`AsyncDriver`):
//!   enter=open+handshake / exit=control loop / disconnect→Reborn(自動再接続) を表現
//!
//! data / calculations / actions 分離: lane 並びの計算は `build_world_lanes`(daemon/server.rs) と
//! `parse_world_lanes`(midi.rs) の純粋関数に委譲。本 module は配線（orchestration）に集中する。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{Mutex, RwLock, mpsc::UnboundedReceiver};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use nostos::{AsyncBracket, AsyncDriver, Outcome};

use crate::capability::{ProcessManagerCapability, RunningProcess};
use crate::device_input::{ControlEvent, DeviceInput, roto::RotoInput};
use crate::device_profile::{DeviceProfile, Rgb, roto::RotoProfile};
use crate::process::lanes_state::LaneInfo;

use super::midi::{
    LaneNav, RotoLane, page_slots, parse_world_lanes, roto_autorespond, roto_lane_nav,
    roto_open_async, roto_project_slots, roto_project_slots_full,
};

// ─── data ──────────────────────────────────────────────────

/// control loop の UI state。reconnect 時に `RotoDescriptor` に退避され、再接続後も
/// 選択ページ・選択 lane が保たれる（抜き差し後の体験連続）。
///
/// ⚠️ `activated` / `lcd_projected` も **意図的に reconnect 跨ぎで carry-over する**
/// （false へ reset しない）。一見すると物理セッション local な flag に見えるが、reset は
/// 退行になる: reconnect 直後は loop 入場時に `poll.tick()` の初回が即時発火し、`lanes` は
/// 空 Vec 始まりなので非空 poll 結果と `next != lanes` が必ず真 → `if view.activated` gate を
/// 通って `roto_project_slots_full` で即再描画される（`activated=true` が必要）。逆に reset
/// すると fresh connect 同様ユーザーが物理操作するまで LCD が blank になる。`activated` block
/// （初回 SysEx での activation）は keepalive SysEx では発火しない（`roto_autorespond` が
/// HELLO/QUERY を先取り continue するため）ので、reconnect の即描画は専ら poll 経路が担う。
#[derive(Default, Clone)]
pub(crate) struct RotoView {
    /// 表示ページ（1 ページ = 8 lane）。reconnect で保持。
    pub page: usize,
    /// 選択中 lane key `"{port}:{token}"`。reconnect で保持。
    pub selected: Option<String>,
    /// projection 可能か。reconnect で carry-over（poll 即描画のため、上記参照）。
    pub activated: bool,
    /// LCD projection 済みか。reconnect で carry-over（上記参照）。
    pub lcd_projected: bool,
}

/// `roto_control_loop` の離脱理由。
pub(crate) enum LoopExit {
    /// midi sender drop / out 送信失敗 = ROTO 物理切断（→ daemon は再接続）
    Disconnected,
    /// shutdown token 発火 = graceful 停止（→ daemon は Done）
    Shutdown,
    /// CLI の secs deadline 到達
    Deadline,
    /// lane source の致命エラー等（→ daemon は Failed）
    Fatal(String),
}

/// 再接続のための device descriptor。`view` を抱えて reconnect 後の UI を復元する。
#[derive(Clone)]
pub(crate) struct RotoDescriptor {
    /// CoreMIDI port displayName の部分一致パターン（例 "Roto"）
    pub port_pattern: String,
    /// reconnect 時に復元する UI state
    pub view: RotoView,
}

// ─── 注入 trait（loop が外界に触れる 2 点）─────────────────

/// 現在の cross-project lane 一覧を供給する。CLI は QUIC、daemon は in-process 直読み。
#[allow(async_fn_in_trait)]
pub(crate) trait LaneSource {
    async fn poll(&mut self) -> Result<Vec<RotoLane>>;
}

/// 選択 lane を対象 SP に switch_lane する。SP 越境なので CLI/daemon とも QUIC が正道。
#[allow(async_fn_in_trait)]
pub(crate) trait SwitchSink {
    async fn switch(&mut self, port: u16, token: &str) -> Result<()>;
}

// ─── actions: 共有 control loop ────────────────────────────

/// LCD バッチを 1ms pacing で送る。送信失敗（= ROTO 切断）で false。
///
/// 元 `execute_roto_control` の `std::thread::sleep(1ms)`（block_on 内なので可）を、
/// daemon の multi-thread runtime で worker を塞がない `tokio::time::sleep().await` に置換。
/// pacing（≥1ms）は保つ。
async fn send_paced(conn_out: &mut midir::MidiOutputConnection, msgs: &[Vec<u8>]) -> bool {
    for m in msgs {
        if conn_out.send(m).is_err() {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    true
}

/// ROTO-CONTROL の双方向 control loop（CLI / daemon 共有 body）。
///
/// - keepalive: `roto_autorespond`（応答遅延 = ROTO 切断のため biased で MIDI 最優先）
/// - input: track button → `switch.switch()`、◄/► → ページ送り
/// - output: lane 一覧（`lanes.poll()`）を LCD 8 slot に projection
///
/// `deadline=Some` で CLI（secs 経過 = Deadline）、`None` で daemon（永続）。
/// `shutdown` 発火で graceful 停止（Shutdown）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn roto_control_loop(
    profile: &mut RotoProfile,
    midi_rx: &mut UnboundedReceiver<Vec<u8>>,
    conn_out: &mut midir::MidiOutputConnection,
    view: &mut RotoView,
    lane_source: &mut impl LaneSource,
    switch: &mut impl SwitchSink,
    deadline: Option<Instant>,
    shutdown: CancellationToken,
) -> Result<LoopExit> {
    // active lane の色（シアン）/ 非 active（暗い青灰）
    let color_active = Rgb::new(0, 200, 255);
    let color_inactive = Rgb::new(60, 80, 120);

    let mut input = RotoInput::default();
    let mut lanes: Vec<RotoLane> = Vec::new();
    // 2 秒間隔で lane を poll（最初の tick は即時 = 初期取得）
    let mut poll = tokio::time::interval(Duration::from_secs(2));

    loop {
        tokio::select! {
            // biased: MIDI を最優先に poll（keepalive 応答遅延 → ROTO 切断を防ぐ）。
            biased;
            // MIDI 入力（midir callback → tokio mpsc）
            midi = midi_rx.recv() => {
                let Some(bytes) = midi else { return Ok(LoopExit::Disconnected); }; // sender drop = 切断
                // realtime 1 byte system message は無視
                if bytes.len() == 1 && bytes[0] >= 0xF8 {
                    continue;
                }
                // keepalive SysEx は自動応答（接続維持）。out 送信失敗 = ROTO 切断。
                match roto_autorespond(&bytes, conn_out) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(_) => return Ok(LoopExit::Disconnected),
                }
                // 最初の SysEx で activated（lanes 到着済なら即 projection）
                if !view.activated && bytes.first() == Some(&0xF0) {
                    view.activated = true;
                    if !lanes.is_empty() && !view.lcd_projected {
                        let slots = page_slots(&lanes, view.page, view.selected.as_deref());
                        let msgs = roto_project_slots_full(profile, &slots, color_active, color_inactive);
                        if !send_paced(conn_out, &msgs).await {
                            return Ok(LoopExit::Disconnected);
                        }
                        view.lcd_projected = true;
                    }
                    tracing::info!("🧲 ROTO 接続成立 — {} lanes", lanes.len());
                    continue;
                }
                // channel message を ControlEvent に → binding 表で nav 解決
                let Some(event) = input.parse(&bytes) else { continue; };
                let ControlEvent::Button { index, pressed: true } = event else {
                    continue;
                };
                let Some(nav) = roto_lane_nav(index) else {
                    continue;
                };
                if lanes.is_empty() {
                    continue;
                }
                let pages = lanes.len().div_ceil(8);
                match nav {
                    // ページ送り（view のみ、switch_lane は送らない）
                    LaneNav::PagePrev | LaneNav::PageNext => {
                        view.page = match nav {
                            LaneNav::PagePrev => (view.page + pages - 1) % pages,
                            _ => (view.page + 1) % pages,
                        };
                        if view.lcd_projected {
                            let slots = page_slots(&lanes, view.page, view.selected.as_deref());
                            let msgs = roto_project_slots(profile, &slots, color_active, color_inactive);
                            if !send_paced(conn_out, &msgs).await {
                                return Ok(LoopExit::Disconnected);
                            }
                        }
                    }
                    // 現ページ内の lane を選択 → 対象 SP へ switch_lane
                    LaneNav::Direct(slot) => {
                        let Some(lane) = lanes.get(view.page * 8 + slot).cloned() else {
                            continue; // 空 slot
                        };
                        view.selected = Some(lane.key.clone());
                        // LCD ハイライト更新
                        if view.lcd_projected {
                            let slots = page_slots(&lanes, view.page, view.selected.as_deref());
                            let msgs = roto_project_slots(profile, &slots, color_active, color_inactive);
                            if !send_paced(conn_out, &msgs).await {
                                return Ok(LoopExit::Disconnected);
                            }
                        }
                        // 対象 SP に switch_lane（失敗は warn のみ、loop は継続）
                        if let Err(e) = switch.switch(lane.port, &lane.token).await {
                            tracing::warn!("switch_lane 失敗 (port {}): {}", lane.port, e);
                        }
                    }
                }
            }
            // graceful shutdown（daemon）
            _ = shutdown.cancelled() => {
                return Ok(LoopExit::Shutdown);
            }
            // lane を poll → lanes 再構築（project / lane 増減を live 反映）
            _ = poll.tick() => {
                match lane_source.poll().await {
                    Ok(next) => {
                        if next != lanes {
                            lanes = next;
                            // page を範囲内に clamp
                            let pages = lanes.len().div_ceil(8).max(1);
                            view.page = view.page.min(pages - 1);
                            if view.activated {
                                let slots = page_slots(&lanes, view.page, view.selected.as_deref());
                                let msgs = roto_project_slots_full(profile, &slots, color_active, color_inactive);
                                if !send_paced(conn_out, &msgs).await {
                                    return Ok(LoopExit::Disconnected);
                                }
                                view.lcd_projected = true;
                            }
                        }
                    }
                    Err(e) => {
                        return Ok(LoopExit::Fatal(format!("lane poll: {}", e)));
                    }
                }
            }
            // CLI の secs deadline（daemon は None = 永続）
            _ = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                return Ok(LoopExit::Deadline);
            }
        }
    }
}

// ─── 注入実装 ──────────────────────────────────────────────

/// CLI 版 lane source: world-process channel に `list_all_lanes` を QUIC request。
pub(crate) struct QuicLaneSource {
    pub world_ch: unison::network::UnisonChannel,
}

impl LaneSource for QuicLaneSource {
    async fn poll(&mut self) -> Result<Vec<RotoLane>> {
        let v = self
            .world_ch
            .request::<serde_json::Value, serde_json::Value>(
                "list_all_lanes",
                &serde_json::json!({}),
            )
            .await
            .map_err(|e| anyhow::anyhow!("list_all_lanes: {}", e))?;
        Ok(parse_world_lanes(&v))
    }
}

/// daemon 版 lane source: `build_world_lanes` を in-process 直読み（QUIC self-loop なし）。
/// `parse_world_lanes` を経由するので CLI（QUIC 経由）と lane 並びが完全一致する。
#[allow(clippy::type_complexity)]
pub(crate) struct InProcessLaneSource {
    pub running_processes: Arc<RwLock<HashMap<String, RunningProcess>>>,
    pub lane_registry: Option<Arc<RwLock<HashMap<String, Vec<LaneInfo>>>>>,
    pub world_cap: Option<Arc<RwLock<ProcessManagerCapability>>>,
}

impl LaneSource for InProcessLaneSource {
    async fn poll(&mut self) -> Result<Vec<RotoLane>> {
        let projects = crate::daemon::server::build_world_lanes(
            &self.running_processes,
            &self.lane_registry,
            &self.world_cap,
        )
        .await;
        let v = serde_json::json!({ "projects": projects });
        Ok(parse_world_lanes(&v))
    }
}

/// CLI/daemon 共有の switch_lane sink: 対象 SP port へ per-port QUIC channel を lazy 接続し
/// `switch_lane` を request。Err 時は cache を drop して次回再接続。
///
/// switch_lane は SP scope のまま（各 project の active lane は各 SP が保持）。これは self-loop
/// ではなく SP 越境の正当な cross-process call なので daemon でも QUIC が正道。
#[derive(Default)]
pub(crate) struct QuicSwitchSink {
    cache: HashMap<u16, (unison::ProtocolClient, unison::network::UnisonChannel)>,
}

impl QuicSwitchSink {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl SwitchSink for QuicSwitchSink {
    async fn switch(&mut self, port: u16, token: &str) -> Result<()> {
        // per-port channel を lazy 接続
        if let std::collections::hash_map::Entry::Vacant(slot) = self.cache.entry(port) {
            let client = super::midi::connect_quic_local(port).await?;
            let ch = client
                .open_channel("process")
                .await
                .map_err(|e| anyhow::anyhow!("process channel open (port {}): {}", port, e))?;
            slot.insert((client, ch));
        }
        let payload = serde_json::json!({ "type": "switch_lane", "lane": token });
        let res = match self.cache.get(&port) {
            Some((_c, ch)) => {
                ch.request::<serde_json::Value, serde_json::Value>("switch_lane", &payload)
                    .await
            }
            None => return Ok(()),
        };
        if let Err(e) = res {
            self.cache.remove(&port); // 次回再接続
            return Err(anyhow::anyhow!("switch_lane (port {}): {}", port, e));
        }
        Ok(())
    }
}

// ─── self-heal: nostos AsyncBracket / AsyncDriver ──────────

/// 接続中 ROTO セッションの Active state。`enter` で open + handshake 済の handle を抱える。
/// open 失敗時は `open_failed` marker のみ（exit が即 Reborn して driver が backoff 再試行）。
pub(crate) struct RotoActive {
    /// Some = open/handshake 失敗（device 抜け等）→ exit が即 Reborn
    open_failed: Option<RotoDescriptor>,
    inner: Option<RotoActiveInner>,
}

struct RotoActiveInner {
    // conn_in は drop すると input が切れるため keep alive
    _conn_in: midir::MidiInputConnection<()>,
    midi_rx: UnboundedReceiver<Vec<u8>>,
    conn_out: midir::MidiOutputConnection,
    profile: RotoProfile,
    view: RotoView,
    port_pattern: String,
}

/// ROTO 持続セッションの 1 接続サイクルを表す `AsyncBracket`。
/// enter = port open(in+out) + DAW_START handshake、exit = control loop を回し
/// disconnect で `Reborn`（view 退避）/ shutdown で `Done` / 致命で `Failed`。
/// `lane_source` / `switch_sink` は reconnect を跨いで再利用するため bracket が所有する。
pub(crate) struct RotoSessionBracket<L: LaneSource, S: SwitchSink> {
    lane_source: Mutex<L>,
    switch_sink: Mutex<S>,
    shutdown: CancellationToken,
}

impl<L: LaneSource, S: SwitchSink> RotoSessionBracket<L, S> {
    pub(crate) fn new(lane_source: L, switch_sink: S, shutdown: CancellationToken) -> Self {
        Self {
            lane_source: Mutex::new(lane_source),
            switch_sink: Mutex::new(switch_sink),
            shutdown,
        }
    }
}

impl<L: LaneSource, S: SwitchSink> AsyncBracket for RotoSessionBracket<L, S> {
    type Input = RotoDescriptor;
    type Active = RotoActive;
    type Done = ();
    type Reborn = RotoDescriptor;
    type Failed = String;

    async fn enter(&self, input: RotoDescriptor) -> RotoActive {
        match roto_open_async(&input.port_pattern) {
            Ok((conn_in, midi_rx, mut conn_out, port_name)) => {
                // handshake（DAW_START）。out 送信失敗 = ROTO 抜け → OpenFailed marker。
                let profile = RotoProfile::default();
                for msg in profile.handshake() {
                    if conn_out.send(&msg).is_err() {
                        return RotoActive {
                            open_failed: Some(input),
                            inner: None,
                        };
                    }
                }
                tracing::info!(
                    "🧲 ROTO connected: {} (DAW_START handshake sent)",
                    port_name
                );
                RotoActive {
                    open_failed: None,
                    inner: Some(RotoActiveInner {
                        _conn_in: conn_in,
                        midi_rx,
                        conn_out,
                        profile,
                        view: input.view.clone(),
                        port_pattern: input.port_pattern,
                    }),
                }
            }
            Err(e) => {
                // device 不在/抜き差し中。enter は Active を返す契約なので marker に畳む。
                tracing::debug!("🧲 ROTO open 待機 (port 不在?): {}", e);
                RotoActive {
                    open_failed: Some(input),
                    inner: None,
                }
            }
        }
    }

    async fn exit(&self, active: RotoActive) -> Outcome<(), RotoDescriptor, String> {
        // open 失敗 → driver が backoff して再 enter（wait-for-plug）
        if let Some(desc) = active.open_failed {
            return Outcome::Reborn(desc);
        }
        let mut inner = active.inner.expect("inner present when not open_failed");

        let mut ls = self.lane_source.lock().await;
        let mut ss = self.switch_sink.lock().await;
        let result = roto_control_loop(
            &mut inner.profile,
            &mut inner.midi_rx,
            &mut inner.conn_out,
            &mut inner.view,
            &mut *ls,
            &mut *ss,
            None, // daemon = 永続（deadline なし）
            self.shutdown.clone(),
        )
        .await;
        // conn_in/conn_out は inner drop で閉じる（= ROTO graceful 切断）
        let reborn = RotoDescriptor {
            port_pattern: inner.port_pattern.clone(),
            view: inner.view.clone(),
        };
        match result {
            Ok(LoopExit::Shutdown) => Outcome::Done(()),
            Ok(LoopExit::Disconnected) | Ok(LoopExit::Deadline) => Outcome::Reborn(reborn),
            Ok(LoopExit::Fatal(msg)) => Outcome::Failed(msg),
            Err(e) => {
                tracing::warn!("🧲 ROTO loop error (再接続を試みる): {}", e);
                Outcome::Reborn(reborn)
            }
        }
    }
}

/// reconnect heal driver。disconnect（Reborn）後に backoff sleep して再 enter、
/// shutdown 中なら打ち切り（`Err` で `AsyncDriver::run` を `Reborn` 終端させる）。
pub(crate) struct RotoHealDriver {
    pub shutdown: CancellationToken,
    pub backoff: Duration,
}

impl<L: LaneSource, S: SwitchSink> AsyncDriver<RotoSessionBracket<L, S>> for RotoHealDriver {
    async fn next(&mut self, reborn: RotoDescriptor) -> Result<RotoDescriptor, RotoDescriptor> {
        if self.shutdown.is_cancelled() {
            return Err(reborn);
        }
        // replug debounce / port 再 enumerate 待ち。shutdown が来たら即打ち切り。
        tokio::select! {
            _ = self.shutdown.cancelled() => Err(reborn),
            _ = tokio::time::sleep(self.backoff) => Ok(reborn),
        }
    }
}
