# Design 24: VP の背骨 — World / SP(presence) / Views 三層 + Lane=atom 四本柱

> **Status**: 構想（未実装・移行は段階的・急がない）。2026-06-19 の設計対話の結晶を正典化したもの。
> **creo SSOT**: `mem_1CcBVgNRhWLy9vZdTmAAt6`（atlas vantage-point、思想の記録）。本 doc は repo に残る正典。
> **系譜**: 「VP Lane Registry 統合 Backbone 設計」(2026-04-23 spark) / 「Lane Lifecycle Architecture milestone」(2026-05-06, VP-129) の延長線上にある。
> **関連 doc**: [doc 12](12-stand-architecture.md)（Stand framework）/ [doc 17](17-port-stability-and-msgbox-isolation.md)（現 port 構成）/ [doc 19](19-canvas-stack-model.md)（PP Canvas）/ [doc 23](23-bastet-justice-stand-wiring.md)（Bastet/Justice）

本 doc は **実装仕様ではなく構想（vision）** を規定する。「VP にとって美しく強い構造とは何か」を、使命
——**道なき道をゆく研究者・開発者・学生に最高の環境を**——から問い直した結論。
移行の手順・コストは別途、段階的に払う（§8）。急がない。

---

## 1. 発端 — バグは設計の声だった

具体的なバグから始まった: **ROTO-CONTROL の LCD に並ぶ project 順序が、vp-app sidebar の順序と一致しない**。

原因を辿ると、順序の source が **3 重化**して乖離していた:

| source | 持ち主 | 実体 |
|--------|--------|------|
| `currents_order` | vp-app（window ローカル session） | D&D で並べ替えた表示順 |
| `project_order` | daemon（TheWorld） | projects.kdl の登録順 |
| `lane_registry` | daemon（SP push の cache） | 各 SP が登録した lane 群 |

これは「順序を直す」話ではなく、**分散構成（SP が project ごとに別プロセス + ポートを持つ）の症状**だった。
バグは「この構造は本当に美しいか」を問うていた。本 doc はその問いへの答え。

---

## 2. 現状構造の問題 — 払っているコスト

現状: `TheWorld 👑 (:32000)` が `Star Platinum ⭐ (SP, :33000-33010)` を **project ごとに別プロセス** spawn し、
**Push（QUIC registry 自己登録）+ Pull（port scan）** の二重パスで reconcile。SP は自分の lanes を
`lane_registry` に push（= World 側の cache）。

問題は、この分散機構が **VP の実態に対して過剰**なこと:

- **SP は重い処理をしていない**。Claude 本体は tmux の別プロセス（`tmux new-session -c <dir>`）、performer は
  git worktree。fault / resource isolation の主役（自律 agent・ユーザーコード実行）は **すでに SP の外** にいる。
  SP は薄い orchestration 層にすぎない。
- **分散管理機構（Push/Pull/reconcile/`lane_registry` cache）は、独立プロセス艦隊を統べるためだけ** に存在する。
- cross-project は常に逆流作業（N 個の SP から掻き集める）。ポート天井（33000-33010 = 11）。N 個の tokio/axum/quic ランタイム。
- **VP は単一ユーザーの個人開発環境**（Creo Memories = サービス とは方針が違う、CLAUDE.md 参照）。
  なのに SP 構成は **multi-tenant サーバーの分解パターン** を背負っている。残る Pros の大半が借り物。

> 分散構成が正当化されるのは multi-tenant・untrusted・独立スケールの世界。VP はそのどれでもない。

---

## 3. 四本柱 — World の state model

美しく強い構造の核となる 4 つの原理。

| 柱 | 一言 |
|----|------|
| **Lane = atom** | 触れる / 寿命を持つ / アドレス可能な単位。project でも SP でもなく **Lane** が原子 |
| **project = namespace** | flat な Lane 空間の区分け（view の関心事）。git に縛らない |
| **agent = projection** | `claude --resume <cc_session_id>` で永続 descriptor から再構成される派生物 |
| **永続が所有する** | 寿命の所有者はプロセス instance ではなく、**永続化された state 木** |

### 3.1 含有 = 所有 = ライフサイクル

強い構造の根本: あらゆる資源は **ただ一つのライフサイクル所有者** を持ち、所有が入れ子になる。

```
World ⊃ Project(namespace) ⊃ Lane ⊃ Stand ⊃ { agent, tmux, pty, worktree }
```

木のどのノードを倒しても、その下が **漏れなく綺麗に畳まれる**。orphan も zombie もない。
今の構造はこれを破る——tmux が Lane より長生きし、`lane_registry` が source から乖離する。
**所有 = 含有 = 寿命** に揃えるのが「強い」。

### 3.2 Lane = atom / project = namespace

