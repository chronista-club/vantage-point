# doc 53 — lane の中を reconcile で合わせる（intent と実体の 2 層）

**Status**: **起草**（2026-07-25、mako の問い「一度落ち着いて、構造や仕様を見直すってものありやけど」から）。
**実装未着手**。設計の入力 = doc 50 §4.7 の A6 実装記録（17 件の実バグ）。
**Owners**: vantage-point（server: `lanes_state` / `unison_server`）+ vp-app（client の roster 導出）
**Related**: [50-pane-chrome-and-session-panes.md](./50-pane-chrome-and-session-panes.md)（§4.7 = 本 doc の証拠）、
[51-zero-base-review.md](./51-zero-base-review.md)（作業台の原理）、
[46-lane-pane-model.md](./46-lane-pane-model.md)（session ↔ Pane 1:1）、
CLAUDE.md「プロセス管理（Reconciliation）」（**本 doc が再適用する原理の出典**）

> ⚠️ **これは A6 の続きではなく、A6 が露出させた構造への応答**。A6（xterm re-key、doc 50 P3）は
> 機能として完成し実機で検証済み。本 doc は「その過程で 17 回叩いた 1 つの性質」を扱う。

---

## 1. Why — 17 個のバグは 1 つの構造的性質の顕れだった

A6 で team-b レビューを **10 周**回し、**毎周新規の機能バグが出た**（合計 17 件。静的発見 3 件 +
実機 dogfood 6 件を含めると 26 件）。周を重ねるほど「探索が深くなった」と読んでいたが、
mako の問いで見直すと別の読み方ができる — **同じ性質を 17 回叩いていた**。

### 1.1 何と何が乖離したのか

VP には **intent（何であるべきか）** と **実体（今どうなっているか）** の 2 層がある:

| 層 | 何が | どこに |
|---|---|---|
| **intent** | lane に session が何枚あり、各 session の act は何か、誰が root か | `session_registry`（disk、SSOT） |
| **実体** | その session に PtySlot / chat engine / pump / replay file があるか | `LanePool` の各 Map + disk の replay file |

そして両者を合わせる作業が、**手書きの遷移として 5 箇所に散っている**:

| 契機 | 手書きの遷移 | 合わせる対象 |
|---|---|---|
| boot / World 再起動 | `restore_term_slots` | slot（非 root の Tui のみ） |
| act 切替 | `set_session_act` → `restart_lane` or `open_slot_for_session` → pump | slot + engine + pump + 投影 |
| Add（slot 追加） | `open_new_slot` → pump | slot + pump |
| ✕（session を閉じる） | `remove_chat_session` → `drop_slot` + `replay_log::clear` + `clear_replay_session` | engine + slot + replay×2 |
| Reset | `clear_fresh_lane_state` + 全 slot drop + `clear_replay_in` + pump | 全部 |

**17 個の大半は「この 5 つのどれかが、合わせるべきものを 1 つ忘れた」形**だった。

### 1.2 証拠 — 各バグがどの乖離だったか

| # | 何を忘れたか | 症状 |
|---|---|---|
| 1 | Add が **pump** を忘れた | 入力は通るのに**出力が永久に沈黙** |
| 2 | replay の gate が lane 単位だった | 非 root chat に replay が来ない |
| 3 | boot が**非 root の slot** を忘れた | 再起動後に pane が空で無反応 |
| 4 | root 付け替えが **`console_mode` 投影**を忘れた | PTY 不発 or **1 会話 2 engine** |
| 5 | Reset が **term の replay file** を忘れた | ghost replay（消したはずの画面が蘇る） |
| 6 | replay file の身元が **role** に紐づいていた | 付け替えで**他 session の画面**が出る |
| 7 | xterm host の身元が **role** に紐づいていた | DOM 位置と focus が旧 root に残留 |
| 8 | `act → host id` の写像が **4 箇所**に散り 2 箇所が古かった | focus ring が別 pane に誤爆 |
| 9 | 書き手が**自分の読み手 cache** を忘れた | ink が畳まれた PtySlot へ送って**黙って消える** |
| 10 | pump の張り直しが **lane 全体**だった（粒度不一致） | 1 枚触ると隣の pane が clear + 全 replay |

**#6 / #7 だけが別軸**（識別子を role で決める vs identity で決める）で、これは A6 内で
identity 化して解決済み。**残り 8 件は「intent と実体を合わせ忘れた」形**。

### 1.3 同じ真実が 10 箇所に表現されている

client 側まで含めると、「lane の session 一覧 × 各 act」は **10 箇所**に表現されている:

| # | 表現 | 供給元 |
|---|---|---|
| 1 | `session_registry`（disk） | **SSOT** |
| 2 | `LaneInfo.console_mode` | 1 の投影（root の act だけ） |
| 3 | `LaneInfo.sessions`（`LaneSessionsWire`） | 1 の wire 投影 |
| 4 | `sidebar_state.lanes_by_project` | 3 の写し（vp-app Rust） |
| 5 | `console.ts` `laneSessions` | **別 RPC**（`echoes_session_list`） |
| 6 | `lane-panes.ts` `sessionsByLane` | event bus |
| 7 | `EchoesHeader.tsx` `sessions` signal | 同じ event bus |
| 8 | World A `laneInstances` + `isRootHost` | 命令（`ensureLane`） |
| 9 | `terminal_pumps` | slot の存在から |
| 10 | `pty_slots` / `chat_engines` | **実体** |

**供給路が 3 本**ある（lanes snapshot の push / `echoes_session_list` の RPC / event bus）。
#9 は 1 と 3 の乖離、#4 は 1 と 2 の乖離、#8 は 6 と 7 が同じ写像を別々に持っていたこと。

---

## 2. 原理 — process 層で既に使っているものを 1 段下へ

**新しい思想を持ち込むのではない。** CLAUDE.md「プロセス管理（Reconciliation）」が既に宣言している:

> TheWorld が **QUIC registry（Push）** でプロセスを管理する。… registry が**単一の真実源**になった。
> heartbeat 15s + 再接続時の snapshot replace で reconcile。

