# doc 40 — 会話 id の SSOT 統合: session registry 一枚岩 + LaneInfo descriptor 完成

> 2026-07-18、mako + Fable 5 conductor session。発端 = #800 出荷後の Act I New dogfood で
> 「cc session id が Pane ヘッダに出ない」→ 解剖の過程で構造の歪みと実バグ 2 段を確定。
> mako 決定:「なるべく正しい session id を、常に出す」「強く美しい構造に考え直す」
> 「急がず一歩一歩、確かな基盤を」。doc 38（1 Lane = N session）/ doc 39（root agent）の続編。

## 0. TL;DR

会話 id（cc_session 等）の置き場を **session registry（`echoes_sessions/*.json`）に一枚岩化**する。
`SessionEntry` が `conversation` を持ち、engine 別 side store ×3（cc/cursor/codex_sessions）は退役。
書き手は **SP の 1 点に漏斗**（hook は「報告者」に降格）、表示は **id 発行時点で eager**、
resume pointer 保護（F1/F2 guard）は **SP の policy 1 箇所**に集約。`LaneInfo` は sessions を
搭載して「lane の完全な descriptor」になる（cwd は既に持っている — sessions が最後の外付け）。

## 1. 解剖 — 現状の 4 分散と実バグ（2026-07-18 dogfood、全て実測/コード確認済み）

「session #k の会話 id はどれか」という 1 つの情報が 4 箇所に分散している:

| # | 置き場 | 内容 | 問題 |
|---|--------|------|------|
| 1 | `echoes_sessions/*.json`（session registry） | `{key, stand}` — **会話 id を持たない**（session_registry.rs 旧 L19 の明示判断） | 構造の主が情報を持っていない |
| 2 | `cc_sessions` / `cursor_sessions` / `codex_sessions`（engine 別 store） | **文字列ラベル**（`conductor#2`）が鍵の 1 行 file | 書き手と読み手が別経路でラベルを導出 → 乖離可能 |
| 3 | `LaneInfo.cc_session_id` / `.engine_session_id` | serialize 境界で lazy read する projection | 供給 4 点（discovery.rs ×2 / routes/lanes.rs ×2）で「呼び忘れるな ⚠️」契約 — 2 回実際に刺さった（2026-07-16 chip 不点灯 / #683 同地形） |
| 4 | env `VP_LANE` | hook の書き込み鍵 | **二君に仕える**: wire address（lane 粒度）と store 鍵（session 粒度であるべき）の両方 |

### 1-1. 実バグ①: 書き手と読み手のラベル乖離（root≥2 で顕在化）

- 書き手 = hook（`vp wire hook-check`、UserPromptSubmit）: 鍵は `VP_LANE` = **素の lane label**
  （`conductor`、stand_spawner.rs L321）→ `cc_sessions/<proj>__conductor` に書く
- 読み手 = chip enrich（`refresh_engine_session_id`）: 鍵は `session_label(lane, root)` =
  root=2 なら **`conductor#2`** → `cc_sessions/<proj>__conductor#2` を読む — **誰も書いていない file**

実測（2026-07-18 03:18 の Act I New 後）: registry は `root:2` に更新、hook は発話時（03:23）に
plain 側へ書き込み、`conductor#2` file は不存在 → **chip は発話後も永久に空**。
doc 39 §3-4 の「root 切替後の床での発話は新 root の store に記録される」は**現実装では成立していない**。

### 1-2. 実バグ②: 床 resume の劣化（①の帰結）

床 spawn（stand_spawner.rs L350-353、P1 済）は root label の store から resume id を読む —
①により root≥2 では常に None → `--resume` でなく **`--continue` fallback（cwd 最新の session を
拾う運任せ）** に落ちる。SP 再起動後の会話継続性が root≥2 lane で壊れている。

### 1-3. 表示の発話ゲート（仕様欠陥）

chip の点灯は #795 の UserPromptSubmit hook（= resume pointer 記録）に相乗りしており、
**id が発行されても初回発話まで点かない**。Act I の生 TUI は stream `SessionInit` を持たないため
boot 時の捕捉経路が存在しない。mako 決定（2026-07-18）:「Act・engine 問わず、session id が
発行された瞬間に表示。発話を待つ意味がない」。

## 2. 決定 — 不変条件

