# VP-SPEC-003: セルフアップデート

> **Status**: 未整理 Draft — 一部実装済 (2026-05-16、VP-191 棚卸し時点)
> **Created**: 2025-12-16
> **Updated**: 2026-05-16

---

## Overview

VP エコシステム（`vp` CLI + VantagePoint.app）のオートアップデート機能。
TheWorld が更新を検知し、ユーザー確認後に自動更新・再起動を行う。

---

## 実装状況サマリ (2026-05-16、VP-191 棚卸し)

本 spec は **未整理 Draft**。要件は当初構想のまま残っており、現状コードとの突き合わせは未完。
確認済の実装状況は下記の通り:

| 領域 | 状態 | 根拠 |
|------|------|------|
| `vp update [--check]` CLI | ✅ 実装済 | `commands/update.rs` + `capability/update_capability.rs` (GitHub Releases API でチェック・ダウンロード・置換) |
| HTTP update routes (`/api/update/check` / `apply` / `rollback` / `restart` / `mac-check`) | ✅ 実装済 | `process/routes/update.rs` |
| GitHub Releases 比較 (`CARGO_PKG_VERSION`) | ✅ 実装済 | `UpdateCapability::check_update` |
| VantagePoint.app 更新ダイアログ (REQ-UPDATE-002) | ⏳ 未整理 | vp-app は wry 移行済、Sparkle 前提の REQ-UPDATE-004 は要再設計 |
| TheWorld 起動時自動チェック (REQ-UPDATE-001) | ⏳ 未確認 | 起動フックの有無は要調査 |
| graceful 再起動フロー (REQ-UPDATE-005) | ⏳ 未整理 | route は存在するが REQ の全項目の充足は未検証 |

> **注意**: 下記 REQ-UPDATE-001〜006 は当初構想 (2025-12) のままで、checkbox は当時のもの。
> REQ-UPDATE-004 の "Sparkle フレームワーク" は vp-app の SwiftUI→wry 移行 (2026-04-26) で前提が変わったため再設計が必要。
> 本 spec の正式整理は別 Phase で行う。

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