**process** に対しては reconcile を採っているのに、**lane 内の session** に対しては手書きの遷移を
書いていた。それが §1 の構造。

### 2.1 reconcile_lane

```
reconcile_lane(addr):
    intent  = session_registry::load(addr)          # SSOT を読む
    live    = { pty_slots, chat_engines, terminal_pumps, replay files }

    # 差分を計算して合わせる（順序は「消す → 立てる」= 法の違反を作らない）
    for session in live - intent:                   # intent から消えた
        drop engine / slot / pump、replay file を掃除
    for session in intent where act=Tui and no slot:
        slot を立てる（その session の stand / conversation で）
    for session in intent where act=Chat and slot exists:
        slot を畳む（1 session = 高々 1 engine の法）
    for live term slot without pump, if 購読者が居る:
        pump を張る
    for pump without live slot:
        abort
```

**全動詞が「registry に書いて reconcile を呼ぶ」になる**:

| 動詞 | いま | reconcile 後 |
|---|---|---|
| act 切替 | registry + restart/open + pump + 投影更新 | registry に書く → reconcile |
| Add | registry + slot + pump | registry に書く → reconcile |
| ✕ | registry から消す + drop + replay×2 + engine | registry から消す → reconcile |
| Reset | registry clear + 全 drop + 掃除 + pump | registry を clear → reconcile |
| boot / World 再起動 | `restore_term_slots` | reconcile |
| root 付け替え | registry + restart + 投影更新 | registry に書く → reconcile |

### 2.2 なぜこれで 8 件が構造的に消えるか

| # | reconcile 後 |
|---|---|
| 1 | 「pump が無い term slot」を reconcile が拾う → **呼び忘れる場所が無い** |
| 2 | reconcile は session ごとに判定 → lane 単位の gate が存在しない |
| 3 | boot = reconcile。「intent に居るのに slot が無い」を必ず埋める |
| 4 | `console_mode` を廃止（§3.1）→ 投影が無いので乖離しない |
| 5 | 「intent から消えた session の後始末」に replay file を含める（1 箇所） |
| 8 | server が pane 一覧を配る（§3.2）→ client 側の写像が消える |
| 9 | client の cache を 1 本に（§3.2）→ 書き手が更新すべき読み手が 1 つ |
| 10 | reconcile が差分を計算 → 「どの範囲で呼ぶか」を呼び手が決めない |

### 2.3 edge → level（demand の問題も同じ形）

pump の起動契機がいま **「購読者数 0→1 の edge」** なので、**寿命の違う 2 プロセス**（daemon は
生き続け、GUI は死んで生まれ直す）の間で signal が落ちる — GUI だけ再起動すると server 側に
stale な subscriber が残り、edge が立たず replay が来ない（実機で確認済み。daemon 再起動で復帰）。

reconcile は **level** で判断する（「**今**購読者が居て pump が無いなら張る」）ので、この問題は
原理的に消える。**edge → level は reconcile の本質そのもの**。

> ⚠️ 一般形として: **寿命の違う 2 者の間で「変化」を signal にしてはいけない**。短命側の
> 再起動が長命側に見えない。level（現在値）で判断すれば収束する。

### 2.4 原理との接続 — reconcile は「表象の共有」の実装形

doc 51 が VP の原理として **「表象の共有」** を置いた。本 doc の機構はその実装形と読める:

> **表象を共有するには、表象が 1 つでなければならない。**

§1.3 の「同じ真実が 10 箇所」は、機構の不備である以前に**原理違反**だった。10 個あるものは
共有できない — どれを見ているかで答えが違うから。SSOT + reconcile は「1 つの表象を全員が見る」
ための最小の形であって、余計な抽象ではない。

同じ読み方が **doc 44 の process 層**にも当てはまる（registry が単一の真実源 → 全 client が
同じ表象を見る）。**VP は原理を一度実装していて、1 段下で忘れていた**。

### 2.5 lane が持つもの / session が持つもの — 15 例の最も一般的な収穫

A6 で「**lane 単位の述語はすべて誤った要約になる**」と分かった（doc 50 §4.7、15 例）。
では lane はまだ意味のある単位なのか。**ある**。ただし持つものが違う:

| lane が持つ | session が持つ |
|---|---|
| cwd / branch / repo（作業の場所） | act（見え方） |
| mailbox `agent@<lane>`（誰に宛てるか）※ 主語は root | engine / stand（投げる先） |
| board（貼る台。lane-scoped で 1 枚） | conversation id（会話の在処） |
| layout scope（`lane:<addr>` の tiling） | PtySlot / chat engine / pump / replay（実体） |
| 代表 = root（**誰が lane を名乗るか**） | 自分の識別子（`__<session>` の付く全て） |

**線引きの基準**: 「lane を複数の session で共有していても答えが 1 つに決まるか」。
cwd は決まる（同じ場所で働く）。act は決まらない（session ごとに違う）。**決まらないものを
lane に置くと誤った要約になる** — これが 15 例の共通形。

> ⚠️ `console_mode`（root の act の投影）は「決まらないものを lane に置いた」形だった。
> §3.1 で廃止する根拠はここにある。逆に board を lane に置いているのは正しい（貼る台は
> 場所に属する。doc 52）。
>
> ⚠️ **root は「lane が持つもの」の側**（誰が代表か）で、**act とは直交**する。A6 で
> tui 限定 gate を撤去したのはこの直交性の帰結（doc 50 §4.7）。

---

## 3. 仕様側で消えるもの

### 3.1 `console_mode`（lane 単位 mode の投影）を廃止する

`LaneInfo.console_mode` は「root session の act の投影」でしかない。読み手は 8 箇所以上あるが、
**それぞれ別の問いを持っている**:

| 読み手 | 本当に訊きたいこと |
|---|---|
| `delivery_actor`（nudge） | root slot が存在するか（PTY に打てるか） |
| `restart_lane` | root session の act（PTY を立てるか engine を畳むか） |
| boot（`lane_spawn_actor`） | **各 session** の act（PTY を立てるか） |
| `ensure_chat_engine` | **その session** の act（A6 で `resolved.act` に修正済） |
| wire snapshot | 表示用の要約 |

