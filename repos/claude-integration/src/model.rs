//! Claudeモデルの定義
//!
//! 利用可能なClaudeモデルとその特性

use serde::{Deserialize, Serialize};
use std::fmt;

/// 利用可能なClaudeモデル
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Model {
    /// Claude 3 Opus - 最も高性能なモデル
    #[serde(rename = "claude-3-opus-20240229")]
    Claude3Opus,

    /// Claude 3 Sonnet - バランスの取れたモデル
    #[serde(rename = "claude-3-sonnet-20240229")]
    Claude3Sonnet,

    /// Claude 3 Haiku - 高速で軽量なモデル
    #[serde(rename = "claude-3-haiku-20240307")]
    Claude3Haiku,

    /// Claude 3.5 Sonnet - 最新の改良版Sonnet
    #[serde(rename = "claude-3-5-sonnet-20241022")]
    Claude35Sonnet,

    /// Claude 2.1 - 前世代の高性能モデル
    #[serde(rename = "claude-2.1")]
    Claude21,

    /// Claude 2.0 - 前世代の標準モデル
    #[serde(rename = "claude-2.0")]
    Claude20,

    /// Claude Instant 1.2 - 前世代の高速モデル
    #[serde(rename = "claude-instant-1.2")]
    ClaudeInstant12,
}

impl Model {
    /// モデルのAPI識別子を取得
    pub fn as_str(&self) -> &'static str {
        match self {
            Model::Claude3Opus => "claude-3-opus-20240229",
            Model::Claude3Sonnet => "claude-3-sonnet-20240229",
            Model::Claude3Haiku => "claude-3-haiku-20240307",
            Model::Claude35Sonnet => "claude-3-5-sonnet-20241022",
            Model::Claude21 => "claude-2.1",
            Model::Claude20 => "claude-2.0",
            Model::ClaudeInstant12 => "claude-instant-1.2",
        }
    }

    /// コンテキストウィンドウサイズを取得（トークン数）
    pub fn context_window(&self) -> usize {
        match self {
            Model::Claude3Opus
            | Model::Claude3Sonnet
            | Model::Claude3Haiku
            | Model::Claude35Sonnet => 200_000,
            Model::Claude21 => 200_000,
            Model::Claude20 => 100_000,
            Model::ClaudeInstant12 => 100_000,
        }
    }

    /// 最大出力トークン数を取得
    pub fn max_output_tokens(&self) -> usize {
        match self {
            Model::Claude3Opus | Model::Claude3Sonnet | Model::Claude3Haiku => 4_096,
            Model::Claude35Sonnet => 8_192,
            Model::Claude21 | Model::Claude20 | Model::ClaudeInstant12 => 4_096,
        }
    }

    /// モデルがビジョン（画像入力）をサポートしているか
    pub fn supports_vision(&self) -> bool {
        matches!(
            self,
            Model::Claude3Opus | Model::Claude3Sonnet | Model::Claude3Haiku | Model::Claude35Sonnet
        )
    }

    /// モデルがファンクションコーリングをサポートしているか
    pub fn supports_function_calling(&self) -> bool {
        matches!(
            self,
            Model::Claude3Opus | Model::Claude3Sonnet | Model::Claude3Haiku | Model::Claude35Sonnet
        )
    }

    /// モデルの相対的な速度（1-5、5が最速）
    pub fn speed_rating(&self) -> u8 {
        match self {
            Model::Claude3Haiku => 5,
            Model::Claude35Sonnet => 4,
            Model::Claude3Sonnet => 3,
            Model::ClaudeInstant12 => 4,
            Model::Claude3Opus => 2,
            Model::Claude21 => 2,
            Model::Claude20 => 2,
        }
    }

    /// モデルの相対的な能力（1-5、5が最高）
    pub fn capability_rating(&self) -> u8 {
        match self {
            Model::Claude3Opus => 5,
            Model::Claude35Sonnet => 5,
            Model::Claude3Sonnet => 4,
            Model::Claude21 => 4,
            Model::Claude20 => 3,
            Model::Claude3Haiku => 3,
            Model::ClaudeInstant12 => 3,
        }
    }

    /// モデルの相対的なコスト（1-5、5が最高価格）
    pub fn cost_rating(&self) -> u8 {
        match self {
            Model::Claude3Opus => 5,
            Model::Claude35Sonnet => 3,
            Model::Claude3Sonnet => 3,
            Model::Claude21 => 4,
            Model::Claude20 => 4,
            Model::Claude3Haiku => 1,
            Model::ClaudeInstant12 => 2,
        }
    }

    /// 推奨される用途の説明
    pub fn recommended_use_case(&self) -> &'static str {
        match self {
            Model::Claude3Opus => "複雑なタスク、研究、詳細な分析、創造的な執筆",
            Model::Claude35Sonnet => "バランスの取れた高性能タスク、コード生成、一般的な会話",
            Model::Claude3Sonnet => "一般的な用途、適度に複雑なタスク",
            Model::Claude3Haiku => "高速レスポンスが必要なタスク、簡単な質問応答、大量処理",
            Model::Claude21 => "長文の文書処理、要約、前世代モデルとの互換性",
            Model::Claude20 => "基本的なタスク、前世代モデルとの互換性",
            Model::ClaudeInstant12 => "軽量なタスク、高速処理、チャットボット",
        }
    }

    /// モデルが廃止予定かどうか
    pub fn is_deprecated(&self) -> bool {
        matches!(self, Model::Claude20 | Model::ClaudeInstant12)
    }

    /// モデルが最新世代かどうか
    pub fn is_latest_generation(&self) -> bool {
        matches!(
            self,
            Model::Claude3Opus | Model::Claude3Sonnet | Model::Claude3Haiku | Model::Claude35Sonnet
        )
    }

    /// 文字列からモデルを解析
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "claude-3-opus-20240229" | "claude-3-opus" => Some(Model::Claude3Opus),
            "claude-3-sonnet-20240229" | "claude-3-sonnet" => Some(Model::Claude3Sonnet),
            "claude-3-haiku-20240307" | "claude-3-haiku" => Some(Model::Claude3Haiku),
            "claude-3-5-sonnet-20241022" | "claude-3.5-sonnet" => Some(Model::Claude35Sonnet),
            "claude-2.1" => Some(Model::Claude21),
            "claude-2.0" | "claude-2" => Some(Model::Claude20),
            "claude-instant-1.2" | "claude-instant" => Some(Model::ClaudeInstant12),
            _ => None,
        }
    }

    /// 利用可能なすべてのモデルを取得
    pub fn all() -> Vec<Model> {
        vec![
            Model::Claude3Opus,
            Model::Claude35Sonnet,
            Model::Claude3Sonnet,
            Model::Claude3Haiku,
            Model::Claude21,
            Model::Claude20,
            Model::ClaudeInstant12,
        ]
    }

    /// 最新世代のモデルのみを取得
    pub fn latest_generation() -> Vec<Model> {
        vec![
            Model::Claude3Opus,
            Model::Claude35Sonnet,
            Model::Claude3Sonnet,
            Model::Claude3Haiku,
        ]
    }

    /// ビジョン対応モデルのみを取得
    pub fn vision_capable() -> Vec<Model> {
        Self::all()
            .into_iter()
            .filter(|m| m.supports_vision())
            .collect()
    }
}

