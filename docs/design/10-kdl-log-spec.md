# 10: KDL log emission spec v0

> **Status**: draft (stage 1 of `mem_1CaaLAtsYRhWgpPhnrvaVd` execution snapshot)
> **Extends**: architectural canon `mem_1CaSiJkD9HATDY2srrv6D4` (VP Observability Stack 設計決定, 2026-04-27)
> **Implements**: Phase B precondition — Phase B (`KdlFormatter` を `vantage-core` に昇格) が安全に実装できるよう、 emission の正規 format を**先に**確定する役割
> **Out of scope**: viewer (TUI / Canvas) の UI 仕様、 SurrealDB sink の schema (両方とも architectural canon 側で確定済 / 未着手)

---

## 0. このドキュメントの位置づけ

VP の observability stack は 3 層構造である (architectural canon § アーキテクチャ概観):

```
Layer 1 source  → Layer 2 aggregator (DB)  → Layer 3 viewer (tail / TUI / Canvas / AI)
```

本 spec は **Layer 1 (emission) の format 規定** のみを扱う。 つまり「どの source が、 どんな bytes を file (および opt-in で SurrealDB) に流すか」を確定する。

決めること:
- 3 emission source の出力 format を **真の KDL** として round-trip 可能な形に統一
- 全 log entry が共有する mandatory field と、 source 固有 / event 固有の optional field の境界
- `tracing::span!` 階層を KDL nested node でどう表現するか
- 既存出力からの移行戦略 (旧 plain tracing 互換、 grep script 救済期間)

決めないこと:
- viewer 側の filter UX / keybind / column layout (stage 3 で別 spec)
- DB sink の `DEFINE TABLE` / index 設計 (architectural canon § schema で既に確定、 Phase A.5 実装側で詳細化)
- 他プロジェクト (creo, fleetflow) との format 互換 — VP 内部の SoT を確立してから議論する

本 spec は **stage 1 deliverable** であり、 確定後すぐに stage 2 (`KdlFormatter` の `vantage-core` 昇格 + daemon 配線) が実装する。

---

## 1. Goal / Non-goal

### Goal

1. 3 emission source (**daemon** / **vp-app** / **project SP**) の log を **真の KDL** で round-trip 可能 — `kdl = "6.5"` の parser に通せる、 つまり `KdlDocument::parse(line) → emit → 同一` が成立する 1-line node に揃える
2. **tail (live) を主用途** とした field 抽出可能性 — `id` / `ts` / `level` / `target` / `msg` を必須化して、 ストリーム上で grep / awk / `jq` 風の field 抽出ができる
3. **post-mortem を v0.2+ で後付け可能** な余地を残す — `id` を ULID にして時系列 sortable + 範囲 query 可能にしておく (v0.2 で mmap + index を後付ける時の前提)
4. 既存 grep script を **deprecation 期間つき** で救済 — `--legacy-format` flag を付け、 段階移行できる

### Non-goal

- viewer 側 UI (stage 3 で別 spec)
- Canvas WebView 統合 (stage 4、 完全 deferred)
- SurrealDB schema 設計 (canonical memory 側で確定済)
- 多言語 SDK (現状 Rust crate のみ)
- 構造化 log の binary format (CBOR / Protobuf 等)
- DB live tail の format (DB sink は `serde::Serialize` した同一 record を SurrealDB に書く前提、 file emission の wire format とは別問題)

---

## 2. Source 一覧 (3 sources)

| component | emission site (現状) | 現状の format | v0 移行後 |
|-----------|---------------------|--------------|-----------|
| **daemon** (`vp daemon` / `vp world`) | `crates/vantage-point/src/cli.rs:398-436` (`init_tracing`) | `tracing_subscriber::fmt()` default — `2026-04-30T... INFO target Registry: SP '...' 登録` | `KdlFormatter` (本 spec) を `vantage-core` 経由で適用 |
| **vp-app** (Mac GUI) | `crates/vp-app/src/log_format.rs:38-81` (`KdlFormatter`) + `crates/vp-app/src/app.rs:2159` で layer 設定 | KDL **風** 1-line — `info ts="..." target="..." key=val "msg"` (kdl-rs 未検証) | 同 `KdlFormatter` を `vantage-core` 経由で reuse、 round-trip 検証あり |
| **project SP** (`vp start` のサーバ本体) | `crates/vantage-point/src/cli.rs:346` (`init_tracing` を共有、 別 process として起動) | daemon と同じく `tracing_subscriber::fmt()` default | 同 `KdlFormatter` を適用 |