1 つの投影が 5 つの別の問いを兼ねている（[[one-predicate-three-properties]] と同型）。
**registry を直読みすれば済む**（`resolve_chat_session` が既にそうしている。disk 読みだが
restart / nudge は頻度が低い）。投影を消せば #4 の class が消え、`sync_root_act_projection`
（A6 で足した helper）も要らなくなる。

### 3.2 client 側の roster 導出を廃止する

server は既に **「どの pane が存在すべきか」を知っている** — `pty_slots` と `chat_engines` が
それそのもの。いま client には「session 一覧 + 各 act」という**生の事実**を配り、client が
3 つの cache でそれぞれ導出している。

**pane 一覧をそのまま配る**（`{id, kind, label, session, root}` の列）:

- `act → host id` の写像が client から消える（#8）
- cache が 1 本になる（#9 の書き手/読み手の対が 1 対 1 に）
- **供給路も 1 本にできる** — lanes snapshot は既に `sessions` を運んでいる（A6 で追加）。
  `echoes_session_list` を client API から退役（CLI には残す）すれば、boot 窓の取りこぼし
  （A6 で保留箱を足して救済した）も**構造的に不要**になる（push は retained）

> 前提: snapshot が **registry の変化ごとに飛ぶ**こと。いまは 5s periodic + 個別 event なので、
> **registry を change stream にする**のが本 doc の実装の核。

### 3.3 派生値を cache に持たない

#4（`console_mode`）と #9（`laneSessions` の act）と #7（`isRootHost`）は、どれも
**派生値を保存していたこと**が原因。規律として:

- **act → host id / kind** は関数（`hostIdForAct`、A6 で 1 本化済）
- **root → focus 優先** は命令の引数で運ぶ（World A に `isRootHost` を持たせない）
- **root の act** は registry から引く（投影を持たない = §3.1）

---

## 4. コスト / 反論

| 論点 | 評価 |
|---|---|
| reconcile は全読みで重い | lane 内の session は数枚。`session_registry::load` は小さな JSON 1 本。restart / 切替は人間の操作頻度。**実質無視できる** |
| いつ呼ぶか | 書き込みの直後（動詞の末尾）+ boot。**定期 tick は要らない**（intent が変わるのは動詞のときだけ）。ただし「購読者の増減」も level 判断の入力なので、demand hook からも呼ぶ |
| 部分更新の方が速い | 速いが、**忘れる**。17 件がその証拠。速度が問題になってから最適化する |
| 800ms spawn を reconcile が直列にやると遅い | 既に `spawn_blocking` 隔離済（A6 の 4 周目）。reconcile も同じ規律に載せる。N 枚立てる場合は並列化の余地あり |
| registry が壊れたら実体を全部畳んでしまう | `is_valid()` 失敗時は `single()` に倒れる = 「root 1 枚」を intent と誤認して他を畳む危険。**reconcile は「読めなかった」と「0 件だった」を区別する**必要がある（[[masked-not-absent]] の同型。`Unknown` と `Known(空)` を分ける — `vp lane cleanup` の liveness 注入と同じ設計） |

---

## 5. Phase（案）

独立して効く順に切る。**それぞれ単体で価値があり、途中で止めても中間状態が残らない**ことを条件にした。

| Phase | scope | 消えるバグ class | 規模 |
|---|---|---|---|
| **R1** | **`console_mode` 廃止** — 読み手 8 箇所をそれぞれの問いに置き換え、投影と `sync_root_act_projection` を撤去 | #4 | 中 |
| **R2** | **pump を reconcile 化** — 「生きた term slot に pump が 1 本」を level で保つ。`respawn_terminal_pump` の `only` 引数と呼び手 4 箇所の判断が消える | #1 / #10 / demand edge / **#11（下記）** | 中 |
| **R3** | **slot / engine を reconcile 化** — 5 つの手書き遷移を `reconcile_lane` 1 本に畳む。動詞は registry に書くだけ | #3 / #5 | 大 |
| **R4** | **pane 一覧を配る** — server が導出、client は描くだけ。`echoes_session_list` を client API から退役、cache 1 本化 | #8 / #9 / boot 窓 | 中〜大 |

> **R2 は既知バグを 1 件抱えている**（doc 50 §4.7「直さないと決めた 1 件」）: boot 復元
> （`restore_term_slots`、800ms×N の逐次）が進む間に demand の 0→1 edge が先に立ち、**後から
> 復元された slot に pump が張られない**。A6 の 11 周目で発見し、**patch が R2 の消す機構になる**
> ため意図的に据え置いた。**R2 の受け入れ条件にこれを含める**（World 再起動 → 非 root term が
> 2 枚以上 → 全 pane に出力が来ること）。
>
> R1 と R2 は **A6 の直後にやると安い**（触った記憶が残っているうちに）。R3 が本体、R4 は client 側。
> R4 の前提として「registry の change stream」が要るので、R3 で intent の変更点が 1 箇所に
> 畳まれているとそこから流せる（R3 → R4 の順に意味がある）。

### 5.1 R2 実装記録（2026-07-25 出荷）

`respawn_terminal_pump(state, lane, only)` → `reconcile_lane_pumps(pool, pumps, router, lane)`
（`terminal_pump.rs`）。level の比較を成立させた 2 つの実装決定:

1. **demand は edge でなく level で読む** — `TopicRouter::demand_active(topic)`（購読者数の
   直読）。計上（`demand_counts`）は hook の有無に依らない常時計上に変更 —
   router 養子縁組の boot 窓（hook 登録前に subscribe が立つ）で計上が抜けないため。
   restart の `had_pump`（「pump 残留 = 購読者が居た証跡」の間接推論）はこれで消えた。
   start / stop の hook は**同じ reconcile の契機**になった（嘘の edge が届いても level が勝つ）。
2. **pump の identity = 張った先の slot の pid**（`TerminalPump.slot_pid`）。「差し替わったか」を
   呼び手の知識（restart の mode / act の向き）でなく live slot の pid との照合で決める
   （doc 54「identity は実体に」）。pid 一致は触らない = 兄弟 pane 保護（team-b 10 回目）が
   `only` 引数（呼び手の注意）から**構造**に変わった。

