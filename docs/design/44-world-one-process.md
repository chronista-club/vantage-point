> ⚠️ **旧命名の歴史文書**: 本 doc は 2026-07-27 の命名エピック以前の語彙（JoJo 愛称 ほか）で書かれている。現行の対応は CLAUDE.md「アーキテクチャ命名体系」参照。

# doc 44 — World 一枚化と Project Host（SP の転生・slot 語彙・conductor の再定義）

> **status**: 方向確定 + **P1 実装設計確定**（2026-07-20）。P0 語彙 ✅出荷（#820）、
> P1 は露払い着手。§1–§4 = dogfood 議論の凍結、**§5 = 実コード調査に基づく P1 実装設計**。
> **発端**: v0.52.1 hotfix（#817）で戦った reverse-route 取りこぼし・demand hook レースが
> **World↔SP のプロセス分割が生む分散システム問題**だと確定したこと。および「Act I でタブが
> 出ない」「床が分かりにくい」という dogfood 指摘の掘り下げが、SP の存在理由まで届いた。
> mako × conductor（Fable 5 / Opus 4.8）の対話で 5 決定に収束。

## 0. 一言で

**World を唯一の常駐プロセスにし（SP fold-in）、project は認知境界（住所）に退化させる。**
SP の名は「project の状態と進行に責任を持つ決定的な執事 = Project Host」として転生し、
conductor は「開発起点」を指すポインタに再定義される。lane から役割の状態機械が消える。

## 1. 決定事項（5 件）

### D1. 語彙: 「床」→「slot」

- コード識別子（`PtySlot` / `ChatEngineSlot`）が既に持つ **slot** を公用語に昇格。
  doc 語彙をコードに寄せる（memory「Naming: Stand vs code」の層分け方針に整合）。
- 旧「床に落ちる」=「**Act I slot は engine が抜けると shell 層が露出する**」と構造で言う。
  Act II slot が空くと「💤 休眠」（#817 で導入済みの語彙）— 両 Act が対称に語れる。
- 「床」は識別子に不在（コメント 155 + docs 78 の散文のみ）。置換は機械的、リネーム波及なし。

### D2. SP の廃止 — World fold-in・フラット化

- **判断根拠**: console の deploy 生存性は `--resume` + disk replay（v0.37/38）で十分
  （mako 判断 2026-07-20。「プロセスは死ぬがコンテキストは蘇る」が tmux decoupling 以降実証済み）。
  プロセス分離が買っていた実益（PTY master fd の daemon 跨ぎ生存）は、この時点で税に見合わない。
- **形は「フラット化」**（ProjectHost actor 案は棄却）: World が LanePool 一枚で slot を直接抱き、
  key は既存の `LaneAddress`（project/lane）。**project は registry エントリ + cwd + grouping +
  address prefix のみ** — 実行時実体としての project は消滅する。
  「project という認知境界は欲しいが、それは runtime container なしで抽出できる」（mako）。
- **消えるもの**: World↔SP 配管 ≈7,600 行（uplink 自己登録 / control reverse-route /
  demand hook の QUIC 往復 / snapshot reconcile — #817 のバグクラスの発生源ごと）、
  `vp sp start|stop`、SP port slot（33000–33024、portless 化で論理 ID だけだったものが完全退役）、
  spine 三段 → **二段（daemon / window）**。
- GUI は既に :32000 のみを見る（portless 化の遺産）ため、surface 側はほぼ無傷。
- 表示層: Star Platinum ⭐ は project ビューの顔として stands.rs に残せる
  （The World と Star Platinum は原作でも同型スタンド — 吸収は lore 整合）。

### D3. Project Host = 決定的 actor + 3 層エスカレーション

旧 SP は "Project Host" を名乗るプロセス宿主で、project そのものを host していなかった。
新 Project Host は **project の状態・進行・交通整理に一人称の責任を持つ決定的サービス**。

**振る舞いインベントリ**（家の主人のメタファー）:

| 振る舞い | 具体 | 現状の担い手 |
|---|---|---|
| 迎え入れ | lane 作成時の base 保証（fetch 済み nightly から切る） | conductor の注意 + 規約 |
| 場の維持 | nightly 前進の通知 / 同一ファイル複数 lane 編集の衝突警告 | **不在**（mtime 事故の根因） |
| 交通整理 | commit / PR / merge の順序調停、ship gate | conductor + 運用規約 |
| 見送り | merge 後の残骸掃除（branch / worktree / lane state） | 手作業 |
| 帳簿 | flow_progress / handoff の SSOT、release までの距離 | creo memory + conductor の記憶 |

**アーキテクチャ原則: Host は決定論。LLM にしない。**
conductor の既知事故 2 件（wire ack 遅延の二重配送 / conductor session 固定化）は
「帳簿と受付を LLM セッションに置いた」ことの帰結であり、LLM Host は同じ問題を再生産する。

3 層モデル:
1. **決定的判定**（Host 内・純関数 = calculations、テスト可能）: merged? 衝突? base 古い?
2. **LLM への発注**（Host が呼ぶ stateless one-shot、既存 OneShot `ClaudeAgent` が道具）:
   進捗サマリ文章化・brief 下書き等の言語タスク
3. **人間へのエスカレーション**（帳簿に積んで注視中の session / GUI へ）: 事実だけで決まらない案件

Host は**推測しない**。2026-07-20 の lane 掃除（19 本中 18 本を機械判定、1 本のみ人間へ）が
この分業の実演であり、その判定基準（PR MERGED? 独自 commit 0? リモート有無?）が
第一号 calculations の仕様草稿になる。

**第一の振る舞い = 「見送り」（merged 残骸の自動掃除）**: 判定基準が言語化済み・失敗の被害が
小さい・毎週確実に価値が出る。「場の維持」（衝突警告）は lane 並列運用の再開後に第二号。

### D4. conductor = 開発起点（Host が持つポインタ）

- conductor は残る（mako: 「開発起点として欲しい」）が、**lane の自意識から project の指定へ**:
  Host の帳簿が「開発起点はこの lane」というポインタを 1 本持つ。lane 自身は役割状態を持たない。
- conductor の機械業務（帳簿 / ack / gate / 掃除）は **Host へ漸次移管** — 移管 1 件ごとに
  conductor が軽くなり、最後に残る「議論・判断・発火・人間の定位置」が新定義になる。
- lane の振る舞い分岐は消滅（wire の受付は Host なので全 lane 対等）。
  `LaneAddress` の `conductor` / `performer/…` は `project/lane-name` にフラット化する
  （起点 lane の予約名 or ポインタで表現 — 詳細は実装時）。
- オーケストラ比喩の修正でもある: 現実の指揮者は舞台進行や譜面管理をしない（それは事務局 =
  Host の仕事）。指揮者は解釈とキュー出し = **意図の供給**。

### D5. session タブ = 注視の切替のみ。起点再指定は sidebar

- タブ strip（現 ChatView 内の「仮置き UI」）を **header 層（両 Act 共通）に昇格**。
  クリック = その session に注視を移すだけ（surface は今の Act のまま）。
  Act I slot の root 切替は既存 root picker の担当のまま。
- **開発起点の再指定はタブに載せない**: タブは lane 内（全 session 同一 cwd）、起点指定は
  lane 間（cwd が違う）— レベルが違う。起点再指定は **sidebar の lane メニュー**
  （「この lane を開発起点に ⭐」= Host のポインタ更新のみ。何も動かず cwd も変わらない）。
- cwd 拘束のある操作（release cut は main checkout 等）は**起点ではなく操作に付く制約**で、
  D3 の Host が正しい場所で実行する。ポインタがどこを向いていても混ざらない。

## 2. 実装順序の素案（各 Phase = 独立に出荷可能）

> pre-MVP 方針: 中間状態を残さず、旧経路は即撤去。ただし fold-in は巨大なので PR は分割する。

- **P0（語彙・随時）**: 床 → slot の散文置換（コメント 155 + docs 78、機械的）。
  D1 だけで独立に出せる。
- **P1（本丸）**: SP fold-in。World が LanePool を in-process 所有し、SP プロセス spawn を廃止。
  uplink / control / canvas-ingest channel → 関数呼び出し・in-process channel に置換、
  reverse-route / refire / snapshot reconcile を撤去。`vp sp` 退役。
- **P2**: `LaneAddress` フラット化 + conductor ポインタ導入（D4 の構造部分）。
- **P3**: Project Host 第一の振る舞い「見送り」+ 帳簿の最小形（D3）。
  以降、conductor の機械業務を 1 つずつ Host へ（それぞれ小 PR）。
- **P4**: タブ header 昇格 + sidebar 起点指定 UI（D5）。
  ※ P1 完了後は demand 往復が消えて純 UI 問題になるため、P1 の後に置く。

## 3. 北極星 — コードの大生産工場と工場グラフ（mako、2026-07-20）

> VP は開発エディタだけど、私的には CreoApps 含めて、**コードの大生産工場かつ工場グラフ**だから、
> そこへ向けて、今後も進んでいくと思う。

