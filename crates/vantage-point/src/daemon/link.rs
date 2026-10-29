//! Editor Link hub（doc 86）— creo-ui editor-host の Link を受けて、AI と机の機材に写す daemon 側の実体。
//!
//! wire の SSOT は creo-ui `docs/design/editor-link.md`。面 → hub は `hello` / `mode` / `fields`（全量）/
//! `selection` / `values` / `set`、hub → 面は `set` / `mode` / `get`。creo-ui は MIDI を知らず、
//! 値の clamp もしない（範囲に収めるのは相手 = ここ の責務）。この module が持つのは:
//!
//! - **面の registry**: 誰が繋がっているか（`hello` の名札 `app` / `surface`）、各面の mode /
//!   fields / selection / values の cache
//! - **調停**: active な面 = 最後に Editor ON した面（無ければ最後に繋いだ面）。Touch 中の切替保留は
//!   機材が入る PR（doc 86 §4）で足す
//! - **clamp**: hub から出す `set` を field の `constraints.min / max` に収める
//! - **MCP の読み**: fields / values は `get` の往復で取る（面が `values` を返した瞬間の cache を
//!   返す = authoritative）。cache 単体は健康表示と将来の mapping 用
//! - **守りの判定** [`admit`]: loopback peer + Origin allowlist。判定だけをここに置き、WS の
//!   upgrade より前に 403 で断るのは `repo/http/link.rs`
//!
//! 口（WS）は `repo/http/link.rs`、AI の口は MCP `editor_*` → daemon "link" channel →
//! [`handle_link_channel`]。`std::sync::Mutex` なのは lock を持ったまま await する箇所が無いため
//! （`get` の待ちは lock の外で `watch` を待つ）。

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

/// wire の版（creo-ui `LINK_VERSION`）。違う版の `hello` は受けない（互換を壊す変更で上がる）。
pub(crate) const LINK_VERSION: u64 = 1;

/// vp-app webview の origin（`boot.rs` の統合 origin fix で `vp-asset://app/index.html` を load する）。
pub(crate) const VP_APP_ORIGIN: &str = "vp-asset://app";

/// `get` の往復を待つ上限。MCP 側の outer timeout（5s、`link_call`）より短くする
/// （VP-163: server が client より長く待つと channel reset → 空振りリトライになる）。
pub(crate) const GET_TIMEOUT: Duration = Duration::from_secs(3);

/// Editor Mode の on / off。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Mode {
    On,
    Off,
}

/// 面 → hub の 1 message。形が合わないものは黙って捨てる（creo-ui `parseInbound` と同じ姿勢）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "lowercase")]
pub(crate) enum Inbound {
    /// open 直後に 1 回。`v` = wire の版、`app` = 名札、`surface` = 面の id（tab ごと）
    Hello {
        v: u64,
        app: String,
        surface: String,
    },
    Mode {
        mode: Mode,
    },
    /// **全量**（差分ではない）。register / unregister で一覧が変わるたびに届く
    Fields {
        fields: Vec<Value>,
    },
    /// `null` = 選択なし（相手の `mode off` で消えた selection もこれで届く）
    Selection {
        selection: Option<Value>,
    },
    /// open 直後と `get` への返答
    Values {
        values: serde_json::Map<String, Value>,
    },
    /// 面の側で値が変わった（slider / console / AI）
    Set {
        id: String,
        value: Value,
    },
}

/// hub → 面 の 1 message。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "t", rename_all = "lowercase")]
pub(crate) enum Outbound {
    Set {
        id: String,
        value: Value,
    },
    /// 面の Editor Mode を外から切替える。送り手は §4 の機材（ROTO の Mode toggle ボタン）で生える —
    /// wire の全量を型に持っておく（creo-ui `parseInbound` が受ける 3 種のうちの 1 つ）
    #[allow(dead_code)]
    Mode {
        mode: Mode,
    },
    /// 面は `mode → fields → selection → values` を送り返す（`hello` は含まない）
    Get,
}

// =========================================================================
// 守りの判定（純 calculation）
// =========================================================================

/// `/link` を断る理由。HTTP 層はこれを 403 の body に出し、warn log に残す
/// （沈黙だと「繋ぎに来ていない」と「弾いている」が区別できない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// loopback 以外の peer。listener が dual-stack で LAN からも届くため、ここで切る
    NotLoopback(SocketAddr),
    /// ブラウザが付けた Origin が allowlist 外（cross-site WebSocket hijacking の防ぎ）
    OriginNotAllowed(String),
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejection::NotLoopback(peer) => write!(f, "Editor Link is loopback-only (peer {peer})"),
            Rejection::OriginNotAllowed(origin) => {
                write!(f, "Editor Link does not accept Origin {origin}")
            }
        }
    }
}

