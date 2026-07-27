//! Vantage Point Core — AI ネイティブ開発環境のコアライブラリ
//!
//! CLI バイナリ (`vp`) や外部クレートから利用される
//! Process サーバー、MCP、Daemon 等のコアロジックを提供する。

pub mod agent;
pub mod agui;
pub mod capability;
pub mod cli;
pub mod commands;
pub mod config;
pub mod creo;
pub mod daemon;
pub mod db;
#[cfg(feature = "midi")]
pub mod devices;
// device_input / device_profile / roto_palette は midistage-profiles に切り出し済み。
// 既存の crate::device_input::* 等の参照パスを保つため再エクスポートする。
#[cfg(feature = "midi")]
pub use midistage_profiles::device_input;
#[cfg(feature = "midi")]
pub use midistage_profiles::device_profile;
pub mod conversation;
#[cfg(feature = "midi")]
pub mod device_io;
pub mod discovery;
pub mod file_watcher;
pub mod flow;
/// Repo Host — repo の面倒を見る決定的サービス（doc 44 D3 / §7）
pub mod host;
// lane lib 本体 (vp-cli の bin `vp lane` も `vantage_point::lane` を経由する)
pub mod lane;
pub mod mcp;
#[cfg(feature = "midi")]
pub mod midi;
pub mod panic_hook;
pub mod platform;
pub mod port_layout;
pub mod protocol;
pub mod repo;
pub mod repos_file;
pub mod resolve;
#[cfg(feature = "midi")]
pub use midistage_profiles::roto_palette;
pub mod daemon_client;
pub mod node;
pub mod screenshot;
pub mod spawn_env;
pub mod stands;
pub mod terminal;
/// test 専用: process-global env (`XDG_STATE_HOME`) を触る test の直列化 + RAII 復元。
#[cfg(test)]
pub(crate) mod test_env;
pub mod trace_log;
