# Stand Capability Type System - 能力分類体系

## 概要

Stand Capability（スタンド能力）を多次元的に分類するための型システム。
JoJoの奇妙な冒険のスタンド分類を参考に、AIエージェント能力に適した軸で整理する。

## 設計背景

### JoJoスタンドの分類軸

JoJoスタンドは以下の軸で分類される:

| 分類軸 | 種類 |
|--------|------|
| **射程距離** | 近距離パワー型、遠隔操作型、自動操縦型 |
| **操作法** | 任意型、半自律型、自動操縦型、独り歩き型 |
| **形態** | 人型、群体型、物質同化型、装着型 |

### AIエージェント能力への転用

AIエージェント能力は以下の特性を持つ:

- **実行モデル**: 同期/非同期、リアルタイム性、スループット
- **自律性**: ユーザー主導/AI主導、判断の委任度
- **データフロー**: 入力/出力/双方向、状態の持ち方
- **統合形態**: スタンドアロン/協調/依存、他の能力との関係
- **動作範囲**: ローカル/ホスト/ネットワーク/グローバル

## 型定義

### CapabilityType構造体

```rust
pub struct CapabilityType {
    /// 実行モデル
    pub execution: ExecutionModel,

    /// 自律性レベル
    pub autonomy: AutonomyLevel,

    /// データフロー方向
    pub data_flow: DataFlowDirection,

    /// 統合形態
    pub integration: IntegrationMode,

    /// 動作範囲
    pub range: OperationalRange,
}
```

## 分類軸の詳細

### 1. ExecutionModel（実行モデル）

能力がどのように実行されるか。

| 種類 | 説明 | レイテンシ | スループット | 適用例 |
|------|------|-----------|-------------|--------|
| `Sync` | 同期実行 | 低〜中 | 低〜中 | 設定取得、状態確認 |
| `Async` | 非同期実行 | 低（応答）<br>中〜高（完了） | 高 | API呼び出し、Claude Agent |
| `Stream` | リアルタイムストリーム | 極低 | 高 | MIDI入力、WebSocket |
| `Batch` | バッチ処理 | 高 | 極高 | ログ解析、レポート生成 |
| `EventDriven` | イベント駆動 | 低〜中 | 可変 | Git hook、ファイル監視 |

### 2. AutonomyLevel（自律性レベル）

ユーザーとAIの間でどの程度の判断を委任するか。

| 種類 | 説明 | 制約 | 強み | 適用例 |
|------|------|------|------|--------|
| `Manual` | 手動操作型 | ユーザー不在時は動作しない | 完全な制御、予測可能性 | ファイル操作、コマンド実行 |
| `Suggestive` | 提案型 | 選択肢生成にAI推論が必要 | 意思決定支援、学習機会 | コード修正候補、次のアクション推薦 |
| `SemiAutonomous` | 半自律型 | 判断基準の設定が必要 | 効率的、定型作業の自動化 | テスト実行&修正、依存関係更新 |
| `FullyAutonomous` | 完全自律型 | 安全性検証が必須 | 長時間実行可能 | CI/CD、定期レポート |
| `Reactive` | リアクティブ型 | ルール設計が重要 | 即応性、運用自動化 | Git hook、ホットリロード |

### 3. DataFlowDirection（データフロー方向）

能力が入力を受け取るか、出力を提供するか。

| 種類 | 説明 | 適用例 |
|------|------|--------|
| `Input` | 入力のみ（センサー型） | MIDI入力、音声入力、ファイル監視 |
| `Output` | 出力のみ（アクチュエータ型） | ファイル書き込み、API呼び出し、MIDI出力 |
| `Bidirectional` | 双方向（プロトコル型） | WebSocket、Claude Agent対話、MCP通信 |
| `Transform` | 変換のみ（パイプライン型） | データ整形、フォーマット変換 |

### 4. IntegrationMode（統合形態）

他の能力やシステムとどのように統合されるか。

| 種類 | 説明 | 特徴 | 適用例 |
|------|------|------|--------|
| `Standalone` | スタンドアロン型 | 独立性が高い、テストしやすい | ファイル読み書き、設定管理 |
| `Collaborative` | 協調型 | 柔軟な組み合わせ、疎結合 | MIDI入力 → AG-UI表示 |
| `Dependent` | 依存型 | 密結合、特化した機能 | LPD8デバイス設定（MidiCapabilityに依存） |
| `Extension` | 装着型（拡張） | プラグイン的、機能追加 | ロギング、パフォーマンス計測 |
| `Bridge` | ブリッジ型 | 相互運用性、抽象化 | MCP-to-WebSocket、MIDI-to-HTTP |

### 5. OperationalRange（動作範囲）

能力がどこまで到達できるか。

| 種類 | 説明 | 制約 | 強み | 適用例 |
|------|------|------|------|--------|
| `Local` | ローカル | 外部リソースにアクセスできない | 高速、安全 | メモリ内計算、状態管理 |
| `Host` | ホスト | ネットワーク越しにはアクセスできない | ファイルシステム、ローカルプロセスへのアクセス | ファイル操作、ホストコマンド |
| `Network` | ネットワーク | 通信遅延、障害 | 外部API、リモートリソースの活用 | Claude API、GitHub API |
| `Global` | グローバル | 同期遅延、競合解決 | シームレスな継続、マルチデバイス | セッション同期（Mac ↔ iPad） |

