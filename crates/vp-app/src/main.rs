//! vp-app バイナリエントリポイント

fn main() -> anyhow::Result<()> {
    vp_app::app::run()
}