1. **会話 id は session の属性** → session registry（`SessionEntry.conversation`）が唯一の SSOT。
   engine 別 side store は廃止（ラベル鍵という乖離可能面そのものを消す）
2. **書き手は SP の 1 点に漏斗**: hook / Act II host / create-chat 事前採番、すべて SP の
   registry 書き込み関数を通る。hook は「(project, lane, session_id, event) の報告者」に降格 —
   root がどの session かの解決は **SP だけ**が行う
3. **表示は発行時点で eager**（SessionStart / SessionInit）。**resume pointer の保護
   （F1/F2 guard）は SP の policy 1 箇所**に集約（§6）— 表示のためにガードを緩めない
4. **LaneInfo = lane の完全な descriptor**: cwd（既在）+ sessions（本 doc で搭載）。
   chip とタブの供給が 1 push に揃う土台
5. **wire は lane 粒度のまま**（doc 39 §3-2 不変）。`VP_LANE` は wire/identity 専用になり
   二君問題は消滅
6. **美しさの判定基準**: §1 の各バグが新構造では「起こせない」こと（置き場を変えるだけで
   バグクラスが残る案は不採用 — 2026-07-18 に daemon DB 案を検討し、この基準で棄却済み。
   per-session の真実が必要で、LaneInfo projection を truth に据えると同じ歪みが残るため）

## 3. データモデル

```jsonc
// echoes_sessions/<proj>__<lane>.json — 唯一の truth（既存 file に conversation が生えるだけ）
{
  "focused": 2, "root": 2, "next": 3,
  "sessions": [
    { "key": 1, "stand": "echoes", "conversation": "94427c81-…" },
    { "key": 2, "stand": "echoes", "conversation": "09b1f564-…" }  // ← chip も resume もここ
  ]
}
```

- `SessionEntry.conversation: Option<String>`（serde default = None、None は skip → file/wire
  後方互換。Draft = None は doc 38 §1.1 のまま）
- `LaneInfo.sessions: Option<SessionRegistry>`（serde default、skip_serializing_if None —
  旧 client と wire 互換）。既存の `cc_session_id` / `engine_session_id` は当面 **sessions からの
  導出**として維持（vp-app / channel D の既存読み手を壊さない。退役は PR-2）
- **legacy backfill（移行 bridge）**: `load_in` が entry.conversation == None の時だけ旧 store
  （stand で dispatch、session_label で読む）から補完する読み取り専用 merge。次の save で
  registry に固定される。全 dogfood lane の resume を殺さずに直切替するための橋で、PR-2 で撤去

## 4. 書き込み経路 — 漏斗（funnel）

| 発生源 | 現状 | 本 doc 後 |
|--------|------|-----------|
| Act I 床の claude（hook） | `cc_session::record(VP_LANE)` を hook が直書き ← バグ① | hook は `/api/lane/session-changed` に **`session_id` + `event` + `session` を載せて報告のみ**。World が SP へ forward、SP が宛先 session を解決して §6 policy で registry に書く |
| Act II claude host（SessionInit） | `cc_session::record(session label)` | SP 内から `set_conversation(key)` — host は自 label を持つので `parse_session_label` で key 逆引き |
| cursor create-chat 事前採番 / codex record-from-init | 各 store に record | **PR-1 では据え置き**（mako 2026-07-18: cursor はオミット予定（doc 39 §7）、codex は TurnHost ごと RpcHost 移行で書き直すため、退役予定の host に再配管しない）。読みは backfill bridge が registry に繋ぐので一貫。新 RpcHost / AcpHost は registry 直結（`set_conversation`）で書く |

- **検証は write 側で dispatch**: entry.stand の engine validator（cc = 英数ハイフン / cursor =
  `is_valid_chat_id` / codex = `is_valid_thread_id`）を通らない id は書かない（`--resume '<id>'`
  への injection 防壁を store 時代から引き継ぐ。spawn 側の再検証も残す = 深層防御）
- **SP 内の直列化**: registry の load-modify-save 変異（create / focus / remove /
  set_conversation / record_conversation）は process 内 mutex で直列化する。
  store 時代は「1 file 1 値」で last-write-wins が無害だったが、registry は複数 field の
  JSON なので並行 load-modify-save が update を失い得る（既存の create/focus/remove にも
  潜在していた穴を同時に塞ぐ）
- **daemon forward の payload 拡張**: `lane/session-changed` に `session_id` / `event` /
  `session` を透過（無い場合は従来どおり re-enrich + push のみ = 新旧 binary 混在に安全）