> 現状、 daemon と project SP は同じ `init_tracing` を呼ぶが、 ファイル名は分かれる: daemon = `~/Library/Logs/Vantage/daemon.kdl.log`、 project SP = `~/Library/Logs/Vantage/project-{slug}.kdl.log` (architectural canon § log file 命名)。 中身が plain tracing なのに `.kdl.log` を名乗っているのは misleading で、 これも本 spec が解消する。

実装後はすべての source が `vantage_core::log::KdlFormatter` を通る。 `source` field (mandatory § 4) でどの source 由来かを log entry 側に持たせる。

---

## 3. Canonical output shape

### 3.0 全体規約

- **1 log = 1 line = 1 KDL top-level node**
- 第一トークン (= node name) = log level (`error` / `warn` / `info` / `debug` / `trace`)
- それ以降は KDL property (`key=value`) の連続、 値は string / number / bool / null
- 末尾の **positional argument 1 個** = `msg` (必須)
- 改行は entry 区切り専用、 msg 内の `\n` は `\n` literal で escape する (§ 9 参照)
- string property の value は常に `"..."` で quote する (bare ident 風には書かない、 round-trip の安定性のため)

### 3.1 info event (typical)

```kdl
info id="01J6N9G7TX0V7Z6N7Q3M9D6FA1" ts="2026-05-01T07:42:11.234Z" source="daemon" target="vantage_point::registry" event="sp.register" lane="vantage-point/lead" pid=44211 "SP registered via QUIC"
```

property 順序は **mandatory → optional → source-specific** の意味的順 (§ 4 / § 5)、 ただし parser は順序を保持する義務を持たないので grep の便宜上の慣習に過ぎない。

### 3.2 error event (with structured fields & escapes)

```kdl
error id="01J6N9G7VC1Y7K6N7Q3M9D6FA2" ts="2026-05-01T07:42:11.512Z" source="vp-app" target="vp_app::client" event="theworld.fetch.fail" err="connection refused: \"127.0.0.1:32000\"" retry=3 "TheWorld fetch 失敗"
```

- 引用符を含む value は `\"` で escape (KDL の string literal escape rule に従う)
- 数値 / bool は **裸**で書く: `retry=3`、 `recovered=false`
- positional `msg` は最後の 1 個のみ

### 3.3 multi-line content

長い stack trace や error chain は msg ではなく field 化する。 msg は **1 行 summary** に保ち、 詳細は `chain` field に escape 込みで埋める:

```kdl
error id="01J6N9G7VC1Y7K6N7Q3M9D6FA3" ts="2026-05-01T07:42:11.700Z" source="daemon" target="vantage_point::pty" event="pty.spawn.fail" err="ENOENT" chain="caused by: spawn(\"claude\")\ncaused by: not found in PATH" "PTY spawn 失敗"
```

emission 側で改行を `\n` literal にエスケープする責務を負う。 viewer / parser 側は復元するときに `\n` を実改行に戻す。

### 3.4 span open / close

`tracing::span!` の階層は **論理的には** KDL nested node (children block) にマップしたいが、 1 log = 1 line 制約と矛盾する。 そこで v0 では **flatten + breadcrumb 方式** を採用する:

- span enter / exit は専用の `event` 名で 1-line emit する
- ネストした event は `span` field に **`/` 区切りの breadcrumb** を持つ
- ULID `id` で因果順を保証する (時刻同一でも ULID monotonic 保証で順序が確定)

