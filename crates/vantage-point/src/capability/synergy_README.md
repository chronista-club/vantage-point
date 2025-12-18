# Stand Capability Synergy System

## 概要

JoJo's Bizarre Adventure のスタンド能力連携にインスパイアされた、Stand Capability間の協調システム。

各能力は単独でも機能するが、連携することでより強力な開発体験を生み出す。

## JoJoスタンド連携パターン

1. **弱点補完型**: 一方の弱点を他方が補う
2. **情報共有型**: 偵察役 + 戦闘役の分業
3. **条件達成型**: 前提条件を他者が作る
4. **相性システム**: 連携効率、禁忌組み合わせ

## 主要コンポーネント

### CapabilityTag

能力が提供/要求する機能を分類するタグ。

```rust
pub enum CapabilityTag {
    // Input/Output
    UserInput,
    HardwareTrigger,
    VisualFeedback,
    AudioFeedback,

    // AI/Agent
    AiAgent,
    NaturalLanguage,
    CodeGeneration,
    Reasoning,

    // Development
    CodeExecution,
    FileSystem,
    GitOperations,

    // Communication
    WebSocket,
    HttpApi,
    Ipc,

    // ... その他
}
```

### CapabilityMetadata

能力のメタデータを定義。

```rust
pub struct CapabilityMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub provides: Vec<CapabilityTag>,      // 提供機能
    pub requires: Vec<CapabilityTag>,      // 依存機能
    pub synergizes_with: Vec<CapabilityTag>, // 相性良好
    pub conflicts_with: Vec<CapabilityTag>,  // 相性不良
    pub forbidden_with: Vec<String>,        // 禁忌
}
```

### SynergyEngine

能力間の相性分析・依存関係チェックを実行。

```rust
pub struct SynergyEngine {
    capabilities: HashMap<String, CapabilityMetadata>,
    synergy_cache: HashMap<(String, String), SynergyAnalysis>,
}

impl SynergyEngine {
    pub fn register(&mut self, metadata: CapabilityMetadata);
    pub fn analyze(&mut self, id_a: &str, id_b: &str) -> Option<SynergyAnalysis>;
    pub fn find_dependencies(&self, id: &str) -> Vec<String>;
    pub fn suggest_combinations(&mut self, base: &str, limit: usize) -> Vec<SynergyAnalysis>;
}
```

### SynergyAnalysis

連携の分析結果。

```rust
pub struct SynergyAnalysis {
    pub capability_a: String,
    pub capability_b: String,
    pub compatibility: u8,           // 相性スコア (0-100)
    pub dependencies_met: bool,      // 依存関係充足
    pub is_forbidden: bool,          // 禁忌組み合わせ
    pub synergy_type: SynergyType,   // 連携タイプ
    pub description: String,         // 説明
}
```

### SynergyType

連携のタイプ分類。

```rust
pub enum SynergyType {
    Complementary,      // 弱点補完型
    InformationSharing, // 情報共有型
    Prerequisite,       // 条件達成型
    Amplification,      // 増幅型
    Independent,        // 独立型
    Conflicting,        // 競合型
    Forbidden,          // 禁忌型
}
```

## 使用例

### MIDI入力 + Claude Agent連携

```rust
use vantage_point::capability::synergy::*;

let mut engine = SynergyEngine::new();

// 事前定義済み能力を登録
engine.register(midi_capability());
engine.register(claude_agent_capability());

// 連携を分析
let analysis = engine.analyze("midi_input", "claude_agent").unwrap();

println!("相性スコア: {}/100", analysis.compatibility);
// 出力: 相性スコア: 90/100

println!("連携タイプ: {:?}", analysis.synergy_type);
// 出力: 連携タイプ: Prerequisite

println!("説明: {}", analysis.description);
// 出力: 非常に相性が良い - MIDI InputがClaude Agentの前提条件を提供
```

### 依存関係の解決

```rust
let deps = engine.find_dependencies("claude_agent");
println!("依存を満たす能力: {:?}", deps);
// 出力: ["midi_input"]
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
// 出力:
// claude_agent + midi_input = 90/100
// claude_agent + session_mgmt = 85/100
// claude_agent + webview_ui = 80/100
```

### カスタム能力の定義

```rust
let voice_input = CapabilityMetadata::new("voice_input", "Voice Input")
    .with_description("音声入力でチャットメッセージを送信")
    .provides(vec![
        CapabilityTag::UserInput,
        CapabilityTag::AudioFeedback,
    ])
    .requires(vec![])
    .synergizes_with(vec![
        CapabilityTag::AiAgent,
        CapabilityTag::NaturalLanguage,
    ]);

engine.register(voice_input);

let analysis = engine.analyze("voice_input", "claude_agent").unwrap();
// 相性スコア: 85/100
```

### 禁忌組み合わせ

```rust
let mutex_a = CapabilityMetadata::new("file_lock_a", "File Lock A")
    .provides(vec![CapabilityTag::FileSystem])
    .forbidden_with(vec!["file_lock_b".to_string()]);

let mutex_b = CapabilityMetadata::new("file_lock_b", "File Lock B")
    .provides(vec![CapabilityTag::FileSystem]);

engine.register(mutex_a);
engine.register(mutex_b);

let analysis = engine.analyze("file_lock_a", "file_lock_b").unwrap();
assert!(analysis.is_forbidden);
assert_eq!(analysis.compatibility, 0);
// 連携タイプ: Forbidden
```

## 相性スコア計算

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

## 事前定義済み能力

| 能力ID | 名前 | 提供 | 依存 |
|--------|------|------|------|
| `midi_input` | MIDI Input | UserInput, HardwareTrigger | なし |
| `claude_agent` | Claude Agent | AiAgent, NaturalLanguage, CodeGeneration, Reasoning | UserInput |
| `webview_ui` | WebView UI | WebViewUi, VisualFeedback, Notification | WebSocket, HttpApi |
| `websocket_comm` | WebSocket | WebSocket, Ipc | なし |
| `session_mgmt` | Session Management | SessionManagement, PersistentStorage, DevelopmentHistory | AiAgent |

## テスト

```bash
# ユニットテスト実行
cargo test -p vantage-point capability::synergy

# サンプルプログラム実行
cargo run --example capability_synergy
```

## 将来の拡張

### Phase 2: イベント駆動連携

```rust
// 能力間でイベントを発火・購読
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

## 参考資料

- [JoJo's Bizarre Adventure - Stand](https://jojo.fandom.com/wiki/Stand)
- [Stand Capability 仕様書](../../../../docs/spec/05-stand-capability.md)
- [Synergy System 設計書](../../../../docs/design/03-synergy-system.md)
