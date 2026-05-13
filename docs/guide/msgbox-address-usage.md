# Guide: Msgbox address v3.1 — Usage

> **Status**: Draft (VP-144 Epic、 Phase 0 SDG)
> **Spec**: [docs/spec/msgbox-address-v3.md](../spec/msgbox-address-v3.md) (Why + What)
> **Design**: [docs/design/14-msgbox-address-v3.md](../design/14-msgbox-address-v3.md) (How)

本 doc は v3.1 address の **使い方** を集約する。 構文の正確な定義は spec、 実装側決定は design を参照。 ここでは **dogfood で打って動く form** を中心に置く。

LAN MVP (Phase 0-3) 完成までは、 一部 example は **Phase X 実装後に valid** と注記する。

---

## 1. address の読み方 (= 1 分 cheat-sheet)

```
            host (DNS-like、 . で qualify)
            ↓
agent  @  mako.chronista.club  /  vantage-point  /  worker  /  objrec
  ↑                                ↑                  ↑
actor                            project           lane (multi-segment 可)
(default: agent)
```

**最 minimal**: `vantage-point/lead` (= `agent@vantage-point/lead`)
**最 verbose**: `agent@mako.chronista.club/vantage-point/worker/objrec`

`@` 1 個 + `/` 階層 + `.` host DNS、 三役直交。 sidebar の lane label をそのまま address として使える。

---

## 2. CLI examples (`vp msg`)

### 2.1 基本: send / watch

```bash
# 同 machine、 vantage-point の lead lane に送信 (default actor = agent)
vp msg send vantage-point/lead "hello"

# 同 machine、 vantage-point の lead lane の agent msgbox を watch
vp msg watch vantage-point/lead

# actor 明示 (= notification address)
vp msg send notify@vantage-point/lead "build done"

# project broadcast (lane 全 actor)
vp msg send '*@vantage-point/lead' "全員へ通知"
```

### 2.2 cross-process (= 同 machine 別 project)

```bash
# self world 内 cross-process (= 別 project process)
vp msg send creo-memories/lead "hello from vantage-point"

# v1 syntax (互換、 default lane = lead)
vp msg send agent@creo-memories "v1 形式 (互換動作)"
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

# LAN msg send
vp msg send agent@macbook-b/vantage-point/lead "hello from macbook-a"

# explicit FQDN
vp msg send agent@macbook-b.local/vantage-point/lead "explicit mDNS"
```

### 2.4 Internet via hub (Phase 4 で valid)

```bash
# hub 経由 user identity 解決
vp world add mako@chronista.club
# → hub に query、 alias 'mako' の pubkey + endpoint を address book に保存

# Internet msg send
vp msg send agent@mako/vantage-point/lead "hello via hub"

# explicit hub URL
vp msg send agent@mako.chronista.club/vantage-point/lead "FQDN explicit"
```

---

## 3. Ruby DSL examples (Phase 5 で valid)

### 3.1 address inline (primary form)

```ruby
# self world、 lane 指定
Vp.send_to("vantage-point/lead", { hello: "world" })

# actor 明示
Vp.send_to("notify@vantage-point/lead", { type: "build_done" })

# LAN
Vp.send_to("agent@macbook-b/vantage-point/lead", { msg: "from A" })

# Internet via hub
Vp.send_to("agent@mako/vantage-point/lead", { msg: "via hub" })
```

### 3.2 connection scope (= shorthand、 batch 用途)

```ruby
# world / project context を fix して address 短縮
Vp.with_world("mako.chronista.club") do |w|
  w.send_to("agent/vantage-point/lead", payload1)
  w.send_to("agent/vantage-point/worker/objrec", payload2)
  # 同 hub への 2 件、 connection 1 個で済ます
end
```

### 3.3 subscribe (long-running listener)

```ruby
Vp.subscribe("agent@vantage-point/lead") do |msg|
  puts "received from #{msg.from}: #{msg.payload}"
  # at-most-once (default) / at-least-once (manual_ack) は msg metadata で判定
end
```

### 3.4 broadcast

```ruby
Vp.broadcast("*@vantage-point/lead", { announce: "release v0.18.0" })
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
| `agent@vantage-point` | ✅ そのまま valid (default lane = lead) | `vantage-point/lead` (lane 明示) または同左 |
| `*@vantage-point` | ✅ valid | `*@vantage-point/lead` (lane 明示) |
| `notify@vantage-point` | ✅ valid (default lane) | `notify@vantage-point/lead` |
| (なかった) | — | `vantage-point/lead` (= actor 省略、 v3.1 新) |
| (なかった) | — | `vantage-point/worker/objrec` (= per-lane、 v3.1 新) |
| (なかった) | — | `mako/vantage-point/lead` (= cross-world、 v3.1 新) |

**v1 user は何も変更不要**、 v3.1 features は opt-in。

---

## 6. Trouble-shooting (= dogfood gap 解消の使い方)

### gap 1 fix: silent drop の解消 (Phase 2 以降)

**before (v1)**:
```bash
$ vp msg send mcp@creo-memories "test"
Message sent to 'mcp@creo-memories' (id: ...)
# ← ack 返るが、 実際は forward 失敗で deliver されず (silent drop)
```

**after (v3.1)**:
```bash
$ vp msg send mcp@creo-memories "test"
Error: actor 'mcp' not registered for cross-process delivery on world 'creo-memories'
       (mcp is per-process ad-hoc; use 'agent@creo-memories' for cross-process inter-agent comm)