本 doc の決定はこの北極星の言葉で読み直せる（mapping は 2026-07-20 に一往復して確定）:
**project = 工場、Project Host = 生産管理（工場長ではなく管理板 — 決定論）、
lane / slot = 生産ライン、conductor = 起点に立つ人間の意図**。
そして「**工場グラフ**」= 工場（project）をノード、供給線をエッジとするグラフ。
2026-07-19 の creo-ui #86 → npm publish → VP #819 はまさに工場間サプライチェーンの実演で、
そこで踏んだ「npm の最新 ≠ repo HEAD」は供給線の同期問題だった。Host の帳簿・交通整理は、
いずれ**工場を跨ぐ版**（供給元が進んだら下流に bump を知らせる等）へ拡張される — その配管は
hub federation 層が既に持っている。生産管理板（Host）を各工場に先に立てておくことは、
工場グラフを流れる情報（進行・供給・衝突）の読み書き口を先に作っておくことでもある。

## 4. 開いている問い（触って見える「次の景色」の候補）

> §4 の前 4 項は **2026-07-20 の実コード調査で決着**した（§5 参照）。残りは P3 以降の問い。

- ~~P1 の panic 封じ込め~~ → §5.1 で決着（**現状の隔壁は幻**、P1 のブロッカーではない）
- ~~DB handle~~ → §5.2 で決着（単一 handle + project 列、要検証 1 点）
- ~~`vp ps` の意味論~~ → §5.3 で決着
- ~~デバッグモードの新しい家~~ → §5.4 で決着（**現状すでに到達不能**だった）
- hub federation は World レベルなので原理的に無関係 — 実装時に要確認のみ。
- Host の帳簿の永続化先（surrealkv）と、creo-memories との棲み分け。

## 5. P1 実装設計（2026-07-20 の実コード調査で確定）

### 5.0 調査で判った 3 つの構造的事実

1. **World と SP は既に同じ `AppState` 型を共有**している（`process/state.rs`）。mode 差は
   フィールドを `Some`/`None` で出し分けているだけ。fold-in は「2 つのプログラムの合体」
   ではなく **既に 1 つの型にある分岐を畳む**作業。
   フィールド内訳 = per-project 14 / global 12 / dead 4（dead は P1 露払いで削除済）。
2. **縫い目は 2 行**。World から SP へ入る経路は `daemon/server.rs:1466`（canvas 上り）と
   `:1589`（process-proxy）に収束し、SP 側は `dispatch_process_method` の単一 `match`
   （60 method）で受ける。ここを直接呼び出しに差し替えると、**7,600 行が「書き換え対象」
   ではなく「孤児」になる**。
3. **`LaneAddress { project, kind, name }` が既に project を key に含む**ため、N 個の
   LanePool を 1 枚に merge してもキー衝突が起きない。D2 の「World が LanePool 一枚で抱く」は
   既存データ構造が既にその形をしている。

### 5.1 panic 封じ込め — 現状の隔壁は幻

実測: `catch_unwind` 0 件 / panic hook 0 件 / `panic = "abort"` 未設定 /
`tokio::spawn` 100 箇所超に対し `JoinError::is_panic()` の観測 **0**。

| 障害の型 | SP プロセス境界は守るか |
|---|---|
| tokio task の panic | **守らない**（unwind で task だけが黙って死ぬ。SP の有無と無関係） |
| `PtySlot` の std Mutex poisoning | **守らない**（Err 化されて lane 単位の恒久故障） |
| deadlock | ✅ 守る |
| OOM / stack overflow / abort | ✅ 守る |

つまり fold-in の実質的な後退は **deadlock と資源枯渇の 2 つだけ**。前者に対しては
`LanePool` が既に「`read()` のまま mutate して長い await 中に write lock を握らない」
規律で設計されている（`submit_chat` / `deliver_nudge` 等）。

**結論**: P1 に `catch_unwind` 足場は作らない。代わりに順序を逆にして、
**隔壁を外す前に「何が落ちているか」を見えるようにする**（panic hook = `src/panic_hook.rs`、
P1 露払いで実装済）。これは fold-in の有無に関わらず価値がある。

### 5.2 DB handle — 単一 `db/world/` + project 列

> **実装済（PR4、2026-07-21）**。以下は設計時の記述で、末尾に実装結果を追記した。

現状 namespace は `vp`/`vp` **固定**で、分離は**ディレクトリ**（`db/world/` と `db/sp_{slug}/`）。
World が N handle を抱く案は「project の runtime 実体」を復活させるので D2 と矛盾する。
`LaneAddress` が project を持つ以上、**table に project 次元を足す**のが canonical。

- ✅ **検証済（2026-07-20 実測）: `db/sp_*` は捨ててよい**。合計 1.4 GB あるが、
  各 db は `wal/` 単一ファイルが全量で `sstables`/`vlog` は 0 バイト（**一度も compaction
  されていない**）。WAL の中身は 3 db で調べて **`!nd`（SurrealDB の node = cluster
  membership key）が 1 件 127 B ちょうどで占有率 ~100%**。実アプリデータは
  `stand_status` 1,055 / `pane_contents` 56 / `wire_messages` 50 件と桁違いに少なく、
  移行すべき実体は KB オーダーしかない。詳細 = creo `mem_1CdDAJuuCfsP1iY2ZutHp9`。
- **fold-in の未計上の実利**: db 24 個 → 1 個で node 書き込みストリームも 24 → 1 になり、
  この churn が **24 分の 1** に落ちる（月 ~1.4 GB → ~60 MB）。ただし compaction が
  走らない限り成長自体は続くので**緩和であって根治ではない**（別途 surrealkv の
  compaction 設定を調べる — P1 の範囲外）。
- **孤児 db 246 MB**（projects.kdl に対応 project が無い 9 個。`sp_creoui` = creo-ui
  リネームの残骸等）は誰も掃除していない。`vp sync` は ghost project を消すが db は残す。
  判定基準が明快なので **D3「Project Host の第一の振る舞い＝見送り」の実例第 2 号**。
- 副次: この LOCK は「重複 SP 検出」も兼ねていた（生存 holder 検出で起動中止）。
  fold-in 後は World の `:32000` bind + `daemon.pid` が単一性を保証するので**代替不要**。

#### 実装結果（PR4）

想定より小さく終わった。理由は **schema が最初から `project_path` 列を持っていた**こと
（旧「SP 固有テーブル」= `pane_contents` / `stand_status` / `prompts` も全て所有し、クエリも
全て `WHERE project_path = $path` で絞っていた）。1 DB = 1 project の時代は事実上冗長だった列が、
そのまま canonical な project 次元になった。**schema 変更・データ移行ともに不要**で、
変更は「handle を 1 本に寄せる」だけになった。

| 変更 | 内容 |
|---|---|
| `start_project` | per-project connect を撤去し、World が開いた handle を引数で受ける |
| `ProjectRuntimes` | `world_db` field を持ち、`for_world()`（旧 `with_lane_view`）で lane view と同時に結線 |
| `db_data_dir_for_project` | 撤去（呼び出し元は `start_project` の 1 箇所のみだった） |
| `DbLockHeldByLiveHolder` | 撤去。「LOCK 保持 = 重複 SP」の判定は `ProjectRuntimes` の map 二重 insert 防止が引き継ぐ |
| `resolve::project_slug` / `fnv1a_64` | 撤去。**slug の用途は `db/sp_{slug}/` の命名だけだった**ため production 呼び出し元が 0 になった |

**旧 `db/sp_*` は削除も移行もしていない**（合計 1.4 GB がその場に残る）。判定基準が明快な
掃除対象なので D3「Project Host の第一の振る舞い＝見送り」の担当に送る（§5.2 の孤児 db 246 MB と同じ扱い）。
実害として、旧 db に入っていた **PP board（`pane_contents`）は引き継がれない**（実測 56 件）。

### 5.3 `vp ps` — PORT / PID 列が無意味化

`PROJECT / LANES(数) / STATUS(active|idle) / ← cwd` へ。detail は既存の `vp lane` が持つ。

### 5.4 debug mode — 撤去で決着（2026-07-21）

当初は「runtime toggle にして初めて使えるようにする」方針だったが、**実装時に消費側も
消えていたことが判明**したため撤去に切り替えた。

到達不能の実測（生産側）:
- World が spawn する SP に **`-d` は渡されない**（fold-in で `vp sp` ごと退役、#824）
- `debug_mode` は fold-in 後 常に `None`（`ProjectRuntimes::start` が None 固定）
- `send_debug` は `if None return` で必ず早期 return、`send_debug_detail` は呼び出し元ゼロ
- `DebugModeChanged` は生産者ゼロ

**消費側も無い**（これが方針転換の決め手）:
- `DebugInfo` を表示していた **WebUI デバッグパネルは旧 localhost browser UI ごと撤去済**
- native vp-app / Swift agent とも `process/debug/*` を購読していない

runtime toggle を作っても出力先が無いため、`DebugMode`（debug パネル用途）/ `DebugInfo` /
`DebugModeChanged` / `DebugModeArg` / `send_debug` / `send_debug_detail` / `TraceLog` +
`watch_and_broadcast` を撤去した。

**残したもの**: `VANTAGE_DEBUG=none|simple|detail` による **tracing レベル選択**は生きた別機構
（`cli::parse_debug_env` → `init_tracing`、log verbosity）。`DebugMode` enum はこの用途だけ
cli.rs にローカル化して温存。ファイルベースの trace log（`init_log_file` / `write_trace`）も
温存（broadcast bridge の `watch_and_broadcast` だけ孤児として撤去）。

### 5.5 PR 分割

