# doc 58 — sidebar 名簿化: cwd を SSOT に、並列開発の場所と進行を映す

## Lane 行の改訂仕様（2026-10-17、実機確認待ち）

Related: `mem_1Cg7MpHWBdf6u9CYjyEAaf`、board `a0722a1b-b6e0-4e3a-a720-ea0a9c9c477a`。
以下は 2026-10-17 の確定仕様。session の階層変更は doc 54 の後に別途扱う。

| 状態 | 入る条件 | 出る条件 | 表現 |
|---|---|---|---|
| 要求中 | chat の未回答質問・承認要求 | 対応する回答成功、要求の終了、turn 終了・engine 終了 | 最も目立つ色の記号と要求内容 |
| 作業中 | turn の進行 | 要求、完了、停止 | 回転記号 |
| 指示待ち | 生存中で実行中の turn・未回答要求・未読報告がない | 新しい活動または停止 | 静かな ● |
| 完了・未読 | 未読報告あり | 既読化、新しい活動または停止 | ✓ と未読件数 |
| 停止 | process 停止 | 再開 | 灰色。異常終了は赤を添える |

左端を状態記号、中央を名前と now-line、右端を最終活動からの経過時間の固定列にする。
経過時間は既存 `last_activity_at` を使用し、1 分未満は `now`、以降は `2m` / `1h` と表示する。
wire の時刻は分単位に切り捨てられるため、`now` / `1m` の境界には最大 59 秒のずれがある。
lead と sub は同じ列配置と高さ。`lanes/<name>` の独立行を除き、branch と `↓N`、dirty、
detached の SHA、PR 番号はコンパクトに同じ行へまとめる。
現実装の dirty・behind・detached SHA は `sub_status` が供給される sub 行に限る。
lead は branch のみを取得するため、これらの補助情報は未表示。lead 向けの取得拡張は保留とする。

now-line は要求内容を優先し、通常時は `vp now` の自己申告、無ければ tool 名と引数の要約を表示する。
自己申告は次の tool 開始または turn 終了で失効する。PR 番号は R sidebar の取得済み情報と branch が一致する場合のみ表示する。
要求中の件数は repo 行・sidebar 上端へ、色は pin strip にも投影する。repo 行は lane 件数と開閉を持ち、
lead の pin ラベルは repo 名にする。engine icon と pin の SSOT、三起点の配置規約を保持する。

検出は chat の `ConversationEvent::Question` / `PermissionRequest` と `AskUserQuestion` の
tool 呼び出し・結果を用いる。既存 lane snapshot push へ状態を投影し、表示中の会話への購読に依存させない。
この状態投影では履歴 replay を無視し、現在の要求や作業状態を上書きしない。TUI の要求検出は今回対象外で、Notification hook の別 task とする。

状態は engine の conversation pump で session ごとの揮発 store に畳み、既存の lane snapshot（約 5 秒周期）へ投影する。再起動後の過去履歴から未回答状態を推定しない。
engine の破棄時は対象 session を停止にし、lane の削除・reset 時は進行記録を消す。
pump の世代を照合し、破棄後の遅れた event が旧要求を復活させない。
表示中の lane も snapshot を優先するため、送信直後の状態・`vp now` の反映には最大約 5 秒かかる。
GUI で progress が欠落しても、wire の `working` / `hitl_pending` / `stuck` を会話の「作業中」に変換しない。
live の実行観測がなければ未読または指示待ちとし、TUI の flow fallback は維持する。
`hitl_pending` は lead 回答待ちで会話の「要求中」と混ぜない。ユーザ本人待ちの `awaiting_user` は既存の needs-you 経路で扱う。

実機確認では、非表示 lane の質問発生と回答後の解除、repo を畳んだ時の件数、pin の色、
長い名前・now-line の省略と二行の高さ、tooltip の干渉を確認する。

