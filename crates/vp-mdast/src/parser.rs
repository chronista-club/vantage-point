//! Markdown パーサー — markdown-rs → VP MdNode 変換
//!
//! markdown::to_mdast() で得られる markdown::mdast::Node を
//! VP 独自の MdNode に変換する。変換レイヤーを挟むことで：
//! - Serialize + TS derive が使える
//! - 将来の拡張ノード（admonition, wiki-link）を追加できる
//! - markdown-rs のバージョンアップに内部で対応できる

use crate::nodes::*;
use markdown::mdast;
use markdown::unist;

/// Markdown テキストを VP mdast にパース
pub fn parse(text: &str) -> Result<MdNode, String> {
    let mut options = markdown::ParseOptions::gfm();
    // YAML frontmatter を有効化
    options.constructs.frontmatter = true;
    let tree = markdown::to_mdast(text, &options).map_err(|e| e.to_string())?;
    let mut node = convert_node(tree);
    // 後処理: admonition ブロック変換
    transform_admonitions(&mut node);
    Ok(node)
}

/// markdown-rs の Node → VP の MdNode に変換
fn convert_node(node: mdast::Node) -> MdNode {
    match node {
        mdast::Node::Root(n) => MdNode::Root(Root {
            children: convert_children(n.children),
            position: convert_position(n.position),
        }),

        // --- ブロック ---
        mdast::Node::Heading(n) => MdNode::Heading(Heading {
            depth: n.depth,
            children: convert_children(n.children),
            position: convert_position(n.position),
        }),
        mdast::Node::Paragraph(n) => MdNode::Paragraph(Paragraph {
            children: convert_children(n.children),
            position: convert_position(n.position),
        }),
        mdast::Node::Blockquote(n) => MdNode::BlockQuote(BlockQuote {
            children: convert_children(n.children),
            position: convert_position(n.position),
        }),
        mdast::Node::List(n) => MdNode::List(List {
            children: convert_children(n.children),
            ordered: n.ordered,
            start: n.start.map(|s| s as u32),
            spread: n.spread,
            position: convert_position(n.position),
        }),
        mdast::Node::ListItem(n) => MdNode::ListItem(ListItem {
            children: convert_children(n.children),
            spread: n.spread,
            checked: n.checked,
            position: convert_position(n.position),
        }),
        mdast::Node::Code(n) => MdNode::Code(Code {
            value: n.value,
            lang: n.lang,
            meta: n.meta,
            position: convert_position(n.position),
        }),
        mdast::Node::ThematicBreak(n) => MdNode::ThematicBreak(ThematicBreak {
            position: convert_position(n.position),
        }),
        mdast::Node::Html(n) => MdNode::Html(Html {
            value: n.value,
            position: convert_position(n.position),
        }),
        mdast::Node::Yaml(n) => MdNode::Frontmatter(Frontmatter {
            value: n.value,
            position: convert_position(n.position),
        }),
        mdast::Node::Toml(n) => MdNode::Frontmatter(Frontmatter {
            value: n.value,
            position: convert_position(n.position),
        }),
        mdast::Node::Table(n) => MdNode::Table(Table {
            children: convert_children(n.children),
            align: n.align.into_iter().map(convert_align).collect(),
            position: convert_position(n.position),
        }),
        mdast::Node::TableRow(n) => MdNode::TableRow(TableRow {
            children: convert_children(n.children),
            position: convert_position(n.position),
        }),
        mdast::Node::TableCell(n) => MdNode::TableCell(TableCell {
            children: convert_children(n.children),
            position: convert_position(n.position),
        }),

        // --- インライン ---
        mdast::Node::Text(n) => MdNode::Text(Text {
            value: n.value,
            position: convert_position(n.position),
        }),
        mdast::Node::Emphasis(n) => MdNode::Emphasis(Emphasis {
            children: convert_children(n.children),
            position: convert_position(n.position),
        }),
        mdast::Node::Strong(n) => MdNode::Strong(Strong {
            children: convert_children(n.children),
            position: convert_position(n.position),
        }),
        mdast::Node::InlineCode(n) => MdNode::InlineCode(InlineCode {
            value: n.value,
            position: convert_position(n.position),
        }),
        mdast::Node::Link(n) => MdNode::Link(Link {
            children: convert_children(n.children),
            url: n.url,
            title: n.title,
            position: convert_position(n.position),
        }),
        mdast::Node::Image(n) => MdNode::Image(Image {
            url: n.url,
            alt: if n.alt.is_empty() { None } else { Some(n.alt) },
            title: n.title,
            position: convert_position(n.position),
        }),
        mdast::Node::Break(n) => MdNode::Break(Break {
            position: convert_position(n.position),
        }),
        mdast::Node::Delete(n) => MdNode::Delete(Delete {
            children: convert_children(n.children),
            position: convert_position(n.position),
        }),

        // 未対応ノードは HTML コメントとして保持
        other => MdNode::Html(Html {
            value: format!("<!-- unsupported: {:?} -->", std::mem::discriminant(&other)),
            position: None,
        }),
    }
}