| PR | 内容 | 規模 | リスク |
|---|---|---|---|
| **1. 露払い** | panic 可視化 + **P1 が抱えて運ぶ羽目になる死コード**の除去（`AppState` の dead field 3 本 / 孤児化した `process/pty.rs`） | 小 | ほぼ 0（挙動不変） |
| **2. fold-in 本体** | `AppState` の per-project 化 → World が LanePool 所有 → dispatch 直結 → SP spawn 停止 → uplink/registry/control 撤去 | 大（〜2,000 行） | 中 |
| **3. 遺物撤去** | `vp sp` / port slot API / `PORT_RANGE` / health monitor / presence 意味論 / `vp ps` / `restart-all` / debug の新居 | 中 | 小 |
| **4. DB 統合** ✅ | `db/sp_*` → `db/world/` の project 列化 | 小（schema は既に project 列を持っていた） | 小（§5.2 実装結果） |

**線引き**: PR1 は「P1 が抱えて運ぶもの」だけを落とす。「P1 が丸ごと消すもの」
（`vp sp` の内部死コード等）は磨かない — 捨てる作業になるため。

PR2 は分割したくなるが、「World と SP のどちらが LanePool を持つか」は**半分だけ出荷できない**
性質なので、中間状態を作らない方針（pre-MVP）に従って一息に切る。

### 5.6 検証戦略 — dev profile で完全並列

`VP_PROFILE=dev`（World :32100 / `~/.local/share/vp-dev/`）で fold-in 版 daemon を立てれば、
**release daemon(:32000) の lane を一切落とさずに実機確認できる**。#643 の namespace 分離が
そのまま P1 の検証装置になる。これにより P1 最大の運用リスク（「daemon 再起動 = 全 lane 死」）が
dogfood 中は発生しない。

## 6. P2 実装設計 — `LaneAddress` フラット化（2026-07-21）

> §2 の P2 のうち**構造部分（フラット化）を実装した**。conductor ポインタ導入（Host の帳簿）は
> P3 と一体なので分離した。

### 6.1 何をしたか

`LaneAddress { project, kind: LaneKind, name: Option<String> }` → **`{ project, name: String }`**。

旧構造は **conductor だけ `name: None`** という非対称を抱えており、これが「lane が役割を自意識する」
構造の物理形だった（D4）。フラット化でこの非対称が消え、開発起点は**予約名**
`CONDUCTOR_LANE_NAME = "conductor"` を持つ lane、という関係に退化した。

| 変更 | 内容 |
|---|---|
| `LaneKind` | 撤去（server / vp-app の両方） |
| `LaneAddress` | `{ project, name }` の 2-tuple。`::conductor()` / `::performer()` は**構築 API として維持**（呼び出し 100 箇所超が無傷） |
| `LaneInfo.kind` / `.name` | 撤去 — どちらも `address` が持つ情報の複製で、真実源が 2 つあった。vp-app 側の `LaneInfo.name` は**常に None** で実質死んでいた |
| Display / `key()` | `<project>/<name>` の 1 形。旧 `Wire::key()` と `LaneAddress::Display` の微妙な挙動差（unknown kind の扱い）も消滅 |
| webview | `isPerformerLane()` は `address.name !== "conductor"` の名前判定に |

### 6.2 予約名を `"conductor"` にした理由

**永続 address を無傷で引き継ぐため。** 開発起点の Display 形が `<project>/conductor` のまま変わらず、
DB / session.json / wire に残る conductor の address がそのまま一致する。
（変わるのは performer 側 `<project>/performer/<name>` → `<project>/<name>` だけ。）

D4 の「lane 自身は役割状態を持たない」に対しては、**型から役割分岐が消えた**ことで達成と見なす。
名前に "conductor" が残るのは D4 が「conductor は残る（開発起点として欲しい）」と言っているのと整合する。

### 6.3 永続データの手当て — 2 系統ある

address は **2 つの形**で永続しており、両方に手当てが要った（片方だけだと静かに壊れる）。

| 形 | 在処 | 手当て |
|---|---|---|
| **object** | `lane.descriptor`（`LaneInfo` を丸ごと FLEXIBLE 保存） | `LaneAddress::name` に `#[serde(default)]` = 予約名。旧 conductor は name 欠落 → 既定値、旧 performer は `name: "foo"` がそのまま読める。余分な `kind` は unknown field として無視 → **custom Deserialize 不要** |
| **文字列 key** | `lane.address` / `lane_lifecycle.address` 列 | `define_schema()` が起動時に旧形を新形へ UPDATE（冪等、best-effort）。放置すると upsert（DELETE+CREATE）の WHERE が当たらず**重複行**、lifecycle は照合できず**孤児**になる |

文字列 key 側は**テストが落ちて初めて気づいた**（reconcile が ready→dead、db の件数が 2→3）。
serde default で descriptor が読めるようになったので「互換は済んだ」と錯覚しやすいが、
**object と文字列 key は別の経路**で、前者の手当ては後者を救わない。

migration は**行単位で失敗を閉じ込める**（1 行の UPDATE 失敗＝典型は旧形と新形が同一 lane を
指す UNIQUE 衝突 — で関数を抜けると、同じ SELECT に載った残り全行が巻き添えで未処理になり、
衝突源が在る限り再起動のたびに同じ巻き添えが起き続ける）。

### 6.4 取り残しやすいのは「型を経由しない文字列」

フラット化の取り残しは **`LaneAddress` 型を使っている箇所**では起きない（コンパイラが止める）。
危ないのは **address を文字列で直に組み立てている箇所**で、実際に 1 件やらかした:

`delivery_actor::wire_agent_to_lane_display` は `format!("{}/performer/{}", …)` で lane address を
組み立て、その結果を `pick_nudge_target` が `LaneAddress::to_string()` と**生の完全一致**で照合する
（間に `parse_address` を挟まない唯一の経路）。旧形のまま取り残された結果、**performer 宛の
wire nudge が恒久的に「lane 不在」となり永久リトライに落ちる**回帰になった。

見つけにくい条件が 3 つ重なっていた:
- **conductor は形が変わらない**ので無症状（開発起点だけ使っていると気づかない）
- 2 つの関数を**個別には**テストしていたが、`pick_nudge_target` の fixture が conductor 固定で、
  **両者を performer で繋ぐテストが無かった** → `test --workspace` は緑のまま
- 他の全経路（`resolve_lane_address` / `handle_lane_nudge` / `handle_lane_delete` 等）は
  `parse_address` を通るので旧形を吸収してしまい、**この 1 箇所だけが地雷化**した

対処は `LaneAddress::new(project, name).to_string()` を経由する形に変えて、直書きをやめた
（Display 形が将来また変わっても自動追随する）。**「文字列直書きは無傷なのではなく、
受信側の正規化に依存して無傷」**という区別が要る — 依存が無い経路が事故る。

この 1 件をきっかけに同型を洗ったところ、**さらに 3 群**見つかった（全て型を経由しない箇所）:

| 箇所 | 症状 |
|---|---|
| `app.rs::lane_key_to_wire_agent` | `wire_agent_to_lane_display` の**逆写像**。3 分節前提の `split_once` で新形が常に `None` → performer の wire inbox が GUI から開けない。**対の関数なので片方だけ直すと非対称に壊れる**（往復テストを追加） |
| `mcp/lane.rs` の 3 箇所 | `lane.get("kind")` / `lane.get("name")` を JSON 直読み。撤去した field なので全て None に落ち、lane 名が `"unknown"` / `"unnamed"` になる（`list_lanes` / `flow_progress` の表示と wire address が壊れる） |
| `CreateLaneReq.kind` | wire 入力 field として残存。「lane に種別は無い」と矛盾する残骸だったので撤去し、validation は**予約名の拒否**に置換（旧 `kind != "performer"` の意図の後継） |

**教訓**: 型を消しても「型を経由しない参照」は残る。フラット化のような改修では
`grep` の対象を型名だけでなく **field 名・文字列リテラル・JSON key** まで広げる必要がある。

なお MCP の 2 件は「取り残し」ではなく**既に発生していた実害**だった（P2 本体 commit の時点で
`list_lanes` の kind フィルタが常に空配列を返し、`flow_progress` は全 lane が `"unnamed"`、
performer の agent address が `agent@<project>/` という壊れた文字列になっていた）。
**独立した 2 回のレビューでも初回では捕まらなかった** — JSON 直読みはそれだけ視界に入りにくい。

### 6.5 lane 作成の経路が 2 本ある（P2 で顕在化、doc 44 の外）

予約名ガードの実機確認で判った構造。lane 作成には**別実装の経路が 2 本**ある:

| 経路 | 実装 | 予約名の扱い |
|---|---|---|
| unison `lane_create` | `routes/lanes.rs::create_performer_orchestrated` | P2 で追加した予約名ガードが効く |
| `POST /api/world/lanes` | `capability::ProcessManagerCapability::create_lane` | **到達しない**。奥の `lane::config::validate_performer_name`（VP-166 の既存ガード）が clone 段階で結果的に弾く |

同じ validation が経路によって効いたり効かなかったりする — P1 が消した「経路ごとの差」の親戚で、
P3 以降で lane 作成を Host に寄せる際の統合対象。

