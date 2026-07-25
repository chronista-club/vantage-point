# doc 54 — lane の働き手モデル（作業台の地図）

**Status**: **起草 = 議論確定（2026-07-25、mako × Claude の一問一答）。実装未着手。**
発端 = mako「何をどう作るのか。それのイメージがない状態で実装書くのは、地図を持たないまま、
うろうろ歩いているのと同じ」。A6 の 17 バグ（doc 50 §4.7）と R1 census（doc 53 §8）が
「地図の不在」の証拠だった — 読み手ごとに「本当の問い」を発掘する必要があったのは、
読み出せる絵が無かったから。
**Owners**: vantage-point + vp-app
**Related**: [53-lane-reconcile.md](./53-lane-reconcile.md)（機構: reconcile。本 doc はその機構が
合わせる**実体モデル**。§9 が本 doc の前身）/ [51-zero-base-review.md](./51-zero-base-review.md)
（原理: 表象の共有）/ [46-lane-pane-model.md](./46-lane-pane-model.md)（session = Pane、
本 doc が語彙を引き継ぎ再定義）/ [50-pane-chrome-and-session-panes.md](./50-pane-chrome-and-session-panes.md)
（§4.7 = 証拠）/ [40-conversation-ssot.md](./40-conversation-ssot.md)（会話 id SSOT — 本 doc で完成形に）/
[39-lane-root-agent.md](./39-lane-root-agent.md)（座と化身 — 本 doc が更新）

> 本 doc の内容はすべて 2026-07-25 の一問一答で確定した。各決定に mako の決め言葉を
> 引用してある（facts-over-narrative — 決定の出典を残す）。

---

## 1. 一枚絵

```
World 👑
└── Project（repo = 場所）
    └── Lane（checkout = 作業台。cwd / branch / board / layout を持つ）
        │
        ├── 働き手（0..N 人）= 席を占めるプロセス（1 人 = 高々 1 プロセス、休眠あり）
        │    ├── 身元: VP 発行 id（雇った瞬間に確定・不変・永久欠番）
        │    ├── agent — engine（不変）+ engine_ref（内部。値であって鍵でない）
        │    │            + act: tui / chat（レンズ。可変）
        │    └── shell — 人間が駆る席。engine なし。act は tui のみ（定義）
        │
        ├── pane / 席 = 見え方と器（scrollback は席のもの。着席は動的 — §3.7）
        │
        ├── 代表 = 役職（Option）。lane の顔: pid / Dead 判定・push の宛先・CLI 既定
        │    └── 自動継承 + 空位許容（§3.3）
        │
        └── 郵便 = 箱（lane 単位の store が真実）。読み手 = 読む時点の代表
```

GUI 側: **注視（focused）は client 所有**。server の pointer は代表 1 本だけ。

---

## 2. 決定の記録

| # | 決定 | mako の決め言葉 |
|---|---|---|
| A | 働き手は複数（並んで働く） | 「物理的に 2 agent が働いてるから、並んで働いている。がしっくりくる」 |
| D | shell も働き手 | 「shell も働き手じゃない。」（働き手 = 台で作業する者。AI との会話は要件でない） |
| — | 働き手 = プロセス | 「1 働き手＝1 プロセス。vp cli も使えるし。あなた (Agent) が存在しないだけで」 |
| — | 郵便は箱、読み手は役職 | 「wire は box に届くから、読み手はその時の代表が読む」 |
| C | 注視は client | 「focus は gui 側にあるべきと思う」 |
| — | 代表の継承 | 「UX 的には『自動継承 + 空位許容』これが良さそう」 |
| B | 身元は VP 発行 | 「VP で発行したものを使って構造化する」「セッションは Echoes の中で完結」 |
| — | shell の万能性は雇用窓口（養子縁組で解く） | 「shell は、cc にも codex にも grok にもなれるからなぁ。でもここを解けたらすごい良い」 |
| — | 観測の trigger は PTY 入力 | 「PTY への入力のデータを観測するのはどう？」 |
| — | enrich は hooks でなく fs 痕跡 | 「cc の hook に頼るのはなんか正確性・拡張性がなさそうな雰囲気する」 |

---

## 3. 実体の定義

### 3.1 働き手（worker）

**台に座って作業する者 = 席を占めるプロセス。** AI との会話は定義の要件ではない。

