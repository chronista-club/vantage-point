//! CreoFormat — 12 メンバー enum (nexus 決定 #2)
//!
//! `Memory.contentType` と同一集合。render client は `format` で dispatch し、
//! 未知 variant は graceful fallback (text render) する。

use serde::{Deserialize, Serialize};

/// 内容の形式識別子。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CreoFormat {
    Mermaid,
    Markdown,
    Text,
    Image,
    Table,
    Chart,
    Code,
    Json,
    Video,
    Audio,
    /// URL / iframe 埋め込み
    Embed,
    /// 拡張枠。`body.custom_kind: string` 必須を推奨 (schema 登録機構は Round 2)。
    Custom,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&CreoFormat::Mermaid).unwrap(),
            "\"mermaid\""
        );
        assert_eq!(
            serde_json::to_string(&CreoFormat::Markdown).unwrap(),
            "\"markdown\""
        );
        assert_eq!(
            serde_json::to_string(&CreoFormat::Custom).unwrap(),
            "\"custom\""
        );
    }

    #[test]
    fn serde_roundtrip() {
        for f in [
            CreoFormat::Mermaid,
            CreoFormat::Markdown,
            CreoFormat::Text,
            CreoFormat::Image,
            CreoFormat::Table,
            CreoFormat::Chart,
            CreoFormat::Code,
            CreoFormat::Json,
            CreoFormat::Video,
            CreoFormat::Audio,
            CreoFormat::Embed,
            CreoFormat::Custom,
        ] {
            let s = serde_json::to_string(&f).unwrap();
            let back: CreoFormat = serde_json::from_str(&s).unwrap();
            assert_eq!(back, f);
        }
    }
}
