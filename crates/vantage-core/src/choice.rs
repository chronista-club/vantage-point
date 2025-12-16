//! AI主導の選択肢UIモデル
//!
//! 基本パターン: AIが選択肢を提示 → ユーザーが選ぶ

use serde::{Deserialize, Serialize};

/// 選択肢の一つ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    /// 選択肢のID (A, B, C など)
    pub id: String,
    /// 表示テキスト
    pub label: String,
    /// 詳細説明（オプション）
    pub description: Option<String>,
}

impl Choice {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
        }
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

/// AIからの選択肢提示
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoicePrompt {
    /// プロンプトメッセージ
    pub message: String,
    /// 選択肢リスト
    pub choices: Vec<Choice>,
    /// テキスト入力を許可するか
    pub allow_text_input: bool,
}

impl ChoicePrompt {
    pub fn new(message: impl Into<String>, choices: Vec<Choice>) -> Self {
        Self {
            message: message.into(),
            choices,
            allow_text_input: true, // デフォルトで許可
        }
    }

    /// クイック選択肢を作成（A/B/C形式）
    pub fn quick(message: impl Into<String>, options: &[&str]) -> Self {
        let choices = options
            .iter()
            .enumerate()
            .map(|(i, label)| {
                let id = (b'A' + i as u8) as char;
                Choice::new(id.to_string(), *label)
            })
            .collect();

        Self::new(message, choices)
    }
}

/// ユーザーの選択結果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UserResponse {
    /// 選択肢を選んだ
    Choice { id: String },
    /// テキストを入力した
    Text { content: String },
    /// キャンセル
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quick_choices() {
        let prompt = ChoicePrompt::quick(
            "次のステップは？",
            &["テストを書く", "リファクタリング", "次の機能へ"],
        );

        assert_eq!(prompt.choices.len(), 3);
        assert_eq!(prompt.choices[0].id, "A");
        assert_eq!(prompt.choices[1].id, "B");
        assert_eq!(prompt.choices[2].id, "C");
    }
}