- **身元**: VP 発行 id。**雇った瞬間（New）に確定** — プロセスが立つ前から名前がある。
  不変・lane 内で永久欠番（Reset でも再利用しない）
- **種類**:
  - **agent** — engine（claude / codex / grok / opencode）は**不変属性**。「engine を替える」
    という操作は存在しない — 会話の文脈は物理的に engine 間を移動できないから。乗り換えたければ
    **新しい働き手を隣に雇う**。engine_ref（cc_session 等の外部会話 id）は**内部属性** —
    resume の spawn 引数として「値」で流れるのは正、wire field / state file 名 / 配送判断の
    「鍵」になるのは誤
  - **shell** — 人間が駆る席。The Hand ✋（stands.rs が語彙を先取りしていた）。
    **wire の市民権は席に付く**: 席の env（`VP_PROJECT` / `VP_LANE` / `VP_SESSION_KEY`）が
    身分証で、`vp wire inbox` / `vp now` / board がフルに使える。
    **欠けているのは AI だけで、席の能力は 1 つも欠けない**
- **1 働き手 = 高々 1 プロセス（休眠あり）**。旧「1 session = 高々 1 engine」の法
  （3 箇所の check で守っていた規則）は、この定義に**吸収**される
- act（レンズ）は可変。shell の act が tui のみなのは禁止でなく**定義**
  （chat レンズには映す会話が無い）

### 3.2 pane（見え方）

働き手 1 人につき 1 枚の窓。A6（doc 50 P3）で確立した「session = Pane」を引き継ぐ:
pane の鍵は働き手 id（identity）。レンズ交換は in-place（renamePane）。

### 3.3 代表（役職）

**lane の顔。** pid / Dead 判定の代表値・push（nudge）の宛先・CLI の省略時既定。

- **範囲 = 全働き手**（shell 可）。旧 root の資格 gate（`prepare_switch_root_session` の
  EngineKind 拒否）は消滅
- **自動継承**: 代表が去ったら残りの働き手が決定的規則で継ぐ（規則の具体は実装で確定 —
  候補: 最古参）
- **空位許容**: 最後の 1 人も去れる。**空の作業台**（皆帰った後の机。board は残る）は
  正当な姿。代表は `Option`
- 旧 root の削除拒否（server Err + client `canCloseSession` の**二重執行**）は消滅

### 3.4 郵便（箱）

- **宛先は箱**（lane 単位の wire store = 真実 = level）。「配送できない」状態は存在しない
- **読み手 = 読む時点の代表**（遅延束縛の役職）。届いた時の担当に固定しない —
  固定すると交代後に誰も読まない手紙が生まれる
- **nudge は加速装置**（edge）: 今の代表が agent のときだけ肩を叩く。pulse ごとに現在値で
  解決する（`delivery_actor` は既にこの形 — `pick_nudge_target` は毎 pulse 選び直し、
  pending 台帳の鍵は箱 × message で働き手ではない）
- **未 ack の手紙は後任に引き継がれる**（受領即 ack の規律 = 後任に二重配送させないための礼儀）
- 代表が shell（or 空位）: push 無し。人間は vp CLI で読む。未読の可視化は計器盤の未読 dot
  （doc 52 wave 3）が level 表示として担う

### 3.5 注視（client 所有）

focused は registry から消える。GUI composer は「どの pane で打っているか」を常に知っている
ので明示 id を渡す。CLI / MCP の省略時既定は**代表に統一** — 「nudge は root、chat 系は
focused」という既定の非対称（CLAUDE.md に ⚠️ 付きで記載）が消える。多視点（2 画面が同じ
lane を見る）でも取り合いが起きない — 注視は視る者の属性であって台の属性ではない。

### 3.6 多孔質な境界 — shell↔tui は VP 抜きで動く（2026-07-25 追記）

> mako「shell↔tui が VP 抜きでできちゃうから、ここだけうまく落とし込まないといけない」
> 「ここは私の設計がまずいのかもと何回か思ってる」

**評価: #661（Act1-layered、login shell 基層）は env 注入税と復元力の実問題を解いた正しい
設計。多孔性は設計ミスの産物ではなく端末の真実** — real shell が 1 席でも存在する限り、
人間はそこで何でも起動できる。選べるのは「多孔性をどこに閉じ込めるか」だけ。

境界には 2 種類ある:

