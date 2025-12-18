# .creo/ - プロジェクトメモリ基盤

creo-memoriesのローカルキャッシュ。スペース×カテゴリで同期。

## 構造

```
.creo/
├── {space-name}/           # workspace と 1:1 対応
│   ├── spec/               # 仕様・要件定義
│   ├── design/             # 設計書
│   ├── guides/             # ガイド
│   ├── learning/           # 学び・調査結果
│   ├── decision/           # 意思決定
│   ├── debug/              # デバッグ記録
│   └── task/               # タスク・計画
└── .sync.json              # 同期メタデータ
```

## ディレクトリ = カテゴリ

| ディレクトリ | category | 用途 |
|-------------|----------|------|
| `spec/` | `spec` | 仕様・要件（REQ-XXX） |
| `design/` | `design` | 設計書 |
| `guides/` | `guides` | 開発ガイド |
| `learning/` | `learning` | 学び・調査結果 |
| `decision/` | `decision` | 意思決定記録 |
| `debug/` | `debug` | デバッグ・トラブルシュート |
| `task/` | `task` | タスク・計画 |

## 同期

```bash
vp creo sync    # CLIで同期
```

## 規約

- 1スペース = 1ディレクトリ = 1 creo-memories workspace
- 1ファイル = 1メモリ（セクション単位）
- ディレクトリ名 = creo-memories category