```kdl
debug id="01J6N9GB10..." ts="2026-05-01T07:42:12.000Z" source="daemon" target="vantage_point::registry" event="span.enter" span="reconcile" "span enter"
debug id="01J6N9GB11..." ts="2026-05-01T07:42:12.001Z" source="daemon" target="vantage_point::registry" event="span.enter" span="reconcile/scan_ports" port_range="33000-33010" "span enter"
info  id="01J6N9GB12..." ts="2026-05-01T07:42:12.040Z" source="daemon" target="vantage_point::registry" event="port.scan.found" span="reconcile/scan_ports" port=33001 lane="creo/lead" "Discovered SP"
debug id="01J6N9GB13..." ts="2026-05-01T07:42:12.080Z" source="daemon" target="vantage_point::registry" event="span.exit" span="reconcile/scan_ports" elapsed_ms=80 "span exit"
debug id="01J6N9GB14..." ts="2026-05-01T07:42:12.090Z" source="daemon" target="vantage_point::registry" event="span.exit" span="reconcile" elapsed_ms=90 "span exit"
```

> 真の nested KDL (children block) は v0.2 以降で「コレクションされた batch」として post-mortem 用 export format に検討する。 emission の wire format は v0 では flat に保つ。

---

## 4. Mandatory fields

すべての log entry が必ず保持する field。 viewer / DB sink / future post-mortem index はこれらの存在を仮定してよい。

| field | 型 | 例 | 説明 / 役割 |
|-------|----|---|------------|
| `<level>` | KDL node name (= 第一トークン) | `info` | tracing level、 `error` / `warn` / `info` / `debug` / `trace` の 5 値 |
| `id` | KDL string (ULID 26 chars) | `"01J6N9G7TX0V7Z6N7Q3M9D6FA1"` | record id、 時系列 sortable。 v0.2+ の post-mortem index の primary key 候補 |
| `ts` | KDL string (RFC 3339 + `Z` + ms) | `"2026-05-01T07:42:11.234Z"` | UTC fixed、 ms precision、 末尾 `Z` 必須 (timezone offset 表記は禁止) |
| `source` | KDL string | `"daemon"` / `"vp-app"` / `"project-sp"` | emission source identifier、 § 2 の 3 値 + 将来追加に拡張可 |
| `target` | KDL string (Rust module path) | `"vantage_point::registry"` | tracing target、 module path に揃える (`with_target(true)`) |
| `msg` | KDL positional argument (string、 末尾 1 個) | `"SP registered via QUIC"` | 人間向け 1 行 summary、 改行は `\n` literal |

### なぜ全 6 つを mandatory にするか

- `level` / `target` / `msg`: tracing の最小表現で、 grep の出発点。 これがないと level filter / target tree / 検索が成り立たない。
- `ts`: 時系列 viewer の前提、 RFC 3339 fixed UTC (Z) で実装間ばらつきを排除。 `+09:00` のような offset 表記は禁止 (mixed timezone 表示で混乱するため)。
- `source`: 3 source を 1 つの tail に重ねた時に区別する唯一の手がかり。 file 名が分かれているので冗長に見えるが、 DB sink (architectural canon § schema) や mixed tail で必須。
- `id`: ULID にすることで「v0.2 で post-mortem index を後付けする」を成立させる前提条件。 ts が ms 同値でも ULID は monotonic 採番されるので、 同 ms 内の因果順が保たれる。 v0.1 は単純な tail でも `id` を必ず emit しておかないと、 後から DB / index 入れるときに `id` 採番が source ごとにバラつくと再設計が必要になる。

> ULID 生成は emission 直前 (formatter 内) で `ulid::Ulid::new()` を呼ぶ。 tracing 側 hook で span の生成時に取れない事情があるため。 monotonic 性は同一プロセス内のみ保証されればよい (cross-process は ts の ms 比較で十分)。

---

## 5. Optional / extensible fields

mandatory 以外で **慣習的に使う** field を予約する。 これらは「あれば viewer / DB sink がより賢く扱う」もので、 emission 側で **必要時のみ付ける**。

### 5.1 共通 optional