## 実装例

### MIDI入力能力

```rust
let midi_input = CapabilityType {
    execution: ExecutionModel::Stream,        // リアルタイムストリーム
    autonomy: AutonomyLevel::Reactive,        // イベント駆動で自動応答
    data_flow: DataFlowDirection::Input,      // 入力専門
    integration: IntegrationMode::Collaborative, // EventBusで配信
    range: OperationalRange::Host,            // USBデバイス
};

// ヘルパー関数でも作成可能
let midi_input = CapabilityType::midi_input();

// 特性チェック
assert!(midi_input.is_realtime());
assert!(midi_input.is_collaborative());
assert!(!midi_input.requires_network());
```

### Claude Agent能力

```rust
let claude_agent = CapabilityType {
    execution: ExecutionModel::Async,            // 非同期API呼び出し
    autonomy: AutonomyLevel::Suggestive,         // 選択肢を提示
    data_flow: DataFlowDirection::Bidirectional, // 双方向対話
    integration: IntegrationMode::Standalone,    // 独立動作
    range: OperationalRange::Network,            // Claude API通信
};

// ヘルパー関数
let claude_agent = CapabilityType::claude_agent();

// 特性チェック
assert!(claude_agent.requires_ai_reasoning());
assert!(claude_agent.requires_network());
```

### WebSocket配信能力

```rust
let websocket = CapabilityType::websocket_broadcast();

assert!(websocket.is_realtime());
assert!(!websocket.requires_ai_reasoning());
assert_eq!(websocket.integration, IntegrationMode::Bridge);
```

## ユーティリティメソッド

### 特性判定

```rust
impl CapabilityType {
    /// AI推論・判断が必要か
    pub fn requires_ai_reasoning(&self) -> bool;

    /// リアルタイム性が要求されるか
    pub fn is_realtime(&self) -> bool;

    /// 外部通信が必要か
    pub fn requires_network(&self) -> bool;

    /// 他の能力と協調するか
    pub fn is_collaborative(&self) -> bool;
}
```

### 制約と強みの取得

```rust
let midi = CapabilityType::midi_input();

// 制約を列挙
let constraints = midi.constraints();
// -> ["継続的なリソース消費", "EventBusへの依存"]

// 強みを列挙
let strengths = midi.strengths();
// -> ["低レイテンシ", "即応性", "柔軟な組み合わせ"]
```

## 設計原則

### 1. 多次元分類

単一の軸ではなく、複数の視点から能力を記述することで:

- より詳細な能力の特性表現
- 動的な能力検索・フィルタリング
- 将来の拡張性（新しい軸の追加）

### 2. 型安全性

Rust型システムで不正な組み合わせを防ぐ:

```rust
// コンパイル時に型チェック
let cap = CapabilityType {
    execution: ExecutionModel::Stream,
    // ... 全てのフィールドが必須
};

// enumで不正な値を防ぐ
let autonomy = AutonomyLevel::Suggestive; // OK
// let autonomy = "suggestive"; // コンパイルエラー
```

### 3. 段階的拡張

Phase 1（トレイト型）から Phase 3（プラグイン型）への移行を見据えた設計:

- Phase 1: CapabilityTypeで静的に定義
- Phase 2: メッセージベースで動的に問い合わせ
- Phase 3: プラグインが自己記述的にTypeを提供

## 将来の拡張

### 能力検索・フィルタリング

```rust
// 全てのリアルタイム能力を検索
let realtime_capabilities = registry
    .find_by_execution(ExecutionModel::Stream)
    .collect::<Vec<_>>();

// AI推論が必要で、ネットワーク通信を行う能力
let ai_network_capabilities = registry
    .filter(|cap| {
        cap.requires_ai_reasoning() && cap.requires_network()
    })
    .collect::<Vec<_>>();
```

### 能力の組み合わせ検証

```rust
// 協調型能力はEventBusが必要
if capability.is_collaborative() {
    ensure!(event_bus.is_available(), "EventBus is required");
}

// ネットワーク能力はオフラインモードで無効化
if offline_mode && capability.requires_network() {
    return Err(Error::NetworkRequired);
}
```

## 関連ドキュメント

- [docs/spec/05-stand-capability.md](../spec/05-stand-capability.md) - Stand Capability仕様書
- [src/capability/types.rs](/Users/makoto/repos/vantage-point/crates/vantage-point/src/capability/types.rs) - 実装コード
- [src/capability/params.rs](/Users/makoto/repos/vantage-point/crates/vantage-point/src/capability/params.rs) - パラメータ評価
- [src/capability/evolution.rs](/Users/makoto/repos/vantage-point/crates/vantage-point/src/capability/evolution.rs) - 進化システム

---

*作成日: 2025-12-18*
*ステータス: Active*
