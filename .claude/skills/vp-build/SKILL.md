---
name: vp-build
description: "Use when editing VP's macOS Swift agent code (apple/VantagePointAgent). Triggers an xcodegen + xcodebuild rebuild of VantagePointAgent.app with log monitoring, so the user doesn't need to run it in a separate terminal. Examples: edited apple/VantagePointAgent/Sources/*.swift, changed apple/VantagePointAgent/project.yml, or changed the club-unison Swift client (../club-unison/clients/swift) the agent depends on."
---

# VP Build Workflow（VantagePointAgent / pure Swift）

VP の macOS surface は **`apple/VantagePointAgent`**（doc 25 Model D の Swift menu bar agent、LSUIElement）。
**pure Apple-native**（Rust を一切リンクしない・FFI 無し）で、club-unison の native Swift Unison client
（`../club-unison/clients/swift`）越しに Mac daemon と喋る。Swift コードを編集したら、Claude が
`xcodegen → xcodebuild` でリビルド・再起動し、ログをモニタして成功/失敗を判定・報告する。

> ⚠️ **旧アーキは退役済み**。`apple/VantagePoint`（Rust-tao app）/ `crates/vp-bridge`（FFI）/ `mr mac`
> task は**もう存在しない**。`vp-bridge` FFI も `ARCHS=arm64` リンクも `mr mac` も出てこない ── これらは
> doc 25 の Swift-first 転換で消えた。本 skill は新 `VantagePointAgent`（xcodebuild）専用。

## リビルドが必要な編集パス

| パターン | 理由 |
|---------|------|
| `apple/VantagePointAgent/Sources/**.swift` | agent UI / ロジック（AgentMenuView / AgentModel / CoreMIDIWatcher / DaemonClient / InstanceScanner / VPProtocol …） |
| `apple/VantagePointAgent/project.yml` | XcodeGen プロジェクト定義（target / Info.plist properties / 依存）→ **xcodegen 再生成が必須** |
| `../club-unison/clients/swift/**`（UnisonClient SDK） | agent が SPM local path で依存。SDK 変更は agent ビルドに波及 |

## リビルドしないケース

- `docs/**`, `README.md`, `CLAUDE.md`, `AGENTS.md` 等のドキュメント
- `crates/vantage-point/**` / `crates/vp-cli/**`（**Rust サーバ/CLI 側** → `cargo install --path crates/vp-cli --force`）
- `crates/vp-app/**`（旧 webview app。frontend は別 dev loop = `mr dev:web` esbuild watch）
- ユーザーが「build するな」と明示したとき
- 既に他の xcodebuild / cargo が走っている（直列化、`Resolve Package Graph` 競合回避）

## 標準手順

### 0. 前提（fresh checkout / sibling 依存）

- **`../club-unison/clients/swift` が存在すること**（project.yml の `UnisonClient` local path 依存）。
  無ければ club-unison を sibling に clone（`new-machine-setup` memory: creoui と同じ sibling 前提）。
- `.xcodeproj` と `Generated/Info.plist` は **gitignore された生成物**。fresh checkout や project.yml
  変更後は **必ず `xcodegen generate` を先に**走らせる（無いと `does not exist` で xcodebuild 失敗）。

### 1. リビルド実行（バックグラウンド）

```bash
# project.yml を変えた / .xcodeproj が無い / Sources にファイルを追加 → xcodegen から
( cd apple/VantagePointAgent && xcodegen generate ) 2>&1 | tee /tmp/vp_agent_build.log
# xcodebuild（Swift のみ変更ならここから直で可）
xcodebuild \
  -project apple/VantagePointAgent/VantagePointAgent.xcodeproj \
  -scheme VantagePointAgent \
  -configuration Debug \
  -derivedDataPath apple/VantagePointAgent/DerivedData \
  build 2>&1 | tee -a /tmp/vp_agent_build.log
```

- `run_in_background: true` で Bash tool 発火、`tee` で stdout/stderr 保存、`2>&1` 必須。
- ビルド成果物 = `apple/VantagePointAgent/DerivedData/Build/Products/Debug/VantagePointAgent.app`。
- **`mr agent` task があればそれを優先**（rig wrapper が kill→xcodegen→xcodebuild→launch を束ねる）。