**Status**: **設計確定（2026-08-19、mako × Claude の一問一答）。実装未着手。**
発端 = mako「サイドバーを抜本的に creo-ui 使ってしっかりとしたサイドバーに生まれ変わらせたい。
見た目の構造から考えたいね」。minimum まで畳み直す議論の到達点。
**Owners**: vp-app（webview sidebar）
**Related**: [54-lane-worker-model.md](./54-lane-worker-model.md)（働き手 = 席を占めるプロセス。
本 doc はその sidebar 投影）/ [56-edge-rail.md](./56-edge-rail.md)（+ New 一本化 — 名簿から
New 動線を追い出す先）/ [57-actions.md](./57-actions.md)（ACTIONS — 下部 creo 段の住人）/
[50-pane-chrome-and-session-panes.md](./50-pane-chrome-and-session-panes.md)（session = Pane。
名簿の行と main area の Pane が 1:1 になる根拠）
**Artboard**: https://claude.ai/code/artifact/af5aa1ac-e3ae-4447-b26e-182301a3bc6a（最小名簿の 1 枚）

> 各決定に mako の決め言葉を引用してある（facts-over-narrative）。

---

## 1. 原理: cwd が SSOT — 並列開発の「場所」と「進行」を映す

> mako「結局 pwd とか cwd を SSOT にして、並列開発作業の場所と進行を、いかにサイドバーで、
> 表現するか」

sidebar が答える問いは 2 つだけ: **どこで**（場所）**何が動いているか**（進行）。
その根は cwd で、3 段が全部 cwd から機械的に導ける:

```
cwd（SSOT）
 ├ proj    = その cwd を含む registered repo（repos.kdl と突き合わせ）
 ├ place   = repo からの相対 — worktree root なら lane 名（.vp/lanes/sampler → sampler）
 └ session = その場所で動いているプロセス（cc#13）… 進行 = state + now-line
```

address（`<repo>/lane/<name>`）は**場所から導出した配送用の鍵**であって、SSOT ではない。
「lane = address」と場所モデルの噛み合わなさ（mako 指摘）はここで解消した — lane は
宛先である前に**場所**。

### 1.1 lane = 場所 = 並行の単位 / session = 働き手 = 並列の単位

> mako「今だと、lane 内で並列性、Lane 自体は並行性って感じか」

境界線は**場所を共有するかどうか**:

| | lane 間（**並行性**） | lane 内（**並列性**） |
|---|---|---|
| 場所 | 別 cwd（worktree で隔離） | 同じ cwd（相部屋） |
| 共有状態 | 無し — branch も files も別 | working tree・branch を共有 |
| 調整 | 明示的 — wire / PR → nightly merge | 暗黙 — 同じ場所・nudge |

（= message-passing 並行 vs shared-memory 並列。worktree を切れば actor、切らなければ
shared memory）

### 1.2 住人のいない住所は存在しない

> mako「（session が無い lane は）出さないというか存在しない」

lane は容器ではなく**生成の系譜**（repo が worktree で lane を産み、lane が slot で
session を産む）。系譜は生成と一緒にしか生まれないので、「空の lane」は概念として無い。
停止中 repo の ▶ 行が名簿に混在している現状は「産める元の一覧」= New 側の情報の混入。

---

## 2. 名簿: proj › sessions

> mako「名簿は、proj > sessions かな」

- 見出し = **proj**（presence dot + 名前。dnd 並べ替え維持）
- 行 = **session**（= Pane、doc 50。行 click = Pane focus で sidebar と main area が 1:1）
- 場所（lane 名）は行の**ラベル**。直前の行と同じ場所なら 2 行目以降は省く
  （= 省略が「同じ場所 = 並列」を型で示す）

```
vantage-point
  ● main     cc#13  NEEDS YOU
      └ sidebar 名簿の 1 枚目を描画中      ← now-line（進行の本体）
  ● (main)   cc#2   WORKING               ← 相部屋: 場所ラベル省略
bikeboy-ladyland
  ● sampler  cc#1   WORKING
  ● build    cc#1   WORKING
+ 11 projects · all idle                    ← idle proj は畳む
```

場所ラベルの種類の数 = 並行の本数、同じラベルの行数 = 並列の人数。

---

## 3. 引き算の台帳（現 sidebar 4,268 行 → どこへ行くか）

