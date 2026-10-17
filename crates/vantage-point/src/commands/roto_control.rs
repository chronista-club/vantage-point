//! Explicit DAW-mode diagnostic CLI (`vp midi roto control`).
//! Normal VP navigation uses devices.rs input-only MIDI listeners instead.
//! LaneSource / SwitchSink keep CLI I/O separate from lane calculations.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::capability::core::CapabilityEvent;
use crate::capability::eventbus::EventBus;
use crate::device_input::{ControlEvent, DeviceInput, roto::RotoInput};
use crate::device_profile::{Rgb, roto::RotoProfile};

use super::midi::{
    LaneNav, RotoLane, page_slots, parse_node_lanes, roto_autorespond, roto_lane_nav,
    roto_repo_slots, roto_repo_slots_full,
};

// ─── data ──────────────────────────────────────────────────

/// DAW CLI control loop の UI state。
#[derive(Default, Clone)]
pub(crate) struct RotoView {
    /// 表示ページ（1 ページ = 8 lane）。reconnect で保持。
    pub page: usize,
    /// 選択中 lane key `"{port}:{token}"`。reconnect で保持。
    pub selected: Option<String>,
    /// projection 可能か。reconnect で carry-over（poll 即描画のため、上記参照）。
    pub activated: bool,
    /// LCD projection 済みか。reconnect で carry-over（上記参照）。
    pub lcd_repoed: bool,
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

// ─── 注入 trait（loop が外界に触れる 2 点）─────────────────

/// 現在の cross-project lane 一覧を供給する。CLI は QUIC、daemon は in-process 直読み。
#[allow(async_fn_in_trait)]
pub(crate) trait LaneSource {
    async fn poll(&mut self) -> Result<Vec<RotoLane>>;
}

/// 選択 lane を対象 repo に switch_lane する。L0 portless: repo は listen しないので CLI/daemon とも
/// Daemon :32000 の repo-proxy ask 経由で forward する（repo_path で repo を逆引き）。
#[allow(async_fn_in_trait)]
pub(crate) trait SwitchSink {
    async fn switch(&mut self, repo_path: &str, token: &str) -> Result<()>;
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
/// loop が消費しない入力（Knob / KnobTouch）を EventBus に流す口（doc 49 LE-19 fleet 配線）。
///
/// ROTO は DeviceRegistry の汎用 input listener（`spawn_input_listener`）から除外され本 loop が
/// input を独占所有するため、`devices.control_event` への合流はここから行う。
/// Button は lane-nav（Device I/O 経路）の意味を既に持つため流さない（二重配送の回避）。
/// None = CLI 前景デバッグ（EventBus 不在）。
pub(crate) struct ControlTap {
    pub event_bus: Arc<EventBus>,
    pub port_name: String,
}

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
    control_tap: Option<&ControlTap>,
    // フィードバック方向（LE-19）: DeviceRegistry `apply_feedback` からの motor byte 列注入。
    // conn_out を loop が独占所有するため、外からの書き込みはこの watch を通る
    // （latest-wins — 未消費の古い frame は新しい値で置き換わる）。
    // sender が閉じたら arm を止める（CLI 前景 = dummy channel で即閉）
    feedback_rx: &mut tokio::sync::watch::Receiver<Option<Vec<Vec<u8>>>>,
) -> Result<LoopExit> {
    // active lane の色（シアン）/ 非 active（暗い青灰）
    let color_active = Rgb::new(0, 200, 255);
    let color_inactive = Rgb::new(60, 80, 120);

    let mut input = RotoInput::default();
    let mut lanes: Vec<RotoLane> = Vec::new();
    // 2 秒間隔で lane を poll（最初の tick は即時 = 初期取得）
    let mut poll = tokio::time::interval(Duration::from_secs(2));
    // feedback sender が閉じたら arm を止める（閉チャネルの即 None で busy loop しない）
    let mut feedback_alive = true;

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
                // 応答 latency を増やさないため activation 判定より先に応答する（continue は
                // activation 判定の後まで遅延 — 下記参照）。
                let autoresponded = match roto_autorespond(&bytes, conn_out) {
                    Ok(r) => r,
                    Err(_) => return Ok(LoopExit::Disconnected),
                };
                // 最初の SysEx で activated（lanes 到着済なら即 projection）。
                // ⚠️ keepalive hello (0A 02) も activation 信号に数える（autorespond 済みでも
                // 判定する）: daemon 側だけが再起動した場合、session 継続中の ROTO は full
                // handshake (0A 0E version 等) を再送せず hello しか送らない。hello を signal に
                // しないと activated が永遠に立たず、LCD projection が全経路 gate されたままになる
                // （track button は channel message で activation 不要のため「lane 切替は効くのに
                // LCD だけ沈黙」という片肺状態に見える）。hello 到来 = device は DAW session 中
                // = projection 可能、なので activation 根拠として十分。
                if !view.activated && bytes.first() == Some(&0xF0) {
                    view.activated = true;
                    if !lanes.is_empty() && !view.lcd_repoed {
                        let slots = page_slots(&lanes, view.page, view.selected.as_deref());
                        let msgs = roto_repo_slots_full(profile, &slots, color_active, color_inactive);
                        if !send_paced(conn_out, &msgs).await {
                            return Ok(LoopExit::Disconnected);
                        }
                        view.lcd_repoed = true;
                    }
                    tracing::info!("🧲 ROTO 接続成立 — {} lanes", lanes.len());
                }
                if autoresponded {
                    continue;
                }
                // channel message を ControlEvent に → binding 表で nav 解決
                let Some(event) = input.parse(&bytes) else { continue; };
                // knob 系は fleet 配線へ（loop は消費しない — 場の操作は webview 側 mapping の領分）
                if let Some(tap) = control_tap
                    && matches!(event, ControlEvent::Knob { .. } | ControlEvent::KnobTouch { .. })
                {
                    let cap_event = CapabilityEvent::new("devices.control_event", "devices")
                        .with_payload(&serde_json::json!({
                            "port_name": tap.port_name,
                            "event": event,
                        }));
                    tap.event_bus.emit(cap_event).await;
                }
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
                        if view.lcd_repoed {
                            let slots = page_slots(&lanes, view.page, view.selected.as_deref());
                            let msgs = roto_repo_slots(profile, &slots, color_active, color_inactive);
                            if !send_paced(conn_out, &msgs).await {
                                return Ok(LoopExit::Disconnected);
                            }
                        }
                    }
                    // 現ページ内の lane を選択 → 対象 repo へ switch_lane
                    LaneNav::Direct(slot) => {
                        let Some(lane) = lanes.get(view.page * 8 + slot).cloned() else {
                            continue; // 空 slot
                        };
                        view.selected = Some(lane.key.clone());
                        // LCD ハイライト更新
                        if view.lcd_repoed {
                            let slots = page_slots(&lanes, view.page, view.selected.as_deref());
                            let msgs = roto_repo_slots(profile, &slots, color_active, color_inactive);
                            if !send_paced(conn_out, &msgs).await {
                                return Ok(LoopExit::Disconnected);
                            }
                        }
                        // 対象 repo に switch_lane（失敗は warn のみ、loop は継続）
                        if let Err(e) = switch.switch(&lane.repo_path, &lane.token).await {
                            tracing::warn!("switch_lane 失敗 (repo {}): {}", lane.repo_path, e);
                        }
                    }
                }
            }
            // フィードバック方向（LE-19）: DeviceRegistry からの motor byte 列を conn_out に流す。
            // 送信失敗 = ROTO 切断（他の out 送信と同じ扱い）
            changed = feedback_rx.changed(), if feedback_alive => {
                match changed {
                    Ok(()) => {
                        let msgs = feedback_rx.borrow_and_update().clone();
                        if let Some(msgs) = msgs
                            && !send_paced(conn_out, &msgs).await
                        {
                            return Ok(LoopExit::Disconnected);
                        }
                    }
                    Err(_) => feedback_alive = false, // sender 閉 = 注入路なし（CLI 前景等）
                }
            }
            // graceful shutdown（daemon）
            _ = shutdown.cancelled() => {
                return Ok(LoopExit::Shutdown);
            }
            // lane を poll → lanes 再構築（repo / lane 増減を live 反映）
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
                                let msgs = roto_repo_slots_full(profile, &slots, color_active, color_inactive);
                                if !send_paced(conn_out, &msgs).await {
                                    return Ok(LoopExit::Disconnected);
                                }
                                view.lcd_repoed = true;
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

