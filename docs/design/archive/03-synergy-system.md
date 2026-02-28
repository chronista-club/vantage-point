# Stand Capability Synergy System 設計書

## 概要

Stand Capability間の連携システム（Synergy System）の設計。
JoJo's Bizarre Adventure のスタンド能力連携にインスパイアされ、複数の能力が協調して動作することで相乗効果を生み出す仕組み。

## 背景

### JoJoスタンド連携パターン

JoJoスタンドは単独でも強力だが、複数のスタンドが連携することでさらに強力な効果を発揮する。

1. **弱点補完型**: 一方の弱点を他方が補う
   - 例: 攻撃特化スタンド + 防御特化スタンド
2. **情報共有型**: 偵察役 + 戦闘役の分業
   - 例: ハイエロファント・グリーン（索敵）+ スタープラチナ（攻撃）
3. **条件達成型**: 前提条件を他者が作る
   - 例: DIOの時間停止中に攻撃を仕込む
4. **相性システム**: 連携効率、禁忌組み合わせ
   - 例: 炎と氷、光と闇

### Vantage Pointへの適用

Stand Capabilityも同様に連携することで開発体験を向上させる。

- MIDI入力 + Claude Agent: 物理ボタンでAI操作
- WebSocket + WebView UI: リアルタイムな双方向通信
- Session管理 + Claude Agent: 複数セッションの切り替え

## アーキテクチャ

### 主要コンポーネント

```
┌─────────────────────────────────────────────┐
│         SynergyEngine                       │
├─────────────────────────────────────────────┤
│ - 能力の登録・管理                           │
│ - 相性分析・スコア計算                       │
│ - 依存関係チェック                           │
│ - 最適組み合わせの提案                       │
└─────────────────────────────────────────────┘
        ↓ uses
┌─────────────────────────────────────────────┐
│     CapabilityMetadata                      │
├─────────────────────────────────────────────┤
│ - id: 能力の識別子                           │
│ - provides: 提供する機能タグ                │
│ - requires: 必要とする機能タグ              │
│ - synergizes_with: 相性良好タグ            │
│ - conflicts_with: 相性不良タグ             │
│ - forbidden_with: 禁忌組み合わせ           │
└─────────────────────────────────────────────┘
        ↓ analyzed into
┌─────────────────────────────────────────────┐
│      SynergyAnalysis                        │
├─────────────────────────────────────────────┤
│ - compatibility: 相性スコア (0-100)         │
│ - dependencies_met: 依存関係充足            │
│ - synergy_type: 連携タイプ                  │
│ - description: 連携の説明                   │
└─────────────────────────────────────────────┘
```

### CapabilityTag

能力が提供/要求する機能を分類するタグシステム。

| カテゴリ | タグ例 |
|---------|--------|
| Input/Output | UserInput, HardwareTrigger, VisualFeedback, AudioFeedback |
| AI/Agent | AiAgent, NaturalLanguage, CodeGeneration, Reasoning |
| Development | CodeExecution, FileSystem, GitOperations |
| Communication | WebSocket, HttpApi, Ipc |
| State Management | SessionManagement, PersistentStorage |
| UI/UX | WebViewUi, CliOutput, Notification |
| Context Awareness | ProjectContext, CodebaseAnalysis |

### SynergyType

連携のタイプを分類。

| タイプ | 説明 | 例 |
|--------|------|-----|
| Complementary | 弱点補完型 | A能力の欠点をB能力が補う |
| InformationSharing | 情報共有型 | 偵察役と戦闘役の分業 |
| Prerequisite | 条件達成型 | A能力がB能力の前提条件を提供 |
| Amplification | 増幅型 | 両方が相乗効果で強化 |
| Independent | 独立型 | 連携なし（並列実行のみ） |
| Conflicting | 競合型 | 同時使用で効率低下 |
| Forbidden | 禁忌型 | 絶対に同時使用不可 |

### 相性スコア計算ロジック

```
ベーススコア = 50

+25: B能力がA能力の依存を満たす
+25: A能力がB能力の依存を満たす
+10〜30: 相性良好タグのマッチ数（1マッチ=+10、最大+30）
-15〜45: 競合タグのマッチ数（1マッチ=-15）

最終スコア = min(100, ベーススコア + ボーナス - ペナルティ)
```

| スコア範囲 | 評価 |
|-----------|------|
| 90-100 | 完璧な連携 |
| 75-89 | 非常に相性が良い |
| 50-74 | 相性が良い |
| 25-49 | 中立的な関係 |
| 1-24 | 相性が悪い |
| 0 | 連携不可（禁忌） |

## 実装例

### MIDI入力 + Claude Agent連携

