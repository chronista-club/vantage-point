//! `vp events` — event log（agent の episodic memory、rebuild Epic L2 / doc 27 §5-3）
//!
//! - `vp events [--since N] [--limit N] [--json]` — log を since cursor 以降で表示（古い順）
//! - `vp events emit --kind K [--source S] [--data JSON]` — event を 1 件 push
//!
//! agent（Claude）は見た最大 seq を記録し、次回 `--since <それ>` で「前回の action から今までに
//! 何が起きたか」だけを取る → 行動間 blind の解消。World :32000 の "events" channel に直結する。

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::daemon::client::DaemonClient;

/// `vp events` の引数（query flags + 任意 `emit` subcommand）。
#[derive(Args, Debug)]
pub struct EventsArgs {
    #[command(subcommand)]
    pub action: Option<EventsAction>,
    /// この cursor(seq) より新しい event だけ表示（0 = ring 全件）。
    #[arg(long, default_value_t = 0)]
    pub since: u64,
    /// 最大表示件数（0 = 無制限）。
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
    /// JSON Lines で出力（agent / script 向け）。
    #[arg(long)]
    pub json: bool,
}

/// `vp events` の subcommand（無指定なら query）。
#[derive(Subcommand, Debug)]
pub enum EventsAction {
    /// event を 1 件 push する（build task / claude hook / agent から）。
    Emit {
        /// 種別（dotted、例: `build.done` / `test.fail`）。
        #[arg(long)]
        kind: String,
        /// 発生元（任意、例: `echoes@vantage-point/conductor`）。
        #[arg(long)]
        source: Option<String>,
        /// payload JSON（省略時 null）。
        #[arg(long)]
        data: Option<String>,
    },
}

/// `vp events` を実行する。
pub async fn run(args: EventsArgs) -> Result<()> {
    let client = DaemonClient::connect(crate::cli::world_port(), 3)
        .await
        .map_err(|e| {
            anyhow::anyhow!("TheWorld 接続失敗: {} (= `vp daemon start` で起動済か?)", e)
        })?;

    match args.action {
        // emit: event を 1 件 push。
        Some(EventsAction::Emit { kind, source, data }) => {
            let data_val = match data {
                Some(s) => serde_json::from_str(&s)
                    .with_context(|| format!("--data が不正な JSON: {}", s))?,
                None => serde_json::Value::Null,
            };
            let seq = client
                .events_emit(&kind, source.as_deref(), data_val)
                .await?;
            println!("📌 event emitted (seq={}, kind={})", seq, kind);
        }
        // query（無指定）: since cursor 以降を古い順に表示。
        None => {
            let events = client.events_query(args.since, args.limit).await?;
            if args.json {
                for e in &events {
                    println!("{}", serde_json::to_string(e)?);
                }
            } else if events.is_empty() {
                println!("(event なし — since={})", args.since);
            } else {
                for e in &events {
                    let src = e
                        .source
                        .as_deref()
                        .map(|s| format!(" <{}>", s))
                        .unwrap_or_default();
                    let data = if e.data.is_null() {
                        String::new()
                    } else {
                        format!(" {}", e.data)
                    };
                    println!("[{}] {} {}{}{}", e.seq, e.ts, e.kind, src, data);
                }
            }
        }
    }
    Ok(())
}