| field | 型 | 例 | 用途 |
|-------|----|---|------|
| `event` | string (dotted) | `"sp.register"` / `"osc99.received"` / `"pty.spawn.fail"` | event 種別 tag、 viewer の event filter / DB index の対象 |
| `lane` | string (Lane address) | `"vantage-point/lead"` / `"creo/w1"` | Lane-as-Process 規約 (VP-77) の address |
| `pane_id` | string | `"%8"` / `"vantage-point/lead"` | tmux pane id または lane label (feedback `pane_id_readability` に従い label 推奨) |
| `pid` | i64 | `44211` | OS process id、 cross-process 相関用 |
| `span` | string (`/` 区切り) | `"reconcile/scan_ports"` | span breadcrumb (§ 3.4) |
| `elapsed_ms` | i64 | `80` | span exit / 計測 event 用 |
| `err` | string | `"connection refused"` | error message (msg は summary、 err は raw) |

### 5.2 source-specific (flatten)

source-specific な field は **prefix なしで flatten** する (例: OSC 99 の `i` / `d` / `p` / `a` / `u`)。 名前衝突は同一 source 内で起きないので prefix 不要。 cross-source で衝突する可能性のある名前 (`port` 等) は **`event` で文脈を明示**することで区別する。

```kdl
debug id="..." ts="..." source="vp-app" target="vp_app::terminal::osc" event="osc99.received" lane="vantage-point/lead" i="bell" d="claude awaiting input" p=33000 a="vantage-point" u="claude" "OSC 99 metadata received"
```

### 5.3 拡張ルール

- **unknown property は infrastructure 側で ignore、 viewer は best-effort 表示**: 新規 source / 新規 event が増えても spec 改定なしで追加できる。
- 予約名は § 5.1 の表のみ。 これと衝突する semantic で別の意味に使うのは禁止。
- 衝突回避のためにどうしても prefix が要る場面 (例: 同じ event で 2 つの port を持つ) は **`event` を分ける** か **値側で区別** (例: `local_port` / `remote_port`) する。 source prefix (`osc99__`) のような汚染は禁止。
- 将来、 ある field が「広く使われるようになった」と判明したら、 v0.x で § 5.1 に格上げする (breaking ではなく additive)。

---

## 6. Span hierarchy mapping

§ 3.4 の方針を再掲・確定する:

| tracing 概念 | KDL 上の表現 |
|-------------|-------------|
| `span!(Level::DEBUG, "reconcile")` 入場 | `event="span.enter"` + `span="reconcile"` の 1-line entry |
| `span!(...)` 退場 | `event="span.exit"` + `span="reconcile"` + `elapsed_ms=...` |
| span 内の event | 通常の event entry に `span="reconcile"` field を付与 |
| nested span (子 span 内) | `span="parent/child"` の `/` 区切り breadcrumb |
| span の `record()` (動的 field 追加) | enter 時に未確定なら、 後続 event 側で field を flatten。 v0 では「span field を後から記録」を専用 event 化しない |

実装メモ (stage 2 以降):
- tracing-subscriber の `Layer` 実装で `on_enter` / `on_exit` を hook する
- span breadcrumb は `Extensions` に `Vec<&'static str>` を保持
- `elapsed_ms` は `on_enter` で `Instant::now()` を Extension に積み、 `on_exit` で diff を取る

---

## 7. Versioning

### 7.1 spec_version の埋め込み

各 log file の **先頭 1 行** に meta node を必ず emit する:

```kdl
meta spec_version="v0" started_at="2026-05-01T07:42:00.000Z" source="daemon" pid=44211 hostname="mako-mbp"
```

- `meta` は予約 node 名 (level enum と衝突しない)
- viewer / parser は最初の `meta` node を読んで spec を分岐する
- file rotation (§ 10 open question) で新ファイルができるたびに先頭に再 emit する
- 既存 file への append 再開時 (process restart) は **新規 meta を 1 行 emit してから** event を続ける (parser は途中の meta を見たら spec を切り替えてよい、 file 全体で単一とは仮定しない)

### 7.2 version 番号運用

- `v0` = 本 spec (= 初版)
- field の **追加** は minor (例: `v0.1`)、 spec_version は `"v0"` のまま (additive change)
- field の **削除 / 意味変更 / 必須化** は major (`v1`)、 spec_version を bump
- viewer / DB sink は `spec_version` が未知の場合 best-effort で読む (mandatory field の存在のみ仮定)