fn convert_children(children: Vec<mdast::Node>) -> Vec<MdNode> {
    children.into_iter().map(convert_node).collect()
}

fn convert_position(pos: Option<unist::Position>) -> Option<Position> {
    pos.map(|p| Position {
        start: Point {
            line: p.start.line,
            column: p.start.column,
            offset: p.start.offset,
        },
        end: Point {
            line: p.end.line,
            column: p.end.column,
            offset: p.end.offset,
        },
    })
}

/// Admonition 変換 — BlockQuote 内の `[!NOTE]` / `[!WARNING]` 等を Admonition ノードに変換
///
/// GitHub Flavored Markdown の alerts 記法に対応:
/// > [!NOTE]
/// > 本文
///
/// および `:::note` 記法（Paragraph テキスト先頭が `:::` で始まる場合）
fn transform_admonitions(node: &mut MdNode) {
    // 再帰的に子ノードを処理
    match node {
        MdNode::Root(root) => transform_admonition_children(&mut root.children),
        MdNode::BlockQuote(bq) => transform_admonition_children(&mut bq.children),
        MdNode::List(list) => transform_admonition_children(&mut list.children),
        MdNode::ListItem(li) => transform_admonition_children(&mut li.children),
        _ => {}
    }
}

fn transform_admonition_children(children: &mut Vec<MdNode>) {
    // 各子ノードを再帰処理
    for child in children.iter_mut() {
        transform_admonitions(child);
    }

    // BlockQuote → Admonition 変換
    let mut i = 0;
    while i < children.len() {
        if let MdNode::BlockQuote(bq) = &children[i] {
            if let Some(admonition) = try_convert_blockquote_to_admonition(bq) {
                children[i] = MdNode::Admonition(admonition);
            }
        }
        i += 1;
    }
}

/// BlockQuote の最初の Paragraph が `[!TYPE]` パターンを含む場合に Admonition に変換
fn try_convert_blockquote_to_admonition(bq: &BlockQuote) -> Option<Admonition> {
    if bq.children.is_empty() {
        return None;
    }

    // 最初の子が Paragraph で、その最初の Text が `[!TYPE]` パターン
    if let MdNode::Paragraph(para) = &bq.children[0] {
        if let Some(MdNode::Text(text)) = para.children.first() {
            // GitHub alerts: [!NOTE], [!WARNING], [!TIP], [!IMPORTANT], [!CAUTION]
            if let Some(kind) = extract_alert_kind(&text.value) {
                let title = extract_alert_title(&text.value);
                // 残りの子ノード（最初の Paragraph の残りテキスト + 後続ノード）
                let mut admonition_children = Vec::new();

                // 最初の Paragraph の残りインライン要素
                let remaining_inline: Vec<MdNode> = para.children[1..].to_vec();
                // タイトル行の後の残りテキスト（改行後）がある場合
                let remaining_text = extract_remaining_text(&text.value);
                if !remaining_text.is_empty() || !remaining_inline.is_empty() {
                    let mut new_children = Vec::new();
                    if !remaining_text.is_empty() {
                        new_children.push(MdNode::Text(Text {
                            value: remaining_text,
                            position: None,
                        }));
                    }
                    new_children.extend(remaining_inline);
                    if !new_children.is_empty() {
                        admonition_children.push(MdNode::Paragraph(Paragraph {
                            children: new_children,
                            position: None,
                        }));
                    }
                }

                // 後続のブロックノード
                admonition_children.extend(bq.children[1..].iter().cloned());

                return Some(Admonition {
                    kind,
                    title,
                    children: admonition_children,
                    position: bq.position.clone(),
                });
            }
        }
    }
    None
}

