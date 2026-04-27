//! vp-cli library exports
//!
//! Phase 4-X (2026-04-27): ccws lib は **vantage-point に移動**。
//! vp-cli は薄い CLI wrapper で、 中身は `vantage_point::ccws` を呼ぶ。
//!
//! 旧 Phase 2.x-e の構造 (vp-cli が ccws lib を持つ) は subprocess 連携前提だった。
//! Phase 4-X で server (vantage-point) からの直接 lib call に方針変更したため、
//! ccws lib の住所も SP server (= vantage-point) 側に move。
//!
//! ## Public re-export
//!
//! - `vp_cli::ccws` ── `vantage_point::ccws` の re-export (後方互換)

pub use vantage_point::ccws;
