//! club-kdl-codegen による生成物モジュール。
//!
//! このディレクトリ配下の `.rs` は手で編集しない。 KDL protocol schema を SSOT に
//! `cargo test -p vp-app --test sidebar_ipc_codegen` で再生成される。
//!
//! - [`sidebar_ipc`] — sidebar IPC (`schema/vp-sidebar.kdl`、 VP-208 Phase A)。
//!   PR-1 時点では生成物を既存の手書き定義と並存させるのみ (caller 移行は PR-2 / PR-3)。

pub mod sidebar_ipc;
