//! vp-app バイナリエントリポイント
//!
//! ## Windows subsystem
//!
//! Windows では **GUI subsystem** (`#![windows_subsystem = "windows"]`) で build する。
//! debug / release 共通。理由:
//!
//! - 起動時に console window が allocate されない (vp-app は GUI app なので不要)
//! - PowerShell の `Start-Process -RedirectStandardOutput/Error` で stdout/stderr を
//!   redirect すると、それが正しく file に向かう (console subsystem だと新 console
//!   が allocate されてそちらに output が向き、redirect 失敗していた)
//! - mise run win の polling tail がログを見るには redirect 経由が必須
//!
//! debug build でログ直見したい場合は cmd.exe から起動 or `mise run win:logs`。
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // rustls の process-level CryptoProvider を最初に install する。
    // wiremsg Stage 1 consumer の QUIC 購読 (unison) が rustls 0.23+ を使うため、
    // 最初の QUIC 接続より前に必須。未 install だと rustls が panic し、
    // `lanes-sub-*` スレッドが silent death して sidebar に Lane が出なくなる。
    // vp-cli / vantage-point と同じ aws-lc-rs provider。
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    vp_app::app::run()
}
