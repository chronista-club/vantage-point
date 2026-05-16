# VP-SPEC-002: Capability / MIDI 仕様

> **Status**: Active
> **Created**: 2025-12-16
> **Updated**: 2026-05-16

---

## Overview

Process が保持する「能力（Capability / Stand）」システムと、MIDI コントローラー連携の仕様。

---

## Capability システム

### 段階的アーキテクチャ

```
Phase 1: トレイト型（現在）— 内部能力を Capability トレイトで整理
Phase 2: プロトコル型（完了, VP-169）— 能力間通信を WhitesnakeStore（SurrealDB embedded）ベースに
Phase 3: プラグイン型（将来）— WASM で能力を動的ロード
```

> **Phase 2 補足**: 当初は msgbox を in-memory `TopicRouter` ベースで設計していたが、 VP-169 epic（doc 19、 Phase 5 完了 / commit `445190c`）で msgbox substrate を **`WhitesnakeStore`（SurrealDB embedded primary）** に統一した。 mpsc ベースの `Router` / `Handle` は物理削除済。 `TopicRouter` 自体は Canvas / pane content の broadcast 配信用途で引き続き存在する（`process/topic_router.rs`）が、 msgbox（能力間メッセージング）は WhitesnakeStore primary。

### REQ-CAP-001: Capability トレイト

**実装**: `crates/vantage-point/src/capability/core.rs`

- [x] 能力の識別子（name, version）を提供できる
- [x] 能力の初期化・終了処理を定義できる
- [x] イベントの購読・発火ができる（EventBus）
- [x] 非同期処理に対応している

### REQ-CAP-002: CapabilityRegistry

**実装**: `crates/vantage-point/src/capability/registry.rs`

- [x] 能力を名前で登録・検索できる
- [x] 能力の有効/無効を切り替えられる

### REQ-CAP-003: EventBus

**実装**: `crates/vantage-point/src/capability/eventbus.rs`

- [x] 型安全なイベント定義・購読・発火
- [x] broadcast による複数購読者配信
- [x] 非同期対応

### REQ-CAP-004: msgbox v2（WhitesnakeStore）

**実装**: `crates/vantage-point/src/capability/msgbox_v2.rs`（`MsgboxStore` trait + `WhitesnakeStore` impl）

VP-169 epic（doc 19、 Phase 5 完了 / commit `445190c`）で確立した能力間メッセージング substrate。 旧 mpsc `Router` / `Handle` を物理削除し、 SurrealDB embedded を primary store に統一した。

- [x] `MsgboxStore` trait — DB primary な msgbox operations を定義
- [x] `WhitesnakeStore` — SurrealDB embedded（`msgs` table）で trait を impl
- [x] msg lifecycle を status field（`active` / `dead_letter` / `archived`）で表現、 1 table 永続
- [x] concurrent recv first-class — atomic claim 機構 + stale claim timeout
- [x] selective receive を WHERE predicate で表現（旧 Erlang stash 廃止）
- [x] `MsgboxStats` — status 別 row 数（`vp msgbox status` 用）
- [x] cross-process forward は両 SP DB write + ack-back HTTP（trace field で audit trail）

> **DB ディレクトリ**: SP は `db/sp_{slug}/`、 World daemon は `db/world/` に物理分離（VP-182 / PR #367、 surrealkv の single-writer 排他ロック対策）。 詳細は doc 19 §6。

---

## 6 パラメータシステム

JoJo Stand Stats を参考に、Capability の特性を定量化。

**実装**: `crates/vantage-point/src/capability/params.rs`

| パラメータ | 意味 | AI エージェント適用 |
|-----------|------|-------------------|
| Power | 影響力・変更範囲 | 扱えるデータ量 |
| Speed | 応答速度 | 初期化・応答時間 |
| Range | 適用範囲 | 統合サービス数 |
| Stamina | 継続動作 | 連続稼働時間 |
| Precision | 制御精度 | 成功率 |
| Potential | 拡張性 | カスタマイズ性 |

ランク: A (81-100) > B (61-80) > C (41-60) > D (21-40) > E (1-20)

---

## MIDI 連携（🍇 Hermit Purple）

### REQ-CAP-010: MidiCapability

- [x] MIDI デバイスの検出・接続
- [x] MIDI 入力イベント受信
- [x] MIDI 出力（Note, CC, SysEx）送信
- [x] LED フィードバック制御
- [x] デバイス固有設定管理

**パラメータ**: D/A/C/A/B/B (23/30)

### REQ-CAP-011: LPD8 デバイス定義

- [x] パッド 8 個 + ノブ 8 個の入力処理
- [x] パッド LED 制御
- [x] SysEx プログラム読み書き

#### パッドマッピング

```
┌─────────┬─────────┬─────────┬───────┐
│  PAD 5  │  PAD 6  │  PAD 7  │ PAD 8 │
│  (40)   │  (41)   │  (42)   │ (43)  │
│ Cancel  │  Reset  │   -     │   -   │
├─────────┼─────────┼─────────┼───────┤
│  PAD 1  │  PAD 2  │  PAD 3  │ PAD 4 │
│  (36)   │  (37)   │  (38)   │ (39)  │
│ Proj 1  │ Proj 2  │ Proj 3  │ Proj 4│
└─────────┴─────────┴─────────┴───────┘
```

| PAD 1-4 | プロジェクト切替 (port 33000-33003) |
| PAD 5 | AI 応答キャンセル |
| PAD 6 | セッションリセット |

#### LED フィードバック

| 状態 | LED |
|------|-----|
| プロジェクト起動中 | 点灯 |
| AI 応答中 | 点滅 |
| AI 入力待ち | 点灯 |
| エラー | 高速点滅 |

#### SysEx プロトコル

Manufacturer: 0x47 (Akai), Model: 0x7F 0x75 (LPD8)

| コマンド | バイト |
|----------|--------|
| Send Program | 0x61 |
| Set Active | 0x62 |
| Get Program | 0x63 |
| Get Active | 0x64 |

#### CLI コマンド

```bash
vp lpd8 write              # VP 設定を Program 1 に書込み
vp lpd8 switch 1           # アクティブプログラム切替
vp midi monitor            # MIDI 入力監視
vp midi ports              # ポート一覧
```

### REQ-CAP-020: Canvas / TUI 連携

- [ ] MIDI イベントが TopicRouter 経由で配信される（realtime broadcast 用途のため TopicRouter のまま）
- [ ] TUI / Canvas で MIDI 状態を表示できる

> **Note**: 能力間の **msgbox メッセージング** は VP-169（doc 19）で `WhitesnakeStore`（SurrealDB embedded primary）に統一済。 一方、 MIDI / Canvas 状態の realtime broadcast 配信は引き続き `TopicRouter` を用いる（永続化不要・低レイテンシ要求のため）。 「msgbox = WhitesnakeStore primary」 と「realtime broadcast = TopicRouter」 を混同しないこと。

### REQ-CAP-021: Claude Agent 連携

- [ ] MIDI 入力で PTY にテキスト送信
- [ ] MIDI 入力でチャットキャンセル
- [ ] LED で Agent 状態表示

---

## References

- `design/02-capability-evolution.md` (VP-DESIGN-002) — 進化システム設計
- `design/19-msgbox-whitesnake-primary.md` (VP-169) — msgbox v2 / WhitesnakeStore 設計（Phase 2 プロトコル型の実体）
- `crates/vantage-point/src/capability/` — 実装
