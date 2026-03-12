//! mdast ノード型定義
//!
//! markdown-rs の mdast 型を VP 用にラップ。
//! #[derive(Serialize, TS)] で JSON シリアライズ + TypeScript 型生成を提供。

use serde::Serialize;
use ts_rs::TS;

/// ソース位置情報
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Position {
    pub start: Point,
    pub end: Point,
}

/// 行・列・オフセット
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Point {
    pub line: usize,
    pub column: usize,
    pub offset: usize,
}

/// mdast ノード（全ノードタイプの enum）
#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "type")]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub enum MdNode {
    // --- ドキュメント ---
    Root(Root),

    // --- ブロック ---
    Heading(Heading),
    Paragraph(Paragraph),
    BlockQuote(BlockQuote),
    List(List),
    ListItem(ListItem),
    Code(Code),
    ThematicBreak(ThematicBreak),
    Table(Table),
    TableRow(TableRow),
    TableCell(TableCell),
    Html(Html),

    // --- インライン ---
    Text(Text),
    Emphasis(Emphasis),
    Strong(Strong),
    InlineCode(InlineCode),
    Link(Link),
    Image(Image),
    Break(Break),
    Delete(Delete),

    // --- 拡張（Phase 2） ---
    Frontmatter(Frontmatter),
    Admonition(Admonition),
    // WikiLink(WikiLink),  // Phase 3
}

// =============================================================================
// ブロックノード
// =============================================================================

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Root {
    pub children: Vec<MdNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Heading {
    pub depth: u8,
    pub children: Vec<MdNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Paragraph {
    pub children: Vec<MdNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct BlockQuote {
    pub children: Vec<MdNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct List {
    pub children: Vec<MdNode>,
    pub ordered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
    pub spread: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct ListItem {
    pub children: Vec<MdNode>,
    pub spread: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Code {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct ThematicBreak {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Html {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

// --- GFM テーブル ---

/// テーブルセルの配置
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub enum AlignKind {
    Left,
    Right,
    Center,
    None,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Table {
    pub children: Vec<MdNode>,
    pub align: Vec<AlignKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct TableRow {
    pub children: Vec<MdNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct TableCell {
    pub children: Vec<MdNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

// =============================================================================
// インラインノード
// =============================================================================

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Text {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Emphasis {
    pub children: Vec<MdNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Strong {
    pub children: Vec<MdNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct InlineCode {
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Link {
    pub children: Vec<MdNode>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Image {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Break {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

/// GFM 取り消し線
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Delete {
    pub children: Vec<MdNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

// =============================================================================
// 拡張ノード（Phase 2）
// =============================================================================

/// YAML frontmatter — `---` で囲まれたメタデータブロック
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Frontmatter {
    /// YAML 生テキスト（Canvas 側で js-yaml 等でパース）
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}

/// Admonition ブロック — `:::note` `:::warning` `:::danger` `:::tip`
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export, export_to = "../../../web/src/mdast/types.ts")]
pub struct Admonition {
    /// タイプ: note, warning, danger, tip, info, caution
    pub kind: String,
    /// タイトル（省略可）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 子ノード（本文）
    pub children: Vec<MdNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
}
