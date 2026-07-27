> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# doc 36 — Echoes engine 軸: surface × engine の直交格子と lane サブコンソール

> 前提: [`cursor-engine.md`](../archive/cursor-engine.md)（#773 Act I / #776 Act II、cursor-agent を 2 つ目の
> engine として追加）、[doc 33 console-unification](./33-console-unification.md)（engine 排他 slot +
> console_mode）、[doc 32 echoes-act2-gui](./32-echoes-act2-gui.md)。

## 目的

cursor engine 追加（#773/#776）で Echoes が事実上 **2 次元の格子**になった。この構造を明文化し、
「engine 軸を lane に対してどう束ねるか」の意思決定を残す。結論として **lane サブコンソール
（engine 軸を lane 内で multi-valued にする）方向で攻める**。

## 1. 直交する 2 軸

Echoes は次の直交格子として実装されている:

```
                Act I (TUI console)        Act II (chat GUI)
              ┌───────────────────────┬──────────────────────┐
  claude      │ PtySlot: claude TUI   │ EchoesAgentHost       │
              │                       │ (headless 常駐)       │
              ├───────────────────────┼──────────────────────┤
  cursor      │ PtySlot: cursor-agent │ CursorAgentHost       │
              │                       │ (turn-scoped)         │
              ├───────────────────────┼──────────────────────┤
  (gemini?    │ PtySlot: <cli> TUI    │ <X>AgentHost          │
   codex? …)  │                       │ + <X>Translate        │
              └───────────────────────┴──────────────────────┘
                surface 軸（lane 内で自由に切替）   engine 軸
```

- **横軸 = surface（Act I / II）**: console で操るか chat で視るか。lane 内で `console_mode` により自由に切替。
- **縦軸 = engine / ベンダー（claude / cursor / 将来 gemini・codex…）**: どの頭脳か。

新ベンダーの追加コスト = **1 行**（Act II 翻訳層 `*_translate` + host `*AgentHost`、Act I spawn command）。
Act I/II の列構造は不変で各ベンダーが両セルを埋める。cursor が「翻訳層 + host を足すだけで乗った」
（`echoes/mod.rs`）のはこの直交性の配当。

## 2. 直交が効くのは surface 層まで（非対称）

| 抽象 | 直交する？ | 根拠 |
|------|-----------|------|
| 会話面（Act II） | ✅ engine 非依存 | 全 engine を `EchoesEvent` 共通面に翻訳 → chatview / topic 無改修 |
| console（Act I） | ✅ engine 非依存 | PtySlot は「CLI を床に置く」だけ |
| **wire（inter-agent 連携）** | ❌ **claude 専用** | hook（`vp wire hook-check`）/ MCP 依存。cursor stand は wire hook を注入しない（`stand_spawner.rs`、相当機構が無いため） |

**ユーザーが触る surface 軸は完全に直交、conductor 間連携の軸は claude 止まり**。この非対称が
「別 Lane に wire で任せる」が cursor で成立しない根本理由。cursor lane との連携は
`vp lane nudge` / `vp lane capture` の **text 経路**のみ（PtySlot 直ホストの帰結）。

## 3. engine 軸を lane に対してどう束ねるか（3 案）

意思決定は 1 次元に還元される: **縦軸（engine）を 1 lane に対して pinned / movable / multi-valued の
どれにするか**。

| 案 | 意味 | コスト | 連携 |
|----|------|--------|------|
| **B. 別 Lane（pinned）** | 1 lane = engine 軸の 1 点。複数 lane で複数 engine をカバー | ゼロ（今動く。`add_performer(stand="cursor")`） | wire 不可 → nudge/capture |
| **swap（movable）** | 1 lane が engine 軸を移動（単一値のまま切替） | 小（host 選択が既に `stand` ベース） | 同上 |
| **A. サブコンソール（multi-valued）** | 1 lane が engine 軸で 2 セル同時（主 claude + 副 cursor） | 大（排他 slot を破る） | lane 内共存 |

## 4. 決定: A（lane サブコンソール）方向で攻める

