# Design 24: VP の背骨 — daemon / app(SP) / window 三段ライフサイクル + Lane=atom 四本柱

> **Status**: 構想（段階的移行中・急がない）。**Phase 1 出荷済み（v0.26.0、order を daemon canonical に）**。
> **creo SSOT**: `mem_1CcBVgNRhWLy9vZdTmAAt6`（atlas vantage-point、思想の記録）。本 doc は repo に残る正典。
> **改訂履歴**:
> - **v1**（2026-06-19 午前）: 三層（World / SP-session-tier / Views）/ state は 2 箇所。
> - **v2**（2026-06-19 午後 ← 本版）: **三段ライフサイクル**（daemon 不死 / app(SP) 常駐 / window ephemeral）。
>   presence = **daemon-canonical command**（Model Q、§4.2; メンタルモデル「app まとめる / 対峙」とは別レイヤー）。
>   namespace = backing-kind / role = relational / 連邦の芽（I1–I5）/ daemon 堅牢化 = reconciliation-first（庭師モデル、§4.6）。v1 の「SP=薄い session tier」「state 2 箇所」は本版が更新。
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
│ + presence canonical (order/active)      │          │                            │
│ 不死・headless                     │          │ 常駐（0 window でも生存）    │
└────────────────────────────────────┘          └────────────────────────────┘
         ▲ attach（local server / 連邦時は QUIC）
   window 1..N（ephemeral・stateless）/ ROTO / TUI
