# 70. Codex Chat の日常開発対応

> **Status**: Draft
> **Related**: design 67、69、`mem_1CeySwxuoVc17bGLnU5Np3`
> **対象**: `conversation/codex_interactions.rs`, `conversation/codex_host.rs`, `webview/codex-interactions.tsx`, `webview/chatview.tsx`

GFP の開発を Codex で再開できるよう、会話中の入力・承認・外部ツールからの要求を扱う。
CLI の確認操作と git push はユーザーの許可リストで扱い、会話の質問・承認は維持する。

## 実装と検収の単位

- 承認: native が提示する選択肢を尊重する。`decline` と `cancel` を区別して表示・応答し、
  Chat 内で表示・許可・拒否／中断を実機確認する。
- 独立権限: 対象 environment・ファイル範囲・ネットワーク・理由を表示し、要求の範囲内で
  許可する項目と turn / session の期間を選ぶ。未要求の権限は配送しない。
- MCP elicitation: server 名を表示し、フォームの型・必須条件に沿って回答する。
  URL 手続きは明示的に開き、開いたことを完了とみなさない。辞退とキャンセルを返せる。
  turn ID がない要求も thread と native request ID で管理する。
- 通常入力と Queue: 現行 native API の追加・編集・取消・順序・開始・再接続を実測してから
  状態の所有者を決める。「今伝える」と「次に実行する」を表示し、受理不明の入力を再送しない。
- 設定とモード: 実効 approval / sandbox を表示し、Plan と通常実行の切替を検証する。

## 実測の根拠

Codex CLI 0.154.0 の `app-server generate-ts --experimental` で生成した型を根拠にする。
旧ローカル Codex ソースだけで現在の protocol を決めない。

2026-09-13、同じ会話の Console では承認操作が成功し、Chat では直ちに失敗した。
daemon の stderr に native が受けた VP の JSON-RPC error が記録されていた:
`今回のみの拒否をサポートしない承認要求`。
`Interactions::receive` が `availableDecisions` に `decline` を必須としており、
`cancel` を提示する有効な要求をカード表示前に拒否する。実効設定は `on-request` と
`workspace-write` であり、`approval_policy = never` による拒否ではない。

## やってはいけない

- 承認表示の不具合を全面バイパス設定へ戻して隠さない。
- 提示されていない選択肢を送らない。中断する応答を単なる拒否と表示しない。
- 履歴から承認権限を復元しない。別 thread へ権限・入力を配送しない。
- 秘密入力を会話本文や診断ログへ複写しない。
- 未対応のフォームを適当に文字列化して受理済みにしない。
- Queue の同じ入力を VP と native の両方で所有しない。

## Status log

- 2026-09-13: ユーザーの「やりきってしまおう」で残作業に GO。
  PR #1125 を全 CI 成功後に nightly へ統合。承認エラーの原因を実機ログで特定。
  独立権限・MCP・Queue・設定の実装と実機検収は未完了。
- 承認の修正では `decline` がなければ、提示された `cancel` を応答に使う。
  `cancel_on_deny` をイベントに付け、カードに「許可せずターンを中断」と表示する。
  `accept` の提示有無・対象コマンドの検証は維持する。
  Rust の回帰テストで実機と同じ拒絶を再現してから修正し、承認関連 11 テストが成功。
  DOM でも従来の「拒否」表示で失敗することを確認し、修正後の WebView は 600 テスト成功。
  Chat での再検収はアプリと daemon への反映後に行う。
- 修正後の `mise run test` は成功（主要ライブラリ 1,102 成功・除外 11、失敗 0）。
  workspace の check / Clippy、WebView typecheck、fmt も成功。
