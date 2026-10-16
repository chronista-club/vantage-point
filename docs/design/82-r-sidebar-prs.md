# 82. R sidebar を edge rail の伸びた形にし、lane の repo の open PR を出す

> Status: Implemented in `wip/r-sidebar-prs` — 実機確認（app:swap + daemon 再起動）は mako 待ち
> Memory: mem_1CfuyxKX6uDCW5VzuJg4DZ
> Related: [56](56-edge-rail.md)（動詞の級 = 住所 / rail）/ [55](55-board-projection.md)（reflow 規律）/
> [80](80-lane-url-api.md)（daemon の読み口を足す流儀）/ [18](18-shortcut-convention.md)（`Cmd+]`）

## mako の原文（2026-10-11）

- 「ここの下部の領域にそのLaneのレポのPRのリストを表示したい」（右端の edge rail を赤枠で指して）
- 「Rのrailとの関わりをどうしようかも含めて。Rsidebarを新しく作るのか、Rrailのどちらに置くのか。ちなみに伸びた状態（25%）で使う想定」
- 「サイドバーあるんだね。ここを使おうか」= GO
- 「ログは一応残しておきたいな。ここは縦のaccordionで並べるかな」
- 「下は下から、中央は中から、上は上からで、配置シテいって」
- 「３箇所起点があるという規約」
- 「R sidebarの幅、ドラッグして変更もできるように（場所も自動で永続化）」

## 決定

1. **新しい sidebar は作らない**。既存の R sidebar（`Cmd+]`）を **edge rail の伸びた形**に作り直す。
   rail と R sidebar は「同じ住人の見え方切替」で、左 sidebar のフル ⇄ スリムと対称
2. 幅は **shell の 25%（下限 320px）が既定**。drag で変えられ、変えた値は **% のまま**自動で永続化する
3. 中身は**縦の accordion**: 上 = Lane / 中央 = Pull Requests / 下 = Debug Log。複数同時に開ける。
   開閉は保持し、既定は Lane 開 / PR 開 / Debug Log 閉
4. 並べ方は**3 起点の規約**（下記 §規約）。rail（畳んだ形）も同じ規約で並ぶ
5. PR 節 = 表示中 lane の repo の open PR。この lane の PR は先頭固定
6. rail の下の起点に **PR ボタン**（open 件数 badge）。押す = R sidebar を開いて PR 節へ
7. Debug Log は app 級（doc 56 の原理では R 側の住人ではない）が、**今回は下の節として残す**。
   設定 main area への移設は別起票（この PR ではやらない）

## 規約: 縦の帯は 3 箇所に起点を持つ

縦の帯（edge rail / R sidebar）の要素は**上詰めで積まない**。起点は **上・中央・下の 3 箇所**で、
各要素はどれかの起点に属する。

| 起点 | 伸びる向き | rail（畳んだ形） | R sidebar（伸びた形） |
|---|---|---|---|
| **上** | 上端から下へ | + New | Lane 節（リンク / + New / しまった pane / board） |
| **中央** | 残りの中で中央 | しまった pane | Pull Requests 節 |
| **下** | 下端から上へ | board 取っ手 / PR ボタン | Debug Log 節 |

- 閉じた節は見出し 1 行だけがその起点に残る（例: PR だけ開いていれば、上端に Lane の見出し、
  中央に PR 一覧、下端に Debug Log の見出し）
- 実装は 3 slot を持つ container（`main_area.rs` の `.tri-anchor` > `.anchor-top` /
  `.anchor-center` / `.anchor-bottom`）。上下は同じ伸び率で余白を分け合い、中央だけが縮む
  （溢れたら中央の中で scroll）。上下の中身が同じ高さなら中央は正確に中央、片方が大きく開いても
  **上端・下端の anchor は崩れない**（押されるのは中央）
- 新しい住人を足す時は「どの起点に属するか」を決めてから置く（doc 56 の「動詞の級 = 住所」が
  rail か否かを決め、この規約が帯の中の位置を決める）

## 住人の付け替え（rail ⇄ Lane 節）

R sidebar を開いている間、rail の住人（`#edge-rail-new-host` / `#edge-rail-stow-host` /
`#board-handle`）は Lane 節へ **DOM ごと付け替わり**、hover で滑り出していた `data-label` を
常時の文字にした行として並ぶ。閉じると rail の元の位置（親 + 次の兄弟）へ戻る。

- 住人を 2 つ描かないので、menu / badge / board の開閉などの状態は 1 つのまま
  （board-view.ts は id 依存だけなので配線は変わらない — doc 56 §3 と同じ配当）
- 旧 `body.rsb-open #edge-rail{display:none}` の排他は残る。帯は消えるが、住人は R 側に居る
- PR ボタンは rail 専用（R 側では PR 節そのものが伸びた形）なので付け替えない

## PR 節

- 行は 2 段: 1 段目 `#番号 タイトル`、2 段目 draft / CI / review / head branch
- **この lane の PR を先頭固定**し強調する。一致は branch の **slug** で見る
  （`wip/<slug>` と `review/<slug>` は同じ仕事 — lane が `wip/x` のまま PR の head が
  `review/x` という段違いが普通に起きる。段は branch-step の `spike` / `exp` / `wip` / `review`）
- click = ブラウザで開く（既存 `open-url` IPC、http(s) 限定は Rust 側）/ 右 click = URL をコピー
- 空なら「open PR なし」/ 取得失敗は理由を 1 行で出す / **gh 不在なら節ごと出さない**
  （rail の PR ボタンも出さない）