---

## 8. Migration policy

### 8.1 deprecation flag

stage 2 で `KdlFormatter` を daemon に配線する PR は、 同時に **`--legacy-format`** flag を CLI / env で提供する:

| 切替方法 | 値 / 動作 |
|---------|----------|
| ENV `VP_LOG_FORMAT=legacy` | 旧 `tracing_subscriber::fmt()` default に fallback |
| ENV `VP_LOG_FORMAT=kdl` (default) | 本 spec の KDL formatter |
| CLI `vp daemon --legacy-format` | 同上 (env を override) |

### 8.2 deprecation timeline (proposal)

| 時期 | 状態 |
|------|------|
| 2026-Q2 (v0 release) | KDL default、 `--legacy-format` で opt-out 可 |
| 2026-Q3 | `--legacy-format` 使用時に warn ログ出力 (毎起動 1 回) |
| 2026-Q4 | `--legacy-format` 削除、 KDL fixed |

> 期限と quarter は方針として stage 1 で決める。 実際の release 整合は VP の version マイルストーンに合わせて stage 2 PR で再確認する。

### 8.3 grep script 移行ガイド

#### 旧 format (例: 既存の運用 grep)

```sh
# 旧: 行頭の ISO timestamp + space + LEVEL を仮定
grep -E '^[0-9-]+T[0-9:.]+Z\s+ERROR' ~/Library/Logs/Vantage/daemon.kdl.log
```

#### 新 format での等価

```sh
# 新: 行頭が level node 名、 ts は property
grep -E '^error ' ~/Library/Logs/Vantage/daemon.kdl.log

# 時間範囲: ts="..." は ISO 8601 なので lexicographic compare できる
awk '/^error / && / ts="2026-05-01T07:/' ~/Library/Logs/Vantage/daemon.kdl.log
```

#### 構造化 query 例

```sh
# event="sp.register" を抽出
grep -E '^[a-z]+ .*event="sp\.register"' ~/Library/Logs/Vantage/daemon.kdl.log

# lane で絞る
grep -E ' lane="vantage-point/lead"' ~/Library/Logs/Vantage/*.kdl.log
```

> 既存運用 script は `.claude/` 内 / mise tasks に散在する想定。 stage 2 PR の checklist で「既知の script を新 format に書き直す」を含める。

---

## 9. kdl-rs (kdl = "6.5") compatibility

### 9.1 round-trip 要件

stage 2 の実装には **round-trip test を必須**とする:

- 各 example (本 doc § 3 の全 listing) を `kdl::KdlDocument::parse()` に通して、 `parse → emit → parse` が同一 AST になることを assert
- test fixture は本 doc から自動生成可能にする (markdown の ```kdl``` block をそのまま input にする pattern)

### 9.2 escape rule (formatter 側責務)

`crates/vp-app/src/log_format.rs:156-174` の既存 `kdl_string` 関数が正しい方向性なので、 vantage-core 昇格時に再利用する:

