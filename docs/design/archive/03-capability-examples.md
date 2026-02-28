# Capability Type System - 実装例

## 概要

Stand Capability Type Systemの具体的な使用例を示す。
実際のVantage Point能力（MIDI、Claude Agent、WebSocket等）の分類と活用方法。

## 実装済み能力の分類

### 1. MIDI入力能力

**特徴**:
- AKAI LPD8等のMIDIコントローラーから入力を受け取る
- リアルタイムでイベントを配信
- 他の能力（AG-UI、Claude Agent）と協調動作

**CapabilityType定義**:

```rust
use vantage_point::capability::types::*;

let midi_input = CapabilityType {
    execution: ExecutionModel::Stream,        // リアルタイムストリーム処理
    autonomy: AutonomyLevel::Reactive,        // イベント駆動で自動応答
    data_flow: DataFlowDirection::Input,      // 入力専門（センサー型）
    integration: IntegrationMode::Collaborative, // EventBusで配信
    range: OperationalRange::Host,            // USBデバイス
};

// または、ヘルパー関数を使用
let midi_input = CapabilityType::midi_input();
```

**特性確認**:

```rust
// リアルタイム性: 低レイテンシが要求される
assert!(midi_input.is_realtime());

// 協調性: 他の能力とEventBusで連携
assert!(midi_input.is_collaborative());

// ネットワーク不要: ローカルUSBデバイス
assert!(!midi_input.requires_network());

// AI推論不要: 単純なイベント中継
assert!(!midi_input.requires_ai_reasoning());
```

**制約と強み**:

```rust
println!("制約:");
for constraint in midi_input.constraints() {
    println!("  - {}", constraint);
}
// 出力:
//   - 継続的なリソース消費
//   - EventBusへの依存

println!("強み:");
for strength in midi_input.strengths() {
    println!("  - {}", strength);
}
// 出力:
//   - 低レイテンシ
//   - 即応性
//   - 柔軟な組み合わせ
```

### 2. Claude Agent能力

**特徴**:
- Claude API（またはClaude CLI）と通信
- ユーザーに選択肢を提示（AI主導UI）
- 双方向対話で段階的に問題を解決

**CapabilityType定義**:

```rust
let claude_agent = CapabilityType {
    execution: ExecutionModel::Async,            // 非同期API呼び出し
    autonomy: AutonomyLevel::Suggestive,         // 提案型（選択肢を提示）
    data_flow: DataFlowDirection::Bidirectional, // 双方向対話
    integration: IntegrationMode::Standalone,    // 独立動作
    range: OperationalRange::Network,            // Claude API通信
};

// ヘルパー関数
let claude_agent = CapabilityType::claude_agent();
```

**特性確認**:

```rust
// AI推論が必要
assert!(claude_agent.requires_ai_reasoning());

// ネットワーク通信が必要
assert!(claude_agent.requires_network());

// リアルタイム性は不要（数秒の遅延は許容）
assert!(!claude_agent.is_realtime());

// 独立動作（他の能力に依存しない）
assert!(!claude_agent.is_collaborative());
```

**協調モードとの対応**:

Vantage Pointの3段階協調モードに対応:

```rust
// 協調モード: ユーザーと一緒に進める
let collaborative = CapabilityType {
    autonomy: AutonomyLevel::Suggestive, // 選択肢を提示
    ..claude_agent
};

// 委任モード: 任せて、途中経過・結果を確認
let delegated = CapabilityType {
    autonomy: AutonomyLevel::SemiAutonomous, // 重要な分岐点で確認
    ..claude_agent
};

// 自律モード: 完全に任せる
let autonomous = CapabilityType {
    autonomy: AutonomyLevel::FullyAutonomous, // 目標達成まで実行
    ..claude_agent
};
```

### 3. WebSocket配信能力

**特徴**:
- Stand内部のイベントをWebUI/AG-UIに配信
- 内部イベント（Agent応答、MIDI入力等）をWebSocketプロトコルに変換
- ブリッジとして動作

**CapabilityType定義**:

```rust
let websocket = CapabilityType {
    execution: ExecutionModel::Stream,           // リアルタイムストリーム
    autonomy: AutonomyLevel::Manual,             // 明示的なメッセージ送信
    data_flow: DataFlowDirection::Output,        // 出力専門
    integration: IntegrationMode::Bridge,        // 内部イベント → WebSocket
    range: OperationalRange::Network,            // ローカル〜ネットワーク
};

// ヘルパー関数
let websocket = CapabilityType::websocket_broadcast();
```

**特性確認**:

```rust
// リアルタイム性が要求される
assert!(websocket.is_realtime());

// AI推論は不要（単純な中継）
assert!(!websocket.requires_ai_reasoning());

// ブリッジ型（プロトコル変換）
assert_eq!(websocket.integration, IntegrationMode::Bridge);
```

### 4. ファイル監視能力

**特徴**:
- プロジェクトディレクトリのファイル変更を監視
- 変更検知時に他の能力に通知
- ホットリロード、自動テスト実行のトリガー

**CapabilityType定義**:

```rust
let file_watcher = CapabilityType {
    execution: ExecutionModel::EventDriven,      // イベント駆動
    autonomy: AutonomyLevel::Reactive,           // 自動検知&通知
    data_flow: DataFlowDirection::Input,         // 入力専門
    integration: IntegrationMode::Collaborative, // EventBusで配信
    range: OperationalRange::Host,               // ホストファイルシステム
};

// ヘルパー関数
let file_watcher = CapabilityType::file_watcher();
```