契機は demand hook（start/stop）/ 動詞の末尾（act 切替・slot 追加・restart・delete）/
boot 復元後（`lane_spawn_actor` の restore 末尾 + `server.rs` run() の conductor 分）の 3 族。
#11 は「復元完了後の reconcile が demand level × 全 slot で収束する」ことで消えた（受け入れ
テスト: `late_restored_slot_gets_pump_on_next_reconcile` / 兄弟保護:
`reconcile_touches_only_the_swapped_slot_leaving_siblings_alone`）。

---

## 6. やってはいけない

- **reconcile を「定期 tick で全 lane を全読み」にする** — intent が変わるのは動詞のときだけ。
  tick は「読めなかった」を「0 件」と誤認する窓を増やす（§4 の最後の行）
- **reconcile の中で LanePool の write lock を握ったまま spawn する** — 800ms×N を lock 下で
  回すと他 lane を待たせる（A6 4 周目の指摘そのもの）。spawn は隔離してから insert
- **client 側に「pane 一覧」と「session 一覧」を両方配る** — 供給路 2 本の再生産。§3.2 の要点は
  「1 本にする」ことであって「良い形を足す」ことではない
- **`console_mode` を「表示用だから」と残す** — 表示用の要約が要るなら **pane 一覧に含める**
  （server 導出）。中間の投影を残すと必ずまた乖離する

---

## 6.5 もう半分 — World A / World B の分割（mako 2026-07-25）

> mako「これだけ、あなたが苦労するのは設計・仕様の取り方や何かが無用に複雑なんだと思う」
> 「あとは完全性を求めすぎてるか。切っていいものもなんとかしようとしているか」

§1 の診断（intent と実体の乖離）は **17 件のうち 5 件**しか説明しない。バグが実際にどこに
集まったかを数え直すと:

| クラスタ | 件数 | 何が起きたか |
|---|---|---|
| **World A / World B の境界** | **4** | host id の身元 / `isRootHost` の焼き込み / `show_lane` の移植漏れ / click-focus selector |
| **5 つの手書き遷移**（本 doc §1） | 5 | 合わせ忘れ |
| client の cache 3 つ | 3 | 供給路の二重化 |
| role ベースの識別子 | 2 | naming（A6 内で解決） |
| lane 単位の gate | 3 | A6 の本題 |

**最大タイのクラスタは World A/B の境界**で、本 doc の reconcile は**それに触れていない**。

### 6.5.1 その境界は doc 自身が「一時的」と書いている

`docs/design/33-console-unification.md:60`:

> **World A（インライン xterm JS）は不可侵**: input-doubling 調査（VP_TERM_TRACE hop A/B）の
> 診断ベースラインを壊さない。**xterm の bundle 移管は input-doubling 決着後の専用 PR**

つまり恒久的な設計ではなく、**調査が終わるまでの保留**。その調査（memory
`vp-term-input-doubling`）は step 2（診断ログ出荷）で止まっている。

境界が生きている間のコスト（A6 で実測）:

- **同じ概念を 2 言語で表現する**（session roster / host id / focus 優先）
- **境界に型が無い** — `evaluate_script` は引数の数が違っても**コンパイルも実行時も黙る**。
  検証を HTML 文字列に対する assert で代替している（`embedded_terminal_api_is_session_keyed`）
- **同じ webview の中**にいるのに 2 コードベース（doc 33 §1 の表現「同一面が 2 コードベース」）

→ **A6 の直後に「input-doubling がまだ World A を要求するか」を再検証**する価値がある。
要求しないなら xterm を bundle へ移し、World A を畳む。**cache も 1 つ減る**（`laneInstances` の
`isRootHost` は §3.3 の派生値そのもの）。

### 6.5.2 切ってよかったもの — 完全性を求めすぎた例

A6 で作ったもののうち、**切る判断をすべきだった**もの:

| 作ったもの | なぜ切れたか |
|---|---|
| **`migrate_legacy_replay_in`**（旧名 replay の移設） | replay は「再起動後に前画面が見える」ための飾りで、失っても次の出力で描き直される。**memory `pre-MVP-development-stance`（後方互換は MVP 到達後）に反していた** |
| **boot 窓の保留箱**（`pending_session_fetch`） | §3.2 のとおり**供給路を 1 本にすれば要らない**。症状に機構を足して、根（2 本目の供給路）を消さなかった |
| ✕ の replay file 掃除 | team-b 自身が sub-75「無害」と判定。29 commit の PR でやることではない |

**メタな原因**: 「見つけたものは全部この PR で直す」を既定にしていた。**「これは切っていいか」を
先に問う手順が無かった**。

> ⚠️ 規律として: 見つけたものは **①この PR で直す ②別 PR に起票する ③切る（直さないと決める）**
> の 3 択で、**既定は ①ではない**。判定基準は「**それが無いと user が困るか**」— replay の
> 前画面は困らない（次の出力で戻る）/ ghost replay は困る（消したはずの画面が出る）。

### 6.5.3 必要な複雑さ（過剰修正しないための線）

逆に、**畳んではいけない**もの:

| もの | なぜ必要か |
|---|---|
| **intent / 実体の 2 層** | 再起動を跨いだ復元が要る（intent が無いと「前回 chat だった」を思い出せない） |
| **vp-app Rust の中継** | webview は QUIC を話せない。ただし**中継の cache を authoritative にする必要はない**（pass-through でよい） |
| **session ごとの資源分離**（slot / engine / replay） | 「1 session = 高々 1 engine」の法の実体。畳むと会話が混ざる |

---

## 7. 未決（議論が要る）

### 7.0 原理・仕様の層（mako「その際に原理や構造も見直していこう」2026-07-25）

機構（reconcile）より上の層で、**この機会に一緒に見直す**もの。答えを先に書かず論点として置く。