### 4-1. 報告の session 粒度化（2026-07-22、doc 46 P5 の続き）

> PR-1 時点の hook は「(project, lane, session_id, 契機)」を報告し、SP は**常に root** に書いていた。
> `VP_LANE` が二君に仕える問題（§1 の表 #4）は消えたが、**報告者が誰かを名乗れない**という
> 非対称が残っていた。doc 46 P5（`pty_slots` の `(lane, session)` re-key）で 1 lane に複数の
> console slot が同居できるようになり、この非対称が producer の blocker になった —
> 2 本目の claude の SessionStart が root の会話 id を上書きし、root の `--resume` が
> 同居人の会話に化ける。

**hook が「自分がどの session か」を名乗り、SP は報告された session に書く。**

| 層 | 変更 |
|---|---|
| spawn（`stand_spawner`） | identity env に **`VP_SESSION_KEY`** = その slot が化身する session の key を追加（現状 slot を立てる経路は全て root なので値は root。非 root slot の producer が入っても変わるのは注入値だけ） |
| hook（`vp wire hook-check`） | `VP_SESSION_KEY` を読み、報告 payload に `session` を載せる。**読めない時は field ごと載せない**（`session_key_from_env` は env 不在 / 非数値 / 0 を `None` にする — 「不明」を 1 に丸めない） |
| World（daemon forward） | `session` を透過するだけ（欠けた値を補完しない = routing のみの原則） |
| SP（`record_conversation`） | 書き先が **root 固定 → 報告 session**。§6 の policy 表（F1/F2 guard / engine 判定 / 形式検証）は**そのまま**、対象 entry が変わるだけ |

**「不明」と「root」を型で分ける**（`ReportTarget::Unspecified` / `Session(key)`）:

| 報告 | 扱い | 根拠 |
|---|---|---|
| `session` 無し（`Unspecified`） | **root に記録**（従来どおり） | `VP_SESSION_KEY` 以前に spawn された slot / VP 外で起動された claude。session 粒度化前は全報告が root 宛だったので、これが後方互換の正解 |
| `session` = 実在する key | その session に記録 | 本節の目的 |
| `session` = **実在しない** key | **書かない**（`UnknownSession`） | root に落とすと「名乗れるのに registry とズレている報告者」が root の会話を壊す = 消したかった事故が fallback 経由で蘇る。報告者は毎ターン再報告するので、registry が追い付けば次の報告で着地する |

> ⚠️ `Option<SessionKey>` を早い段階で `unwrap_or(root)` しないこと。丸めた瞬間に上表の
> 1 行目と 3 行目が見分けられなくなる（着地先はどちらも root なので**テストでも気付けない**）。
> root への fallback は registry 側 policy の 1 箇所だけが行う。

- **wire mailbox は lane 粒度のまま**（§2 決定 5 不変）。`agent@<lane>` を名乗るのは root で、
  本節が変えたのは会話 id の記録先だけ。同居人は「読み書きできる console」であって
  「mailbox を持つ住人」ではない（doc 46 §3 の producer が入る時に再検討する）
- channel D の headless claude（`delivery_actor::spawn_bg_dispatch`）は `VP_SESSION_KEY` を
  持たない = `Unspecified` = root 宛。root の会話を `--resume` する経路なので**それが正しい**

## 5. 読み経路 — 全 reader の一斉切替

| # | reader | 現状の読み | 本 doc 後 |
|---|--------|-----------|-----------|
| 1 | Act I chip（`refresh_engine_session_id`） | root label で engine store ×3 dispatch | registry load 1 回 → root entry の `conversation`（dispatch 消滅）+ `LaneInfo.sessions` に registry 全体を搭載 |
| 2 | Act II タブ（`list_chat_sessions`） | entry ごとに engine store dispatch | entry の `conversation` を直読み |
| 3 | chat spawn（`ensure_chat_engine` 3 arm） | label で store 読み | resolve 済み entry の `conversation`（claude は `transcript_exists` filter 維持 — doc 33 C2） |
| 4 | 床 spawn（`build_stand_command` 3 arm） | root label で store 読み | root entry の `conversation`（同 filter） |
| 5 | channel D（delivery_actor `--resume`） | `LaneInfo.cc_session_id` — ただし **uplink push 経路（agent_card / LaneDiff）は一度も populate しておらず実質常に None**（= channel D の resume は de-facto OFF だった。f286fc8 の「refresh は cc_session_id を触らない」設計と snapshot 限定 enrich の組み合わせ） | `cc_session_id` の導出元が sessions[root]（stand==echoes、全 lane）になり **resume が実質初めて ON になる**。headless `claude -p` は `\|\| claude` fallback を持たないため、配信時に `transcript_exists` pre-flight を追加（他 resume 経路と同じ防壁 — moody 指摘、PR-1 で対処済み）。conductor 限定だった旧 populate は撤廃 = performer too（R3-b の「resume policy 化の際に広げる」の実現） |
| 6 | transcript replay（`transcript_path`） | id を受けて `~/.claude/projects` 走査 | 変更なし（id の出所が registry になるだけ） |

