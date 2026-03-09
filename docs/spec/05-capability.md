# Capability 仕様書

## 概要

Process に拡張可能な「能力（Capability）」システムを導入する。
将来的に100〜1000規模の能力、コミュニティ開発を想定した設計。

## 背景

### JoJo スタンドの世界観

> 「スタンドは能力を持つ」

Process はユーザーの傍らに立ち、様々な能力（Stand）を発揮する。
各能力は独立しているが、Process 全体として協調動作する。

### 技術的背景

- MCPがツール拡張を解決したように、能力の拡張基盤が必要
- 段階的に拡張性を高めていく設計

## 段階的アーキテクチャ

```
Phase 1: トレイト型（現在）
    内部能力をCapabilityトレイトで整理
    ↓
Phase 2: プロトコル型
    能力間通信をメッセージベースに
    ↓
Phase 3: プラグイン型
    外部プロセス/WASM で能力を動的ロード
```

## 要件

### REQ-CAP-001: Capabilityトレイト

**概要**: 全ての能力が実装する共通インターフェース

**実装ファイル**: `crates/vantage-point/src/capability/core.rs`

**受け入れ条件**:
- [x] 能力の識別子（name, version）を提供できる
- [x] 能力の初期化・終了処理を定義できる
- [x] イベントの購読・発火ができる
- [x] 非同期処理に対応している

---

### REQ-CAP-002: CapabilityRegistry

**概要**: 能力の登録・検索・管理を行うレジストリ

**実装ファイル**: `crates/vantage-point/src/capability/registry.rs`

**受け入れ条件**:
- [x] 能力を名前で登録できる
- [x] 登録済み能力を一覧取得できる
- [x] 能力を名前で検索できる
- [x] 能力の有効/無効を切り替えられる

---

### REQ-CAP-003: EventBus

**概要**: 能力間のイベント通信基盤

**実装ファイル**: `crates/vantage-point/src/capability/eventbus.rs`

**受け入れ条件**:
- [x] イベントを型安全に定義できる
- [x] 能力がイベントを購読（subscribe）できる
- [x] 能力がイベントを発火（emit）できる
- [x] 複数の購読者に配信できる（broadcast）
- [x] 非同期イベント処理に対応している

---

### REQ-CAP-010: MidiCapability

**概要**: MIDI入出力を提供する能力

**受け入れ条件**:
- [ ] MIDIデバイスの検出・接続ができる
- [ ] MIDI入力イベントを受信できる
- [ ] MIDI出力（Note, CC, SysEx）を送信できる
- [ ] LEDフィードバックを制御できる
- [ ] デバイス固有設定（LPD8等）を管理できる

---

### REQ-CAP-011: LPD8デバイス定義

**概要**: Akai LPD8の固有機能サポート

**受け入れ条件**:
- [ ] パッド8個の入力を処理できる
- [ ] ノブ8個の入力を処理できる
- [ ] パッドLEDの制御ができる
- [ ] SysExでプログラム設定を読み書きできる

---

### REQ-CAP-020: Canvas 連携

**概要**: MIDI イベントを Canvas / TUI に配信

**受け入れ条件**:
- [ ] MIDI イベントが TopicRouter 経由で配信される
- [ ] TUI / Canvas で MIDI 状態を表示できる

---

### REQ-CAP-021: Claude Agent 連携

**概要**: MIDI で Claude CLI（📖 Heaven's Door）のアクションを発火

**受け入れ条件**:
- [ ] MIDI 入力で PTY にテキスト送信ができる
- [ ] MIDI 入力でチャットキャンセルができる
- [ ] MIDI 入力でセッションリセットができる
- [ ] LED で Agent 状態（応答中/入力待ち/エラー）を表示できる

---

## LEDフィードバック仕様

| 状態 | LED表現 |
|------|---------|
| プロジェクト起動中 | 対応パッド点灯 |
| プロジェクト停止 | 対応パッド消灯 |
| Agent思考中 | 点滅 |
| Agent待機 | 点灯 |
| Agentエラー | 高速点滅 |
| モード: 協調 | パッド1点灯 |
| モード: 委任 | パッド2点灯 |
| モード: 自律 | パッド3点灯 |
| 押下確認 | 一時的に全点灯 |

## 対応デバイス（ロードマップ）

1. **Akai LPD8** - Phase 1で完成
2. Korg Livestage
3. Arturia MidiLab mkII
4. YAMAHA FGPD-50
5. Studiologic Numa Compact X SE

## 関連設計

- [02-capability-evolution.md](../design/02-capability-evolution.md)