> ⚠️ **この §4 の決定は [doc 37](./37-echoes-two-axes.md) で「保留」に格下げされた（2026-07-15）**。現 canonical は **engine を lane-pinned に据える**（複数 engine = 複数 lane、doc 33 の法を維持）方針。サブコンソールは 3 エンジン実機 dogfood 後に「1 lane = N session」再カットとして再検討する。以下の §4–§5 は**保留された設計資産**として残す（再カット時の素材）。

**主エンジン（claude）で作業する文脈のまま、副コンソールとして cursor を同 lane 内に呼び出せる**
状態を目指す。理由:

- 「別作業者に投げて画面を見る」（B）より、**同じ cwd・同じ作業文脈の中で別 engine の視点を即座に
  重ねる**方が dogfood 体験として濃い。
- engine 軸が Act II で既に抽象化済み（`ChatHost` enum、pump 非依存）なので、副 host を足す土台はある。
- 排他 slot（doc 33 C1）は「1 surface = 1 engine」を守る不変条件だが、**サブコンソールは別 surface
  （副ペイン）として増設**するので、主 surface の排他は保ったまま拡張できる（= 排他を「破る」のではなく
  「surface を 1 枚増やす」と読み替える）。

## 5. アーキテクチャ方針（実装の骨格）

現状: lane は 1 `ChatHost`（`ensure_chat_engine` が `stand` で選択）+ Act I PtySlot。

サブコンソール: lane に **副 ChatHost slot** を増設する。

- **アドレス**: 副エンジンを sub-key で識別（例: `<lane>#sub` / `<lane>/cursor`）。cursor_session /
  topic / console_mode を sub-key 空間に分離（主と衝突させない）。
- **host**: 副は `ChatHost::Cursor(CursorAgentHost::spawn(...))`。主 lane と同じ cwd を共有、chatId は
  副専用 state file（`cursor_sessions/<project>__<lane>#sub`）。
- **topic 配線**: 副の EchoesEvent を副 topic（`process/echoes/data/<lane>#sub/event`）に publish。
  pump は engine 非依存なので主 topic 経路をそのまま複製できる。
- **webview**: Act II chat pane を**左右 split**し、**主 chat = 左 / cursor 副 = 右**に配置
  （決定 2026-07-15）。副 topic を購読する 2 個目の ChatView instance。resync-loader / accordion 等の
  描画資産はそのまま再利用（engine 非依存）。Phase 0 は比率固定（例 60/40）、後で可変に。
- **ライフサイクル**: 副 host は on-demand spawn（主 lane 作成時には立てない）/ 明示 close で drop。
  lane 削除で主・副とも teardown。

### Phase 0 — dogfood slice（最小で「動かす」、code-explorer マップ 2026-07-15 反映）

「主 claude lane の Act II を左右 split し、右に cursor 副コンソールを 1 枚開けて会話できる」までを
最小スライスとする。核心は **副 = `chat_engines` と並列の `sub_chat_engines` map**（`LaneInfo` には
持たせない — 既存 `chat_engines` が LanePool 側の別 map である慣習に忠実）。

**Rust（vantage-point）**
1. `process/lanes_state.rs`: `LanePool` に `sub_chat_engines: HashMap<LaneAddress, ChatEngineSlot>` を
   追加。`ensure_sub_chat_engine`（**`ChatHost::Cursor` 固定**、`console_mode==Chat` ガードは持ち込まない
   — 副は別 surface）/ `submit_sub_chat` / `interrupt_sub_chat` / `drop_sub_chat_engine` を既存メソッドの
   副版として新設。`remove()` の teardown 列に `sub_chat_engines.remove(addr)` を 1 行追加（全削除経路が
   ここに収束）。
2. `lane/cursor_session.rs`: 無改修。副の chatId は `lane` 引数に `"<lane_label>#sub"` を渡すだけで
   別 state file（`cursor_sessions/<project>__<lane>#sub`）になる（`#` は sanitize 対象外で安全）。
3. `process/unison_server.rs`: `echoes_sub_open` / `echoes_sub_submit` / `echoes_sub_close` /
   `echoes_sub_interrupt` を `handle_echoes_*`（770-948）と同型で新設、dispatch table に登録。
