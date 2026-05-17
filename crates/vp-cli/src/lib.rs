//! vp-cli library exports
//!
//! Phase 4-X (2026-04-27): lane lib は **vantage-point に移動**。
//! vp-cli は薄い CLI wrapper で、 中身は `vantage_point::lane` を呼ぶ。
//!
//! 旧 Phase 2.x-e の構造 (vp-cli が lane lib を持つ) は subprocess 連携前提だった。
//! Phase 4-X で server (vantage-point) からの直接 lib call に方針変更したため、
//! lane lib の住所も SP server (= vantage-point) 側に move。
//!
//! ## Public re-export
//!
//! - `vp_cli::lane` ── `vantage_point::lane` の re-export (後方互換)

pub use vantage_point::lane;