### 2. Monitor で完了判定

```bash
until grep -qE "\*\* BUILD SUCCEEDED \*\*|\*\* BUILD FAILED \*\*|error: |The following build commands failed" /tmp/vp_agent_build.log; do sleep 3; done
grep -E "\*\* BUILD (SUCCEEDED|FAILED) \*\*|error: |warning: .*Sendable|ld: " /tmp/vp_agent_build.log | tail -15
```

Monitor tool の `command` にそのまま渡す。`persistent: false`、`timeout_ms: 600000`（10 分）で十分。

### 3. 結果判定

| ログパターン | 判定 | アクション |
|-------------|------|------------|
| `** BUILD SUCCEEDED **` | 成功 | agent を再起動（手順 4）→「再起動済、menu bar を確認してください」と報告 |
| `** BUILD FAILED **` | 失敗 | `error: ` 行（Swift type / concurrency / missing member）を抽出して原因報告 |
| `error: no such module 'UnisonClient'` | 依存欠落 | `../club-unison/clients/swift` の存在 + `xcodebuild -resolvePackageDependencies` を確認 |
| `scheme ... does not contain` / `.xcodeproj does not exist` | 生成漏れ | `xcodegen generate` を先に走らせていない → 手順 1 の xcodegen から |
| `CoreSimulator is out of date` | 無視 | macOS target には無関係 |

### 4. 再起動（LSUIElement = menu bar）

```bash
pkill -x VantagePointAgent 2>/dev/null; sleep 0.3
open apple/VantagePointAgent/DerivedData/Build/Products/Debug/VantagePointAgent.app
```

agent は **dock icon を持たない**（`LSUIElement: true`）。起動確認は menu bar アイコン or `pgrep -x VantagePointAgent`。

## 既知のハマりどころ

### xcodegen を飛ばす事故
`.xcodeproj` は gitignore。**project.yml を変えたのに xcodebuild だけ回す**と古い project graph でビルド
され、新しい Info.plist / 依存 / source が反映されない。project.yml 変更時は必ず `xcodegen generate`。

### SPM local path 依存（club-unison）
`UnisonClient` は remote URL ではなく **sibling repo の local path**（`../../../club-unison/clients/swift`）。
sibling が無い / branch がズレていると `no such module 'UnisonClient'`。VP と club-unison は併せて更新する。

### codesign（cp 禁止は Rust 側の話）
Swift agent は xcodebuild が自動署名（`CODE_SIGN_STYLE: Automatic`）。**`.app` を `cp` で別所へ運ぶと
署名が剥がれ macOS に kill される**ので、`open` で DerivedData の成果物を直接起動する。CLI（`vp`）側は
`mise run daemon:build`（dev root `~/.local/opt/vp-dev/bin` に cargo install。`~/.cargo/bin` には置かない — CLAUDE.md VP_PROFILE 節。cp は厳禁）。

### Swift 6 concurrency
`SWIFT_VERSION: "6.0"`（strict concurrency）。`Sendable` / actor-isolation の error が出やすい。
`error:` が concurrency 系なら、該当型に `@MainActor` or `Sendable` 整合を促す形で報告する。

## CLI / daemon 側のインストール（別系統）

Swift agent ではなく Rust（`crates/vantage-point/` / `crates/vp-cli/`）を触ったとき:

```bash
mise run daemon:build   # dev vp（~/.local/opt/vp-dev/bin/vp）更新。~/.cargo/bin には置かない（CLAUDE.md VP_PROFILE 節）
```

daemon への反映は 2 経路。普段使い（brew、:32000）に効かせるなら `VP_SWAP_RESTART_DAEMON=1 mise run app:swap`（lane の claude が全部落ちる。⚠️ VP の lane の中から打つと自分が死ぬ）。dev profile（:32100）で試すなら `mise run daemon`（build → stop → start、brew 側は触らない）。

## やらない判断（ユーザー確認を挟む）

- `project.yml` の target/署名設定を大きく変えたとき（動作が不確定）
- club-unison SDK 側の破壊的変更を伴うとき（VP/unison の両 repo 整合が要る）
- 同時並行で複数編集 → まとめて確認したいとき
- ユーザーが「しばらく build するな」「貯めてからやる」と指示したとき
