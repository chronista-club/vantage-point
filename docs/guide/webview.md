# webview（vp-app GUI の JS 層）開発ガイド

vp-app の GUI は `crates/vp-app/webview/` の SolidJS + xterm.js を esbuild で bundle し、
`crates/vp-app/assets/*.bundle.js` として **cargo build 時に binary へ embed**（`include_str!`）する。

## bundle は生成物（commit しない — 2026-07-19 転換）

`assets/editor-host.bundle.js` / `assets/sidebar.bundle.js` は **gitignore の生成物**。

| 経路 | bundle 生成 |
|---|---|
| `mise run app:swap` / `mise run release:mac` | 内部で `mise run app:bundle` を自動実行 |
| CI（Clippy+Test / Check Windows） | bun setup + `bun install --frozen-lockfile && bun run build` step |
| 手元で cargo を直接叩く | 先に `mise run app:bundle`（不在なら vp-app の build.rs が手順を案内して fail） |

bundle の再 embed は `crates/vp-app/build.rs` の `rerun-if-changed` が保証する
（旧「`touch main_area.rs` の儀式」は不要）。

### 旧運用と転換理由

旧: bundle を commit し、webview を触った PR が手動 `bun run build` で積む。
入力が sibling repo への `file:` 依存だったため「commit で内容を pin する」ことが再現性の担保だった。
代償として **積み忘れ = stale bundle が ship される footgun**（v0.52.1 リリース時に実発生）。

新: 依存を npm semver pin に移行（下記）したことで、bundle は bun.lock から環境非依存に再現できる。
pin の役割は git blob から npm version + lockfile へ移った。

## 依存は npm（file: sibling 依存は撤去済み）

| パッケージ | 供給元 repo | publish |
|---|---|---|
| `@chronista-club/creo-ui-editor-host` | chronista-club/creo-ui | tag `editor-host-vX.Y.Z` push（or workflow_dispatch） |
| `@chronista-club/creo-ui-icons-web` | chronista-club/creo-ui | tag `icons-web-vX.Y.Z` push（or workflow_dispatch） |
| `@chronista-club/unison-client` | chronista-club/club-unison | club-unison の publish フロー |

**供給側を更新したら**: 供給 repo で version bump + publish → VP で
`bun update @chronista-club/<pkg>`（lock 更新を commit）。
⚠️ 供給 repo の HEAD ≠ npm 最新 になりがち（editor-host で実発生: npm 0.5.3 のまま
#74/#76 が未 publish だった）。VP へ取り込む前に `npm view <pkg> version` と bump commit を突き合わせる。

## dev loop: HMR（cargo build なしで bundle を差し替える — doc 48 Phase 1）

tsx/ts 変更のたびに `bun run build` + cargo 再ビルド + app:swap を回すのは重い。
`VP_WEBVIEW_DEV` を設定して vp-app を起動すると、`*.bundle.js` を **disk から fresh read**
するようになり、bundle 差し替えが reload だけで反映される:

```bash
# 1. esbuild watch を常駐（保存ごと ~0.5s で bundle 再生成）
cd ~/repos/vantage-point/crates/vp-app/webview && bun run dev

# 2. vp-app を assets dir 指定で起動（brew/dev どちらの binary でも効く）
VP_WEBVIEW_DEV=~/repos/vantage-point/crates/vp-app/assets vp app start

# 3. 編集 → 保存 → View メニュー「Reload WebView」(Cmd+R) で反映。cargo build 不要
```

- Reload WebView は **View → Developer Mode ON の時だけ enabled**（Cmd+R も同 gate。
  一般 user の Cmd+R を奪わない）。
- 未設定 / read 失敗時は焼き込み bundle に fallback（= prod 挙動、壊れない）。
- creo-ui 側を触る日は下の `bun link` と組み合わせる — 供給側 `bun run build` →
  watch が拾って bundle 再生成 → Cmd+R、で **cross-repo でも cargo 無しの秒ループ**になる。
- 実装: `web_assets.rs`（disk-read）+ `main_area.rs`（bundle の外部 `<script src>` 化）。
  #494 の復活（#815 で撤去、外部 script 化で発火するようになった）。

## dev loop: creo-ui / club-unison を同時開発する日（bun link）

npm 化で失った「隣の repo を直して即反映」は `bun link` で一時的に復元する:

```bash
# 1. 供給側を link 登録（例: editor-host）
cd ~/repos/creoui/packages/editor-host && bun link

# 2. VP 側で link を張る（node_modules の該当だけ local 参照になる）
cd ~/repos/vantage-point/crates/vp-app/webview
bun link @chronista-club/creo-ui-editor-host

# 3. 以降 bun run build は local の creoui を bundle する（editor-host は dist を
#    include するため、供給側の変更後は供給側でも bun run build が要る）

# 4. 戻す（npm 版に復帰。lock は汚れていないので install し直すだけ）
bun install --frozen-lockfile
```

⚠️ link したまま `mise run app:bundle` / release を回さない — `--frozen-lockfile` は
link を検知できない。出荷系は必ず npm 版で（link 解除 → install → build）。

## 操作結果と生成物の検証

`bun run test` と `bun run typecheck` は CI の必須 check job でも実行する。
Rust テストは KDL / ts-rs の生成物を書き出すため、CI はテスト後の tracked diff と
未追跡ファイルを検知して失敗する。schema と生成物は同じ変更に含める。

sidebar の lane 作成は repo path と名前が一致する結果を待ち、成功時だけフォームを閉じる。
失敗時は入力を保つ。一般エラーは sidebar に表示し、ユーザーが閉じるまで残す。
chat の受付結果は lane / session / request ID に照合し、本文・画像は失敗表示から入力欄へ戻せる。
通信失敗で受付結果が不明な場合は、応答を確認してから再送する。

旧生成ファイル `ActiveStand.ts` / `ProjectPaneState.ts` は各々 `ActiveComponent.ts` /
`RepoPaneState.ts` に移行済みの重複のため撤去。旧 clone folder picker IPC は呼び出す UI と
結果の消費者がなく、schema・dispatch・native picker をまとめて撤去する。
公開 Rust adapter と lane component container の削除はこの整理に含めない。repo 内の参照不在だけでは
外部 API 利用や MIDI runtime の不要性を証明できないため、別途境界を確認して扱う。