> ⚠️ **follow-up（doc 44 とは独立の pre-existing バグ疑い）**: `ProcessManagerCapability::create_lane` は
> ①`db.upsert_lane`（DELETE+CREATE）→ ②clone で `validate_performer_name` が reject → ③rollback が
> `db.delete_lane` という順で走る。`name="conductor"` を投げると **①で本物の conductor descriptor 行を
> 上書きし、③で消す**経路が静的に読み取れる（= 失敗したはずの操作がデータを壊す）。VP-166 起源で
> P2 の変更対象外のため本 PR では触らない。再現手順は使い捨て project に
> `POST /api/world/lanes {"name":"conductor"}` を直接投げ、conductor descriptor の生存を確認する。

### ✅ 着地: 実装を 1 本にする（2026-07-22）

§9（名前 gate の一本化）で **入口の validation** は揃えたが、実装自体は 2 本のままだった（§9.4）。
ここでそれを畳んだ。**残したのは `routes/lanes.rs::create_performer_orchestrated`**、
`ProcessManagerCapability::create_lane` は「名前 gate → project runtime を引く → core を呼ぶ」
だけの adapter になった。

**どちらに寄せたか、の根拠**:

| 判断材料 | 内容 |
|---|---|
| 分かれていた理由が消えていた | doc 24 §5.3「ground を provision する唯一の主体は daemon」が World 側実装の根拠だったが、**P1 fold-in で daemon = project = 同一プロセス**になった時点で成立しない（両者とも同じ `new_performer_in` を呼んでいた） |
| 片方は単独で完結しない | World 側は descriptor + worktree だけ作り、PtySlot は lane watcher の loopback（= 結局 core 側）に賭けていた。**「A から spawn を抜いて watcher 経由の遠回りを足したもの」**が World 側だった |
| 利用者の数 | core 側 = MCP `add_performer` / `flow_handoff` / CLI `vp flow handoff` / lane watcher。World 側 = vp-app の「+ Add Performer」のみ（`daemon::client::lanes_create` は呼び手ゼロ） |
| 機能の包含関係 | `base` / `model` / explicit `cwd` / stand 永続（`stand_store`）/ `SystemEvent::Lane(Diff::Add)` は core 側だけが持っていた（= World 経由では効かなかった） |

**消える側にしか無かった振る舞いと、その扱い**:

| 振る舞い | 扱い |
|---|---|
| **intent-first bracket**（`upsert_lane` + `lane_lifecycle=Provisioning` → 成功で `Ready` / 失敗で削除、doc 24 §4.6） | **core へ移設**。旧構成ではこの永続が GUI 経由の create にしか効かず、MCP / CLI / watcher で作った lane は **descriptor が一度も db に載らなかった**（= boot reconcile の射程外）。移設で全入口に効く |
| lane watcher が `lane_registry` の descriptor から **stand を引き直す** hop（bug mem_1Cd4M7i5Enp3HHMLVYayRe の対処） | **撤去**。GUI create が自分で spawn するようになり、その lane が watcher に届く時は必ず "already exists" で弾かれる = 引き継ぐ相手が居ない。stand は create request が直接運ぶ |
| `lane_registry` への即時 push | **落とした（等価な経路がある）**。fold-in 後 `lane_registry` は project の `publish_lanes` が LanePool snapshot で毎回全置換するので、core の `Diff::Add` → publish で ms 単位で収束する（手で push しても次の publish に上書きされるだけ） |
| 停止中 project でも worktree を作れた | **明示 error に変更**。旧構成では descriptor と worktree だけ残って PtySlot が付かない（= watcher が動いていない）ため、実質「作ったのに動かない lane」だった。vp-app 側も `showAddPerformer = isActiveProject() && !isPaused()` で稼働中限定にしており、UI の契約とも一致する |

**副産物（書き手を増やした対の後始末）**: `delete_lane_orchestrated` が db の descriptor /
lifecycle を回収するようにした。旧構成にもこの非対称（create が書いて delete が消さない）は
在ったが、書き手が GUI 経由だけだったため表に出にくかった。全入口が書くようにした以上、
消し手が無いと削除済 lane の行が溜まって次の boot で ghost になる。

**残った尻尾**:

- `db::replace_lanes_for_project` は呼び手ゼロ（SP register snapshot 反映用の遺物、fold-in で
  producer が消えた）。`daemon::client::lanes_create` も呼び手ゼロ（doc 45 段 1 で足された
  client wrapper）。どちらも本統合とは独立の pre-existing で、触っていない
- lane を **live にする**実装はもう 1 本ある（`lane_spawn_actor` = boot bootstrap の respawn）。
  ただしそれは「作る」ではなく「起こす」で、worktree provision も descriptor 永続もしない。
  今回の統合対象（= 作成の動詞）とは別軸
- **成功系の end-to-end test は書けない**（`create_performer_orchestrated` の成功 = user の
  login shell を実際に起こす）。bracket の enter/exit は helper 直呼びで、書いた行の回収は
  spawn を経ない `delete_lane_orchestrated` 経由で固定した

### 6.6 残した振る舞い分岐

`stand_spawner::claude_command` の「開発起点なら `--continue`、それ以外は fresh」は**挙動不変で残した**
（`LaneKind` 判定 → `is_conductor()` 判定に置換）。cwd の性質差に根拠がある実在の差で、
repo root なら `--continue` が自分の会話に当たるが、worktree で同じことをすると他 lane の
セッションを掴む（dashboard 罠）。P3 で Host がポインタを持てば「起点 lane か？」の問い合わせになる。

構造変更（P2）と挙動変更を同じ PR に混ぜると回帰の切り分けが効かなくなるため、分けた。

## 7. P3 実装設計 — Project Host の第一の振る舞い「見送り」（2026-07-21）

> **第一スライス実装済**。判定 → 表示 → 実行を縦に 1 本通した。帳簿の永続化と
> conductor ポインタは後続スライス。

### 7.1 Host が実体として現れた

`crates/vantage-point/src/host/` を新設。D3 の「project の面倒を見る決定的な執事」が
初めてコード上の家を持った。第一の住人は [`host::farewell`]（見送り）。

### 7.2 3 層の物理化

D3 の 3 層モデルを、CLAUDE.md の data / calculations / actions 分離にそのまま対応させた:

| 層 | 実装 | 性質 |
|---|---|---|
| data | `LaneFacts` | 判定に要る事実だけ。git も DB も知らない |
| **層 1: 決定的判定** | `judge_farewell` | **純関数**。I/O ゼロで全分岐をテスト可能 |
| actions | `collect_facts` | git subprocess で事実を集めるだけ。判定しない |
| 集約 | `survey_project` | project の全 lane を判定して並べる（実行はしない） |
| **層 3: 人間へ** | `FarewellVerdict::AskHuman` | 事実だけで決まらないものを積む |

層 2（LLM への発注）は**無い**。見送りは決定的に決まるため — D3 の「Host は決定論。
LLM にしない」がこの構成に出ている。

### 7.3 旧実装が抱えていた欠陥（Host に移して解けたもの）

`lane::commands::classify_performer_for_cleanup`（撤去）は収集・判定・分類が 1 関数に
混ざっており:

1. **テスト不能** — `run_git_in(fetch)` を内部で呼ぶため、判定だけを試せない
2. **判定が 2 値**（削除 / 保持）— 「事実だけで決まらない」を表現できない
3. **`merged` なら未コミット変更を見ずに削除候補**へ入れていた
   （= 取り込み済み branch 上に残った作業を黙って捨てうる。**Host は推測しない**の違反）

3 が実質。Host 版は **dirty を merged より先に見て `AskHuman` に回す**。
実機検証（fast-forward merge した dirty な lane）で、旧なら「削除可能」だったものが
「⚠️ 要判断」になり、`--force` でも残ることを確認した。

### 7.4 実機検証が炙り出した「区別できない 2 状態」

`MergedState::NotMerged` は 2 つの状態を畳んでいる:

- lane を作ったばかりで何もしていない（fresh）
- lane の作業が **fast-forward merge** で取り込まれ、既定 branch が追いついた

どちらも `HEAD == origin/<default>` になり、`is_branch_merged` は「fresh は消さない」
安全弁として ancestry 判定より前に `false` を返す。**ローカル git の ancestry 情報だけでは
区別できない**。

ただし全く区別不能なわけではない: `is_branch_squash_merged` が `gh pr list --head <branch>
--state merged` で PR メタデータを引くため、**gh が使えて同名 branch の merged PR が実在すれば**
fast-forward merge も `merged` 側に倒る（`HEAD == headRefOid` なので ancestor 判定が自明に真）。
つまり本当に区別できないのは **gh 不在 / 未認証 / PR を経ていない repo** の場合で、
実機検証がその条件（GitHub 連携のない使い捨て repo）に当たっていた。
日常の `mako/{slug}` → PR フローではこの曖昧さの多くは解消される。

初版はこれを知らずに `MergedState` をそのまま理由文に出しており、実機で
**merge 済みの lane に「取り込み状態: 未 merge」と表示**されて露見した。判定自体は
正しかったが理由が嘘だった。`integration_label` で「既定 branch と同位置（未着手 or
fast-forward で取り込み済）」と**断定できる範囲だけ**を言うように直した
（facts-over-narrative）。

> 判定は正しくても**添える事実が嘘**ということが起きる。Host は人間の判断材料を作る
> 立場なので、理由文の精度は判定の精度と同格に扱う。

### 7.5 後続スライス

- ~~**帳簿の永続化**~~ → 下の「✅ 着地: 見送りの帳簿」。予告は「いつ何を見送ったか」と
  「`AskHuman` の滞留」の 2 つで、**両方とも永続が要る**と確定した（§8.5 の初版は滞留を
  「都度計算で復元できる」として持たない判断だったが、これは誤り — 下記）
