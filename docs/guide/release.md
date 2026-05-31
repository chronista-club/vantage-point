# リリースフロー

Vantage Point のリリース手順を説明します。

配布は **notarized `.dmg` 直配布（GitHub Releases）/ Homebrew cask / `cargo install`** の
三本柱（現状 macOS arm64 主軸）。`.dmg` の build → 署名 → notarize → publish → cask 更新は
すべて **`mise run release:mac`**（Ruby 製 mise task）に集約されている。

## バージョニング

Semantic Versioning (SemVer) に従います。

```
v{major}.{minor}.{patch}[-prerelease]
```

- **major**: 破壊的変更
- **minor**: 新機能追加（後方互換）
- **patch**: バグ修正
- **prerelease**: `-alpha`, `-beta`, `-rc.1` など（プレリリース）

## ブランチ運用 — nightly / main 二段

開発の最新は **nightly**、公開 release のみ **main** が進む二段運用。default branch は `nightly`。

| branch | 役割 | 直 push | PR | 更新元 |
|--------|------|--------|----|--------|
| **nightly** | 開発の最新版（day-to-day 積み上げ） | 可（force / deletion 禁止） | 任意 | lane → PR or 直 push |
| **main** | 公開 release の単位 | **禁止** | 必須（force / deletion 禁止） | nightly → release PR → tag cut |
| **lane / wing** | 単一タスク隔離 | 自由 | 必須 | from nightly |

```
nightly  ───────────────────────────────────►
            │                  │
            │ release PR       │ tag cut（vX.Y.Z）+ mise run release:mac
            ▼                  ▼
main    ───●──────────────────●──────────────►
                              │
                              ▼
                     GitHub Release（.dmg / homebrew cask / cargo install）
```

## CI（GitHub Actions）

CI（`.github/workflows/ci.yml`）は **fmt / clippy / test / security-audit のみ**を実行する。
**tag-trigger の release job は存在しない** — リリース成果物の build / publish は
ローカルの `mise run release:mac` が担う。

| トリガー | 実行内容 |
|---------|----------|
| PR → main、push → main | `cargo fmt --check`, `clippy`, `test`, security-audit |

## リリース手順

### 1. ローカル確認

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p vantage-point
```

### 2. バージョンを更新

```bash
# Cargo.toml ([workspace.package]) の version を x.y.z に更新
```

### 3. release PR（nightly → main）

```bash
# nightly を最新化してから release PR を作る
gh pr create --base main --title "release: vX.Y.Z"

# CI が green になったらマージ（nightly は trunk なので --delete-branch しない）
gh pr merge --merge
```

### 4. タグ cut + .dmg リリース

```bash
git checkout main
git pull origin main
git tag vX.Y.Z
git push origin vX.Y.Z

# notarized .dmg を build → sign → notarize → GitHub Release publish → cask 更新
mise run release:mac
```

`mise run release:mac`（`.mise/tasks/release/mac`）が一気通貫で行う処理:

1. **前提チェック** — Developer ID Application 証明書 / notarytool keychain profile (`vp-notary`)
2. **build** — `cargo build --release --target aarch64-apple-darwin -p vp-app -p vp-cli`
3. **`.app` 組立** — `VantagePoint.app`（`vp-app` 主 executable + 同梱 `vp` daemon + icon + Info.plist、`LSMinimumSystemVersion = 11.0`）
4. **codesign** — Developer ID + hardened runtime（inside-out で同梱 binary → bundle の順）
5. **`.dmg` 作成** — `hdiutil`（`/Applications` symlink 付き drag-install UX、`VantagePoint-<ver>-arm64.dmg`）
6. **notarize + staple** — `xcrun notarytool submit --wait` → `xcrun stapler staple` → `spctl` 検証
7. **GitHub Release publish** — `gh release create`（既存タグなら `gh release upload --clobber`）
8. **Homebrew cask 自動更新** — 末尾で `mise run release:cask` を best-effort 呼び出し

#### ドライ実行 / 部分実行

| 環境変数 | 効果 |
|---------|------|
| `VP_RELEASE_DRY=1` | notarize + publish を skip（build / sign / `.dmg` だけローカル検証、creds 不要） |
| `VP_RELEASE_NO_PUBLISH=1` | notarize + staple まで（`gh` publish だけ skip、sha256 を出力） |
| `VP_RELEASE_SKIP_BUILD=1` | `cargo build` を skip（既存 binary 前提、CI / メモリ逼迫時の検証用） |

### 5. Homebrew cask の更新（手動再試行）

`release:mac` の末尾で自動更新されるが、失敗した場合は単体で再試行できる。

```bash
mise run release:cask
```

`mise run release:cask`（`.mise/tasks/release/cask`）は tap repo
`chronista-club/homebrew-tap` の `Casks/vantage-point.rb` を現 version に揃える:

1. **sha256 算出** — ローカルの `target/dist/<dmg>` 優先、無ければ GitHub Release から download
2. **tap を temp に clone** → `version` / `sha256` の 2 行を差し替え → commit & push（idempotent、既に最新なら push しない）

## 前提（gate）

`mise run release:mac` には以下が必要:

- **Developer ID Application 証明書**（keychain に import 済）
- **notarytool keychain profile `vp-notary`**（App Store Connect API key 推奨）:

```bash
xcrun notarytool store-credentials vp-notary \
  --key <AuthKey.p8> --key-id <KEY_ID> --issuer <ISSUER_ID>
```

- icon: `crates/vp-app/assets/icon.icns`（自動 embed）

## チェックリスト

- [ ] バージョン番号更新（`Cargo.toml`）
- [ ] ローカルでテスト・lint 通過確認
- [ ] release PR（nightly → main）マージ（CI 通過後）
- [ ] タグ作成・プッシュ
- [ ] `mise run release:mac` で `.dmg` build → notarize → publish
- [ ] Homebrew cask 更新確認（自動 / `mise run release:cask`）
- [ ] （任意）リリースノート追記（`gh release edit vX.Y.Z --notes "..."`）
