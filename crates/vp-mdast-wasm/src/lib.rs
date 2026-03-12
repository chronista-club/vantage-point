//! VP mdast WASM バインディング
//!
//! Canvas (WebView) 内で Markdown → mdast パースを実行する。
//! wasm-bindgen でエクスポートし、TypeScript から呼び出す。

use wasm_bindgen::prelude::*;

/// Markdown テキストを mdast JSON にパース
///
/// Canvas の TypeScript から呼び出される。
/// 戻り値は MdNode (Root) の JsValue（ネイティブ JS オブジェクト）。
#[wasm_bindgen]
pub fn parse(markdown: &str) -> Result<JsValue, JsValue> {
    let ast = vp_mdast::parse(markdown).map_err(|e| JsValue::from_str(&e))?;
    serde_wasm_bindgen::to_value(&ast).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Markdown テキストを mdast JSON 文字列にパース（デバッグ用）
#[wasm_bindgen]
pub fn parse_to_json(markdown: &str) -> Result<String, JsValue> {
    let ast = vp_mdast::parse(markdown).map_err(|e| JsValue::from_str(&e))?;
    serde_json::to_string_pretty(&ast).map_err(|e| JsValue::from_str(&e.to_string()))
}
