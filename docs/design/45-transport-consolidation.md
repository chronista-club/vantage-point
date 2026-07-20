# doc 45 — control plane transport の Unison 統一（HTTP route の棚卸し）

> **status**: 方針確定・未着手（2026-07-21）。doc 44 P1（fold-in）の dogfood 中に
> 「HTTP と Unison が二重化したままでは」という mako の指摘で顕在化した。
> **doc 44 とは独立**（fold-in の後始末ではなく transport 層の設計判断）。

## 0. 一言で

**World の control plane を Unison(QUIC) に寄せ、HTTP は `/api/health` と `/api/shutdown`
の 2 本だけ残す。** 28 route → 2 route。

## 1. なぜ寄せるか

transport が 2 つあること自体より、**HTTP 側には Unison 側にある足場が無い**ことが問題。

| | Unison | HTTP |
|---|---|---|
| schema | `crates/vantage-point/schema/vp-daemon.kdl`（typed request / returns） | 無し（手書き axum handler + ad-hoc JSON） |
| drift 検出 | あり（`tests/vp_daemon_kdl.rs`、source 突き合わせ） | **無し** |
| MCP tool 化 | unison-mcp が自動合成 | されない |

VP は AI ネイティブ開発環境なので、**Unison に乗せた面はそのまま agent が触れる面になる**。
これは副次効果ではなく本筋の利得。

### 実害（2026-07-21 時点）

- **processes 一覧を取る経路が 3 つ**ある: Unison `registry.list` / Unison
  `world-process.list` / HTTP `GET /api/world/processes`。同じ情報に 3 実装。
- doc 44 PR3 の `vp ps` 実装で、当初この HTTP 版を選んで 4 つ目の依存を足しかけた
  （commit `f1dea10` で Unison に書き直した）。面が 1 つなら起こらない間違い。

## 2. なぜ 0 本ではなく 2 本残すか

`/api/health` と `/api/shutdown` は **VP 外に消費者がいる**:

- `.mise/tasks/app/swap`（Ruby）: 「`/api/health` の 200 を単一の真実源にする」
- `apple/VantagePointAgent/Sources/InstanceScanner.swift`（Swift menu bar agent）

これらを Unison に寄せると Ruby / Swift に Unison client を持たせる必要が出る。さらに
**health は「他が壊れている時に動いてほしい」probe** で、Unison 層が wedge した時に
health も Unison だと診断手段ごと失う。**意図的に鈍い外殻**として HTTP を残すのは
統一の失敗ではなく設計。`/api/shutdown` も同じ（緊急停止は最も単純な経路であるべき）。

## 3. route の行き先

| route 群 | 本数 | 行き先 | 備考 |
|---|---|---|---|
| `/api/world/projects*` | 7 | **Unison `world-control`** | projects CRUD。CLI は既に world-control、vp-app は HTTP |
| `/api/world/processes*`（start/stop/restart/pointview） | 4 | **Unison** | lifecycle。`projects/start\|stop` は doc 44 で移設済 |
| `/api/world/lanes*` | 2 | **Unison** | `lanes/list` は `f1dea10` で world-control に新設済 |
| `/api/canvas/*` | 2 | **Unison** | layout / switch_lane |
| `/api/world/{port_for,refresh}` | 2 | **Unison** or 撤去 | port_for は slot API（doc 44 で意味論変化）、要精査 |
| `/api/update/*` | 7 | **Unison**（優先度低） | self-update。churn が低いので後回しでよい |
| `/api/health` `/api/shutdown` | 2 | **HTTP 維持** | §2 |

## 4. 波及の大きさ

- **vp-app の REST client 12 method が丸ごと消える**（`crates/vp-app/src/client.rs`）。
  vp-app は既に Unison を 36 箇所で使っているので、二重 transport を抱える理由が消える。
- CLI の `commands/config.rs` / `commands/daemon.rs` / `discovery.rs` の HTTP 呼び出しを
  Unison client に差し替え。
- `WorldControlClient` に不足 method を追加（既存の projects/* と同じパターン）。

## 5. 進め方（案）

fold-in（doc 44）が nightly に落ち着いてから着手。route 群ごとに小 PR に割る:

1. `world-control` に不足 RPC を出す（server + client + KDL + drift テスト）
2. CLI の HTTP 呼び出しを Unison に差し替え（`vp ps` は `f1dea10` で完了済み）
3. vp-app の `client.rs` を Unison に差し替え（`app.rs` の既存 Unison 経路と統合）
4. HTTP route を撤去（`/api/health` `/api/shutdown` を除く）
5. `apple/` の InstanceScanner は既に機能停止（SP-portless 以降 port scan が常に空）
   なので、health 単発 probe だけ残して port scan は撤去（UI 判断とセット）

各段階で「HTTP を消す前に Unison 経路が実機で動く」ことを確認してから旧経路を落とす
（doc 44 で確立した「新面が動く → 旧面撤去」の順序）。
