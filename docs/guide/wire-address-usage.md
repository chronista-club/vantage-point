# Guide: Wire address v3.1 — Usage

> **改訂 (2026-05-21)**: 旧 msgbox 実装は 2026-05 の wiremsg 再設計 (R1〜R6、 PR #406〜#420) で全廃された。
> 本 doc の **address syntax (`<actor>@<location>`) は wiremsg がそのまま継承**しており現行有効。
> ただし CLI は `vp msg` → **`vp wire`** に、 MCP tool は `msg_send` / `msg_recv` → **`wire_send` / `wire_recv` / `wire_thread`** に置き換わった。 本 doc の CLI / MCP example は現行の wiremsg 系コマンドに更新済。
> Ruby DSL / mDNS / hub の example はいずれも将来計画 (Phase 3+) であり、 syntax は wiremsg 上で有効。

> **Status**: address syntax は現行有効 (wiremsg が継承)。 旧称 "Msgbox address v3.1" (VP-144 Epic、 Phase 0 SDG)。
> **Spec**: [docs/spec/wire-address-v3.md](../spec/wire-address-v3.md) (Why + What)
> **Design**: [docs/design/14-wire-address-v3.md](../design/14-wire-address-v3.md) (How)

本 doc は v3.1 address の **使い方** を集約する。 構文の正確な定義は spec、 実装側決定は design を参照。 ここでは **dogfood で打って動く form** を中心に置く。

LAN MVP (Phase 0-3) 完成までは、 一部 example は **Phase X 実装後に valid** と注記する。

---

## 1. address の読み方 (= 1 分 cheat-sheet)

```
            host (DNS-like、 . で qualify)
            ↓
agent  @  mako.chronista.club  /  vantage-point  /  performer  /  objrec
  ↑                                ↑                  ↑
actor                            project           lane (multi-segment 可)
(default: agent)
```

**最 minimal**: `vantage-point/conductor` (= `agent@vantage-point/conductor`)
**最 verbose**: `agent@mako.chronista.club/vantage-point/performer/objrec`

`@` 1 個 + `/` 階層 + `.` host DNS、 三役直交。 sidebar の lane label をそのまま address として使える。

---

## 2. CLI examples (`vp wire`)

> `vp wire` は wiremsg 再設計 (R5-2) で旧 `vp mailbox` を置換した CLI。 `watch` (long-poll subscribe) / `send` を提供する。

### 2.1 基本: send / watch

```bash
# 同 machine、 vantage-point の conductor lane に送信 (default actor = agent)
vp wire send --to vantage-point/conductor --body "hello"

# 同 machine、 vantage-point の conductor lane の agent inbox を watch
# (受信 message を 1 行 JSON で stdout に出力、 Claude Code Monitor の subscription source 想定)
vp wire watch --agent agent@vantage-point/conductor

# actor 明示 (= notification address)
vp wire send --to notify@vantage-point/conductor --body "build done"

# project broadcast (lane 全 actor)
vp wire send --to '*@vantage-point/conductor' --body "全員へ通知"
```

### 2.2 cross-process (= 同 machine 別 project)

```bash
# self world 内 cross-process (= 別 project process、 wire R3 の best-effort forward)
vp wire send --to creo-memories/conductor --body "hello from vantage-point"

# v1 syntax (互換、 default lane = conductor)
vp wire send --to agent@creo-memories --body "v1 形式 (互換動作)"
```

### 2.3 LAN cross-machine (Phase 3 で valid)

```bash
# 同 LAN の他 machine を mDNS 列挙
vp world list --lan
# →
# [
#   { alias: "macbook-b", endpoint: "https://macbook-b.local:32000" },
#   { alias: "studio", endpoint: "https://studio.local:32000" }
# ]

# address book に追加
vp world add macbook-b

# LAN wire send
vp wire send --to agent@macbook-b/vantage-point/conductor --body "hello from macbook-a"

# explicit FQDN
vp wire send --to agent@macbook-b.local/vantage-point/conductor --body "explicit mDNS"
```

### 2.4 Internet via hub (Phase 4 で valid)

```bash
# hub 経由 user identity 解決
vp world add mako@chronista.club
# → hub に query、 alias 'mako' の pubkey + endpoint を address book に保存

# Internet wire send
vp wire send --to agent@mako/vantage-point/conductor --body "hello via hub"

# explicit hub URL
vp wire send --to agent@mako.chronista.club/vantage-point/conductor --body "FQDN explicit"
```

---

## 3. Ruby DSL examples (Phase 5 で valid)

### 3.1 address inline (primary form)

```ruby
# self world、 lane 指定
Vp.send_to("vantage-point/conductor", { hello: "world" })

# actor 明示
Vp.send_to("notify@vantage-point/conductor", { type: "build_done" })

# LAN
Vp.send_to("agent@macbook-b/vantage-point/conductor", { msg: "from A" })

# Internet via hub
Vp.send_to("agent@mako/vantage-point/conductor", { msg: "via hub" })
```

### 3.2 connection scope (= shorthand、 batch 用途)

```ruby
# world / project context を fix して address 短縮
Vp.with_world("mako.chronista.club") do |w|
  w.send_to("agent/vantage-point/conductor", payload1)
  w.send_to("agent/vantage-point/performer/objrec", payload2)
  # 同 hub への 2 件、 connection 1 個で済ます
end
```

### 3.3 subscribe (long-running listener)

```ruby
Vp.subscribe("agent@vantage-point/conductor") do |msg|
  puts "received from #{msg.from}: #{msg.payload}"
  # at-most-once (default) / at-least-once (manual_ack) は msg metadata で判定
end
```

### 3.4 broadcast

```ruby
Vp.broadcast("*@vantage-point/conductor", { announce: "release v0.18.0" })
```

### 3.5 discovery

```ruby
# LAN 内 (Phase 3)
worlds = Vp.discover_lan
worlds.each { |w| puts "#{w.alias}: #{w.endpoint}" }

# hub directory (Phase 4)
public_users = Vp.discover_hub("chronista.club")
```

---

## 4. address book 管理 (Phase 3 以降)

### file location

```
~/.config/vp/addresses.toml
```

### example

```toml
# auto-discovered (mDNS) は last_seen で freshness 判定
[[world]]
alias = "macbook-b"
pubkey = "ed25519:6f3e..."
endpoint = "https://macbook-b.local:32000"
discovered_via = "mDNS"
last_seen = "2026-05-08T07:00:00Z"

# manual register (hub 経由)
[[world]]
alias = "taro"
pubkey = "ed25519:abc..."
endpoint = "wss://hub.chronista.club/relay/xyz"
discovered_via = "hub"
last_seen = "2026-05-08T06:30:00Z"

# trust list (= broadcast / public msg を受け入れる world)
trust_list = ["macbook-b", "studio", "taro"]
```

### CLI 操作

```bash
# 列挙
vp world list

# 追加
vp world add <alias>          # mDNS or hub から自動解決
vp world add <alias>@<hub>    # hub 明示

# 削除
vp world remove <alias>

# trust list 編集
vp world trust add <alias>
vp world trust remove <alias>
```

---

## 5. v1 → v3.1 migration cheat-sheet

| v1 で使っていた form | v3.1 でも valid? | 推奨 v3.1 form |
|---------------------|------------------|----------------|
| `agent@vantage-point` | ✅ そのまま valid (default lane = conductor) | `vantage-point/conductor` (lane 明示) または同左 |
| `*@vantage-point` | ✅ valid | `*@vantage-point/conductor` (lane 明示) |
| `notify@vantage-point` | ✅ valid (default lane) | `notify@vantage-point/conductor` |
| (なかった) | — | `vantage-point/conductor` (= actor 省略、 v3.1 新) |
| (なかった) | — | `vantage-point/performer/objrec` (= per-lane、 v3.1 新) |
| (なかった) | — | `mako/vantage-point/conductor` (= cross-world、 v3.1 新) |

**v1 user は何も変更不要**、 v3.1 features は opt-in。

---

## 6. Trouble-shooting (= dogfood gap 解消の使い方)

> **改訂 note (2026-05-21)**: gap 1/2 はもともと旧 msgbox の `mcp` actor 周りの問題。 wiremsg では `mcp` reserved 名が `agent` に統合され、 inter-agent comm は一律 `agent` actor。 cross-process 配送は `wire_remote` の best-effort forward (R3) で、 forward 不能時は明示的 error を返す方針 (silent drop なし) は継承されている。 recv は `wire_recv` の per-agent cursor。

### gap 1 fix: silent drop の解消

**before (旧 msgbox)**:
```bash
$ vp wire send --to mcp@creo-memories --body "test"
# ← ack 返るが、 実際は forward 失敗で deliver されず (silent drop)
```

**after (wiremsg)**: cross-process forward 不能時は明示的 error。 inter-agent comm は `mcp` ではなく `agent@creo-memories` を使う (wiremsg では `mcp` reserved 名は `agent` に統合済)。

→ **明示的 error**、 silent drop なし。 sender が即座に address ミスに気付ける。

### gap 2 fix: 他 actor inbox の観察

**before (旧 msgbox)**: MCP recv は self process の `mcp` actor inbox 限定、 他 actor は recv 不可。

**after (wiremsg)**: `wire_recv` は呼び出し agent の wire address (= 自 lane 由来) の cursor を進めて未読を取得。 CLI で任意 actor の inbox を watch する場合は:

```bash
# agent inbox を watch (inter-agent comm の default)
vp wire watch --agent agent@vantage-point/conductor

# notify actor inbox を観察
vp wire watch --agent notify@vantage-point/conductor
```

### gap 3 fix: 2 namespace 統合

**before (旧 msgbox)**: `vantage-point/conductor` (sidebar lane label) を wire address と誤認 → `actor name contains invalid character` parse error。

**after (v3.1)**: `vantage-point/conductor` を valid address として解釈 (= `agent@vantage-point/conductor` shorthand)。 sidebar label と address が **同 syntax**。

```bash
$ vp wire send --to vantage-point/conductor --body "hello"
# → agent@vantage-point/conductor として解釈される
```

### gap 4 fix: cross-process recv の visualization

**before (旧 msgbox)**: 別 lane から `agent@<self>` に送信 → receiver は MCP recv で見えない、 CLI watch で観察必要。

**after (v3.1)**: vp-app sidebar の Echoes icon 右隣に **未読 message icon** が出現、 click で tooltip 表示。

```
┌─────────────────────────┐
│ 💬 Conductor 📨           ●  │  ← 📨 icon = 未読 message あり
│   sidebar-session-title │     ● = OSC 99 awaiting input (VP-142)
└─────────────────────────┘
```

→ **sidebar UI が primary observation path**、 daily dogfood で見落とさない。

---

## 7. LAN MVP dogfood シナリオ (Phase 3 完成時)

### Setup

- 2 台 macbook (= macbook-a / macbook-b) が同 LAN (Wi-Fi or Ethernet)
- 両方で `vp daemon start` + `vp app start` 完了
- 両方で `vp world init` で keypair 生成済

### Step 1: discovery

**macbook-a**:
```bash
$ vp world list --lan
[
  { alias: "macbook-b", pubkey: "ed25519:abc...", endpoint: "https://macbook-b.local:32000" }
]

$ vp world add macbook-b
✓ macbook-b added to address book
```

### Step 2: send

**macbook-a**:
```bash
$ vp wire send --to agent@macbook-b/vantage-point/conductor --body "hello from A"
Message sent (id: 01h...)
```

### Step 3: receive

**macbook-b**:
```bash
$ vp wire watch --agent agent@vantage-point/conductor
[2026-05-08 07:00:01] from agent@macbook-a/vantage-point/conductor:
  payload: "hello from A"
  signed: ed25519:6f3e... (verified ✓)
```

### Step 4: sidebar 反映

macbook-b の vp-app sidebar:
```
vantage-point
├── 💬 Conductor 📨    ← 📨 (= 未読 1)
└── (...)
```

→ **macbook-a が macbook-b の sidebar に msg を flow**、 LAN MVP 完成。

---

## 8. example walkthrough — Echoes 同士の inter-agent comm

### scenario

performer lane で実装中の Claude が「conductor lane の Claude に lint result を投げる」 シナリオ。

### macbook-a の vantage-point/performer/code-1 lane で

```bash
# performer Claude が実行
$ cargo clippy --workspace 2>&1 | tee /tmp/clippy.txt
$ vp wire send --to agent@vantage-point/conductor --body "$(cat /tmp/clippy.txt)"
```

> MCP 経由なら performer Claude は `wire_send` tool を直接呼ぶ (CLI 不要)。

### 同 machine の vantage-point/conductor lane で

- vp-app sidebar の Conductor row に 📨 icon 表示
- click → tooltip で「from agent@vantage-point/performer/code-1、 2 min ago、 lint result preview」
- conductor Claude が `wire_recv` (MCP tool) で取得、 内容に応じて指示

### Ruby DSL 版 (Phase 5)

```ruby
# performer
Vp.send_to("agent@vantage-point/conductor", {
  type: "lint_result",
  output: File.read("/tmp/clippy.txt"),
  ts: Time.now,
})

# conductor 側
Vp.subscribe("agent@vantage-point/conductor") do |msg|
  next unless msg.payload[:type] == "lint_result"
  # ... handle lint result ...
end
```

---

## 9. FAQ

### Q. v1 syntax は廃止される?

A. **廃止しない**。 v1 `<actor>@<project>` は v3.1 で default lane = `conductor` に解釈、 forward-compat。 既存 dogfood / Ruby DSL / CLI を書き換える必要なし。

### Q. actor 名を省略すると何になる?

A. **`agent`** (= reserved default)。 `vantage-point/conductor` = `agent@vantage-point/conductor`。 sidebar lane label をそのまま address として打てる。

### Q. lane 名と actor 名が衝突した場合は?

A. 衝突しない設計。 actor は `@` の左、 lane は `/` の中。 構文上 disambiguous (`agent@vantage-point/conductor` の `conductor` は lane segment、 `agent` は actor)。 reserved actor 名 (`agent` / `notify` / `mcp` / `protocol` / `world` / `*`) は lane segment / project name でも reject (= validate error)。

### Q. hub.chronista.club が落ちたら何が起きる?

A. **LAN msg は影響なし** (= mDNS direct path、 hub 経由しない)。 self-process / same machine / LAN は hub 不要。 Internet 経由 (`<actor>@<user>/...`) のみ影響、 sender outbox に retry queue で TTL 内に再送 attempt。

### Q. 自分で hub を立てられる?

A. はい (Phase 4+)。 hub spec は ~500 LOC の Rust or TypeScript で実装可能、 self-host で `your-hub.example.com` を運営できる。 `vp world add taro@your-hub.example.com` で接続可能。 vendor lock-in なし。

### Q. msg payload は hub に見える?

A. **見えない** (Phase 4+)。 payload は receiver pubkey で NaCl `crypto_box_seal` 暗号化、 hub は envelope (= routing info、 from/to/ts/sig) のみ見る。 payload は receiver 以外復号不能。

---

## 関連

- **Spec**: [docs/spec/wire-address-v3.md](../spec/wire-address-v3.md)
- **Design**: [docs/design/14-wire-address-v3.md](../design/14-wire-address-v3.md)
- **Linear Epic**: [VP-144](https://linear.app/chronista/issue/VP-144)
- **Phase sub-issues**: [VP-145](https://linear.app/chronista/issue/VP-145) [VP-146](https://linear.app/chronista/issue/VP-146) [VP-147](https://linear.app/chronista/issue/VP-147) [VP-148](https://linear.app/chronista/issue/VP-148)