```rust
use vantage_point::capability::synergy::*;

fn main() {
    let mut engine = SynergyEngine::new();

    // MIDI入力能力を登録
    let midi = CapabilityMetadata::new("midi_input", "MIDI Input")
        .provides(vec![
            CapabilityTag::UserInput,
            CapabilityTag::HardwareTrigger,
        ])
        .synergizes_with(vec![
            CapabilityTag::AiAgent,
            CapabilityTag::VisualFeedback,
        ]);

    // Claude Agent能力を登録
    let agent = CapabilityMetadata::new("claude_agent", "Claude Agent")
        .provides(vec![
            CapabilityTag::AiAgent,
            CapabilityTag::NaturalLanguage,
        ])
        .requires(vec![
            CapabilityTag::UserInput,
        ])
        .synergizes_with(vec![
            CapabilityTag::FileSystem,
            CapabilityTag::GitOperations,
        ]);

    engine.register(midi);
    engine.register(agent);

    // 連携を分析
    let analysis = engine.analyze("midi_input", "claude_agent").unwrap();

    println!("相性スコア: {}/100", analysis.compatibility);
    println!("連携タイプ: {:?}", analysis.synergy_type);
    println!("説明: {}", analysis.description);

    // 出力例:
    // 相性スコア: 90/100
    // 連携タイプ: Prerequisite
    // 説明: 非常に相性が良い - MIDI InputがClaude Agentの前提条件を提供
}
```

### WebView + WebSocket連携

```rust
let webview = CapabilityMetadata::new("webview_ui", "WebView UI")
    .provides(vec![
        CapabilityTag::WebViewUi,
        CapabilityTag::VisualFeedback,
    ])
    .requires(vec![
        CapabilityTag::WebSocket,
    ]);

let websocket = CapabilityMetadata::new("websocket_comm", "WebSocket")
    .provides(vec![
        CapabilityTag::WebSocket,
        CapabilityTag::Ipc,
    ]);

engine.register(webview);
engine.register(websocket);

let analysis = engine.analyze("webview_ui", "websocket_comm").unwrap();
// 相性スコア: 75/100
// 連携タイプ: Prerequisite
```

### 禁忌組み合わせ

```rust
let capability_a = CapabilityMetadata::new("mutex_lock_a", "Mutex A")
    .provides(vec![CapabilityTag::PersistentStorage])
    .forbidden_with(vec!["mutex_lock_b".to_string()]);

let capability_b = CapabilityMetadata::new("mutex_lock_b", "Mutex B")
    .provides(vec![CapabilityTag::PersistentStorage]);

engine.register(capability_a);
engine.register(capability_b);

let analysis = engine.analyze("mutex_lock_a", "mutex_lock_b").unwrap();
assert!(analysis.is_forbidden);
assert_eq!(analysis.compatibility, 0);
// 連携タイプ: Forbidden
```

### 依存関係の解決

```rust
// Claude Agentが必要とする依存を探す
let deps = engine.find_dependencies("claude_agent");
println!("依存を満たす能力: {:?}", deps);
// 出力: ["midi_input", "keyboard_input", "voice_input"]
```

### 最適な組み合わせを提案

```rust
let suggestions = engine.suggest_combinations("claude_agent", 5);
for suggestion in suggestions {
    println!("{} + {} = {}/100",
        suggestion.capability_a,
        suggestion.capability_b,
        suggestion.compatibility
    );
}
// 出力例:
// claude_agent + midi_input = 90/100
// claude_agent + session_mgmt = 85/100
// claude_agent + webview_ui = 80/100
```

## 事前定義済み能力

### midi_capability()

MIDI入力能力

- **provides**: UserInput, HardwareTrigger
- **requires**: なし
- **synergizes_with**: AiAgent, VisualFeedback, SessionManagement

### claude_agent_capability()

Claude Agent能力

- **provides**: AiAgent, NaturalLanguage, CodeGeneration, Reasoning
- **requires**: UserInput
- **synergizes_with**: FileSystem, GitOperations, CodeExecution, ProjectContext

### webview_capability()

WebView UI能力

- **provides**: WebViewUi, VisualFeedback, Notification
- **requires**: WebSocket, HttpApi
- **synergizes_with**: AiAgent, SessionManagement
- **conflicts_with**: CliOutput

### websocket_capability()

WebSocket通信能力

- **provides**: WebSocket, Ipc
- **requires**: なし
- **synergizes_with**: WebViewUi, AiAgent

### session_management_capability()

セッション管理能力

- **provides**: SessionManagement, PersistentStorage, DevelopmentHistory
- **requires**: AiAgent
- **synergizes_with**: ProjectContext, WebViewUi

## 将来の拡張

### Phase 2: イベント駆動連携

```rust
// 能力間でイベントを発火・購読
pub trait EventEmitter {
    fn emit(&self, event: CapabilityEvent);
    fn subscribe(&mut self, event_type: &str, handler: EventHandler);
}

// 例: MIDI入力でAgentをトリガー
midi_capability.on_note_on(36, |note| {
    agent_capability.trigger_chat("Continue the task");
});
```

### Phase 3: プラグインシステム

```rust
// 外部プロセスやWASMで能力を動的ロード
let plugin = CapabilityPlugin::load("./plugins/custom_capability.wasm")?;
engine.register_plugin(plugin);
```

## テスト

```bash
# ユニットテスト実行
cargo test -p vantage-point capability::synergy

# 相性分析のテスト
cargo test synergy_analysis_midi_and_agent

# 依存関係解決のテスト
cargo test find_dependencies

# 禁忌組み合わせのテスト
cargo test synergy_analysis_forbidden
```

## 関連ドキュメント

- [Stand Capability 仕様書](../spec/05-stand-capability.md)
- [JoJo Stand参考資料](https://jojo.fandom.com/wiki/Stand)

---

*作成日: 2025-12-18*
*ステータス: Draft*
