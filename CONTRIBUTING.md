# Contributing to Vantage Point

VP は **AI ネイティブ開発環境** です。コア機能の改善・バグ修正・ドキュメント・テストなど、あらゆる貢献を歓迎します。

## ライセンス

VP は **MIT OR Apache-2.0** dual license で公開しています。
コントリビュートされたコードも同じ dual license で受け入れます。

## DCO (Developer Certificate of Origin)

PR には [DCO](https://developercertificate.org/) サインオフを推奨します。
コミット時に `git commit -s` を付けると `Signed-off-by: Your Name <email>` が自動付与されます。

複雑な CLA は採用しません。

## Platform Tier 制度

| Tier | OS | メンテナンス責任 | スコープ |
|------|-----|----------------|--------|
| **Tier 1** | macOS | maintainers | 全機能、CI green 必須 |
| **Tier 2** | Linux | community | CLI + Web UI 目標、breakage は PR 歓迎 |
| **Tier 2** | Windows | community | CLI + Web UI、tmux は WSL or 代替 |

- **macOS Tier 1**: 全 PR で動作保証、CI matrix に含む
- **Linux/Windows Tier 2**: メンテナは検証していません。動作不良の修正 PR・互換性改善 PR を歓迎。CI matrix で fail しても block しない（fail tolerance）

Linux/Windows ユーザは「動かない」issue を歓迎しますが、最速の解決は **PR で fix を提供** です。

## PR フロー

1. **Issue 作成**（推奨、ただし trivial fix は不要）
   - bug / feature / docs から template を選択
2. **branch 作成**（base = `main`、命名: `feature/...`, `fix/...`, `docs/...`）
3. **実装 + テスト**（`cargo test --workspace`）
4. **format + lint**（`cargo fmt --all && cargo clippy --workspace --all-targets`）
5. **PR 作成**
   - title は短く、body に背景・変更点・テスト方法を記載
   - 関連 Issue は `Closes #XX` で紐付け
6. **CI 通過 + レビュー → squash merge**

## 開発環境

詳細は [`README.md`](README.md) と [`CLAUDE.md`](CLAUDE.md) を参照。

最低限:
- Rust 1.95+ (mise / rustup で管理)
- Xcode 15+ (macOS app 開発時)
- tmux 3.4+
- (engine: Claude CLI — VP の AI engine として）

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
