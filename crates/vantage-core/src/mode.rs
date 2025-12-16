//! 協調モード
//!
//! 協調 / 委任 / 自律 の3段階

use serde::{Deserialize, Serialize};

/// 協調モード
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CooperationMode {
    /// 協調: ユーザーと一緒に進める
    #[default]
    Cooperative,
    /// 委任: 任せて、途中経過・結果を確認
    Delegated,
    /// 自律: 完全に任せる
    Autonomous,
}

impl CooperationMode {
    pub fn description(&self) -> &'static str {
        match self {
            Self::Cooperative => "ユーザーと一緒に進める",
            Self::Delegated => "任せて、途中経過・結果を確認",
            Self::Autonomous => "完全に任せる",
        }
    }

    pub fn short_name(&self) -> &'static str {
        match self {
            Self::Cooperative => "協調",
            Self::Delegated => "委任",
            Self::Autonomous => "自律",
        }
    }
}

impl std::fmt::Display for CooperationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.short_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_mode() {
        let mode = CooperationMode::default();
        assert_eq!(mode, CooperationMode::Cooperative);
    }

    #[test]
    fn test_serialization() {
        let mode = CooperationMode::Delegated;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"delegated\"");
    }
}
