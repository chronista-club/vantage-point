//! VP mdast パイプライン
//!
//! Markdown テキストを mdast (Markdown Abstract Syntax Tree) にパースし、
//! TypeScript 型定義を自動生成する。
//!
//! - パーサー: markdown-rs (wooorm)
//! - 型生成: ts-rs (#[derive(TS)])
//! - シリアライズ: serde (JSON)

pub mod nodes;
pub mod parser;

pub use nodes::*;
pub use parser::parse;
