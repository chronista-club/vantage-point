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
        ├── pane = 見え方（働き手 1 人につき 1 枚）
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
   （コードは機能名・Stand 名は表示層）に従う
2. **id の形式**: ULID 等。CLI の指し方（短縮 prefix / 表示序数）とセットで決める
3. **代表継承の決定的規則の具体**: 最古参か。要件は「決定的で、user に説明可能」
4. **Reborn との整合**: 「同じ働き手で会話を作り直す」= engine_ref を捨てる（id 不変）か、
   新しい働き手を雇うか。doc 50 §4.7 の語彙決定と突き合わせる
5. **registry schema / wire 契約 / migration**（forward-only — 過去会話の移設 migration は
   書かない。doc 53 §6.5.2 の教訓）
6. **働き手宛の郵便**: 箱は lane 粒度のまま = **意図的な cut**。必要が実証されたら再訪
