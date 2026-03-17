# リリースフロー

Vantage Pointのリリース手順を説明します。

## バージョニング

Semantic Versioning (SemVer) に従います。

```
v{major}.{minor}.{patch}[-prerelease]
```

- **major**: 破壊的変更
- **minor**: 新機能追加（後方互換）
- **patch**: バグ修正
- **prerelease**: `-alpha`, `-beta`, `-rc.1` など（プレリリース）

## GitHub Actions CI/CD

リリースはGitHub Actionsで自動化されています。

### ワークフロー構成

```yaml
# .github/workflows/ci.yml

on:
  push:
    branches: [main, feature/*]
    tags: ['v*']  # ← タグプッシュでリリースジョブ発動
  pull_request:
    branches: [main]

jobs:
  check:    # 常に実行
  release:  # タグ時のみ実行（checkが成功後）
```

### 自動実行される処理

| トリガー | 実行内容 |
|---------|----------|
| Push/PR | `cargo fmt --check`, `clippy`, `build`, `test` |
| タグ (`v*`) | 上記 + バイナリビルド + GitHubリリース作成 |

### リリースジョブの動作

1. タグとCargo.tomlのバージョンが一致するか検証
2. リリースバイナリ (`vp`) をビルド
3. GitHubリリースを自動作成
4. リリースノートを自動生成
5. プレリリース版 (`-alpha` 等) は自動でプレリリースとしてマーク

## リリース手順

### 1. 変更をfeatureブランチにまとめる

```bash
git checkout -b feature/xxx
git add -A
git commit -m "feat: 機能追加"
```

### 2. バージョンを更新

```bash
# Cargo.tomlのバージョンを更新
# [workspace.package]
# version = "x.y.z"
```

### 3. PR作成・マージ

```bash
git push -u origin feature/xxx
gh pr create --title "feat: v0.x.0 - 機能追加" --base main

# CIが通ったらマージ
gh pr merge --merge --delete-branch
```

### 4. タグプッシュ（自動リリース）

```bash
git checkout main
git pull origin main

# タグ付け＆プッシュ → CIが自動でリリース作成
git tag v0.x.0
git push origin v0.x.0
```

これでGitHub Actionsが自動的に：
- ✅ バージョン検証
- ✅ バイナリビルド
- ✅ GitHubリリース作成
- ✅ リリースノート生成

### 手動でリリースノートを追加したい場合

```bash
# リリース作成後に編集
gh release edit v0.x.0 --notes "追加のリリースノート"
```

## ローカル確認

リリース前に以下を確認：

```bash
# フォーマット
cargo fmt --all -- --check

# Clippy
cargo clippy --workspace --all-targets -- -D warnings -A dead_code

# テスト
cargo test --workspace

# ビルド
cargo build --release -p vantage-point
```

## リリースノートのテンプレート

自動生成されますが、手動で編集する場合：

```markdown
## Vantage Point vX.Y.Z

### 主な変更

- 機能A
- 機能B

### Breaking Changes

- XXXがYYYに変更

### インストール

\`\`\`bash
cargo install --path crates/vp-cli
\`\`\`

---

**Full Changelog**: https://github.com/anycreative-tech/vantage-point/compare/vA.B.C...vX.Y.Z
```

## チェックリスト

- [ ] バージョン番号更新（Cargo.toml）
- [ ] ローカルでテスト・lint通過確認
- [ ] PRマージ（CI通過後）
- [ ] タグ作成・プッシュ
- [ ] CI自動リリース確認
- [ ] （任意）リリースノート追記
