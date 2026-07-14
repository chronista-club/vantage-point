---
description: "Vantage Point のリリースフロー（品質ゲート → nightly で bump → release PR → tag cut → notarized .dmg + Homebrew cask）"
allowed-tools: Bash, Read, Edit, Grep, Glob, AskUserQuestion
---

# Vantage Point リリースフロー

> **SSOT は `docs/guide/release.md`**。本 command はそれを実行手順に落としたもの。
> 齟齬があったら doc 側が正。

配布は **notarized `.dmg`（GitHub Releases）/ Homebrew cask / `cargo install`** の三本柱（macOS arm64 主軸）。
`.dmg` の build → 署名 → notarize → publish → cask 更新は **すべて `mise run release:mac`** に集約されている。
**tag-trigger の CI release job は存在しない** — 成果物の build / publish はローカル実行が正。

各ステップの結果を報告し、**エラーが出たら即座に停止**して状況を報告すること。

---

## ⚠️ 絶対に踏んではいけない罠

| 罠 | なぜ危険か |
|---|---|
| release PR に **`--delete-branch`** を付ける | **nightly（dev trunk）が消える**。CLAUDE.md が deletion 禁止と明記している branch。release PR は `gh pr merge --merge` のみ |
| release PR を **`--squash`** する | nightly の履歴が潰れる。main へは **merge commit** で運ぶ（過去も `Merge pull request #NNN from chronista-club/nightly`） |
| **main へ直 push** | 規約違反（main は PR 必須）。version bump は **nightly 上**でコミットし、release PR で main に運ぶ |
| **ディスク空きを見ない** | `target/` は dogfood ループで数十 GB に膨らむ。満杯だと **notarize や .dmg 作成の途中で ENOSPC** に落ちる（2026-07-14 実際に踏んだ） |
| テスト失敗を「flaky でしょ」で流す | PTY 系テストは高負荷で timeout する。**isolation で再実行して実証**してから進む。本物の regression を出荷しかねない |
| background 実行の exit code を信じる | `cmd \| tail` の exit code は **`tail` のもの**。**GitHub の実体（`gh release view` / cask の sha256）で検証**すること |

---

## Step 1: 事前チェック

1. **ブランチ**: `nightly` に居ること（`git checkout nightly && git pull --ff-only origin nightly`）
2. **作業ツリー**: `git status` がクリーン（未コミットの変更があれば報告して停止）
3. **ディスク空き**: `df -h /System/Volumes/Data` — **10 GB 以上**の空きを確認。
   足りなければ `~/.claude/skills/hdd-resque/scripts/hdd_resque.sh --dry-run` → ユーザー承認 → 本実行
4. **notarize 資格情報**（無いと Step 6 の終盤で落ちる。ここで fail-fast する）:
   ```bash
   xcrun notarytool history --keychain-profile vp-notary   # profile が生きているか
   security find-identity -v -p codesigning | rg "Developer ID Application"
   ```

## Step 2: 品質ゲート（macOS 専用アプリのため CI の代わり）

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings   # doc と同じ厳格ゲート（-W ではない）
cargo test --workspace
```

- いずれか失敗 → 停止して報告
- **テストが落ちたら原因を切り分ける**:
  - `No space left on device` → Step 1-3 のディスク解放へ戻る
  - PTY 系（`pty_slot` / `terminal_demand_start`）の timeout → **isolation で再実行**して負荷起因か実証する:
    ```bash
    cargo test -p vantage-point --lib -- --test-threads=1 <test_name>
    ```
    isolation でも落ちるなら**本物の regression**。リリース中止

## Step 3: バージョン bump（**nightly 上で**）

1. 現行 version を `Cargo.toml` の `[workspace.package] version` から取得して表示
2. ユーザーに patch / minor / major を確認（`feat` が入っていれば minor、breaking があれば major）
3. `Cargo.toml` の version を更新
4. **`Cargo.lock` を更新**。⚠️ `cargo check` は version 変更で全クレートを再ビルドして遅いので使わない:
   ```bash
   cargo update --workspace    # コンパイルせず lock の workspace member だけ更新
   ```
5. コミット + push（差分は `Cargo.toml` + `Cargo.lock` の 2 ファイルのみ）:
   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "release: vX.Y.Z"   # 本文に主要変更を列挙
   git push origin nightly
   ```

## Step 4: release PR（nightly → main）

```bash
gh pr create --base main --head nightly --title "release: vX.Y.Z"   # 本文にリリースノート

# ⚠️ --merge のみ。--squash / --delete-branch は絶対に付けない（nightly が消える）
gh pr merge <PR#> --merge
```

merge 後、`git ls-remote --heads origin nightly` で **nightly が生きていること**を確認する。

## Step 5: tag cut（main 上で）

```bash
git checkout main && git pull --ff-only origin main
git tag vX.Y.Z
git push origin vX.Y.Z
```

tag が release merge commit を指していることを確認（`git show --stat vX.Y.Z`）。

## Step 6: `.dmg` build → notarize → publish → cask

```bash
mise run release:mac
```

`mise run release:mac`（`.mise/tasks/release/mac`）が一気通貫で行う:

1. 前提チェック（Developer ID 証明書 / notarytool profile `vp-notary`）
2. `cargo build --release --target aarch64-apple-darwin -p vp-app -p vp-cli`
3. `.app` 組立（`vp-app` + 同梱 `vp` daemon + icon + Info.plist）
4. codesign（Developer ID + hardened runtime、inside-out）
5. `.dmg` 作成（`hdiutil`、`/Applications` symlink 付き）
6. **notarize + staple**（`xcrun notarytool submit --wait` → `stapler staple` → `spctl` 検証）— Apple 往復で数分〜十数分
7. GitHub Release publish（`gh release create`）
8. Homebrew cask 自動更新（末尾で `mise run release:cask` を best-effort 呼び出し）

**長時間かかるので background 実行を推奨**（完了通知を待つ）。

### 部分実行 / ドライ実行

| 環境変数 | 効果 |
|---|---|
| `VP_RELEASE_DRY=1` | notarize + publish を skip（build / sign / `.dmg` のみ、creds 不要） |
| `VP_RELEASE_NO_PUBLISH=1` | notarize + staple まで（publish だけ skip、sha256 を出力） |
| `VP_RELEASE_SKIP_BUILD=1` | `cargo build` を skip（既存 binary 前提） |

## Step 7: 検証（**ログではなく実体で**）

```bash
# GitHub Release の実体
gh release view vX.Y.Z --json tagName,isDraft,isPrerelease,assets \
  --jq '{tag: .tagName, draft: .isDraft, prerelease: .isPrerelease, assets: [.assets[].name]}'

# latest が当該 tag か
gh api repos/chronista-club/vantage-point/releases/latest --jq '.tag_name'

# Homebrew cask の version / sha256 が build 時の値と一致するか
gh api repos/chronista-club/homebrew-tap/contents/Casks/vantage-point.rb --jq '.content' \
  | base64 -d | rg "version|sha256"
```

期待値: draft:false / prerelease:false / asset = `VantagePoint-X.Y.Z-arm64.dmg` / latest = 当該 tag /
cask の sha256 が `release:mac` ログの値と一致。

## Step 8: 完了報告

- version / Release URL / `.dmg` のサイズ
- notarize 結果（`spctl: accepted / source=Notarized Developer ID`）
- cask の version + sha256 一致確認
- インストール導線:
  ```
  brew upgrade --cask vantage-point   # 既存
  brew install --cask vantage-point   # 新規
  ```
