# VP-SPEC-003: セルフアップデート

> **Status**: Draft
> **Created**: 2025-12-16
> **Updated**: 2026-03-10

---

## Overview

VP エコシステム（`vp` CLI + VantagePoint.app）のオートアップデート機能。
TheWorld が更新を検知し、ユーザー確認後に自動更新・再起動を行う。

---

## Requirements

### REQ-UPDATE-001: 更新チェック

TheWorld が起動時に GitHub Releases API で最新バージョンを確認。

- [ ] 起動時に自動チェック
- [ ] `CARGO_PKG_VERSION` との比較
- [ ] ネットワークエラー時は警告のみ（ブロックしない）

### REQ-UPDATE-002: ユーザー確認

VantagePoint.app にダイアログ表示。「今すぐ更新」「後で」「スキップ」。

- [ ] 確認ダイアログ表示
- [ ] 選択結果が TheWorld に送信される
- [ ] スキップ時は次回起動まで非表示

### REQ-UPDATE-003: VP CLI 更新

GitHub Releases からバイナリをダウンロードして置換。

- [ ] 正しいプラットフォームのバイナリ取得
- [ ] 既存バイナリのバックアップ
- [ ] 失敗時ロールバック

### REQ-UPDATE-004: VantagePoint.app 更新

Sparkle フレームワークまたはカスタム実装。

- [ ] アプリバンドル置換
- [ ] 署名検証
- [ ] 更新後自動再起動

### REQ-UPDATE-005: 再起動フロー

1. TheWorld に停止リクエスト
2. 稼働中 Process を graceful shutdown
3. バイナリ更新
4. TheWorld 再起動
5. VantagePoint.app 再起動

- [ ] セッション状態の保持
- [ ] 完了通知
- [ ] 更新ログ記録

### REQ-UPDATE-006: vp app コマンド

- [ ] `vp app` で VantagePoint.app 起動
- [ ] TheWorld 未稼働時は自動起動
- [ ] 起動中はフォーカス移動

---

## Architecture

```
VantagePoint.app ◄───► TheWorld 👑 (vp world)
       │                      │
       ▼                      ▼
  GitHub Releases        Project Process
```

---

## References

- `spec/01-core-concept.md` (VP-SPEC-001) — REQ6 プロセス管理, REQ7 Mac ネイティブ
