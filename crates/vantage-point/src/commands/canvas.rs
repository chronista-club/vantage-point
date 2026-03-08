//! `vp canvas` コマンドの実行ロジック

use anyhow::Result;

/// Canvas ウィンドウを起動
pub fn execute(port: u16, project_name: &str, lanes: bool) -> Result<()> {
    crate::canvas::run_canvas(port, project_name, lanes)
}
