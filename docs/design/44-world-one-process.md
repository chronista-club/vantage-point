# doc 44 — World 一枚化と Project Host（SP の転生・slot 語彙・conductor の再定義）

> **status**: 方向確定（2026-07-20 の dogfood 議論。実装未着手 — 本 doc は議論の凍結）
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

- P1 の panic 封じ込め: 一枚化した World 内で 1 project の障害をどう局所化するか
  （task 境界 / catch_unwind / health monitor の粒度）。
- DB handle: per-project vpdb を World がどう束ねるか（namespace vs multi-handle）。
- `vp ps` の意味論: SP プロセス一覧 → 「active lane を持つ project 一覧」へ。
- デバッグモード（`vp sp start -d`）の新しい家。
- hub federation は World レベルなので原理的に無関係 — 実装時に要確認のみ。
- Host の帳簿の永続化先（surrealkv）と、creo-memories との棲み分け。