**① `act` の定義軸は「見え方」か「今どの資源で往復しているか」か。**
doc 50 §4.0 は Echoes を「往復そのもの。違いは**見え方** × **投げる先**」と定義した。実装では
act = tui/chat が「PTY-hosted TUI か headless stream-json か」= **資源の種類**と 1:1 に対応する。
定義（見え方）と実装（資源）が一致しているのは幸運か、それとも**見え方が資源に縛られている**
のか。後者なら「同じ会話を chat で見ながら別 pane で TUI」も原理上ありうる話になる（今は
「1 session = 高々 1 engine」の法で禁止。禁止の根拠は resume handoff のコストであって見え方
ではない）。→ **法の根拠を「原理」から「実装制約」へ言い直す**べきかもしれない。

**② 不変条件を誰が守るか。**「1 session = 高々 1 engine」「root は engine を持つ」等の法は、
いま**各動詞が個別に守っている**（`open_slot_for_session` が check、`ensure_chat_engine` が check、
`prepare_switch_root_session` が check）。reconcile 化すると**守り手を 1 箇所にできる**が、
それは「動詞は法を知らなくてよい」に倒すことでもある。**入口で弾く（fail fast）か、
reconcile が収束させる（eventually correct）か**は設計思想の選択。混在は最悪（今それに近い）。

**③ 「見え方の乗り換え」は本当に無料か。** doc 50 §4.7 で「act 切替 = Reborn ではない。VP は
会話を持っていないので、取り替えるのは読み取り装置」と結論した。だが実装では engine プロセスを
落として `--resume` で立て直している（= 会話は engine 側で継がれるが**プロセスは死ぬ**）。
「レンズの交換」という語彙は**利用者にとって**正しいが、**実装コスト**は Reborn に近い。
語彙が実装を軽く見せていないか。→ Reborn 実装時に**両者の差**を明文化すべき。

**④ 上位階層（艦隊 / atlas group）を視野に入れるか。** mako の構想
（`project` → `repository`、その上に束ね層。creo `vp-fleet-map-sidebar-idea`）は階層を 1 段
増やす。§2.5 の「lane が持つ / session が持つ」の線引きを決めるなら、**その上の層が何を持つか**
も同じ基準（「複数で共有しても答えが 1 つに決まるか」）で決められる。**今決めないが、
基準は共有できる**ことを記録しておく。

**⑤ edge → level は VP 全体の通信規律にすべきか。** §2.3 は pump の demand を level に倒す話
だが、同じ形は他にもある（board の retained / wire の nudge / hub の presence）。
**「寿命の違う 2 者の間で変化を signal にしない」を doc 51 の原理の 1 つに昇格**させる価値が
あるか。process 層の heartbeat（15s の level 報告）は既にこの規律に従っている。

### 7.1 機構の層

1. **`act` は intent か実体か。** いま registry の `act` は intent（永続する意図）で、slot / engine が
   実体。この 2 層構造自体は正しいと思われるが、「act を持たず**実体の種類から導出**する」案も
   ありうる（その場合、再起動を跨いだ復元ができなくなる = intent が必要）。→ **intent 側に
   置くのが正**という理解でよいか
2. **reconcile の失敗をどう扱うか。** 1 枚の slot が立たなかったとき、他を巻き戻すのか（原子性）
   部分成功を許すのか（best-effort + 次の reconcile で再挑戦）。A6 の実装は後者（warn で継続）。
   reconcile なら**後者が自然**（次の契機で収束する）だが、「永久に立たない slot」を検出して
   user に見せる仕組みが要る
3. **どこまで reconcile に含めるか。** replay file の掃除は含めた（#5）。`cc_session` / `wire`
   mailbox / board も lane-scoped state だが、これらは intent/実体の 2 層になっていない。
   **含めない**方が境界が明確だと思われるが、`clear_lane_state_in`（lane 削除の一元 GC）との
   関係は整理が要る
4. **R4 の「pane 一覧」に何を載せるか。** id / kind / label / session / root は要る。
   灯（活動）/ 会話 id / 鮮度は event 由来で頻度が違うので**別 channel が正**か

---

## 8. R1 実装 census（2026-07-25、着手前の棚卸し）

`console_mode` の grep 169 箇所を分類し、**読み手ごとの「本当の問い」と置き換え先**を確定した。
[[comment-is-not-proof]] の規律で、doc §3.1 の表は**コードを読んで訂正**してある（下記 8.2）。

### 8.1 169 箇所の内訳

| 分類 | 数 | 扱い |
|---|---|---|
| **実読み手**（値で分岐する） | 9（server 6 / client 3 系統） | §8.2 の表で置き換え |
| **書き手**（投影に代入） | 5 箇所 | field ごと撤去 |
| test fixture `console_mode: Default::default()` | ~20 | field 削除で機械的に消滅 |
| **`migrate_console_modes`**（旧 disk file の one-shot migration） | 6 | **別物 — 残す**（disk の後方互換であって投影ではない） |
| comment / 旧名の言及 / 命名規則の参照 | 残り | 文言のみ追随 |

> ⚠️ **書き手のうち 2 箇所（`lane_spawn_actor:281` / `routes/lanes.rs:134`）は既に
> `session_registry::root_act()` を直読して LaneInfo に詰めている**。つまり現状は
> 「registry を読む → 投影に書く → 読み手が投影を読む」の**往復**で、投影を消せば
> 読み手が registry を直読するだけになる（経路が 1 本減る）。

### 8.2 読み手 × 本当の問い × 置き換え先

| # | 読み手 | 本当の問い | 置き換え |
|---|---|---|---|
| 1 | `lanes_state::restart_lane`（:1064） | root の act（PTY 張替か engine drop か） | `root_act()` 直読 |
| 2 | `focus_chat_session`（:1714） | `LaneInfo.pid` の意味（chat engine か PTY か） | 同上 |
| 3 | `remove_chat_session`（:1758） | 同上 | 同上 |
| 4 | `routes/lanes.rs`（:998） | restart 後に engine を eager spawn するか | 同上 |
| 5 | `unison_server`（:924） | **lane が pool に実在するか**（値は不使用） | **明示的な存在 check**（§8.4） |
| 6 | `unison_server`（:1957） | capture 失敗の案内文（root が chat か） | `root_act()` 直読 |
| 7 | `delivery_actor` / `delegation`（nudge） | **打てる先の種類**（PTY か engine か） | 呼び手が registry から埋める（§8.3） |
| 8 | client `isLaneAlive`（`sidebar/lane.ts:85`） | engine-less でも生きているか（#683 再演防止） | `sessions` から導出（§8.5） |
| 9 | client `is_chat_lane` / respawn gate（`app.rs:2160/2578`） | 同上 | 同上 |

