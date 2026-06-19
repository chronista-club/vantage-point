# Design 24: VP の背骨 — daemon / app(SP) / window 三段ライフサイクル + Lane=atom 四本柱

> **Status**: 構想（段階的移行中・急がない）。**Phase 1 出荷済み（v0.26.0、order を daemon canonical に）**。
> **creo SSOT**: `mem_1CcBVgNRhWLy9vZdTmAAt6`（atlas vantage-point、思想の記録）。本 doc は repo に残る正典。
> **改訂履歴**:
> - **v1**（2026-06-19 午前）: 三層（World / SP-session-tier / Views）/ state は 2 箇所。
> - **v2**（2026-06-19 午後 ← 本版）: **三段ライフサイクル**（daemon 不死 / app(SP) 常駐 / window ephemeral）。
>   presence = app-SP の live authority ＋ daemon の durable backing（2 storage / 1 writer）。
>   namespace = backing-kind / role = relational / 連邦の芽（I1–I5）。v1 の「SP=薄い session tier」「state 2 箇所」は本版が更新。
> **系譜**: 「VP Lane Registry 統合 Backbone 設計」(2026-04-23) / 「Lane Lifecycle Architecture」(2026-05-06, VP-129)。
> **関連 doc**: [12](12-stand-architecture.md)（Stand）/ [17](17-port-stability-and-msgbox-isolation.md)（port）/ [19](19-canvas-stack-model.md)（PP Canvas）/ [23](23-bastet-justice-stand-wiring.md)（Bastet/Justice）

本 doc は **実装仕様ではなく構想（vision）** を規定する。「VP にとって美しく強い構造とは何か」を、使命
——**道なき道をゆく研究者・開発者・学生に最高の環境を**——から問い直した結論。移行は段階的に払う（§10）。急がない。

---

## 1. 発端 — バグともやもやは設計の声だった

具体的なバグから始まった: **ROTO-CONTROL の LCD に並ぶ project 順序が、vp-app sidebar の順序と一致しない**。
原因を辿ると、順序の source が **3 重化**して乖離していた（`currents_order`(vp-app ローカル) / `project_order`(daemon) / `lane_registry`(SP push の cache)）。
これは「順序を直す」話ではなく、**分散構成（SP が project ごとに別プロセス + ポート）の症状**だった。

もう一つの声は **もやもや**だった: 「project が常に git repo に縛られている。探索の自由度を狭めていないか？」。
バグも、この漠とした違和感も、**「この構造は本当に美しいか」を問うていた**。本 doc はその問いへの答え。

---

## 2. 現状構造の問題 — 払っているコスト

現状: `TheWorld 👑 (:32000)` が `Star Platinum ⭐ (SP, :33000-33010)` を **project ごとに別プロセス** spawn し、
**Push（QUIC registry 自己登録）+ Pull（port scan）** の二重パスで reconcile。SP は自分の lanes を `lane_registry` に push。

この分散機構が **VP の実態に対して過剰**:

- **SP は重い処理をしていない**。Claude 本体は tmux の別プロセス、performer は git worktree。fault/resource isolation の主役は **すでに SP の外**。SP は薄い orchestration 層にすぎない。
- 分散管理機構（Push/Pull/reconcile/`lane_registry` cache）は **独立プロセス艦隊を統べるためだけ** に存在する。
- cross-project は常に逆流作業（N 個の SP から掻き集める）。ポート天井（11）。N 個の tokio/axum/quic ランタイム。
- **VP は単一ユーザーの個人開発環境**（Creo Memories = サービス とは方針が違う）。なのに SP 構成は **multi-tenant サーバーの分解パターン** を背負っている。残る Pros の大半が借り物。

> 分散構成が正当化されるのは multi-tenant・untrusted・独立スケールの世界。VP はそのどれでもない。

---

## 3. 四本柱 — World の state model

美しく強い構造の核となる 4 つの原理。

