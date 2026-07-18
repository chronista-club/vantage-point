//! Vantage Point Core — AI ネイティブ開発環境のコアライブラリ
//!
//! CLI バイナリ (`vp`) や外部クレートから利用される
//! Process サーバー、MCP、Daemon 等のコアロジックを提供する。

pub mod agent;
pub mod agui;
#[cfg(feature = "midi")]
pub mod bastet;
pub mod capability;
pub mod cli;
pub mod commands;
pub mod config;
pub mod creo;
pub mod daemon;
pub mod db;
// device_input / device_profile / roto_palette は midistage-profiles に切り出し済み。
// 既存の crate::device_input::* 等の参照パスを保つため再エクスポートする。
#[cfg(feature = "midi")]
pub use midistage_profiles::device_input;
#[cfg(feature = "midi")]
pub use midistage_profiles::device_profile;
pub mod discovery;
pub mod echoes;
pub mod file_watcher;
pub mod flow;
#[cfg(feature = "midi")]
pub mod justice;
// lane lib 本体 (vp-cli の bin `vp lane` も `vantage_point::lane` を経由する)
pub mod lane;
pub mod mcp;
#[cfg(feature = "midi")]
pub mod midi;
pub mod platform;
pub mod port_layout;
pub mod process;
pub mod projects_file;
pub mod protocol;
pub mod resolve;
#[cfg(feature = "midi")]
pub use midistage_profiles::roto_palette;
pub mod screenshot;
pub mod spawn_env;
pub mod stands;
pub mod terminal;
/// test 専用: process-global env (`XDG_STATE_HOME`) を触る test の直列化 + RAII 復元。
#[cfg(test)]
pub(crate) mod test_env;
pub mod trace_log;
pub mod world;
pub mod world_client;
