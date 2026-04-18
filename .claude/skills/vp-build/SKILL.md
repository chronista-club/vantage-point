---
name: vp-build
description: "Use when editing VP's Swift / vp-bridge code. Triggers a rebuild of VantagePoint.app via `mr mac` with log monitoring. Claude handles the rebuild and error reporting so the user doesn't need to run it in a separate terminal. Examples: edited apple/VantagePoint/Sources/*.swift, modified crates/vp-bridge/**, changed project.yml, changed .mise.toml (build task)."
---

# VP Build Workflow

VP の Swift / vp-bridge コード変更時、Claude が自動で `mr mac` を走らせて VantagePoint.app をリビルド・再起動する。ログはモニタして成功/失敗を判定し、エラー時は該当箇所を抽出して報告する。

## リビルドが必要な編集パス

| パターン | 理由 |
|---------|------|
| `apple/VantagePoint/Sources/**.swift` | Swift UI / ロジック変更 |
| `apple/VantagePoint/Resources/**` | Info.plist / Assets / Entitlements |
| `apple/VantagePoint/project.yml` | XcodeGen プロジェクト定義 |
| `crates/vp-bridge/src/**` | FFI bridge / PTY / ターミナル描画 |
| `crates/vp-bridge/Cargo.toml` | vp-bridge 依存変更 |
| `.mise.toml`（mac タスク関連） | ビルド task 自体の変更 |

## リビルドしないケース

- `docs/**`, `README.md`, `CLAUDE.md` 等のドキュメント
- `crates/vantage-point/**`（VP サーバ側、`cargo install --path crates/vp-cli` 対応）
- `apple/**` 以外の Rust コード
- ユーザーが「build するな」と明示したとき
- 既に他の cargo / xcodebuild が走っている（直列化）

## 標準手順

### 1. リビルド実行（バックグラウンド）

```bash
mr mac 2>&1 | tee /tmp/vp_local_build.log
```

- `run_in_background: true` で Bash tool 発火
- `tee` で stdout/stderr を両方ログ保存
- `2>&1` を忘れない（xcodebuild はエラーを stderr に出す）

### 2. Monitor で完了判定

```bash
until grep -qE "🚀 Launching|BUILD FAILED|ERROR task|error\[|error: |ld: " /tmp/vp_local_build.log; do sleep 3; done
# 完了条件 hit → イベント発火
grep -E "🚀 Launching|BUILD FAILED|ERROR task|error|ld: " /tmp/vp_local_build.log | tail -10
```

Monitor tool の `command` にそのまま渡す。`persistent: false`、`timeout_ms: 600000`（10分）で十分。

### 3. 結果判定

| ログパターン | 判定 | アクション |
|-------------|------|------------|
| `🚀 Launching .../VantagePoint.app` | 成功 | ユーザーに「再起動済、試してください」と報告 |
| `Finished \`release\` profile` のみ（xcodebuild 未完了） | vp-bridge のみ | まだ待つ |
| `BUILD FAILED` | 失敗 | `ld: symbol` / `error[E` 等を抽出して原因報告 |
| `CoreSimulator is out of date` | 無視 | iOS Simulator 絡み、macOS ビルドには無関係 |
| 何も hit しない（timeout） | ハング疑い | ログ末尾を読んで状態確認 |

## 既知のハマりどころ

### target/ ロック競合

他の `cargo install` / `cargo build` / `cargo test` が走っていると vp-bridge のビルド段階で `Blocking waiting for file lock on artifact directory` で待機する。**同時実行はしない** — 先行ビルドを Monitor で完了待ちしてから発火。

### ARCHS=arm64 必須

`.mise.toml` の `mac` タスクで `ARCHS=arm64 ONLY_ACTIVE_ARCH=YES` を xcodebuild に渡している。これが無いと universal (arm64+x86_64) を試み、arm64-only の `libvp_bridge.a` とリンク失敗する。

### scheme の存在

`apple/VantagePoint/project.yml` の VantagePoint target に `scheme: {}` セクションが必要。これが無いと XcodeGen が shared scheme を生成せず、`xcodebuild -scheme VantagePoint` が `does not contain a scheme` で失敗する。

### codesign

**`cp` でバイナリを配置してはいけない** — codesign が剥がれて macOS に kill される。CLI 側は必ず `cargo install --path crates/vp-cli --force`。Native アプリは xcodebuild が自動署名。

## CLI 側のインストール（別系統）

Swift / vp-bridge ではなく `crates/vantage-point/` や `crates/vp-cli/` を触ったとき:

```bash
cargo install --path crates/vp-cli --force
```

`vp` コマンド（`~/.cargo/bin/vp`）が更新される。Process の再起動は `vp restart` または `vp stop` → `vp start`。

## モード別タスク

| タスク | 用途 |
|--------|------|
| `mr mac` | 開発 loop（ビルド + 既存 kill + 再起動） |
| `mr mac:build` | ビルドのみ（起動しない） |
| `mr mac:release` | 署名 + Notarize + DMG 化（リリース用） |

## やらない判断

以下のシグナルが出たら Claude は自発的にはリビルドせず、ユーザー確認を挟む:

- `.mise.toml` の `mac` タスク自体を改変したとき（動作が不確定）
- Cargo.lock の大規模変更（依存更新）
- 同時並行で複数編集を行って、まとめて確認したいとき
- ユーザーが明示的に「しばらく build するな」「貯めてからやる」と指示したとき
