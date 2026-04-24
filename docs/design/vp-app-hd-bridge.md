# vp-app ↔ HD bridge 設計

**Status**: reviewed (2026-04-24) — Option D 確定、Q1-Q5 合意済
**Author**: Claude (reconnaissance 基に起草) + mito (review)
**Date**: 2026-04-24
**Target**: Phase W2.5 step 2 以降 (VP-93 = 2a+2b+2c、2d は別 issue)

---

## 1. 背景と目的

Phase W2.5 step 1 で vp-app (Windows native) は TheWorld daemon に到達できる基盤を持った
(`TheWorldClient` が IPv4 loopback、daemon が `[::]` dual-stack bind)。

しかし terminal pane の中身は **ローカル PTY を直接起動** している:

```
vp-app (Windows/WSL 境界)
  └── portable-pty → wsl.exe → bash -lc "claude --continue || claude"
```

この結果、vp-app は "Claude を terminal で走らせる薄いアプリ" 止まりで、以下が実現できていない:

- **session 永続化** (vp-app 終了で claude が死ぬ)
- **project 切替** (起動 shell が固定、切り替えで re-exec)
- **複数 HD の同時観測** (Mac 側の MainWindow のような "複数 project pane")
- **canvas + HD 同居** (Paisley Park を隣に置きたい)

本 doc は、**vp-app ↔ daemon ↔ HD の経路を設計する** ためのもの。
既存の daemon / SP / HD 実装を棚卸しし、選択肢と段階案を並べて議論の土台にする。

## 2. 実現したい体験 (tentative)

ユーザが最初の release で得たい体験を言語化:

1. vp-app を立ち上げたら sidebar に既知 project が並ぶ (既に動いている)
2. project をクリック → right pane に **HD (Claude Code) terminal** が開く
3. その裏で **同 project の Canvas** (Paisley Park) が WebView で開ける
4. vp-app を閉じて再度開いても **同じ session が継続**
5. **複数 project を切替え可能** (tab / dropdown / ⌘1-9)

上記を満たす上で本 doc は (2)(4) を主に扱う。(1)(3)(5) は別 phase で処理。

## 3. 既存コンポーネント棚卸し

### 3.1 SP (Process Server) の `/ws`

**役割**: browser (xterm.js) と PTY を結ぶ既存 WebSocket。
**実装**: `crates/vantage-point/src/process/routes/ws.rs:26` `ws_handler()`。

- 単一セッション前提 (PtyManager は `Arc<Mutex<_>>`、最初の session のみサポート)
- message: `BrowserMessage::{TerminalInput, TerminalResize, Chat, ...}` / `ProcessMessage::TerminalOutput{data: base64}`
- xterm.js 互換
- 認証なし (localhost only で運用されてきた前提)

**評価**: xterm.js 向けに出来上がってる。vp-app が WebView でなく WS client として参加する場合、プロトコルは既にある。ただし単一 session の制約がつきまとう。

### 3.2 Daemon (TheWorld) の Unison QUIC terminal channel

**役割**: `vp hd attach` など Rust-native console クライアントが daemon と session を RPC でやり取りする経路。
**実装**: `crates/vantage-point/src/daemon/server.rs:574-` `start_daemon_server()`。

- 3 チャネル: `session` / `terminal` / `system`
- 形式: request/response RPC (`id`, `method`, `payload`) + event push
- terminal 関連: `terminal.create_pane`, `.write`, `.resize`, `.read_output`, `.kill_pane`
- data は **base64**
- **出力は polling** (`read_output` が 50ms timeout で block 待ち) ← terminal 用途ではやや辛い
- 認証は shell cmd allowlist のみ (コマンドインジェクション防止)
- SessionRegistry + PtySlot で session × pane 管理

**評価**: Rust-native に丁寧、設計はクリーン。ただし QUIC (UDP) + polling read が vp-app で扱いにくい (特に WSL2 UDP boundary は歴史的に不安定)。

### 3.3 HD (Heaven's Door)

**役割**: Claude CLI を tmux session 内で生かす本体。
**実装**: `crates/vantage-point/src/agent.rs`、`crates/vantage-point/src/commands/hd_cmd.rs:87` `hd_start()`。

- **tmux session がホスト**: HD = 独立 tmux session (`{project}-vp` 命名)
- Claude CLI は tmux pane 内で subprocess として生きる
- daemon の registry keepalive に `agent_card` を post (project_name / port / pid / terminal_token / tmux_session)
- **daemon の PtySlot では "ない"** — HD は daemon から独立した tmux host に乗っている