- **conductor ポインタ**（D4 の残り）— 帳簿ができた時点でその最初のエントリとして載せる
- ~~**稼働中 lane の保護**~~ → 下の「✅ 着地」。予告は「daemon 経由の surface（LanePool を
  持つ層）から呼べば埋まる」だったが、**CLI から World に訊く**形に落ちた（`vp lane cleanup`
  自体が surface なので、供給のために別 surface を待つ必要はなかった）
- ~~**lane 作成の 2 経路統合**（§6.5）と、そこに紐づく descriptor 破壊疑い~~ →
  descriptor 破壊は §9 で解消（実在を確認して回帰テストで固定）、**実装の統合は
  §6.5「✅ 着地: 実装を 1 本にする」で完了**（2026-07-22）。予告どおり「どちらが正か」を
  決めるだけで済んだ（core = `create_performer_orchestrated` に寄せ、World 側は adapter 化）

### ✅ 着地: 稼働中 lane の保護（2026-07-21）

`judge_farewell` の稼働 guard（`running_lane_is_kept`）は**一度も発火していなかった**。
判定は最初から正しく、`LaneFacts.is_running` も持っていたが、CLI 経路
（`vp lane cleanup`）が `survey_project(&repo_root, &[], &origin)` と**常に空配列**を
渡しており、事実が届いていなかった。純関数テストは緑のまま、実運用でだけ稼働中 lane が
「削除可能」に出る形（§7.3 の `branch -d` never-fire と同じ穴の作り方）。

**供給経路**: World の "world-process" channel に `list_all_lanes` を ask し、
`host::liveness::running_lanes_in`（純関数）で **この project の分**だけを取り出して渡す。

決めたことと理由:

| 決めたこと | 理由 |
|---|---|
| process-proxy (`lanes_list`) ではなく **cross-project の `list_all_lanes`** | process-proxy は対象 project が World に登録されていないと逆引き失敗 = error になり、「project が動いていない（稼働 0）」と「World に訊けなかった（不明）」が区別できない。cross-project 一覧なら前者は**答え**として返る |
| 稼働の判定は `state`（`running` / `exiting`）で見る。**pid は見ない** | Act II（chat）の lane は `pid: None` + `state: running` が正常形。pid で見ると chat lane を停止中と誤認して見送る |
| `spawning` は稼働に**数えない** | `build_lanes_snapshot` が disk 上の intended performer を `Spawning(pid=null)` で merge する（#683）。稼働に数えると World が上がっている限り全 lane が保護され、見送りが恒久的に無効化される（守り過ぎも無音の失敗） |
| lane 名は `address.name` から読む | `LaneInfo.kind` は P2 で消えた。`kind` を条件にすると全 lane が落ちる（既存 `parse_world_lanes` はこの形のまま残っており、ROTO の cross-project 一覧は空になっているはず — 別件） |

#### 不明は「無い」に畳まない

World に訊けなかった場合、**削除を実行せず「稼働状況を確認できないため見送りを保留」と
告げて抜ける**（判定にも進まない）。`Liveness::{Known, Unknown}` の 2 値で型に固定し、
`Unknown` からは稼働 lane 名を**取り出せない**ようにした（`lanes_for_survey()` が `None`）。

- 畳むと「**daemon が落ちている時だけ**稼働中 lane が削除可能に見える」= 一番危ない条件で
  guard が消える。今回のバグ（`&[]` 直書き）と同じ機序を型で塞ぐ
- 判定だけ出して「削除可能」と表示するのも嘘になりうるので、survey ごと止める
- **`--force` でも通さない**。`--force` は「判定結果を実行する」意思表示であって
  「事実が無くてよい」ではない（1 flag に 2 仕事を兼ねさせない、§8.1 の同型）
- 起点照会（§8.4）は保留判定の**後**に置く。保留するなら World に 2 度目の ask をしない

> §8.3 の 3 値フォールバックと同じ規律。Host は人の判断材料を作る立場なので、
> **区別できるものを畳まない**。ただし今回は「告げて続行」ではなく「告げて止める」—
> 起点は間違えても保持側に倒れるが、稼働は間違えると**動いている lane を消す**ため。

#### テストが固定していること

- `farewell::survey_keeps_running_lane` — `running` 引数が判定まで届き、**同じ lane で
  供給の有無だけを変えると verdict と理由が変わる**（純関数テストだけでは供給の穴を検出できない）
- `liveness` の純関数群 — 稼働状態の線引き（`spawning` / `dead` を含めない）、
  project 越境の除外、壊れた応答での非 panic、`Unknown` が空リストに化けないこと
- `lane::commands::cleanup_holds_when_liveness_is_unknown` — `Unknown` + `--force` でも
  `Held` で 1 件も消さず、**起点照会にも進まない**。同時に `Known(空)` では判定へ進むことも
  見て「常に保留（= 見送り機能が死んだ状態）」が緑にならないようにする
- **git primitive の置き場所** — 第一スライスでは `lane::commands` の git 関数 5 本を
  `pub(crate)` に上げて Host から直接使った（`run_git_in` / `count_changes` /
  `is_branch_merged` / `is_branch_squash_merged` / `get_branch`）。依存の向きとしては
  Host → lane の plumbing で逆転している。今は 1 箇所なので実害は無いが、
  **後続の振る舞い（迎え入れ / 場の維持 / 交通整理）がそれぞれ独立に同じ手を伸ばす**と
  肥大化する。2 つ目が伸びた時点で共有 git primitive module への切り出しを検討する

### ✅ 着地: 見送りの帳簿（2026-07-22）

`host_farewell` table を新設し、見送りの記録を永続化した。読み手は **board UI ではなく
`vp lane cleanup` の出力**（+ `vp lane history`）。

#### 最初の読者を board UI にしなかった

§8.5 は「見送りの履歴」の消費者を board UI と書いて先送りしていた。そのまま作ると
**UI フェーズ（doc 47 §7）まで読み手がゼロ** = §8.2 で自分が挙げた「読み手のない書き込み」
そのものになる（`LaneId` が 2 年間そうだった）。

`vp lane cleanup` の要判断行に滞留を添える形にすると、**書いた翌回の実行で読まれる**。
UI を待たずに書き手と読み手が同じ PR で閉じるので、書き込みが本当に要るかが即座に検証される。

```
  ⚠️ 要判断: feat-x (未コミットの変更が 4 件ある（…）) — 3 回連続、初回 2026-07-15
```

`vp lane history` は「いつ何を見送ったか」の読み手（`Reclaimed` を消費する側）。
これを足さないと、履歴だけが読み手ゼロのまま残る — 記録の種類ごとに読み手を用意する。

#### 何を記録するか — 「計算で復元できない事実」だけ、を 2 種に絞った

| 判定 | 帳簿 | なぜ |
|---|---|---|
| 実行した見送り（`Reclaimed`） | **書く** | lane を消したので survey では二度と再現できない |
| `AskHuman` の滞留（`Pending`） | **書く** | 「今 `AskHuman` か」は都度計算できるが、**「いつから」「何回目」は観測の履歴**で計算では出ない |
| `Keep` / 判定だけの `Reclaim` | **書かない** | 次の survey で同じ答えが出る（= 復元できる）。書いても行が増えるだけ |

> §8.5 の初版は滞留を「lane が残るので復元可能」として**持たない**判断だった。これは
> 誤りで、復元できるのは**現在の verdict** であって滞留の長さではない。今回訂正した。

#### 連続は 1 行に畳む（streak + first_seen_at）

`vp lane cleanup` は何度でも走る。観測ごとに行を足すと**放置された lane ほど帳簿を太らせる**
（滞留を追うための表が滞留で壊れる）。同じ判定の連続を 1 行に畳むと、行数は
**「判定が変わった回数」に比例し、実行回数には比例しない**。

| 状態遷移 | 書き込み |
|---|---|
| 記録対象外 → 記録対象外 | **0 write**（安定した lane は survey を素通り = 多数派） |
| なし → `AskHuman` | 1 CREATE（streak=1、first_seen_at=now） |
| `AskHuman` → `AskHuman` | 1 UPDATE（streak+1、last_seen_at=now。**名前と初回時刻は不変**） |
| `AskHuman` → それ以外 | 1 UPDATE（`ongoing=false` で閉じる。行は履歴として残す） |

閉じるのが要点 — 閉じないと、後でまた `AskHuman` になった時に**連続していない観測が
1 本の滞留に見える**（「3 週間放置」が実は「1 回 → 解決 → 2 週後にまた 1 回」だったことになる）。

⚠️ `streak` は**日数ではなく実行回数**。連打すれば増えるので、表示は必ず `first_seen_at`
（初回時刻）とセットにする。回数だけを見せると滞留の長さを誤読させる。

#### key は `LaneId`、名前は記録時点のスナップショット

§8.5 が先に決めていた通り。実装で効いた点:

- **名前は書いた後 更新しない**。更新すると過去の記録が rename で書き換わる
  （「old-feat を見送った」が「new-feat を見送った」になる）。しかも見送った lane は
  引く先が消えているので、そもそも今の名前を引けない
- 逆に **`vp lane cleanup` の現在の行は survey が持つ生きた名前**を出し、帳簿からは
  回数と初回時刻だけを取る。こうすると凍結名が現在の一覧に漏れない
