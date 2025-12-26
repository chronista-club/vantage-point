//! Capability Evolution System - Stand能力の成長と進化
//!
//! JoJoスタンドのACT進化システムを参考に、Stand Capabilityが段階的に成長する仕組みを実装。
//!
//! ## 成長の種類
//!
//! 1. **ACT進化** (Level-based Evolution)
//!    - 段階的に能力が追加される（ACT1 → 2 → 3 → 4）
//!    - 使用回数や成功率で自動的にレベルアップ
//!    - 例: MidiCapability ACT1（入力のみ） → ACT2（出力+LED） → ACT3（SysEx制御）
//!
//! 2. **レクイエム進化** (Requiem Evolution)
//!    - 特殊イベントで質的変化が起こる
//!    - 元の能力とは異なる新しい能力を獲得
//!    - 例: MidiCapability → AudioSyncCapability（音楽との同期）
//!
//! 3. **覚醒** (Awakening)
//!    - 極限状況で隠された能力が発現
//!    - 一時的なブースト効果
//!    - 例: 高負荷時のパフォーマンス向上
//!
//! 4. **訓練** (Training)
//!    - 継続使用でパラメータが向上
//!    - 精度、速度、安定性などの数値的改善
//!
//! ## 設計思想
//!
//! - **自律的成長**: AIが自動的に能力を学習・進化させる
//! - **透明性**: ユーザーは成長過程を可視化できる
//! - **コミュニティ駆動**: 進化パスをカスタマイズ可能

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

// =============================================================================
// Core Evolution Types
// =============================================================================

/// 能力の進化段階（ACT相当）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EvolutionLevel(pub u8);

impl EvolutionLevel {
    pub const ACT1: Self = Self(1);
    pub const ACT2: Self = Self(2);
    pub const ACT3: Self = Self(3);
    pub const ACT4: Self = Self(4);

    /// 次のレベルを取得（最大レベルの場合はNone）
    pub fn next(self) -> Option<Self> {
        if self.0 < 255 {
            Some(Self(self.0 + 1))
        } else {
            None
        }
    }

    /// レベルを名前で表現
    pub fn display_name(&self) -> String {
        format!("ACT {}", self.0)
    }
}

/// 能力の特殊進化（レクイエム相当）
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequiemType(pub String);

impl RequiemType {
    /// 標準的なレクイエム進化
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// レクイエム進化が適用されているか
    pub fn is_evolved(&self) -> bool {
        !self.0.is_empty()
    }
}

/// 覚醒状態（一時的なブースト）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwakeningState {
    /// 覚醒の種類
    pub kind: AwakeningKind,
    /// 覚醒した時刻
    pub activated_at: SystemTime,
    /// 覚醒の持続時間
    pub duration: Duration,
    /// ブースト倍率 (1.0 = 通常, 2.0 = 2倍)
    pub boost_multiplier: f64,
}

impl AwakeningState {
    /// 覚醒が有効かチェック
    pub fn is_active(&self) -> bool {
        SystemTime::now()
            .duration_since(self.activated_at)
            .map(|elapsed| elapsed < self.duration)
            .unwrap_or(false)
    }
}

/// 覚醒の種類
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AwakeningKind {
    /// 極限状況での覚醒
    Crisis,
    /// ユーザーの強い意志による覚醒
    Resolve,
    /// 新しい発見による覚醒
    Discovery,
}

// =============================================================================
// Evolution Metrics - 成長を追跡する指標
// =============================================================================

/// 能力の使用統計
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageMetrics {
    /// 総使用回数
    pub total_uses: u64,
    /// 成功回数
    pub successful_uses: u64,
    /// 失敗回数
    pub failed_uses: u64,
    /// 最終使用時刻
    pub last_used: Option<SystemTime>,
    /// 初回使用時刻
    pub first_used: Option<SystemTime>,
}

impl UsageMetrics {
    /// 成功率を計算 (0.0 ~ 1.0)
    pub fn success_rate(&self) -> f64 {
        if self.total_uses == 0 {
            0.0
        } else {
            self.successful_uses as f64 / self.total_uses as f64
        }
    }

