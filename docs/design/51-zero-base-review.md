# doc 51 — ゼロベース棚卸し: 二つの認知のための環境

**Status**: 進行中（2026-07-24 開始）。「1 枚の Echoes」を原点に、これまでの開発を
必要の側から一つずつ再審査する。**速く作ることを優先しない**（mako 規範 2026-07-24 —
いろいろな方向から検討して、最高の開発環境を作るのが目的）。
**Owners**: mako × Claude（本 doc は審査の進行と結論の帳簿）
**Related**: [50-pane-chrome-and-session-panes.md](./50-pane-chrome-and-session-panes.md)（直前の到達点）

---

## 0. スコープと原理

**backend は信任する**（P4/P5 の session registry / pty_slots / resume / replay は堅い）。
審査するのは上物 — **UX と、mako と AI 双方の認知、開発を強く美しく進める環境と道具**。

### 0.1 原理: 表象の共有

> **同じ構造を、人は目で掴み、AI は address で掴めること。**
> mako の認知資源は画面と注意。AI の認知資源は context と tools。
> 良い構造とは両方から同じ形に見える構造 — 表象が共有されているとき、協働の摩擦が消える。

実証（2026-07-24 の dogfood）: うまくいった瞬間は表象が共有されていた
（`layout_get` の notation と mako の画面は同じもの → 「三枚並べて撮り比べ」が成立）。
躓いた瞬間は表象が割れていた（Act II の画面は mako にしか掴めず、AI に GUI の address が
無い / AI の実装進行は report 頼み）。**摩擦は常に、片方の認知にしか存在しないものの
周りで起きる。**

### 0.2 審査の 3 レンズ（各項目をこの順で通す）

1. **どうしたいか** — 体験の理想。実装の都合を忘れて、まずここ
2. **どうすべきだったのか** — 回顧。作ったものは理想に向かっていたか、どこで逸れたか
3. **何が必要か** — 理想への最小の形。そして毎回:
   - mako の目にどう見えるか（認知負荷・注意の配分）
   - AI の address にどう掴めるか（観察・宛先・検証）
   - **両者は同じ構造を指しているか**（割れていたら摩擦の源）

### 0.3 歩く順路（内側 → 外側）

| # | 必要（の候補） | これまでの答え | 審査 |
|---|---|---|---|
| 1 | もう 1 本の往復 | session 複数化（doc 38→46→50） | **§1 済** |
| 2 | 同じ往復を別の見え方で | Act I/II → Act = Pane kind（P4 残） | **§2 済** |
| 3 | 別の相手に投げたい | engine 選択（doc 39） | **§3 済** |
| 4 | 死んでも続きから | resume / replay 永続 | **§4 済** |
| 5 | 隔離された並行作業 | performer lane | **§5 済** |
| 6 | AI が視せる面 | PP board / Canvas | **§6 済** |
| 7 | project を跨ぐ | sidebar / TheWorld | **§7 済** |
| 8 | AI と一緒に UI を調整 | Editor Mode / layout MCP（doc 48/49） | **§8 済** |
| 9 | 物理で演奏する | 艦隊 | **§9 済** |
| 10 | lane 同士の通信 | wire | **§10 済** |
| 11 | machine を跨ぐ | hub federation | |

### 0.4 AI 側の摩擦台帳（面 B、2026-07-24 の実測。対応項目に差し込む）

| 摩擦 | 欠けている道具 | 差し込み先 |
|---|---|---|
| GUI の状態を変えられない（Act 切替を mako に依頼） | GUI 操作の MCP 面（lane 選択 / pane focus / mode） | §2 |
| 並べ確認を mako に依頼 | lane scope の layout MCP（write gate 待ち） | §1 |
| chat 会話を読めない | session 会話の観察 MCP | §1 |
| 検証 loop = swap ~90s | webview の hot reload | §8 |
| shot → crop の手作業 | pane 単位 capture | §8 |

---

## 1. もう 1 本の往復（session 複数化）— 2026-07-24 結論

### どうしたいか（mako）

> **同時注視。二つ以上 Echoes があるということは、同じ場所での作業の並行性を支える
> 視点が必要。**

- lane = **場所**（同じ worktree / cwd）。session = その場所で並行する往復
- 複数 session は「棚に置いてたまに見る」ものではなく「**並べて同時に見る**」もの
- 別の場所での並行は performer lane（#5）の領分 — 混ぜない

### どうすべきだったのか（回顧）