| 柱 | 一言 |
|----|------|
| **Lane = atom** | 触れる / 寿命を持つ / アドレス可能な単位。project でも SP でもなく **Lane** が原子 |
| **project = namespace** | flat な Lane 空間の区分け。git に縛らない（namespace は **backing-kind** を持つ、§5） |
| **agent = projection** | `claude --resume <cc_session_id>` で永続 descriptor から再構成される派生物 |
| **永続が所有する** | 寿命の所有者はプロセス instance ではなく、**永続化された state 木** |

### 3.1 含有 = 所有 = ライフサイクル

```
World ⊃ Namespace ⊃ Lane ⊃ Stand ⊃ { agent, tmux, pty, ground(worktree/dir) }
```

どのノードを倒しても、その下が **漏れなく綺麗に畳まれる**。orphan も zombie もない。
**所有 = 含有 = 寿命** に揃えるのが「強い」。実行 ground（cwd）は **daemon が唯一の provision/reclaim 主体**（§5.3）。

### 3.2 Lane = atom / project = namespace

ユーザーが実際に触れている単位は project でも SP でもなく **Lane**（探索の一筋）。
ROTO で選ぶのも、active を切り替えるのも、「lane を楽器にする」のも Lane。ならば構造も Lane 中心に。

`project = namespace` から降りてくる帰結:

1. **cross-project が特別でなくなる** — Lane 空間は最初から一つの flat な空間。namespace はその区分け。
2. **conductor / performer は namespace を跨ぐ role**（§6）。
3. **namespace は view の関心事。state/process の境界ではない**。順序問題も「flat な Lane 集合への複数 ordering view」に還元。
4. **namespace を git repo に縛らない** — project は **backing が git な namespace の一種**（§5）。

### 3.3 agent = projection / 永続が所有する

ライフサイクルの所有者は **プロセス instance ではなく、永続化された state（Lane の木）**。

- daemon を再起動しても Lane は死なない。daemon は boot 時に永続 Lane 木を読み、**agent を `--resume` で再構成**する。
- `--resume <session-id>` は **agent を「真実の source」から「真実の projection」に変える**。走っている agent は descriptor（cwd / stand / `cc_session_id`）から **いつでも無損失で再構成できる派生物**。

> daemon 再起動は **re-animate であって reset ではない**。

**継続の二層**:
- **default = cold（`--resume`）** … agent は ephemeral、Lane は data。reboot/crash も無損失で越える。lazy（触れた Lane だけ起こす）。
- **opt-in = hot（detached tmux）** … 長い自律実行を中断したくない時だけ detach し、daemon 再起動を越えさせ再 discovery。

この二層が §4.4 の「daemon = 常時 substrate ＋ per-lane opt-in engine」に直結する。

---

## 4. 三段ライフサイクル — 実行構造（v2 確定）

v1 は World / SP-session-tier / Views の「三層」だった。**SP の物理位置を「app プロセス」と確定**すると、
寿命が **三段** になり、構造が一段シンプルになる。

### 4.1 三段の寿命

```
┌ daemon: TheWorld 👑 ──────────────┐ ⟷双方向⟷ ┌ app: Star Platinum ⭐ (SP) ┐
│ 環境/OS/外部 を用意（地盤）         │  message  │ lane を live にまとめる      │
│ + durable truth (Lane木/descriptor) │          │ agent 駆動 / I/O / 描画      │
│ + presence の durable backing      │          │                            │
│ 不死・headless                     │          │ 常駐（0 window でも生存）    │
└────────────────────────────────────┘          └────────────────────────────┘
         ▲ attach（local server / 連邦時は QUIC）
   window 1..N（ephemeral・stateless）/ ROTO / TUI
```

| tier | 寿命 | 持つもの | 死んだ時 |
|------|------|----------|----------|
| **daemon (TheWorld)** | 不死 | truth(Lane木/descriptor) / 環境 / presence backing | 基本死なない。再起動 = re-animate |
| **app (SP)** | 常駐〜quit（0 window でも生存） | **live presence**(active/order/focus) / まとめ / agent 駆動 | snapshot を daemon に残し復元可 |
| **window** | ephemeral | 何も持たない（純 View） | 無痛 |

> **daemon と app(SP) は対峙する 2 者**。融合（World 内に SP を畳む）でも、別 daemon（薄い独立 SP 常駐）でもない。
> **app プロセスに宿る SP** が、不死の daemon と双方向で向き合う。これが VP の成立条件。

