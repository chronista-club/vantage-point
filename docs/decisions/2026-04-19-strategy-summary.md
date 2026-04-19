# VP 戦略決定総まとめ (2026-04-19)

2026-04-18 + 2026-04-19 の 2 セッションで確定した戦略決定の総決算。

## 1. Positioning

**「AI ネイティブ開発環境」一本** で打ち出す。Claude には言及しない、静かな佇まい。

- README タグライン候補: "AI ネイティブ開発環境" / "AI-native development environment"
- Claude を内部実装として淡々と存在させる（fact だが宣伝しない）
- 抽象化レイヤーは作らない（YAGNI）
- 他 LLM 対応 issue が来ても "By design, currently Claude" で簡潔に return
- 利点: positioning 明確、想定ユーザー流入、将来の柔軟性も維持

## 2. ccwire ビジョン転換

ccwire は **削除せず、tmux power-tool として進化**。messaging は VP mailbox に完全委譲（オミット）。

### 役割分離

| コンポーネント | 責務 |
|--------------|------|
| **VP Mailbox** | actor 間 messaging（cross-Process 含む） |
| **ccwire** | tmux session orchestration / pane lifecycle |
| **`vp tmux`** | tmux primitive operations |
| **`claude-plugin-ccwire` の wire-send/receive/status** | 削除予定（messaging は mailbox 集約） |

### ccwire 進化方向（次セッション spec）

Pane orchestration / metadata 管理 / capture-pane 拡張 / send-keys マクロ /
monitor-activity / pipe-pane / Hooks 連携 / Format クエリ等。
`vp tmux` との境界線は spec 起こし時に決定（A: 吸収 / B: primitive vs workflow / C: 単一 vs inter-session）。

詳細: `docs/design/03-mailbox-vs-ccwire.md`

## 3. クロスプラットフォーム戦略

| Tier | OS | メンテ責任 | スコープ |
|------|-----|----------|---------|
| **Tier 1** | macOS | 自分 + maintainers | 全機能、CI green 必須 |
| **Tier 2** | Linux | **community-supported** | CLI + Web UI（Canvas）目標、breakage は PR 歓迎 |
| **Tier 2** | Windows | **community-supported** | CLI + Web UI、tmux は WSL or 代替 |

### 実装方針

- 自分は macOS のみ保証、Linux/Windows は friends / community に委ねる
- コードは cross-platform 意識（path 抽象化済）
- CI matrix に Linux/Windows job 追加（fail tolerance あり）
- CONTRIBUTING.md に Tier 制度 + community 歓迎セクション

### やらない

- Linux/Windows での実機検証
- platform-specific UI 実装（必要時受け入れ）

## 4. OSS 公開準備チェックリスト

### 必須最小セット（公開ライン、1-2 日工数）

- [ ] LICENSE-MIT / LICENSE-APACHE 配置
- [ ] Copyright header 統一（`// Copyright (c) 2026 Anycreative Inc.`）
- [ ] NOTICE / 第三者ライセンス（`cargo-about` 自動生成）
- [ ] README 大幅拡充（tagline は "AI ネイティブ開発環境"）
- [ ] CONTRIBUTING.md（DCO 推奨、PR フロー、Tier 制度）
- [ ] CODE_OF_CONDUCT.md（Contributor Covenant）
- [ ] SECURITY.md
- [ ] `.github/ISSUE_TEMPLATE/` + PR テンプレート
- [ ] Bundle ID は `tech.anycreative.VantagePoint` のまま

### 推奨

- [ ] ロゴ / ヒーロー画像 / スクリーンショット
- [ ] ランディングページ（`vantage-point.app`）
- [ ] デモ GIF / 動画
- [ ] Quickstart 動画

### 軽リスク（許容）

- macOS only — Tier 制度で明示
- Apache 2.0 が patent grant 含むので OK
- `rustls` 等の crypto は OSS export 例外
- 機密 / 内部参照はサニタイズ済（1Password / Linear ID 等）

## 5. Cloud 機能ロードマップ

`vantage-point.app` ドメイン取得済。OSS コア + SaaS 上物のハイブリッド構成。

| 段階 | 機能 |
|------|------|
| A | **Creo ID サインイン + 個人設定同期**（最初の足場）|
| B | セッション sync（cross-device 引き継ぎ） |
| C | Narrative 共有（VP流 原則7 直接実装） |
| D | Stand マーケットプレイス |

実装順 A→B→C→D。A は Phase 5（OSS 公開）の基礎。

## 6. 直近 TODO（優先順）

1. **必須 OSS ドキュメント整備**（License / README / CONTRIBUTING / Code of Conduct / Security / Issue templates）
2. **ccwire 進化 spec 起こし**（次セッション、A/B/C 案決定 + 機能スコープ）
3. **Cloud A 着手**（Creo ID 統合 — creo-memories 側 Phase 1 完了済の Auth0 流用）
4. **Computer-use 完全対応**（draft あり）
5. **`claude-plugin-ccwire` の messaging 系削除 PR**（別リポ）
6. **Ruby worker レーン追加**（保留）

## 7. 今日 (2026-04-18 + 19) merge 済 PR

| # | 内容 |
|---|------|
| #139 | tmux pane ID label |
| #140 | Mailbox Phase 1 (persistent) |
| #141 | ambiguous 幅文字 fix |
| #143 | Rust 1.95 clippy fixes |
| #144 | Mailbox Phase 2 (TTL/manual_ack/GC) |
| #145 | workspace cleanup + /Applications 反映 |
| #146 | Mailbox Phase 3 Step 1 (registry) |
| #147 | Mailbox Phase 3 Step 2a (client + 5 改善) |
| #148 | Mailbox Phase 3 Step 2b (runtime wiring) |
| #149 | Mailbox Phase 3 Step 3 (ccwire 役割明示) |
| #150 | Cmd+D fix (SwiftUI .commands) |

11 PR merged、Mailbox Phase 3 完結。

## 関連 creo-memories

各個別決定は creo-memories に保存済（次セッション以降 search 可能）:

- OSS 化 + MIT/Apache dual: `mem_1CaB5LdpcdqgrMfkTPAWKV`, `mem_1CaB5PmdWNfPPVR1UkFYLC`
- クラウド統合 + Creo ID: `mem_1CaB5kXQ5cPGs3tduEZ8Du`
- Cloud 機能順序 A→B→C→D: `mem_1CaB5qVydzxkc6cnvNMMqU`
- vantage-point.app 取得: `mem_1CaB5v1cTrCE4REd2ii5vy`
- Route Path C 確定: `mem_1CaBDETcyDY5YKeYpXBf2j`
- Mailbox Address 形式仕様: `mem_1CaBRBdh1PGop2iGLAnwSY`
- Computer-use 対応 Issue Draft: `mem_1CaAvGr1BYe3WP81mFKknV`
- VP TODO 順序: `mem_1CaBD32Voi3NSpQznQLioq`