### 8.3 §3.1 の訂正 — nudge の問いは実体ではなく intent

§3.1 の表は `delivery_actor` の問いを「**root slot が存在するか（PTY に打てるか）**」= 実体と
書いていた。**これは誤り**。実体で答えると壊れる:

`pick_nudge_target` は Running な lane を選ぶ。Running かつ root slot 不在は 2 通りある
——「chat lane（正常）」と「**tui だが slot が死んだ**」。実体ベースだと後者を chat と誤判定して
`echoes_nudge` に流し、engine が lazy spawn される → PTY が復活した瞬間に**同一会話 2 engine**。
現状の投影（intent）なら `lane_nudge` を送って失敗し、**pending 保持で次 pulse に再試行**という
正しい挙動になる。

> **gate が副作用として守っていたもの、1 件目**（[[gate-hid-a-second-bug]]）。
> 一般形: **「打てる先の種類」は intent、「今打てるか」は実体**。混ぜると boot 窓と
> 死んだ slot で誤る。

置き換えは「`NudgeTarget.console_mode` → `root_act`（呼び手が `session_registry` から埋める）」。
純関数性は保つ（unit test を disk 依存にしない）。読む頻度は pending がある lane のみで軽微。

### 8.4 副産物 — 存在 check の流用（#5）

`unison_server:924` の `pool.console_mode(&addr).is_none()` は **値を使っていない** —
getter が `Option` を返すことを「lane 実在」の signal に流用している。投影廃止と独立に
「明示的な存在 check」へ直す（[[one-predicate-three-properties]] の亜種）。

### 8.5 client 側をどうするか（R1 の scope 判断）

client の読み手 3 系統（alive 判定 / Act II 判定 / respawn gate）は**すべて同じ問い**に還元できる
——「この lane は PTY を持たなくても生きているか」。選択肢と判断:

| 案 | 内容 | 判定 |
|---|---|---|
| (i) server だけ直す | wire に `console_mode` を残す | ✗ 中間状態（[[pre-MVP-development-stance]] に反する） |
| (ii) `sessions` から導出 | 既に wire に載る `sessions`（A6 で追加）から root の act を引く | **採用** |
| (iii) 実体 field を新設 | `alive` 等を server から配る | ✗ R4 の pane 一覧で消える field を今足す（§6「中間の投影を残すな」） |

(ii) を採り、**client の導出は 1 関数に閉じる**（3 読み手が同じ関数を呼ぶ）。R4 で pane 一覧に
差し替えるときの改修点も 1 箇所になる。

> ⚠️ **未確認の前提**: `sessions` は `Option<SessionRegistry>`（旧 SP 互換の serde default）。
> `app.rs:1821` に「registry 不在なら console_mode が tui として root 1 枚」という fallback が
> あり、投影を消すとこの退路が失われる。fold-in 後は旧 SP が存在しないので `None` の意味は
> 「boot 窓（enrich 前）」だけのはずだが、**実装時に供給点を数えて確認する**
> （[[ssot-concentrates-existing-weakness]]）。

### 8.6 壊し方テスト（[[decide-the-breaking-change-first]]）

fix を外して赤、では不十分。**特例を作る / 順序を入れ替える / 呼び手を繋がない**の 3 通りで赤を見る:

1. **root を chat にして nudge** → `echoes_nudge` に route されること（投影ではなく registry を見る）
2. **root 付け替え直後に restart** → 新 root の act が honor されること（投影の同期忘れが再発しない）
3. **root=tui のまま非 root だけ chat** で capture / demand → lane 単位の要約に落ちないこと（:918 の旧バグ再演）

加えて既存 `moving_root_syncs_the_console_mode_projection` は投影ごと消えるので、
**同じ性質を registry 直読で言い直す**形に書き換える（テストの消滅 = 性質の消滅にしない）。

---

## 9. mako 提案（2026-07-25）— VP が identity の発行者になる

> mako「VPは、あまりモデルとのsession idを利用しない構造にして、VPで発行したものを使って
> 構造化するイメージ」「Echoesが一つのIDを生成時にもって、それをrootに割り当てる」
> 「セッションはEchoesの中で完結するようにする」

**状態: 確定（2026-07-25 の一問一答で採用・拡張）→ SSOT は
[54-lane-worker-model.md](./54-lane-worker-model.md)。本節は経緯の記録として残す。**
（議論の過程で本節の「単独 presence としての Echoes ID」は棄却され、「働き手 = 席を占める
プロセス（shell 含む・複数）」へ発展した — 詳細は doc 54 §2 の決定の記録）
CLAUDE.md の第一行「VP が主、Claude Code はそのエンジン」に identity 層を追いつかせる話 —
DB の surrogate key vs natural key と同型（engine session id = 他システムの natural key）。

### 9.1 診断 — 「session」1 語が 3 役を兼ねている

shell が root になれない gate（`prepare_switch_root_session` の EngineKind check）は症状で、
病因は **pane（見え方 1 枚）/ conversation（engine との対話、resume の単位）/
presence（lane の働き手 = mailbox の主）の 3 概念が「session」1 つに畳まれている**こと
（[[one-predicate-three-properties]] の命名版）。presence の座を pane が争う構造だから
資格審査（gate）が要る。3 役に別の identity を与えれば gate は**表現不能**に変わる
（A6「identity で識別子を決める = 間違える手段を消す」の 1 段上への適用）。

### 9.2 目標形（sketch）

```
Lane（場所）
└── Echoes（presence）— VP 発行 ID、生成時に確定、不変。agent@<lane> の解決先（= 旧 root）
    ├── active: ConvId（lane 宛 mail に誰が答えるか。旧 root/focused の非対称はここに畳まれる）
    └── conversations: [ConvId]
Conversation — VP 発行 ID。engine: EngineKind / engine_ref: Option<String>（cc_session 等、
    私有・後着・交換可）/ act: レンズ
Pane — view（ConvId に紐づく）。shell pane は conversation を持たない pane = Echoes の外
```