| 境界 | 誰が動かすか |
|---|---|
| **tui ↔ chat**（PTY の席 ↔ chat の席） | **VP の動詞でしか起きない** |
| **shell ↔ tui**（素の prompt ↔ engine TUI） | **VP 抜きで起きる**（engine exit → prompt に落ちる / 人間が手打ちで engine 起動） |

**原理: VP が仲介する境界は intent に書ける。VP 抜きで動く境界を intent に書くと、それは
「VP が制御していない現実の cache」= 投影病（doc 53）の再来。観測（level）で読む。**

- **intent = 席の仕込み**（bare-pty / engine-pty の type-ahead / engine-chat）。boot と
  reconcile が enforce するのは**ここまで** — 席の中身に intent を強制しない（人間が意図的に
  exit した engine を勝手に再注入したら、それは作業台ではなく檻）
- **shell/tui の現在形 = 観測**。観測装置は実在する: ①席 env 経由の hooks 自己申告
  （手打ち起動の claude も `VP_LANE` を持つので SessionStart hook が届く = 席の市民権の実利）
  ②delivery の CC activity poll（`recipient_readiness` は**観測してから打鍵** — 居なければ
  型打ちせず headless fallback。素の prompt への打鍵事故は観測を前提条件にして防ぐ）

**設計 fork（設計ゲートで決める。今決めない）**:

| 案 | 形 | 得る / 失う |
|---|---|---|
| **C: 基層 shell + 観測**（現行の完成形） | #661 維持 + SessionEnd hook / activity poll で現在形を観測 | 変更最小・席内 prompt 復帰の自由 / 多孔性が全 engine 席に残る（観測で追う） |
| **A: 封印席** | engine 席は `$SHELL -lc 'exec <engine> …'` — **engine exit = 席の死 = 決定的 signal** | 「1 働き手 = 1 プロセス」が文字通り・prompt への nudge 誤爆が構造的に消滅・多孔性は shell 席のみ / 席内 prompt 復帰を失う（受け皿 = 隣の shell 席。本 doc で shell は一級市民）・exit 後 UI（respawn / close）が要る |

→ **§3.7（養子縁組）の採用により C 案が本線に確定**（2026-07-25 同日）。A 案（封印席）は
「shell が何にでもなれる」= 雇用窓口そのものを塞ぐため棄却。

### 3.7 養子縁組（adoption）— 雇用の 2 つの入口と 4 層観測（2026-07-25 追記）

**shell の万能性は「モデルの穴」ではなく「人間側の雇用窓口」。** 働き手を雇う入口は 2 つ、
着地は 1 つ:

```
VP の動詞で雇う（Add メニュー）──┐
                                ├──→ 同じ registry（働き手 + engine + engine_ref）
人間が席で手打ちで招く ──────────┘
        └── 観測で「養子縁組」
```

養子縁組後は VP 雇用と区別が付かない — **再起動すると手打ちで招いた会話も resume される**
（作業台が、あなたが招いた客を覚えている）。

#### 席と働き手の分離（§3.1 / §3.2 の精錬）

「shell が cc になる」のではなく「**席に cc の働き手が座った**」:

| | 席（seat） | 働き手（worker） |
|---|---|---|
| 何か | lane の備品。PTY + scrollback | engine の会話（or 人間の手） |
| 身元 | pane の id | VP 発行 id、**engine 不変** |
| 歴史 | **scrollback / replay は席のもの**（prompt → claude → prompt → codex が 1 本の巻物） | **会話は働き手のもの**（transcript / engine_ref） |

engine 不変の法は働き手の水準で守られたまま、席は誰でも招ける。着席（seat × worker の
binding）だけが動的。A6 が replay を session（= 席）に鍵付けしたのは、この分離の
**既に正しい側**にいる。

#### 4 層観測 — すべて engine 非協力で成立

| 層 | 源 | 役割 | engine 協力 |
|---|---|---|---|
| **trigger** | 入力 tap（CR のみ検知。**内容は読まない** = keylog 性が構造的にゼロ） | 検査の合図（edge — 落としてよい） | 不要 |
| **inspect** | 席のプロセス木 + argv（VP は shell の pid を持つ。同 uid で sysctl 可） | 誰が座ったか / `--resume` id | 不要 |
| **watch** | kqueue NOTE_EXIT | 退席の event-driven 検知 | 不要 |
| **enrich** | **fs 痕跡観測**（claude: `~/.claude/projects/<slug>/*.jsonl` / codex: rollout の filename — 中身の深 parse は不要） | fresh 起動の会話 id | **不要 — 規約知識のみ** |