4. topic 分離: 副 EchoesEvent を主と区別できる経路に流す（`process/echoes-sub/data/{lane}/event` 新
   capability か `ProcessMessage::EchoesEvent{…, sub:bool}`）。`echoes_pump` は無改修で流用可。

**webview（+ vp-app Rust）**
5. `chatview.tsx`: `ChatView` を `lane: Accessor<string|null>` prop 対応に一般化（既定は現 `activeLane`）。
   `foldInto`/`laneChat`/`foldEvent`・描画資産（ToolGroupRow/resync-loader 等）は lane 引数を取る純関数
   群なので**無改修で再利用**。`installSubChatView` を追加。
6. `main_area.rs` + `entry.tsx`: `#console-chat-host` を split の器にし `#console-chat-main`（左 60%）/
   `#console-chat-sub`（右 40%、既定 hidden）を増設。主 = `installChatView`、副 = `installSubChatView`。
7. `vp-app/src/app.rs` + `terminal.rs`: 副 session（topic 用 key と RPC `lane` 用 key を**別引数**で持つ）を
   新設。`echoes:sub_submit` 等の IPC を配線。

HITL / permission / replay は cursor 非対応（archive/cursor-engine.md）なので副では対象外。主・副の会話は独立
（chatId 別）で開始。**推奨順 = 1→3(RPC 単体まで) → 7(session 分離) → 5→6(見た目)**。

## 6. 未解決・リスク

- **⚠️ 最重要の落とし穴: `#sub` を wire 越しの `lane` に埋めるな**。`LanePool::parse_address`
  （`lanes_state.rs`）は `"vp/performer/foo#sub"` を `LaneAddress::performer("vp","foo#sub")` として
  **パース自体は成功**させるが、実在登録（name=`"foo"`）と `Eq`/`Hash` 不一致 → `chat_engines.get()` が
  必ず外れ "Lane not found" で落ちる。しかも console.ts / vp-app の `echoes_sessions` は文字列 key で
  何でも受けるため**「webview では動いて見えて SP だけ壊れる」**発見しにくい形になる。
  → **SP への RPC は常に unmangled な `lane`** を送り、副である識別は method 名（`echoes_sub_*`）or
  別 field（`sub:bool`）で表現。`#sub` の埋め込みは「webview ローカル Map key」と「Rust の
  `cursor_session`/`console_mode` state-file key（`lane_label`）」の 2 箇所だけに閉じる。
- **副に console_mode ガードを持ち込むな**: `set_console_mode` の Chat 許可（`stand ∈ {echoes,cursor}`）は
  主エンジンの Act I/II 切替専用。`ensure_chat_engine` を副に流用すると「主が Tui のとき副も開けない」
  意図しない制約を生む → 専用 `ensure_sub_chat_engine` を新設しガードを持ち込まない。
- **restart（New Session）は副を知らない**: `restart_lane` の fresh 分岐は主の cc_session/cursor_session/
  chat_engines のみ clear。副の state file・`sub_chat_engines` は残る（stale 副 chatId）。Phase 0 は見送り可。
- **per-lane stand 非永続**（archive/cursor-engine.md 制約）: 副 host の engine 種別も SP 再起動で失われる。
  副の永続をどこまでやるかは Phase 0 スコープ外（まず in-memory で dogfood）。
- **アドレス sub-key の正規化**: `normalize_path_key` / lane_label / wire address 等が sub-key を
  取りこぼさないか要検証（`#` を含む key の sanitize）。
- **UI レイアウト**: 副ペインを frame-engine の Scene に載せるか、Act II 内 split で閉じるか（doc 31/33
  の surface 群との整合）。
- **リソース**: cursor は turn-scoped で 1 turn 1 プロセス。主 claude 常駐 + 副 cursor 断続で
  spawn/CPU cap（PR3 の SP spawn cap）と干渉しないか。

## 7. 将来: engine 軸の一般化

Phase 0 は cursor 副コンソールだが、副 host slot は engine 非依存に作る。gemini / codex 等の
CLI エンジンも `*_translate` + `*AgentHost` を足せば同じ副 slot に載る（第 1 節の格子に新 row を
足すのと同型）。engine 軸の pinned/movable/multi の意思決定はこの doc を SSOT とする。
