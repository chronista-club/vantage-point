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