/// `/link` の入場判定。**Origin が無ければ通す**（ブラウザは必ず付け、CLI 等の非ブラウザは付けない
/// — 非ブラウザは loopback 判定だけで `vp` CLI と同じ信頼）。Origin があれば allowlist に限る。
pub(crate) fn admit(peer: SocketAddr, origin: Option<&str>) -> Result<(), Rejection> {
    if !is_loopback(peer.ip()) {
        return Err(Rejection::NotLoopback(peer));
    }
    match origin {
        None => Ok(()),
        Some(o) if origin_allowed(o) => Ok(()),
        Some(o) => Err(Rejection::OriginNotAllowed(o.to_string())),
    }
}

/// `127.0.0.0/8`、`::1`、そして dual-stack listener が IPv4 client に見せる v4-mapped
/// `::ffff:127.x.x.x` の 3 形。最後を忘れると Chrome からの接続が全部落ちる。
fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// allowlist: vp-app webview（[`VP_APP_ORIGIN`]）と、loopback host の http(s) origin
/// （creo-ui site の dev server 等。port は問わない）。`null`（opaque origin）は通さない —
/// sandbox iframe 等から誰でも名乗れる値なので、入れると allowlist が意味を失う。
fn origin_allowed(origin: &str) -> bool {
    if origin == VP_APP_ORIGIN {
        return true;
    }
    let Some(host_port) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    // Origin は scheme://host[:port] だけ。path 付きは形が違うので弾く
    if host_port.contains('/') {
        return false;
    }
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        // `[::1]:5173` — 閉じ括弧まで
        match rest.find(']') {
            Some(end) => &rest[..end],
            None => return false,
        }
    } else {
        host_port.rsplit_once(':').map_or(host_port, |(h, _)| h)
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

// =========================================================================
// 面の registry と調停
// =========================================================================

/// 1 接続ぶんの登録票。`conn_id` は接続ごとに一意で、同じ `surface` id の再接続（reload）が
/// 古い接続の後始末に巻き込まれないための鍵（古い task の `disconnect` は id が違えば no-op）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registration {
    pub(crate) conn_id: u64,
    pub(crate) surface: String,
}

/// hub の操作が失敗する理由。MCP には `to_string()` がそのまま届く。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkError {
    /// `hello.v` が [`LINK_VERSION`] と違う
    UnsupportedVersion(u64),
    /// 面が 1 つも繋がっていない
    NoSurface,
    /// `surface` 指定がどの面にも当たらない（id にも名札にも）
    UnknownSurface {
        target: String,
        available: Vec<String>,
    },
    /// 面の fields に無い id へ `set` しようとした（host は知らない id を黙って捨てるので、ここで言う）
    UnknownField { surface: String, id: String },
    /// `get` を送ったが面が答えなかった
    Timeout { surface: String },
    /// 送る途中で面が消えた
    SurfaceGone(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::UnsupportedVersion(v) => {
                write!(
                    f,
                    "unsupported Editor Link version {v} (hub speaks {LINK_VERSION})"
                )
            }
            LinkError::NoSurface => write!(
                f,
                "no editor surface is linked (open vp-app, or a creo-ui site with link enabled)"
            ),
            LinkError::UnknownSurface { target, available } => write!(
                f,
                "editor surface '{target}' is not linked (available: {})",
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                }
            ),
            LinkError::UnknownField { surface, id } => {
                write!(f, "field '{id}' is not bound on surface {surface}")
            }
            LinkError::Timeout { surface } => write!(
                f,
                "surface {surface} did not answer `get` within {}s",
                GET_TIMEOUT.as_secs()
            ),
            LinkError::SurfaceGone(surface) => write!(f, "surface {surface} disconnected"),
        }
    }
}

/// 1 面の状態（registry の値）。
struct Surface {
    conn_id: u64,
    app: String,
    mode: Mode,
    fields: Vec<Value>,
    selection: Option<Value>,
    values: serde_json::Map<String, Value>,
    connected_at: Instant,
    /// 最後に Editor ON になった時刻（active の調停に使う。OFF では消さない）
    editor_on_at: Option<Instant>,
    /// hub → 面 の送り口（WS task が受けて書く）
    outbound: mpsc::UnboundedSender<Outbound>,
    /// `values` が届くたびに +1。`get` の往復待ちはこれを `watch` する
    snapshot_seq: watch::Sender<u64>,
}

/// 外に見せる 1 面の要約（`/api/health` と `surfaces` method）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct SurfaceInfo {
    pub(crate) surface: String,
    pub(crate) app: String,
    pub(crate) mode: Mode,
    pub(crate) fields: usize,
    pub(crate) active: bool,
}