/// CLI 版 lane source: daemon-repo channel に `list_all_lanes` を QUIC request。
pub(crate) struct QuicLaneSource {
    pub daemon_ch: unison::network::UnisonChannel,
}

impl LaneSource for QuicLaneSource {
    async fn poll(&mut self) -> Result<Vec<RotoLane>> {
        let v = self
            .daemon_ch
            .request::<serde_json::Value, serde_json::Value>(
                "list_all_lanes",
                &serde_json::json!({}),
            )
            .await
            .map_err(|e| anyhow::anyhow!("list_all_lanes: {}", e))?;
        Ok(parse_node_lanes(&v))
    }
}

/// CLI の switch_lane sink: L0 portless で repo が listen しなくなったため、対象 repo の
/// repo_path を使い Daemon :32000 の repo-proxy ask 経由で `switch_lane` を forward する。
///
/// switch_lane は repo scope のまま（各 repo の active lane は各 repo が保持）。daemon = Daemon 本体
/// なので世界への self-loop QUIC になるが、switch はボタン押下時のみの低頻度なので loopback の
/// connect+forward（数 ms）で keepalive を落とさない（lane poll の 2 秒周期とは違い cache 不要）。
#[derive(Default)]
pub(crate) struct QuicSwitchSink;

impl QuicSwitchSink {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl SwitchSink for QuicSwitchSink {
    async fn switch(&mut self, repo_path: &str, token: &str) -> Result<()> {
        // payload = RepoMessage::SwitchLane の JSON 表現（`{"type":"switch_lane","lane":...}`）。
        // daemon が repo_path を path_key 逆引きして当該 repo の dispatch_repo_method へ forward。
        let payload = serde_json::json!({ "type": "switch_lane", "lane": token });
        super::process_client::daemon_repo_request(
            crate::cli::daemon_port(),
            repo_path,
            "switch_lane",
            payload,
        )
        .await
        .map_err(|e| anyhow::anyhow!("switch_lane (repo {}): {}", repo_path, e))?;
        Ok(())
    }
}