**評価**: tmux を使うことで "session 永続化" が無料で手に入る (vp-app 終了しても tmux + Claude が残る)。これは強力。一方、vp-app が HD に届くには "tmux pane の I/O を外部クライアントに expose する" 中継が必要。

### 3.4 `vp hd attach` の実態

**役割**: HD の tmux session に接続する CLI。
**実装**: `crates/vantage-point/src/commands/tui_cmd.rs`。

- 現状は **ローカル tmux attach を portable-pty でラップして ratatui が描画** しているだけ
- **daemon とは通信していない** (!)
- つまり「daemon 経由で remote HD attach する経路」はまだ実装されていない

**評価**: これが大きな設計ギャップ。daemon の terminal channel は存在するが、HD (tmux) に橋渡ししている実装が無い。vp-app の HD bridge を作るなら、**この橋渡しも同時に設計する** 必要がある。

### 3.5 認証 (terminal_token)

- `discovery.rs::generate_terminal_token()` で UUID v4 生成
- registry keepalive で daemon に送り、`/api/health` で取得可能
- **現在は authz チェックなし** (token は session 識別子として機能するのみ)

**評価**: 将来的には request signing したい。`[::]` dual-stack に bind する今、localhost only の assumption が破れつつあるので authz 強化が先送りできなくなる。

### 3.6 vp-app 側

- `crates/vp-app/src/terminal.rs`: portable-pty で wsl.exe 起動 + reader_loop
- `crates/vp-app/src/app.rs`: `EventLoopProxy<AppEvent>` で PTY 出力を xterm.js に配信
- `XtermReady` buffering: ConPTY の DSR 応答問題対策
- IPC: `window.ipc.postMessage` → Rust 側で PTY write

**評価**: ローカル PTY に密結合。次 phase で差し替える。buffering パターンは remote 経路でも有用。

## 4. 経路の選択肢

以下すべて **vp-app が Windows から WSL 側 daemon (IPv4 127.0.0.1:32000) に届く** 前提。

### Option A: SP `/ws` を直接叩く

```
vp-app (WS client) ──tcp──► SP /ws (port 33000+) ──► PtyManager → HD tmux attach
```

- vp-app に tungstenite / tokio-tungstenite を追加
- SP が PtyManager で "tmux attach -t {hd_session}" を起動し、その PTY を `/ws` に流す
- xterm.js プロトコルそのまま使える

**Pros**: プロトコル既成、WebSocket は TCP で WSL2 に優しい、vp-app 依存軽い
**Cons**: PtyManager は 1 session 制約、SP 側の改修が必要、`ws` 経由で base64 往復はやや非効率

### Option B: Daemon QUIC terminal channel を叩く

```
vp-app (Unison client) ──udp──► TheWorld QUIC :32000 ──► PtySlot → HD tmux attach
```

- vp-app に unison / quinn 依存追加
- daemon に "HD 接続用 PtySlot" を作って tmux attach を中に入れる

**Pros**: Rust native RPC、設計クリーン、session/terminal channel 既成
**Cons**: WSL2 UDP forwarding は歴史的に不安定、vp-app の dep 増、polling read は terminal 応答で微妙

### Option C: 今のローカル PTY を磨く (据え置き)

- vp-app は wsl.exe 起動の portable-pty を維持
- daemon との繋ぎは REST (project 一覧等) のみ

**Pros**: 設計負債無し、今すぐ動く
**Cons**: session 永続化されない、複数 project 難しい、HD が vp-app のプロセスに握られて非対称

### Option D: TheWorld に `/ws/terminal` を新設 (new)

```
vp-app (WS client) ──tcp──► TheWorld :32000/ws/terminal ──► PtySlot → HD tmux attach
```

- daemon の HTTP (axum) 側に WS endpoint を足す
- SP の `/ws` と似た形にして PtyManager/PtySlot のどちらかを再利用

**Pros**:
- WS (TCP) で WSL2 に優しい
- Daemon が既に dual-stack bind 完了
- 認証を最初から設計できる (terminal_token を WS handshake で)
- SP の単一 session 制約を背負わずに済む

**Cons**: daemon 側に新 endpoint 追加 = 実装コスト

## 5. 採用 Option: D (確定)

**Option D (TheWorld に `/ws/terminal` 新設)** を採用。

