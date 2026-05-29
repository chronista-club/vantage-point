//! `nexus` binary entry point。
//!
//! 起動時の責務は parse + bind + serve のみ。 router 構築 / handler は
//! lib 側 ([`vp_nexus::app`]) に寄せている。
//!
//! ## 設定
//!
//! - `--port` / env `NEXUS_PORT` (default: 9200)
//! - `--host` / env `NEXUS_HOST` (default: 127.0.0.1)
//! - log は `RUST_LOG` で制御 (default: info)

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

const DEFAULT_PORT: u16 = 9200;
const DEFAULT_HOST: &str = "127.0.0.1";

#[derive(Parser, Debug)]
#[command(name = "nexus", version, about = "VP federation hub server")]
struct Args {
    /// listen port (= 指定なき場合 env `NEXUS_PORT`、 さらに無ければ default 9200)
    #[arg(long)]
    port: Option<u16>,

    /// listen host / IP (= 指定なき場合 env `NEXUS_HOST`、 さらに無ければ default 127.0.0.1)
    #[arg(long)]
    host: Option<String>,
}

impl Args {
    /// 解決順: CLI flag → env var → default。
    /// (workspace の `clap` は `env` feature 無効のため、 ここで自前 fallback)
    fn resolve_port(&self) -> Result<u16> {
        if let Some(p) = self.port {
            return Ok(p);
        }
        match std::env::var("NEXUS_PORT") {
            Ok(s) => s
                .parse()
                .with_context(|| format!("invalid NEXUS_PORT: {s}")),
            Err(_) => Ok(DEFAULT_PORT),
        }
    }

    fn resolve_host(&self) -> String {
        self.host
            .clone()
            .or_else(|| std::env::var("NEXUS_HOST").ok())
            .unwrap_or_else(|| DEFAULT_HOST.to_string())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 別 service なので KDL formatter は使わず simple fmt で十分
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // rustls 0.23 の process-level CryptoProvider を install (= QUIC settings server が
    // 使う。 aws-lc-rs と ring が両方 dep graph にあるため明示が必要)。 既 install 時は
    // Err になるが無視 (= 冪等)。 vp-cli/vp-app/vantage-point と同じ慣行。
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args = Args::parse();
    let host = args.resolve_host();
    let port = args.resolve_port()?;
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid listen address: {host}:{port}"))?;

    let app = vp_nexus::app();

    // JWKS を起動時に 1 回 fetch (= HTTP /v1/auth/me の Claims Extractor と
    // settings channel の Authenticate が共に依存する)。 fetch 失敗は warn して
    // 続行 (= JWKS endpoint 復帰まで認証系が 401 を返すだけ、 health 等は生きる)。
    if let Err(e) = vp_nexus::auth::refresh_jwks().await {
        tracing::warn!(error = %e, "JWKS initial fetch failed; auth will 401 until reachable");
    }

    // settings sync + node tree の unison QUIC server を axum HTTP と並走させる
    // (= Phase A3a + dogfood 14)。 QUIC は UDP、 HTTP は TCP で OS の port 名前空間が
    // 独立するため同一 port で共存可能。
    let stores = vp_nexus::settings::NexusStores::new();
    let quic_port = port + vp_nexus::settings::QUIC_PORT_OFFSET;
    let quic_addr = format!("[::]:{quic_port}");
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    // shutdown_tx は process lifetime 中 hold (= drop で graceful shutdown 発火)。
    let (_settings_shutdown_tx, settings_shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if let Err(e) =
            vp_nexus::settings::serve_settings(stores, quic_addr, settings_shutdown_rx, ready_tx)
                .await
        {
            tracing::error!(error = %e, "settings QUIC server terminated");
        }
    });
    match ready_rx.await {
        Ok(Some(quic_local)) => {
            tracing::info!(%quic_local, "settings QUIC listening (= Phase A3a sync)")
        }
        _ => tracing::warn!("settings QUIC server failed to bind"),
    }

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {}", addr))?;

    tracing::info!(
        version = vp_nexus::VERSION,
        %addr,
        "nexus listening (= VP federation hub MVP)"
    );

    axum::serve(listener, app)
        .await
        .context("nexus server terminated with error")?;

    Ok(())
}
