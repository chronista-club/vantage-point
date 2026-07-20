//! `vp restart-all` コマンドの実行ロジック
//!
//! doc 44 P1 (fold-in): project が World プロセス内の `Arc<AppState>` になったため、
//! 「全 Process + TheWorld の一括再起動」は **daemon の再起動 1 手**に collapse する。
//! 停止側は World の graceful shutdown が抱えている project を全部畳み、起動側は
//! autostart が enabled な project を順に起こす。
//!
//! 旧実装は「稼働 SP を port scan で列挙 → 1 本ずつ API 停止 → daemon 停止 →
//! daemon 起動 → 1 本ずつ detached spawn」という多段の段取りだったが、
//! 対象である SP プロセスが存在しなくなったため段取りごと不要になった。

use anyhow::Result;

/// `vp restart-all` を実行
///
/// 実体は `vp daemon restart` と同じ ownership-agnostic な再起動
/// （実 port holder を停止 → LaunchAgent 優先で起動）。 別名を残しているのは
/// 「新バイナリへ切り替える」という用途の入口として定着しているため。
pub fn execute() -> Result<()> {
    super::daemon::restart(false)?;
    println!();
    println!("project は autostart で順次立ち上がります（`vp ps` で確認）");
    Ok(())
}
