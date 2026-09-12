# 67. Codex Chat の質問・承認応答

> **Status**: Draft
> **Related**: design 41、65、66、`mem_1CeySwxuoVc17bGLnU5Np3`
> **対象**: `conversation/codex_host.rs`, `conversation/codex_interactions.rs`, `webview/codex-interactions.tsx`

Codex の質問とコマンド・ファイル変更の承認を Chat で回答する。
ユーザー裁定は「質問・承認への応答やろう」。既存 Claude の control protocol は維持する。

## 要求と応答

インストール済み Codex 0.154.0 の生成 JSON schema を基準とする。
[公式 app-server 文書](https://learn.chatgpt.com/docs/app-server) で承認通知の順序と失効通知を照合する。
`item/tool/requestUserInput`、`item/commandExecution/requestApproval`、
`item/fileChange/requestApproval` を扱う。未知の要求には JSON-RPC error を返す。
質問の回答は質問 ID → `{answers:[text]}`。同じ質問文でも ID で区別する。
承認は今回の `accept` / `decline`。継続的な許可ルールの保存は追加しない。
ネットワーク承認は専用の見出しと、接続先を含む詳細を表示する。承認対象やファイルの差分が
取得できない要求は拒否のみ可能にし、ファイル差分は要求への移管後にキャッシュを解放する。
質問のキャンセルは空の回答 map として返す。turn の停止は既存 interrupt 操作を使う。
秘密入力は password 欄とし、回答を会話履歴へ保存しない。

Chat の起動で approvalPolicy / sandbox を上書きせず、native の設定を尊重する。
質問の experimental API は initialize で opt-in する。Plan mode の操作は別段階。
独立した権限要求 `item/permissions/requestApproval` と MCP elicitation は本段階の対象外。
既存設定が承認不要ならカードは発生しない。

## ライフサイクル

- native request ID（文字列または整数）は型を保持。UI 向け ID は host 世代と採番を含め、
  host 再生成後の古いカードが新しい要求へ答えないようにする。
- 対象 thread / turn を照合し、未回答要求を host 状態に保持する。
- 表示状態は Codex 専用の一括 snapshot。履歴 snapshot と同じ lock で順序付ける。
  過去の transcript から承認を再発火しない。再接続時は現 host の未回答だけを配送する。
- 回答の検証後、送信中として確保する。二重応答を防ぎ、書込成功後だけ要求を取り除く。
  書込失敗は要求を失効させ、途絶を報告する。勝手に再送・再承認しない。
- `serverRequest/resolved`、turn 完了、host 途絶で失効する。非 blocking 質問は
  native の指定を保持するが、進行中 turn の表示・type-ahead を誤って完了させない。
- GUI の送信結果は要求 ID 付きで返す。失敗を成功表示へ変換しない。
  承認・回答待ちには通常送信とは独立した状態を持つ。

## 検証

実 JSONL reader → 要求表示 → 元 ID への回答、許可／拒否、同文の別質問、
別 thread、二重回答、失効、再接続、送信失敗、他 session との隔離を確認する。
Rust ↔ TypeScript の型・fixture 契約と既存 Chat テストも通す。
実機では質問への回答、承認と拒否、Chat 往復、engine 停止中の要求失効を検収する。

## やってはいけない

- 質問文を回答のキーにする、整数と文字列の native ID を混同する。
- UI のクリックを応答成功と扱う、古い履歴の承認を操作可能にする。
- 承認対象の詳細を省略したまま包括的な許可を送る。

## Status log

- 2026-09-12: 最新 nightly の確認後、ユーザーが本段階を選択。native schema と既存経路を調査。
- 2026-09-12: 要求受付・回答の JSONL 送信・Chat カードを実装。`mise run test` は
  1,412 成功・失敗 0・除外 23、WebView の `bun run test` は 592 成功。
  WebView の型チェック・bundle ビルド、compile check、Clippy も成功。実機での操作確認は未実施。