- N session は doc 37/38 で「engine 並走比較」の器として生まれ、UI は tab（排他表示）の
  仮置きだった。**同時注視の必要に対して、排他表示の tab は初日から形が逆**だった
- 現行の既定「1 枚ずつ表示」（showOnly、doc 47 §1 決着までの暫定）も同型の逆 —
  同時注視の必要が確定した今、**暫定は解除方向**
- 教訓: 「どうしたいか」を先に決めずに「作れる形」（tab）から入ると、仮置きが住み着く

### どうしたいか — 続報（2026-07-24、mock v2 で確定）

> **lane = cwd / branch / board を共有する作業台。** performer（別の場所）に投げない
> 同じ場所での並行を、**異種の器械**で支える — cc と対話し、grok に打たせ、
> 素の Echoes でログを tail し、PP が視せる。

- **PP は app の隣人ではなく lane の一員**（doc 46 の `canvas` kind の予約が正解だった。
  board の lane 一本化（proj 撤去）もここに収束）
- 作業台の合流: board には cc も grok も show でき、tail の WARN と grok の調査が
  同じ事象を指す — 情報が台の上で合流する

### 何が必要か

- **並べる既定（tiling）**: session pane は既定で並ぶ。「畳んで取っておく」という
  タブ時代の中間状態は不要方向 → 下端の帯（pane chip）の存在理由が消える
  （= 帯撤去パッケージ: 帯撤去 + tiling 既定 + P4 前倒し + New の入口移設）
- **並行性を支える視点 = 3 状態の灯 + 「今なにを」の動的一行（B 案採用、mako 2026-07-24
  「めっちゃいいアイデア。この見た目今までの数倍いい」）**:
  - 灯: 動いている（緑・脈動）/ 待っている（無灯）/ あなたが要る（赤・速い脈動）
  - 動的一行: 各 pane の「今」を 1 行で（AI の作業要約 / 質問の要旨 / shell の最終
    コマンド + tail 状態）。**名札（素性・不変）とは区別された帯**として名札の直下に
    置く — 縦軸の修正: pane 上段 = 名札 + 今行の 2 段
  - ⚠️ 供給源の設計が要る: 一行は誰が書くか（engine の turn 要約 = AI が自分の状態を
    環境へ報告する新しい契約 / shell は最終コマンドで機械的に導出可）— 表象の共有の
    AI→人 方向。§1 の実装フェーズで設計
- **AI 側の対**: 宛先は済（P2 で session addressable）。残りは**観察**
  （session 会話を読む MCP）と**配置**（lane scope layout MCP の公開）— 表象の共有を
  API まで届かせる

mock: `.notes/` ではなく Artifact（vp-concurrent-gaze、workbench-v2）。
layout 記法: `cc16 | grok17/board | sh18`

---

## 2. 同じ往復を別の見え方で（Act I/II）— 2026-07-24 結論

### どうしたいか（mako）

> Act II で行っていて、Act I でしかできないことが出たり、Act II の使い勝手が微妙な時は
> Act I によく戻ってた。**切り替えつつも、セッションは継続させたかったから、とても
> 助かってた。**

- 乗り換えは目的ではなく**避難**（capability gap / quality gap からの）。理想は Act II の
  充実で頻度が下がること。ただし構造的 gap（生の TUI 対話）は Act I の領分として残る
- **価値の核 = 会話の連続性**: 見え方は替わっても往復は切れない（Echoes の定義そのもの —
  往復が本体、見え方は衣装）。resume handoff は残す価値のある機構

### どうすべきだったのか（回顧）

- 実装は「lane 全体の mode 切替」だったが、必要だったのは **session 単位の見え方切替**。
  backend は P4 #848 で session 属性化済み — UI（lane toggle）だけが取り残された
- アーキテクチャの事実: 同じ session の term / chat **同時 2 枚は原理的に不可**
  （Act I = TUI claude 常駐、Act II = headless stream-json。1 会話 1 プロセス、
  切替 = resume handoff でプロセスを入れ替える）。同時 2 view は追わない

### 何が必要か

- **per-pane の「見え方を切り替える」操作** — session pane の中に置く（避難路なので
  低頻度・低目立ち。計器盤の隅 or 名札 menu。実体は既存 resume handoff）
- **lane-level Act toggle は退役**（doc 50 P4 完遂。作業台に「lane の mode」は存在しない）
- **AI 側の対**: `console:set_mode` の session 単位化を P4 に含める（AI も「この pane を
  term にして」と address できる）

---