impl Default for Model {
    fn default() -> Self {
        Model::Claude35Sonnet
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for Model {
    type Err = ModelParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Model::from_str(s).ok_or_else(|| ModelParseError {
            value: s.to_string(),
        })
    }
}

/// モデル解析エラー
#[derive(Debug, Clone)]
pub struct ModelParseError {
    value: String,
}

impl fmt::Display for ModelParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Unknown model: {}", self.value)
    }
}

impl std::error::Error for ModelParseError {}

/// モデル選択のヘルパー関数
pub struct ModelSelector;

impl ModelSelector {
    /// タスクに最適なモデルを推奨
    pub fn recommend(
        requires_vision: bool,
        requires_speed: bool,
        requires_max_capability: bool,
        budget_conscious: bool,
    ) -> Model {
        match (
            requires_vision,
            requires_speed,
            requires_max_capability,
            budget_conscious,
        ) {
            // ビジョンが必要で高速性も必要
            (true, true, _, _) => Model::Claude3Haiku,
            // ビジョンが必要で最高性能も必要
            (true, _, true, _) => Model::Claude3Opus,
            // ビジョンが必要（バランス重視）
            (true, _, _, _) => Model::Claude35Sonnet,
            // 高速性と予算を重視
            (_, true, _, true) => Model::Claude3Haiku,
            // 最高性能が必要
            (_, _, true, _) => Model::Claude3Opus,
            // 高速性を重視
            (_, true, _, _) => Model::Claude3Haiku,
            // デフォルト（バランス重視）
            _ => Model::Claude35Sonnet,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_display() {
        assert_eq!(Model::Claude3Opus.to_string(), "claude-3-opus-20240229");
        assert_eq!(
            Model::Claude35Sonnet.to_string(),
            "claude-3-5-sonnet-20241022"
        );
    }

    #[test]
    fn test_model_from_str() {
        assert_eq!(Model::from_str("claude-3-opus"), Some(Model::Claude3Opus));
        assert_eq!(
            Model::from_str("claude-3.5-sonnet"),
            Some(Model::Claude35Sonnet)
        );
        assert_eq!(Model::from_str("unknown-model"), None);
    }

    #[test]
    fn test_model_properties() {
        let opus = Model::Claude3Opus;
        assert_eq!(opus.context_window(), 200_000);
        assert_eq!(opus.max_output_tokens(), 4_096);
        assert!(opus.supports_vision());
        assert!(opus.supports_function_calling());
        assert_eq!(opus.speed_rating(), 2);
        assert_eq!(opus.capability_rating(), 5);

        let haiku = Model::Claude3Haiku;
        assert_eq!(haiku.speed_rating(), 5);
        assert_eq!(haiku.cost_rating(), 1);
    }

    #[test]
    fn test_model_selector() {
        // ビジョンと高速性が必要
        assert_eq!(
            ModelSelector::recommend(true, true, false, false),
            Model::Claude3Haiku
        );

        // 最高性能が必要
        assert_eq!(
            ModelSelector::recommend(false, false, true, false),
            Model::Claude3Opus
        );

        // バランス重視（デフォルト）
        assert_eq!(
            ModelSelector::recommend(false, false, false, false),
            Model::Claude35Sonnet
        );
    }

    #[test]
    fn test_model_lists() {
        let all = Model::all();
        assert_eq!(all.len(), 7);

        let latest = Model::latest_generation();
        assert_eq!(latest.len(), 4);

        let vision = Model::vision_capable();
        assert_eq!(vision.len(), 4);
    }
}
