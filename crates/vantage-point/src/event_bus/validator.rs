//! Topic schema validator — canonical 4-part + allowed scope/category enum.
//!
//! VP-73 `looks_canonical` は shape のみ check。本モジュールは scope / category の
//! 許容値まで踏み込む。Worker C (rust-reviewer) Finding H-1 の一部を吸収するが、
//! 完全な Topic newtype 化は VP-81 で別途実施。

use crate::creo::looks_canonical;

/// scope slot の許容値 (D-8 canonical 4-part)。
const ALLOWED_SCOPES: &[&str] = &["project", "user", "system"];

/// category slot の許容値 (D-8 canonical 4-part)。
const ALLOWED_CATEGORIES: &[&str] = &["state", "command", "lifecycle", "error", "notify"];

/// Topic validation error (structured、wire 側での diagnostic 用)。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("topic shape invalid (need 4+ non-empty segments): {0}")]
    Shape(String),
    #[error("unknown scope '{0}' (allowed: project/user/system)")]
    Scope(String),
    #[error("unknown category '{0}' (allowed: state/command/lifecycle/error/notify)")]
    Category(String),
}

/// canonical 4-part topic を schema 上で validate する。
pub fn validate_topic(topic: &str) -> Result<(), ValidationError> {
    if !looks_canonical(topic) {
        return Err(ValidationError::Shape(topic.into()));
    }
    let mut parts = topic.split('/').filter(|s| !s.is_empty());
    // unwrap 安全: looks_canonical で 4+ segment 保証
    let scope = parts.next().unwrap();
    if !ALLOWED_SCOPES.contains(&scope) {
        return Err(ValidationError::Scope(scope.into()));
    }
    let _capability = parts.next().unwrap();
    let category = parts.next().unwrap();
    if !ALLOWED_CATEGORIES.contains(&category) {
        return Err(ValidationError::Category(category.into()));
    }
    // detail 以降は自由 (wildcard 前提)
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_ok() {
        assert!(validate_topic("project/hd/notify/message").is_ok());
        assert!(validate_topic("user/user/command/click").is_ok());
        assert!(validate_topic("system/tw/lifecycle/process-started").is_ok());
    }

    #[test]
    fn canonical_with_extra_segments_ok() {
        // detail が `/` を含む (sub-addressing)
        assert!(validate_topic("project/sc/state/item-added/sc-id/abc").is_ok());
    }

    #[test]
    fn unknown_scope_rejected() {
        assert_eq!(
            validate_topic("foo/hd/notify/message"),
            Err(ValidationError::Scope("foo".into()))
        );
    }

    #[test]
    fn unknown_category_rejected() {
        assert_eq!(
            validate_topic("project/hd/xxx/message"),
            Err(ValidationError::Category("xxx".into()))
        );
    }

    #[test]
    fn short_shape_rejected() {
        let err = validate_topic("project/hd").unwrap_err();
        assert!(matches!(err, ValidationError::Shape(_)));
    }

    #[test]
    fn alias_form_rejected() {
        // alias 形式 "pp.route" は canonical ではない (Bus 側で先に alias 解決される想定)
        let err = validate_topic("pp.route").unwrap_err();
        assert!(matches!(err, ValidationError::Shape(_)));
    }
}
