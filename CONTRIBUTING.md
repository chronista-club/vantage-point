# Contributing to Vantage Point

VP は **AI ネイティブ開発環境** です。コア機能の改善・バグ修正・ドキュメント・テストなど、あらゆる貢献を歓迎します。

## ライセンス

VP は **MIT OR Apache-2.0** dual license で公開しています。
コントリビュートされたコードも同じ dual license で受け入れます。

## DCO (Developer Certificate of Origin)

PR には [DCO](https://developercertificate.org/) サインオフを推奨します。
コミット時に `git commit -s` を付けると `Signed-off-by: Your Name <email>` が自動付与されます。

複雑な CLA は採用しません。

## Platform Tier

| Tier | OS | スコープ | 状態 |
|------|-----|--------|------|
| **Tier 1** | macOS 13+ | 全機能 (Swift VantagePoint.app + cross-platform vp-app + CLI) | CI green 必須 |
| **Tier 1** | Windows 11 | cross-platform vp-app + CLI (no MIDI) | CI green 必須 (`check-windows`) |
| **Tier 2** | Linux | CLI + Web UI 目標 | コミュニティメンテ、CI 未整備 |

- **Tier 1 (macOS / Windows)**: 全 PR で動作保証、CI matrix に含む
- **Tier 2 (Linux)**: メンテナは未検証。動作不良の修正 PR・互換性改善 PR を歓迎。CI matrix で fail しても block しない (fail tolerance)

Linux ユーザは「動かない」issue 歓迎ですが、最速は **PR で fix を提供**。

## Issue / PR フロー

VP の Issue 管理は **Linear に一元化** ([Vantage Point project](https://linear.app/chronista/project/vantage-point-d0b78d9cb67e))。GitHub Issues は基本的に使いません。

1. **Issue 作成 (推奨、trivial fix は不要)** — Linear で起票
   - チーム: `Vantage Point`、プロジェクト: `Vantage Point` を指定
2. **branch 作成** — Linear が生成する `mako/vp-XX-...` 形式を使用 (base = `main`)
3. **実装 + テスト**
   ```bash
   cargo test --workspace
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   ```
4. **PR 作成**
   - title は短く、body に背景・変更点・テスト方法を記載
   - 関連 Linear Issue は `Closes VP-XX` で紐付け (merge 時に Linear が自動 close)
5. **CI 通過 + レビュー → squash merge**

CI は GitHub Actions で `Format` (Linux) / `Clippy + Test` (macOS) / `Check (Windows)` / `Security Audit` の 4 job が走ります。

## 開発環境

詳細は [`README.md`](README.md) と [`CLAUDE.md`](CLAUDE.md)、build / run の具体例は [`docs/guide/setup.md`](docs/guide/setup.md) と [`.mise.toml`](.mise.toml) を参照。

最低限:
- **Rust 1.94+** (mise / rustup で管理、workspace で `1.94.0` を pin)
- **Claude CLI** (VP のエンジン — [Claude Code](https://docs.anthropic.com/en/docs/build-with-claude/claude-code))
- **mise** (推奨、tool / env / task 統合)
- **macOS app 開発時**: Xcode 15+
- **Windows app 開発時**: Git Bash (MINGW64) + scoop 経由で `mingw` + `nasm`

## コミュニケーション

- **Issue**: バグ報告、機能要望、議論
- **PR**: 実装の提案
- **Discussions**: 設計議論、質問（GitHub Discussions、後で有効化）

## 行動規範

[Code of Conduct](CODE_OF_CONDUCT.md) (Contributor Covenant 2.1) に従ってください。

## セキュリティ

セキュリティ脆弱性は public issue ではなく [SECURITY.md](SECURITY.md) の手順に従って報告してください。

---

質問があれば issue で気軽に聞いてください。