### 4.2 presence の住所 — 2 storage / 1 writer

- **presence の live authority = app-SP**（active lane / lane の live なまとめ）。app が走る間はここが真実。in-process だから速く、「**app が lane をまとめる**」が文字通りになる。
- **daemon = presence の durable backing**（不透明 snapshot として保管）。app quit / daemon 再起動 / reboot を越えるため。daemon は中身を **解釈しない**（純客観を維持）。
- window / ROTO / TUI は **app-SP に attach** する client。

**presence は一枚岩ではない — 要素ごとに intent（surface 間で一致してほしいか）で tier が決まる**（§12-C の解）:

| presence 要素 | tier | 共有の意図 |
|---|---|---|
| order / arrangement | World | 全 surface 一致＋durable（発端バグの本体） |
| active lane | SP | **当面は単一**（全 surface の default 基準。follow/pin は将来 option） |
| surface target / focus / scroll / zoom | surface | 割れてよい（ephemeral or per-surface pref） |

判定の鍵: 「surface 間で一致してほしいか？」yes-always→World / yes-while-engaged→SP / no（割れるのが機能）→surface。発端バグは『一致すべき order を surface ローカルに置いた』誤配置で、focus を surface に置くのは正配置——同じバグにならない。**presence は未知数が大きい領域なので、当面は active lane 単一の最小実装に留める**（surface target / follow/pin は dogfood の声を聞いてから足す。surface tier があるので後付けは extension）。

**「2 箇所に戻った」のでは？** 違う。旧 `lane_registry` バグは「**2 つの独立 authority が並行 write**」して乖離した。本版は「**live authority は常に 1 つ**（走行中=app-SP、quit 中=daemon snapshot）、daemon backing は write-through follower」。
editor の buffer(真実) と autosave file(backing) の関係——**2 storage / 1 writer は乖離しない**。

| イベント | window | app-SP | daemon |
|---|---|---|---|
| window 1 枚閉じ | 消 | 生存・presence 無傷 | 無関係 |
| 全 window 閉じ | 全消 | **生存(0-window)・無傷** | 無関係 |
| app quit | — | 消・直前 snapshot を daemon へ | snapshot 保持 |
| app 再起動 | 新 window | daemon から復元→authority | snapshot 手渡し |
| daemon 再起動(app 走行中) | — | presence を新 daemon へ再 push | disk から truth re-animate |
| reboot | — | — | boot→復元、agent は触れるまで cold |

→「永続が所有する」が **各段で効く**: window 喪失=無痛 / app quit=復元可 / daemon 再起動=re-animate。

### 4.3 SP = app プロセス（ROTO 問題の解決）

v1 は「GUI 内蔵だと ROTO CLI（別プロセス）が通せない」ので SP を別 session tier に置こうとした。
だがその懸念は **SP が *window の中* に閉じる前提**だった。**SP = app プロセス（window から独立・0 window でも生存・local server を持つ）** なら：

- **ROTO は app-SP に attach できる**。**「app が 0 window でも生存する」ことが、ROTO を SP の peer にする enabler**。
- window も ROTO も同一 app-SP の presence を共有 → **order が割れようがない**（発端バグが構造的に書けない）。
- v1 の「薄い session tier」の正体は、別 daemon でも World 内 object でもなく **app プロセスそのもの**だった。

### 4.4 daemon = 常時 substrate ＋ per-lane opt-in engine（= 「SP optional」の正体）

「app quit 後、daemon は engine か vault か」は二択ではなく、既決事項から自動的に決まる:

| app quit 中、daemon は… | 振る舞い | 強制するもの |
|---|---|---|
| **常に** wire 受信＋store（local＋連邦）/ peer-World link 維持 / truth＋presence backing 永続 / ground 保管 | post office ＋ vault | home-World authority(I2) と連邦 |
| **lane ごと** cold(default)=dormant→`--resume` / hot(opt-in)=detached tmux 継続 | 選択的 engine | cold/hot 二層（§3.3） |
| **やらない** active/order/focus/ROTO 駆動 | engagement は app-SP の仕事 | ROTO は app-SP に attach |

