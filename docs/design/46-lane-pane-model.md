# doc 46 — Lane Pane Model（lane 内を tiling にする）

> mako 要件（2026-07-21）:
> 1. lane 内のコンソールは**既定で左右に Pane を並べる**
> 2. Pane は**1 クリックでタブエリアに縮小化 / Pane 化**できる
> 3. Pane の**フォーカスが視認でき、移動できる**
>
> doc 44 D5 の「タブ strip を header に昇格」はこの model に**吸収して置き換える**
> （§1.3）。

## 0. 一言で

lane の表示領域を「Act I **か** Act II」の排他から、**N 枚の Pane を並べる tiling**へ。
タブは**Pane の縮小形**であって別 UI ではない。

## 1. 決定事項

### 1.1 Pane に入るもの（4 種）

| kind | 中身 | 追加コスト |
|---|---|---|
| `term` | Act I の xterm（lane の PtySlot） | 無し（既存 `#lane-host` を pane 化） |
| `chat` | Act II の会話 view（session 単位） | 無し（既存 `#console-chat-host` を pane 化） |
| `canvas` | PP board / Canvas | **中**（見積もり訂正、§6） |
| `file` | file view / editor | 後続 |

⚠️ **語の衝突**: VP には既に PP Canvas の「pane」がある（`pane_contents` table /
`.vp-pane` / `vp pane`）。本 doc の Pane は**その上位概念**で、Canvas pane は
`kind: canvas` の Pane として内包される。既存の table 名・CLI は変えない。

### 1.2 「左右並列」に PtySlot の re-key は要らない

現状 `pty_slots: HashMap<LaneAddress, Mutex<PtySlot>>` = **lane に 1 slot**。
ここから素直に読むと「端末を並べるには `(lane, session)` へ re-key が要る」に見えるが、
**要件 1〜3 はそれ無しで成立する**:

- 今の排他は「Act I **か** Act II」であって、`term` 1 枚 + `chat` 1 枚 + `canvas` 1 枚を
  並べるのに slot は 1 本で足りる
- re-key が要るのは「**端末を 2 枚以上**」だけ

→ **要件 1〜3 は server 無改造**。端末の複数枚化は独立した後続 phase（§3）。

> 「並列表示 = 実体も複数要る」と早合点すると、server の大工事を前提にしてしまう。
> 並べたい**種類**と、同じ種類を**何枚**か、は別の問い。

### 1.3 タブは Pane の状態であって別 UI ではない

要件 2 の帰結。Pane は `docked`（並んでいる）/ `minimized`（タブエリアに畳まれている）の
2 状態を持ち、1 クリックで往復する。

これにより doc 44 D5 の宿題（「タブ strip を header 層へ昇格」）は**別実装を作らずに解ける** —
タブエリアが header 層に 1 つあり、そこに全 kind の minimized Pane が並ぶ。
D5 が悩んでいた「Act I でタブを押したら何が起きるか」（root 切替 = 重い respawn か、
注視の移動か）は、**Pane の復元**という 1 つの意味に畳まれて消える。

### 1.4 Act は lane の mode ではなく **Pane の kind** になる

要件 4「新コンソールは **Engine と Act を選んで**作成」の帰結。

現状 Act は **lane 単位の mode**（`console_mode` = `tui` | `chat`、per-lane に永続）で、
lane 全体がどちらかに切り替わる。Pane model ではこれが成り立たない — 同じ lane に
`term` Pane と `chat` Pane が**同時に並ぶ**から。

→ **Act = Pane の kind**（`term` = Act I / `chat` = Act II）。lane は Act を持たない。

これで doc 44 D5 が抱えていた曖昧さ（「Act I でタブを押したら root 切替か注視の移動か」）が
**問いごと消える**: Pane を作る時に kind を選び、以後その Pane は最後までその kind。