/// `get` 往復後の 1 面の全体像（MCP `editor_fields` / `editor_values` の材料）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) surface: String,
    pub(crate) app: String,
    pub(crate) mode: Mode,
    pub(crate) fields: Vec<Value>,
    pub(crate) selection: Option<Value>,
    pub(crate) values: serde_json::Map<String, Value>,
}

/// hub から出した `set` の結果。`value` は clamp 後の値（cache にもこれが入る）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SetOutcome {
    pub(crate) surface: String,
    pub(crate) id: String,
    pub(crate) value: Value,
    pub(crate) clamped: bool,
}

#[derive(Default)]
struct Inner {
    surfaces: HashMap<String, Surface>,
}

/// Editor Link の hub。`DaemonState::link` として daemon に 1 つ。
pub(crate) struct LinkHub {
    inner: Mutex<Inner>,
    next_conn: AtomicU64,
}

impl Default for LinkHub {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkHub {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            next_conn: AtomicU64::new(1),
        }
    }

    /// poison は中身を取り出して続行（map は短い操作しかしないので壊れない）。
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `hello` を受けて面を登録する。同じ `surface` id が居れば置き換える（reload / 再接続）。
    pub(crate) fn connect(
        &self,
        v: u64,
        app: String,
        surface: String,
        outbound: mpsc::UnboundedSender<Outbound>,
    ) -> Result<Registration, LinkError> {
        if v != LINK_VERSION {
            return Err(LinkError::UnsupportedVersion(v));
        }
        let conn_id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let (snapshot_seq, _) = watch::channel(0u64);
        let entry = Surface {
            conn_id,
            app,
            mode: Mode::Off,
            fields: Vec::new(),
            selection: None,
            values: serde_json::Map::new(),
            connected_at: Instant::now(),
            editor_on_at: None,
            outbound,
            snapshot_seq,
        };
        self.lock().surfaces.insert(surface.clone(), entry);
        Ok(Registration { conn_id, surface })
    }

    /// 面からの 1 message を cache に写す。`conn_id` が今の登録と違えば（置き換え済み）無視。
    /// 2 回目以降の `hello` は名札の更新として扱う。
    pub(crate) fn apply(&self, reg: &Registration, msg: Inbound) {
        let mut inner = self.lock();
        let Some(s) = inner.surfaces.get_mut(&reg.surface) else {
            return;
        };
        if s.conn_id != reg.conn_id {
            return;
        }
        match msg {
            Inbound::Hello { app, .. } => s.app = app,
            Inbound::Mode { mode } => {
                s.mode = mode;
                if mode == Mode::On {
                    s.editor_on_at = Some(Instant::now());
                }
            }
            Inbound::Fields { fields } => s.fields = fields,
            Inbound::Selection { selection } => s.selection = selection,
            Inbound::Values { values } => {
                s.values = values;
                s.snapshot_seq.send_modify(|seq| *seq += 1);
            }
            Inbound::Set { id, value } => {
                s.values.insert(id, value);
            }
        }
    }

    /// 接続が閉じた。登録が自分のものなら外す（置き換えられていれば触らない）。
    pub(crate) fn disconnect(&self, reg: &Registration) {
        let mut inner = self.lock();
        if inner
            .surfaces
            .get(&reg.surface)
            .is_some_and(|s| s.conn_id == reg.conn_id)
        {
            inner.surfaces.remove(&reg.surface);
        }
    }

    /// active な面の id。順位は (今 ON か, 最後に ON になった時刻, 繋いだ時刻) の降順。
    pub(crate) fn active_surface(&self) -> Option<String> {
        active_of(&self.lock())
    }

    /// `target`（面の id か名札）を面の id に解く。省略は active。本番の呼び手は `set` / `snapshot` が
    /// 内側で `resolve_in` を使うので、外に出すのは test の観測用だけ。
    #[cfg(test)]
    pub(crate) fn resolve(&self, target: Option<&str>) -> Result<String, LinkError> {
        resolve_in(&self.lock(), target)
    }

    /// 繋がっている面の一覧（id 順、安定）。
    pub(crate) fn surfaces(&self) -> Vec<SurfaceInfo> {
        let inner = self.lock();
        let active = active_of(&inner);
        let mut out: Vec<SurfaceInfo> = inner
            .surfaces
            .iter()
            .map(|(id, s)| SurfaceInfo {
                surface: id.clone(),
                app: s.app.clone(),
                mode: s.mode,
                fields: s.fields.len(),
                active: active.as_deref() == Some(id.as_str()),
            })
            .collect();
        out.sort_by(|a, b| a.surface.cmp(&b.surface));
        out
    }

    /// `/api/health` の `services.link`。`connected` = 面が 1 つ以上、`idle` = 無し。
    pub(crate) fn health(&self) -> (&'static str, Value) {
        let surfaces = self.surfaces();
        let active = surfaces.iter().find(|s| s.active).map(|s| s.app.clone());
        let status = if surfaces.is_empty() {
            "idle"
        } else {
            "connected"
        };
        (status, json!({ "surfaces": surfaces, "active": active }))
    }

    /// 面へ `set` を送る。値は field の `constraints.min / max` に収めてから送り、cache も更新する
    /// （面は hub からの `set` を echo しない契約なので、cache は自分で書く）。
    pub(crate) fn set(
        &self,
        target: Option<&str>,
        id: &str,
        value: Value,
    ) -> Result<SetOutcome, LinkError> {
        let mut inner = self.lock();
        let surface = resolve_in(&inner, target)?;
        let s = inner
            .surfaces
            .get_mut(&surface)
            .ok_or_else(|| LinkError::SurfaceGone(surface.clone()))?;
        let field = s
            .fields
            .iter()
            .find(|f| f.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| LinkError::UnknownField {
                surface: surface.clone(),
                id: id.to_string(),
            })?;
        let (value, clamped) = clamp_to_constraints(field, value);
        s.outbound
            .send(Outbound::Set {
                id: id.to_string(),
                value: value.clone(),
            })
            .map_err(|_| LinkError::SurfaceGone(surface.clone()))?;
        s.values.insert(id.to_string(), value.clone());
        Ok(SetOutcome {
            surface,
            id: id.to_string(),
            value,
            clamped,
        })
    }

    /// `get` を送り、面が `values` を返すまで待ってから cache を読む（= authoritative な読み）。
    pub(crate) async fn snapshot(
        &self,
        target: Option<&str>,
        timeout: Duration,
    ) -> Result<Snapshot, LinkError> {
        // lock の中: 解決 + 送り口と watch の取り出し。await は lock の外
        let (surface, mut seq_rx) = {
            let inner = self.lock();
            let surface = resolve_in(&inner, target)?;
            let s = inner
                .surfaces
                .get(&surface)
                .ok_or_else(|| LinkError::SurfaceGone(surface.clone()))?;
            let rx = s.snapshot_seq.subscribe();
            s.outbound
                .send(Outbound::Get)
                .map_err(|_| LinkError::SurfaceGone(surface.clone()))?;
            (surface, rx)
        };
        // `subscribe()` 時点の版は既読。次の `values` で +1 されるのを待つ
        match tokio::time::timeout(timeout, seq_rx.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err(LinkError::SurfaceGone(surface)),
            Err(_) => return Err(LinkError::Timeout { surface }),
        }
        let inner = self.lock();
        let s = inner
            .surfaces
            .get(&surface)
            .ok_or_else(|| LinkError::SurfaceGone(surface.clone()))?;
        Ok(Snapshot {
            surface,
            app: s.app.clone(),
            mode: s.mode,
            fields: s.fields.clone(),
            selection: s.selection.clone(),
            values: s.values.clone(),
        })
    }
}