→ **daemon は常時 substrate（post office＋vault＋連邦＋ground 保管）、その上に per-lane の opt-in engine（hot）**。
これが **「SP optional」の正体**: World は SP 無しで回り、SP(app) が engagement を上に足す。

### 4.5 確定判断

- **SP = per-presence（1 user = 1 SP）**、SP の物理位置 = **app プロセス**（window 独立）。
- **window は stateless**（per-window state は消滅）。複数 window は同一 app-SP の projection。
- **presence（order/active/focus）= app-SP live authority ＋ daemon durable backing**。
- **daemon は headless で回る**（app を閉じても truth/presence/hot lane は生存）。
- **将来 N presence が 1 World、さらに N World が peering**（§7）へ素直に伸びる。

---

## 5. namespace = backing-kind ＋ 実行 ground

### 5.1 namespace は backing-kind を持つ

「namespace == git repo」だった本当の理由は **path = identity**（現状 `normalize_path_key()` が repo path をキーに Lane を識別）。
位置独立 id（§7 I1）で **id ≠ path** にした瞬間、namespace は repo を必要としなくなる。git 束縛は path=identity の **副作用**だった。

> **namespace = backing-kind を持つ論理パーティション。project = backing が git な namespace（数ある kind の一つ）。**

### 5.2 kind は開いた registry にする（自由度の本体）

固定 enum にすると、また天井を作る。`provision(kind) -> ground` を **拡張点**にすれば、種は増やせる:

| backing-kind | 実行 ground（daemon が provision） | git? |
|---|---|---|
| **git**（= 現 project） | worktree（performer）/ checkout（conductor） | versioned |
| **scratch** | 確かな作業 dir（例 `data/grounds/<id>/`、後で `git init` 可） | no |
| **theme**（横断テーマ） | 確かな workspace dir（複数 repo を参照/内包しうる） | no |
| **meta**（cross-cutting） | workspace dir（MARU×VP 等の跨ぎ作業） | no |
| **device** / **dataset** / **remote-projection** … | 種別ごと（device-backed lane = 「lane を楽器にする」の根） | — |

**連邦と収束する**: peer World が publish した namespace は、ローカルでは **backing-kind = `remote-projection`** として現れる。
「remote な Lane」は特別扱いではなく **ただの一 backing-kind**。namespace・連邦・I1 が同じ機構に landing する（§7）。

### 5.3 実行 ground は常に concrete（id ⟂ location）

**確かなディレクトリ（計算空間 / 実行環境）はほぼ全 Lane の default 不変条件**。git かどうかは「その cwd が worktree か否か」の違いであって、cwd の有無ではない。地盤なし（純 canvas/会話 lane）は稀な例外。

- **identity ⟂ location**: id は位置独立（I1）、しかし実行は concrete な cwd を要る。lane は path を **持つ**が、path で **識別されない**。
- **backing-kind は進化できる**: scratch で始めた lane を、結晶化した時点で `git init` して git-backed に **昇格**できる——**同じ id のまま**。「道なき道」をまず scratch で探索し、道が見えたら repo に束ねる。
- **teardown 一本化**: ground を provision する唯一の主体が daemon なら、reclaim（worktree remove / dir 削除 / tmux kill）も daemon に集約 → daemon 側 ground ハンドルの Drop で含有=所有=寿命を担保（§7 teardown リスクの構造的解消）。

---

## 6. role = relational

conductor / performer は lane の **type ではなく、lane 間の relationship**。「lane X が lane Y を conduct している」。

- 一つの lane が、ある orchestration では conductor、別では performer になれる。
- orchestration が **namespace を跨ぐ**（meta namespace の conductor が git namespace の performer 群を率いる = cross-repo refactor）。
- 連邦で **World を跨ぐ**（home World の conductor が peer World の performer を率いる、MARU×VP）。
- 各 namespace の「主たる lane」は **default conductor の予約席**があるだけ。本質は relation。

> **relational は simple を内包する**。「namespace に 1 conductor」は relational の特殊形。普段は素朴に使えて、天井は cross-project/cross-World まで開く。dogfood で変えるのは *使い方* であって *構造* ではない。
> 「こんなの誰もやっていない」——だから触って使って磨く（VP の dogfooding 方針そのもの）。

