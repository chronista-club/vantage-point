# Stand Capability Parameters 実装ガイド

## 概要

このドキュメントは、Stand Capability Parametersの6パラメータシステムの実装と使用方法を説明します。

## ファイル構成

```
crates/vantage-point/src/capability/
├── mod.rs           # モジュール定義
├── params.rs        # 6パラメータシステム（本実装）
├── evolution.rs     # 進化システム
└── types.rs         # 能力分類
```

## 6パラメータシステム

### パラメータ定義

JoJo's Bizarre Adventureのスタンドパラメータを参考に、AIエージェント能力向けに再定義:

| パラメータ | 英語名 | 意味 | AIエージェントへの適用 |
|-----------|--------|------|----------------------|
| 破壊力 | Power | 影響力・変更範囲 | 扱えるデータ量・ファイル数 |
| スピード | Speed | 応答速度・実行速度 | 初期化時間・応答時間 |
| 射程距離 | Range | 適用範囲・統合度 | 統合サービス数・プロトコル数 |
| 持続力 | Stamina | 継続動作時間 | 連続稼働時間・メモリリーク耐性 |
| 精密動作性 | Precision | 制御精度・エラー率 | 出力精度・成功率 |
| 成長性 | Potential | 拡張性・学習可能性 | カスタマイズ性・学習機能 |

### ランク体系

A〜Eの5段階 + 特殊ランク（?、-）:

```rust
pub enum Rank {
    None,    // -: 該当なし
    Unknown, // ?: 測定不能
    E,       // E: 最低 (1-20)
    D,       // D: 低 (21-40)
    C,       // C: 中 (41-60)
    B,       // B: 高 (61-80)
    A,       // A: 最高 (81-100)
}
```

**順序**: `None < Unknown < E < D < C < B < A`

### Rust構造体

```rust
pub struct CapabilityParams {
    pub power: Rank,
    pub speed: Rank,
    pub range: Rank,
    pub stamina: Rank,
    pub precision: Rank,
    pub potential: Rank,
}
```

## 具体例: MIDI Capability

### パラメータ設定

```rust
pub const MIDI_CAPABILITY_PARAMS: CapabilityParams = CapabilityParams {
    power: Rank::D,      // 破壊力: D (軽量、小規模制御)
    speed: Rank::A,      // スピード: A (即座、リアルタイム)
    range: Rank::C,      // 射程距離: C (USBローカル、将来的に拡張可能)
    stamina: Rank::A,    // 持続力: A (長時間稼働、メモリリーク無し)
    precision: Rank::B,  // 精密動作性: B (MIDIプロトコル確実、デバイス差異あり)
    potential: Rank::B,  // 成長性: B (デバイス定義追加可能)
};
```

### 総合スコア

- **Power (D)**: 2点
- **Speed (A)**: 5点
- **Range (C)**: 3点
- **Stamina (A)**: 5点
- **Precision (B)**: 4点
- **Potential (B)**: 4点

**合計**: 23/30点

### パラメータ分析

#### Power (破壊力): D

- MIDI入出力は軽量
- 制御対象: 8パッド + 8ノブ = 16個の物理コントロール
- データ量: MIDIメッセージは3バイト程度
- 大規模な変更は引き起こさない

#### Speed (スピード): A

- MIDI入力は即座に処理 (<1ms)
- リアルタイム性が高い
- 遅延が許されない用途（演奏、ライブ操作）

#### Range (射程距離): C

- USBローカル接続が基本
- 将来的にネットワークMIDI対応可能
- 複数デバイス同時接続は可能だが、現状は単一デバイス想定

#### Stamina (持続力): A

- MIDI接続は安定、長時間稼働可能
- メモリリークなし、リソース消費少
- デーモンとして常駐可能

#### Precision (精密動作性): B

- MIDIプロトコル自体は確実（7bit精度）
- デバイス固有の挙動差異あり
- SysEx解析は実装依存

#### Potential (成長性): B

- 新規デバイス定義を追加可能
- デバイスプロファイル（JSON/TOML）で拡張
- LEDフィードバック、SysExカスタマイズ可能

## 使用例

### 1. パラメータの作成

```rust
use crate::capability::{CapabilityParams, Rank};

// バランス型（全てC）
let balanced = CapabilityParams::balanced();

// カスタム
let custom = CapabilityParams {
    power: Rank::A,
    speed: Rank::B,
    range: Rank::C,
    stamina: Rank::D,
    precision: Rank::E,
    potential: Rank::B,
};

// 未測定
let none = CapabilityParams::none();
```

### 2. パラメータの比較

```rust
// ランク比較
assert!(Rank::A > Rank::B);
assert!(Rank::E > Rank::None);

// 総合スコア
let score = params.total_score(); // 0-30
```

### 3. パラメータの表示

