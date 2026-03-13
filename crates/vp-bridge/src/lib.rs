//! VP Bridge — ratatui NSView Backend + FFI
//!
//! ratatui の Cell グリッドを C ABI 経由で Swift/NSView に公開するブリッジ。
//! NativeBackend が ratatui Backend trait を実装し、描画結果を内部バッファに蓄積。
//! Swift 側は FFI 関数でセルデータを読み取り、Core Text で描画する。

pub mod backend;
pub mod ffi;
pub mod pty;