---

## 7. World 連邦 — 位置独立 identity と home-World authority

「N presence が 1 World」の更に先、**N World が peering する**地平。Phase 2 の後だが、**芽を Phase 2 で植える**かどうかを今決める（I1/I2 を植える＝確定、§10）。

### 7.1 一本の原理 — 2 storage / 1 writer の相似拡大

> **すべての Lane / SP は、ただ一つの home World を持つ（lifecycle authority）。連邦は read-only projection を共有し、mutation は home World に routing する。co-ownership を作らない。**

同じ規律が三段の相似形で効く（split-brain が原理的に起きない）:
machine 内（buffer/autosave）→ machine 内 IPC（app-SP/daemon backing）→ machine 間（home World/peer projection）。

### 7.2 メッセージング = 連邦化した wire 一本

SP↔SP（presence↔presence）も Lane↔Lane（work↔work）も **別プロトコルを作らず wire 一本**に乗せる:
`wire send <world>/<ns>/<lane>` が remote なら home World へ forward。既存の store/inbox/thread/ack/nudge に
**境界 in/out queue を足すだけ**。`agent = projection` が enabler——wire は live process でなく永続 descriptor の
inbox に届き、home World が受信時 `--resume` で起こす（remote の cold lane も messageable）。

**スムーズの三本柱**: ① store-and-forward（peer offline でも失われない・ブロックしない）/ ② 位置透過 ＋ 正直な継ぎ目（手触りは local 同等、遅延/offline は隠さず surface）/ ③ presence は home World にだけ繋ぐ（remote 接続は World の仕事）。

### 7.3 addressing / transport / topology

- **address** = `world/namespace/lane`（local は world 省略）。global 一意 id は I1 が供給。
- **transport** = 既存 QUIC（Unison 33100+）を World 間へ。connection migration が物理 fleet の roaming に効く。
- **topology** = trusted peer の mesh（手動 peering = git remote 風）を base、Creo ID を discovery/relay の optional 層に。
- **二つの collab モード（両立可）**: 連邦（N World peering、自律/offline 可）/ 共有 World（N SP → 1 World、強整合）。

### 7.4 仕込む不変条件（後付けが高価な芽だけ Phase 2 で植える）

| # | 不変条件 | 今のコスト | 後付け | Phase 2 採用 |
|---|---|---|---|---|
| **I1** | 安定・位置独立 id（World/Lane/SP に global id、local は world 省略） | 小 | **大** | ✅ |
| **I2** | home-World single authority ＋ projection 規律 | 極小（既存規律の延長） | 大 | ✅ |
| **I3** | wire は address-routed 単一基盤（locality 仮定の side-channel 禁止） | 小 | 中 | Phase 2〜3 |
| **I4** | message は descriptor に届く（`--resume` で起こす） | 既に pillar | — | 済 |
| **I5** | transport 抽象（local-IPC / remote-QUIC 両対応の interface 裏に） | 小（QUIC 既存） | 大 | Phase 2〜3 |

**I1・I2 が「今安い・後高い」**。これを植えれば連邦は **rewrite ではなく extension** になる。

---

## 8. メタファー整合（JoJo）— 詩は正しい設計を指す

- **Star Platinum ⭐ は Jotaro の「個人の」近距離スタンド** — 1 体、持ち主のもの。"per-project に 12 体" は比喩破綻だった。「user の World への単一 avatar（= app に宿る presence）」なら Star Platinum そのもの。
- **承太郎 vs DIO の対峙 = daemon(TheWorld) と app(SP) の対峙**。アーキ中核の interaction（SP ↔ TheWorld の双方向）が、原作の構図に一致。
- **TheWorld 👑 (DIO) = 時を止める領域 = 永続 world-state（不死 daemon）** / **Star Platinum (Jotaro) = 意志の延長 = app に宿る単一 presence**。
- **"Star Platinum : The World"（あの融合）= SP が TheWorld に作用する特異な瞬間**（presence が durable truth に手を入れる）。