供給 4 点の「呼び忘れるな ⚠️」契約は、enrich が「registry 1 read + clone」に縮むことで
危険度が落ち、PR-2（in-memory authoritative 化）で契約ごと消える。

## 6. eager 表示と resume pointer 保護の両立（F1/F2 guard の移設）

背景: 床は `claude --resume X || claude` で spawn する。resume が transient に失敗すると
`||` fallback が**発話ゼロの幻 session Y** を立てる。旧実装が SessionStart で pointer を
記録していた時代、この Y が健在な X への復帰路を上書き破壊した（F1 clobber / F2 幻ポインタ、
解剖 memory `cc-session-pointer-self-destruction`）。「UserPromptSubmit のみ記録」（#795）は
この 1 ケースを潰す鈍器で、安全な 2 ケース（New root の fresh 発番 / resume 成功の no-op）まで
巻き添えにして表示を遅らせていた。本 doc は鈍器を **SP の精密な policy** に置き換える:

`record_conversation(project, lane, report)` — SP 1 箇所のみ
（`report` = 宛先 session + 会話 id + 契機。§4-1 で root 固定から報告 session になった。
下表の「root entry」は「**宛先 session の entry**」と読む — 判定内容は変わっていない）:

| 条件 | SessionStart（eager） | UserPromptSubmit（authoritative） |
|------|----------------------|----------------------------------|
| 宛先 entry の stand が echoes でない | 無視（claude hook の id を他 engine の session に混ぜない） | 同左 |
| conversation == Some(同 id) | no-op | no-op |
| conversation == None（New root / fresh） | **記録**（chip が boot で点く） | 記録 |
| conversation == Some(旧 id) かつ旧 id の transcript **実在** | **据え置き**（`||` fallback の幻 = F1/F2 guard。chip は守った旧 id を映し、次の発話で self-heal） | **記録**（user が実際に話した = commit） |
| conversation == Some(旧 id) かつ transcript **消滅** | 記録（どうせ蘇らない。幻 pointer 保持の現状より改善） | 記録 |

- hook 側は `WIRE_HOOKS` に SessionStart を追加し、両 event で同じ報告を送る（policy 判定は
  一切持たない）。通知前に registry を read-only load して no-op なら送信 skip（毎ターンの
  無駄打ち回避 — 旧実装の `changed` 判定と同じ）
- Act II（headless）には `||` fallback が存在しない（doc 33 C2 で pre-flight 済み）ため、
  host の `set_conversation` は無条件で authoritative

## 7. 段階計画

| PR | 内容 | 状態 |
|----|------|------|
| **PR-1（本体）** | §3 データモデル + §4 漏斗（claude のみ — cursor/codex は bridge 据え置き、§4 表参照）+ §5 reader 一斉切替 + §6 policy + backfill + registry save の atomic rename 化 + 変異の process 内 mutex 直列化。バグ①② root-cause fix + eager chip がこの 1 本で立つ | 実装中（2026-07-18） |
| **PR-2（純化）** | legacy store の record/last/clear 退役（validator / CLI path / transcript helper は各 module に残置）+ backfill 撤去 + lane GC が registry を clear + reader 統一（`echoes_demand_start` の replay 源を registry へ） | **着地済み（2026-07-19）**。前提の「soak」は構造欠陥だった（load は save しないため休眠 lane は永遠に backfill 依存のまま）— 代わりに **one-shot migration**（backfill 同一意味論の使い捨て script = `.vp-scratch/migrate-cc-sessions-doc40-pr2.ts`、充填 49 / backfill 依存ゼロ化）を実施してから撤去。⚠️ **他マシンへこの binary を deploy する前に同 script の実行が必要** |
| PR-2 の defer 分 | ①`LanePool` in-memory authoritative 化 — audit の結果 **SP 外の disk reader が実在**（`vp wire hook-check` / `vp lane` statusline が別 process から registry を直読み）し、disk を非 truth に降格できない ②vp-app タブの `LaneInfo.sessions` 消費 + `list_chat_sessions` RPC 統合 — `ChatSessionInfo.live`（in-memory engine 生死）が registry snapshot に無く、session mutation が `Diff::Update` を emit しない | ①reader の再設計（RPC 化 or 契約変更）②live 供給 + mutation Diff emission の設計、が各前提。別 PR |
| **PR-3（env 剪定）** | §8 の VP_SESSION / VP_CWD 退役 | **前提クリア（2026-07-19）**: user statusline（`~/.claude/statusline/`）に VP_ 参照ゼロを grep 確認 |

