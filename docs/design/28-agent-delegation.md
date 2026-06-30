# 28 — 場の上の agent 協働（ask + tell）

> doc 27 §3.4「場 × 動詞 × 規律」の延長。場の上で agent が互いに仕事を頼み（ask）・知らせる（tell）ための
> messaging 設計。trace: creo `mem_1CcSQ6sxFPtm2mZw873VRp`。親: Transport 哲学 `mem_1CcRw6kSu9Jr3ejhZ4ALUJ`。

## 0. 一行で

agent 同士が**場**を通して協働する。芯は **2-party の委譲（A が B に頼み、完了で block が解けて再開）を
人間の介入なしで自走**させること。同じ substrate に tell（broadcast）と federation（別 World / 別マシン）が乗る。

## 1. 起点 — dogfood の分布（観測は道具の天井）

- **8〜9 割 = 2-agent 委譲**: A が実装中に追加実装 D に気づき B に頼む → 完了報告 → block 解除 → 再開。
- **1〜2/10 = 3 対**: 独立 2 件の fan-out / 連鎖(A→B→C) / 2 doer 分担。すべて**委譲の合成**。
- **観測ゼロ ≠ 需要ゼロ**: 「3 人で議論して決める」は現 wire では**不可能だった**から出ていないだけ（潜在需要）。
  観測は「今の道具が許す範囲」しか映さない —— 設計の天井をそこに置かない。

**司会を人間から World へ。** 今この orchestration（伝える・起こす・見張る・再開させる）を回しているのは
user 本人。その state machine を World に移すのが「介入なし」の核。

## 2. 唯一のプリミティブ — lifecycle を持つ「場」

**場 = 名前のついた番地（World 裏付け）。1 つの primitive で全部やる。**

- **召集**で参加者を入れ、**投下**（ask / tell）で中身を入れ、参加者は場を **observe** する。
- **1 対 1 = doer 1 人の場、N 対 = 参加者を増やした場。違いは人数だけで別物ではない。** スケール=「もう 1 召集」。
- 番地は**相手（agent）でなく仕事 / 議題**。`agent@B` を ask するのでなく、仕事 D の場に B を召集する
  （「相手が増えたら？」が「召集を増やすだけ」に落ちる）。
- **surface は単純なまま**: `delegate(doer, task)` は directed に見え、場は暗黙に自動生成（doer 1 人）。
  複雑さは「N 人欲しくなった時」まで隠れる。
- doc 27 §3.4.2 の軸では **agent 協働 = (topic × ask) と (topic × tell)**。`(direct × ask)` は infra RPC
  （`process/*` の SP に state を聞く reverse-route）に残す —— **infra は direct、協働は場**。

**不変条件**（最初から焼き込む。後で拡張を解放するため）:
1. **参加者は対称（peer）**: requester / doer は役でなく振る舞い。どの agent も頼める / 頼まれる / 議論できる
   （連鎖と discussion がここから解放可能になる）。
2. **requester は複数 outstanding を持てる**: 同時に N 件 await（fan-out）。

## 3. 設計の芯 — 受け手（agent）の生理から来る規律

messaging を「走るプロセスへの配送」でなく、**受け手の生理**から設計する。agent の現実: **ターンの間は
存在しない（background で待てない）/ context は有限で compaction で消える / 非決定的 / 起こす = 1 ターン =
コスト + 中断**。ここから 6 つの規律:

1. **wake が単位。「条件への interest → 充足で起こす」に統一**。block-await / broadcast 購読 / event 待ち は
   全部 conditional wake の特殊例。
2. **wake は高価 → 規律 tier**: `interrupt-now`（await 中の block-clearing 完了）/ `at-next-turn`（自然な
   境界で surface = pull-hook、FYI はここ）/ `ambient`（push せず場に retained、見れば在る）。送り手 or 種別が
   tier を宣言。受け手は **attention 状態**（潜行中 / 決定点 / idle / blocked）を publish できる。
   presence = online/offline でなく **availability**。