- 同名 lane の再作成が混ざらないのは `lane_ids` state file の GC が支えている —
  `clear_lane_state_in` が lane 削除時に id file を消すので、作り直した lane は必ず別 id
  （名前 key だったら、作ったばかりの lane が「3 回連続で判断待ち」と表示される）

#### 名前 → id を World 側で解決しなかった

帳簿の書き手は CLI（`vp lane cleanup`）だが、**`lane_id` は CLI が解決して RPC に載せる**。
World の registry から引く形にしなかった理由:

- 見送りの対象には **World が一度も見たことのない lane**（disk にだけ在る worktree）が
  含まれる。registry 逆引きだとそれらが黙って記録から落ちる — 落ちるのは**まさに放置されて
  溜まった lane** なので、追いたいものだけが追えなくなる
- id の SSOT は `lane_ids/<project>__<lane>` state file で、World の `LaneInfo.id` も
  同じ `lane_id::load_or_create` から来る = **両プロセスが同じ id を得る**
- 解決の順序が意味を持つ: **lane を消す前に**引かないと、`clear_lane_state_files` が
  id file を消した後に別 id が生える（記録が「知らない lane」になる）。この順序を握れるのは
  削除を実行する CLI 側

repo basename から project key を導く derivation は `lane_state_project_key` に畳んだ
（同じ導出を要る場所が 2 つになったので、1 箇所ズレて無音で別 key を触るのを構造的に防ぐ）。

#### `Unknown`（稼働状況不明）では 1 文字も書かない

保留は survey に進まないので観測が 1 件も生まれず、帳簿への呼び出し自体が起きない。
**事実が無い状態を履歴に残さない** — 書いてしまうと、後から見た人が「その日は判断待ちが
0 件だった」と読む（無いのは判断待ちではなく観測）。

これを `cleanup_performers_with` に注入した `FarewellLedger` の spy で固定した。
`resolve_origin` を関数で注入したのと同じ理由（**順序が意味を持つものは注入して検証する**）。

#### 経路は world-control（#858 の型）

帳簿は `db/world/` にあり surrealkv の OS 排他ロックで World が専有する（§8.4）ので、
CLI からは World 経由でしか触れない。#858 で確立した world-control の型に倣い
**server + client + KDL + drift テスト**を揃えた:

| method | 種別 | KDL |
|---|---|---|
| `host/farewell_observe` | mutation（判定の記録 + 滞留の返却） | omission（手で叩くと**起きていない見送り**を書ける） |
| `host/farewell_reclaimed` | mutation（実行の記録） | omission（同上） |
| `host/farewell_log` | read-safe（履歴の読み出し） | **記述**（agent に露出する観測面） |

`observe` が記録と読み出しを 1 往復にまとめているのは、`vp lane cleanup` が判定を表示する
その場で「何回目 / 初回いつ」を添えるため（別 RPC にすると表示のたびに 2 往復になる）。

帳簿への書き込み失敗は **見送りを止めない**（best-effort）。止める理由になるのは稼働状況が
不明な時だけ — 記録は判断材料であって、それが取れないことは lane を消してよいかの判断を
変えない（1 つの失敗に 2 つの意味を持たせない）。

#### テストが固定していること

- `ledger::stable_verdicts_write_nothing` — 安定した lane は **0 write** で素通りする
  （ここが崩れると帳簿が実行回数に比例して太る）
- `ledger::consecutive_ask_human_folds_into_streak` / `resolved_pending_is_closed_not_extended`
  — 連続の畳み方と閉じ方（時刻は注入して固定値で検証）
- `ledger::rename_keeps_history_in_place` — **同じ id を別名で観測しても**滞留は繋がり、
  記録済みの名前は動かない
- `ledger::recreated_lane_does_not_inherit_history` — 同名 lane を作り直しても前の滞留を
  引き継がない。`lane::commands::recreated_lane_gets_a_fresh_stable_id` がその土台
  （lane 削除で id file が消える）を別途固定する
- `lane::commands::cleanup_output_carries_stagnation_from_ledger` — **滞留が出力に実際に出る**。
  出力を `&mut dyn Write` で注入して本文を assert する（書き込みだけを見ると読み手のない
  書き込みに戻る）。表示名が survey 由来（帳簿のスナップショットではない）ことも同時に見る
- `lane::commands::cleanup_holds_when_liveness_is_unknown` — `Unknown` では帳簿に触らない。
  同時に `Known(空)` では観測が送られることも見て「常に書かない」が緑にならないようにする
- `ledger::farewell_rows_share_the_normalized_row_key` — 書き手と読み手で path の形が
  違っても同じ行（起点 / 並び順と同じ回帰、`row_key` に乗っているか）
- `ledger::observation_serde_shape` — wire 形の往復。ここがズレると World 側の
  `from_value` が落ちて**記録だけが黙って止まる**（cleanup 自体は動くので気付けない）

## 8. 帳簿の第一形 — 開発起点ポインタ（D4、2026-07-21）

> **実装済**。D4「conductor = Host が持つポインタ」を、判定 → 経路 → surface → 消費で
> 縦に 1 本通した（P3 の見送りと同じ形）。

### 8.1 §6.6 の予告は誤りだった — `is_conductor` は 2 つの問いを兼ねていた

§6.6 は「P3 で Host がポインタを持てば、`stand_spawner::claude_command` は
『起点 lane か？』を Host に問う形になる」と予告した。**これは実装できない。**

同じ箇所のコメントが理由を書いている:

> `--continue`（最新セッションを継ぐ）が自分の会話に当たるのは開発起点 lane が repo root に
> 居るから。worktree の lane で同じことをすると他 lane のセッションを掴む（dashboard 罠）。

つまり `claude_command` が訊いているのは **「この lane の cwd は repo root か？」（物理）**で
あって、**「開発起点か？」（意図）**ではない。今は起点 = 予約名 = repo root が一致しているので
同じ答えになるだけで、D5 が起点を worktree lane へ移せるようにした瞬間に分岐する。
予告通り Host ポインタへ繋ぐと、**そのコメントが警告している dashboard 罠が発火する**。

→ `claude_command` の分岐は cwd の性質（`has_ground` 相当）を訊く形が正しい。本 PR では
挙動不変で残し、ポインタとは繋がない（§8.5）。

> 1 つの述語が偶然 3 つの性質（repo root / 予約名 / 開発起点）を同時に満たしていると、
> どれを訊いているのか静的には判別できない。**答えが分岐するケースを作って初めて見える**。

### 8.2 key は名前ではなく `LaneId`

帳簿が持つのは `project_path → lane_id`（`host_origin` table）。address 文字列にしない理由:

- ポインタが指すのは lane **そのもの**であって「今その名前で呼ばれているもの」ではない
- 将来 lane を rename できるようにすると、名前 key のポインタは書き換えが要る
- §6.3 の教訓（address 文字列を key にした列は起動時 migration が要る）を繰り返さない

`LaneId`（UUID v7、doc 24 §7 の I1）は 2 年間 **生成・永続されながら誰にも読まれていなかった**
（`LaneInfo.id` に載って wire も流れていたが、pool key も DB 列も address 文字列のまま）。
strangler の「id を持つが id で引かない」中間状態が止まっていたもので、帳簿がその**最初の読者**になる。

名前 ↔ id の変換は **境界で 1 回だけ**行う: 人が打つのは名前（`vp lane origin <name>`）、
帳簿に入るのは id（`ledger::lane_id_of` が書き側、`ledger::resolve_origin_name` が読み側）。

### 8.3 フォールバックは 3 値で返す

ポインタが無い / 指す lane が実在しない場合は予約名 `conductor` に落ちる。ただし
**どう決まったかを隠さない**（`OriginSource::{Default, Pinned, Dangling}`）。

`Dangling` を潰して `Default` に畳まないのは、指定したはずの起点が消えた時に人が気付けなく
なるため。§7.4 の「判定は正しくても添える事実が嘘」と同じ規律 — Host は人の判断材料を作る
立場なので、区別できるものを畳まない。

### 8.4 CLI から帳簿を読む経路

帳簿は DB（`db/world/`）にあり、surrealkv の OS 排他ロックで **World が専有**する。
CLI（`vp lane cleanup` / `vp lane origin`）は直接読めないので、`switch_lane_via_quic` と同じ
World process-proxy ask（`lane_origin_get` / `lane_origin_set`）を通す。

World 不在なら予約名にフォールバックし、**その旨を告げてから続行**する。黙って落とすと
移動済みの起点 lane を見送りうる（実害の確率は低いが、確認できなかった事実は人に見せる）。

### 8.5 このスライスに入れなかったもの（判断は済ませてある）

