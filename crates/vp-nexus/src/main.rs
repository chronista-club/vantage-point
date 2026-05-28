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

    let args = Args::parse();
    let host = args.resolve_host();
    let port = args.resolve_port()?;
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("invalid listen address: {host}:{port}"))?;

    let app = vp_nexus::app();

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