> 旧 SP は「ポートの彼方の遠隔プロセス」で近距離パワー型のメタファーに反していた。app プロセス内・単一 presence・daemon との対峙が詩を回復する。

---

## 9. 構造的に消える痛み

| かつての痛み | なぜ消えるか |
|---|---|
| ROTO ≠ sidebar の順序ズレ | window も ROTO も同一 app-SP の presence を共有（Phase 1 で頭出し済） |
| 2 window で order 割れ | window は stateless、presence は app-SP に一本 |
| cross-project 集約の逆流 | flat な Lane 空間を daemon が一元保持 |
| `lane_registry` の cache 乖離 | 2 storage / 1 writer（live authority は常に 1 つ） |
| orphan / teardown 漏れ | 含有=所有=寿命 ＋ ground は daemon が唯一 provision/reclaim ＋ agent=projection(`--resume`) |
| git 縛りで探索の自由度が狭い | namespace = backing-kind（id ⟂ location、kind は進化可） |

> 発端のバグ（`currents_order` が per-window）は、この設計では **書きようがない**。バグも、もやもやも、設計の声だった。

---

## 10. 移行方針 — strangler、急がない

背骨の手術（port layout・reconciliation 全体・vp-app 接続モデルに波及）。段階的に:

- **Phase 1 — World を canonical 集約点に（出荷済み v0.26.0）**
  order・lane list の canonical を daemon が持つ。目先の順序バグを「D&D → daemon canonical 永続化」で解消。

- **Phase 2 — state を World へ移送 ＋ 連邦の芽 ＋ 三段ライフサイクル**
  - LanePool authority を SP → daemon へ反転（SP は live engagement だけ残す）。
  - **SP optional 化**: daemon を常時 substrate に（§4.4）。三段ライフサイクル（§4）を実装。
  - presence = app-SP live authority ＋ daemon durable backing（§4.2）。
  - **I1（位置独立 id）＋ I2（home-World authority）を descriptor/規律に planting**（§7.4）。
  - namespace = backing-kind の **scaffold**（§5、まず git ＋ scratch から）。ground は daemon が provision/reclaim（teardown 一本化）。
  - cwd は spawn 時明示（tmux `-c` / worktree で大半済み）。

- **Phase 3 — port/reconciliation 撤去 ＋ namespace 非 git の本格化 ＋ 連邦**
  Push/Pull/`lane_registry` を退役。backing-kind を open registry に。role=relational の orchestration。I3/I5 で連邦 wire を開通。

> 移行コストは「後で自分たちが払えばよい」もの。先に **構造の正しさ** を確定させ、それに向かって少しずつ寄せる。

---

## 11. 使命に照らして

道なき道をゆく人に必要なもの:

1. **連続性** — 状態を失わない・いつでも続きから（`永続が所有する`、三段ライフサイクル各段で復元）。
2. **棲む AI** — 道具ではなく環境の住人（Lane に宿る Echoes、`agent = projection`）。
3. **安全な並列** — worktree で隔離された探索の枝（performer Lanes、ground は daemon 所有）。
4. **透明な基盤** — ポートも registry も意識させない（分散同期の撤去、client は attach するだけ）。
5. **身体性** — 触れて演奏できる（Bastet 🧲 / Justice 🌫️、device-backed namespace、ROTO は first-class client、[doc 23](23-bastet-justice-stand-wiring.md)）。

四本柱・三段ライフサイクル・backing-kind・relational role・連邦の芽は、すべてこの 5 つに奉仕する。
**accidental complexity（ポート・分散同期・git 縛り）は、探索から盗まれた集中力。** それを構造から消すことが、最高の環境への道。

---

## 12. 既知のリスク / 残る緊張（2026-06-19 自己批判レビュー）

本設計は速く・気持ちよく cohere した。**その「美しさ」自体が盲点を作る**——統一の快感は、統一を *強制* した継ぎ目を見えなくする。以下は構造が抱える既知の弱点。隠さず正典に残す。