| 今ある物 | 処遇 | 理由 / 備考 |
|---|---|---|
| proj 見出し（presence dot / dnd） | 残す | cwd から導く proj |
| lane 行 | **session 行に** | 1 lane 1 session の間は見た目同じ |
| state dot / state 文字 | 残す | FSM は `laneConnector` 導出のまま。**描画（spine/connector/photon）だけ落とす** |
| agent icon | 残す | session の属性 |
| session title（CC 会話 title） | 2 行目の fallback | ⚠️ 現状 lane address 鍵で 1 本 = 相部屋で破綻。**session 鍵に落とす** |
| `vp-lane-cwd`（「地」） | **昇格 → 場所ラベル** | cwd-SSOT の実体は既にここに居た |
| ★ 開発起点マーカー | 消す | 場所ラベル `main` と二重 |
| `#N`（ピン #N = ⌘N / ⌘ hold l、design 81） | 残す | 操作 affordance、対象外 |
| git meta（dirty / ↑↓） | 残す | 場所の進行 |
| awaiting / canvas unread / mailbox | 残す | 「今どうなっているか」層。3 つで打ち止め |
| 📁 files-btn | 消す | Cmd+F で足りる（非 active lane は行 click → Cmd+F の 2 手。1-click 動線が欲しくなったら context menu に足す） |
| now-line（`vp now`、session 鍵） | **2 行目に新設** | 進行の本体。無ければ session title を薄く |
| header `+` / AddSub の常設「+」 | **残す**（④ の再裁定 2026-08-20） | ④ 初版は rail の + New menu へ一本化したが、mako 裁定で**「産む動詞は親の行に住む」**に確定 — repo を産む + は CURRENTS 行、lane を産む + は repo 見出し、console を足す + New は rail（= doc 56「動詞の級 = 住所」の素直な適用）。左で産んで右（main area）に現れる = 操作の流れが生成の系譜と一致する。空状態の「+ repo を登録」CTA は案内として残す |
| ▶ start repo / 停止中 repo の行 | **留保** — 名簿に残す | 起動口の代替（rail からの停止中 repo 一覧）が未設計。X で代替と言うには X が要る（scope-cut-reachable-states）。edge rail は lane 不在時に帯ごと隠れる制約もあり、別途設計 |
| ACTIONS（BucketList） | **下部 creo 段へ**（§4） | |
| DaemonWidget 11 行 | **下部 machine 帯へ**（§4） | |
| SlimRail（`[`） | 残す | 名簿の縮約形 |
| ContextMenu / WirePanel / LanePicker / ⌘K / delete hint | 残す（overlay） | 形に依らず常時 mount |
| tree spine / connector / photon（Light Grid 演出） | 消す | 場所の包含は見出しで足りる |

決定済みの細目: idle は **proj 単位**で畳む（16 proj active の実測から）。

---

## 4. 下部: machine 帯 / creo 段の 2 段

> mako「daemon と hub と device / creo(actions) サイドバー下部をこう分けたいね」

線は**アーキテクチャの scope 境界**そのまま:

| 段 | 住人 | scope |
|---|---|---|
| **creo 段**（上） | ACTIONS（doc 57）+ Creo ID（auth 状態） | cloud。offline なら段ごと dim = **creo 依存をこの段に隔離**（mako の「Creo がガッツリ絡むのがやだ」への答え — 依存を消すのではなく、名札を付けて閉じ込める） |
| **machine 帯**（最下） | daemon ⚙️（port / version / repos）+ hub（federation）+ devices 🧲（艦隊スイッチ） | machine-local。健康なら緑 dot + 最小の文字だけの 1 行、詳細は click で展開 |

現 DaemonWidget の 11 行 ≈ 340px はこの 2 段（健康時 2〜3 行）に畳まれる。

---

## 5. 実装方針: 見た目が変わらない re-platform

> mako「そう。こんなイメージだね。今と変わらんかw」

絵はほぼ今と同じ = **芯は正しかった**。やることは「引き算 + 土台替え」で、完成判定は
「今と同じに見えるか」（目で parity が取れる）。手書き CSS 4,268 行を creo-ui
（outliner / sidenav / accordion）の上に組み直し、§3 の台帳どおりに消す・移す。

順序: ①台帳の引き算を creo-ui 上で再現（parity）→ ②row=session 化（session title の
session 鍵化 + now-line 2 行目）→ ③下部 2 段 → ④New の edge rail 移設。

## 6. 未決（この doc の外）

- ACTIONS の中身の再設計（doc 57 の家は §4 で決まったが、bucket 構成は据え置き）
- SlimRail の名簿対応（proj 頭文字のまま維持で足りるか）
- lane / link 区別（別 canvas で 3 案あり、未選択）
