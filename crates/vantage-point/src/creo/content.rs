//! CreoContent envelope + provenance (CreoSource) + CreoCallContext
//!
//! - nexus 決定 #3: `{format, body}` を基本骨格に
//! - nexus 決定 #4: inline content は auto-embed で `CreoSource::Inline` 受け
//! - nexus 決定 #6: MCP call context から auto-link

use serde::{Deserialize, Serialize};

use super::event::ActorRef;
use super::format::CreoFormat;

/// Memory id alias。R0 は文字列で抽象化、creo-memories 側が専用型を出したら差し替え。
pub type MemoryId = String;

/// Content envelope: `{format, body, source?, memory_ref?}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreoContent {
    pub format: CreoFormat,
    pub body: serde_json::Value,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CreoSource>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_ref: Option<MemoryRef>,
}

/// Provenance: 中身がどこから来たか。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CreoSource {
    /// b-stream: 連動更新される live memory。
    Live {
        live_memory_id: MemoryId,
        version: u64,
    },
    /// c-stream: immutable snapshot。
    Snapshot { snapshot_memory_id: MemoryId },
    /// 既存 memory を直接参照。
    Memory { memory_id: MemoryId },
    /// 生データ受け → runtime が auto-embed で ephemeral memory 化する前の暫定。
    Inline,
}

/// memory 参照 (revision pinning 可)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRef {
    pub id: MemoryId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// MCP call に随伴する文脈。auto-link / auto-embed の推論源。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreoCallContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<ActorRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_live_memory_id: Option<MemoryId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_minimal_serde_skips_none() {
        let c = CreoContent {
            format: CreoFormat::Markdown,
            body: serde_json::json!({"text": "hello"}),
            source: None,
            memory_ref: None,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["format"], "markdown");
        assert_eq!(json["body"]["text"], "hello");
        assert!(json.get("source").is_none(), "None should be skipped");
        assert!(json.get("memory_ref").is_none(), "None should be skipped");
    }

    #[test]
    fn source_variant_tag_is_kind() {
        let s = CreoSource::Live {
            live_memory_id: "live_1".into(),
            version: 3,
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["kind"], "live");
        assert_eq!(json["live_memory_id"], "live_1");
        assert_eq!(json["version"], 3);
    }

    #[test]
    fn source_inline_has_no_extra_fields() {
        let s = CreoSource::Inline;
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["kind"], "inline");
    }

    #[test]
    fn roundtrip_with_source() {
        let c = CreoContent {
            format: CreoFormat::Json,
            body: serde_json::json!({"value": {"k": 1}}),
            source: Some(CreoSource::Snapshot {
                snapshot_memory_id: "snap_42".into(),
            }),
            memory_ref: Some(MemoryRef {
                id: "mem_1".into(),
                revision: Some("r1".into()),
            }),
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: CreoContent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.format, CreoFormat::Json);
        match back.source {
            Some(CreoSource::Snapshot { snapshot_memory_id }) => {
                assert_eq!(snapshot_memory_id, "snap_42");
            }
            other => panic!("expected Snapshot, got {:?}", other),
        }
    }
}
