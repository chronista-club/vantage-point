# ADR-0001: Web/Browser Terminal Dual Track Strategy

| | |
|---|---|
| **Status** | Accepted |
| **Date** | 2026-05-09 |
| **Origin** | `mem_1CarfVbaXpzuYv85QTRg9K` (creo-memories) |
| **Parent** | `mem_1CaQPwXQHHfvxPuT6KL9C8` (xterm.js 化検討、 2026-04-25) |
| **Scope** | VP に Web/Browser フロント (iPad / Vision Pro / 一般ブラウザ) を追加する際のターミナル描画レイヤ構成 |

---

## Context

### 背景

VP は元々 Mac (apple/VantagePoint = Core Text + vp-bridge) と Windows (vp-app = wry/tao + xterm.js) で **2 系統並立** の構成だった (`mem_1CaQPwXQHHfvxPuT6KL9C8`)。 そこに Web/Browser フロント (iPad / Vision Pro / 一般ブラウザ) を追加する要件が発生し、Rust コアを Wasm 化する選択肢が新たに視野に入ってきた。 二者択一ではなく多軸での再評価が必要となった。

### 現状 (2026-05-09)

memory 起草時点と現状で以下の差分があり、本 ADR ではこの差分を前提に決定する:

| 項目 | memory 想定 | 現状 |
|------|-------------|------|
| Mac native (vp-bridge) | 残存・並立 | **削除済** (Phase 3-C 2026-04-27、 `mem_1CaSjEz55wBuy3JdU14LCS`)。 vp-app に統一 |
| `broadcast::Sender<Vec<u8>>` 挿入 | 新規導入が必要 | **既に存在** — `crates/vantage-point/src/daemon/pty_slot.rs:100` (Phase 2.x-c scrollback 込み) |
| `alacritty_terminal` 依存 | 新規導入を提案 | **既に Cargo.toml:131 で v0.26 を使用中**。 `crates/vantage-point/src/terminal/{state,mod,renderer}.rs` + `tui/app.rs` (ratatui TUI mode) で実体運用 |
| WebSocket bridge | 既存実装あり | **既に `/ws/terminal?lane=...` 稼働中** (`crates/vantage-point/src/process/routes/ws_terminal.rs`) |

### 候補比較

| 案 | 実装コスト | Safari/iPad | VP コア整合 | メンテ | 1 ヶ月後出荷 | 3 年後戦略 |
|----|-----------|------------|-------------|--------|--------------|------------|
| (1) xterm.js のみ | ◎ | ◎ | ✗ | ◎ | ◎ | △ |
| (2) Rio Wasm まるごと | △ | △ (WebGPU 待ち) | ○ | △ (バス係数 1) | ✗ | ○ |
| (3) `alacritty_terminal` Wasm + 自前レンダラ | ✗ (数ヶ月) | ○ (Canvas2D) | ◎ | ✗ | ✗ | ◎ |
| (4) Sugarloaf だけ部品借用 | △ | △ | ◎ | △ | △ | ◎ |
| **(5) xterm.js + Rust コア併走** | ○ | ◎ | ◎ (運用担保) | ◎ | ◎ | ◎ |

評価軸:
- **VP コア整合**: 「Rust コアが真実」 思想との一致度
- **3 年後戦略**: 自社 Wasm レンダラへの移行可能性
- **メンテ**: バス係数・上流コミュニティの活発さ

---

## Decision

**Dual Track 戦略を採用。** 短期出荷ラインと R&D ラインを分離し、リスクを隔離しつつ未来の選択肢を温存する。

### 短期出荷ライン: (5) xterm.js + Rust コア併走

```
                                  ┌─→ alacritty_terminal::Parser → Term<T>  (ネイティブ・既存 TUI mode)
pty.read() → broadcast::Sender ──┤
                                  └─→ broadcast::Receiver → QUIC/WS → xterm.js.write()  (Web・既存 + 新規 Web 経路)
```

