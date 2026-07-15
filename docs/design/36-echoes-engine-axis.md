# doc 36 — Echoes engine 軸: surface × engine の直交格子と lane サブコンソール

> 前提: [`cursor-engine.md`](./cursor-engine.md)（#773 Act I / #776 Act II、cursor-agent を 2 つ目の
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

### Phase 0 — dogfood slice（最小で「動かす」）

「主 claude lane の Act II に、cursor 副コンソールを 1 枚開けて会話できる」までを最小スライスとする:

1. LanePool entry に `sub_host: Option<ChatHost>` を追加、副 spawn/close の dispatch（MCP or IPC）。
2. 副 cursor_session / 副 topic（sub-key 空間）。
3. webview: 主 ChatView の隣に副 ChatView を mount（副 topic 購読）、開閉トグル。
4. 送信は副 topic 経由で副 host の submit へ。

HITL / permission / replay は cursor 側が元々非対応（cursor-engine.md）なので Phase 0 対象外。
主・副の会話は独立（chatId 別）で開始する。

## 6. 未解決・リスク

- **per-lane stand 非永続**（cursor-engine.md 制約）: 副 host の engine 種別も SP 再起動で失われる。
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