## 8. 付録 — VP_* env 棚卸し（2026-07-18 audit）

lane 子プロセスへ注入される 6 種（stand_spawner L316-345）:

| env | 判定 | 根拠 |
|-----|------|------|
| `VP_PROJECT` / `VP_LANE` | **残す** | hook 報告と wire address 導出の identity channel（本 doc 後、VP_LANE は store 鍵の役を失い wire 専用に単純化） |
| `VP_SESSION_KEY` | **新設（2026-07-22、§4-1）** | 報告者が「自分がどの session か」を名乗る identity。値は slot が化身する session の key（`1` / `2` …） |
| `VP_PROFILE` | **残す** | dev/brew namespace 分離（#643） |
| `MISE_TRUSTED_CONFIG_PATHS` | **残す** | mise trust footgun 抑止（PR2 実機検証、env-only で依存境界維持） |
| `VP_SESSION` | **退役済み（2026-07-19 PR-3）— 復活させない** | 退役理由は「読み手ゼロ」（repo 内 + user statusline `~/.claude/statusline/` を grep 確認）。⚠️ **同名で復活させなかったのは意味が別だから** — 旧 `VP_SESSION` は **lane の論理 identity**（`LaneAddress` Display 形 `vp/root`、さらに遡ると tmux session 名）で、§4-1 が要るのは **session の採番 key**（`1` / `2`）。同名再利用は「repo 外に残っている旧読み手（他マシンの dotfile / tmux 時代の script）が `vp/root` を期待して `2` を受け取る」型の無音事故を招く。形が全く違う（path 形 vs 整数）ので壊れ方も静か。名前を分けて `VP_SESSION_KEY` にした |
| `VP_CWD` | **退役済み（2026-07-19 PR-3）** | 同上（stand_spawner / delivery_actor の注入を撤去） |

repo 全体では VP_* が 31 種。残りは各 component の config knob / dev override
（VP_WORLD_URL / VP_OIDC_* / VP_SHELL / VP_TERM_TRACE 等）で本 doc の scope 外。

## 9. 既知の考慮点

- **durability 窓**: hook が file 直書きをやめるため、「World 不達のまま SP が死ぬ」と当該
  turn の会話 id 更新が失われる。hook は毎 turn 再報告するので次の発話で self-heal し、
  World down 中は channel D 等の読み手も居ない。許容（旧 file 直書きの「daemon 不在でも
  書ける」性質より、単一書き手の整合を取る — mako「確かな基盤」方針）
- **fresh reset × backfill の蘇生**: registry clear 後に legacy store が残っていると backfill が
  閉じた会話を蘇らせる。既存 reset は store も clear している（lanes_state / commands.rs）ため
  現行のままで整合。PR-2 の backfill 撤去まで **store clear を外さないこと**
- **lane GC の registry 漏れ（既存バグ）**: `clear_lane_state_files_in`（commands.rs）は
  cc_session / console_mode / engine_model / stand_store を消すが session registry と
  cursor/codex store を消さない — PR-2 で registry clear を追加
- **doc 39 との整合**: §3-4 の「root 切替後の発話は新 root の store に記録される」は
  本 doc §1-1 のとおり現実装で不成立 → PR-1 が実装で成立させる（doc 39 側に注記済み）。
  doc 39 §4-1（発行時点表示の不変条件）の実現機構も本 doc に一本化
