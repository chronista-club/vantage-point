# Mailbox address v3.1 — federated identity + simple syntax

> **Status**: Draft (VP-144 Epic、 Phase 0 SDG)
> **Linear**: [VP-144](https://linear.app/chronista/issue/VP-144)
> **Supersedes**: `creo-memories: project_mailbox_address_spec.md` (v1、 2026-04-19)
> **Phase 0 (本 doc)**: Why + What。 How は [docs/design/14-mailbox-address-v3.md](../design/14-mailbox-address-v3.md)、 Usage は [docs/guide/mailbox-address-usage.md](../guide/mailbox-address-usage.md)

---

## Why — 起点

VP-143 (#304) ship 後の 2026-05-08 dogfood で、 vantage-point/lead と creo-memories/lead の間で actor msg を相互送受信したところ、 4 件の整合性 gap が判明 (`mem_1CapRAtpCpahQGn8nW2fmT`):

| # | gap | 結果 |
|---|-----|------|
| 1 | `mcp` actor の registry 非対称 | self-process 内では存在するが TheWorld registry 未登録 → cross-process `mcp@<other>` 送信は forward 失敗 (silent drop) |
| 2 | MCP `msg_recv` の inbox scope 固定 | self process の `mcp` actor inbox 限定、 他 actor (`agent` / `notify` / `protocol`) は MCP tool で観察不可 |
| 3 | lane address (`<project>/<lane>`) と actor address (`<actor>@<project>`) の **2 namespace 混乱** | user が `vantage-point/echoes` を mailbox address と誤認 → parse error、 mental model split |
| 4 | cross-process recv observation gap | `agent` inbox に msg deliver 完了しても receiver 側で即時検知不可、 sidebar UI / CLI watch 経路必要 |

加えて user vision: **将来的にマシンだけじゃなく、 他の LAN の PC や、 hub.chronista.club 経由でアドレス解決して、 ネット経由で msg 交換したい**。

## ありたい姿 (= prime directive)

- **simplicity**: `<actor>@<location>` 1 syntax に集約、 入力負担最小、 認知コスト 1 個
- **federated**: process / machine / LAN / Internet の 4 layer 全てを同 syntax で表現
- **2 namespace 解消**: lane address と actor address を統合、 sidebar の lane label がそのまま mailbox address として使える
- **dogfood-driven**: 既存 protocol idiom (SMTP / DNS / Matrix / Nostr) を mash-up、 自前 protocol 設計を最小化

---

## What — Address syntax v3.1

### BNF

```
address  = (actor "@")? location
actor    = [a-zA-Z0-9_-]+ | "*"  // 省略時 default = "agent"、 `*` は broadcast wildcard (reserved)
location = (world "/")? project ("/" lane)?
world    = world-segment ("." world-segment)*    // DNS-like
project  = [a-zA-Z0-9_-]+                         // reserved: "world" = system project
lane     = lane-segment ("/" lane-segment)*
world-segment = [a-zA-Z0-9_-]+
lane-segment  = [a-zA-Z0-9_-]+
```

### separator 役割直交

- `@` = actor / location 境界 (1 個だけ)
- `/` = internal hierarchy (world → project → lane)
- `.` = host DNS-qualifier (mDNS / Internet)

3 separator が役割重複なく **完全直交**。

### 4 階層 (= location 内 + actor)

```
agent @ mako.chronista.club / vantage-point / worker / objrec
  ^         ^                      ^             ^
  actor    world identity         project       lane (multi-segment 可)
        (host = machine / user / hub)
```

| 階層 | 役割 |
|------|------|
| **actor** | 受信 inbox の役割 (= "誰が読むか"、 default = `agent`) |
| **world** | identity namespace (= machine / user / hub、 host segment) |
| **project** | VP project (= self world に register された project name、 reserved: `world`) |
| **lane** | lane within project (= multi-level、 `worker/objrec` 等) |

### 4 layer matrix

| address | layer | meaning | resolve |
|---------|-------|---------|---------|
| `agent` | self process | mailbox-local | direct dispatch |
| `vantage-point/lead` | same machine | self world、 lead lane の agent inbox | TheWorld registry (port lookup) |
| `notify@vantage-point/lead` | same machine | OS notification trigger | local routing |
| `mako/vantage-point/lead` | Internet via hub | mako world、 hub-resolved | `hub.chronista.club` query (Phase 4+) |
| `mako.chronista.club/vantage-point/lead` | Internet (explicit hub URL) | full FQDN | hub URL inline |
| `macbook.local/vantage-point/lead` | LAN | mDNS resolve | `_vp._tcp.local` (Phase 3) |
| `*@vantage-point/lead` | broadcast | lead lane 全 actor | local fanout |
| `hermit_purple@world` | self world (system) | TheWorld daemon の actor | (reserved project `world`) |
| `hermit_purple@mako/world` | Internet | mako world's TheWorld daemon | hub query |

### actor optional の効果

- **default actor = `agent`** で省略可、 sidebar の lane label そのものが address として使える
- 入力 UX: `vp msg send vantage-point/lead "hello"` → 自動で `agent@vantage-point/lead` 解釈
- mental model: 「sidebar に出ている文字列 = mailbox address」 (= 統合)

### email idiom との parallel

- email: `info@example.com` (= info role + example.com domain)
- VP: `agent@vantage-point/lead` (= agent role + location)、 agent 省略可で `vantage-point/lead`
- 役割明示の `notify@<...>` / `mcp@<...>` / `protocol@<...>` は SMTP の `postmaster@` `noreply@` `abuse@` と同 family

---

## Identity model — Ed25519 pubkey + alias

### 各 world は keypair で identify

| 要素 | 値の例 |
|------|--------|
| **pubkey** | `ed25519:6f3e...` (= 32-byte fingerprint、 永続不変の真の identity) |
| **alias** | `mako` (= human-readable claim、 hub 単位で unique) |
| **endpoint** | `wss://relay-x.chronista.club/<routing-token>` (= hub registered 接続先) |

### 設計選択

- **pubkey 中心** = self-sovereign identity (Nostr / Bluesky / WebID family、 vendor lock-in なし)
- **alias** は hub 単位の DNS 風 claim、 user が pubkey を持っていれば hub 移行 / self-host 可能
- **endpoint** は hub 経由 push relay の connection token、 user が掌握

### address 内の identity 表現

```
mako                       → alias (default hub = chronista.club)
mako.chronista.club        → fully qualified (hub URL inline)
mako.taro.chronista.club   → multi-tenant hub (= hub 内 user/group hierarchy)
macbook.local              → mDNS local (LAN)
```

---

## Reserved names

### reserved actor

| actor | 役割 |
|-------|------|
| `agent` (default) | inter-agent generic comm (= Claude / human) |
| `notify` | OS notification trigger (DistributedNotification 等) |
| `mcp` | MCP server self inbox |
| `protocol` | system protocol (control plane) |
| `lane-spawn` / `sp-bootstrap` | infra reserved |
| `*` | broadcast (special、 actor wildcard) |

### reserved project

| project | 役割 |
|---------|-----|
| `world` | system project、 TheWorld daemon が holding (例: `hermit_purple@world`、 `hermit_purple@mako/world`) |

### reserved world-segment (将来予約)

| segment | 役割 |
|---------|-----|
| `local` | mDNS suffix (= `<machine>.local`) |
| `lan` | LAN broadcast (将来) |
| `net` | Internet root (将来) |

---

## v1 → v3.1 migration policy

### forward compatibility (= breaking change なし)

| v1 syntax | v3.1 解釈 | 備考 |
|-----------|-----------|------|
| `agent@vantage-point` | OK (= default lane = `lead` で routing) | v1 user 何もしなくて良い |
| `<actor>@<project>` (任意 actor) | OK (= default lane = `lead`) | reserved 名衝突は v1 と同 rule |

### v3.1 で新規対応

| v3.1 syntax | 動作 |
|-------------|------|
| `<location>` のみ (= actor 省略) | default actor = `agent`、 v3.1 新文法 |
| `<actor>@<project>/<lane>` | per-lane mailbox routing (Phase 2 で物理化) |
| `<host>/<project>/<lane>` | cross-world routing (Phase 3 で LAN、 Phase 4+ で hub) |

### migration phase

| Phase | 内容 | sub-issue |
|-------|------|-----------|
| **Phase 0** (本 doc) | SDG 3 file 整備 | [VP-145](https://linear.app/chronista/issue/VP-145) |
| **Phase 1** | Parser 拡張 + actor optional | [VP-146](https://linear.app/chronista/issue/VP-146) |
| **Phase 2** | per-lane mailbox + sidebar Echoes 横 icon | [VP-147](https://linear.app/chronista/issue/VP-147) |
| **Phase 3** | mDNS resolver — LAN MVP 完成 | [VP-148](https://linear.app/chronista/issue/VP-148) |
| Phase 4 | hub MVP (chronista.club) | (placeholder) |
| Phase 5 | Ruby DSL / CLI / sidebar UI 全面 v3 対応 | (placeholder) |
| Phase 6 | WebFinger / federation / cross-hub | (placeholder) |

LAN MVP (Phase 0-3) 完成後に Phase 4+ の planning session で sub-issue 化判断。

---

## Wildcard 拡張 (v1 継承 + 4 layer)

| pattern | 意味 |
|---------|------|
| `*@vantage-point` | project broadcast (v1 既存) |
| `*@vantage-point/lead` | project + lane broadcast |
| `*@macbook.local/vantage-point/lead` | LAN machine 内 lane broadcast |
| `*@mako/vantage-point/lead` | user-wide lane broadcast (全 machine、 Phase 4+) |

その他の wildcard (`agent@*`、 `*@*`、 glob) は採用しない (v1 仕様継承)。 高度 query は `vp.broadcast(...)` / `vp.find_actors(...)` API で。

---

## design quality 評価

### simplicity (prime directive)

- email より simple (= location only で書ける)
- `@` `/` `.` の三役 separator が直交、 認知コスト 1 個
- sidebar lane label と address 統合、 mental model 1 個

### federation (vision)

- pubkey 中心 self-sovereign identity
- hub は **endpoint resolver のみ** (NOT store-and-forward)、 spam / load 影響を受けず privacy preserve
- WebFinger 互換で multi-hub federation も自然に発展可能 (Phase 6+)

### compatibility (v1 → v3.1)

- v1 syntax 全部 forward-compat
- 新規 v3.1 features (host segment、 lane segment、 actor optional) は opt-in
- breaking 0 件で incremental migration

---

## 関連

- **Design**: [docs/design/14-mailbox-address-v3.md](../design/14-mailbox-address-v3.md) (How)
- **Usage**: [docs/guide/mailbox-address-usage.md](../guide/mailbox-address-usage.md) (Usage)
- **Linear Epic**: [VP-144](https://linear.app/chronista/issue/VP-144)
- **VP-24 Mailbox core**: `mem_1CZA6PxWEnKSwC5tCbm7bF`
- **Mailbox + Monitor agent inbox** (predecessor design): `mem_1CabUu1biCwMFjsX5oEoG9`
- **dogfood gap 詳細**: `mem_1CapRAtpCpahQGn8nW2fmT`
- **VP-143 完了** (本 spec の trigger): `mem_1CapFaggT8iy1XxMKAgUqA`