fn active_of(inner: &Inner) -> Option<String> {
    inner
        .surfaces
        .iter()
        .max_by_key(|(_, s)| (s.mode == Mode::On, s.editor_on_at, s.connected_at))
        .map(|(id, _)| id.clone())
}

/// `target` の解き方: 面の id に一致 → それ。名札（大文字小文字を無視）に一致 → その中で順位最上位。
fn resolve_in(inner: &Inner, target: Option<&str>) -> Result<String, LinkError> {
    let Some(target) = target.map(str::trim).filter(|t| !t.is_empty()) else {
        return active_of(inner).ok_or(LinkError::NoSurface);
    };
    if inner.surfaces.contains_key(target) {
        return Ok(target.to_string());
    }
    let by_app = inner
        .surfaces
        .iter()
        .filter(|(_, s)| s.app.eq_ignore_ascii_case(target))
        .max_by_key(|(_, s)| (s.mode == Mode::On, s.editor_on_at, s.connected_at))
        .map(|(id, _)| id.clone());
    by_app.ok_or_else(|| {
        let mut available: Vec<String> = inner
            .surfaces
            .iter()
            .map(|(id, s)| format!("{id} ({})", s.app))
            .collect();
        available.sort();
        LinkError::UnknownSurface {
            target: target.to_string(),
            available,
        }
    })
}

/// number field だけ `constraints.min / max` に収める。他の型と、数でない値はそのまま。
/// 戻りの bool = clamp で値が変わったか。
fn clamp_to_constraints(field: &Value, value: Value) -> (Value, bool) {
    if field.get("type").and_then(Value::as_str) != Some("number") {
        return (value, false);
    }
    let Some(n) = value.as_f64() else {
        return (value, false);
    };
    let constraints = field.get("constraints");
    let min = constraints
        .and_then(|c| c.get("min"))
        .and_then(Value::as_f64);
    let max = constraints
        .and_then(|c| c.get("max"))
        .and_then(Value::as_f64);
    let mut out = n;
    if let Some(min) = min {
        out = out.max(min);
    }
    if let Some(max) = max {
        out = out.min(max);
    }
    if out == n {
        (value, false)
    } else {
        (json!(out), true)
    }
}

