//! `vp canvas` コマンドの実行ロジック

use anyhow::Result;

/// Canvas ウィンドウを起動
pub fn execute(port: u16) -> Result<()> {
    crate::canvas::run_canvas(port)
}