**原則: engine の id は「値」として流れてよいが「鍵」になってはならない**（`--resume` の
引数は正、wire field / state file 名 / 配送判断の鍵は誤）。

### 9.3 消える病巣（証拠つき）

| # | 病巣 | 証拠 |
|---|---|---|
| a | **遅れて届く身元**（engine id は初応答まで無い）→「名前の無い session」窓の機構一式 | `ReportTrigger::Issued/Spoken` + F1/F2 guard（幻 session 対策、session_registry.rs:418） |
| b | **供給点全部で enrich** | `refresh_engine_session_id` の「供給点すべてで呼ぶこと」⚠️（lanes_state.rs:399、#683 地形） |
| c | **claude 特別扱いの非対称** | `LaneInfo.cc_session_id` / `NudgeTarget.cc_session_id` / `cc_session` file / channel D headless（`build_bg_args` = claude 専用。codex/grok root に channel D の「継ぐ」経路が無い — 要検証） |
| d | **root 資格 gate + 付け替え機構** | `prepare_switch_root_session` gate / `canCloseSession` の二重執行（server Err + client gating） |
| e | **key 再利用 ghost** | fresh Reset で採番が戻る → A6 の c3289e3a（ghost replay）と同族。一意・永久 ID で構造的に消滅 |

### 9.4 消えないもの / コスト

- **本 doc の本丸（intent/実体の reconcile）は消えない** — R2/R3 はそのまま必要。World A/B も無関係
- mapping invariant（VP id → engine_ref が腐ると silent fresh resume）が registry に集中
  （[[ssot-concentrates-existing-weakness]]）
- **[[writer-without-reader]] の前例 = `LaneId`（2 年間読み手ゼロ）**。Echoes ID は読み手
  （mailbox 解決 / chat 動詞の既定宛先）と同じ PR で入れること
- 移行は forward-only（§6.5.2 の教訓 — 過去会話の migration は書かない）

### 9.5 順序への制約

**R4 の wire 契約（pane 一覧の鍵）より前に決める** — 契約を 2 回鋳直さない。R1/R2 は
両モデルで同形なので影響なし。→ `R1 → R2 → [設計枠: doc 54 起草 + §6.5 World A/B 再検証] → R3 → R4`

§7.0 との接続: ①（act の定義軸）は「act = Conversation のレンズ」で答えの半分が出る。
③（Reborn の語彙）は「VP id は不変で engine_ref を捨てる」か「新 ConvId」かの選択として
きれいに言い直せる（design phase 送り）。

---

## 10. R3 実装 census（2026-07-25、着手前の棚卸し）

R1/R2 と同じ手順（census → 地図 → 実装）。**動詞の全数**と、各動詞が持つ
「registry write（intent）/ 手書きの実体遷移 / LaneInfo 代表値の追随」の 3 つ組を数えた。
設計ゲート決定①（doc 54 §7）により **R3 は現行 schema のまま機構のみ**。

### 10.1 動詞の全数 — 3 つ組の対応表

| # | 動詞（実装） | registry write | 手書きの実体遷移 | 代表値追随 |
|---|---|---|---|---|
| 1a | boot conductor（`with_root`） | 初回のみ既定 act | root PTY spawn + `restore_term_slots` | insert |
| 1b | boot performer（`lane_spawn_actor::handle_cmd`） | なし（root_act 読みで分岐） | chat: engine-less 登録 / tui: root spawn + restore + **pump reconcile（R2）** | insert |
| 2 | act 切替（`set_session_act`） | `set_session_act` | →chat: `drop_slot` / →tui: `chat_engines.remove` + root なら `restart_lane(Resume)`・非 root なら `open_slot_for_session` | pid / state |
| 3a | Add console（`open_new_slot`） | `create`(Tui)、**失敗時 remove 巻き戻し** | `open_slot_for_session` | — |
| 3b | Add chat（`create_chat_session`） | `create`(Chat, focus) | なし（engine は lazy） | — |
| 4 | ✕（`remove_chat_session`） | `remove` | `chat_engines.remove` + `drop_slot` + `replay_log::clear` + `clear_replay_session` | pid（root=chat 時） |
| 5 | restart（`restart_lane` ×3 mode） | Reset のみ: replay ×N + registry `clear` + PTY replay 破棄 | chat: `chat_engines.remove` / tui: `drop_slot(root)` + spawn + insert | pid / state |
| 6 | New root（`prepare_new_root_session`） | `create_root`（root+focused 付替、既定 act） | caller が restart(**Bare**) | — |
| 7 | Switch root（`prepare_switch_root_session`） | root+focused 付替 | caller が restart(**Resume**) | — |
| 8 | focus（`focus_chat_session`） | focused 付替 | `ensure_chat_engine`（eager） | pid |

### 10.2 実体は 4 種 + 台帳 1 つ — desired の導出規則

| 実体 | あるべき条件（desired） | eager / lazy |
|---|---|---|
| PtySlot（+TermAttach 双子） | act=Tui の session | **eager**（console は見る物 — boot 復元が現に eager） |
| chat engine | act=Chat の session **∧ demand**（submit / focus / GUI 購読） | **lazy**（pump と同型の demand 依存） |
| replay 貯蔵（PTY replay / replay_log） | session が registry に存在 | 随伴（remove / Reset で破棄） |
| LaneInfo.pid / state | root の実体から**導出**（読む時点の代表） | 派生値（reconcile が更新側を畳む） |
| terminal pump | R2 で reconcile 済 | reconcile_lane の末尾の 1 契機に |

「1 session = 高々 1 エンジン」の法（`open_slot_for_session` の 4 拒否 / `ensure_chat_engine` の
番人）は、**desired が act から一意に導出される**ことで構造的に満たされる — reconcile 化の後、
入口ガードは「法の執行」から「不正 intent の早期拒否」に役割が変わる。

### 10.3 発見 — RespawnMode は registry に吸収できる（要検証）