ユーザーが実際に触れている単位は project でも SP でもなく **Lane**（探索の一筋。agent と view を伴う作業線）。
ROTO で選ぶのも、active を切り替えるのも、「lane を楽器にする」のも Lane。ならば構造も Lane 中心に。

`project = namespace` から自然に降りてくる帰結:

1. **cross-project が特別でなくなる** — Lane 空間は最初から一つの flat な空間。project はその区分け。
   「全 project の Lane を見る」= flat 空間をそのまま並べるだけ。N 個の SP から掻き集める作業は、
   Lane が per-project プロセスに閉じ込められていたからこその逆流だった。
2. **conductor / performer が Lane 間の「役割」になる** — project の備品ではなく **role**。
   namespace を跨いで conductor が performer を率いることすら自然（cross-project refactor, MARU × VP）。
3. **namespace は view の関心事。state/process の境界ではない** — Lane 集合が唯一の state、
   namespace は「人間のためにどう束ねるか」。順序問題も「flat な Lane 集合に対する複数の ordering view」に還元。
4. **namespace を git repo に縛らない** — project は namespace の一種（git-backed なやつ）。
   `scratch` / テーマ別 / cross-cutting な meta 用 namespace も自由。pioneer は探索を repo 境界に押し込めたくない。

### 3.3 agent = projection / 永続が所有する

ここが今回の核。ライフサイクルの所有者は **World というプロセス instance ではなく、永続化された state（Lane の木）**。

- World プロセスを再起動しても Lane は死なない。World は boot 時に永続 Lane 木を読み、**agent を `--resume` で再構成**する。
- `claude --continue` / `--resume <session-id>` は **agent プロセスを「真実の source」から「真実の projection」に変える**。
  Claude が会話をディスクに永続化しているから、走っている agent は Lane descriptor（cwd / stand / `cc_session_id`）から
  **いつでも無損失で再構成できる派生物** になる。→ agent も「一つの真実、多くの眺め」（§4.2）の世界に入る。

> World 再起動は **re-animate であって reset ではない**。生きているものは何一つ「真実の source」ではなく、すべて永続 state の projection。

**継続の二層**（re-attach 案を廃し、`--resume` を主軸に）:

- **default = cold（`--resume`）** … agent は ephemeral、Lane は data。reboot / crash も無損失で越える。
  lazy（World boot で全 agent を起こさず、触れた Lane だけ起こす）。地ならしは `cc_session_id`（R3-b）が既にある。
- **opt-in = hot（detached tmux）** … 長い自律実行を中断したくない時だけ detach し、World 再起動を越えさせ再 discovery。

唯一のトレードオフは **in-flight の損失**（turn 途中で殺すと最後の turn 境界から戻る）。default を cold にし、
hot を opt-in に降格することで吸収する。これは **「初期は Agent を止めたくなかった → もうその時期は終わり」の構造化**。

**tmux の役割が変わる**: 不死身（常時 ON の足場）→ (default) Lane 寿命に縛られた **I/O アンカー**（PTY・cwd）
+ (opt-in) hot 継続のための **detach 基盤**。

---

## 4. 三層構造 — 実行構造（2026-06-19 確定）

### 4.1 SP の redefine — 状態保持(N) → interface(1)

| | 旧 SP | 新 SP |
|---|---|---|
| 数 | per-project（N 個） | **単一（1 個）** |
| 役割 | 状態保持プロセス | **user の World への interface / presence** |

```
TheWorld (daemon, durable)
  └ 何が在るか: Lane 木 / namespace / descriptor(cwd, stand, cc_session_id) / default order
        ▲
        │  SP : The World
        ▼
Star Platinum / SP (単一 presence, 永続)
  └ どう engage しているか: active Lane / arrangement(order) / focus / 入力 relay
        ▲
        │  projection
        ▼
Views (N, stateless)
  └ GUI windows / ROTO / TUI — 同じ SP を描くだけ。状態を持たない
```

### 4.2 状態は 2 箇所にしか無い

- **World (durable / 客観)** … 何が在るか。Lane 木・namespace・descriptor・default order。
- **SP (presence / 主観・永続)** … どう engage しているか。active Lane・arrangement(order)・focus・入力 relay。
- **Views (stateless)** … GUI windows / ROTO / TUI。**状態ゼロ**。同じ SP の projection。

これが「一つの真実、多くの眺め」。command が state を変え、state が view に delta を流す。**単方向**。

### 4.3 確定した判断

- **SP = per-presence**（1 user = 1 SP）。
- **複数 window は別物だが「同じ SP の projection」** → window はローカル状態を持たない（`currents_order` のような per-window 状態は消滅）。
- **order / active Lane は SP(presence)** に置く → 全 modality（sidebar・ROTO・TUI）が自動一致、SP 永続化で arrangement も復元。
- **World = 客観（何が在る）／ SP = 主観（どう engage している）** の分離。TheWorld は UI 概念を知らず **headless で回る**
  （app を閉じても Lane は World で生存、SP が後で再 attach）。