⚠️ **移行の影響**: `console_mode`（per-lane 永続 / `console:set_mode` / `#console-switching`
の切替 overlay / `vp:console-mode` bus）は Pane model では意味を失う。P1 では
**既存の lane mode を「初期 Pane 構成」に写して残置**し、撤去は P4（§2）。
いきなり消すと Act 切替の全経路（doc 33 / doc 38 の資産）が同時に壊れる。

### 1.5 Pane は必ず新しい session id で始まる

要件 5。新しい console（Pane）を作ると **新しい session id を発行**し、その lane の cwd から
fresh に始める。既存 session の再表示ではない。

- 「同じ会話を 2 枚出す」は**しない** — 会話は 1 つの session に属し、2 枚出しても
  どちらが真かが曖昧になる（doc 38 の focused 排他が壊れる）
- 既存 session を見たい場合は **minimized な Pane を復元する**（§1.3）。
  つまり session ↔ Pane は **1:1**

> 「新規作成」と「既存を開く」を 1 つの操作に混ぜない。混ぜると
> 「+ を押したら前の会話が出た / 出なかった」が状況依存になる。

### 1.6 フォーカスは Pane に付く

「どの Pane がキー入力を受けるか」を Pane 単位で持ち、視認可能にする（枠 / 縁）。
移動は click と keyboard の両方。

`term` Pane が focus を持つ時だけ xterm がキーを取る — 今は Act I が全面なので
「表示 = 入力先」だったが、並列になると**表示と入力先が分離**する。

## 2. Phase

| phase | 内容 | server 変更 |
|---|---|---|
| **P1** | Pane shell（並列 / 縮小・復元 / focus 視認・移動）+ `term` `chat` を載せる | 無し |
| **P2** | 新 Pane 作成 UI（**Engine × Act** を選ぶ、要件 4）+ 新 session 発行（要件 5） | 小 |
| **P3** | `canvas` を Pane に寄せる | 無し（既存 board を mount するだけ） |
| **P4** | `console_mode`（per-lane Act）の撤去 — §1.4 の移行完了 | 中 |
| **P5** | 端末の複数枚化（`pty_slots` を `(lane, session)` へ re-key） | **大** |
| **P6** | `file` kind / layout 永続 | 小 |

P1 と P2 の順が逆でないのは、**作る前に置き場が要る**から。P1 の時点では
既存 lane の Act を初期 Pane 構成に写して並べる（新規作成は既存の「+」のまま）。

## 3. P5（端末の複数枚化）が重い理由（先に測っておく）

> 訂正（2026-07-21）: 見出しは phase 振り直し前の「P3」のままだった。本節が測っているのは
> **P5（`pty_slots` re-key）**。canvas の P3 は §6 で別途訂正済み。

`pty_slots` の key を変えると、**lane key を前提にした全経路**が影響を受ける:
spawn / pump / `lane_capture` / `deliver_nudge` / Dead 検出 / zombie reap。
doc 44 §11 で見たとおり、この層は「1 つの辺が 2 つの仕事をしている」箇所が残っており、
key 変更は同型の見落としを生みやすい。**P1/P2 を出してから単独で扱う。**

## 4. 実装メモ

### 4.1 レイアウトの現状（P1 の改造対象）

```
#pane-terminal
├── #echoes-header       (30px、両 Act 共通の既存 header)
├── #lane-host           ← Act I: .lane-pane > .lane-term（display 切替）
├── #console-chat-host   ← Act II: ChatView（.active の時だけ display:block）
└── #lane-empty
```

`#lane-host` と `#console-chat-host` は **absolute で全面を占め、`display` で排他**。
P1 はこれを flex row の Pane container に置き換え、両者を Pane の中身にする。

### 4.2 layout の真実源

P1 は **in-memory（webview 側）**。永続は P4。
理由: 並べ方は「今この瞬間の作業の形」で、lane や project より寿命が短い。
先に永続すると「復元されるべきか」の判断（lane を切り替えたら？ 再起動したら？）が
実際に触る前に決まってしまう。dogfood してから決める。

