//! SP → TheWorld の wire/delegation transport (L0 portless B-4: unison "wire" channel)
//!
//! wire store / delegation store は TheWorld (daemon QUIC、 port 32000、 config override 可) に
//! 中央化されている。 SP の wire ハンドラ ([`crate::process::unison_server`]) / actor
//! (notify / lane-spawn) / delegation ([`crate::process::delegation`]) はこの client 経由で
//! 中央 store を読み書きする。
//!
//! ## B-4: transport を HTTP → unison channel に移行 (doc 27 §62「全通信 unison channel」)
//!
//! 旧 `POST /api/wire/*` `/api/delegation/*` (reqwest) を daemon の **"wire" unison channel** に置換。
//! `call` のシグネチャ (`path` / payload / `Result<Value, String>`) と error 規約は不変なので、
//! 呼び出し側 (handle_wire_* / delegation / actor) は無改造。 path の `/api/` prefix を剥いだ残り
//! (= `"wire/send"` / `"delegation/create"`) を channel method として投げ、 daemon 側
//! ([`crate::daemon::server::handle_wire_channel`]) が wire / delegation の store dispatch に振る。
//!
//! ## 接続は SP 1 本の永続 QUIC を再利用する (per-call 新造は fd leak)
//!
//! 旧実装は `call()` ごとに QuicClient を新造して drop 任せにしていたが、 unison の
//! `connect()` が spawn する accept_bi task が Connection clone を保持し続けるため、 client を
//! drop しても quinn endpoint driver (= UDP socket) は解放されない。 **1 call = 1 fd leak** となり、
//! SP 常駐 actor の wire/recv long-poll (≤25s) により約 100 分で RLIMIT_NOFILE=256 に到達、
//! wire mesh 全体が EMFILE で死んでいた (mem_1Ccbou9kzfGtJsLbUrRX8b)。
//! 現実装は lazy connect した 1 接続を cache し、 切断検知 / outer timeout で破棄 → 次の call が
//! 再接続する (registry uplink ([`crate::discovery`] の `WorldUplink`) と同型の epoch 再接続)。
//!
//! TheWorld 停止 = wire 停止 (設計決定 D1-c で許容済、 実運用は既に事実上依存)。
//! 呼び出し側は Err を受けて各自の方針で扱う:
//! - proxy (handle_wire_*): エラーをそのまま caller に返す
//! - actor (notify / lane-spawn): retry loop で TheWorld 復帰を待つ

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

/// connect → handshake → request 全体の outer timeout。
///
/// wire_recv は server 側で long-poll する (各 caller は ≤25s に抑える規約 — unison 内部 request
/// timeout 30s 下に収める)。 outer を 40s 取り、 TheWorld が wedge した場合の無言ハングを防ぐ
/// (= unison 内部 30s が先に発火する safety net)。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(40);

/// 永続 wire 接続 (client + "wire" channel のペア)。
///
/// client は QUIC connection の所有権として保持する。 破棄は必ず `disconnect()` 経由で行う —
/// drop 任せでは UDP socket が解放されない (module doc 参照)。
struct WireLink {
    client: unison::ProtocolClient,
    channel: unison::UnisonChannel,
}

/// SP 1 プロセス = TheWorld への wire 接続 1 本 (lazy connect、 切断検知で再接続)。
static WIRE_LINK: Mutex<Option<Arc<WireLink>>> = Mutex::const_new(None);

/// TheWorld の port を解決する (config override → default 32000)
pub(crate) fn world_port() -> u16 {
    crate::config::Config::load()
        .map(|c| c.port_layout().world_port)
        .unwrap_or(crate::cli::world_port())
}

