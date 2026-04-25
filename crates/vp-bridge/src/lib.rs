//! VP Bridge — Cell グリッド NSView Backend + FFI
//!
//! alacritty grid の Cell を独自表現に変換し、C ABI 経由で Swift/NSView に公開する
//! ブリッジ。NativeBackend が描画結果を内部バッファに蓄積、Swift 側は FFI 関数で
//! セルデータを読み取り Core Text で描画する。
//!
//! ratatui crate からの脱却 (2026-04-25): TUI Widget/Layout 機能を一切使って
//! いなかったため、ratatui::buffer::Buffer / Cell / Style 依存を独自 `types` で
//! 置換し、依存削減 + wide cell の実 2 cell layout 拡張余地を確保。

pub mod backend;
pub mod ffi;
pub mod pty;
pub mod types;