## Lane 節のリンク（design 80 追加裁定）

上起点の Lane 節に、表示中 lane の **検出 URL → この lane の PR → 登録済み URL** を置く。
左 sidebar の各 lane 行には URL 欄を置かない。lane を切り替えると表示と取得対象も追従する。

- 検出は既存 console/chat stream を使用し、保存は「登録」からの明示操作だけ
- URL の load / 自動 probe は R sidebar と Lane 節が両方開いている間だけ。失敗は節内の短い一行
- PR はこの節用に取り直さず、既存 PR 節の cache と poller を共用する
- PR 応答・検出結果の更新だけでは、登録フォームの draft や load を再初期化しない
- 描画は保存 IPC を持つ sidebar bundle、節の可視状態・検出 URL・PR は main bundle が所有。
  `lane-links-bridge.ts` が両者の接点で、評価順が逆でも最後の表示状態を引き継ぐ

保存・正規化・probe 制約と原文は [design 80](80-lane-url-api.md#lane-リンクへの追加裁定) を参照。

## 契約

### daemon-control `lane/prs`（read-only）

```
request  { path: string }                 // lane の cwd（worktree の絶対 path）
response {
  available: bool,                        // false = gh 不在（エラーではない）
  branch: string | null,                  // cwd の現在の branch（detached は null）
  prs: [{ number, title, url, draft, head,
          ci: "pass"|"fail"|"pending"|"none",
          review: "approved"|"changes_requested"|"review_required"|"none" }],
  error?: string                          // gh はあるが失敗した理由
}
```

- 裏は当面 `gh pr list --state open --limit 50 --json number,title,isDraft,headRefName,url,statusCheckRollup,reviewDecision`
  を lane の cwd で実行（`crates/vantage-point/src/lane/prs.rs`）
- **「許容 ≠ 依存」**: gh 不在 = `available:false`、失敗 = `error`。どちらも RPC を Err にせず
  daemon も落とさない。launchd 起動の daemon でも brew の gh が見えるよう PATH は
  `spawn_env::augmented_spawn_path`、未認証時の対話 prompt を避けるため `GH_PROMPT_DISABLED=1`、
  上限 20 秒
- **応答は gh の形を漏らさない** — CI（CheckRun / StatusContext の混在）と review は daemon が
  語彙 4 つずつに畳む。裏を GitHub API に差し替えても GUI は変わらない
- CI の畳み込みは **fail > pending > pass**（1 つでも落ちていれば赤）。SKIPPED / NEUTRAL は通過扱い

### GUI ⇄ vp-app

- webview → Rust: `{"t":"lane:prs_fetch","lane":"<repo>/<lane>"}`（main IPC allowlist に載せる —
  漏れると sidebar IPC へ流れて silent drop）。vp-app が lane address → cwd を解決して `lane/prs` を叩く
- Rust → webview: push `lane:prs { lane, payload }`（payload は上の response そのまま）。daemon に
  届かなかった時も `{available:true, error}` を返す（黙ると「読み込み中」で止まる）

### 取得のタイミング（消費者主導）

- **R が開いていて Lane 節または PR 節が開いている間だけ**、即 1 回 + 60 秒ごと。両方閉じたら止める
  （Debug Log の watch / unwatch と同じ型。`pr-list.ts` の `createPrPoller`）
- lane が変われば新 lane で即取り直す。同じ状態の再通知では撃たない
- rail の badge は**最後に取った値**（lane ごとに覚える。rail 自身は取りに行かない）

### Debug Log の契約（不変）

- watch / unwatch の契約は変えない。見えている時（R 開 × Log 節開）だけ `debuglog:watch`、
  それ以外は `debuglog:unwatch`。source 切替は watch の送り直し（最後の watch が勝つ）

### 幅の永続化

- session file の `shell_layout.right_sidebar_pct`（15〜60%、Rust 側で clamp）が R の幅の正
- `right_sidebar_width`（px）は旧版向けの互換の写しで、webview はもう R の幅に使わない —
  旧版は R を開閉するたびに既定 420px を保存していたので、px を読むとほぼ全員が 25% 既定を
  見ないまま 420 に留まる
- 下限 320px は CSS の `min-width` が持つ（% だけでは狭い窓で潰れる）
- accordion の開閉は webview の localStorage（`vp.rsb.sections`）。壊れた値は既定に倒す

## やらないこと

- lane panes（`lane-panes.ts`）/ 中央のレイアウトには触らない（reflow 規律、doc 55 / 56）。
  R の開閉で `#host` が縮むのは従来どおり user 操作起点の reflow のみ
- `gh` を product の必須依存にしない
- Debug Log の移設・撤去（別起票）

## 検証

- daemon: `lane::prs` の純関数（CI 畳み込み / review / parse）と「gh 不在 = `available:false`」を test
- webview: `pr-list.test.ts`（parse / slug 先頭固定 / 2 段目 / poll の開始・停止・lane 切替）、
  `right-sidebar.test.ts`（accordion 既定と壊れた保存値）、`shell-layout.test.ts`（% の drag 計算）
- 実機（mako）: `VP_SWAP_RESTART_DAEMON=1 mise run app:swap` で R の幅 25% / drag 永続 /
  3 起点の配置 / 住人の付け替え / PR の click・右 click / gh 不在時の非表示 を確認する
