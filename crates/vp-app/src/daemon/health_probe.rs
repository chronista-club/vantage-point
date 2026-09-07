//! daemon の HTTP 面 (`/api/health`) — Unison が壊れた時に動く診断用 probe。
//!
//! ## doc 45 段 3 — 残っているのは health だけ
//!
//! 元は repos / processes / lanes を触る REST client (12 method) だったが、
//! control plane は Unison に寄せた ([`crate::daemon::control`])。ここに残るのは
//! **`/api/health` 1 本**で、これは統一の取りこぼしではなく doc 45 §2 の設計判断:
//! health は「他が壊れている時に動いてほしい」probe なので、Unison 層が wedge した時に
//! 診断手段ごと失わないよう、意図的に鈍い外殻 (HTTP) として置く。
//! `daemon::launcher` の起動待ちも同じ probe を叩く。
//! （旧 `client.rs` の `DaemonRpcClient`。wire 型は `crate::daemon_wire` へ、6-0b で分解、2026-09-08）
//!
//! ## URL 解決
//! 1. `VP_DAEMON_URL` env var があれば優先 (例: `http://172.20.78.253:32000`)
//! 2. それ以外は `http://127.0.0.1:32000` (IPv4 loopback)
//!
//! **IPv6 `[::1]` は WSL2 → Windows の localhost 転送で通らない**ため
//! デフォルトは IPv4。WSL2 側で daemon を立ち上げて Windows の
//! vp-app から接続するケースを前提にしている。

use anyhow::Result;

use crate::daemon::default_daemon_port;
use crate::daemon_wire::DaemonHealthInfo;

/// デフォルト URL 解決
///
/// `VP_DAEMON_URL` env var → `http://127.0.0.1:{default_daemon_port()}`
fn default_base_url() -> String {
    std::env::var("VP_DAEMON_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", default_daemon_port()))
}

/// daemon の HTTP health probe (`/api/health` 専用)
///
/// doc 45 段 3 以降、control plane は [`crate::daemon::control::DaemonControl`] が持つ。
pub struct HealthProbe {
    base_url: String,
    client: reqwest::Client,
}

impl HealthProbe {
    // doc 45 段 3: `new(port)` は撤去した。 port 指定で HTTP を叩いていたのは control plane の
    // 呼び出し元だけで、 それらは Unison に移った (daemon port は共有 connection manager が持つ)。
    // 残る唯一の caller は `Default` (= `VP_DAEMON_URL` / profile 既定の解決) なので、
    // 使われない ctor を「いつか誰か使う」で残さない。

    /// 任意の base URL で作成 (env var override / テスト用)
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
        }
    }

    /// `/api/health` の中身を取得 (Activity widget 用)
    ///
    /// doc 45 §2: health は Unison に寄せない — 「他が壊れている時に動いてほしい」probe を
    /// Unison に載せると、Unison 層が wedge した時に診断手段ごと失う。
    /// `.mise/tasks/app/swap` (Ruby) や Swift menu bar agent も同じ endpoint を叩いており、
    /// それらに Unison client を持たせる理由もない。
    pub async fn daemon_health(&self) -> Result<DaemonHealthInfo> {
        let url = format!("{}/api/health", self.base_url);
        let info: DaemonHealthInfo = self.client.get(&url).send().await?.json().await?;
        Ok(info)
    }
}

impl Default for HealthProbe {
    fn default() -> Self {
        Self::with_base_url(default_base_url())
    }
}