```

→ **明示的 error**、 silent drop なし。 sender が即座に address ミスに気付ける。

### gap 2 fix: MCP recv で他 actor 観察 (Phase 2 以降)

**before (v1)**: MCP `msg_recv` は self process の `mcp` actor msgbox 限定、 他 actor (`agent` / `notify` 等) は recv 不可。

**after (v3.1)**: `msg_recv` に `actor` param 追加。

```bash
# default は agent (= 旧 v1 の `mcp` から変更、 inter-agent comm が default)
vp msg recv

# notify actor msgbox を観察
vp msg recv --actor notify

# 任意 actor
vp msg recv --actor protocol
```

### gap 3 fix: 2 namespace 統合 (Phase 1 以降)

**before (v1)**: `vantage-point/lead` (sidebar lane label) を msgbox address と誤認 → `actor name contains invalid character` parse error。

**after (v3.1)**: `vantage-point/lead` を valid address として解釈 (= `agent@vantage-point/lead` shorthand)。 sidebar label と address が **同 syntax**。

```bash
$ vp msg send vantage-point/lead "hello"
Message sent to 'agent@vantage-point/lead' (id: ...)
```

### gap 4 fix: cross-process recv の visualization (Phase 2 以降)

**before (v1)**: 別 lane から `agent@<self>` に送信 → receiver は MCP recv で見えない、 CLI watch で観察必要。

**after (v3.1)**: vp-app sidebar の Echoes icon 右隣に **未読 message icon** が出現、 click で tooltip 表示。

```
┌─────────────────────────┐
│ 💬 Lead 📨           ●  │  ← 📨 icon = 未読 message あり
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
$ vp msg send agent@macbook-b/vantage-point/lead "hello from A"
Message sent (id: 01h...)
```

### Step 3: receive

**macbook-b**:
```bash
$ vp msg watch agent@vantage-point/lead
[2026-05-08 07:00:01] from agent@macbook-a/vantage-point/lead:
  payload: "hello from A"
  signed: ed25519:6f3e... (verified ✓)
```

### Step 4: sidebar 反映

macbook-b の vp-app sidebar:
```
vantage-point
├── 💬 Lead 📨    ← 📨 (= 未読 1)
└── (...)
```

→ **macbook-a が macbook-b の sidebar に msg を flow**、 LAN MVP 完成。

---

## 8. example walkthrough — Echoes 同士の inter-agent comm

### scenario

worker lane で実装中の Claude が「lead lane の Claude に lint result を投げる」 シナリオ。

### macbook-a の vantage-point/worker/code-1 lane で

```bash
# worker Claude が実行
$ cargo clippy --workspace 2>&1 | tee /tmp/clippy.txt
$ vp msg send agent@vantage-point/lead "$(cat /tmp/clippy.txt)" --kind notification
```

### 同 machine の vantage-point/lead lane で

- vp-app sidebar の Lead row に 📨 icon 表示 (Phase 2 で path 6 物理化)
- click → tooltip で「from agent@vantage-point/worker/code-1、 2 min ago、 lint result preview」
- lead Claude が `vp msg recv --actor agent` で取得、 内容に応じて指示

### Ruby DSL 版 (Phase 5)

```ruby
# worker
Vp.send_to("agent@vantage-point/lead", {
  type: "lint_result",
  output: File.read("/tmp/clippy.txt"),
  ts: Time.now,
})

# lead 側
Vp.subscribe("agent@vantage-point/lead") do |msg|
  next unless msg.payload[:type] == "lint_result"
  # ... handle lint result ...
end
```

---

## 9. FAQ

### Q. v1 syntax は廃止される?

A. **廃止しない**。 v1 `<actor>@<project>` は v3.1 で default lane = `lead` に解釈、 forward-compat。 既存 dogfood / Ruby DSL / CLI を書き換える必要なし。

### Q. actor 名を省略すると何になる?

A. **`agent`** (= reserved default)。 `vantage-point/lead` = `agent@vantage-point/lead`。 sidebar lane label をそのまま address として打てる。

### Q. lane 名と actor 名が衝突した場合は?

A. 衝突しない設計。 actor は `@` の左、 lane は `/` の中。 構文上 disambiguous (`agent@vantage-point/lead` の `lead` は lane segment、 `agent` は actor)。 reserved actor 名 (`agent` / `notify` / `mcp` / `protocol` / `world` / `*`) は lane segment / project name でも reject (= validate error)。

### Q. hub.chronista.club が落ちたら何が起きる?

A. **LAN msg は影響なし** (= mDNS direct path、 hub 経由しない)。 self-process / same machine / LAN は hub 不要。 Internet 経由 (`<actor>@<user>/...`) のみ影響、 sender outbox に retry queue で TTL 内に再送 attempt。

### Q. 自分で hub を立てられる?

A. はい (Phase 4+)。 hub spec は ~500 LOC の Rust or TypeScript で実装可能、 self-host で `your-hub.example.com` を運営できる。 `vp world add taro@your-hub.example.com` で接続可能。 vendor lock-in なし。

### Q. msg payload は hub に見える?

A. **見えない** (Phase 4+)。 payload は receiver pubkey で NaCl `crypto_box_seal` 暗号化、 hub は envelope (= routing info、 from/to/ts/sig) のみ見る。 payload は receiver 以外復号不能。

---

## 関連

- **Spec**: [docs/spec/msgbox-address-v3.md](../spec/msgbox-address-v3.md)
- **Design**: [docs/design/14-msgbox-address-v3.md](../design/14-msgbox-address-v3.md)
- **Linear Epic**: [VP-144](https://linear.app/chronista/issue/VP-144)
- **Phase sub-issues**: [VP-145](https://linear.app/chronista/issue/VP-145) [VP-146](https://linear.app/chronista/issue/VP-146) [VP-147](https://linear.app/chronista/issue/VP-147) [VP-148](https://linear.app/chronista/issue/VP-148)