| 項目 | 判断 | なぜ今やらないか |
|---|---|---|
| lane の並び順 `ord` | 帳簿に **id key** で持つ。`lane` table には置かない（`upsert_lane` が DELETE+CREATE で消すため — `lane_lifecycle` を別 table にしたのと同じ理由） | 消費者が P4 の reorder UI しかない。**読み手のない書き込みを作らない**（§8.2 の `LaneId` がまさにその状態だった） |
| ~~見送りの履歴~~ | 帳簿に **id key + 記録時点の名前スナップショット**（履歴は rename で動いてはいけない / 同名 lane の再作成と衝突してはいけない） | → **§7.5「✅ 着地: 見送りの帳簿」で実装済**（2026-07-22）。判断はこの行のまま通った |
| ~~`AskHuman` の滞留~~ | ~~**持たない**~~ → **持つ** | → **§7.5 で訂正**。「滞留は lane が残るので復元可能」は誤りで、復元できるのは**現在の verdict** であって「いつから / 何回目」ではない（それは観測の履歴） |
| lane の rename | state file 5 系統（`lane_ids` / `echoes_sessions` / `console_mode` / `replay_log` / `cc_session`）の key を id 化してから | それらの名前 key は `load_in(base, project, lane)` の**内側に閉じている** = カプセル化された負債で、放置しても修正箇所が増えない。address 文字列（§6.4）が viral だったのとは性質が違う |
| `claude_command` の分岐 | `has_ground` 相当（cwd が repo root か）を訊く形へ | §8.1。挙動変更なので構造変更と混ぜない（§6.6 と同じ分割） |

## 9. lane 作成の入口を 1 本にする（§6.5 の統合、2026-07-21）

> **実装済**。§6.5 が挙げた「経路ごとに validation の効く範囲が違う」を、名前 gate の
> 一本化で解いた。§6.5 の follow-up バグ（descriptor 破壊疑い）は**実在が確定**し、
> 決定的な回帰テストで固定した。

### 9.1 バグは疑いではなく実在だった（ただし普段は masking されている）

`ProcessManagerCapability::create_lane` に `name = "conductor"` を投げると:

1. `upsert_lane`（DELETE+CREATE）が `<project>/conductor` 行を**上書き**
2. clone が `validate_performer_name` で失敗
3. rollback の `delete_lane` が `<project>/conductor` 行を**削除**

= 拒否されるべき request が**本物の開発起点 descriptor を消す**。テストで `list_lanes()` が
`[]` を返すことを確認済み（`test_create_lane_rejects_reserved_name_without_touching_db` の
修正前挙動）。

実機で再現しなかったのは、手前の **dup check（in-memory `lane_registry`）が先に弾いていた**
から。`lane_registry` は daemon boot で db から load されるので、通常は conductor 行が入って
いて「already exists」で止まる。だが:

- **dup check は validation ではない**（別の関心事で、たまたま同じ入力を弾いていただけ）
- **その cache は db と乖離しうる** — boot load 失敗（`list_lanes` Err は「空で継続」）や
  SP snapshot による上書き（前例: `build_lanes_snapshot` が performer を落とした #683）

> 「実機で再現しない」は「バグが無い」ではなく「**別の何かが偶然マスクしている**」の
> ことがある。マスクしている側が壊れる条件を数えると、実在かどうかが決まる。

### 9.2 本質はガード漏れではなく順序

`create_lane` は doc 24 §4.6 の **intent-first bracket**（crash 耐性のため provision より
先に descriptor を永続する）で、この設計自体は正しい。問題は validation が bracket の
**内側**に居たこと。

> intent-first は「意図を先に書く」パターンなので、**意図が不正なら bracket に入る前に
> 落とす**必要がある。書くのを早めた分だけ、検証も早めないと不正な意図が永続に届く。

### 9.3 gate は `validate_performer_name` 1 本

両経路とも入口で同関数を呼ぶ形に揃えた（空文字 / 文字 allowlist / 先頭文字 / 予約名）。

| | 旧 | 新 |
|---|---|---|
| unison `lane_create` | 空文字 + 予約名を直書き。文字 allowlist は奥の clone 頼み | 入口で `validate_performer_name` |
| HTTP `POST /api/world/lanes` | 空文字のみ。それ以外は奥の clone 頼み（= 永続の後） | 同上、**永続の前** |

副産物: `validate_performer_name` の予約名判定が `"conductor"` 直書きだったのを
`CONDUCTOR_LANE_NAME` に寄せた（§6.4「型を経由しない文字列」の同型が 1 件残っていた）。

### 9.4 残り — 実装の統合は別スライス

本 PR で揃えたのは**入口の gate** だけ。2 経路の実装自体（`create_performer_orchestrated` は
clone + PtySlot spawn、`create_lane` は daemon 側 worktree provision のみ）はまだ別物で、
D3 の「迎え入れ」を Host に実装する時に寄せる。gate が 1 本になったので、その時に
「どちらが正か」を決めるだけで済む。

> **→ 完了（2026-07-22）**: §6.5「✅ 着地: 実装を 1 本にする」。予告どおり「どちらが正か」を
> 決めるだけで済み、core（`create_performer_orchestrated`）に寄せて World 側は adapter 化した。
> D3 の Host を待たずに畳めたのは、**入口の gate が既に 1 本だったおかげで
> 「実装の差」だけを見ればよかった**ため。

## 10. P4 第一スライス — 開発起点を GUI から見る / 動かす（D5、2026-07-21）

> **実装済**。§8 で作った帳簿のポインタを、sidebar の star と context menu に繋いだ。
> D5 の「起点再指定は sidebar の lane メニュー（Host のポインタ更新のみ。何も動かない）」。

### 10.1 起点は `LaneInfo` ではなく snapshot に添える

`ProcessMessage::LanesSnapshot` に `origin: Option<String>`（lane 名）を足した。
`LaneInfo` に持たせない理由: `LaneInfo` は `lane.descriptor` として **DB に永続される**ので、
起点を入れると帳簿と二重の真実源になり、片方が stale になる。起点は lane の属性ではなく
project の指定（D4）なので、project 単位の snapshot に 1 本添えるのが正しい層。

### 10.2 `None` は「無い」ではなく「判らない」

publish 経路は 2 本ある（project runtime の live push と、World が vp-app 接続時に配る
retained snapshot）。**片方だけ解決すると受け手が起点の有無で flicker する**ので、
解決は `ledger::origin_name_for_lanes` 1 本に畳み、両方が通る。

その上で `origin: None`（旧 server / 欠落）は **受け手が前回値を保つ**。既定値に落とすと、
起点を指定済の project で star が明滅する。

### 10.3 楽観更新をしない

IPC handler は `sidebar_state` を先読み更新せず、次の snapshot の `origin` を待つ。
帳簿が真実源なので、楽観更新すると失敗時に UI だけが嘘をつく。

### 10.4 帳簿の row key は module 境界で正規化する

**実装中に見つけた不整合**: `ProjectRuntimes::start` は map key に `normalize_path_key`
（canonicalize 済）を使う一方、`CapabilityConfig.project_dir` には**生のパス**を渡す。
そのため帳簿に触る 4 経路で渡ってくる path の形が揃っていない:

| 経路 | 渡す path |
|---|---|
| `process::server::publish_lanes` / unison handler | `state.project_dir`（生） |
| `daemon::server::send_lanes_snapshot` | `path_key`（正規化済） |

ズレると**書き手と読み手が別の行を触り、起点を指定しても snapshot に載らない**。
症状は「設定が効かない」だけで error も log も出ない — 完全に無音で失敗する。

慣習は call site 側で正規化だが、**帳簿は書き手が 1 module に閉じた新設 table なので、
正規化を `ledger::row_key` に畳んで構造的に一致させた**。経路 4 本のうち 1 つ忘れたら
無音で行が割れる形を、そもそも作らない。

> 本番では両 path が偶然一致していれば動く。§9.1 と同じ「masking されているだけ」の形で、
> テスト（`write_and_read_agree_across_path_shapes`）で固定した。

### 10.5 icon は Phosphor モノクロ

star は `ph:star-fill`（表示）/ `ph:star`（menu）。sidebar は既に emoji ゼロで Phosphor に
統一済み（`LanePicker` の「脱 TUI: 📁/◉ の text glyph を CreoIcon に置換」）。

配置は stand icon の直後 = 「この lane が**何か**」を修飾する層（右端の state / badge は
「今**どうなっているか**」で層が違う）。色は `--lg-mute` で**光らせない** — 起点は状態では
なく属性なので、目立たせると常時鳴る警告になる（光 = needs-you の専有）。

### 10.6 未解決 — vp-app への push 起床経路（実機確認が要る）

**本スライスのバグではなく、P1 fold-in の副作用の疑い。** team-b レビュー（#834）が指摘。

vp-app は World daemon の `"lanes"` channel を購読する。初回 snapshot 送信後の**再 push は
`lane_change_tx` 駆動のみ**（`daemon/server.rs` の push loop）で、その `send` は
リポジトリ全体で **1 箇所** — wire `send`/`ack` 後の `notify_lane_change_for_projects` だけ。

一方 `handle_lane_origin_set` は帳簿を書くだけで、`lane_change_tx` にも `SystemEvent` にも
触れない。project 側の `publish_lanes`（5s tick + `SystemEvent::Lane`）は
`world_lanes`（= daemon の `lane_registry` **そのもの**）を更新するが、
**daemon 側 push loop を起こす手段を持たない**（`AppState` に `lane_change_tx` が無い）。

この辺は元々「SP 自己登録（`register`/`lanes-diff`）→ `lane_registry` + `lane_change_tx`」が
担っていた。fold-in の置き換えコメントは `running_processes` / `lane_registry` /
`process_presence` の移管を挙げるが、**`lane_change_tx` の発火はその列に無い**。

観測と矛盾する点があり、静的読みだけでは決まらない: vp-app 側は `LanesLoaded` を
「project × frequency でループする systematic event」と書いており、push が高頻度である
前提に見える。wire 活動（hook が prompt ごとに撃つ）が十分頻繁で masking しているのか、
別の push 経路があるのかは**実機で見ないと判別できない**。

