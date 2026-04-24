//! vp-shell バイナリエントリポイント

fn main() -> anyhow::Result<()> {
    vp_shell::app::run()
}