3. **メッセージ = 再開可能な continuation**。context を失っていても単体で行動できる packet（何が起きた /
   何が期待 / 最小文脈 / 続きの在処）。委譲完了は「D を B に頼んだ→Done、結果 R、元タスク T 再開」まで
   自己完結で再注入。chat 行でなく suspend/resume の継続フレーム。
4. **場は head-state（現在要旨）を保つ**: 決定 / 未解決の問い / 担当。join・再開は digest を読み、深掘りが要る
   時だけ全ログへ。途中召集でも context が溶けない（event log + materialized view、World canonical）。
5. **ask は response schema を同梱、Outcome は三相**: 返答型が来ると返答が machine-checkable・await が決定論的。
   nostos の **Done / Reborn / Failed** —— Reborn（進んだがもう 1 周）が agent の現実、Failed は「死」でなく
   **会話の 1 手**（交渉・再スコープ）。
6. **能力指名**（late binding）: 「DB schema 分かる奴」「Rust review できる奴」に宛て、場 / registry が能力で
   束ねる（居なければ spawn）。相手の名前を知らなくていい（fleet が増えても効く・post-v1）。

> reasoning は Outcome に従属する任意 stream として流せる（観測者が仕事を追える = 信頼 + Pane 可視化）。
> first-class は Outcome、reasoning は畳める付随物。

## 4. atom — 2-party 委譲（durable cross-agent future）

8〜9 割を担う芯。A が B を ask、応答（完了）はずっと後 = **async future**。A の block = その future を await。

**agent の block はスレッド block でない**: A は delegate した状態で**ターンを終え（park）**、World が
**session を再 invoke して Outcome 注入（wake）**で再開。スレッドでなくターン境界の park + event wake。

**動詞 3 つ**（+ §3-1 の pull 保険）:

| 動詞 | 主体 | 中身 |
|---|---|---|
| `delegate(doer, task) → id` | A | 場作成 → B を wake → A の await 登録 → A park |
| `complete(id, outcome)` | B | Outcome 確定 → A を wake（Done / Reborn / Failed） |
| `respond(id, answer)` | A | NeedsInput(=Reborn) に回答 → B を再 wake |

**state machine = nostos Bracket + 三相 Outcome**（club-nostos 0.2.0 で async 済 = `AsyncBracket` /
`drive_bounded_async` / `AsyncDriver`、`Outcome` / `Voyage` は sync/async 共通データ）。委譲が**足す**のは
**durable / distributed 化**（Active を World に永続、exit が別プロセス / 別 World から来る）。`drive_bounded`
の timeout が doer 沈黙 / 死を `Failed{timeout}` に落とす。

**reliability = Push + Pull 調停**（process 管理と同じ DNA。autonomy の本丸は edge の self-heal）:

| 失敗 | self-heal |
|---|---|
| wake 取りこぼし | await / outcome は durable、reconcile loop が再 nudge |
| doer 沈黙 / 死 | `drive_bounded` timeout → `Failed{timeout}` で A wake |
| requester stall | 場グラフを Canvas に → user は介入でなく観測 |

**wire との関係**: 委譲 = wire thread の特殊形（delegate=root / complete=reply）。**wire は基板（World 中央
store）のまま**、欠けていた ① typed Outcome ② 遷移ごとの wake state machine を 1 枚乗せるだけ。

## 5. 合成と拡張（staged）

すべて同じ場 primitive の上に乗る。フル設計は各々を作る時。

### 5.1 3 対の合成
- **fan-out**（独立 2 件）= requester が future を複数持つだけ。**v1**（不変条件 2）。
- **連鎖 A→B→C** = doer が自分も delegate（再帰）。**v1**（不変条件 1 = 対称性だけで落ちる）。
- **shared 場**（2 doer 分担）= 1 場に co-doer N + **集約 Outcome**（全 Done→Done / どれか Failed→Failed{誰}）。
  **v1.5**（唯一の新ルール）。