> 推測で直さない: `publish_lanes` から起床を撃つと **5s × project 数**の全 snapshot push に
> なる。それが意図的に避けられている設計なのか単なる配線漏れなのかは、実機の挙動を見て
> から決める（§9.1 の「masking されているだけ」を今度は逆方向に踏まないため）。

**次に実機を触る時の最優先確認**: 「開発起点にする」クリック後、star が即動くか /
次の wire 活動まで固まるか。固まるなら影響は origin だけでなく lane 作成・削除・死活の
反映にも及ぶので、doc 44 の独立 task として起票する。

### 10.7 残り

タブ strip の header 昇格（D5 前半）と lane の並び順（`ord`、§8.5 で key の判断のみ済）は未着手。

## 11. lanes push の起床経路を fold-in 後に再配線する（2026-07-21）

> **§10.6 の未解決を決着**。実機を待たずに静的に確定できたので直した。
> P4 のバグではなく **P1 fold-in の副作用**。

### 11.1 何が落ちていたか

vp-app は World daemon の `"lanes"` channel を購読し、初回 snapshot 以降の**再 push は
`lane_change_tx` 駆動のみ**。その `send` はリポジトリ全体で **1 箇所**（wire `send`/`ack` 後の
`notify_lane_change_for_projects`）しか無かった。

fold-in 前は SP の QUIC uplink（`register` / `lanes-diff`）が

1. World の `lane_registry`（= 集約 view）を更新し
2. 同時に `lane_change_tx` を撃つ

の**2 つを一緒に**やっていた。fold-in で uplink が消えた際、**1 だけが `publish_lanes` へ
移管され、2 が移管されなかった**。

結果: vp-app の sidebar は **wire 活動がある間しか新鮮でない**。wire hook は prompt 送信ごとに
撃つので「作業中は更新される / idle だと固まる」となり、**sidebar を見るのは作業中だけ**
なので気付かれなかった（§9.1 と同じ masking）。

固まる対象は lane 追加・削除・死活（dim）・git meta・開発起点 star・並び順すべて。

> **同型の前例がある**: `process_lifecycle_tx` も同じ「旧 registry handler が担っていた経路」で、
> fold-in 中に「生産者ゼロで永久沈黙する」として発見・再配線済（`server.rs` にコメントが残る）。
> 1 つの辺が 2 つの仕事をしていると、片方を移管した時にもう片方が静かに落ちる。

### 11.2 静的に確定できた根拠

実機を待たなかったのは、push 経路が閉じていることを読み切れたため:

- `lane_change_tx.send` は 1 箇所（grep で全数確認）
- daemon の push loop は `rx.recv()` のみで駆動（periodic な再送は無い）
- vp-app の heartbeat は**失敗時にしか**再接続しない（15s ping は liveness 確認だけで、
  成功しても再購読しない）→ 「たまたま再接続で新しい snapshot が来る」救済も無い

### 11.3 直し方 — 指紋で「変わった時だけ」起こす

`publish_lanes` に `LaneChangeNotifier` を持たせ、**内容が前回と変わった時だけ**通知する。

素直に毎回撃つと、供給点の 1 つが **5s periodic tick**（disk-only performer の safety net）
なので *5s × project 数* の全 snapshot push になる。指紋は
**vp-app に届く値そのもの**（`lanes` + `origin`）から取る — 一部だけにすると
「見えている値が変わったのに起こさない」穴ができる（例: `lanes` だけを見ると
D4 の起点変更で star が動かない）。

channel は DaemonState より**先に**作って `ProjectRuntimes` と両方へ配る。生産者が
project 側・消費者が daemon 側なので、DaemonState 任せだと生産者に渡す手段が無い
（`process_lifecycle_tx` を capability と共有しているのと同じ構図）。

### 11.4 指紋の純度 — 非決定な値を混ぜない

指紋方式は「snapshot に**呼ぶたび変わる値**が混ざっていたら無効化される」という前提に立つ。
team-b レビューが実際に 1 件見つけた: `build_lanes_snapshot` の self-heal merge
（pool 未登録の disk-only performer を placeholder として載せる）が
`created_at: chrono::Utc::now()` を焼いていた。

これが発火する間は指紋が毎回変わり、**その project だけ 5s tick がそのまま push 源に戻る**。
発火条件は ①project 起動直後の boot window（spawn 完了まで）と
②`delete_performer(cleanup=false)` の残置 dir（手動掃除するまで恒久的）。

ground dir の birthtime（無ければ mtime）に変えて決定的にした。placeholder の
`created_at` が publish ごとに動くこと自体が元々おかしく、**指紋を導入したことで
初めてその不整合が「効く」ようになった**という関係。

> 指紋・ハッシュ・差分検出を入れる時は、**入力に非決定な値が無いか**を必ず棚卸しする。
> 混ざっていても機能は壊れず「効かなくなる」だけなので、テストも実機も静かに通る。

### 11.5 テスト

- `publish_lanes_wakes_vp_app_push_loop` — 起こすこと
- `publish_lanes_does_not_wake_on_unchanged_snapshot` — 5s tick が push 源にならないこと
- `notifier_wakes_when_only_origin_changes` — 指紋の取り方（origin だけの変化も拾う）

いずれも**起床通知を外すと赤くなる**ことを実証済み。

## 12. lane の並び順（P4 後半、2026-07-21）

> **実装済**。§8.5 で「帳簿に id key で持つ / 実装は消費者と同じスライスで」と決めていたものを、
> sidebar の D&D と一緒に通した。

### 12.1 なぜ `lane` table に置けないか

`upsert_lane` は DELETE+CREATE なので、project 由来の descriptor push が来るたび
同じ行に載せた `ord` が消える。`lane_lifecycle` を別 table にしたのと**同じ理由**で、
「Host の intent」と「lane が報告する state」は table を分ける。

key は `host_origin` と同じく **`lane_id`**。並び順は lane そのものに付く指定なので、
表示名が変わっても動いてはいけない（§8.2）。

### 12.2 未指定は「末尾」

指定のある lane が ord 昇順で先、無い lane は**元の並びのまま**後ろ（安定 sort）。

- **新しく作られた lane が既存の並びに割り込まない** — 割り込むと「並べ替えたのに
  勝手に崩れた」に見える
- **帳簿が何も言っていない範囲では既定の意味論を壊さない** — `LanePool::list()` の
  「開発起点が先頭 → created_at」がそのまま残る

### 12.3 適用は `build_lanes_snapshot` の 1 箇所

`lanes_list`（CLI / GUI の直接問い合わせ）も QUIC snapshot も同じ関数を通るので、
**見る場所で順番が違う**ことが起きない。供給点ごとに sort を書くとそこが割れる。

### 12.4 楽観更新をしない — project の並べ替えとは違う

既存の `commitProjectReorder` は楽観更新する（Rust が re-push しない設計だったため）。
lane の並べ替えは**しない**:

- 並び順の真実源は Host の帳簿。楽観更新すると保存失敗時に UI だけが嘘をつく（§10.3 と同じ規律）
- §11 で push の起床が直ったので、保存 → `SystemEvent::LanesReordered` → publish →
  snapshot が即座に戻る

### 12.5 `SystemEvent::LanesReordered` を足した理由

並び替えでは**個々の lane は何も変わらない**ので `Lane(Diff::*)` では表せない
（`Diff` は per-lane の差分で、偽の Add/Update を流すと購読側が実在しない変化に反応する）。
並びは snapshot 全体の性質なので独立 variant にした。

### 12.6 D&D の伝播 — `drop` はポインタ直下で発火する

`LaneRow` は project accordion（`<details>`、こちらも draggable）の内側にある。
初版は `onDrop` の冒頭で無条件に `stopPropagation()` していて、**project を drag して
lane 行の上で離すと drop が消える**回帰を作っていた（team-b が検出）。

HTML5 DnD の `drop` は**ポインタ直下の要素**で発火し、祖先が `dragover` で
`preventDefault()` していても発火先は変わらない。つまり lane を drag していなくても
`LaneRow.onDrop` には来るので、**ガードが真の時だけ止める**必要がある
（同ファイルの `onDragOver` は最初からその順序だった = 隣接するハンドラで規律が割れていた）。

### 12.7 部分解決は許すが全滅は拒む

`set_lane_order` は解決できなかった名前を落として続行し、**1 つも解決できなければ Err**。
一覧と帳簿の間には常に時間差がある（送信中に lane が消える等）ので 1 件の不一致で
並べ替え全体を失敗させる意味は無い。逆に全滅は呼び手の指定が丸ごと間違っているので、
黙って空の並びを書かない。

### 12.8 手順の訂正 — clippy は `-D warnings` で回す

本スライスで `SystemEvent` に variant を足した結果 `clippy::large_enum_variant` が発火し、
**CI（`cargo clippy --workspace --all-targets -- -D warnings`）だけが落ちる**状態になった。
素の `cargo clippy` は warning のみで exit 0 なので気付けない。

`/release` skill にも「doc と同じ厳格ゲート（`-W` ではない）」と明記されている。
以降のスライスはローカルでも `-D warnings` を付ける。

（対処は `#[allow(clippy::large_enum_variant)]`。本 enum は broadcast channel を流れるだけで
滞留せず最悪 22KB、対して `Lane(Diff::*)` の構築点は複数あり Box 化は割に合わない。）