- **SP の物理位置**: GUI process 内蔵だと ROTO CLI（別プロセス）が通せない。**GUI 寿命に縛られない薄い session tier**
  （小さな常駐 or World 内の session object）に GUI / ROTO / TUI が attach する。
- 将来「**N presence が 1 World に対峙**」= remote / collab / MARU へ素直に伸びる。

---

## 5. メタファー整合（JoJo）— 詩は正しい設計を指す

VP 固有の財産は JoJo メタファーが **思考の道具** であること。美しい構造とは、
**メタファーを理解すること = システムを理解すること** になる状態。

- **Star Platinum ⭐ は本来 Jotaro の「個人の」近距離スタンド** — 1 体、持ち主のもの。
  "per-project に 12 体" は元から比喩破綻していた。「user の World への単一 avatar」なら Star Platinum そのもの。
- **"Star Platinum : The World"（あの融合）= SP が TheWorld に作用する形**。
  アーキ中核の interaction（SP ↔ TheWorld）が、原作最強の融合そのままの名になる。
- **TheWorld 👑 (DIO) = 時を止める領域 = 永続 world-state** / **Star Platinum (Jotaro) = 意志の延長 = 単一 presence**。
  役割分担が canon と一致する。

> 現状の SP は「ポートの彼方の遠隔プロセス」。近距離パワー型のメタファーに反していた。in-process / 単一 presence 化が詩を回復する。

---

## 6. 構造的に消える痛み

| かつての痛み | なぜ消えるか |
|---|---|
| ROTO ≠ sidebar の順序ズレ | 両方 1 つの SP の projection |
| 2 window で order 割れ | window は状態を持たない |
| cross-project 集約の逆流 | flat な Lane 空間を World が一元保持 |
| `lane_registry` の cache 乖離 | cache が要らない（state は 2 箇所のみ） |
| orphan / teardown 漏れ | 含有=所有=寿命 ＋ agent=projection(`--resume`) |

> 発端のバグ（`currents_order` が per-window）は、この設計では **書きようがない**。バグは設計の声だった。

---

## 7. 残りの糸 / Open Questions

- **addressing** — flat な Lane 空間の名前体系（`namespace/lane`、conductor は予約名 or role tag、グローバル一意 id をどう持つか）。
- **role 一般化** — conductor / performer を「Lane に付く役割」に開く（namespace 横断の orchestration が書けるように）。
- **namespace の非 git 化** — project 以外の束ね（scratch / テーマ別 / meta）。
- **SP 物理位置の確定** — 薄い session tier の実体（小常駐 vs World 内 session object）。
- **hot 継続の再 discovery protocol** — detached tmux/agent を World 再起動後に拾い直す手順。
- **検証したい実リスク**:
  - PTY / tmux の **clean teardown を Drop で担保** できるか。
  - **World 再起動で lane / PTY state を失わない永続 + 復元**（VP-on-VP dogfood の生命線）。

---

## 8. 移行方針 — strangler、急がない

背骨の手術（port layout・reconciliation 全体・vp-app 接続モデルに波及）。一気にやらず段階的に:

1. **World を canonical 集約点に**（読み取り側）— order・lane list の canonical を World/単一 presence が持つ。
   vp-app の cross-project view を 1 本に。**目先の順序バグは「D&D → 単一 presence に order を寄せる（≒ canonical 永続化）」の最小修正で Phase 1 の頭出し**。
2. **state を World へ移送** — LanePool / Canvas を project-keyed で集約。SP プロセスを optional 化。cwd は spawn 時明示
   （tmux `-c` / worktree で既に大半は明示済み）。
3. **port / reconciliation 撤去** — World が全部ホスト。Push/Pull/`lane_registry` を退役。

> 移行コストは「後で自分たちが払えばよい」もの。先に **構造の正しさ** を確定させ、それに向かって少しずつ寄せる。

---

## 9. 使命に照らして

道なき道をゆく人に必要なもの:

1. **連続性** — 状態を失わない・いつでも続きから（`永続が所有する`）。
2. **棲む AI** — 道具ではなく環境の住人（Lane に宿る Echoes、`agent = projection`）。
3. **安全な並列** — worktree で隔離された探索の枝（performer Lanes）。
4. **透明な基盤** — ポートも registry も意識させない（分散同期の撤去）。
5. **身体性** — 触れて演奏できる（Bastet 🧲 / Justice 🌫️ を第一級の入力モダリティに、[doc 23](23-bastet-justice-stand-wiring.md)）。

四本柱と三層は、すべてこの 5 つに奉仕する。
**accidental complexity（ポート・分散同期）は、探索から盗まれた集中力。** それを構造から消すことが、最高の環境への道。
