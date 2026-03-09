//! Topic パス: MQTT v5 inspired topic routing
//!
//! 命名規則: `{scope}/{capability}/{category}/{detail}`
//! 例: `"process/paisley-park/command/show"`
//!
//! ## ワイルドカード
//! - `+` (または `*`): 1セグメントに一致（MQTT 互換）
//! - `#`: 0個以上のセグメントに一致（末尾のみ、MQTT 互換）

use std::fmt;

/// Topic パス（具体的なメッセージの宛先）
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TopicPath {
    segments: Vec<String>,
}

impl TopicPath {
    /// スラッシュ区切りの文字列からパース
    pub fn parse(s: &str) -> Self {
        let segments = s
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        Self { segments }
    }

    /// セグメントをスラッシュで結合して返す
    pub fn as_str(&self) -> String {
        self.segments.join("/")
    }

    /// capability セグメント（2番目）を返す
    pub fn capability(&self) -> Option<&str> {
        self.segments.get(1).map(|s| s.as_str())
    }

    /// category セグメント（3番目）を返す
    pub fn category(&self) -> Option<&str> {
        self.segments.get(2).map(|s| s.as_str())
    }

    /// state または command カテゴリのトピックは retained（最新値を保持）
    pub fn is_retained(&self) -> bool {
        matches!(self.category(), Some("state") | Some("command"))
    }

    /// パターンとのマッチング判定
    pub fn matches(&self, pattern: &TopicPattern) -> bool {
        pattern.matches(self)
    }

    /// セグメント数を返す
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// セグメントが空かどうか
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// セグメントへの参照を返す
    pub fn segments(&self) -> &[String] {
        &self.segments
    }
}

impl fmt::Display for TopicPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.segments.join("/"))
    }
}

impl From<&str> for TopicPath {
    fn from(s: &str) -> Self {
        Self::parse(s)
    }
}

/// パターンセグメント（リテラルまたはワイルドカード）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// 完全一致するリテラル文字列
    Literal(String),
    /// 1セグメントに一致（`+` または `*`）
    SingleWildcard,
    /// 0個以上のセグメントに一致（`#`、末尾のみ有効）
    MultiWildcard,
}

/// Topic パターン（ワイルドカード付きのフィルタ）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicPattern {
    segments: Vec<Segment>,
}

impl TopicPattern {
    /// スラッシュ区切りの文字列からパース
    ///
    /// - `+` または `*` → SingleWildcard
    /// - `#` → MultiWildcard（末尾のみ）
    /// - それ以外 → Literal
    pub fn parse(s: &str) -> Self {
        let segments = s
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| match s {
                "+" | "*" => Segment::SingleWildcard,
                "#" => Segment::MultiWildcard,
                _ => Segment::Literal(s.to_string()),
            })
            .collect();
        Self { segments }
    }

    /// TopicPath がこのパターンに一致するか判定
    ///
    /// マッチングルール:
    /// - Literal: 完全一致
    /// - SingleWildcard (`+`/`*`): 任意の1セグメントに一致
    /// - MultiWildcard (`#`): 残り全セグメント（0個以上）に一致（末尾のみ）
    pub fn matches(&self, topic: &TopicPath) -> bool {
        self.matches_recursive(&self.segments, &topic.segments)
    }

    /// 再帰的マッチング
    fn matches_recursive(&self, pattern: &[Segment], topic: &[String]) -> bool {
        match (pattern.first(), topic.first()) {
            // 両方消費完了 → 一致
            (None, None) => true,
            // パターンのみ残り: # なら0個マッチで成功
            (Some(Segment::MultiWildcard), _) => true,
            // パターン消費完了だがトピック残り → 不一致
            (None, Some(_)) => false,
            // トピック消費完了だがパターン残り → 不一致
            (Some(_), None) => false,
            // リテラル: 完全一致なら次へ
            (Some(Segment::Literal(lit)), Some(seg)) => {
                lit == seg && self.matches_recursive(&pattern[1..], &topic[1..])
            }
            // SingleWildcard: 任意の1セグメントに一致
            (Some(Segment::SingleWildcard), Some(_)) => {
                self.matches_recursive(&pattern[1..], &topic[1..])
            }
        }
    }

    /// セグメントへの参照を返す
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }
}

