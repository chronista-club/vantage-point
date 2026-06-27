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
//! TheWorld 停止 = wire 停止 (設計決定 D1-c で許容済、 実運用は既に事実上依存)。
//! 呼び出し側は Err を受けて各自の方針で扱う:
//! - proxy (handle_wire_*): エラーをそのまま caller に返す
//! - actor (notify / lane-spawn): retry loop で TheWorld 復帰を待つ

use std::time::Duration;

/// connect → handshake → request 全体の outer timeout。
///
/// wire_recv は server 側で long-poll する (各 caller は ≤25s に抑える規約 — unison 内部 request
/// timeout 30s 下に収める)。 outer を 40s 取り、 TheWorld が wedge した場合の無言ハングを防ぐ
/// (= unison 内部 30s が先に発火する safety net)。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(40);

/// TheWorld の port を解決する (config override → default 32000)
pub(crate) fn world_port() -> u16 {
    crate::config::Config::load()
        .map(|c| c.port_layout().world_port)
        .unwrap_or(crate::cli::WORLD_PORT)
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
        let transport = unison::network::quic::QuicClient::builder()
            .trust_anchors(unison::network::TrustAnchors::SkipVerification)
            .build()
            .map_err(|e| format!("QUIC client build failed: {e}"))?;
        let client = unison::ProtocolClient::new(transport);
        client.connect(&addr).await.map_err(|e| {
            format!(
                "TheWorld unreachable ({addr}): {e} — wire/delegation store は TheWorld に \
                 中央化されています。 `vp daemon start` を確認してください"
            )
        })?;
        let channel = client
            .open_channel("wire")
            .await
            .map_err(|e| format!("open wire channel 失敗 ({addr}): {e}"))?;
        let resp: serde_json::Value = channel
            .request::<serde_json::Value, serde_json::Value>(method, &payload)
            .await
            .map_err(|e| format!("wire channel request ({method}) 失敗: {e}"))?;
        // unison は専用 error frame を持たず、 server Err を success frame に {"error":..} で詰める
        // (= 旧 HTTP handler の `{status:error, error}` と同じく、 ここで Err に変換)。
        if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
            return Err(err.to_string());
        }
        Ok::<serde_json::Value, String>(resp)
    };
    match tokio::time::timeout(REQUEST_TIMEOUT, work).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "TheWorld ({addr}) wire channel ({method}) が {}s 以内に応答しませんでした",
            REQUEST_TIMEOUT.as_secs()
        )),
    }
}