- **生の入力 parse は採らない**: 補完（`cla<TAB>`）・履歴（`↑↑`）・Ctrl-R では、コマンド本文が
  入力 stream に**存在しない**。入力は合図（edge）、真実はプロセス木（level）— doc 53 §2.3 の
  規律の観測版
- **痕跡は自己検証的**: resume が読むもの（transcript / rollout）と同一の source を見るので、
  「報告」と違い嘘をつかない。VP は既に hook 報告を `transcript_exists` で裏取りしている
  （stand_spawner.rs）— **聞くのをやめて身分証を直接見る**形に揃える
- **段階構造の保険**: 安全（nudge 誤爆防止）は inspect 層だけで成立。enrich が欠けた engine
  でも安全は劣化せず、取れる engine だけが「再起動で会話も蘇る」上位体験を得る
- **engine 追加コスト = 静的知識 2 行**（argv の形 + 痕跡の path）。`EngineKind::from_stand`
  と同じ「対応表 1 箇所」パターン — runtime のプロトコル統合はゼロ

#### hooks は identity 任務から退役

CC hook を identity の源にしない根拠（全て実績）: **幻 session**（`|| claude` が新 id で
SessionStart → F1/F2 guard が要った）/ **発火の非一貫性**（Notification hook は headless +
bypass で不発と実測済）/ **注入の穴**（`--settings` type-ahead 限定 = 手打ち claude に
乗らない）/ **schema 契約なし**。

- `vp wire hook-check` の 2 仕事を分離（[[one-edge-two-jobs]] の逆適用）: **会話報告は
  fs 観測へ退役**、wire 未読 pull（ターン頭の inbox check = agent の礼儀作法）は残る
- F1/F2 幻 session guard は「報告の信頼調停」から「**intent と fs の乖離検出**」（reconcile 形）
  へ置き換わる — 観測はすべて駅③（`record_conversation` の policy 集約点）に収束

---

## 4. 原理（この議論で結晶したもの）

1. **VP が identity の発行者** — identity は lifecycle を所有する系が発行する。engine の id は
   他システムの natural key → 私有属性へ（surrogate key vs natural key）。
   CLAUDE.md「VP が主、Claude Code はそのエンジン」に identity 層を揃える
2. **identity は実体に、role は責務に** — 特定の実体（会話・画面・replay）を指すなら
   identity（A6 のバグ群は実体を role で指したから起きた）。責務（読む義務・lane の顔）を
   指すなら role の遅延束縛（責務を identity で固定すると継承が壊れる）
3. **席の市民権** — wire の市民権は席（env）に付く。AI かどうかは市民権と無関係
4. **store が真実（level）、push は加速（edge）** — doc 53 §2.3 の edge→level が郵便でも
   同じ答えを出した
5. **規則を定義に吸収する** — gate には**規則型**（表現できる状態を実行時に禁じる = 地図が
   歪んでいる兆候）と**定義型**（その状態に意味が無い）がある。地図を直すと規則型 gate は消える

---

## 5. 歩行検証の記録（6 場面）

| 場面 | 歩行結果 |
|---|---|
| 再起動 | registry = 働き手一覧 + 代表。agent は resume で文脈が蘇り、shell は画面の残像（replay）だけ戻る — 物理的に正直 ✓ |
| New | id は雇った瞬間に確定。幻 session guard / Draft 状態の存在理由が消える ✓ |
| レンズ交換 | 変わるのは化身（プロセス）だけ。id / engine / engine_ref / pane は不変 ✓ |
| 郵便 | 箱 + 遅延束縛の代表。shell 代表への「通知」は計器盤の未読 dot（level） ✓ |
| 併用 | 各働き手が id / pane / replay / 会話を持つ。model も働き手の属性（lane 単位 `engine_model` は地図の間違いが field に出ていたもの） ✓ |
| ✕ / Reset | id 永久欠番で ghost class 消滅。代表は自動継承、空位可 ✓ |
| 手打ち起動（多孔性） | 入力 tap → プロセス木 → fs 痕跡で養子縁組（§3.7）。VP 雇用と同じ registry に合流、再起動で resume ✓ |

