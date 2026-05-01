//! VP mdast — alias for creo-md (chronista-club/creo-views)。
//!
//! 2026-05-01: vp-mdast を creo-md からの thin re-export に切替。
//! 真実は creo-md crate (creo-views repo)、 vp-mdast は backward-compat alias。
//!
//! Migration: vp-mdast / vp-mdast-wasm の中身を creo-md / creo-md-wasm に同等抽出
//! (chronista-club/creo-views) → こちらは re-export。 health.rs (vantage-point) や
//! 他 consumer の `use vp_mdast::...` は無変更で動作。

pub use creo_md::nodes;
pub use creo_md::parser;
pub use creo_md::*;
