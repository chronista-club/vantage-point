# Capability Evolution 設計書

## 概要

Capability（能力）の成長・進化システムの設計。JoJoスタンドのACT進化システムを参考に、段階的な能力拡張を実現する。

## アーキテクチャ

```
┌─────────────────────────────────────────────────────────┐
│                  Capability Evolution                    │
├─────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐     │
│  │  ACT進化    │  │  レクイエム │  │    覚醒     │     │
│  │  (Level)    │  │  (Requiem)  │  │ (Awakening) │     │
│  │             │  │             │  │             │     │
│  │ ACT1 → 2 → │  │ 質的変化    │  │ 一時ブースト│     │
│  │   3 → 4    │  │             │  │             │     │
│  └─────────────┘  └─────────────┘  └─────────────┘     │
│         ↓                ↓                ↓             │
│  ┌─────────────────────────────────────────────────┐   │
│  │           Evolution State Manager               │   │
│  │  - Usage Metrics (使用統計)                      │   │
│  │  - Training Parameters (訓練パラメータ)          │   │
│  │  - Evolution History (進化履歴)                  │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
```

## 成長の種類

### 1. ACT進化（段階的成長）

能力が段階的に機能を追加していく。

```rust
EvolutionLevel::ACT1  // 基本機能
    ↓
EvolutionLevel::ACT2  // 機能追加
    ↓
EvolutionLevel::ACT3  // 高度な機能
    ↓
EvolutionLevel::ACT4  // 最上位機能
```

**レベルアップ条件**:
- 使用回数（`min_uses`）
- 成功率（`min_success_rate`）
- 使用期間（`min_days`）
- 訓練スコア（`min_training_score`）

### 2. レクイエム進化（質的変化）

特殊イベントで元の能力とは異なる新しい能力を獲得。

```rust
MidiCapability + AudioCapability
    ↓ (組み合わせ)
AudioSyncCapability  // 音楽との完全同期
```

**トリガー種類**:
- `Event`: 特定イベント発生
- `Threshold`: 閾値到達
- `UserChoice`: ユーザー選択
- `Combination`: 他能力との組み合わせ

### 3. 覚醒（一時的ブースト）

極限状況で隠された能力が発現し、一時的に性能向上。

```rust
AwakeningKind::Crisis      // 高負荷時
AwakeningKind::Resolve     // 強い意志
AwakeningKind::Discovery   // 新発見
```

**効果**:
- 一定時間（`duration`）継続
- 性能倍率（`boost_multiplier`）で向上
- 自動的に終了

### 4. 訓練（数値的改善）

継続使用で4つのパラメータが向上。

```rust
TrainingParameters {
    accuracy: 0.5,    // 精度
    speed: 0.5,       // 速度
    stability: 0.5,   // 安定性
    efficiency: 0.5,  // 効率性
}
```

## データ構造

### EvolutionState

各Capabilityが保持する進化状態。

```rust
pub struct EvolutionState {
    pub level: EvolutionLevel,              // 現在レベル
    pub requiem: Option<RequiemType>,       // レクイエム進化
    pub awakening: Option<AwakeningState>,  // 覚醒状態
    pub metrics: UsageMetrics,              // 使用統計
    pub training: TrainingParameters,       // 訓練パラメータ
    pub evolution_history: Vec<EvolutionEvent>, // 進化履歴
}
```

### UsageMetrics

能力の使用統計を追跡。

```rust
pub struct UsageMetrics {
    pub total_uses: u64,        // 総使用回数
    pub successful_uses: u64,   // 成功回数
    pub failed_uses: u64,       // 失敗回数
    pub last_used: Option<SystemTime>,
    pub first_used: Option<SystemTime>,
}

// メソッド
metrics.success_rate()  // 成功率 (0.0 ~ 1.0)
metrics.usage_days()    // 使用日数
```

### EvolutionCondition

レベルアップに必要な条件。

```rust
pub struct EvolutionCondition {
    pub min_uses: u64,                // 最小使用回数
    pub min_success_rate: f64,        // 最小成功率
    pub min_days: Option<u64>,        // 最小使用期間
    pub min_training_score: Option<f64>, // 必要な訓練スコア
    pub custom: HashMap<String, Value>, // カスタム条件
}
```

## 使用例: MidiCapability

### 進化パス定義

```rust
use capability::evolution::*;

let path = midi_capability_evolution_path();

// ACT1 → ACT2: MIDI出力とLED制御
// - 50回使用
// - 成功率70%以上
// - 3日間以上

// ACT2 → ACT3: SysEx制御
// - 200回使用
// - 成功率80%以上
// - 7日間以上
// - 訓練スコア0.6以上

// ACT3 → ACT4: 複数デバイス同時制御
// - 500回使用
// - 成功率90%以上
// - 14日間以上
// - 訓練スコア0.8以上
```

### 実装統合

