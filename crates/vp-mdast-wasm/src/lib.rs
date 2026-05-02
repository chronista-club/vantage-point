//! VP mdast WASM バインディング
//!
//! Canvas (WebView) 内で Markdown → mdast パースを実行する。
//! wasm-bindgen でエクスポートし、TypeScript から呼び出す。
//!
//! 2026-05-01: 内部実装を creo-md (chronista-club/creo-views) に移行。
//! crate 名 (vp-mdast-wasm) と output filename (vp_mdast_wasm_bg.wasm) は維持、
//! health.rs:759-763 の include_bytes! は無変更。

use wasm_bindgen::prelude::*;

/// Markdown テキストを mdast JSON にパース
///
/// Canvas の TypeScript から呼び出される。
/// 戻り値は MdNode (Root) の JsValue（ネイティブ JS オブジェクト）。
#[wasm_bindgen]
pub fn parse(markdown: &str) -> Result<JsValue, JsValue> {
    let ast = creo_md::parse(markdown).map_err(|e| JsValue::from_str(&e))?;
    serde_wasm_bindgen::to_value(&ast).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Markdown テキストを mdast JSON 文字列にパース（デバッグ用）
#[wasm_bindgen]
pub fn parse_to_json(markdown: &str) -> Result<String, JsValue> {
    let ast = creo_md::parse(markdown).map_err(|e| JsValue::from_str(&e))?;
    serde_json::to_string_pretty(&ast).map_err(|e| JsValue::from_str(&e.to_string()))
}