- Rust 側 `Term<T>` と xterm.js が**同じバイト列を独立解釈**する設計
- `tokio::sync::broadcast` で tee、 ズレが出たらバイト列を信じて再 play する権威を Rust 側に置く
- IME / accessibility / Safari・iPad 対応は xterm.js に丸投げ — 10 年分の地雷を踏み直さない
- `mem_1CY9W1ix7Yyf3ob5c2kVM7` (Phase 4 双方向ブリッジ) の自然な拡張
- `mem_1CaQPwXQHHfvxPuT6KL9C8` の「xterm.js 化」 検討で挙がった IME 懸念は、 native パスを残すことで両取り

### R&D ライン: (3)+(4) `alacritty_terminal` + Sugarloaf Wasm PoC

VP の experimental ターゲットとして、 プロダクト本流から分離して進める。

第一マイルストーン:
```bash
cargo build --target wasm32-unknown-unknown -p alacritty_terminal --no-default-features
```
落ちる候補は `mio` / `polling`、 libc Unix syscall、 `tokio` full feature。

第二マイルストーン: Wasm から `Term::new()` + `Parser::advance()` で `b"hello\x1b[31m world"` を流し、 Grid から差分セルを取り出して `console.log`。

第三マイルストーン: Sugarloaf を WebGPU レンダラとして組み込む (Safari は WebGL2 fallback)。

### 棄却した代替案

- **(1) xterm.js のみ**: 短期 OK だが「Rust コアが真実」 思想と長期齟齬。 将来 Wasm 化への path が断たれる
- **(2) Rio Wasm まるごと**: メンテバス係数 1、 ドロップイン npm 未整備、 iPad WebGPU 実用性が未確定。 プロダクト依存先としてリスク大
- **(3) 単独**: 数ヶ月実装を本流出荷とリンクさせるのは無謀
- **(4) 単独**: 短期出荷の足にできない

---

## Consequences

### Positive

- **既存基盤を最大限活用**: `broadcast::Sender` も `alacritty_terminal` も既存資産。 短期ラインの新規実装は subscriber 増設 + Web client への配線のみで済む
- **二重解釈の整合性チェック**: Rust 側 `Term<T>` を「真実」 として持つことで、 xterm.js 側のバグを検出できる仕組みが副産物として得られる (= ghost char `mem_1CaVpvsBKR3ckieRXo1nwr` 的問題の検証 oracle)
- **R&D ラインの非破壊的進行**: experimental crate 分離により、 PoC が失敗しても本流に影響しない
- **multi-frontend 対応**: WS protocol を共通化することで Mac / Windows / iPad / Vision Pro / 一般ブラウザを同じ pipeline で吸収

### Negative

- **二重解釈の整合性担保が継続コスト**: Rust 側 Term<T> と xterm.js のセル状態がズレた時の再 play 設計・検出機構が要る
- **broadcast subscriber の memory cost**: subscriber を増やすと channel buffer (= 256 chunk) の lag 圧力が増す。 Web client 接続数の上限設計が必要
- **WS endpoint のセキュリティ**: ブラウザから直接接続される endpoint には origin / 認証ポリシーが必要 (= 別 ADR で扱う)
- **alacritty_terminal v0.26 → Wasm**: 上流が Wasm を first-class でサポートしていないので、 R&D ライン第一マイルストーンのビルドが失敗する可能性

### 設計上の論点 (R&D ラインで決める)

- **差分セル ABI**: 候補は (a) バイト列再生 / (b) CellGrid 行差分 / (c) SGR 属性付きトークン列。 短期ラインがバイト列なので、 初手は **同じバイト列 API** で xterm.js 互換、 後で部分的に Wasm 置換しやすい設計に
- **Sugarloaf の入力 IME**: ブラウザ `compositionstart/update/end` を自前ハンドリングし、 `alacritty_terminal` にどう渡すか (Sugarloaf 自体はブラウザ IME 概念を持たない)
- **WebGPU フォールバック**: iPad Safari 18+ で WebGPU 対応進行中、 当面は WebGL2 で出して Safari 改善を待つ
- **scrollback 同期**: Rust 側 `Term<T>` のスクロールバックと Wasm 側のセル状態をどこでマージするか

---

## Implementation Notes — broadcast::Sender の挿入位置

### 結論