## 5. P2 実装メモ — Engine × Act で新コンソール（2026-07-21）

### 5.1 既存 IPC を拡張し、新設しない

`console:new_session` は元々 **lane の Act と現 focused の engine を継承**していた
（doc 39 §4「New は今いる Act に出す」）。要件 4 はこれを**明示選択**にする話なので、
新 IPC を足さず `engine` / `act` を **optional** で受けるよう拡張した。

- 省略時は従来どおり継承 → 既存の呼び手（header の ✨ New 等）は無改造
- 明示指定があれば `echoes_session_list` の往復ごと省ける（engine を引く必要が無い）
- 未知の `act` は**継承に倒す** — 「指定したのに黙って別の Act で作られた」より
  「指定が効かなかった」方が気付きやすい

### 5.2 chat 非対応 engine に Act II を出さない

`newPaneChoices` は `chat_capable` が false の engine から chat の選択肢を落とす。
出すと「作れるが submit がエラーになるだけ」の行き止まり Pane になる
（doc 38 Phase 3 が tab の「+」で同じ判断をしている）。
`chat_capable` 未指定も非対応扱い — **不明なら行き止まりを作らない側**に倒す。

Act I（tui）は login shell に流し込むだけなのでどの engine でも成立する。

### 5.3 タブエリアは常時表示に変えた

「+ New」を常に載せるので `--pane-tabs-h` は 26px 固定。`.pane-tabs-active` は
「畳まれた Pane が 1 つ以上ある」= **区切り線を出すかどうか**にだけ効く形へ意味を絞った
（class を無意味に残すと[読み手のない書き込み]と同じ形になる）。

## 6. P3（canvas）の見積もり訂正（2026-07-21）

§1.1 は「canvas は既に pane 概念があるので**寄せるだけ**」としていたが、**誤り**。

`#pane-paisley-park` は `#pane-terminal` の**兄弟にあたるトップレベル pane** で、
**Frame Engine（3D Scene システム）が transform で配置している**:

```ts
const FRAME_PANE_IDS = ["echoes", "pp", "ge", "hp", "preview", "empty"]
generateAllFocusScenes(FOCUSABLE_PANE_IDS)  // pp を含む全 focus scene
```

P3 は「PP を Frame Engine の Scene 管理から外し、lane の flex row に入れる」ことになる。
Scene 定義 / keybindings（Cmd+Shift+N の layout 切替）/ per-lane Scene 記憶が
すべて `pp` を参照しており、**P1/P2（既存 host に class を付けるだけ）とは質が違う**。

> **「既に pane と呼ばれている」ことと「同じ tiling に載せられる」ことは別。**
> §1.1 で語の衝突（PP の pane と本 doc の Pane）を注記していたのに、
> **見積もりの方には反映できていなかった** — 語の衝突は工数見積もりにも効く。

### 着手前に決めること

1. Frame Engine の Scene と lane 内 tiling の**関係**（Scene が Pane 構成を決めるのか、
   tiling が Scene の下に入るのか、Scene 自体を tiling に置き換えるのか）
2. GE / HP / preview も同じ扱いにするのか（`pp` だけ特別扱いすると語彙が割れる）

Phase 表の順序も見直す価値がある — **P4（`console_mode` 撤去）を先に**やると
「Act = Pane の kind」が完成し、P3 の設計判断（1）の材料が増える。

> **決着（2026-07-21）**: 順序は **doc 47 §7 が SSOT**（Epic 全体を 1 本に並べたもの）。
> P4 → P5 を内部フェーズで先に済ませ、**P3 を含む UI は最後**。
> しかも設計判断（1）は「Scene と tiling の関係」ではなく、
> **GUI LayoutEngine を作り直してそこに載せる**という形で解く（doc 47 §1）。