- **discussion**（N peer が議論して決める）= 道具が無くて未発現の**潜在需要**。場が解放する（peer 対称 +
  `Decided{結論}` Outcome + turn-taking）。**enabled-later** —— substrate は v1 から peer 対称に作り、解放は
  機能追加（司会 / turn-taking のメカニクスは解放時に設計、今は書かない）。

### 5.2 場の tell 側 — broadcast / dependency bus
ask（委譲）の対 = **tell**（撃ちっぱなし、受け手が自律反応）。例「nostos 0.3.0 出た」→ 依存先が各自判断して
アプデ開始。
- **「誰が必要か」は構造で解く**: subscription = 依存宣言。`release/nostos` を observe するのは実依存 project
  だけ（**Cargo.toml deps から自動 subscribe**）。各 agent の判断は「いつアプデするか（timing）」に絞られる。
- **tell → ask 連鎖**: broadcast(tell) → 自律反応 → 必要なら「アプデして」を delegate(ask)。= dependency bus。
- **後段**（federation + event feed の上）。

### 5.3 location-transparent federation（3 台 / LAN 超え / 別 World）
**動詞・state machine・Outcome は不変、変わるのは番地解決だけ。** `studio-pc:agent@vp/wing` を hub が解決。
- authority = 双方が共に到達できる最小合流点（same World ⇒ World、別 World ⇒ **chronista-hub**）。
- **制御面 = chronista-hub**（registry / rendezvous / offline buffer、Creo ID scope）/ **データ面 =
  World-to-World QUIC**（direct、不可なら relay）。NAT / relay の実装詳細は doc 27 §8.3 と一緒に実装時。
- **v2 = LAN（direct）/ v3 = WAN（relay + offline buffer）**。

### 5.4 自己ホスト flywheel
委譲システムは委譲システム自身で作る。**v1 ローカル → それで VP↔chronista-hub を回し v2 を建てる →
自己ホスト。** acceptance test = 「自分の cross-project 開発を司会できるか」。

## 6. 既存 VP 部品への載せ方（新規は薄い）

| 必要物 | 既存 |
|---|---|
| 永続 store / thread | **wire store**（World 中央）に typed record |
| 状態 / presence | **flow.rs `WorkState`** を一般化 → hand-rolled を畳む |
| wake | **nudge / tmux send-keys**（撃つ相手 / tier を state machine が決める） |
| pull 保険 | **wire hook**（未読注入）→ await-resolved + event 注入に拡張 |
| Outcome / Bracket / timeout | **club-nostos 0.2.0** |
| 観測 | **Canvas** に場グラフ + reasoning stream（Pane 可視化 want） |

新規 = 場 state machine + 動詞 + reconcile loop。残りは結線。

## 7. 段階

- **v1 = atom**（2-party 委譲: park/wake / 三相 Outcome / reliability）+ **不変条件**（peer 対称 / 複数 future）。
  → 8〜9 割 + fan-out + 連鎖。**§3 の生理規律を最初から焼く**（wake tier / continuation / head-state）。
- **v1.5 = shared 場**（集約 Outcome）。
- **v2 / v3 = federation**（LAN → WAN）。その上に **broadcast / dependency bus**、**discussion** 解放、**能力指名**。
- spike 先行: park/wake は実機 send-keys タイミングで初めて綻ぶ（terminal 狭幅 bug の構図）。最小往復を 1 回
  通してから固める。

## 8. 未解決 / 次

1. **wake の実 transport**（push の再送 / idempotency: id + delivered-once flag）。
2. **head-state digest の更新規律**（誰が・いつ要約を更新するか）。
3. **conditional-wake の condition 表現**（delegation 完了 / topic publish / 外部 event を 1 つの interest 語彙へ）。
4. **集約 Outcome の partial**（shared 場で一部 Failed の扱い）。
5. **chronista-hub の現状棚卸し**（registry / rendezvous / relay / offline buffer をどこまで持つか）。
6. **認証 scope**（自分の Creo ID fleet 内に限定、`vp auth`）。