## 3. 別の相手に投げたい（engine 選択）— 2026-07-24 結論

### どうしたいか（mako）

> 開発を続けていくと役割固定になっていくと思うけど、進化が目まぐるしいから、
> その場の判断も重要。

- **役割は固定化へ収斂する — が、固定を構造に焼かない**。engine の進化速度が速く、
  今日の分担は半年後に無効になりうる（model-tier-strategy と同じ哲学）
- 「比較」（同じ問いを並べる）は一次要求ではない
- **まとめ上げる層 = creo-memories**。異種 engine が一つのチームでいられるのは、
  記憶が engine の外に住むから。作業台の共有は三層:
  場所（cwd/branch/board = lane）/ 往復（resume = session）/ **記憶（creo = engine 非依存）**

### どうすべきだったのか（回顧）

- doc 39 の「常駐型のみの一枚岩」と cursor/agy 撤去は正しかった — 目まぐるしい進化に
  個別実装で追従しない、という同じ原理の実装側
- AcpHost / RpcHost の共通骨格（1 度作れば新 engine はほぼゼロ）は進化速度への正しい投資
  （opencode がその実証: route B は「AcpHost 完成後ほぼゼロ」の見積り通り着地）

### 何が必要か

- **作業台プリセット（soft）**: よく使う組（例 `cc | grok/board | sh`）を 1 発で開ける。
  layout scene の lane 版。⚠️ 役割ラベル（「調査係」等）は作らない — engine 名のまま、
  組はいつでも編集可能（固定を構造に焼かない、の UI 表現）
- **その場の判断の入口は現状形**（engine × Act の menu）を維持
- **記憶の配線の等価性（要調査）**: creo-memories MCP が届くのは現状 claude のみのはず。
  codex / grok / opencode の session に同じ記憶の口を配線できるか（各 engine の MCP
  対応）を調べる —「まとめ上げる」image の成立条件
- **AI 側の対**: session 作成の MCP（AI が「grok の pane を開いて調査を打たせる」を
  address できる — 面 B 台帳に追加）

---

## 4. 死んでも続きから（resume / replay）— 2026-07-24 結論

### どうしたいか（mako）

> **「昨日の続きがそこにある」**

再起動後・翌朝の作業再開 = 昨日の台がそのまま（pane 配置・並び・focused まで復元）。
白紙から組み直す儀式ではなく、続きから。

### どうすべきだったのか（回顧）

- 会話の復元（`--resume` / transcript replay / disk 永続）への投資は正解だった
  （§2「会話の連続性が価値の核」の裏付け）
- ただし復元を**往復単位**でしか考えてこなかった。今の VP は
  場所（cwd/branch）永続 ✓ / 記憶（creo）永続 ✓ / 往復（resume+replay）永続 ✓ /
  board（SurrealDB）永続 ✓ / **台の形（layout・focused）だけが揮発** —
  layout 永続は doc 46 P6 → doc 50 P5 と 2 度先送りされ、**復元の非対称**として残った

### 何が必要か

- **台の形の永続**: lane scope の layout（構造 + attention）+ focused session を
  server 永続し、attach 時に復元する。doc 50 P5 を「follow-up」から**本審査の必須**へ格上げ
- **AI 側の対**: lane scope layout MCP の公開（write gate / 承認 UX）と永続化は同根 —
  同じ工事で両方通す（私が台の形を読み書きできる = 「台を整えておいたよ」ができる）

---

## 5. 隔離された並行作業（performer lane）— 2026-07-24 結論

### どうしたいか（mako）

> 同じ形でいきたい。「終わったら知らせて」は cc の subagent にあるので、別 Lane では
> あるけど wire で協調しながら、同じような環境で、私も時には介入して、別の作業・
> タスクを走らせるイメージ。

**委譲の三段階層**が確定:

| 段 | 形 | 距離感 |
|---|---|---|
| subagent（cc の中） | 往復の中の入れ子 | 見えなくていい、終わったら知らせて |
| 台の上の別器械（同 lane） | grok に打たせる等 | 同時注視 |
| **performer lane（別の場所）** | **もう一つの作業台** | wire で協調、灯で見え、時に人が訪ねて介入 |

- performer に「見えない worker」を求めない — 放置型は subagent の領分
- performer lane も作業台（§1 と同じ環境・同じ認知言語）。§4 の台の復元があれば
  「訪ねて介入」がそのまま成立する

### どうすべきだったのか（回顧）

