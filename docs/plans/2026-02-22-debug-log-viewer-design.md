# Debug Log Viewer 設計書

**日付**: 2026-02-22
**ステータス**: 承認済み

## 背景

MCP → QUIC → Stand のメッセージフローが複数プロセスにまたがるため、問題の切り分けが困難。
リアルタイムでログを追跡できるデバッグツールが必要。

## コンセプト

- **デバッグ特化**のログビュワー
- **2プロセス統合**: `vp mcp` と `vp start` (Stand) のログを共通ファイルに集約
- **コリレーション ID**: `trace_id` で1リクエストの全ステップをフィルタリング
- **表示先**: ファイル（tmux/tail）+ Canvas WebSocket ストリーム

## アーキテクチャ

```
vp mcp  ──tracing-appender──┐
                             ├──> ~/.config/vantage/logs/debug.log
vp start ──tracing-appender──┘
                                    │
                              notify (file watcher)
                                    │
                              Stand hub.broadcast()
                                    │
                              WebSocket → Canvas ログビュワー
```

## コリレーション ID

MCP のツール呼び出しごとに `trace_id`（例: `t-3a8f`）を生成。
QUIC リクエストのペイロードに付与し、Stand 側で引き継ぐ。

```json
{"ts":"...","process":"mcp","trace_id":"t-3a8f","step":"connect","msg":"QUIC connecting to [::1]:34001"}
{"ts":"...","process":"mcp","trace_id":"t-3a8f","step":"open_channel","msg":"Opening 'stand' channel"}
{"ts":"...","process":"stand","trace_id":"t-3a8f","step":"receive","msg":"show request received"}
{"ts":"...","process":"stand","trace_id":"t-3a8f","step":"broadcast","msg":"Broadcasted to 2 clients"}
{"ts":"...","process":"mcp","trace_id":"t-3a8f","step":"response","msg":"OK in 45ms"}
```

## トレースポイント

### MCP 側 (`vp mcp`)

| ステップ | 内容 |
|---------|------|
| `connect` | `ensure_channels()` → QUIC 接続開始/成功/失敗 |
| `open_channel` | チャネルオープン成功/失敗 |
| `request` | `ch.request()` 送信 |
| `response` | レスポンス受信 + 所要時間 |
| `error` | エラー発生時の詳細 |

### Stand 側 (`vp start`)

| ステップ | 内容 |
|---------|------|
| `accept` | QUIC 接続受付 |
| `channel_open` | チャネルオープンリクエスト受信 |
| `receive` | リクエスト受信（メソッド名 + ペイロードサマリ） |
| `process` | 処理開始 |
| `broadcast` | WebSocket クライアントへの配信 |
| `respond` | レスポンス送信 |

## ログファイル

- **パス**: `~/.config/vantage/logs/debug.log`
- **フォーマット**: JSON Lines (1行1エントリ)
- **ローテーション**: 起動時に前回分を `.log.1` にリネーム（直近1世代のみ保持）
- **有効化条件**: `RUST_LOG` 環境変数 or `vp start -d simple` 以上

## Canvas ログビュワー UI

- リアルタイムストリーム（自動スクロール）
- フィルタ機能:
  - `trace_id` で絞り込み（1リクエストの全ステップ）
  - `process` で絞り込み（mcp / stand）
  - `level` で絞り込み（debug / info / warn / error）
- 各ログ行はクリックで詳細展開（ペイロード JSON）
- 既存の debug モードパネルを拡張

## 変更対象

| コンポーネント | 変更内容 |
|---------------|---------|
| `vp mcp` (mcp.rs) | tracing-appender 追加、各ステップに trace_id 付きログ |
| `vp start` (Stand) | tracing-appender 追加、unison_server.rs にログ追加 |
| Stand server | ログファイル監視 (notify) → WebSocket broadcast |
| Canvas HTML | ログビュワー UI |
| unison protocol | request ペイロードに trace_id フィールド追加 |

## 追加 crate

- `tracing-appender` — ファイルへのログ出力
- `notify` — ファイル変更監視

## 非スコープ

- 運用監視・メトリクス
- ログの永続保存・検索
- リモートからのログ閲覧