// =========================================================================
// daemon "link" channel の method dispatch（MCP editor_* の着地点）
// =========================================================================

/// "link" channel の method を hub に振る。戻りは成功時 result JSON、失敗時 `Err(String)`
/// （呼び手が Unison の慣習 VP-163 で `{"error": ...}` に詰める）。
///
/// payload の `surface`（任意）は面の id か名札。省略は active な面。
pub(crate) async fn handle_link_channel(
    hub: &LinkHub,
    method: &str,
    payload: Value,
) -> Result<Value, String> {
    let target = payload
        .get("surface")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match method {
        "surfaces" => Ok(json!({
            "surfaces": hub.surfaces(),
            "active": hub.active_surface(),
        })),
        "editor_fields" => {
            let s = hub
                .snapshot(target.as_deref(), GET_TIMEOUT)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "surface": s.surface,
                "app": s.app,
                "mode": s.mode,
                "fields": s.fields,
            }))
        }
        "editor_values" => {
            let s = hub
                .snapshot(target.as_deref(), GET_TIMEOUT)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "surface": s.surface,
                "app": s.app,
                "mode": s.mode,
                "selection": s.selection,
                "values": s.values,
            }))
        }
        "editor_set" => {
            let id = payload
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "editor_set: id 必須".to_string())?;
            let value = payload
                .get("value")
                .cloned()
                .ok_or_else(|| "editor_set: value 必須".to_string())?;
            let out = hub
                .set(target.as_deref(), id, value)
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "ok": true,
                "surface": out.surface,
                "id": out.id,
                "value": out.value,
                "clamped": out.clamped,
            }))
        }
        other => Err(format!("unknown link method: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(ip: &str) -> SocketAddr {
        format!("{ip}:54321")
            .parse()
            .unwrap_or_else(|_| format!("[{ip}]:54321").parse().expect("addr"))
    }

    // ---------- 守りの判定 ----------

    /// loopback は 3 形（v4 / v6 / v4-mapped）とも通り、LAN は Origin が何でも断る。
    #[test]
    fn admit_requires_loopback_peer_in_all_three_shapes() {
        for ip in ["127.0.0.1", "127.0.0.2", "::1", "::ffff:127.0.0.1"] {
            assert_eq!(admit(peer(ip), None), Ok(()), "ip={ip}");
        }
        for ip in ["192.168.1.20", "10.0.0.5", "fe80::1", "::ffff:192.168.1.20"] {
            assert!(
                matches!(admit(peer(ip), None), Err(Rejection::NotLoopback(_))),
                "ip={ip}"
            );
            assert!(
                matches!(
                    admit(peer(ip), Some(VP_APP_ORIGIN)),
                    Err(Rejection::NotLoopback(_))
                ),
                "LAN は Origin が allowlist でも断る ip={ip}"
            );
        }
    }

    /// Origin 無し = 非ブラウザは通す。あれば vp-app と loopback host の http(s) だけ。
    #[test]
    fn admit_allows_only_vp_app_and_loopback_origins() {
        let lo = peer("127.0.0.1");
        assert_eq!(admit(lo, None), Ok(()));
        for ok in [
            VP_APP_ORIGIN,
            "http://localhost:5173",
            "http://localhost",
            "https://localhost:4443",
            "http://127.0.0.1:3000",
            "http://[::1]:5173",
        ] {
            assert_eq!(admit(lo, Some(ok)), Ok(()), "origin={ok}");
        }
        for bad in [
            "null",
            "https://evil.example",
            "http://localhost.evil.example:5173",
            "http://127.0.0.1.nip.io",
            "vp-asset://evil",
            "http://localhost/path",
            "ws://localhost:5173",
            "",
        ] {
            assert!(
                matches!(admit(lo, Some(bad)), Err(Rejection::OriginNotAllowed(_))),
                "origin={bad:?} は断る"
            );
        }
    }

    // ---------- wire 型 ----------

    #[test]
    fn inbound_parses_every_wire_message_and_rejects_strangers() {
        let hello: Inbound =
            serde_json::from_str(r#"{"t":"hello","v":1,"app":"creo-ui","surface":"abc"}"#)
                .expect("hello");
        assert_eq!(
            hello,
            Inbound::Hello {
                v: 1,
                app: "creo-ui".into(),
                surface: "abc".into()
            }
        );
        assert_eq!(
            serde_json::from_str::<Inbound>(r#"{"t":"mode","mode":"on"}"#).expect("mode"),
            Inbound::Mode { mode: Mode::On }
        );
        assert_eq!(
            serde_json::from_str::<Inbound>(r#"{"t":"selection","selection":null}"#)
                .expect("selection null"),
            Inbound::Selection { selection: None }
        );
        assert_eq!(
            serde_json::from_str::<Inbound>(r#"{"t":"set","id":"sb.text.base","value":1.75}"#)
                .expect("set"),
            Inbound::Set {
                id: "sb.text.base".into(),
                value: json!(1.75)
            }
        );
        assert!(
            serde_json::from_str::<Inbound>(r#"{"t":"get"}"#).is_err(),
            "get は hub → 面 だけ"
        );
        assert!(serde_json::from_str::<Inbound>(r#"{"t":"mode","mode":"maybe"}"#).is_err());
        assert!(serde_json::from_str::<Inbound>(r#"{"hello":1}"#).is_err());
    }

    /// 出す側は creo-ui `parseInbound` が受ける形そのまま（`t` tag、値は型のまま = 文字列化しない）。
    #[test]
    fn outbound_serializes_in_the_shape_parse_inbound_accepts() {
        assert_eq!(
            serde_json::to_value(Outbound::Set {
                id: "sb.text.base".into(),
                value: json!(1.75)
            })
            .expect("set"),
            json!({"t":"set","id":"sb.text.base","value":1.75})
        );
        assert_eq!(
            serde_json::to_value(Outbound::Mode { mode: Mode::Off }).expect("mode"),
            json!({"t":"mode","mode":"off"})
        );
        assert_eq!(
            serde_json::to_value(Outbound::Get).expect("get"),
            json!({"t":"get"})
        );
    }

    // ---------- registry と調停 ----------

    fn hello(
        hub: &LinkHub,
        app: &str,
        surface: &str,
    ) -> (Registration, mpsc::UnboundedReceiver<Outbound>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let reg = hub
            .connect(LINK_VERSION, app.into(), surface.into(), tx)
            .expect("connect");
        (reg, rx)
    }

    fn number_field(id: &str, min: f64, max: f64) -> Value {
        json!({"id": id, "label": id, "type": "number", "semantic": "size",
               "constraints": {"min": min, "max": max, "step": 0.25, "unit": "rem"}, "initial": 1.0})
    }

    #[test]
    fn connect_rejects_other_wire_versions() {
        let hub = LinkHub::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        assert_eq!(
            hub.connect(2, "x".into(), "s".into(), tx).err(),
            Some(LinkError::UnsupportedVersion(2))
        );
        assert!(hub.surfaces().is_empty());
    }

    /// active = 今 ON の面が最優先、次に最後に ON になった面、どれも ON になっていなければ最後に繋いだ面。
    #[test]
    fn active_prefers_the_last_surface_that_turned_editor_on() {
        let hub = LinkHub::new();
        let (a, _ra) = hello(&hub, "creo-ui", "tab-a");
        std::thread::sleep(Duration::from_millis(2));
        let (b, _rb) = hello(&hub, "Vantage Point", "vp-app#0");
        // 誰も ON していない → 最後に繋いだ b
        assert_eq!(hub.active_surface().as_deref(), Some("vp-app#0"));
        hub.apply(&a, Inbound::Mode { mode: Mode::On });
        assert_eq!(
            hub.active_surface().as_deref(),
            Some("tab-a"),
            "ON した面が勝つ"
        );
        std::thread::sleep(Duration::from_millis(2));
        hub.apply(&b, Inbound::Mode { mode: Mode::On });
        assert_eq!(
            hub.active_surface().as_deref(),
            Some("vp-app#0"),
            "最後に ON した面"
        );
        hub.apply(&b, Inbound::Mode { mode: Mode::Off });
        assert_eq!(
            hub.active_surface().as_deref(),
            Some("tab-a"),
            "OFF になれば今 ON の面へ戻る"
        );
        hub.disconnect(&a);
        assert_eq!(
            hub.active_surface().as_deref(),
            Some("vp-app#0"),
            "切断で次へ"
        );
        hub.disconnect(&b);
        assert_eq!(hub.active_surface(), None);
        assert_eq!(hub.resolve(None), Err(LinkError::NoSurface));
    }

    /// 同じ surface id の再接続は置き換え。古い接続の disconnect / apply は新しい登録を壊さない。
    #[test]
    fn reconnect_with_same_surface_id_replaces_and_old_connection_is_inert() {
        let hub = LinkHub::new();
        let (old, _r1) = hello(&hub, "creo-ui", "tab-a");
        hub.apply(
            &old,
            Inbound::Fields {
                fields: vec![number_field("x", 0.0, 1.0)],
            },
        );
        let (new, _r2) = hello(&hub, "creo-ui", "tab-a");
        assert_ne!(old.conn_id, new.conn_id);
        hub.apply(&old, Inbound::Mode { mode: Mode::On }); // 古い接続からの frame は無視
        hub.disconnect(&old); // 古い接続の後始末は新しい登録を消さない
        let infos = hub.surfaces();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].mode, Mode::Off);
        assert_eq!(infos[0].fields, 0, "fields は新しい接続の分（まだ空）");
        hub.disconnect(&new);
        assert!(hub.surfaces().is_empty());
    }

    #[test]
    fn resolve_accepts_surface_id_or_app_label_case_insensitively() {
        let hub = LinkHub::new();
        let (_a, _ra) = hello(&hub, "creo-ui", "tab-a");
        let (_b, _rb) = hello(&hub, "Vantage Point", "vp-app#0");
        assert_eq!(hub.resolve(Some("tab-a")).as_deref(), Ok("tab-a"));
        assert_eq!(
            hub.resolve(Some("vantage point")).as_deref(),
            Ok("vp-app#0")
        );
        assert_eq!(hub.resolve(Some(" creo-ui ")).as_deref(), Ok("tab-a"));
        match hub.resolve(Some("figma")) {
            Err(LinkError::UnknownSurface { target, available }) => {
                assert_eq!(target, "figma");
                assert_eq!(
                    available,
                    vec!["tab-a (creo-ui)", "vp-app#0 (Vantage Point)"]
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// set は constraints に収めて送り、cache にも clamp 後の値を書く。知らない id は送らずに言う。
    #[test]
    fn set_clamps_numbers_to_constraints_and_updates_cache() {
        let hub = LinkHub::new();
        let (a, mut rx) = hello(&hub, "creo-ui", "tab-a");
        hub.apply(
            &a,
            Inbound::Fields {
                fields: vec![
                    number_field("sb.text.base", 0.5, 2.0),
                    json!({"id": "theme.brand", "label": "Brand", "type": "color", "semantic": "color", "initial": "#000000"}),
                ],
            },
        );
        let out = hub.set(None, "sb.text.base", json!(9)).expect("set");
        assert_eq!(out.value, json!(2.0));
        assert!(out.clamped);
        assert_eq!(
            rx.try_recv().expect("sent"),
            Outbound::Set {
                id: "sb.text.base".into(),
                value: json!(2.0)
            }
        );
        let out = hub
            .set(None, "sb.text.base", json!(1.25))
            .expect("set in range");
        assert_eq!(out.value, json!(1.25));
        assert!(!out.clamped);
        let out = hub
            .set(None, "theme.brand", json!("#FF4A2D"))
            .expect("color passes through");
        assert_eq!(out.value, json!("#FF4A2D"));
        assert!(!out.clamped);
        assert_eq!(
            hub.set(None, "nope", json!(1)).err(),
            Some(LinkError::UnknownField {
                surface: "tab-a".into(),
                id: "nope".into()
            })
        );
        assert_eq!(
            rx.try_recv().ok(),
            Some(Outbound::Set {
                id: "sb.text.base".into(),
                value: json!(1.25)
            })
        );
        assert_eq!(
            rx.try_recv().ok(),
            Some(Outbound::Set {
                id: "theme.brand".into(),
                value: json!("#FF4A2D")
            })
        );
        assert!(rx.try_recv().is_err(), "知らない id は面に送らない");
    }

    #[test]
    fn clamp_touches_only_numbers_with_bounds() {
        let f = number_field("x", 0.0, 10.0);
        assert_eq!(clamp_to_constraints(&f, json!(-1)), (json!(0.0), true));
        assert_eq!(clamp_to_constraints(&f, json!(5)), (json!(5), false));
        assert_eq!(
            clamp_to_constraints(&f, json!("5")),
            (json!("5"), false),
            "文字列は触らない（型の責任は呼び手）"
        );
        let unbounded = json!({"id": "y", "type": "number"});
        assert_eq!(
            clamp_to_constraints(&unbounded, json!(1e9)),
            (json!(1e9), false)
        );
        let only_min = json!({"id": "z", "type": "number", "constraints": {"min": 1}});
        assert_eq!(
            clamp_to_constraints(&only_min, json!(0.5)),
            (json!(1.0), true)
        );
    }

    /// snapshot は `get` を送り、面の `values` が届いた後の cache を返す。
    #[tokio::test]
    async fn snapshot_sends_get_and_returns_after_values_arrive() {
        let hub = std::sync::Arc::new(LinkHub::new());
        let (a, mut rx) = hello(&hub, "creo-ui", "tab-a");
        hub.apply(&a, Inbound::Mode { mode: Mode::On });
        let hub2 = hub.clone();
        let reg = a.clone();
        // 面の代役: get を受けたら fields → selection → values を返す
        let face = tokio::spawn(async move {
            let msg = rx.recv().await.expect("get");
            assert_eq!(msg, Outbound::Get);
            hub2.apply(
                &reg,
                Inbound::Fields {
                    fields: vec![number_field("sb.text.base", 0.5, 2.0)],
                },
            );
            hub2.apply(
                &reg,
                Inbound::Selection {
                    selection: Some(json!({"targetId": "sb", "fieldIds": ["sb.text.base"]})),
                },
            );
            hub2.apply(
                &reg,
                Inbound::Values {
                    values: serde_json::from_value(json!({"sb.text.base": 1.0})).expect("map"),
                },
            );
        });
        let snap = hub
            .snapshot(None, Duration::from_secs(2))
            .await
            .expect("snapshot");
        face.await.expect("face");
        assert_eq!(snap.surface, "tab-a");
        assert_eq!(snap.app, "creo-ui");
        assert_eq!(snap.mode, Mode::On);
        assert_eq!(snap.fields.len(), 1);
        assert_eq!(snap.values.get("sb.text.base"), Some(&json!(1.0)));
        assert!(snap.selection.is_some());
    }

    /// 面が答えなければ timeout を言う（lock を抱えて固まらない）。
    #[tokio::test]
    async fn snapshot_times_out_when_the_surface_stays_silent() {
        let hub = LinkHub::new();
        let (_a, _rx) = hello(&hub, "creo-ui", "tab-a");
        let err = hub.snapshot(None, Duration::from_millis(30)).await.err();
        assert_eq!(
            err,
            Some(LinkError::Timeout {
                surface: "tab-a".into()
            })
        );
        assert_eq!(hub.surfaces().len(), 1, "timeout しても面は残る");
    }

    #[tokio::test]
    async fn snapshot_reports_a_surface_that_vanished_mid_wait() {
        let hub = std::sync::Arc::new(LinkHub::new());
        let (a, _rx) = hello(&hub, "creo-ui", "tab-a");
        let hub2 = hub.clone();
        let gone = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            hub2.disconnect(&a);
        });
        let err = hub.snapshot(None, Duration::from_secs(2)).await.err();
        gone.await.expect("gone");
        assert_eq!(err, Some(LinkError::SurfaceGone("tab-a".into())));
    }

    // ---------- channel dispatch ----------

    #[tokio::test]
    async fn link_channel_dispatches_methods_and_names_missing_arguments() {
        let hub = LinkHub::new();
        assert_eq!(
            handle_link_channel(&hub, "surfaces", json!({})).await,
            Ok(json!({"surfaces": [], "active": null}))
        );
        assert_eq!(
            handle_link_channel(&hub, "editor_set", json!({"value": 1})).await,
            Err("editor_set: id 必須".to_string())
        );
        assert_eq!(
            handle_link_channel(&hub, "editor_set", json!({"id": "x"})).await,
            Err("editor_set: value 必須".to_string())
        );
        let err = handle_link_channel(&hub, "editor_set", json!({"id": "x", "value": 1}))
            .await
            .expect_err("no surface");
        assert!(err.contains("no editor surface is linked"), "{err}");
        assert_eq!(
            handle_link_channel(&hub, "editor_mode", json!({})).await,
            Err("unknown link method: editor_mode".to_string())
        );
        let (a, mut rx) = hello(&hub, "creo-ui", "tab-a");
        hub.apply(
            &a,
            Inbound::Fields {
                fields: vec![number_field("x", 0.0, 1.0)],
            },
        );
        let ok = handle_link_channel(
            &hub,
            "editor_set",
            json!({"surface": "creo-ui", "id": "x", "value": 3}),
        )
        .await
        .expect("set");
        assert_eq!(
            ok,
            json!({"ok": true, "surface": "tab-a", "id": "x", "value": 1.0, "clamped": true})
        );
        assert_eq!(
            rx.try_recv().ok(),
            Some(Outbound::Set {
                id: "x".into(),
                value: json!(1.0)
            })
        );
        let listed = handle_link_channel(&hub, "surfaces", json!({}))
            .await
            .expect("surfaces");
        assert_eq!(listed["active"], json!("tab-a"));
        assert_eq!(listed["surfaces"][0]["fields"], json!(1));
    }

    #[test]
    fn health_distinguishes_idle_from_connected() {
        let hub = LinkHub::new();
        let (status, detail) = hub.health();
        assert_eq!(status, "idle");
        assert_eq!(detail, json!({"surfaces": [], "active": null}));
        let (a, _rx) = hello(&hub, "Vantage Point", "vp-app#0");
        hub.apply(&a, Inbound::Mode { mode: Mode::On });
        let (status, detail) = hub.health();
        assert_eq!(status, "connected");
        assert_eq!(detail["active"], json!("Vantage Point"));
        assert_eq!(detail["surfaces"][0]["surface"], json!("vp-app#0"));
        assert_eq!(detail["surfaces"][0]["mode"], json!("on"));
    }
}