/// `[!NOTE]` → "note", `[!WARNING]` → "warning" 等を抽出
fn extract_alert_kind(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.starts_with("[!") {
        if let Some(end) = trimmed.find(']') {
            let kind = trimmed[2..end].to_lowercase();
            match kind.as_str() {
                "note" | "warning" | "tip" | "important" | "caution" | "danger" | "info" => {
                    Some(kind)
                }
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    }
}

/// `[!NOTE] Custom Title\nBody` → Some("Custom Title")
/// `[!NOTE]\nBody` → None
fn extract_alert_title(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if let Some(end) = trimmed.find(']') {
        let after = &trimmed[end + 1..];
        // 改行までの部分がタイトル
        let title_part = if let Some(nl) = after.find('\n') {
            after[..nl].trim()
        } else {
            after.trim()
        };
        if title_part.is_empty() {
            None
        } else {
            Some(title_part.to_string())
        }
    } else {
        None
    }
}

/// `[!NOTE]\nRemaining text` → "Remaining text"
/// `[!NOTE] Title\nRemaining text` → "Remaining text"
fn extract_remaining_text(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(end) = trimmed.find(']') {
        let after = &trimmed[end + 1..];
        if let Some(newline_pos) = after.find('\n') {
            after[newline_pos + 1..].trim_start().to_string()
        } else {
            String::new()
        }
    } else {
        String::new()
    }
}

fn convert_align(align: mdast::AlignKind) -> AlignKind {
    match align {
        mdast::AlignKind::Left => AlignKind::Left,
        mdast::AlignKind::Right => AlignKind::Right,
        mdast::AlignKind::Center => AlignKind::Center,
        mdast::AlignKind::None => AlignKind::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_heading() {
        let ast = parse("# Hello").unwrap();
        match ast {
            MdNode::Root(root) => {
                assert_eq!(root.children.len(), 1);
                match &root.children[0] {
                    MdNode::Heading(h) => {
                        assert_eq!(h.depth, 1);
                        match &h.children[0] {
                            MdNode::Text(t) => assert_eq!(t.value, "Hello"),
                            other => panic!("Expected Text, got {:?}", other),
                        }
                    }
                    other => panic!("Expected Heading, got {:?}", other),
                }
            }
            other => panic!("Expected Root, got {:?}", other),
        }
    }

    #[test]
    fn parse_gfm_table() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let ast = parse(md).unwrap();
        match ast {
            MdNode::Root(root) => match &root.children[0] {
                MdNode::Table(t) => {
                    assert_eq!(t.children.len(), 2); // header + body row
                    assert_eq!(t.align.len(), 2);
                }
                other => panic!("Expected Table, got {:?}", other),
            },
            other => panic!("Expected Root, got {:?}", other),
        }
    }

    #[test]
    fn parse_code_block() {
        let md = "```rust\nfn main() {}\n```";
        let ast = parse(md).unwrap();
        match ast {
            MdNode::Root(root) => match &root.children[0] {
                MdNode::Code(c) => {
                    assert_eq!(c.lang.as_deref(), Some("rust"));
                    assert_eq!(c.value, "fn main() {}");
                }
                other => panic!("Expected Code, got {:?}", other),
            },
            other => panic!("Expected Root, got {:?}", other),
        }
    }

    #[test]
    fn parse_inline_elements() {
        let md = "Hello **bold** and *italic* and `code`";
        let ast = parse(md).unwrap();
        match ast {
            MdNode::Root(root) => match &root.children[0] {
                MdNode::Paragraph(p) => {
                    // Text, Strong, Text, Emphasis, Text, InlineCode
                    assert!(p.children.len() >= 5);
                }
                other => panic!("Expected Paragraph, got {:?}", other),
            },
            other => panic!("Expected Root, got {:?}", other),
        }
    }

    #[test]
    fn parse_frontmatter() {
        let md = "---\ntitle: Test\nstatus: Draft\n---\n\n# Hello";
        let ast = parse(md).unwrap();
        match ast {
            MdNode::Root(root) => {
                assert!(root.children.len() >= 2);
                match &root.children[0] {
                    MdNode::Frontmatter(fm) => {
                        assert!(fm.value.contains("title: Test"));
                        assert!(fm.value.contains("status: Draft"));
                    }
                    other => panic!("Expected Frontmatter, got {:?}", other),
                }
                match &root.children[1] {
                    MdNode::Heading(h) => assert_eq!(h.depth, 1),
                    other => panic!("Expected Heading, got {:?}", other),
                }
            }
            other => panic!("Expected Root, got {:?}", other),
        }
    }

    #[test]
    fn parse_admonition_note() {
        let md = "> [!NOTE]\n> This is a note.";
        let ast = parse(md).unwrap();
        match ast {
            MdNode::Root(root) => match &root.children[0] {
                MdNode::Admonition(a) => {
                    assert_eq!(a.kind, "note");
                    assert!(a.title.is_none());
                }
                other => panic!("Expected Admonition, got {:?}", other),
            },
            other => panic!("Expected Root, got {:?}", other),
        }
    }

    #[test]
    fn parse_admonition_warning_with_title() {
        let md = "> [!WARNING] Be careful\n> This is dangerous.";
        let ast = parse(md).unwrap();
        match ast {
            MdNode::Root(root) => match &root.children[0] {
                MdNode::Admonition(a) => {
                    assert_eq!(a.kind, "warning");
                    assert_eq!(a.title.as_deref(), Some("Be careful"));
                }
                other => panic!("Expected Admonition, got {:?}", other),
            },
            other => panic!("Expected Root, got {:?}", other),
        }
    }

    #[test]
    fn serialize_to_json() {
        let ast = parse("# Test").unwrap();
        let json = serde_json::to_string(&ast).unwrap();
        assert!(json.contains("\"type\":\"Root\""));
        assert!(json.contains("\"type\":\"Heading\""));
        assert!(json.contains("\"depth\":1"));
    }
}