    /// 使用期間（日数）
    pub fn usage_days(&self) -> Option<u64> {
        self.first_used.and_then(|first| {
            SystemTime::now()
                .duration_since(first)
                .ok()
                .map(|d| d.as_secs() / 86400)
        })
    }

    /// 記録を更新
    pub fn record_use(&mut self, success: bool) {
        self.total_uses += 1;
        if success {
            self.successful_uses += 1;
        } else {
            self.failed_uses += 1;
        }

        let now = SystemTime::now();
        self.last_used = Some(now);
        if self.first_used.is_none() {
            self.first_used = Some(now);
        }
    }
}

/// 訓練による数値的改善
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingParameters {
    /// 精度パラメータ (0.0 ~ 1.0, デフォルト: 0.5)
    pub accuracy: f64,
    /// 速度パラメータ (0.0 ~ 1.0, デフォルト: 0.5)
    pub speed: f64,
    /// 安定性パラメータ (0.0 ~ 1.0, デフォルト: 0.5)
    pub stability: f64,
    /// 効率性パラメータ (0.0 ~ 1.0, デフォルト: 0.5)
    pub efficiency: f64,
}

impl Default for TrainingParameters {
    fn default() -> Self {
        Self {
            accuracy: 0.5,
            speed: 0.5,
            stability: 0.5,
            efficiency: 0.5,
        }
    }
}

impl TrainingParameters {
    /// パラメータを向上させる（上限: 1.0）
    pub fn improve(&mut self, category: TrainingCategory, amount: f64) {
        let param = match category {
            TrainingCategory::Accuracy => &mut self.accuracy,
            TrainingCategory::Speed => &mut self.speed,
            TrainingCategory::Stability => &mut self.stability,
            TrainingCategory::Efficiency => &mut self.efficiency,
        };
        *param = (*param + amount).min(1.0);
    }

    /// 総合スコア（4つの平均）
    pub fn overall_score(&self) -> f64 {
        (self.accuracy + self.speed + self.stability + self.efficiency) / 4.0
    }
}

/// 訓練カテゴリ
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrainingCategory {
    Accuracy,
    Speed,
    Stability,
    Efficiency,
}

// =============================================================================
// Evolution Conditions - レベルアップ条件
// =============================================================================

/// レベルアップ条件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionCondition {
    /// 最小使用回数
    pub min_uses: u64,
    /// 最小成功率 (0.0 ~ 1.0)
    pub min_success_rate: f64,
    /// 最小使用期間（日数）
    pub min_days: Option<u64>,
    /// 必要な訓練スコア (0.0 ~ 1.0)
    pub min_training_score: Option<f64>,
    /// カスタム条件（JSONで柔軟に拡張）
    pub custom: HashMap<String, serde_json::Value>,
}

impl EvolutionCondition {
    /// 条件を満たしているかチェック
    pub fn is_satisfied(&self, metrics: &UsageMetrics, training: &TrainingParameters) -> bool {
        // 使用回数チェック
        if metrics.total_uses < self.min_uses {
            return false;
        }

        // 成功率チェック
        if metrics.success_rate() < self.min_success_rate {
            return false;
        }

        // 使用期間チェック
        if let Some(required_days) = self.min_days {
            if metrics.usage_days().unwrap_or(0) < required_days {
                return false;
            }
        }

        // 訓練スコアチェック
        if let Some(required_score) = self.min_training_score {
            if training.overall_score() < required_score {
                return false;
            }
        }

        true
    }
}

/// レクイエム進化のトリガー
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequiemTrigger {
    /// トリガーの種類
    pub kind: RequiemTriggerKind,
    /// トリガーの説明
    pub description: String,
    /// 新しい能力名
    pub new_capability_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RequiemTriggerKind {
    /// 特定のイベント発生
    Event(String),
    /// 特定の閾値到達
    Threshold { metric: String, value: f64 },
    /// ユーザーの明示的な選択
    UserChoice,
    /// 他の能力との組み合わせ
    Combination(Vec<String>),
}

// =============================================================================
// Evolution State - 進化の状態管理
// =============================================================================

/// 能力の進化状態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionState {
    /// 現在のレベル
    pub level: EvolutionLevel,
    /// レクイエム進化の種類
    pub requiem: Option<RequiemType>,
    /// 覚醒状態
    pub awakening: Option<AwakeningState>,
    /// 使用統計
    pub metrics: UsageMetrics,
    /// 訓練パラメータ
    pub training: TrainingParameters,
    /// レベルアップの履歴
    pub evolution_history: Vec<EvolutionEvent>,
}

