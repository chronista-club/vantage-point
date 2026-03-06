# Capability Module - Stand能力の拡張システム

JoJoスタンドの世界観から着想を得た、Stand能力の成長・進化システム。

## モジュール構成

```
capability/
├── mod.rs          # モジュールエントリーポイント
├── types.rs        # 能力の分類体系（実行モデル、自律性、データフロー等）
├── params.rs       # 能力のパラメータ評価（A〜Eランク、6パラメータ）
├── evolution.rs    # 能力の成長・進化システム（ACT進化、レクイエム、覚醒）
└── README.md       # このファイル
```

## 概要

### 1. 能力の分類体系 (types.rs)

5つの軸で能力を多次元的に分類:

- **実行モデル**: Sync, Async, Stream, Batch, EventDriven
- **自律性レベル**: Manual, Suggestive, SemiAutonomous, FullyAutonomous, Reactive
- **データフロー**: Input, Output, Bidirectional, Transform
- **統合形態**: Standalone, Collaborative, Dependent, Extension, Bridge
- **動作範囲**: Local, Host, Network, Global

```rust
use capability::types::CapabilityType;

// MIDI入力能力の型定義
let midi = CapabilityType::midi_input();
assert!(midi.is_realtime());
assert!(midi.is_collaborative());
```

### 2. 能力のパラメータ評価 (params.rs)

JoJoスタンドの6パラメータをAI能力向けに再定義（A〜Eランク）:

- **破壊力 (Power)**: 能力の影響力・変更範囲
- **スピード (Speed)**: 応答速度・実行速度
- **射程距離 (Range)**: 能力の適用範囲・統合度
- **持続力 (Stamina)**: 継続動作時間・セッション寿命
- **精密動作性 (Precision)**: 制御精度・エラー率
- **成長性 (Potential)**: 拡張性・学習可能性

```rust
use capability::params::{CapabilityParams, Rank, MIDI_CAPABILITY_PARAMS};

// MIDI能力のパラメータ
let params = MIDI_CAPABILITY_PARAMS;
println!("{}", params);
// Power:     D (破壊力)
// Speed:     A (スピード)
// Range:     C (射程距離)
// Stamina:   A (持続力)
// Precision: B (精密動作性)
// Potential: B (成長性)
// Total Score: 23/30
```

### 3. 能力の成長・進化システム (evolution.rs)

#### ACT進化（段階的成長）

使用回数・成功率に応じて段階的にレベルアップ。

```rust
use capability::evolution::*;

let mut state = EvolutionState::default();
assert_eq!(state.level, EvolutionLevel::ACT1);

// 使用を記録
for _ in 0..50 {
    state.record_use(true);
}

// レベルアップ条件をチェック
let condition = EvolutionCondition {
    min_uses: 50,
    min_success_rate: 0.7,
    min_days: Some(3),
    min_training_score: None,
    custom: HashMap::new(),
};

if state.try_level_up(&condition) {
    println!("Level up! Now: {}", state.level.display_name());
}
```

#### レクイエム進化（質的変化）

特殊イベントで元の能力とは異なる新しい能力を獲得。

```rust
// MidiCapability + AudioCapability → AudioSyncCapability
state.apply_requiem(
    RequiemType::new("AudioSyncCapability"),
    "音楽との完全同期能力を獲得".to_string()
);
```

#### 覚醒（一時的ブースト）

極限状況で隠された能力が発現し、一時的に性能向上。

```rust
// 高負荷時の覚醒
state.awaken(
    AwakeningKind::Crisis,
    Duration::from_secs(60),
    1.5  // 1.5倍のブースト
);

if state.is_awakened() {
    let boost = state.current_boost();
    println!("Awakening active! Boost: {}x", boost);
}
```

#### 訓練（数値的改善）

継続使用で4つのパラメータが向上。

```rust
state.train(TrainingCategory::Accuracy, 0.1);
state.train(TrainingCategory::Speed, 0.05);

let overall = state.training.overall_score();
println!("Training score: {:.1}%", overall * 100.0);
```

## 使用例: MidiCapability

### 進化パス定義

```rust
let path = midi_capability_evolution_path();

// ACT1 → ACT2: MIDI出力とLED制御
// - 50回使用、成功率70%以上、3日間以上

// ACT2 → ACT3: SysEx制御
// - 200回使用、成功率80%以上、7日間以上、訓練スコア0.6以上

// ACT3 → ACT4: 複数デバイス同時制御
// - 500回使用、成功率90%以上、14日間以上、訓練スコア0.8以上
```

### 実装統合

```rust
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

## データ永続化

進化状態は設定ファイルに保存され、再起動後も継続。

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
```

## WebUIでの可視化

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

## 設計原則

1. **自律的成長**: AIが自動的に能力を学習・進化させる
2. **透明性**: ユーザーは成長過程を可視化できる
3. **コミュニティ駆動**: 進化パスをカスタマイズ可能
4. **段階的拡張**: Phase 1(トレイト) → Phase 2(プロトコル) → Phase 3(プラグイン)

## 関連ドキュメント

- [docs/spec/05-capability.md](../../../../docs/spec/05-capability.md) - Capability仕様
- [docs/design/02-capability-evolution.md](../../../../docs/design/02-capability-evolution.md) - Evolution設計書

## テスト

```bash
# 全てのテストを実行
cargo test --bin vp evolution

# 特定のテストを実行
cargo test --bin vp test_evolution_level
cargo test --bin vp test_level_up
cargo test --bin vp test_awakening
```

## 将来の拡張

1. **機械学習統合**: 使用パターンから最適な進化経路を提案
2. **コミュニティ共有**: 進化パスをGitHubで共有
3. **実績システム**: 特定の進化達成で称号獲得
4. **進化の可視化**: 3Dグラフで成長を表現