歩行で出た新しい問いは「代表の空位規則」1 つだけ（→ §2 で決定済み）。構造の破綻はゼロ。

---

## 6. 現行構造とのギャップ表

| 今 | 地図 | 消えるもの |
|---|---|---|
| 「session」（1 語 3 役: pane / conversation / presence） | 働き手 + pane | 概念の混在 |
| SessionKey（lane 内小整数、Reset で再利用） | VP 発行 id（永久欠番） | ghost replay class（c3289e3a と同族）。⚠️ CLI の指し方に短縮形が要る |
| root: SessionKey + EngineKind 資格 gate | 代表: Option（役職・自動継承・shell 可） | gate / エラー文 / 削除拒否（二重執行） |
| focused（registry） | client の注視 | root/focused の既定非対称 |
| `console_mode` 投影 | （無し） | **R1 がそのまま第一歩**（撤去着手済、stash 退避中） |
| `LaneInfo.cc_session_id` / `NudgeTarget.cc_session_id` / `cc_session` file | 働き手の engine_ref（内部） | claude 特別配管。channel D が engine 汎化 |
| `engine_model`（lane 単位 file） | 働き手の属性 | lane 単位 model の嘘 |
| `prepare_new_root_session` / `prepare_switch_root_session` | 「雇う」+「代表指名」の直交 2 動詞 | cross-engine 注意書き一式 |
| 「1 session = 高々 1 engine」（3 箇所 check） | 定義（1 働き手 = 高々 1 プロセス） | check の分散 |
| 幻 session guard（F1/F2 / `ReportTrigger`） | 不要（id は誕生時確定） | doc 40 の guard 機構 |

---

## 7. doc 53（reconcile）との関係と実装順

doc 53 の機構（intent と実体を reconcile で合わせる）は**そのまま残る** — 本 doc は
「reconcile が合わせる対象の実体モデル」を与える。identity をどの系が発行しても
「slot を立て忘れる」は起きるので、両者は補完関係。

```
R1（console_mode 廃止 — 着手済、本地図と整合）
→ R2（pump level 化）
→ [設計ゲート: 本 doc の実装 phase 切り + World A/B 再検証（doc 53 §6.5）]
→ R3（reconcile_lane — 本 doc のモデルを reconcile する）
→ R4（pane 一覧配信 — wire 契約は本 doc の id で鋳る。2 回鋳直さない）
```

---

## 8. 未決（実装層に送る詳細）

1. **語彙**: 「働き手」のコード識別子（候補: worker / WorkerId）。Stand 名との層分け規律
   （コードは機能名・Stand 名は表示層）に従う。
   **act の rename 候補（mako 2026-07-25）: `shell → act-i / tui → act-ii / chat → act-iii`**
   — 3 幕の梯子は #661 の物理層（Act1 = login shell が土台）と一致し物理モデルに忠実。
   ⚠️ rename 時に処理する 3 点: ①act は registry / wire に**永続される値** = 純 rename でなく
   format 変更 → **本 doc の schema 変更（id 形式）と同じ migration に束ねる** ②既存の
   「Act I/II」（doc 33 系 = tui/chat）と採番がシフトする（今の Act I が新 act-ii）ため
   doc / memory / コメントの旧参照を一括訂正 ③§3.6 により shell/tui の現在形は stored enum
   でなく観測 derive → rename は「仕込み intent + 観測」への再編とセットで行う
2. **id の形式**: ULID 等。CLI の指し方（短縮 prefix / 表示序数）とセットで決める
3. **代表継承の決定的規則の具体**: 最古参か。要件は「決定的で、user に説明可能」
4. **Reborn との整合**: 「同じ働き手で会話を作り直す」= engine_ref を捨てる（id 不変）か、
   新しい働き手を雇うか。doc 50 §4.7 の語彙決定と突き合わせる
5. **registry schema / wire 契約 / migration**（forward-only — 過去会話の移設 migration は
   書かない。doc 53 §6.5.2 の教訓）
6. **働き手宛の郵便**: 箱は lane 粒度のまま = **意図的な cut**。必要が実証されたら再訪
7. **観測の実装詳細（§3.7）**: 同 cwd に同 engine が 2 席のときの痕跡相関（起動時刻 +
   必要なら PTY 活動 ↔ file mtime）/ grok・opencode の痕跡規約調査 / 痕跡 path の
   対応表化（`EngineKind` 拡張）