impl Default for EvolutionState {
    fn default() -> Self {
        Self {
            level: EvolutionLevel::ACT1,
            requiem: None,
            awakening: None,
            metrics: UsageMetrics::default(),
            training: TrainingParameters::default(),
            evolution_history: Vec::new(),
        }
    }
}

impl EvolutionState {
    /// 使用記録を追加
    pub fn record_use(&mut self, success: bool) {
        self.metrics.record_use(success);
    }

    /// 訓練パラメータを向上
    pub fn train(&mut self, category: TrainingCategory, amount: f64) {
        self.training.improve(category, amount);
    }

    /// レベルアップを試行
    pub fn try_level_up(&mut self, condition: &EvolutionCondition) -> bool {
        if condition.is_satisfied(&self.metrics, &self.training) {
            if let Some(next_level) = self.level.next() {
                self.level = next_level;
                self.evolution_history.push(EvolutionEvent {
                    timestamp: SystemTime::now(),
                    kind: EvolutionEventKind::LevelUp {
                        from: self.level,
                        to: next_level,
                    },
                });
                true
            } else {
                false // 既に最大レベル
            }
        } else {
            false
        }
    }

    /// レクイエム進化を適用
    pub fn apply_requiem(&mut self, requiem: RequiemType, description: String) {
        self.requiem = Some(requiem.clone());
        self.evolution_history.push(EvolutionEvent {
            timestamp: SystemTime::now(),
            kind: EvolutionEventKind::Requiem {
                requiem_type: requiem,
                description,
            },
        });
    }

    /// 覚醒状態を発動
    pub fn awaken(&mut self, kind: AwakeningKind, duration: Duration, boost: f64) {
        self.awakening = Some(AwakeningState {
            kind,
            activated_at: SystemTime::now(),
            duration,
            boost_multiplier: boost,
        });
        self.evolution_history.push(EvolutionEvent {
            timestamp: SystemTime::now(),
            kind: EvolutionEventKind::Awakening { kind, boost },
        });
    }

    /// 覚醒が有効かチェック
    pub fn is_awakened(&self) -> bool {
        self.awakening
            .as_ref()
            .map(|a| a.is_active())
            .unwrap_or(false)
    }

    /// 現在の実効ブースト倍率
    pub fn current_boost(&self) -> f64 {
        if let Some(awakening) = &self.awakening {
            if awakening.is_active() {
                return awakening.boost_multiplier;
            }
        }
        1.0
    }
}

/// 進化イベント（履歴記録用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionEvent {
    pub timestamp: SystemTime,
    pub kind: EvolutionEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvolutionEventKind {
    LevelUp {
        from: EvolutionLevel,
        to: EvolutionLevel,
    },
    Requiem {
        requiem_type: RequiemType,
        description: String,
    },
    Awakening {
        kind: AwakeningKind,
        boost: f64,
    },
}

// =============================================================================
// Evolution Path - 能力ごとの成長経路定義
// =============================================================================

/// 能力の進化パス定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionPath {
    /// 能力名
    pub capability_name: String,
    /// 各レベルの条件
    pub level_conditions: HashMap<EvolutionLevel, EvolutionCondition>,
    /// 利用可能なレクイエム進化
    pub requiem_options: Vec<RequiemTrigger>,
    /// 覚醒トリガー
    pub awakening_triggers: Vec<AwakeningTrigger>,
}

impl EvolutionPath {
    /// 次のレベルへの条件を取得
    pub fn next_level_condition(&self, current: EvolutionLevel) -> Option<&EvolutionCondition> {
        current
            .next()
            .and_then(|next| self.level_conditions.get(&next))
    }
}

/// 覚醒トリガー
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwakeningTrigger {
    pub kind: AwakeningKind,
    pub condition: String,
    pub duration: Duration,
    pub boost: f64,
}

// =============================================================================
// Example: MidiCapability Evolution Path
// =============================================================================