```

| tier | 寿命 | 持つもの | 死んだ時 |
|------|------|----------|----------|
| **daemon (TheWorld)** | 不死 | truth(Lane木/descriptor) / 環境 / **presence canonical**(order/active) | 基本死なない。再起動 = re-animate |
| **app (SP)** | 常駐〜quit（0 window でも生存） | **live engagement runtime**（agent 駆動 / I/O relay / render / command 発行）。authoritative presence は持たない | daemon が presence を保持＝喪失ゼロ |
| **window** | ephemeral | 何も持たない（純 View） | 無痛 |

> **daemon と app(SP) は対峙する 2 者**。融合（World 内に SP を畳む）でも、別 daemon（薄い独立 SP 常駐）でもない。
> **app プロセスに宿る SP** が、不死の daemon と双方向で向き合う。これが VP の成立条件。

### 4.2 presence の住所 — メンタルモデル ⟂ 実装（Model Q）

**メンタルモデルと内部実装は別レイヤー**でよい（doc §5 が JoJo を「思考の道具」と定義する通り、VP は元来 model ≠ impl を受け入れている）。

- **メンタルモデル（人が考え・語る層）**: daemon ⟷ app(SP) が **対峙**し、**app が lane をまとめる**。order を並べ替え active を切り替えるのは app での *行為*。
- **内部実装（Model Q、最もシンプル）**: presence（order / active lane）は **daemon-canonical**。app は「reorder」「switch active」等の **command を発行して render する** だけで、**authoritative な presence を持たない**。Phase 1 で order を daemon canonical にした command パターンを、presence 全体へ拡張しただけ。

「app が lane をまとめる」は **model 層では真**（まとめ *行為* は app で起きる）。authoritative bytes が daemon に在ることは model からは見えない → **Q 実装は P モデルを裏切らず、最小の機構で実現する**。

**presence は要素ごとに intent（surface 間で一致してほしいか）で在処が決まる**（§12-C の解）:

| presence 要素 | 真実の在処（実装） | 共有の意図 |
|---|---|---|
| order / arrangement | **daemon**（command パターン、Phase 1 で実証済） | 全 surface 一致＋durable（発端バグの本体） |
| active lane | **daemon**（command パターン、当面は単一） | 全 surface の default 基準。follow/pin は将来 option |
| surface target / focus / scroll / zoom | **surface**（local） | 割れてよい（ephemeral or per-surface pref） |

判定の鍵: 「surface 間で一致してほしいか？」yes→**daemon の単一 authority（command）** / no（割れるのが機能）→surface。発端バグは『一致すべき order を surface ローカルに置いた』誤配置で、focus を surface に置くのは正配置——同じバグにならない。

**単一 authority が race を蒸発させる**: presence の真実は **daemon ただ一つ**（app は command client、写しを持たない）。だから旧 `lane_registry` バグ（2 独立 authority の並行 write）も、§12-A の sync / lease / snapshot も **構造的に発生しない**——app に「失う物・sync する物・lease する物」が無い。残る実装課題は **daemon の transactional 永続**（§12-B と共有）と **daemon 再起動中の command 再接続** だけ。

| イベント | window | app(SP) | daemon |
|---|---|---|---|
| window 1 枚 / 全閉じ | 消 | 生存（0-window、engagement runtime） | 無関係（presence 保持） |
| app quit | — | 消（authoritative state 不所持＝喪失ゼロ） | presence/truth を保持 |
| app 再起動 | 新 window | daemon の presence を render（command client 再接続） | 真実を手渡し |
| daemon 再起動(app 走行中) | — | command を buffer/retry → reconnect で復帰 | disk から truth+presence re-animate |
| reboot | — | — | boot→復元、agent は触れるまで cold |

→「永続が所有する」が **daemon 一点で効く**: presence の真実は不死 daemon にあり、app/window/ROTO は projection。

### 4.3 SP = app プロセス（ROTO 問題の解決）

v1 は「GUI 内蔵だと ROTO CLI（別プロセス）が通せない」ので SP を別 session tier に置こうとした。
だがその懸念は **SP が *window の中* に閉じる前提**だった。**SP = app プロセス（window から独立・0 window でも生存・local server を持つ）** なら：

- **presence は daemon-canonical**（§4.2 Model Q）なので、ROTO は **daemon の presence を読み・command を送る**。window も ROTO も **同一 daemon の単一 presence を共有** → order が割れようがない（発端バグが構造的に書けない）。
- これは §12-G も緩和する: presence が不死 daemon にあるので **ROTO は app quit でも生きる**（ambient 身体性が自然に出る）。
- app-SP は engagement runtime（agent 駆動 / I/O relay / render）として 0 window でも生存。v1 の「薄い session tier」の正体は、別 daemon でも World 内 object でもなく **app プロセスそのもの**だった。

### 4.4 daemon = 常時 substrate ＋ per-lane opt-in engine（= 「SP optional」の正体）

「app quit 後、daemon は engine か vault か」は二択ではなく、既決事項から自動的に決まる:

| app quit 中、daemon は… | 振る舞い | 強制するもの |
|---|---|---|
| **常に** wire 受信＋store（local＋連邦）/ peer-World link 維持 / **truth＋presence canonical 永続** / ground 保管 | post office ＋ vault ＋ presence authority | home-World authority(I2) と連邦 |
| **lane ごと** cold(default)=dormant→`--resume` / hot(opt-in)=detached tmux 継続 | 選択的 engine | cold/hot 二層（§3.3） |
| **やらない** render / agent I/O relay / surface-local focus | engagement runtime は app-SP（quit 中は不在） | ROTO は daemon の presence を共有（app quit でも可） |

→ **daemon は常時 substrate（post office＋vault＋連邦＋ground 保管）、その上に per-lane の opt-in engine（hot）**。
これが **「SP optional」の正体**: World は SP 無しで回り、SP(app) が engagement を上に足す。

### 4.5 確定判断

- **SP = per-presence（1 user = 1 SP）**、SP の物理位置 = **app プロセス**（window 独立）。
- **window は stateless**（per-window state は消滅）。複数 window は同一 app-SP の projection。
- **presence = daemon-canonical（command パターン、Model Q）**: order/active は daemon が単一 authority、app は command 発行＋render、focus は surface-local。メンタルモデル（対峙 / app まとめる）とは別レイヤー（§4.2）。
- **daemon は headless で回る**（app を閉じても truth/presence/hot lane は生存）。
- **将来 N presence が 1 World、さらに N World が peering**（§7）へ素直に伸びる。

### 4.6 daemon 堅牢化 — reconciliation-first（庭師モデル）

Model Q で複雑さを daemon に集約した（§12-B）。その堅牢性は **「完全な状態維持」ではなく「ゆるやかな収束」** で担保する。daemon は state を厳密に enforce する transaction monitor ではなく、truth へ向けて世話をして収束させる **庭師**。

**なぜ reconciliation-first が唯一の道か**: external resource（worktree / tmux / agent process）は **DB transaction の外**（`git worktree add` も `tmux new-session` も `claude --resume` も OS 操作）。「store と外界を同一 txn で atomic に」は原理的に不可能 → crash-safety は **desired-state(store) と actual-state(OS) を reconcile して heal** するしかない。VP は既にこの grain を持つ（health_monitor / FSEvents lane_watcher）。§12-B はそれを **durable な desired-state の上で boot 保証付きに鍛える**。

**統治原理（ゆるやか・柔軟）**: 完全保存を追わず、catastrophic loss だけ防ぐ。heal は寛容（adopt / keep / retry-then-degrade）。「ゆるやか」は妥協ではなく **ACID 不可な現実に正直**であること——単一ユーザー dev tool の VP にはそれが美徳（厳密保存を *演じる* と、不可能と戦って複雑さだけ膨らむ＝§12-A で機構を足そうとした罠）。

**lifecycle state machine（durable）＝ 軽量 WAL**: `provisioning` / `ready` / `destroying` / `dead` を store に durable 記録。中間状態が「何が in-flight か」を語る = 別 WAL ファイル不要。**intent-first bracket** で external を挟む:

```
create:  txn{descriptor + provisioning} → external{ground provision}    → txn{ready}
destroy: txn{destroying}                → external{ground+tmux reclaim} → txn{remove}
```

external 操作は **idempotent**（再実行安全）にして crash 後 retry 可能に。intent を先に durable 記録するから、倒れても reconcile が「作りかけ / 壊しかけ」を判別して完了 or rollback できる（external-first だと orphan を heuristic で当てるしかない）。

**boot reconcile — desired × actual の heal**（boot で必ず 1 周 ＋ continuous）:

| store state | actual ground | heal |
|---|---|---|
| `provisioning` | 在る | ready に完了 |
| `provisioning` | 無い | retry 1 回 → 失敗で `dead` |
| `ready` | 在る | ok（agent は触れるまで cold） |
| `ready` | 外部で消えた | `dead`（user の rm を尊重、勝手に作り直さない） |
| `destroying` | 在る | reclaim 完了 → descriptor 削除 |
| `destroying` | 無い | descriptor 削除（reclaim 済） |
| descriptor 無し | orphan dir 在る | **adopt**（descriptor 復元、VP の FSEvents grain） |
| `dead` | 任意 | 保持（inspection / `--resume` 可、ground は当面残す） |

**txn / 衛生 / durability tier**:
- transaction は **store 内 multi-write のみ**（`replace_all_projects` の DELETE→import を atomic に等）。external を跨ぐ部分は txn 不可 → reconcile が担う（役割分担）。
- Whitesnake は **temp+rename+fsync** で atomic write（現状の直書きは truncation リスク）。
- durability tier: **truth(descriptor) / wire = 堅く durable** ／ **presence(order/active) = tail-loss 許容**（crash で直前 reorder を失うのは可）／ **live process(agent/PTY) = projection で再構成**（cold=`--resume` / hot=detached tmux 再 discover）。

> §12-B（daemon = god-object / SPOF）の代償は受容。緩和は本節の crash-recovery——「倒れても綺麗に re-animate する庭」。これは「永続が所有する」の crash 版。

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

> **現時点の判断（2026-06-20、git-uniform を保つ）**: **非 git backing-kind（scratch / theme / meta / device …）は Phase 3 へ deferred**。非 git は「worktree でない ground・repo-less addressing・registry の非 git entry」という **二つ目のルールセット** を丸ごと持ち込み、難易度が現状の tradeoff に見合わない（dogfood で「別ルールが入る」摩擦を確認）。**Phase 2 は git-uniform**。当面の scratch 空間は **loose に使う git repo を project 登録**して達成（dogfood では `~/repos/nexus` が既にこの役割、sidebar 常駐）——別機構ゼロで worktree がそのまま効く。非 git は device/dataset/remote が揃い「別ルールセットを持つ価値」が立った時にまとめて入れる（= 作る前に "作らない" を選べた、§12-D）。

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

> **I5 の到達点 = control-plane の Unison(QUIC) 一本化**（2026-06-20 補足）。現状の transport は混在: app↔daemon = HTTP `/api/world/*`（projects CRUD / reorder / set_active_lane …）、SP↔daemon = QUIC registry。I5 の transport 抽象を噛ませ、**HTTP control-plane を層ごと一括で QUIC へ sweep** する（piecemeal にせず control-plane 全体を一度に＝presence/order 等を個別 Unison 化しない）。これは初日の「**World ⟷ SP の双方向 messaging で成り立つ**」backbone を transport 層で実現するもの。Phase 3。

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
| 2 window で order 割れ | window は stateless、presence は daemon に単一 authority |
| cross-project 集約の逆流 | flat な Lane 空間を daemon が一元保持 |
| `lane_registry` の cache 乖離 | presence は daemon-canonical 単一 authority（app は command client、写しを持たない） |
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
  - presence = daemon-canonical command パターン（Model Q、§4.2）。app は command 発行＋render（authoritative presence を持たない）。
  - **I1（位置独立 id）＋ I2（home-World authority）を descriptor/規律に planting**（§7.4）。
  - **git-uniform を維持**（非 git backing-kind は Phase 3 へ deferred、§5 の判断 2026-06-20）。ground は daemon が provision/reclaim（worktree / checkout、teardown 一本化）。当面の scratch は loose な git repo を project 登録で（例: nexus、別機構ゼロ）。
  - cwd は spawn 時明示（tmux `-c` / worktree で大半済み）。

- **Phase 3 — port/reconciliation 撤去 ＋ control-plane の Unison 一本化 ＋ namespace 非 git ＋ 連邦**
  Push/Pull/`lane_registry` を退役。**control-plane を Unison(QUIC) に一本化**（app↔daemon の HTTP `/api/world/*`＝reorder / set_active_lane 等を I5 の transport 抽象経由で層ごと一括 sweep、現状 HTTP+QUIC 混在 → 単一 transport、§7.4）。**backing-kind を open registry に（非 git: scratch / theme / meta / device / remote-projection を「二つ目のルールセット」としてまとめて導入）**。role=relational の orchestration。I3/I5 で連邦 wire を開通。

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
| **A** | ~~「2 storage / 1 writer」の遷移 race~~ → **解決（2026-06-19）**。presence を **daemon-canonical command（Model Q、§4.2）** に寄せ、app が authoritative state を持たない構造にした → snapshot 喪失 / split-brain / sync cadence が **構造的に発生しない**（失う物・lease する物・sync する物が無い） | ✅ | **解決＋実装最小**: lease/sync/snapshot は不要。残るは **daemon の transactional 永続（§12-B と共有）＋ daemon 再起動中の command 再接続** のみ |
| **B** | **daemon が god-object ＝ SPOF**。SP の fault 隔離を simplicity と引き換えに捨てた。Model Q で A/G も daemon に寄せた分、一層 load-bearing。**VP-on-VP dogfood では daemon crash が開発環境ごと落とす** | 🟠 | **設計確定（§4.6、reconciliation-first / 庭師モデル）**: durable desired-state ＋ heal-to-truth ＋ intent-first lifecycle（軽量 WAL）＋ atomic write。SPOF は受容、緩和＝「倒れても綺麗に re-animate」。実装は Phase 2-3 |
| **C** | ~~presence の過剰統一~~ → **解決（2026-06-19）**。presence は一枚岩でなく intent で三段に正配置: order→World / active lane→SP / focus・surface target→surface（§4.2）。発端バグは order の誤配置で、focus を surface に置くのは正配置＝同じバグにならない | ✅ | **解決＋最小実装**: 三段への正配置。presence は未知数が大きいので **当面 active lane は SP 単一の最小実装**。surface target / follow/pin（身体性 vision 用）は dogfood 後の将来 option（surface tier があるので後付けは extension） |
| **D** | **backing-kind「open registry」の "open" が複雑さの本体**。各 kind が provision/reclaim/teardown/再起動復元/連邦 projection を別々に要し、統一 interface が leaky になりうる。**backing 進化（scratch→git）は cwd/PTY/tmux/agent の mid-flight migration**＝クリーンに動くことが稀 | 🟠 | **判断（2026-06-20）: 非 git を作らない**。Phase 2 は **git-uniform**（二つ目のルールセットを持ち込まない）。scratch は loose git repo を project 登録で代替（nexus、sidebar 常駐）。非 git backing-kind は device/dataset/remote が揃う Phase 3 へ deferred。**この警告が実装直前に効いて「作る前に作らない」を選べた**実例 |
| **E** | **I1/I2 を植えるが 1 World では検証できない**。local では規律が空回り。Phase 3 で多 World 制約（id 衝突・remote ref 形式・projection 整合）が出た時、植えた id 体系が間違っていた可能性。「今安い」は *正しいものを植えれば* の話 | 🟠 | **縮小 accept**: I2（規律）は植える。**I1 は「id 欄を持つ」に留め、id の体系（format/採番/衝突解決）は Phase 3 まで決め打ちしない** |
| **F** | **「agent = projection」は Claude CLI `--resume` への外部依存**。in-flight turn を失う / CLI version 間で session 形式が変わりうる / Claude のローカル storage 次第。四本柱の一つが VP の所有しない挙動に load-bearing | 🟡 | **accept（不可避）＋ 監視**: cc_session_id を VP 側でも保持（既存）で最低限の自衛。根の依存は消えない事実として持つ |
| **G** | ~~ROTO は app quit で死ぬ~~ → **緩和（2026-06-19）**。presence が daemon-canonical（Model Q）になり、ROTO は **daemon の presence を共有** → app quit でも presence の読取/操作が生きる（ambient 身体性が自然に出る） | ✅ | **緩和**: 残るは「app quit 中の agent I/O relay（render 経路）」だけ app 依存。status/presence は daemon 直で常時可 |

**メタ的弱点**: 設計が速く揃った体験は、*検証されたから揃った* のか *揃えたいから揃えた* のか区別がつきにくい。**「美しい」と感じた瞬間こそ最も疑うべき**。A/C/G は解決し B は設計確定したが、dogfood で最初に裏切るのは、おそらく **B の実装**（庭師モデルの収束が実機で本当に綺麗か）か **D**（backing-kind の heterogeneity）。

> 骨格は強い。**A・C・G 解決、B 設計確定**（A: presence daemon-canonical で race 蒸発 / C: 三段正配置 / G: ROTO は daemon presence 共有 / B: reconciliation-first 庭師モデル §4.6）。残るは **実装**（Phase 2-3）と、dogfood で B の収束品質・D/E/F の緊張を観察すること。
> **本リデザインの一貫した stance: 未知数の大きい領域（presence・backing-kind 進化・連邦・relational role）は、構造の天井は高く保ちつつ実装は最小に留める**——simple を内包する rich な構造を選び、rich な実装は dogfood の声を聞いてから足す。
> この §12 は「決定の確信」ではなく「持っておくべき緊張」の記録であり、dogfood の観察で更新される。
