//! lane ごとの session（PTY / chat engine）と lane の見え方。
//!
//! session 本体（terminal / conversation）は 6-1 で app から移設済。address の wire 型は
//! 共有型として `crate::lane_address` に置く。棚卸し 項目 6 / 6-0（2026-09-08）。

/// conversation session（gui mode の chat）: 購読 + submit / respond の上り request。
pub mod conversation;
/// terminal session（PTY 出力の購読 + write / resize の上り request）。
pub mod terminal;
/// session title の解決（cwd → 表示名）。title poller が使う純粋関数。
pub mod title;
