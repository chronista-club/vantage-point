//! `vp restart-all` コマンドの実行ロジック
//!
//! doc 44 P1 (fold-in): project が World プロセス内の `Arc<AppState>` になったため、
//! 「全 Process + TheWorld の一括再起動」は **daemon の再起動 1 手**に collapse する。
//! 停止側は World の graceful shutdown が抱えている project を全部畳み、起動側は
//! autostart が enabled な project を順に起こす。
//!
//! 旧実装は「稼働 SP を World registry から列挙 → 1 本ずつ API 停止 → daemon 停止 →
//! daemon 起動 → 1 本ずつ detached spawn」という多段の段取りだったが、
//! 対象である SP プロセスが存在しなくなったため段取りごと不要になった。
//!
//! # ⚠️ 復元の権威が変わった（意味論の転換）
//!
//! 旧: **その時点で running だったもの**を記憶して戻す（実体ベース）
//! 新: 起動時 autostart が **`enabled` な project** を起こす（設定ベース）
//!
//! 実挙動が変わる組み合わせが 2 つある:
//!
//! - `enabled=false` だが手動で起動していた project は、**再起動後に戻らない**
//!   （`vp projects start` は enabled を見ないので、この状態は作れる）
//! - `enabled=true` だが手動で `vp projects stop` していた project は、
//!   **勝手に生き返る**
//!
//! これは `vp daemon restart` と同じ挙動に揃った、とも言える（fold-in 後は
//! project が daemon の中にいる以上、daemon の再起動は必ず autostart を通る）。
//! 「今動いているもの」ではなく「動かすつもりのもの」を権威にする整理で、
//! 停止を永続させたいなら `vp projects disable` を使う。

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
