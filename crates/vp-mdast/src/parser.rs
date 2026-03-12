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
    let options = markdown::ParseOptions::gfm();
    let tree = markdown::to_mdast(text, &options).map_err(|e| e.to_string())?;
    Ok(convert_node(tree))
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
    fn serialize_to_json() {
        let ast = parse("# Test").unwrap();
        let json = serde_json::to_string(&ast).unwrap();
        assert!(json.contains("\"type\":\"Root\""));
        assert!(json.contains("\"type\":\"Heading\""));
        assert!(json.contains("\"depth\":1"));
    }
}