1. **WSL2 境界で TCP/WS**: UDP/QUIC の不確実性を回避。既存の `[::]` HTTP bind がそのまま使える。
2. **daemon 中心の architecture**: HD の session 寿命は daemon 側が握っているべき (vp-app は view に徹する)。SP は project-specific 、HD/terminal という横断 concern は TheWorld (daemon) が持つのが CLAUDE.md の意匠と整合。
3. **認証を最初から**: `[::]` bind で LAN 越えに届く今、terminal_token を WS handshake で検証する新 endpoint のほうが安全に作れる。
4. **SP `/ws` の単一 session 制約を引きずらない**: PtySlot ベースで複数セッション対応を一発で入れる。
5. **Q1-Q4 の合意と整合**: tmux 継続 / Windows 先行 / daemon owner / 2a+2b+2c フルスコープ、全て D で自然に成立。

実装コストは A/B より大きいが、**VP-93 の対象範囲 (Step 2a+2b+2c)** を一本の設計で走り切れるので割に合う。

## 6. MVP phasing (VP-93 スコープ = 2a+2b+2c)

### Step 2a: Daemon に `/ws/terminal` 骨格 (VP-93)
- 新 endpoint: `GET /ws/terminal?token={terminal_token}&pane={pane_id}`
- 既存 PtySlot を流用、ただし `tmux attach-session -t {session}` を subprocess に
- vp-app 側は portable-pty を消して tungstenite で接続
- "今動いている wsl.exe -- bash" 相当の単発 terminal を remote 化して、まず現状同等を達成

### Step 2b: HD 専用チャネル化 (VP-93)
- Step 2a の汎用 terminal を HD 向けに特化
- **tmux session を daemon が起動・明示終了** (Q3 の daemon owner モデル)
  - daemon 起動時: tmux session spawn + Claude CLI 起動
  - daemon 終了時: tmux kill-session で Claude も明示終了
  - vp-app の起動・終了は daemon の session lifecycle に影響しない (WebSocket connect/disconnect のみ)
- registry の agent_card と WS session を紐づけ

### Step 2c: project 切替 (VP-93)
- vp-app の sidebar クリック → 現 session detach → 別 HD 再 attach
- 複数 project の HD を daemon が同時管理 (project 単位の `{project}-vp` tmux session)
- xterm.js の buffer 保持 / 復元
- **vp-app 閉じて開き直しても、daemon 側の HD は継続 → xterm に前回表示が残る体験**

### Step 2d: authz / LAN 対策 (別 issue — VP-95 等で後続)
- terminal_token の signed handshake
- `[::]` bind を loopback only に戻すか、localhost auth を強化
- LAN 越えを正式サポートする/しないの decision も含める

## 7. Resolved decisions (2026-04-24 review)

| # | Question | 結論 |
|---|---|---|
| Q1 | HD は tmux 継続前提で良いか | ✅ **tmux 継続**。`tmux attach -t ...` を daemon の PtySlot で起動する Mac 実装との整合、session 永続化の無料感、既存コード流用性で勝る |
| Q2 | Windows 単発 release を Mac と揃えるか | ✅ **Hybrid — Windows 先行、Mac 統一は後続 Epic**。今 Mac を触ると Windows 体験を磨く余力が消える |
| Q3 | session 永続性の担保範囲 | ✅ **daemon owner model**。tmux は PTY host として残すが lifecycle は daemon が明示的に握る。daemon stop = tmux kill、再起動で Claude 再 init は許容 (debug 楽優先) |
| Q4 | MVP スコープ | ✅ **Step 2a+2b+2c 全部**。project 切替まで含めて VP-93 完結。2d (authz) は別 issue |
| Q5 | Option 選択 | ✅ **D (TheWorld に `/ws/terminal` 新設)** |

**派生する重要な特性** (Q3 + Q4 から):
- vp-app (GUI frontend) を閉じて開き直しても **daemon 側の Claude は生きてる** → 前回 session に re-attach
- `vp world stop` すると Claude も明示終了 (意図した挙動)
- OS 再起動は両方消える

## 8. 次アクション

- 本 doc PR (#184) merge
- VP-93 を Step 2a / 2b / 2c のサブ issue に分解 (または inline TODO として track)
- Step 2a 実装から着手 — `mako/vp-93-*` branch
- WS endpoint の詳細 protocol が固まってきたら `docs/spec/` に別出し検討
