# テスト戦略

Vantage Point プロジェクトのテスト方針とベストプラクティス。

## テストピラミッド

```
        ▲ E2E テスト（少）
       ╱ ╲  CLI コマンド統合
      ╱───╲
     ╱ 統合 ╲ 統合テスト（中）
    ╱ テスト ╲ Capability間連携
   ╱───────────╲
  ╱  単体テスト  ╲ 単体テスト（多）← 主力
 ╱───────────────╲ Protocol, データ構造
```

## CI パイプライン

3ジョブが**並列実行**:

```
┌─────────────────────────────────────────┐
│              push/PR                     │
└────────────────┬────────────────────────┘
                 │
    ┌────────────┼────────────┐
    ▼            ▼            ▼
┌───────┐  ┌─────────┐  ┌─────────┐
│ lint  │  │  test   │  │  build  │
│ fmt   │  │ 114+    │  │ release │
│clippy │  │ tests   │  │         │
└───┬───┘  └────┬────┘  └────┬────┘
    │           │            │
    └───────────┼────────────┘
                │ (タグ時のみ)
                ▼
          ┌───────────┐
          │  release  │
          └───────────┘
```

## 単体テスト

### 配置場所

```rust
// 同一ファイル内に #[cfg(test)] モジュール
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        // ...
    }
}
```

### テスト対象の優先順位

1. **Protocol** - シリアライズ/デシリアライズ
2. **データ構造** - 状態遷移、バリデーション
3. **Capability** - 初期化、状態管理
4. **ビジネスロジック** - 計算、変換

### テストの書き方

```rust
#[test]
fn test_process_message_serialization() {
    // Arrange
    let msg = ProcessMessage::ChatChunk {
        content: "Hello".to_string(),
        done: false,
    };

    // Act
    let json = serde_json::to_string(&msg).unwrap();

    // Assert
    assert!(json.contains(r#""type":"chat_chunk""#));
    assert!(json.contains(r#""content":"Hello""#));
}
```

## 統合テスト

`tests/` ディレクトリに配置:

```
tests/
├── capability_integration.rs  # Capability間連携
└── protocol_roundtrip.rs      # Protocol往復テスト
```

### 例: Capability統合テスト

```rust
// tests/capability_integration.rs
use vantage_point::capability::*;

#[tokio::test]
async fn test_capability_lifecycle() {
    let registry = CapabilityRegistry::new();
    // ...
}
```

## テスト実行

```bash
# 全テスト実行
cargo test --workspace

# 特定モジュールのテスト
cargo test protocol::

# 特定テストのみ
cargo test test_process_message

# 出力表示
cargo test -- --nocapture

# 並列度制限（CI向け）
cargo test -- --test-threads=1
```

## モック/スタブ

### トレイトを使ったモック

```rust
// プロダクションコード
trait AgentRunner {
    async fn run(&self, prompt: &str) -> Result<String>;
}

// テストコード
struct MockAgent {
    response: String,
}

impl AgentRunner for MockAgent {
    async fn run(&self, _prompt: &str) -> Result<String> {
        Ok(self.response.clone())
    }
}
```

## 非同期テスト

```rust
#[tokio::test]
async fn test_async_capability() {
    let cap = SomeCapability::new();
    cap.initialize().await.unwrap();
    assert!(cap.is_ready());
}
```

## テストカバレッジ

### 現在の状況

```
モジュール                    テスト数
─────────────────────────────────────
protocol/                    ~30
capability/                  ~50
process/                       ~15
world/                       ~10
その他                        ~10
─────────────────────────────────────
合計                          114+
```

### カバレッジ計測（オプション）

```bash
# cargo-tarpaulin をインストール
cargo install cargo-tarpaulin

# カバレッジ計測
cargo tarpaulin --workspace --out Html
```

## ベストプラクティス

1. **高速を維持** - 単体テストは0.1秒以内
2. **独立性** - テスト間で状態を共有しない
3. **明確な命名** - `test_<対象>_<条件>_<期待結果>`
4. **AAA パターン** - Arrange, Act, Assert
5. **エッジケース** - 境界値、空、null、エラー

## 追加予定

- [ ] E2Eテスト（CLI実行）
- [ ] WebSocket統合テスト
- [ ] MIDI入力テスト（モック使用）
