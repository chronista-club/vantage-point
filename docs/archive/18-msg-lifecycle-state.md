# 18. msg lifecycle state — forwarded / consumed flag による cross-process 重複再配信遮断

> **対象 Issue**: [VP-164](https://linear.app/chronista/issue/VP-164) — SP restart で永続 cross-process msg が（重複）再配信される（受信側 ack-back 欠如、 restore_pending 再 forward）
> **親 Epic**: [VP-156](https://linear.app/chronista/issue/VP-156) — Msgbox routing 統一 + 永続化 first-class
> **関連設計**: [17-port-stability-and-msgbox-isolation.md](17-port-stability-and-msgbox-isolation.md)（決定D `restore_pending` の project 境界 guard）/ [16-worker-lane-msgbox-recv.md](16-worker-lane-msgbox-recv.md)（VP-166 で実装した recv path）/ [14-wire-address-v3.md](14-wire-address-v3.md)（address syntax / `normalize_from`）
> **関連 Issue**: [VP-158](https://linear.app/chronista/issue/VP-158)（全 msg 永続化、 PR #325、 本設計の前提）/ [VP-161](https://linear.app/chronista/issue/VP-161)（cross-machine replay、 Phase 2 で foundation 提供）
> **改訂 (2026-05-21)**: 本 doc を superseded した doc 19 (Whitesnake-primary msgbox) 自体も、 その後の **wiremsg 再設計 (R1〜R6、 PR #406〜#420) で全廃**された。 wiremsg は per-agent cursor accumulation のため `forwarded_at` / `consumed_at` flag や `msgs` table を持たず、 cross-process 重複再配信問題は別 model で構造的に解消されている。 本 doc は historical reference として残置。

> **Status**: **Superseded by [doc 19](19-msgbox-whitesnake-primary.md)** — 本 doc の `forwarded_at` / `consumed_at` schema は doc 19 §4.1 の `msgs` table schema に統合され、 VP-169 epic（Phase 5 完了, commit `445190c`）で実装された。 本 doc は VP-164 の起点となった lifecycle state 設計の historical reference として残置する。
>
> **Superseded note**: 本 doc は `Message` struct に `forwarded_at` / `consumed_at` の dual flag を足して cross-process 重複再配信を遮断する設計だった。 doc 19 (VP-169) で mpsc substrate を Whitesnake-primary に揃えた際、 これらの flag は `msgs` table の `forwarded_at` / `consumed_at` field として schema に統合された（doc 19 §4.1）。 本 doc が依拠していた `Router::remote_forward_loop` / `restore_pending` / mpsc `Router` は doc 19 Phase 5 で物理削除済のため、 本 doc の決定α〜ε（特に `restore_pending` の skip guard）は DB primary 化によって構造的に置き換わった（= status field + claim 機構 + ack-back HTTP path、 doc 19 §4.6〜§4.8）。 VP-164 が解決しようとした「SP restart で永続 cross-process msg が重複再配信される」 症状は、 doc 19 epic で root 解消されている。

## Abstract

dogfood（2026-05-11、 VP-163 lead 間 msg 送受信 fix 直後）で「`msg_send to=agent@creo-memories` → 1 回 recv して ack → SP-A を `vp sp restart` → 再び同じ msg が receiver box に湧いて出る、 しかも 1 回の restart で `49dab0d6` が **2 回**重複到達」 が観測された。 構造上 SP restart N 回で receiver には N+1 回到達する線形 bug。

### 問題の本質を一行に

VP-164 の症状は **「sender 側 SP は forward 成功後も msg を Whitesnake から消さない」** に還元される。 そして消せない理由は **`Message` struct が「forward 済」 という state を持っていない** から。 これで全症状が説明できる:

| | 症状の源 | 修正方向 | 本設計の決定 |
| -- | -- | -- | -- |
| **(α)** | `Router::remote_forward_loop`（msgbox.rs:581-604）が `client.forward().await` 成功時に何もしない（warn ログすら無し） → sender 側 Whitesnake に msg が残る | `Message` に **`forwarded_at: Option<i64>`** を持たせ、 forward 成功時に sender が `ws.extract` で更新（= 「既読」 マーク）| 決定α |
| **(β)** | `http_forward`（msgbox_remote.rs:650）は receiver が返す `{"status":"delivered"}` を捨て、 status code だけ確認 → sender に ack-back の情報が伝わらない | response body をパースして `delivered` を sender 側 cleanup の trigger に。 ack-back の **wire は既にある** | 決定β |
| **(γ)** | `restore_pending`（msgbox.rs:909-967）が「forward 済」 区別を持たず全件再投入。 VP-165 PR-3 の project guard は別 project のみ skip、 自 project の forward 済 msg は通過 | restore 時に `forwarded_at != None` の msg は skip + warn。 物理削除は GC に委ねる（= `consumed_at` が立った msg を TTL 内でも掃く） | 決定γ |
| **(δ)** | `Message` lifecycle は「未配信 / 配信完了」 の 2 状態しか表現できず、 「forward 済だが receiver consume 未確認」 という第 3 状態を可視化できない → post-mortem 不能、 VP-161 cross-machine replay の前提も曖昧 | **dual flag** `forwarded_at` + `consumed_at` で 3 状態（Pending / Forwarded / Consumed）を明示。 Phase 1 で `forwarded_at` のみ実装、 Phase 2 で `consumed_at` + receiver → sender 2nd ack 経路を追加（= VP-161 foundation） | 決定δ |
| (ε) | 既存滞留 msg（= 過去 send 分で sender WS に残ってる forward 済 msg）が migration で arbitrary な lifecycle 上にある | 起動時 1 回限り migration で「Whitesnake に居て `forwarded_at` 無しの msg」 は legacy 扱い、 conservatively `forwarded_at = now()` を立てて再 forward を止める。 TTL 失効で自然消滅 | 決定ε（任意、 PR で判断）|

(α)(β)(γ)(δ) は同じ「state を msg に乗せる」 設計の異なる側面 = 4 つで 1 つ。 Phase 1（= 本 doc Implementation 表）で `forwarded_at` のみ実装すれば dogfood 症状（重複配信）は完全に止まる。 Phase 2（= VP-161 連動）で `consumed_at` 経路を入れて msg lifecycle を完全観測可能にする。

### なぜ flag path（既読・未読マーク）を選んだか — 削除 path との比較

設計選択肢を整理:

| approach | sender WS state | observability | msg loss risk | VP-161 foundation | scope |
| -- | -- | -- | -- | -- | -- |
| A: ack-back で sender 即削除 | 削除 | ✗（履歴消える） | あり（receiver crash 前に sender 削除済） | △ | S |
| B: A + receiver msg_id dedupe set | 削除 | ✗ | あり | △ | M |
| C: A + B + consumed ack で完全削除 | 削除 + 2nd ack 経由 | ✗ | なし | ◎ | L |
| **本設計（F-2 dual flag）** | flag で残す（= history log） | **✅✅ msg lifecycle 完全観測** | なし（unconsumed なら再送条件で復活）| ◎ | M |

flag path を採用する根拠:

1. **観察可能性 first**: VP は「TUI で msg lifecycle 全観測する」 文化（VP-83 系譜）。 msg を WS から消すと post-mortem ができなくなる。 flag で残せば `vp msg list` で `Pending / Forwarded / Consumed` が一目、 dogfood が回る
2. **VP-161 foundation**: 「forwarded_at != None && consumed_at == None」 = 「届いたが consume されてない」 という第 3 状態が一級市民になる → cross-machine replay で「未配信」 を厳密定義できる
3. **msg loss なし**: Phase 1 + Phase 2 揃った状態で、 「forwarded_at が立ってる && consumed_at が立ってない」 msg は restart 時に **再送条件** で再 forward 可能（= 削除 path の C と等価）
4. **schema 拡張で十分**: 既存 `Message` struct に `Option<i64>` 2 つ足すだけ、 protocol 追加は Phase 2 の 2nd ack のみ。 削除 path の C と比べて scope 同程度かやや軽い

## 現状 — コード上の事実

### (α) sender 側 forward 成功時の cleanup 経路が無い

`Router::remote_forward_loop`（`crates/vantage-point/src/capability/msgbox.rs:581-604`）:

```rust
async fn remote_forward_loop(mut rx, client, shutdown) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            item = rx.recv() => {
                let Some((resolved, msg)) = item else { break };
                if let Err(e) = client.forward(&resolved, msg).await {
                    tracing::warn!("Router: remote forward 失敗 to='{}' err={}", ...);
                }
                // ⚠️ Ok(()) 時: 何もしない、 sender WS の msg は残る
            }
        }
    }
}
```

forward 成功は receiver の msgbox に投函成功（= HTTP 200 受信）を意味するが、 sender 側 Whitesnake には何のマークも書かれない。 `routing_loop`（msgbox.rs:771）で `ws.extract` 永続化された msg は **forward 後も Pending と区別不能**。

### (β) ack-back の wire は既にある（activate されていない）

receiver 側 `msgbox_remote_deliver_handler`（`crates/vantage-point/src/process/routes/health.rs:448-451`）は成功時に:

```rust
Ok(()) => (
    axum::http::StatusCode::OK,
    Json(serde_json::json!({"status": "delivered", "to": msg.to})),
),
```

を返している。 だが sender 側 `http_forward`（`crates/vantage-point/src/capability/msgbox_remote.rs:650-657`）は:

```rust
let resp = req.send().await?;
if !resp.status().is_success() {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    anyhow::bail!("HTTP {}: {}", status, body);
}
Ok(())  // ⚠️ success 時は body を捨て、 status code だけ確認
```

→ body の `delivered` 情報は捨てている。 sender に ack-back を伝える wire は存在するが、 cleanup を起動する経路がない。

### (γ) restore_pending には「forward 済」 区別がない

`Router::restore_pending`（`crates/vantage-point/src/capability/msgbox.rs:909-967`）は WS に残っている msg を全件 `router_tx` に再投入する。 VP-165 PR-3（commit `efb7855`）で `msg_is_foreign_to_local` guard が入って **異 project 宛/発の msg は skip** されるようになったが、 **自 project の forward 済 msg は guard を通過** して再投入 → routing_loop で再 forward。

caller は `capabilities.rs:125` の 1 箇所のみ（= 「2 回呼ばれる」 仮説は否定済、 dogfood 観測の「2 回」 は初回 send + 1 回 restart の単純合計）。

### (δ) Message struct は state を持っていない

`Message` struct（`crates/vantage-point/src/capability/msgbox.rs` 周辺）は現状:

```rust
pub struct Message {
    pub id: String,
    pub to: String,
    pub from: String,
    pub body: serde_json::Value,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    // ⚠️ forwarded_at / consumed_at が無い
}
```

→ 「Pending / Forwarded / Consumed」 の lifecycle state を表現する手段がない。 sender / receiver の挙動に state は乗っているが、 永続化されないので restart で失われる。

## Target Model

### 決定α: `Message` に `forwarded_at` + `consumed_at` を追加（Phase 1 では `forwarded_at` のみ書き込み）

```rust
pub struct Message {
    pub id: String,
    pub to: String,
    pub from: String,
    pub body: serde_json::Value,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    /// forward 成功時刻（ms）。 cross-process msg のみ意味を持つ（local 配信は `consumed_at` のみ更新）。
    /// `Some(_)` = receiver の box に到達済、 sender 側 restart で再 forward する理由がない。
    /// `None` = 未配信（routing_loop に乗ってる or restart で復元すべき）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded_at: Option<i64>,
    /// recv 完了時刻（ms）。 receiver が `recv()` で取り出して ack した時に更新。
    /// Phase 1 では local 配信のみ書き込み（既存 auto-ack 経路）、 Phase 2 で cross-process の
    /// 2nd ack 経路を追加。 VP-161 cross-machine replay の「未配信」 定義は
    /// `forwarded_at != None && consumed_at == None` を「届いたが consume されてない」、
    /// `forwarded_at == None` を「未配信」 として区別する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<i64>,
}
```

- `serde(default)` + `skip_serializing_if`: 既存 DISC（= flag 無し）との互換性、 永続層の wire format 後方互換
- state machine: `[Pending: 両 None]` → `[Forwarded: forwarded_at = Some]` → `[Consumed: forwarded_at = Some, consumed_at = Some]`
- local 配信は `forwarded_at` を skip（= sender == receiver で意味なし）、 `consumed_at` のみ recv 時に立つ
- cross-process では `forwarded_at` 必須、 `consumed_at` は Phase 2 で

### 決定β: forward 成功時に sender が `forwarded_at` を更新

`Router::remote_forward_loop`（msgbox.rs:581-604）の Ok(()) 経路を:

```rust
if let Err(e) = client.forward(&resolved, msg.clone()).await {
    tracing::warn!("Router: remote forward 失敗 ...");
} else {
    // 決定β: forward 成功 → forwarded_at を更新して WS に upsert
    if let Some(ws) = &whitesnake {
        let mut forwarded_msg = msg.clone();
        forwarded_msg.forwarded_at = Some(now_ms());
        let key = format!("{}{}", MAILBOX_KEY_PREFIX, msg.id);
        if let Err(e) = ws.extract(MAILBOX_NAMESPACE, &key, &forwarded_msg).await {
            tracing::warn!("Router: forwarded_at 更新失敗 id={} err={}", msg.id, e);
        }
    }
}
```

- `remote_forward_loop` に `Option<Whitesnake>` を渡す（= `new_inner` 経由で clone）
- `ws.extract` は同 key で idempotent upsert（既存仕様）→ flag 更新も安全
- 失敗時は warn のみ（= 次の restart で再試行可能、 msg は再 forward されるが receiver 側 dedupe は無し → 重複到達するが既存 broken state と同等以下）
- `http_forward`（決定β2）と組み合わせて「receiver からの 200 OK 受信」 を ack-back と解釈

### 決定β2: `http_forward` が response body を読んで `status == "delivered"` を確認（任意、 minimal は status code で十分）

現状の `http_forward` は status code だけ見ているが、 これは Phase 1 では維持で良い（= 200 OK = forward 成功）。 Phase 2 で `consumed_at` 経路を入れる時に response body 構造を拡張（例: `{"status": "delivered", "consumed_at": null}` + receiver が consume したら別 endpoint で 2nd ack）。

→ Phase 1 では決定β = 「Ok(()) 時に forwarded_at 更新」 のみで完結、 wire format 変更不要。

### 決定γ: `restore_pending` が `forwarded_at != None` を skip

`Router::restore_pending`（msgbox.rs:909-967）に追加 guard:

```rust
for disc in discs {
    let msg = disc.extract::<Message>()?;
    if msg.is_expired() { /* 削除 */ continue; }
    // VP-165 (D): 異 project の msg は復元しない（既存）
    if let Some(local) = &local_project
        && msg_is_foreign_to_local(&msg, local)
    {
        tracing::warn!("Msgbox: restore skip — 異 project ...");
        skipped_foreign += 1;
        continue;
    }
    // VP-164 決定γ: forward 済の msg は再投入しない（重複配信遮断）
    if msg.forwarded_at.is_some() {
        tracing::debug!(
            "Msgbox: restore skip — forwarded 済 (id={} to={} forwarded_at={:?})",
            msg.id, msg.to, msg.forwarded_at
        );
        skipped_forwarded += 1;
        continue;
    }
    self.router_tx.send(msg).await?;
    restored += 1;
}
```

- Phase 2 で「`forwarded_at != None && consumed_at == None` を retry policy で再送」 を追加する場合は、 別経路（= `vp msg replay` 手動 trigger or VP-161 cross-machine replay の自動経路）に分離。 default の `restore_pending` は **Phase 1 では skip のみ**（= 受信側に到達済の msg を盲目的に再送しない安全側）
- `skipped_forwarded` カウンタを logging（debug + sum で info）

### 決定δ: `consumed_at` 更新経路（Phase 1 = local 配信のみ、 Phase 2 = cross-process 2nd ack）

#### Phase 1: local 配信の auto-ack で `consumed_at` 更新

既存 `Handle::ack`（msgbox.rs:483 付近）が `ws.remove` する経路を、 「remove ではなく `consumed_at` 更新で残す」 に変更するかは **判断分岐**:

- **(δ-a) remove 維持**: local 配信は WS から消す（既存挙動）。 Phase 1 では `consumed_at` は cross-process 専用に予約。 minimal scope、 既存テスト影響なし
- **(δ-b) `consumed_at` 更新で残す**: local 配信も history log として WS に残す。 observability ↑、 ただし WS 肥大化、 GC を「`consumed_at != None && now - consumed_at > TTL_CONSUMED`」 で短めに掃く設計が必要

→ **Phase 1 = (δ-a)** で minimal（= remove 維持）、 Phase 2 で (δ-b) を検討。 dogfood で「local msg lifecycle も観察したい」 要求が出たら upgrade。

#### Phase 2: cross-process 2nd ack

receiver の `recv()` 完了時（= `Handle::recv` で取り出した時 or 明示 ack）に、 receiver SP から sender SP に HTTP 2nd ack を打つ:

```
POST /api/msgbox/consumed
Body: { "msg_id": "...", "consumed_at": <epoch_ms> }
```

sender 側 handler が `ws.extract` で `consumed_at` 更新。 これで msg は「Consumed」 状態に到達、 VP-161 cross-machine replay で「`forwarded_at && !consumed_at` だけ replay 対象」 が成立。

→ Phase 2 は VP-161 と一体設計（= 本 doc では roadmap として記述、 実装 PR は別 Epic）。

### 決定ε: 起動時 1 回限り migration（任意）

既存滞留 msg（= legacy `forwarded_at` 無しで sender WS に残ってる forward 済 msg）の扱い:

- **保守的**: 起動時 1 回限り、 `forwarded_at == None` && `created_at < now - GRACE_PERIOD_MS`（例: 5 分）の msg を「legacy = `forwarded_at = now()` を後追いマーク」 で再 forward を止める。 TTL 失効で自然消滅
- **アグレッシブ**: 全 legacy msg を `forwarded_at = now()` で即マーク（= 再 forward しない、 ロス覚悟）
- **何もしない**: 既存 msg は今まで通り restart で再 forward される（= 移行期間中の重複配信を許容）

→ **保守的を採用**（= `created_at` の grace period で「これは古い」 を判別、 直近 send は実害ある可能性ある）。 PR-3 で実装、 dogfood で挙動確認。

## Implementation（段階）— PR 分割

「schema 拡張」 → 「sender 側 update」 → 「restore guard」 → 「migration」 → Phase 2 の順。

| PR | 内容 | 状態 |
| -- | -- | -- |
| **PR-pre1** | 設計 doc（本ファイル）のみ。 VP-164 の「設計の受け皿」 | 着手中（本 PR） |
| **PR-1** | (δ) `Message` struct に `forwarded_at: Option<i64>` + `consumed_at: Option<i64>` を追加、 既存 DISC との互換性確保（`serde(default)` + `skip_serializing_if`）、 単体 test（serde round-trip / legacy DISC parse） | 未着手 |
| **PR-2** | (α)(β) `Router::remote_forward_loop` の Ok(()) 経路で `forwarded_at` を sender WS に upsert。 `new_inner` に `whitesnake` 経路を伝播。 失敗時の warn のみ、 retry なし。 test: forward 成功で flag が立つ / 失敗で flag が立たない | 未着手 |
| **PR-3** | (γ) `restore_pending` に `forwarded_at.is_some()` skip guard 追加、 `skipped_forwarded` counter + logging。 (ε) migration: 起動時 1 回限り、 `forwarded_at == None && created_at < now - 5min` の msg を保守的 mark。 test: skip 動作 / migration 動作 | 未着手 |
| **PR-4** | observability: `vp msg list` / `/api/msgbox/list` で `forwarded_at` / `consumed_at` を表示、 `vp msg stats` で lifecycle 別カウント。 sidebar Lane の msg badge に「stuck = forwarded but not consumed for > 1h」 表示（任意） | 未着手（dogfood 後判断）|
| **PR-5（Phase 2）** | (δ) `consumed_at` 経路: receiver の `recv()` 完了で `POST /api/msgbox/consumed` を sender に打つ。 sender 側 handler が `ws.extract` で flag 更新。 VP-161 cross-machine replay の foundation。 → **VP-164 close blocker ではない、 別 Epic（VP-161 or 新規）で着手** | 未着手（Phase 2）|

→ PR-1〜PR-3 で VP-164 dogfood 症状（重複配信）は完全に止まる。 PR-4 は observability の延長、 PR-5 は VP-161 一体設計。

### Phase 2 roadmap（= VP-161 と一体、 本 doc では概要のみ）

- receiver `Handle::recv` で取り出した瞬間に sender へ `POST /api/msgbox/consumed`
- sender 側 `msgbox_consumed_handler` が `ws.extract` で `consumed_at` 更新
- `restore_pending` の policy 拡張: 「`forwarded_at != None && consumed_at == None` && `now - forwarded_at > retry_threshold`」 を retry queue に投入（= VP-161 cross-machine replay の自動経路）
- `vp msg replay <id>` 手動 CLI: forwarded but not consumed な msg を手動で再 forward
- GC 拡張: `consumed_at != None && now - consumed_at > CONSUMED_TTL`（例: 1h）で物理削除、 WS 肥大化抑制

## 残り（任意 follow-up — VP-164 close blocker ではない）

- **`consumed_at` 経路（Phase 2）**: 上記 PR-5、 VP-161 と一体
- **local 配信の `consumed_at` 更新（決定δ-b）**: 全 msg を history log として残す path、 dogfood で要求が出たら採用
- **`vp msg replay` 手動 CLI**: stuck msg の manual recover、 Phase 2 で必要になったら追加
- **WS hierarchical layout**: 現状の `discs/p_{slug}/msg/{id}` flat layout を `discs/p_{slug}/msg/{pending|forwarded|consumed}/{id}` に分けるか（list_by_prefix を効率化）。 必要性は msg 量次第、 dogfood で判断
- **dedupe set fallback**: race window（= forwarded_at 更新中に sender crash）の安全弁として receiver 側 msg_id LRU を追加するか。 Phase 2 consumed_at で構造的に解決するなら不要、 Phase 1 単独運用が長引くなら追加検討

## 将来拡張 — msg lifecycle full observability arc

本設計 Phase 1+2 が landed すれば、 VP の msg layer は以下を達成:

- **Phase 1（本 doc 主スコープ）**: `forwarded_at` で「sender 側 restart の重複配信」 が消える。 dogfood で SP restart が安全に
- **Phase 2（VP-161 一体）**: `consumed_at` で「receiver consume 完了」 を sender が知る → cross-machine replay が「未配信」 を厳密定義できる → LAN node 復帰時の自動 replay
- **Phase 3（Future Vision）**: `vp msg history`（= `~/.vp/<project>/msg-history.json`）で full lifecycle archive、 audit log として機能、 dogfood の bug 再現が trivial に
- **Phase 4（Vision）**: msg replay は cross-machine だけでなく cross-time（= 過去の特定時刻の状態を再現）にも応用可能、 development debugging の foundation

→ Phase 3+ は別 doc / 別 Epic（VP-156 の長期 vision）。 本 doc は Phase 1 確定 + Phase 2 概要まで。

## 設計判断ログ（議論で確定したもの）

| 判断 | 結論 | 理由 |
| -- | -- | -- |
| state 表現の場所 | **`Message` struct の field**（永続層に乗せる）| sender / receiver で同じ semantic を共有、 restart 越えで state が残る、 observability ↑、 VP-161 foundation |
| flag の段数 | **dual（`forwarded_at` + `consumed_at`）** | single flag だと「forwarded だが unconsumed」 が表現できず Phase 2 で schema 変更が再び必要。 dual で forward-compat |
| Phase 1 のスコープ | **`forwarded_at` のみ書き込み**（`consumed_at` は schema 追加のみ、 経路は Phase 2）| VP-164 dogfood 症状（重複配信）は forwarded_at だけで完全に止まる。 consumed_at 経路は VP-161 と一体で別 Epic |
| local 配信の扱い | **既存 remove 維持**（決定δ-a）| Phase 1 では minimal、 既存テスト影響なし。 dogfood で要求が出たら decisionδ-b に upgrade |
| flag 型 | **`Option<i64>` epoch_ms**（`bool` でも `enum` でもなく）| 時刻情報も同時に乗る（= post-mortem で「いつ forward された」 が見える）、 既存 `created_at` / `expires_at` と整合、 serde 表現も簡潔 |
| 既存 DISC との互換性 | **`serde(default)` + `skip_serializing_if`** | wire format 後方互換、 legacy DISC は flag 無しで parse 成功、 新 msg のみ flag が乗る |
| migration | **保守的（grace period 5min）** | 古い legacy msg は forwarded_at マークで再送遮断、 直近 send（grace 内）は念のため再送許容（= 万一の crash recover を残す）|
| 「再投入しない」 と決めた根拠 | **forwarded = receiver box に到達済 = sender restart で再送する理由なし** | receiver 側に既に投函済、 後は receiver の lifecycle（recv → ack）に委ねる。 receiver が crash しても receiver 側の永続層（= local 配信扱い、 receiver の WS）に乗ってるはず → 別問題（receiver 側 msg loss は別 Issue で扱う）|
| restore_pending の policy | **Phase 1 = `forwarded_at != None` skip のみ**、 retry policy は Phase 2 | Phase 1 単独で「sender restart の重複配信」 は完全に止まる、 retry は VP-161 と一体設計 |
| ack-back wire | **既存 HTTP 200 response 維持**、 wire format 拡張は Phase 2 | Phase 1 は status code 判定で十分、 Phase 2 で response body 構造化 + consumed endpoint 追加 |

### 廃案（議論の過程で出たが採らなかったもの）

- **A: ack-back で sender 即削除（削除 path）**: msg loss risk（receiver crash 前に sender 削除）、 observability ✗（履歴消える）、 VP-161 foundation 微妙。 → flag path で同等以上の機能を実現
- **B: A + receiver msg_id dedupe set**: A の補完だが、 「削除 path」 の根本問題（observability 喪失）は解決しない。 → flag path に統合
- **C: A + B + consumed ack（削除 path 完全版）**: scope 大、 msg loss なしだが observability ✗。 → 同等機能を flag path で実現、 観察可能性も得る
- **F-1: single flag `read: bool`**: Phase 2 拡張で schema 変更が再び必要。 → dual flag で forward-compat
- **flag を sender / receiver 双方の永続層に書く**: 二重書き込み、 整合性問題、 split-brain。 → sender 側だけが「forwarded」 を真実とする、 receiver は recv 時に sender へ 2nd ack（Phase 2）で consumed を伝える single source of truth
- **forward 失敗時に `forwarded_at` を仮立て + retry**: 失敗状態を表現する flag が必要、 schema 複雑化。 → 失敗は flag 立てない、 restart 時に restore で再投入（既存挙動維持）
- **flag を `enum State { Pending, Forwarded, Consumed }` で表現**: dual flag と等価だが、 timestamp 情報が消える。 → `Option<i64>` で時刻も同時に乗せる
- **Phase 1 で `consumed_at` 経路も実装**: scope 拡大、 VP-161 と一体設計が望ましい。 → Phase 2 で別 Epic
- **migration なしで legacy msg を放置**: 移行期間中に既存滞留 msg が再送される、 dogfood ノイズが残る。 → 保守的 migration（grace 5min）で抑制
- **migration でアグレッシブに全 legacy mark**: 直近 send の crash recover を捨てる、 risk 大。 → 保守的（grace 5min）|

## 実装時に詰める細部（小）

- `now_ms()` helper: 既存 `created_at` / `expires_at` で使われている `chrono::Utc::now().timestamp_millis()` 系を統一活用
- `Router::new_inner` の signature: `whitesnake` を `remote_forward_loop` にも渡すため、 closure capture or struct 内 Arc を経路追加
- `restore_pending` の counter logging: 既存 `restored` / `expired` / `skipped_foreign` に `skipped_forwarded` を追加、 info log で sum 表示
- migration trigger: `restore_pending` の最後（= 全件 scan 直後）に `migrate_legacy_forwarded` を呼ぶ vs 別 method として分離するか。 1 回限り保証は「最初の restore_pending 呼び出しでのみ実行」 を AtomicBool で
- legacy detection: `forwarded_at == None && created_at < now - 5min` の閾値（`LEGACY_GRACE_MS`）を const で定義、 dogfood で調整可能に
- test fixture: `Message` の builder pattern で flag 操作（既存 helper があれば流用、 無ければ追加）
- serde test: legacy JSON（= forwarded_at 無し）→ deserialize 成功、 flag 付き JSON → serialize 後 minimal（`null` 出力しない）

## Testing

- **ユニット**: `Message` serde round-trip（legacy DISC parse / forward-compat serialize）/ `Router::remote_forward_loop` 成功で `forwarded_at` 更新（in-memory backend で WS state 確認）/ `restore_pending` で `forwarded_at != None` skip 動作（= 復元数が減る、 skip counter が立つ）/ `migrate_legacy_forwarded` の grace period 動作（grace 内 / grace 外で挙動分岐）
- **統合**: send → forward 成功 → SP restart → restore で再 forward が起きない（= receiver 側に重複到達しない）/ send → forward 失敗（HTTP error 等で receiver 不在 simulator）→ flag 立たない → restart で再投入される（= retry が機能）/ legacy DISC（flag 無し）+ grace 外で起動 → migration で mark + 再 forward 抑制 / legacy DISC + grace 内 → 通常通り再 forward
- **dogfood**: VP-163 で再現させた `msg_send to=agent@creo-memories` シナリオを N 回 restart → receiver 側で重複なし / `vp msg list`（PR-4 で）で msg lifecycle が見える / 過去送信した msg が起動時に re-flood しないことを確認

## 影響 / Migration

- **`Message` schema 拡張**: `forwarded_at` / `consumed_at` 2 field 追加、 既存 DISC との互換性は `serde(default)` で確保
- **既存滞留 msg**: migration（決定ε）で grace 外を後追い mark、 grace 内は再送許容（= dogfood で挙動確認後、 grace 閾値を tune）
- **WS namespace 変更なし**: 既存 `discs/p_{slug}/msg/{id}` をそのまま使用、 layout 変更は将来 hierarchical 化（任意 follow-up）
- **wire format 変更なし**: receiver の `{"status": "delivered"}` response 維持、 Phase 2 で `consumed` endpoint 追加時に明文化
- **docs/CLAUDE.md 影響**: 「msgbox の挙動」 セクションがあれば flag lifecycle を明記、 既存 dogfood feedback memory に「forwarded_at の挙動」 を追記
- **下流影響**: VP-161（cross-machine replay）の前提が整う、 VP-156 epic の「永続化 first-class」 が `Message` lifecycle 観点でも完成形に近づく