/// 生きた cache 接続を返すか、 無ければ接続して cache する。
///
/// lock は接続確立まで保持する (= 同時 reconnect の thundering herd 防止)。 request 自体は
/// lock 外で行うため、 long-poll が他の call を塞ぐことはない (UnisonChannel は message_id で
/// 多重化されており、 同一 channel 上の並行 request は安全)。
async fn get_or_connect(addr: &str) -> Result<Arc<WireLink>, String> {
    let mut guard = WIRE_LINK.lock().await;
    if let Some(link) = guard.as_ref() {
        if link.client.is_connected().await {
            return Ok(Arc::clone(link));
        }
        // 死んだ接続は disconnect で背後の endpoint / accept_bi task を確実に解放してから作り直す
        if let Some(stale) = guard.take() {
            let _ = stale.client.disconnect().await;
        }
    }
    let transport = unison::network::quic::QuicClient::builder()
        .trust_anchors(unison::network::TrustAnchors::SkipVerification)
        .build()
        .map_err(|e| format!("QUIC client build failed: {e}"))?;
    let client = unison::ProtocolClient::new(transport);
    client.connect(addr).await.map_err(|e| {
        format!(
            "TheWorld unreachable ({addr}): {e} — wire/delegation store は TheWorld に \
             中央化されています。 `vp daemon start` を確認してください"
        )
    })?;
    let channel = match client.open_channel("wire").await {
        Ok(ch) => ch,
        Err(e) => {
            // connection は確立済みなので、 leak しないよう disconnect してから Err を返す
            let _ = client.disconnect().await;
            return Err(format!("open wire channel 失敗 ({addr}): {e}"));
        }
    };
    let link = Arc::new(WireLink { client, channel });
    *guard = Some(Arc::clone(&link));
    Ok(link)
}

/// cache 中の接続が `link` と同一なら破棄する (request 失敗時の self-heal)。
///
/// `Arc::ptr_eq` で「自分が使った接続」だけを落とす — 他 task が既に張り直した新しい接続を
/// 巻き添えにしない。
async fn invalidate(link: &Arc<WireLink>) {
    let mut guard = WIRE_LINK.lock().await;
    if guard.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, link))
        && let Some(stale) = guard.take()
    {
        let _ = stale.client.disconnect().await;
    }
}

/// cache 中の接続を無条件破棄する (outer timeout 発火時の self-heal)。
async fn invalidate_current() {
    let mut guard = WIRE_LINK.lock().await;
    if let Some(stale) = guard.take() {
        let _ = stale.client.disconnect().await;
    }
}

/// TheWorld の wire/delegation API を呼ぶ。 `path` は `/api/wire/send` / `/api/delegation/create` 等。
///
/// path から `/api/` prefix を剥いだ残り (= `"wire/send"` / `"delegation/create"`) を unison "wire"
/// channel の method として TheWorld daemon (QUIC) に request する。
///
/// エラー規約 (HTTP 版と不変):
/// - transport 失敗 → `Err("TheWorld unreachable ...")`
/// - 応答 JSON に `error` field → その内容を Err として relay
///   (TheWorld 側 channel handler は server Err を success frame の `{"error": <msg>}` で返す)
pub(crate) async fn call(
    path: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    // QUIC(rustls) は CryptoProvider の install が前提 (install 済みなら no-op)。
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    // path "/api/wire/send" → method "wire/send"。 prefix が無ければ path をそのまま使う (防御的)。
    let method = path.strip_prefix("/api/").unwrap_or(path);
    let addr = format!("[::1]:{}", world_port());

    let work = async {
        let link = get_or_connect(&addr).await?;
        match link
            .channel
            .request::<serde_json::Value, serde_json::Value>(method, &payload)
            .await
        {
            Ok(resp) => {
                // unison は専用 error frame を持たず、 server Err を success frame に {"error":..}
                // で詰める (= 旧 HTTP handler の `{status:error, error}` と同じく、 ここで Err に変換)。
                if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
                    return Err(err.to_string());
                }
                Ok::<serde_json::Value, String>(resp)
            }
            Err(e) => {
                // 接続が死んでいる時だけ破棄 (次の call が再接続)。 unison 内部 timeout 等の
                // app 層エラーで生きている接続まで落とさない。
                if !link.client.is_connected().await {
                    invalidate(&link).await;
                }
                Err(format!("wire channel request ({method}) 失敗: {e}"))
            }
        }
    };
    match tokio::time::timeout(REQUEST_TIMEOUT, work).await {
        Ok(result) => result,
        Err(_) => {
            // 40s 無応答 = 接続 wedge の疑い。 破棄して次の call を fresh connect に倒す
            // (旧 per-call 実装が持っていた「毎回 fresh」の self-heal と等価)。
            invalidate_current().await;
            Err(format!(
                "TheWorld ({addr}) wire channel ({method}) が {}s 以内に応答しませんでした",
                REQUEST_TIMEOUT.as_secs()
            ))
        }
    }
}