```rust
// Display trait実装
println!("{}", MIDI_CAPABILITY_PARAMS);
// Output:
// Stand Capability Parameters:
//   Power:     D (破壊力)
//   Speed:     A (スピード)
//   Range:     C (射程距離)
//   Stamina:   A (持続力)
//   Precision: B (精密動作性)
//   Potential: B (成長性)
//   Total Score: 23/30
```

### 4. パラメータの配列化（視覚化用）

```rust
let array = params.as_array(); // [Rank; 6]

for (i, rank) in array.iter().enumerate() {
    let name = CapabilityParams::PARAM_NAMES[i];
    let name_jp = CapabilityParams::PARAM_NAMES_JP[i];
    println!("{} ({}): {}", name, name_jp, rank);
}
```

### 5. JSON Serialization

```rust
use serde_json;

// シリアライズ
let json = serde_json::to_string(&MIDI_CAPABILITY_PARAMS)?;
// {"power":"D","speed":"A","range":"C",...}

// デシリアライズ
let params: CapabilityParams = serde_json::from_str(&json)?;
```

## 測定基準の詳細

### Power (破壊力)

| ランク | データ量 | ファイル数 | 例 |
|--------|---------|-----------|---|
| E | 1-10 files | 少量 | 単一ファイル編集 |
| D | 10-100 files | 小規模 | 小規模プロジェクト |
| C | 100-1000 files | 中規模 | 中規模プロジェクト |
| B | 1000-10000 files | 大規模 | 大規模プロジェクト |
| A | 10000+ files | 超大規模 | 複数プロジェクト横断 |

### Speed (スピード)

| ランク | 応答時間 | 例 |
|--------|---------|---|
| E | >30秒/操作 | 重いバッチ処理 |
| D | 10-30秒/操作 | 中程度の処理 |
| C | 3-10秒/操作 | 一般的な処理 |
| B | 1-3秒/操作 | 高速処理 |
| A | <1秒/操作 | リアルタイム |

### Range (射程距離)

| ランク | サービス数 | スコープ | 例 |
|--------|-----------|---------|---|
| E | 1 | ローカルのみ | 単一ツール |
| D | 2-3 | 少数統合 | Git + Editor |
| C | 5-10 | 中規模統合 | Git + Editor + CI + Cloud |
| B | 10-50 | 大規模統合 | 多数サービス |
| A | 無制限 | プロトコル非依存 | 汎用API統合 |

### Stamina (持続力)

| ランク | 稼働時間 | 例 |
|--------|---------|---|
| E | 数分 | 一時的なスクリプト |
| D | 数時間 | 短期セッション |
| C | 1日 | デイリーバッチ |
| B | 1週間 | 長期サービス |
| A | 無期限 | 常駐デーモン |

### Precision (精密動作性)

| ランク | エラー率 | 例 |
|--------|---------|---|
| E | >20% | 不安定 |
| D | 10-20% | やや不安定 |
| C | 3-10% | 安定 |
| B | 1-3% | 高精度 |
| A | <1% | 超高精度（形式検証済み）|

### Potential (成長性)

| ランク | 拡張性 | 例 |
|--------|-------|---|
| E | 固定機能 | ハードコード |
| D | 設定ファイル | TOML/JSON設定 |
| C | API/Hook | 拡張ポイントあり |
| B | プラグイン | プラグイン機構 |
| A | 自己学習 | AI/機械学習統合 |

## テスト

```bash
# パラメータシステムのテストを実行
cargo test -p vantage-point params
```

### テストケース

- `test_rank_ordering`: ランクの順序確認
- `test_rank_score`: ランクの数値化
- `test_rank_from_score`: 数値からランクへの変換
- `test_balanced_params`: バランス型パラメータ
- `test_midi_capability_params`: MIDI能力パラメータの妥当性
- `test_params_display`: Display trait
- `test_params_as_array`: 配列化
- `test_serialization`: JSON serialization

## 将来の拡張

### Phase 1（現在）: 静的定義

- 能力ごとに定数でパラメータを定義
- 手動で値を設定

### Phase 2: 動的計測

- 実行時メトリクスから自動計算
- `UsageMetrics` と連携

```rust
impl CapabilityParams {
    pub fn from_metrics(metrics: &UsageMetrics) -> Self {
        // メトリクスから動的にパラメータを算出
    }
}
```

### Phase 3: 学習ベース

- 機械学習モデルで最適化
- ユーザーフィードバックに基づく調整

## 参考文献

- [JoJo Stand Stats - JoJo Wiki](https://jojowiki.com/Stand_Stats)
- [docs/spec/05-stand-capability.md](../../spec/05-stand-capability.md)
- [crates/vantage-point/src/capability/params.rs](../../crates/vantage-point/src/capability/params.rs)

## 関連ドキュメント

- [Stand Capability 仕様書](../../spec/05-stand-capability.md)
- [Stand Capability 設計書](../../design/02-stand-capability-design.md)