impl fmt::Display for TopicPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<&str> = self
            .segments
            .iter()
            .map(|s| match s {
                Segment::Literal(lit) => lit.as_str(),
                Segment::SingleWildcard => "+",
                Segment::MultiWildcard => "#",
            })
            .collect();
        write!(f, "{}", parts.join("/"))
    }
}

impl From<&str> for TopicPattern {
    fn from(s: &str) -> Self {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // TopicPath
    // =========================================================================

    #[test]
    fn test_parse_and_as_str_roundtrip() {
        let path = TopicPath::parse("process/paisley-park/command/show");
        assert_eq!(path.as_str(), "process/paisley-park/command/show");
        assert_eq!(path.len(), 4);
    }

    #[test]
    fn test_parse_empty() {
        let path = TopicPath::parse("");
        assert!(path.is_empty());
        assert_eq!(path.as_str(), "");
    }

    #[test]
    fn test_parse_with_leading_trailing_slashes() {
        // 先頭・末尾のスラッシュは空セグメントとしてフィルタされる
        let path = TopicPath::parse("/process/debug/log/");
        assert_eq!(path.as_str(), "process/debug/log");
        assert_eq!(path.len(), 3);
    }

    #[test]
    fn test_capability() {
        let path = TopicPath::parse("process/paisley-park/command/show");
        assert_eq!(path.capability(), Some("paisley-park"));
    }

    #[test]
    fn test_category() {
        let path = TopicPath::parse("process/heavens-door/event/chat-message");
        assert_eq!(path.category(), Some("event"));
    }

    #[test]
    fn test_capability_none_for_short_path() {
        let path = TopicPath::parse("process");
        assert_eq!(path.capability(), None);
        assert_eq!(path.category(), None);
    }

    #[test]
    fn test_is_retained_state() {
        let path = TopicPath::parse("process/terminal/state/ready");
        assert!(path.is_retained());
    }

    #[test]
    fn test_is_retained_command() {
        let path = TopicPath::parse("process/paisley-park/command/show/main");
        assert!(path.is_retained());
    }

    #[test]
    fn test_is_not_retained_event() {
        let path = TopicPath::parse("process/heavens-door/event/chat-message");
        assert!(!path.is_retained());
    }

    #[test]
    fn test_is_not_retained_data() {
        let path = TopicPath::parse("process/terminal/data/output");
        assert!(!path.is_retained());
    }

    #[test]
    fn test_display() {
        let path = TopicPath::parse("process/debug/log");
        assert_eq!(format!("{}", path), "process/debug/log");
    }

    #[test]
    fn test_from_str() {
        let path: TopicPath = "process/debug/trace".into();
        assert_eq!(path.as_str(), "process/debug/trace");
    }

    // =========================================================================
    // TopicPattern パース
    // =========================================================================

    #[test]
    fn test_pattern_parse_literals() {
        let pattern = TopicPattern::parse("process/paisley-park/command/show");
        assert_eq!(pattern.segments().len(), 4);
        assert!(matches!(&pattern.segments()[0], Segment::Literal(s) if s == "process"));
        assert!(matches!(&pattern.segments()[3], Segment::Literal(s) if s == "show"));
    }

    #[test]
    fn test_pattern_parse_single_wildcard_plus() {
        let pattern = TopicPattern::parse("process/+/state/#");
        assert_eq!(pattern.segments().len(), 4);
        assert!(matches!(pattern.segments()[1], Segment::SingleWildcard));
        assert!(matches!(pattern.segments()[3], Segment::MultiWildcard));
    }

    #[test]
    fn test_pattern_parse_star_alias() {
        // `*` は `+` のエイリアス
        let pattern = TopicPattern::parse("process/paisley-park/command/*");
        assert_eq!(pattern.segments().len(), 4);
        assert!(matches!(pattern.segments()[3], Segment::SingleWildcard));
    }

    // =========================================================================
    // パターンマッチング
    // =========================================================================

    #[test]
    fn test_match_exact_literal() {
        let topic = TopicPath::parse("process/paisley-park/command/show");
        let pattern = TopicPattern::parse("process/paisley-park/command/show");
        assert!(topic.matches(&pattern));
    }

    #[test]
    fn test_no_match_different_literal() {
        let topic = TopicPath::parse("process/paisley-park/command/show");
        let pattern = TopicPattern::parse("process/paisley-park/command/clear");
        assert!(!topic.matches(&pattern));
    }

    #[test]
    fn test_match_single_wildcard() {
        let topic = TopicPath::parse("process/paisley-park/command/show");
        let pattern = TopicPattern::parse("process/paisley-park/command/+");
        assert!(topic.matches(&pattern));
    }

    #[test]
    fn test_match_star_as_single_wildcard() {
        let topic = TopicPath::parse("process/paisley-park/command/show");
        let pattern = TopicPattern::parse("process/paisley-park/command/*");
        assert!(topic.matches(&pattern));
    }

    #[test]
    fn test_single_wildcard_does_not_match_multiple() {
        // `+` は1セグメントのみ一致
        let topic = TopicPath::parse("process/paisley-park/command/show/main");
        let pattern = TopicPattern::parse("process/paisley-park/command/+");
        assert!(!topic.matches(&pattern));
    }

    #[test]
    fn test_match_multi_wildcard_zero_segments() {
        // `#` は0セグメントにも一致
        let topic = TopicPath::parse("process/paisley-park/command");
        let pattern = TopicPattern::parse("process/paisley-park/command/#");
        assert!(topic.matches(&pattern));
    }

    #[test]
    fn test_match_multi_wildcard_one_segment() {
        let topic = TopicPath::parse("process/paisley-park/command/show");
        let pattern = TopicPattern::parse("process/paisley-park/command/#");
        assert!(topic.matches(&pattern));
    }

    #[test]
    fn test_match_multi_wildcard_many_segments() {
        let topic = TopicPath::parse("process/paisley-park/command/show/main/extra");
        let pattern = TopicPattern::parse("process/paisley-park/command/#");
        assert!(topic.matches(&pattern));
    }

    #[test]
    fn test_match_combined_wildcards() {
        // `+` と `#` の組み合わせ
        let topic = TopicPath::parse("process/heavens-door/state/session-list");
        let pattern = TopicPattern::parse("process/+/state/#");
        assert!(topic.matches(&pattern));
    }

    #[test]
    fn test_no_match_too_short() {
        let topic = TopicPath::parse("process/paisley-park");
        let pattern = TopicPattern::parse("process/paisley-park/command/show");
        assert!(!topic.matches(&pattern));
    }

    #[test]
    fn test_match_all_with_hash() {
        // `#` だけで全トピックに一致
        let topic = TopicPath::parse("process/debug/log");
        let pattern = TopicPattern::parse("#");
        assert!(topic.matches(&pattern));
    }

    #[test]
    fn test_match_all_events() {
        let pattern = TopicPattern::parse("process/+/event/#");

        let chat = TopicPath::parse("process/heavens-door/event/chat-message");
        let session = TopicPath::parse("process/heavens-door/event/session-created");
        let command = TopicPath::parse("process/paisley-park/command/show");

        assert!(chat.matches(&pattern));
        assert!(session.matches(&pattern));
        assert!(!command.matches(&pattern));
    }

    #[test]
    fn test_pattern_display() {
        let pattern = TopicPattern::parse("process/+/state/#");
        assert_eq!(format!("{}", pattern), "process/+/state/#");
    }

    #[test]
    fn test_pattern_from_str() {
        let pattern: TopicPattern = "process/debug/#".into();
        let topic = TopicPath::parse("process/debug/log");
        assert!(topic.matches(&pattern));
    }
}