```rust
// MidiCapability内部
pub struct MidiCapability {
    config: MidiConfig,
    evolution: EvolutionState,
    // ... その他のフィールド
}

impl MidiCapability {
    pub async fn handle_midi_event(&mut self, event: MidiEvent) -> Result<()> {
        // イベント処理
        let result = self.process_event(&event).await;

        // 進化状態を更新
        self.evolution.record_use(result.is_ok());

        // 覚醒中はブーストを適用
        let boost = self.evolution.current_boost();
        if boost > 1.0 {
            tracing::info!("Awakening boost active: {}x", boost);
        }

        // レベルアップをチェック
        if let Some(condition) = self.get_next_level_condition() {
            if self.evolution.try_level_up(condition) {
                tracing::info!("Level up! Now: {}", self.evolution.level.display_name());
                self.on_level_up().await?;
            }
        }

        result
    }

    async fn on_level_up(&mut self) -> Result<()> {
        match self.evolution.level {
            EvolutionLevel::ACT2 => {
                // MIDI出力機能を有効化
                self.enable_output()?;
            }
            EvolutionLevel::ACT3 => {
                // SysEx制御を有効化
                self.enable_sysex()?;
            }
            EvolutionLevel::ACT4 => {
                // 複数デバイス制御を有効化
                self.enable_multi_device()?;
            }
            _ => {}
        }
        Ok(())
    }
}
```

### 訓練システム

```rust
impl MidiCapability {
    pub fn train_from_usage(&mut self, event_result: &MidiEventResult) {
        // 成功率に応じて精度を向上
        if event_result.is_success() {
            self.evolution.train(TrainingCategory::Accuracy, 0.01);
        }

        // レスポンス時間に応じて速度を向上
        if event_result.response_time < Duration::from_millis(10) {
            self.evolution.train(TrainingCategory::Speed, 0.01);
        }

        // 連続成功に応じて安定性を向上
        if event_result.consecutive_successes > 10 {
            self.evolution.train(TrainingCategory::Stability, 0.01);
        }
    }
}
```

### 覚醒トリガー

```rust
impl MidiCapability {
    pub async fn check_awakening(&mut self) {
        // 高負荷時の覚醒
        if self.input_rate > 100.0 {  // 100 events/sec
            self.evolution.awaken(
                AwakeningKind::Crisis,
                Duration::from_secs(60),
                1.5  // 1.5倍のブースト
            );
            tracing::warn!("Crisis awakening! High input rate detected.");
        }
    }
}
```

## UI 表示（Canvas / TUI）

進化状態の表示例:

```
┌─────────────────────────────────────┐
│ MidiCapability - ACT 2              │
├─────────────────────────────────────┤
│ Status: Active (Awakened 🔥)        │
│ Usage: 127 times (85% success)      │
│ Days: 5                             │
│                                     │
│ Training Progress:                  │
│ ━━━━━━━━━━ Accuracy:   80%         │
│ ━━━━━━━━░░ Speed:      75%         │
│ ━━━━━━━░░░ Stability:  70%         │
│ ━━━━━━━━━░ Efficiency: 85%         │
│                                     │
│ Next Level (ACT 3):                 │
│ ━━━━━━░░░░░░░ 63% (127/200 uses)   │
│ ✓ Success rate: 85% (need 80%)     │
│ ✗ Days: 5 (need 7)                 │
└─────────────────────────────────────┘
```

## 永続化

### ファイル保存

```toml
# ~/.config/vantage/capabilities/midi.toml
[evolution]
level = 2
requiem = ""
last_updated = 2024-12-18T10:30:00Z

[metrics]
total_uses = 127
successful_uses = 108
failed_uses = 19
first_used = 2024-12-13T08:00:00Z
last_used = 2024-12-18T10:29:55Z

[training]
accuracy = 0.80
speed = 0.75
stability = 0.70
efficiency = 0.85

[[history]]
timestamp = 2024-12-15T14:22:00Z
kind = "LevelUp"
from = 1
to = 2
```

## 拡張性

### カスタム進化パス

コミュニティが独自の進化パスを定義できる。

```rust
// プラグイン作者が定義
pub fn my_custom_capability_path() -> EvolutionPath {
    EvolutionPath {
        capability_name: "CustomCapability".to_string(),
        level_conditions: custom_conditions(),
        requiem_options: custom_requiem(),
        awakening_triggers: custom_awakening(),
    }
}
```

### JSON設定

```json
{
  "capability": "CustomCapability",
  "evolution": {
    "act2": {
      "min_uses": 100,
      "min_success_rate": 0.75,
      "custom": {
        "special_condition": "user_rating > 4.5"
      }
    }
  }
}
```

## 将来の拡張

1. **機械学習統合**: 使用パターンから最適な進化経路を提案
2. **コミュニティ共有**: 進化パスをGitHubで共有
3. **実績システム**: 特定の進化達成で称号獲得
4. **進化の可視化**: 3Dグラフで成長を表現

## 関連

- [05-capability.md](../spec/05-capability.md) - Capability仕様
- [evolution.rs](crates/vantage-point/src/capability/evolution.rs) - 実装
