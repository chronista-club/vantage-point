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
    vp_app::app::run()
}
