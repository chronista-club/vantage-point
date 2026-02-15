---
description: "Vantage Pointのリリースフロー（ローカル検証→マージ→バージョンアップ→タグ→リリース→検証）"
allowed-tools: Bash, Read, Edit, Grep, Glob, AskUserQuestion
---

# Vantage Point リリースフロー

以下のステップを順番に実行し、各ステップの結果を報告してください。
エラーが発生した場合は即座に停止し、状況を報告してください。

## Step 1: 事前チェック

1. `git status` で作業ツリーがクリーンか確認（未コミットの変更があれば報告して停止）
2. 現在のブランチを確認
3. mainブランチでない場合、PRが存在するか確認（`gh pr view`）

## Step 2: ローカル品質チェック（macOS専用アプリのためCIの代わり）

1. `cargo fmt --all -- --check` でフォーマット確認
2. `cargo clippy --workspace --all-targets -- -W clippy::all` でlint確認
3. `cargo test --workspace` でテスト実行
4. いずれかが失敗 → 状況を報告して停止

## Step 3: CI確認（フォーマットチェック）

1. PRがある場合: `gh pr checks` でCI状態を確認
2. CI未通過またはpendingの場合 → ユーザーに続行するか確認（ローカルチェックは通過済み）

## Step 4: マージ（featureブランチの場合）

1. `gh pr merge --squash --delete-branch` でマージ
2. `git checkout main && git pull` でmainを最新に

## Step 5: バージョンアップ

1. 現在のバージョンを `Cargo.toml` のworkspace versionから取得して表示
2. ユーザーにバージョンの種類を質問（patch / minor / major）
3. `Cargo.toml` のworkspace versionを更新
4. コミット: `release: v{VERSION}`
5. `git push origin main`

## Step 6: タグ & リリース

1. `git tag v{VERSION}`
2. `git push origin v{VERSION}`
3. ローカルでリリースビルド: `cargo build --release --target aarch64-apple-darwin -p vantage-point`
4. `gh release create v{VERSION} target/aarch64-apple-darwin/release/vp --title "v{VERSION}" --generate-notes --latest`

## Step 7: インストール & 検証

1. `cargo install --path crates/vantage-point` でローカルインストール
2. `vp --version` でバージョン確認（期待値: {VERSION}）
3. `gh release view v{VERSION}` でリリースが正しく作成されたか確認

## Step 8: 完了報告

リリース結果をまとめて報告:
- バージョン: v{VERSION}
- リリースURL: https://github.com/chronista-club/vantage-point/releases/tag/v{VERSION}
- ビルド成果物の確認結果