「**新規挿入は不要**」。 既存 `pty_slot.rs:100` の `broadcast::channel(256)` をそのまま tee point として使う。 短期ラインの実装は「**この broadcast を購読する subscriber を増設する**」 が本質。

### 既存 broadcast の構造

```rust
// crates/vantage-point/src/daemon/pty_slot.rs

pub struct PtySlot {
    output_tx: broadcast::Sender<Vec<u8>>,  // L46 — 既存 fan-out 点
    scrollback: Arc<Mutex<Vec<u8>>>,        // L50 — 256 KB ring buffer
    ...
}

impl PtySlot {
    pub fn spawn(...) -> Result<(Self, broadcast::Receiver<Vec<u8>>)> {
        let (output_tx, initial_rx) = broadcast::channel(256);  // L100
        ...
    }

    // 既存 subscriber API
    pub fn subscribe_output(&self) -> broadcast::Receiver<Vec<u8>>;             // L143
    pub fn subscribe_with_scrollback(&self) -> (broadcast::Receiver<Vec<u8>>, Vec<u8>);  // L158
}

// reader_task: PTY → ring + broadcast.send (atomicity 保証)
fn start_reader_task(reader, tx, scrollback) -> JoinHandle  // L207
```

すでに `subscribe_output()` / `subscribe_with_scrollback()` が公開されており、 fan-out の口は開いている。

### 短期ラインで増設する subscriber

```
crates/vantage-point/src/daemon/pty_slot.rs (既存)
    PtySlot::output_tx — broadcast::Sender<Vec<u8>>
        │
        ├─→ subscribe_output() ─────→ [既存] /ws/terminal?lane=... (vp-app xterm.js 用)
        │                              ws_terminal.rs:159
        │
        ├─→ subscribe_output() ─────→ [新規 A] alacritty_terminal::Term<T> attach task
        │                              「Rust コアが真実」 を保持する解釈レーン
        │                              新設場所: crates/vantage-point/src/terminal/term_attach.rs (仮)
        │                              起動責務: PtySlot::spawn() 内で reader_task と並走起動
        │
        └─→ subscribe_output() ─────→ [新規 B] /ws/web-terminal?lane=... (Web/iPad/Vision Pro 用)
                                       既存 ws_terminal.rs ハンドラを path 違いで再利用
                                       認証 / origin policy は別 ADR で
```

### 実装提案 (3 stage)

#### Stage 1: Rust 側 Term<T> attach (= "真実" レーンの確立)

**新設**: `crates/vantage-point/src/terminal/term_attach.rs`

責務:
- 既存 `PtySlot::subscribe_output()` で broadcast::Receiver を取得
- `alacritty_terminal::vte::Parser` + `Term<T>` で bytes を解釈
- (将来) 差分セルを別 channel に流して整合性チェック / 再 play oracle として使う

起動責務:
- `PtySlot::spawn()` 内、 既存の `start_reader_task` 直後に並走起動
- もしくは `LanePool::spawn_lane` 経路で attach する形 (= 起動責務を分離して PtySlot を pure に保つ案)

```rust
// 概念コード (実装時に詳細化):
pub fn start_term_attach_task(
    mut rx: broadcast::Receiver<Vec<u8>>,
    cols: u16,
    rows: u16,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut parser = alacritty_terminal::vte::Parser::new();
        let mut term = Term::new(...);  // listener / config は別途設計
        loop {
            match rx.recv().await {
                Ok(bytes) => {
                    for byte in bytes {
                        parser.advance(&mut term, byte);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Term<T> 側は再 play 不可なので、 lagged 時は full snapshot 要求
                    tracing::warn!("term_attach lagged: {} chunks dropped — needs full resync", n);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}
```

呼び出し位置 (案 A: PtySlot::spawn 内):
```rust
// pty_slot.rs L107 付近
let reader_handle = start_reader_task(reader, output_tx.clone(), scrollback.clone());
let term_attach_handle = start_term_attach_task(output_tx.subscribe(), cols, rows);  // ← 新規
```

