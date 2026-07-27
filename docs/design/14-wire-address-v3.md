> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# Design 14: Wire address v3.1 — How

> **改訂 (2026-05-21)**: 旧 msgbox 実装は 2026-05 の wiremsg 再設計 (R1〜R6、 PR #406〜#420) で全廃された。
> 本 doc の **address resolve cascade / identity model / hub / 暗号 / NAT traversal / federation 設計は wiremsg がそのまま継承**しており現行有効。
> ただし §7 の `MsgboxRouter` を key 化する旧実装記述・`msg_recv` への actor param 追加といった **実装詳細は撤去済 msgbox 固有**。 wiremsg では cross-process 配送が `wire_remote` 経由 (best-effort)、 recv が per-agent cursor (`wire_recv`) に置き換わっている。
> Phase 3+ の mDNS / hub / federation は wiremsg 上での将来計画として有効。

> **Status**: 設計は現行有効 (wiremsg が継承)。 旧称 "Msgbox address v3.1" (VP-144 Epic、 Phase 0 SDG)。
> **Spec**: [docs/spec/wire-address-v3.md](../spec/wire-address-v3.md) (Why + What)
> **Guide**: [docs/guide/wire-address-usage.md](../guide/wire-address-usage.md) (Usage)

本 doc は v3.1 の **How** を規定する。 syntax / identity model は spec を参照、 sample 利用例は guide を参照。 本 doc は実装側の決定事項に集中する。

---

## 1. Resolve cascade — 4 step fall-through

address parse 後、 **location 部分の resolve** を以下の順序で fall-through する。 1 pass、 simple。

```
1. location 不在        → self process inbox (= local dispatch、 即時)
2. location = project   → self world、 SP port を解決:
   2′. config (projects.kdl) の slot から local read-only 解決 (Tier 2′、 PR #425)
       — `sp_port_from_config` で port = PORT_RANGE_START + slot をネットワーク往復ゼロで算出
   2.  local miss (project 未登録 / slot 未割当) のときだけ TheWorld (:32000) へ HTTP fallback
       — `GET /api/world/port_for?project=<name>`。 TheWorld は slot の唯一の writer
3. location = host/...  → host を resolve:
   a. host が "<machine>.local" 形 (.local TLD)  → mDNS query (Phase 3)
   b. host が "<seg>" (single segment、 dot 不在) → mDNS first、 失敗時 hub query (Phase 4)
   c. host が "<seg>.<TLD>" (full FQDN)         → hub.chronista.club (Phase 4)
4. resolve 失敗                                 → error 返却 (silent drop しない)
```

### 設計上の決定

- **silent drop は禁止**: 既存 v1 dogfood で `mcp@<other>` 送信が ack を返しつつ silent drop していた問題を解消。 receiver 不在 / mDNS miss / hub 不到達は **明示的 error** で sender に return。
- **hub は last resort**: LAN (mDNS) を優先、 hub query は明示的 FQDN または mDNS miss 時のみ。 LAN 内の low-latency direct path を default に。

### resolve cache

- mDNS / hub の resolve 結果を **address book** (`~/.config/vp/addresses.toml`) に opt-in 保存
- TTL: mDNS は service announcement TTL に従う、 hub query は 1 hour default (= 中央依存 minimal)
- user CLI: `vp world add <alias>` で manual cache、 `vp world list` で表示

---

## 2. Hub spec (`hub.chronista.club`、 minimal)

### 役割 (= simplicity 担保)

| 役割 | 詳細 | 実装 |
|------|------|------|
| **Identity registry** | `<alias>` ↔ `<pubkey>` ↔ `<endpoint>` の 3-column registry | SurrealDB or PostgreSQL、 small footprint |
| **Endpoint resolver** | `GET /resolve/<alias>` → JSON (pubkey、 endpoint URL) | HTTP/JSON API、 cache 可 |
| **WSS push relay** | each world が `wss://hub/relay/<token>` 接続、 hub が msg push | Cloudflare Worker / fly.io / Deno Deploy |
| **WebFinger compat** | `GET /.well-known/webfinger?resource=acct:<alias>@<hub>` | (Phase 6+) federation 拡張 |

### 非役割 (= simplicity 担保)

- ❌ store-and-forward (= msg 永続保管)、 receiver offline 時は **bounce** で sender に return
- ❌ moderation (= spam / abuse 制御)、 user の trust list で対処
- ❌ group chat / federated state、 別 protocol
- ❌ msg payload の閲覧、 hub は header (= routing info) のみ見る、 payload は E2E encrypted

### implementation 規模

- ~500 LOC (Rust or TypeScript)
- 月数 USD 以内 (Cloudflare Worker / Deno Deploy / fly.io 等)
- single instance で十分 (= 100s of users 想定、 horizontal scale は federation で)

---

## 3. Auth & Encryption — End-to-End

### envelope structure

```
[envelope (= plaintext header)]
  from:    agent@mako.chronista.club/vantage-point/conductor
  to:      agent@taro.chronista.club/devbox/main
  msg_id:  ulid
  ts:      2026-05-08T...
  sig:     ed25519:<sender pubkey signature over envelope+payload hash>

[payload (= NaCl crypto_box encrypted)]
  <opaque ciphertext>
```

### crypto choices

- **signing**: Ed25519 (sender pubkey で envelope hash + payload hash を sign)
- **encryption**: NaCl `crypto_box_seal` (receiver pubkey で sealed box、 anonymous sender も可)
- **key exchange**: 不要 (NaCl box が internally Curve25519 ECDH)
- **forward secrecy**: not in scope (= single-shot msg、 long session ではないため)。 Signal 級 protocol 必要なら Phase 7+ で別途検討。

### 同 world 内 (= self process / same machine) の encrypt skip

- trust boundary 内では encrypt skip 可 (= performance、 同 process 内 wire 配送で encrypt 不要)
- LAN / Internet では mandatory encrypt
- v3.1 parser が address 解析後の resolve 結果に応じて encrypt path を auto select

### key 管理

- pubkey/privkey は `~/.config/vp/keys/` に保存 (mode 600)
- 初回 `vp world init` で keypair 生成、 hub に register (alias claim)
- key rotation は明示 user action (`vp world rotate-keys`)、 旧 pubkey は revocation list に

---

## 4. NAT traversal — WSS via hub (1 strategy)

### 問題

家庭 NAT で receiver world は inbound port 開けない。 inbound 不可なら hub 経由しか push 受信不能。

### 解決: WSS persistent connection

```
sender world ──(WSS)──> hub ──(push WSS)──> receiver world
                          │
                          └─ endpoint resolver (registry lookup)
```

- receiver world は起動時に `wss://hub/relay/<token>` 接続を **持続維持** (heartbeat 30s)
- hub は msg arrival で WSS push (= server-initiated)
- sender → hub は通常 HTTP POST (= request-response)

### LAN は hub 経由不要

- mDNS で同 LAN の world を直接発見、 QUIC で peer-to-peer 通信
- NAT 越え不要 (= 同 LAN なので)
- hub-down でも LAN 内 dogfood は影響なし (= local-first)

### 将来の transport 拡張

| transport | 用途 | priority |
|-----------|------|----------|
| QUIC over loopback | self process / same machine (既存) | 既存 |
| QUIC over LAN | mDNS-discovered LAN | Phase 3 |
| WSS via hub | Internet (NAT 越え) | Phase 4+ |
| WireGuard / Tailscale | LAN 拡張 / encrypted overlay | Phase 7+ (option) |
| libp2p / iroh | distributed mesh | Phase 8+ (option) |

LAN MVP (Phase 0-3) では QUIC over loopback + LAN の 2 transport で完結、 hub-related transport は別 Phase。

---

## 5. Reliability & Delivery

> **改訂 note (2026-05-21)**: 以下の `ephemeral` / `persistent` / `manual_ack` の 3 delivery flag は撤去済 msgbox 固有の semantics。 wiremsg 再設計 (R1〜R6) では delivery モデルが **per-agent 単一 cursor の accumulation** に置き換わった: message は wire に追記され、 受信側は自分の cursor を進めて未読を取得する (`wire_recv`)、 ack flag は不要。 cross-process 配送は `wire_remote` 経由の best-effort (R3)。 cross-world (hub / federation) の将来計画は引き続き有効。

### delivery 種別 (旧 msgbox、 historical)

| 種別 | 説明 | 既存 v1 | v3.1 |
|------|------|---------|------|
| **ephemeral (default)** | in-memory queue、 receiver online 必須 | ✅ | 同左 |
| **persistent** | SurrealDB outbox、 TTL あり (default 48h) | ✅ | 同左 |
| **manual_ack** | 受信側で `msg_ack(id)` 明示、 at-least-once 保証 | ✅ | 同左 |
| **cross-world outbox** | sender 側で retry queue、 hub bounce 受信時 sender に通知 | ❌ | **新規 (Phase 4)** |

### v3.1 cross-world reliability

```
sender world ──> outbox queue ──> hub
                                   │
                                   ├─ deliver to receiver (online) → ack → sender remove from outbox
                                   ├─ receiver offline (TTL内 retry) → eventually deliver
                                   └─ TTL expire → bounce notify to sender (dead-letter)
```

- LAN: outbox 不要 (immediate delivery、 失敗時 bounce)
- Internet: outbox + retry、 hub TTL 内なら eventual delivery

### 既存 v1 仕様継承

- `persistent` flag、 `manual_ack` flag、 `ttl_secs` 全て同 semantics 継承
- v3.1 拡張は cross-world delivery 部分のみ

---

## 6. Discovery — 「誰がどこにいるか」

### 4 つの discovery channel

| channel | scope | 実装 | priority |
|---------|-------|------|----------|
| **TheWorld registry** | self world | 既存 (cross-process 内) | 既存 |
| **mDNS** | LAN | `_vp._tcp.local` (`mdns-sd` crate) | Phase 3 |
| **address book** | manual | `~/.config/vp/addresses.toml` | Phase 3 |
| **hub directory** | Internet | `GET /users/public` (opt-in 公開 user 列挙) | Phase 4+ |
| **WebFinger** | cross-hub federation | `GET /.well-known/webfinger?resource=acct:...` | Phase 6+ |

### address book file format (Phase 3 想定)

```toml
# ~/.config/vp/addresses.toml
[[world]]
alias = "macbook"
pubkey = "ed25519:6f3e..."
endpoint = "https://macbook.local:32000"
discovered_via = "mDNS"
last_seen = "2026-05-08T07:00:00Z"

[[world]]
alias = "taro"
pubkey = "ed25519:abc..."
endpoint = "wss://hub.chronista.club/relay/xyz"
discovered_via = "hub"
last_seen = "2026-05-08T06:30:00Z"
```

---

## 7. Migration v1 → v3.1 — detail

### parser 互換性 layer

Phase 1 で parser 拡張時、 v1 syntax を v3.1 として解釈する rule:

| v1 input | v3.1 internal representation |
|----------|------------------------------|
| `agent` | `Address { actor: "agent", world: None, project: None, lane: vec![] }` |
| `agent@vantage-point` | `Address { actor: "agent", world: None, project: Some("vantage-point"), lane: vec!["conductor"] }` (default lane) |
| `*@vantage-point` | `Address { actor: "*", world: None, project: Some("vantage-point"), lane: vec!["conductor"] }` |

新 v3.1 input:

| v3.1 input | internal |
|------------|----------|
| `vantage-point/conductor` | `Address { actor: "agent", world: None, project: Some("vantage-point"), lane: vec!["conductor"] }` |
| `vantage-point/performer/objrec` | `Address { actor: "agent", ..., lane: vec!["performer", "objrec"] }` |
| `mako/vantage-point/conductor` | `Address { actor: "agent", world: Some("mako"), ... }` |
| `notify@vantage-point/conductor` | `Address { actor: "notify", ... }` |

### default lane policy

- v1 `<actor>@<project>` で lane 未指定 → v3.1 で `lane: vec!["conductor"]` に解釈
- これで「v1 user は何も変更不要」 + 「v3.1 で lane segment 明示は opt-in」

### lane segment 明示時の routing

> **Supersede note (= [doc 19](19-msgbox-whitesnake-primary.md) / VP-169)**: 以下の `MsgboxRouter` を `(actor, project, lane_path)` でキー化する HashMap-based 設計は **VP-169 で廃止**された。 msgbox は Whitesnake (SurrealDB embedded) primary に揃い、 per-lane 軸は HashMap key ではなく `msgs` table の DB row field (`to_actor` / `to_lane`) になった (doc 19 §4.5)。 `register` / `unregister` 機構も不要になり廃止。 address syntax / parser (本 doc §7 上段 + spec) は現行のまま有効。
>
> **さらに改訂 (2026-05-21)**: VP-169 の Whitesnake msgbox 自体も wiremsg 再設計 (R1〜R6) で全廃された。 現行の per-lane 配送は wire accumulation の per-agent cursor (`wire_recv`) で実現される。 doc 19 / `msgs` table への言及は historical。 address syntax / parser のみ現行有効。

- ~~Phase 2 で per-lane msgbox 物理化 (`MsgboxRouter` を `(actor, project, lane_path)` キー)~~ → VP-169 で DB row field 化
- ~~v1 既存 msgbox は spawn 時に `(actor, project, vec!["conductor"])` キーへ migration~~ → VP-169 で box concept 廃止
- ~~performer lane spawn で新 msgbox 自動 register、 lane delete で cleanup~~ → VP-169 で register/unregister 廃止、 consumer が `WHERE to_lane=$mine` で LIVE SELECT

### gap 1-4 の物理 fix

> **改訂 note (2026-05-21)**: 以下の gap 1/2/4 の fix 内容は撤去済 msgbox 固有の実装記述。 wiremsg では cross-process 配送が `wire_remote` の best-effort (R3)、 recv が per-agent cursor (`wire_recv`、 任意 actor の inbox を指定可能) に置き換わっている。 gap 3 (2 namespace 統合) は address syntax の話なので現行有効。

| gap | Phase | fix 内容 |
|-----|-------|----------|
| 1. mcp registry 非対称 | Phase 2 | reserved actor (`mcp` 含む) を per-process で registry 公開、 forward 失敗時 error 明示 |
| 2. MCP recv inbox 固定 | Phase 2 | recv に `actor` param 追加、 任意 inbox 観察可、 default は `agent` (= `mcp` から変更) |
| 3. 2 namespace 混乱 | Phase 1 | parser で location 統合、 sidebar lane label と address 統合 |
| 4. cross-process recv observation | Phase 2 | per-lane inbox + sidebar Echoes 横 message icon、 sidebar UI = primary observation path |

---

## 8. Phase implementation plan (LAN MVP)

### Phase 0: SDG ドキュメント (本 doc + spec + guide) — VP-145

scope: `docs/spec/wire-address-v3.md` + `docs/design/14-wire-address-v3.md` (本 doc) + `docs/guide/wire-address-usage.md`

deliverable: 3 file merged、 後続 Phase の議論 base 確立

### Phase 1: Parser 拡張 — VP-146

scope: wire address parser (現行は `crates/vantage-point/src/capability/wire_remote.rs` 周辺) で v3.1 syntax 受け付け

deliverable: 全 sample address parse OK、 v1 互換維持、 unit tests 10+ cases (t-wada 流テストピラミッド)

### Phase 2: per-lane inbox + sidebar Echoes 横 icon — VP-147

> **Supersede note (= [doc 19](19-msgbox-whitesnake-primary.md) / VP-169)**: `MsgboxRouter` の `(actor, project, lane_path)` キー化は VP-169 で廃止。 per-lane は DB row field 化。
>
> **さらに改訂 (2026-05-21)**: VP-169 msgbox 自体も wiremsg 再設計で全廃。 per-lane 配送は wire accumulation の per-agent cursor に置き換わった。 sidebar 側の lane inbox 観測 (`SidebarState` の poller) は引き続き有効。

scope:
- per-lane 配送 → wiremsg では wire address (`<actor>@<project>/<lane>`) の cursor recv で実現
- sidebar の lane inbox state + poller
- sidebar `.vp-message-icon` を Echoes icon の右隣に配置 (未読あり = filled、 なし = 表示なし)

deliverable: dogfood で 2 lane 間 msg 視認、 gap 1+2+4 解消

### Phase 3: mDNS resolver — VP-148 (LAN MVP terminal)

scope:
- mDNS register: `_vp._tcp.local` で TheWorld daemon 起動時 announce
- mDNS discover: `vp world list --lan` CLI
- address book: `~/.config/vp/addresses.toml`
- LAN cross-machine msg: QUIC over LAN、 NaCl encrypt + Ed25519 sign

deliverable: macbook-a と macbook-b で `vp wire send --to agent@macbook-b/vantage-point/conductor --body "hello"` が動く

### Phase 4-6 (placeholder、 LAN MVP 完成後 planning)

- Phase 4: hub MVP deploy
- Phase 5: Ruby DSL / CLI / sidebar UI 全面 v3 対応
- Phase 6: WebFinger / federation / cross-hub

---

## 9. 既存 protocol idiom 起用 mapping

VP v3.1 は **自前 protocol 設計を最小化** し、 既存 protocol idiom を mash-up:

| component | idiom 起用元 | 理由 |
|-----------|--------------|------|
| address syntax (`<actor>@<location>`) | SMTP / email | 認知コスト最小、 user familiar |
| location hierarchy (`/` 階層) | URL path | hierarchical reading natural |
| host segment (`.` qualifier) | DNS | LAN (`.local`) / Internet (FQDN) でそのまま使える |
| Ed25519 pubkey identity | Nostr / Bluesky / WebID / SSH | self-sovereign、 vendor lock-in なし |
| NaCl crypto_box | NaCl/libsodium standard | well-audited、 simple API |
| WSS via hub for NAT | Matrix homeserver / Discord gateway | proven scale、 simple deploy |
| mDNS service discovery | Bonjour / Avahi / Apple AirPlay | zero-config LAN |
| WebFinger federation | ActivityPub / Mastodon | cross-hub interop free |
| TXT record metadata | DNS-SD / SRV record | mDNS standard |

自前定義は **address BNF と Phase 計画のみ**、 transport / crypto / discovery / federation は全て standard idiom そのまま。

---

## 10. Open Questions (LAN MVP 完成前に解決)

| # | 課題 | 解決 deadline |
|---|------|--------------|
| Q-1 | mDNS service name (`_vp._tcp.local` で OK か、 collision 検査) | Phase 3 着手前 |
| Q-2 | LAN QUIC port (= 既存 TheWorld 32000 流用 or 別 port?) | Phase 3 着手前 |
| Q-3 | pubkey 初期生成タイミング (= `vp world init` 専用 cmd? 既存 daemon 起動時 implicit?) | Phase 3 着手前 |
| Q-4 | address book 編集 UI (CLI のみ or vp-app sidebar 内?) | Phase 3 中盤 |
| Q-5 | mDNS announce 内 alias (= `<machine_name>.local` で衝突した時の対処) | Phase 3 dogfood で発覚予定 |
| Q-6 | LAN 暗号化 mandatory or optional (trust boundary の判断) | Phase 3 着手前 |

---

## 関連

- **Spec**: [docs/spec/wire-address-v3.md](../spec/wire-address-v3.md)
- **Guide**: [docs/guide/wire-address-usage.md](../guide/wire-address-usage.md)
- **Linear Epic**: `VP-144`
- **Phase sub-issues**: `VP-145` `VP-146` `VP-147` `VP-148`
- **dogfood gap**: `mem_1CapRAtpCpahQGn8nW2fmT`
- **VP-24 Msgbox core**: `mem_1CZA6PxWEnKSwC5tCbm7bF`
- **Msgbox + Monitor agent msgbox** (predecessor): `mem_1CabUu1biCwMFjsX5oEoG9`