**特性確認**:

```rust
// イベント駆動（待機中はリソース消費ゼロ）
assert_eq!(file_watcher.execution, ExecutionModel::EventDriven);

// 協調型（他の能力に通知）
assert!(file_watcher.is_collaborative());

// ホスト範囲（ネットワーク不要）
assert!(!file_watcher.requires_network());
```

## 能力の組み合わせパターン

### パターン1: MIDI → AG-UI → Claude Agent

**フロー**:
```
MIDIパッド押下
  ↓ (Input, Reactive)
EventBus
  ↓ (Bridge)
WebSocket配信
  ↓ (Output, Manual)
AG-UI表示
  ↓ (User interaction)
Claude Agent実行
  ↓ (Async, Suggestive)
選択肢提示
```

**能力の連携**:

```rust
// 1. MIDI入力能力
let midi = CapabilityType::midi_input();
assert!(midi.is_collaborative()); // EventBusで配信

// 2. WebSocket配信能力
let websocket = CapabilityType::websocket_broadcast();
assert_eq!(websocket.integration, IntegrationMode::Bridge);

// 3. Claude Agent能力
let agent = CapabilityType::claude_agent();
assert!(agent.requires_ai_reasoning()); // 選択肢生成
```

### パターン2: ファイル監視 → 自動テスト → 結果通知

**フロー**:
```
ファイル保存
  ↓ (EventDriven, Reactive)
ファイル監視能力が検知
  ↓
テスト実行能力を起動
  ↓ (Async, SemiAutonomous)
テスト完了
  ↓
通知能力が結果を配信
  ↓ (Output, Manual)
AG-UIに表示 / MIDIでLED点灯
```

**能力の連携**:

```rust
// 1. ファイル監視
let watcher = CapabilityType::file_watcher();

// 2. テスト実行（半自律型）
let test_runner = CapabilityType {
    execution: ExecutionModel::Async,
    autonomy: AutonomyLevel::SemiAutonomous, // 失敗時に確認を求める
    data_flow: DataFlowDirection::Transform,
    integration: IntegrationMode::Standalone,
    range: OperationalRange::Host,
};

// 3. 通知（MIDI LED + WebSocket）
let notifier = CapabilityType {
    execution: ExecutionModel::Async,
    autonomy: AutonomyLevel::Manual,
    data_flow: DataFlowDirection::Output,
    integration: IntegrationMode::Collaborative,
    range: OperationalRange::Host,
};
```

## 将来の能力例

### 音声入力能力

```rust
let voice_input = CapabilityType {
    execution: ExecutionModel::Stream,           // リアルタイム音声ストリーム
    autonomy: AutonomyLevel::Reactive,           // 音声検知で自動起動
    data_flow: DataFlowDirection::Input,         // 入力専門
    integration: IntegrationMode::Collaborative,
    range: OperationalRange::Host,               // マイク入力
};
```

### セッション同期能力（Mac ↔ iPad）

```rust
let session_sync = CapabilityType {
    execution: ExecutionModel::Stream,           // リアルタイム同期
    autonomy: AutonomyLevel::FullyAutonomous,    // 自動同期
    data_flow: DataFlowDirection::Bidirectional, // デバイス間双方向
    integration: IntegrationMode::Bridge,        // CRDT → ネットワーク
    range: OperationalRange::Global,             // グローバル同期
};

// 特性確認
assert!(session_sync.requires_network());
assert_eq!(session_sync.range, OperationalRange::Global);
```

### Git操作能力

```rust
let git_ops = CapabilityType {
    execution: ExecutionModel::Async,            // コマンド実行
    autonomy: AutonomyLevel::SemiAutonomous,     // コミット前に確認
    data_flow: DataFlowDirection::Bidirectional, // 読み書き
    integration: IntegrationMode::Standalone,
    range: OperationalRange::Host,               // ローカルリポジトリ
};
```

## 能力検索・フィルタリング（将来）

### リアルタイム能力を検索

```rust
// 全てのリアルタイム能力を検索
let realtime_capabilities = registry
    .capabilities()
    .iter()
    .filter(|cap| cap.capability_type.is_realtime())
    .collect::<Vec<_>>();

// 結果:
// - MIDI入力
// - WebSocket配信
// - 音声入力
// - セッション同期
```

### AI推論が必要な能力を検索

```rust
let ai_capabilities = registry
    .capabilities()
    .iter()
    .filter(|cap| cap.capability_type.requires_ai_reasoning())
    .collect::<Vec<_>>();

// 結果:
// - Claude Agent（Suggestive）
// - テスト実行（SemiAutonomous）
// - セッション同期（FullyAutonomous）
```

### ネットワーク不要の能力を検索（オフラインモード）

```rust
let offline_safe = registry
    .capabilities()
    .iter()
    .filter(|cap| !cap.capability_type.requires_network())
    .collect::<Vec<_>>();

// 結果:
// - MIDI入力
// - ファイル監視
// - Git操作
// - 音声入力
```

## まとめ

CapabilityType分類体系により:

1. **能力の特性を一目で把握**: 5つの軸で多次元的に記述
2. **適切な組み合わせ**: 制約と強みを理解し、協調動作を設計
3. **動的な能力管理**: 検索・フィルタリングで柔軟な制御
4. **段階的拡張**: Phase 1（トレイト）→ Phase 2（プロトコル）→ Phase 3（プラグイン）

---

*作成日: 2025-12-18*
*関連: [03-capability-type-system.md](03-capability-type-system.md)*