/// MidiCapabilityの進化パス例
pub fn midi_capability_evolution_path() -> EvolutionPath {
    let mut level_conditions = HashMap::new();

    // ACT1 → ACT2: MIDI出力とLED制御を習得
    level_conditions.insert(
        EvolutionLevel::ACT2,
        EvolutionCondition {
            min_uses: 50,
            min_success_rate: 0.7,
            min_days: Some(3),
            min_training_score: None,
            custom: HashMap::new(),
        },
    );

    // ACT2 → ACT3: SysEx制御を習得
    level_conditions.insert(
        EvolutionLevel::ACT3,
        EvolutionCondition {
            min_uses: 200,
            min_success_rate: 0.8,
            min_days: Some(7),
            min_training_score: Some(0.6),
            custom: HashMap::new(),
        },
    );

    // ACT3 → ACT4: 複数デバイス同時制御
    level_conditions.insert(
        EvolutionLevel::ACT4,
        EvolutionCondition {
            min_uses: 500,
            min_success_rate: 0.9,
            min_days: Some(14),
            min_training_score: Some(0.8),
            custom: HashMap::new(),
        },
    );

    // レクイエム進化: AudioSyncCapability（音楽との同期）
    let requiem_options = vec![RequiemTrigger {
        kind: RequiemTriggerKind::Combination(vec![
            "MidiCapability".to_string(),
            "AudioCapability".to_string(),
        ]),
        description: "音楽との完全同期能力を獲得".to_string(),
        new_capability_name: "AudioSyncCapability".to_string(),
    }];

    // 覚醒トリガー: 高負荷時のパフォーマンス向上
    let awakening_triggers = vec![AwakeningTrigger {
        kind: AwakeningKind::Crisis,
        condition: "MIDI入力レート > 100 events/sec".to_string(),
        duration: Duration::from_secs(60),
        boost: 1.5,
    }];

    EvolutionPath {
        capability_name: "MidiCapability".to_string(),
        level_conditions,
        requiem_options,
        awakening_triggers,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evolution_level() {
        assert_eq!(EvolutionLevel::ACT1.next(), Some(EvolutionLevel::ACT2));
        assert_eq!(EvolutionLevel::ACT2.display_name(), "ACT 2");
    }

    #[test]
    fn test_usage_metrics() {
        let mut metrics = UsageMetrics::default();
        metrics.record_use(true);
        metrics.record_use(true);
        metrics.record_use(false);

        assert_eq!(metrics.total_uses, 3);
        assert_eq!(metrics.successful_uses, 2);
        assert_eq!(metrics.success_rate(), 2.0 / 3.0);
    }

    #[test]
    fn test_evolution_condition() {
        let condition = EvolutionCondition {
            min_uses: 10,
            min_success_rate: 0.7,
            min_days: None,
            min_training_score: None,
            custom: HashMap::new(),
        };

        let mut metrics = UsageMetrics::default();
        for _ in 0..10 {
            metrics.record_use(true);
        }

        let training = TrainingParameters::default();
        assert!(condition.is_satisfied(&metrics, &training));
    }

    #[test]
    fn test_level_up() {
        let mut state = EvolutionState::default();
        assert_eq!(state.level, EvolutionLevel::ACT1);

        let condition = EvolutionCondition {
            min_uses: 5,
            min_success_rate: 0.5,
            min_days: None,
            min_training_score: None,
            custom: HashMap::new(),
        };

        // 条件を満たすまで使用
        for _ in 0..5 {
            state.record_use(true);
        }

        // レベルアップ
        assert!(state.try_level_up(&condition));
        assert_eq!(state.level, EvolutionLevel::ACT2);
    }

    #[test]
    fn test_training_improvement() {
        let mut training = TrainingParameters::default();
        training.improve(TrainingCategory::Accuracy, 0.3);
        assert_eq!(training.accuracy, 0.8);

        // 上限チェック
        training.improve(TrainingCategory::Accuracy, 0.5);
        assert_eq!(training.accuracy, 1.0);
    }

    #[test]
    fn test_awakening() {
        let mut state = EvolutionState::default();
        state.awaken(AwakeningKind::Crisis, Duration::from_secs(10), 2.0);

        assert!(state.is_awakened());
        assert_eq!(state.current_boost(), 2.0);
    }
}
