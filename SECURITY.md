# Security Policy

## サポート対象バージョン

VP は活発に開発中のため、**最新リリース系列のみセキュリティ修正を提供** します。

| Version | Supported |
|---------|-----------|
| 0.11.x  | :white_check_mark: |
| < 0.11  | :x: |

## 脆弱性の報告

セキュリティ脆弱性を発見した場合、**public な GitHub Issue では報告しないでください**。

代わりに以下のいずれかの方法で連絡してください:

1. **メール**: [mito@chronista.club](mailto:mito@chronista.club)
   - 件名に `[VP Security]` を付けてください
2. **GitHub Security Advisories**: [Report a vulnerability](https://github.com/chronista-club/vantage-point/security/advisories/new)

### 報告に含めてほしい情報

- 脆弱性の概要と影響範囲
- 再現手順（PoC があれば）
- 影響を受けるバージョン
- 想定される修正方針（任意）

## 対応プロセス

1. **受領確認**: 報告から **3 営業日以内** に受領を確認
2. **初期評価**: 1 週間以内に深刻度を評価し、対応方針を共有
3. **修正開発**: 深刻度に応じて優先度を決定
4. **公開**: 修正リリース後、Security Advisory を公開

## 開示ポリシー

- **Coordinated disclosure** を採用
- 報告者と相談の上、修正リリース後に詳細を公開
- 報告者のクレジットを Advisory に記載（希望時）

## サポートされる脅威モデル

VP はローカル開発環境で動作するツールであり、以下を脅威モデルの主対象とします:

- ローカル PTY / プロセス管理に関する権限昇格
- MCP / Unison QUIC レイヤーの認証バイパス
- WebView レンダリングの XSS / コンテンツ injection
- Claude CLI への意図しないコマンド注入

以下は **対象外** です（責任範囲外）:

- Claude CLI 自体の脆弱性 → Anthropic に報告してください
- ユーザの設定ミスによる露出（例: 公開ネットワーク上での `vp start`）
- サードパーティ依存の脆弱性は upstream に報告した上で、必要に応じて patch