Bare / Resume / Reset の差を数え直すと、**すべて registry の今に既に表現されている**:

- **Resume** = その session の `conversation` で立てる（level で表現可能）
- **Bare** = 新 root は conversation **無し**で作られる（`create_root` 直後）→
  「registry に従って立てる」だけで bare になる。現に `handle_echoes_session_new_root` の
  コメントが「未発話の非 #1 root は build_stand_command が bare に倒す」と**既にこの方向**
- **Reset** = registry `clear` が intent を運ぶ（既にそう — 実体の畳みは reconcile の仕事）

→ R3 で `RespawnMode` 引数は「registry に何を書くか」（動詞側）に還元できる可能性が高い。
検証ポイント: `build_stand_command` の bare フラグを mode でなく registry（conversation 有無）
から決めて全経路で同値か。

### 10.4 reconcile_lane の素描

```
reconcile_lane(addr):
  desired = registry の sessions（key, act, stand, conversation）
  各 desired session:
    act=Tui ∧ slot 無  → 立てる（conversation 有れば resume、無ければ bare — §10.3）
    act=Chat ∧ slot 有 → 畳む
    act=Tui ∧ engine 有 → 畳む
  registry に無い session の実体 → slot / engine 畳む + replay 破棄
  LaneInfo 代表値を root の実体から導出して更新
  末尾で terminal pump reconcile（R2）
  （chat engine の起立は lazy のまま = demand 契機に委ねる）
```

動詞は「registry に書く + `reconcile_lane(addr)`」だけになる（§2 の応答そのもの）。

### 10.5 設計判断が要る点（R3 の地図で決める）

1. **spawn の隔離**: slot 起立は 800ms×N sync。§6 の規律（write lock 下で spawn しない）を
   reconcile_lane 自身がどう満たすか — 「読みで desired/actual 差分を計算 → lock 外で spawn →
   write lock で insert（race 再検査）」の 3 段（`lane_spawn_actor` と同じ形）
2. **失敗の意味論の統一**: 現行 `open_new_slot` は失敗時に registry を**巻き戻す**が、boot 復元は
   **残して空 pane**（graceful degrade）。reconcile 思想では後者に統一（残った intent は次の契機で
   再試行される）が、「Draft が残る」体験の変化を含む — 要判断
3. **restart の「差し替え」表現**: reconcile は「無ければ立てる」— restart（生きた slot を意図的に
   殺して張り替える）は「動詞が slot を落としてから reconcile を呼ぶ」で表現するか、
   世代（pid）を intent 側に持つか

---

## 11. roster の供給 1 本化（2026-07-25 夜、bug 起点の §3.2 前倒し）

R2 の実機受け入れ検証で踏んだバグ（creo `mem_1CdNphDXfrVwCs8Z4Etrhm`）の根治。
**症状**: GUI 起動後に GUI の外（CLI / MCP）から `vp lane slot-new` した term session が
**pane grid に現れない**（xterm 実体と terminal 購読は裏で動いている）。

### 11.1 診断 — 同じ roster が 2 本の道から届いている

| 消費者 | 供給路 | server の変化に |
|---|---|---|
| World A（xterm instance + terminal 購読） | lanes **snapshot** の `lane.sessions` | **追随する** |
| World B（pane grid の roster） | `echoes_session_list` の **fetch 結果のみ** | **追随しない** |

fetch の契機は ①lane を開く ②GUI 自身が動詞を撃った後 ③boot 窓の再送 の 3 つだけ。
つまり **GUI は自分が起こした変化しか見えない**。しかも server の session 動詞
（slot_new / session_create / session_remove / set_act / new_root / switch_root / focus）は
**`emit_lane_update` を 1 つも呼んでいない**（既存の呼び手は restart と hook 通知の 2 箇所のみ）
= 「roster が変わった」を server が誰にも知らせていない。

これは §3.2 が予告していた class そのもの（供給路 2 本 / client cache 3 つ）。

### 11.2 決定 — 2 本目を削る（足さない）

mako「不具合がある上に新しい実装は載せたくない」（2026-07-25）。対症（snapshot 到着時に
差分を検知して webview へ push = 2 本のまま同期を足す）は §6 の「やってはいけない」に該当。

| # | 決定 | 理由 |
|---|---|---|
| 1 | **roster の供給 = snapshot 1 本**。`echoes_session_list` は **client から撃たない**（CLI / MCP 用に server は残す） | §3.2。供給路が 1 本なら「どちらが新しいか」の問いが消える |
| 2 | **知らせるのは動詞の責務** — session を変える動詞の末尾で `emit_lane_update` | R2 の「動詞の末尾で reconcile」と同型。契機は判断を持たない |
| 3 | **wire 型を disk 型から分ける** — `LaneInfo.sessions` を `SessionRegistry`（disk SSOT）の直載せから **wire view** に変え、`chat_capable` を **server で導出**して載せる | 能力表は server が SSOT（client に engine 名の分岐を作らない）。disk 型に runtime 由来の field を足さない |
| 4 | **保留箱（`pending_session_fetch`）を撤去** | §6.5.2 の予言どおり — 供給が 1 本になれば boot 窓の取りこぼしは構造的に消える（snapshot は変化時 push + 定期） |
| 5 | `live`（chat engine の in-memory 有無）は **wire に載せない** | roster の読み手がゼロ（grep 確認）。fetch 側 `list_chat_sessions` には残す |

### 11.3 R4 との関係

R4 は「**pane 一覧**を server が導出して配る」。本節は roster（session 一覧）の**供給路**だけを
1 本にする — 形は変えない。R4 はこの 1 本の上で「配るものを pane 一覧に変える」だけになり、
**R4 の前提（供給 1 本 + 変化時 push）が先に揃う**。R3（reconcile_lane）とも独立。

### 11.4 受け入れ条件

1. GUI 起動中に **CLI から** `vp lane slot-new` → **pane が現れる**（本バグ）
2. GUI 自身の動詞（Add / ✕ / act 切替 / New root / Switch root）で従来どおり即時反映
3. boot 直後に lane を開いても roster が出る（保留箱の撤去で退行しない）