- wire 協調（ack 規律 / NEEDS YOU / flow_state）の方向は正しかった
- ただし**状態言語が二重化**した: sidebar の lane 行（connector FSM / NEEDS YOU）と、
  今回の pane の灯（3 状態）は同じ概念（並行作業の状態）の別方言 — 統一されていない

### 何が必要か

- **同じ形を lane スケールへ**: sidebar の lane 行に 3 状態の灯 + 「今なにを」の動的一行
  （lane の掲げる一行 = root/focused session の now-line）。一行はフラクタルに掲揚される:
  session pane → lane 行 → （将来 project 行）
- **状態言語の統一**: connector / NEEDS YOU / 灯 を一つの 3 状態語彙に畳む
- wire は現状形を維持（協調の背骨）。subagent との使い分け基準を明文化（本節の表）
- **AI 側の対**: wire / lanes_list / capture / nudge で対称性は概ね済 — lane の now-line を
  AI が書く口だけ追加（session の now-line と同じ契約の lane 版）

---

## 6. AI が視せる面（PP board）— 2026-07-24 結論

### どうしたいか（mako）

> PP の最初は掲示板だった。codex・grok は Artifact では賄えないから活躍した。
> **計器盤があると、Information Navigator らしくなってくる。**

- **掲示板は残す** — PP の価値の核は **engine 中立性**（Artifact を持たない engine の
  唯一の「視せる」口）。多 engine 化で見つかった真の役割
- **計器盤へ進化させる** — 同じ item が更新され続ける生きた面（進捗表・テスト結果の
  最新値・design の現在形）。functional name（Information Navigator）に実体が追いつく

### どうすべきだったのか（回顧）

- 掲示板から入ったのは正しい最小形。board 化（#771 server-authoritative）が土台として効いた
- 「claude の canvas」という出自の framing は多 engine 化で古びた — 価値は中立性にあった

### 何が必要か

- **item に identity を与える**: `show` は現状 append 型。id 指定の update（同じ item を
  置き換える）を足すと掲示板が計器盤になる — 「この表を貼っておいて、進むたび更新して」
- **常設（pin）と流れ（stream → 履歴 strip）の区別** — 計器は流されない
- **AI 側の対**: MCP `show` は全 engine から届く（済）。update 口も同じ MCP に足すだけ

---

## 7. project を跨ぐ（sidebar / TheWorld）— 2026-07-24 結論

### どうしたいか（mako）

> 開発したいものが多く、10 に近いプロジェクトをガンガン進めているから、**一覧性は欲しい**。

- sidebar は艦隊の**一覧（fleet overview）**であるべき — 「注視の数個だけ」仮説は否定
- ただし一覧性 ≠ 全展開。求められているのは「**畳んだままでも艦隊の状態が読める**」こと
- **触る頻度はその時その時で結構変わる** — 注視の分布は動的。並び順は固定でなく
  「最近触ったものが浮く」soft な recency（ただし隠さない — 一覧性は保つ）。
  section 名 CURRENTS（潮流）は最初からこれを言っていた

- **一覧と稼働は別**: 全 project の常時稼働はマシン資源（CPU / メモリ）の上限に当たるし、
  **localLLM を動かす余白も確保したい** →「一旦止めておく」を一級の状態にする。
  一覧には全艦が並び、休眠中の行は正直にそう見える（💤 — 2026-07-24 の表示 fix が土台）。
  起こすのは明示操作（▷）— 勝手に起きない（disable の意味論そのまま）

### どうすべきだったのか（回顧）

- sidebar は accordion（展開して lane 一覧）が基本形で、「lane picker」と「fleet overview」の
  2 役を 1 つの構造で担ってきた — 14 project 時代はスクロールと死んだ hint（SP starting…）で
  一覧性が壊れていた（2026-07-24 実機）。一覧性は overview の要求で、picker とは別の関心

### 何が必要か

- **フラクタル掲揚の完成**: §5 で決めた「灯 + 今なにを」を project 行にも —
  session pane → lane 行 → **project 行**。畳んだ project 行 1 行 = 灯（その project で
  何か動いている / あなたが要る）+ 一行（いま何が進んでいるか）。10 行で艦隊全体が読める
- 展開（accordion を開く）=「入る」操作として残す。enable / disable / add は UI から軽く
  （今日は CLI でやった — 引数語彙の不統一 add NAME PATH / disable PATH / stop NAME も要修正）
- **AI 側の対**: fleet の読みは `vp ps` / lanes_list / health で概ね済。project 行の一行の
  供給は lane now-line の集約（新契約は不要）

