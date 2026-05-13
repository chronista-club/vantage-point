# doc 20: VP-170 Phase 1 spike report — SurrealDB LIVE Query feasibility (Q1-Q3 結果)

> **Status**: Phase 1 spike core Q1-Q3 完了、 Q4-Q7 は Phase 2 移行判断後に追加検証
> **Linear**: [VP-170](https://linear.app/chronista/issue/VP-170) (parent: [VP-169](https://linear.app/chronista/issue/VP-169))
> **Date**: 2026-05-14
> **SurrealDB version**: v3.0.4 (= `Cargo.lock` 確認済、 embedded `kv-mem` で実機検証)
> **Test code**: `crates/vp-db/tests/spike_msgbox_live.rs`
> **Related**: doc 19 (`19-msgbox-whitesnake-primary.md`) §6 spike Q definitions

---

## §1 結論先出し

### 判定

```mermaid
flowchart TB
    Q1[Q1: LIVE + $bind] -->|✅ PASSED| OK1[SDG 主路 confirm]
    Q2[Q2: filter 脱落 event] -->|⚠️ no event| F2[consumer 側 設計補強要]
    Q3[Q3: atomic claim syntax] -->|❌ UPDATE ORDER BY LIMIT 不可| Fix3[SDG §4.1 主 query 修正必須]

    OK1 --> Verdict
    F2 --> Verdict
    Fix3 --> Verdict
    Verdict[Verdict: PROCEED WITH SDG REVISION]

    classDef pass fill:#bfb,stroke:#080,color:#000
    classDef partial fill:#fc9,stroke:#a60,color:#000
    classDef fail fill:#fbb,stroke:#a00,color:#000
    classDef result fill:#cdf,stroke:#048,color:#000
    class Q1,OK1 pass
    class Q2,F2 partial
    class Q3,Fix3 fail
    class Verdict result
```

| Q | result | severity | SDG 影響 |
|---|---|---|---|
| **Q1** LIVE SELECT + $bind 動作 | **✅ PASSED** | — | SDG §4.2 LIVE primary path confirm |
| **Q2** UPDATE で WHERE 脱落時 event 通知 | **⚠️ no event** | High (= 設計補強要) | SDG §4.2 / §4.6 で「consume 完了は他 consumer に notify されない」 を明示、 ack-back HTTP path で補完 (= 既存設計と整合) |
| **Q3** atomic claim `UPDATE ORDER BY LIMIT 1 RETURN AFTER` | **❌ syntax 不可** | Critical (= SDG 主 query path 動かない) | SDG §4.1 主 query 修正必須、 transaction wrap workaround は Phase 2 carry-over |

### Phase 2 着手可否

- **Phase 2 (受け皿 trait + 旧並走) 着手 OK**: Q1 で LIVE Query feasibility 確定、 Q2 no event は ack-back path で吸収済設計、 Q3 syntax 修正は Phase 2 PR-1 の claim 機構実装内で吸収可能
- **SDG §4.1 主 query path** は Phase 2 PR-1 着手前に **inline revision 推奨** (= follow-up commit)

---

## §2 Q1: LIVE SELECT + $bind 動作 ✅

### 検証 SQL

```sql
LIVE SELECT * FROM msgs
  WHERE to_actor=$actor AND to_lane=$lane
    AND status='active' AND consumed_at IS NONE
```

bind: `actor='agent'`, `lane='lead'`

### 結果

INSERT で **notification 即時受信** (= `Action::Create` + 全 field):

```json
{
  "action": "Create",
  "data": {
    "id": "msgs:`msg-1`",
    "to_actor": "agent",
    "to_lane": "lead",
    "status": "active",
    "payload": {"text": "hello"},
    ...
  }
}
```

### 意味

- **Purple Haze F2 finding (= v1.4.2 まで $bind 不可) は v3.0.4 で完全解消** を 1 次 source + 実機で confirm
- SDG §4.2 「LIVE Query primary、 fallback なし」 path に必要な前提が成立
- Phase 2 で `Handle::recv()` を LIVE stream + atomic claim ベースに実装可能

### Mermaid

```mermaid
sequenceDiagram
    participant T as test
    participant DB as SurrealDB v3.0.4 embedded

    T->>DB: LIVE SELECT WHERE to_actor=$actor AND to_lane=$lane
    DB-->>T: stream open OK
    Note over T: 100ms wait
    T->>DB: CREATE msg (to_actor=agent, to_lane=lead)
    DB-->>T: Notification { action: Create, data: {...} }
    Note over T: ✅ 1s 以内に届く
```

---

## §3 Q2: UPDATE で WHERE 脱落時 event ⚠️

### 検証 SQL

```sql
-- 既存 active row を LIVE で subscribe
LIVE SELECT * FROM msgs WHERE status='active' AND to_actor='agent' AND to_lane='lead'

-- 別 connection から row を archived に
UPDATE type::record('msgs', 'msg-2') SET status='archived', status_at=$now
```

### 結果

**1 秒以内に event 来ず timeout** (= filter 脱落時 LIVE は何も notify しない)

### 意味

SurrealDB v3.0.4 の LIVE は **INSERT 時のみ notify** (= INSERT 内 WHERE 条件マッチ判定はあるが、 UPDATE で WHERE 外に出た既存 row は何も通知しない)。

#### consumer 視点での影響

- **正の影響**: consumer が claim → consume → `UPDATE consumed_at=now` した時、 他 consumer の LIVE に notify されない = 他 consumer の `recv().await` が誤起動しない (= claim race の確率低下)
- **負の影響**: observer pattern (= consume せず watch) で「**msg が消費された**」 イベントを観測する path が消える。 sidebar UI / `vp msgbox watch` で active inbox の lifecycle 観測には ack-back HTTP 経路が必須

#### SDG §4.6 ack-back path との整合

SDG §4.6 で既に **cross-process 「consume 完了」 は `/api/msgbox/consume-ack` HTTP POST で明示通知** と設計済。 Q2 finding はこの設計を **強制 (= LIVE 経由の代替 path がない)** にする。 同 SP 内 observer (= sidebar) も同じく ack-back の **local broadcast 経路** を別途用意する必要。

#### SDG 修正提案

§4.2 廃案リストに **observer pattern 用 secondary signal** を追記:

> consume 完了の同 SP 内 broadcast (= sidebar UI 用 observer): SDG §4.3 Pattern C 用に `tokio::sync::broadcast` for "msg consumed" event を SP 内に追加。 これは LIVE Query の補完 path であり、 fallback ではなく **observer pattern の物理化**。

---

## §4 Q3: atomic claim `UPDATE ORDER BY LIMIT` ❌

### 検証 SQL (= SDG §4.1 主 query)

```sql
UPDATE msgs SET claim_id=$cid, claimed_at=$now
 WHERE status='active' AND to_actor='agent' AND to_lane='lead'
   AND consumed_at IS NONE
   AND claim_id IS NONE
 ORDER BY ts ASC LIMIT 1
 RETURN AFTER
```

### 結果

```
Parse error: Unexpected token `ORDER`, expected Eof
 --> [5:22]
  |
5 | ORDER BY ts ASC LIMIT 1
```

**SurrealDB v3.0.4 では UPDATE statement に `ORDER BY` + `LIMIT` を直接付けられない**。

公式 syntax (= SurrealDB v3 docs から):
```
UPDATE [ ONLY ] @targets
    [ CONTENT @value | MERGE @value | PATCH @value | SET @field = @value ... ]
    [ WHERE @condition ]
    [ RETURN [ NONE | BEFORE | AFTER | DIFF | @field ... ] ]
    [ TIMEOUT @duration ]
    [ PARALLEL ]
```

`ORDER BY` / `LIMIT` は **listed されていない**。

### 意味

SDG §4.1 「atomic claim 機構」 主 query path は **SurrealDB v3.0.4 では動かない**。 Phase 2 (PR-1 受け皿 trait + 旧並走) で claim 機構実装する前に SDG §4.1 / §4.3 の main query path を revise 必須。

#### workaround paths (= Phase 2 PR-1 で選択)

##### A. Transaction wrap (= Purple Haze F4 obligatory path)

```sql
BEGIN;
LET $candidates = SELECT * FROM msgs
    WHERE status='active' AND to_actor=$actor AND to_lane=$lane
      AND consumed_at IS NONE AND claim_id IS NONE
    ORDER BY ts ASC LIMIT 1;
LET $target_id = $candidates[0].id;
UPDATE $target_id SET claim_id=$cid, claimed_at=$now;
COMMIT;
RETURN $target_id;
```

ただし spike で `$candidates[0].id` 抽出が想定通り動作せず (= 結果が `null` 返却)、 完全動作 + race-free 検証は **Phase 2 carry-over**。 仮説候補:
- subquery in LET の semantics が想定と異なる (= 結果が unwrap されてる?)
- `$candidates` が array of arrays として扱われてる
- `id` field が record id (`msgs:msg-1`) として alias されている影響

##### B. SurrealDB DEFINE FUNCTION で atomic 化

```sql
DEFINE FUNCTION fn::claim_one($actor: string, $lane: string, $cid: string) {
    LET $cand = SELECT * FROM msgs WHERE ... ORDER BY ts ASC LIMIT 1;
    IF count($cand) > 0 {
        UPDATE $cand[0].id SET claim_id=$cid, claimed_at=time::now();
        RETURN $cand[0];
    };
    RETURN NONE;
};
```

DB 内 function として atomic 化、 client は `RETURN fn::claim_one(...)` で呼ぶ。 性能 + race 性質を Phase 2 で bench。

##### C. 2-step approach + 楽観的 lock

1. SELECT で id + version 取得
2. UPDATE record id SET ... WHERE claim_id IS NULL AND version = $expected (= CAS pattern)
3. update count > 0 で claim 成功、 0 なら他 consumer が先に取った → retry

race-free だが retry loop 必要。 latency 増。

#### Phase 2 carry-over

実機 race-free verification + path A/B/C の選択は Phase 2 PR-1 (受け皿 trait + 旧並走) で。 spike では「SDG main query 不可」 を確定して、 path A/B/C の trade-off を Phase 2 設計で詳細化。

### SDG 修正提案 (= follow-up commit)

- §4.1 主 query example を transaction wrap (= path A) に rewrite、 ただし Phase 2 で path A/B/C 比較後に最終確定する旨を note
- §4.3 atomic_claim 実装例 (`Handle::atomic_claim()`) を Phase 2 で実装と同時に確定、 SDG では「Phase 2 で path A/B/C のいずれかを選択」 と placeholder

---

## §5 Q4-Q7 carry-over (= Phase 2 移行判断後に追加検証)

| Q | 内容 | carry-over 理由 |
|---|---|---|
| **Q4** | 100 msg/sec で latency < 50ms | Q3 syntax fix 後でないと bench 設計が dependent |
| **Q5** | 3 concurrent query (producer + consumer + GC) lock contention | Q3 path A/B/C 確定後の benchmark |
| **Q6** | recv_idx で active filter が大量 archived row scan に堕ちないか | partial index 機能調査 + 1000 row 蓄積 bench |
| **Q7** | LIVE stream embedded の切断 trigger と reconnect | shutdown 経路 + reconnect resilience integration test |

これらは Q3 path 確定後の Phase 2 開発中に実機 dogfood で観測する方が efficient。 spike PR で扱うより Phase 2 PR-1 / PR-2 の context で行う。

---

## §6 spike 副次 finding

### F-A: `type::thing` → `type::record` rename

SurrealDB v3.0.4 で record id 構築関数が rename:
- 旧: `type::thing('msgs', $id)`
- 新: `type::record('msgs', $id)`

VP の既存 code は `type::thing` 使用箇所なし (= 確認済)、 影響ゼロ。

### F-B: SCHEMAFULL の nested object → FLEXIBLE 必須

`payload` field を `TYPE object` で定義すると `payload.text` 等 nested field 書き込み不可:

```
Found field 'payload.text', but no such field exists for table 'msgs'
```

`TYPE object FLEXIBLE` (= TYPE の後に FLEXIBLE) で nested JSON 受入可能。 SDG §4.1 schema 定義に **`payload` field は FLEXIBLE 必須** を追記要。

### F-C: `FLEXIBLE` keyword の位置

```
Parse error: FLEXIBLE must be specified after TYPE
```

`FLEXIBLE TYPE object` (= TYPE の前) は不可、 `TYPE object FLEXIBLE` (= TYPE の後) のみ valid。

---

## §7 SDG (doc 19) revision 推奨項目

| section | 修正 | 緊急度 |
|---|---|---|
| **§4.1 schema** | `payload` field に `FLEXIBLE` 追加 (`TYPE object FLEXIBLE`)、 SCHEMAFULL での nested JSON 必須 (F-B/F-C) | High |
| **§4.1 主 query** | atomic claim を transaction wrap pattern に書き換え、 Phase 2 で path A/B/C 確定の note | Critical |
| **§4.2 reconnect** | Q2 「filter 脱落時 no event」 を明示、 observer pattern 用 secondary signal (= `tokio::sync::broadcast`) を §4.3 Pattern C で物理化と明記 | Medium |
| **§4.6 ack-back** | LIVE 経由の自然伝播ではなく **HTTP POST が必須** であることを強調 (= Q2 finding 反映) | Medium |
| **§6 spike Q 表** | Q1 ✅ / Q2 ⚠️ / Q3 ❌ の inline mark + 本 doc 20 への link | Low (= 本 PR で同梱) |

---

## §8 Phase 2 着手判断

### Go condition (= all satisfied)

- ✅ Q1 LIVE working (= SDG primary substrate 動作)
- ✅ Q2 finding が ack-back HTTP 既存設計と整合 (= 設計矛盾なし)
- ✅ Q3 syntax 修正の workaround path (A/B/C) が複数あり、 Phase 2 内で選択可
- ✅ F-B/F-C schema 制約は SDG §4.1 軽微修正で済む

### Stop condition (= 一つでも該当)

- ❌ なし

### 判定: **Proceed to Phase 2** (= SDG follow-up revise を 1 PR で吸収後、 Phase 2 PR-1 着手)

---

## §9 関連リソース

- **Linear**: [VP-170](https://linear.app/chronista/issue/VP-170) (= 本 spike issue)
- **SDG**: [doc 19](19-msgbox-whitesnake-primary.md) (= 検証対象設計)
- **test code**: `crates/vp-db/tests/spike_msgbox_live.rs`
- **review history**:
  - Moody Blues APPROVE_WITH_NITS (PR #354)
  - Purple Haze SPIKE FIRST (Q-PH 5 件、 本 spike で F2 解消 + F1/F10 即修正済)
- **SurrealDB official**: [LIVE SELECT docs](https://surrealdb.com/docs/surrealql/statements/live), [UPDATE syntax](https://surrealdb.com/docs/surrealql/statements/update)