| # | 弱点 | severity | stance |
|---|------|----------|--------|
| **A** | **「2 storage / 1 writer」は定常状態だけ綺麗、遷移が race**。app crash で最終 snapshot を失う（無損失 ⇔ 軽さのトレードオフ）/ daemon 再起動中は truth owner 不在で app が read-only に degrade / app 二重起動を構造的に防げず presence split-brain | 🔴 | **design-for**: sync cadence・crash 復旧（WAL 的）・single-writer lease を Phase 2 で明示設計 |
| **B** | **daemon が god-object ＝ SPOF**。SP の fault 隔離（1 project 落ちても他は生存）を simplicity と引き換えに捨てた。**VP-on-VP dogfood では daemon crash が開発環境ごと落とす**。書き込み途中 crash で単一 store 破損 risk | 🔴 | **accept ＋ 緩和必須**: transactional persistence ＋ crash recovery。隔離を捨てた事実を明記して持つ |
| **C** | ~~presence の過剰統一~~ → **解決（2026-06-19）**。presence は一枚岩でなく intent で三段に正配置: order→World / active lane→SP / focus・surface target→surface（§4.2）。発端バグは order の誤配置で、focus を surface に置くのは正配置＝同じバグにならない | ✅ | **解決＋最小実装**: 三段への正配置。presence は未知数が大きいので **当面 active lane は SP 単一の最小実装**。surface target / follow/pin（身体性 vision 用）は dogfood 後の将来 option（surface tier があるので後付けは extension） |
| **D** | **backing-kind「open registry」の "open" が複雑さの本体**。各 kind が provision/reclaim/teardown/再起動復元/連邦 projection を別々に要し、統一 interface が leaky になりうる。**backing 進化（scratch→git）は cwd/PTY/tmux/agent の mid-flight migration**＝クリーンに動くことが稀 | 🟠 | **accept（segment）**: Phase 2 は git＋scratch の 2 kind に絞る。in-place 昇格は約束しない（scratch を捨てて git lane を新規、で代替しうる） |
| **E** | **I1/I2 を植えるが 1 World では検証できない**。local では規律が空回り。Phase 3 で多 World 制約（id 衝突・remote ref 形式・projection 整合）が出た時、植えた id 体系が間違っていた可能性。「今安い」は *正しいものを植えれば* の話 | 🟠 | **縮小 accept**: I2（規律）は植える。**I1 は「id 欄を持つ」に留め、id の体系（format/採番/衝突解決）は Phase 3 まで決め打ちしない** |
| **F** | **「agent = projection」は Claude CLI `--resume` への外部依存**。in-flight turn を失う / CLI version 間で session 形式が変わりうる / Claude のローカル storage 次第。四本柱の一つが VP の所有しない挙動に load-bearing | 🟡 | **accept（不可避）＋ 監視**: cc_session_id を VP 側でも保持（既存）で最低限の自衛。根の依存は消えない事実として持つ |
| **G** | **ROTO は app quit で死ぬ**（app-SP に attach を選んだ帰結）。「lane を楽器にする / 物理 fleet first-class」の身体性 vision は headless でも卓が生きている ambient を望むかも。clean さのために ambient を売った | 🟡 | **認識して保留**: 将来「薄い ROTO-bridge を daemon peer に（status＋wake のみ）、フル制御は app」の二段が要るかも。売ったものを記録 |

**メタ的弱点**: 設計が数十分で揃った体験は、*検証されたから揃った* のか *揃えたいから揃えた* のか区別がつきにくい。C はその典型。**「美しい」と感じた瞬間こそ最も疑うべき**。dogfood で最初に裏切るのは、おそらく C か A。

> 骨格は強い（A/B/F は単一ユーザー前提で許容範囲、緩和策も明確）。**C は三段への正配置で解決**（§4.2、当面は最小実装）。残る最重要は **A（遷移 race）= Phase 2 実装で必ず正面から設計すべき**。
> **本リデザインの一貫した stance: 未知数の大きい領域（presence・backing-kind 進化・連邦・relational role）は、構造の天井は高く保ちつつ実装は最小に留める**——simple を内包する rich な構造を選び、rich な実装は dogfood の声を聞いてから足す。
> この §12 は「決定の確信」ではなく「持っておくべき緊張」の記録であり、dogfood の観察で更新される。