---

## 8. AI と一緒に UI を調整する（Editor Mode / layout MCP）— 2026-07-24 結論

### どうしたいか（mako）

> できるだけ、あなた（AI）の自由度を奪いたくない。今も bypass を使っている通り、
> 適切に動かしてもらって構わない。

- **gate ではなく、自由度 + 透明性**。bypassPermissions と同じ思想を UI 操作にも
- HITL ループ（doc 48）は「AI が提案し、人が実画面で見る」協働のリズムであって、
  許可の関所ではない

### どうすべきだったのか（回顧）

- lane scope layout の「承認 UX 決着まで非公開」（doc 49 の write gate 保留）は慎重すぎた —
  mako の哲学は最初から自由度側だった。ただし保留の間に監査（settle-log author:"ai"）が
  先に整ったのは正しい順序: **自由は監査の上に開く**

### 何が必要か

- **lane scope layout MCP の公開（gate なし）** — settle-log で監査可能、
  restoreLastSettle で可逆。AI が動かした瞬間が見える軽い合図（一瞬の highlight 程度）
- **GUI 操作の MCP 面も同じ哲学で開く**（面 B: lane 選択 / pane focus / 見え方切替 /
  session 作成）— 「私が台を整えておく」を可能に
- 開発ループの道具（面 B）: webview hot reload / pane 単位 capture — AI の検証自律性
- Editor Mode ループは現行形を継続（実運用 #872 で実証済み）

---

## 9. 物理で演奏する（艦隊）— 2026-07-24 結論

### どうしたいか（mako）

> 注視の移動は（キーボードショートカット含め）実際に使ってる。台の形にも使う。
> **期待してるのは creo-ui を経由したデザインなどの細かい微調整。**

- 現用: **注視の移動**（keyboard + 物理）と**台の形**（knob = share 等）— 磨き続行
- **本命の期待 = Live Token の演奏**: 物理 fader で design token（spacing / color / 字階）を
  撫でて微調整する（size-stepper 構想）。「lane を楽器にする」より先にこちら
- > **みながら、言葉（プロンプト）で調整よりも、つまみで調整する。指先の感覚を見た目の
  > 調整に使いたい。絶対いいものが素早くできる。**
  — modality の原理: 見た目の微調整は**連続空間の探索**で、言葉（離散・往復が遅い）より
  指（連続・即時・双方向）が速い。分業: **AI は言葉の係**（探索の場を組む・値を source へ
  書き戻す）、**指は連続値の係**。doc 48 HITL ループの完成形はこの分業

### どうすべきだったのか（回顧）

- doc 48/49（Editor Mode + 艦隊双方向）は期待の方向と一致していた。warm palette の
  実運用（#872）が最初の一周。基盤はある — 常用化が薄い

### 何が必要か

- **fader ↔ editor field の binding を軽く組める**こと（mapping preset — 「今日はこの
  token 群を演奏する」を 1 手で）
- **lock → 書き戻し**の流れの常設（探った値は source へ、AI が書き戻し係 — doc 48 の HITL）
- **motor fader sync** = 表象の共有の物理面（token の現在値が fader 位置に還る。X-Touch は
  motorized — 画面と指先が同じ値を指す）
- 注視移動: keyboard shortcuts も一級の入力として整備（物理が無い場面の同型）

---

## 10. lane 同士の通信（wire）— 2026-07-24 結論

### どうしたいか（mako）

> 時間かかったけど、直近ではだいぶいい感じで動いてる。**可視化したい。流れが追えれば、
> もっといい**（TODO に既起票かも — 実装フェーズで creo と照合）。

### どうすべきだったのか（回顧）

- 配送規律への投資（2-phase nudge / ack / category = delivery policy）は正しかった —
  時間はかかったが「動く背骨」になった
- 可視化が置き去り = **表象の共有の逆向きの欠け**: wire は AI ↔ AI の言語で、AI からは
  読める（wire_inbox / thread）のに、**mako の目にだけ流れが無い**

### 何が必要か

- **流れの可視化**: 誰が誰へ何を投げ、ack され、どう進んだかを時系列で追える view
  （`vp wire thread` の GUI 版）。doc 44「Host = 生産管理板」と自然に接続する
- 未 ack / 滞留が fleet 一覧の灯・badge に出る（§7 の掲揚に合流）
- 役割・規律は §5 の通り現状形を維持（協調の背骨）

---

（以降の項目は審査が進み次第追記。残: §11 hub）