| 入力文字 | 出力 |
|---------|------|
| `"` | `\"` |
| `\` | `\\` |
| `\n` | `\n` (literal 2 文字) |
| `\r` | `\r` |
| `\t` | `\t` |
| その他 `c < 0x20` | `\u{HH}` (hex) |
| 通常文字 (UTF-8 multi-byte 含む) | そのまま |

### 9.3 streaming parse の制約

- viewer は **1 line ずつ** parse する (`KdlDocument::parse` を行単位で呼ぶ)
- 1 行内に node 1 つ、 children block を持たない、 という前提を § 3 で固めたので line-oriented parse が成立する
- 行末の `\n` は KDL parser に渡す前に trim する (kdl-rs は trailing newline を許容するが、 strict parse のため明示 trim 推奨)
- 巨大 msg (多 MB) は emission 側で truncate (例: 64 KiB cap、 unison frame size 上限と揃える) — 詳細は stage 2 で決める

### 9.4 数値・bool・null の扱い

- 数値: `port=33000` (i64) / `cpu=0.42` (f64)、 quote しない
- bool: `recovered=true`、 quote しない
- null: 原則 emit しない (field が存在しないなら property を出さない)

---

## 10. Open question (stage 2 で決める)

stage 1 (本 spec) で**確定しない**が、 stage 2 着手前にチームで方針を決める必要がある事項:

1. **Source-specific field を nested node 化するか、 flatten のままか**
   - v0 = flatten で進めるが、 OSC handler の field が増えると 1 行が長くなる懸念。 nested node 化は spec_version v1 候補
2. **Span ID の採番方針**
   - 現状 `span` は string breadcrumb、 ULID 等の span id を別 field で持つかは未決。 tracing-subscriber の `Id` を再利用するか、 独自に採番するか
3. **Rotation policy**
   - size-based (例: 64 MiB で rotate) / time-based (daily) / 両方。 既存の `tracing-appender` rolling を使うが、 spec として記録するかは要検討
4. **Truncate の境界**
   - msg / err / chain field の最大長。 unison frame 64 KiB と揃えるか、 もっと厳しくするか
5. **Multi-process ULID 衝突**
   - 同 ms に複数 process が ULID を採番した場合の cross-process tie-break。 現状は ts の ms 比較に依存、 sub-ms 衝突時の挙動を v0.2 post-mortem index 設計時に確認する
6. **DB sink との同期**
   - file → DB の bridge (Phase A.5) が KDL を再 parse するか、 emission 直前で 2 経路に分岐するか。 後者 (`Layer` を 2 つ stack) が architectural canon の方針 (dual-sink)、 stage 2 で SurrealLogLayer を書く時に確定
7. **vp-app の app.rs:2159 の既存 layer 構成との整合**
   - 現状 vp-app は `tracing_subscriber::fmt::layer()` を独自に組み合わせている。 vantage-core 昇格後の API shape が `Subscriber` にするか `Layer` にするかで、 vp-app 側の繋ぎ替えが変わる

---

## 11. 実装後の確認 checklist (stage 2 受け入れ基準)

stage 2 PR が本 spec に準拠しているかを判定する acceptance criteria:

- [ ] `vantage-core::log::KdlFormatter` が公開され、 daemon / vp-app / project SP の 3 source で同一 type を使う
- [ ] 全 mandatory field (`level` / `id` / `ts` / `source` / `target` / `msg`) が常に emit される
- [ ] `id` は ULID 26 文字、 `ts` は RFC 3339 + Z + ms
- [ ] file 先頭に `meta spec_version="v0" ...` node が emit される
- [ ] `kdl::KdlDocument::parse()` が 100 件 sample log で全件成功する round-trip test がある
- [ ] `--legacy-format` / `VP_LOG_FORMAT=legacy` で旧 tracing default に fallback できる
- [ ] § 3 の全 example が round-trip test fixture に含まれる
- [ ] § 8.3 の grep migration ガイドが実 file で動作する (整合性確認)

---

## 12. 関連

- **architectural canon**: creo memory `mem_1CaSiJkD9HATDY2srrv6D4` (2026-04-27 「VP Observability Stack 設計決定」) — 本 doc の上位 spec
- **execution snapshot**: creo memory `mem_1CaaLAtsYRhWgpPhnrvaVd` (2026-05-01) — stage 0-4 の実行計画、 本 doc は stage 1 の deliverable
- **既存 KdlFormatter**: `crates/vp-app/src/log_format.rs:38-81` — 出発点となる KDL 風 formatter
- **daemon tracing init**: `crates/vantage-point/src/cli.rs:346-437` — stage 2 で formatter 差し替えする箇所
- **PR #233** (`mako/osc-debug-keys`): OSC 99 metadata structured key=value debug log — 本 viewer 需要の発火源、 § 5.2 の OSC example はここから
- **VP-77 Lane-as-Process 規約**: `lane` field 値の address 体系 (`{project}/{role}`)
- **kdl skill**: KDL syntax / kdl-rs の操作方法 (本 spec の前提知識)
- **architectural Phase B**: `KdlFormatter` の `vantage-core` 昇格 — 本 spec 確定後すぐに実装する次工程