呼び出し位置 (案 B: LanePool 経路で外付け):
```rust
// lanes_state.rs の spawn_lane 経路で
let pty_slot = PtySlot::spawn(...)?;
let term_rx = pty_slot.subscribe_output();
let term_handle = start_term_attach_task(term_rx, cols, rows);  // ← 新規
```

**推奨は案 B**。 理由:
- PtySlot を「PTY 1 つ + broadcast fan-out」 という pure な責務に保つ
- Term<T> attach は LanePool / 別 service の責務にして、 起動・停止のライフサイクルを分離管理
- 将来 Term<T> attach を ON/OFF できる feature toggle にしやすい

#### Stage 2: Web 用 WS endpoint 増設

`/ws/terminal` をそのまま使うか、 `/ws/web-terminal` で path 分けるか。

- **案 X: 同 endpoint 共用** — vp-app と Web を区別せず単一経路。 認証 / origin policy も全部一律
- **案 Y: path 分離** — Web 専用 endpoint を切って認証 / rate-limit / scrollback policy を別管理

**推奨は案 Y**。 理由:
- vp-app は localhost 限定で信頼境界が違う
- Web は origin / token / TLS / CORS の独自ポリシーが必要
- ADR で別管理することを明示しやすい

実装は `process/routes/ws_terminal.rs` を template にして `web_terminal.rs` を新設、 `subscribe_output` の使い方は同じで認証 middleware を挟む。

#### Stage 3: 整合性チェック oracle (optional, 後追い)

Rust 側 Term<T> の Grid と xterm.js 側 Buffer の cell 状態を周期スナップショットで比較する diagnostic API。 `mem_1CaVpvsBKR3ckieRXo1nwr` の ghost char のような問題の検出 oracle として。

実装優先度は低い (= R&D ラインに繰り入れ可)。

---

## Next Actions

### 短期ライン (本流出荷)

1. ✅ `broadcast::Sender` 挿入 — 既存 `pty_slot.rs:100` で完了済 (本 ADR で再確認)
2. ⬜ Rust 側 Term<T> attach task 新設 (`crates/vantage-point/src/terminal/term_attach.rs`)
3. ⬜ LanePool から term_attach 起動配線
4. ⬜ Web 用 WS endpoint 増設 (`/ws/web-terminal`、 別 ADR で認証ポリシー)
5. ⬜ xterm.js 側で `term.write(bytes)` 受けで動作確認 (vp-app 側はそのまま流用)
6. ⬜ vp-app + Web の 2 系統並列動作確認 (Mac native は削除済なので 3 系統 → 2 系統)

### R&D ライン (experimental)

1. ⬜ `alacritty_terminal --target wasm32-unknown-unknown` ビルド検証
2. ⬜ 最小 Wasm から `Term::new()` + `Parser::advance()` 動作確認
3. ⬜ 差分セル取り出し API の暫定定義 (バイト列互換から開始)
4. ⬜ Sugarloaf 単独の Wasm 動作確認 (WebGPU → WebGL2 fallback)

---

## 関連 memories

- `mem_1CarfVbaXpzuYv85QTRg9K`: 本 ADR の origin (Dual Track 決定)
- `mem_1CaQPwXQHHfvxPuT6KL9C8`: VP Mac Terminal xterm.js 化検討 (本決定の前哨)
- `mem_1CaSjEz55wBuy3JdU14LCS`: Mac native 削除 (Phase 3-C)
- `mem_1CY9KnDWXq3dHLZHdWvAYj`: alacritty_terminal API 調査 (0.25.1 ベース、 現状 v0.26)
- `mem_1CY9W1ix7Yyf3ob5c2kVM7`: VP Native Console Phase 4 双方向ブリッジ (短期ラインの基盤実装)
- `mem_1CXyD8CZwUPtAiQh1WDoqw`: ghostty-web 統合 (Wasm ターミナルの代替案)
- `mem_1CY9TxDBAYEtppCx4TL6U1`: VP Native Console Phase 3 (alacritty_terminal ラッパー)
- `mem_1CYySXRhqeSobnG862ifBT`: PTY + IME + CJK fallback (IME 設計参照)
- `mem_1CaVpvsBKR3ckieRXo1nwr`: xterm.js ghost characters (整合性チェック oracle の動機)
