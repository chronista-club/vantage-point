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

## 独立権限の応答

`item/permissions/requestApproval` は既存の thread / turn 検証と未回答台帳を使う。
表示用の項目 ID から、host が保持する要求内の権限へ対応付ける。UI からパスや権限
profile を受け取ってそのまま転送しない。質問と同じ session 下書きに選択を保持するが、
権限要求そのものは履歴や別 host に復元しない。

ネットワークと legacy read / write の各パスは個別に選択する。`entries` を含む
ファイル権限は deny や glob の関係を保つため全体を一項目として扱い、ルールを表示する。
未対応の権限形式は許可せず、黙って切り捨てた部分的な profile を作らない。
初期状態は全項目未選択、期間は turn。明示的に session を選んだ場合だけ session を返す。
「許可しない」は空の `permissions` と `scope: turn` を返す。

## MCP elicitation の応答

`mcpServer/elicitation/request` の native request ID を既存台帳で管理する。
turn ID は相関情報として扱い、null や過去の turn でも thread ID が一致すれば受け付ける。turn の完了だけでは
MCP 要求を捨てず、native の resolved 通知、回答の配送、host 終了で解放する。

form / openai/form / openaiForm は表示できる平坦な object schema を受け付ける。
文字列・数値・真偽値・単一選択・複数選択を表示し、文字列の下書きから元の型に変換する。
任意項目の未回答と false を区別する。制約検証は `jsonschema` に委譲し、format 検証も有効にする。
検証器の API は [jsonschema の公式ドキュメント](https://docs.rs/jsonschema/0.56.0/jsonschema/) を参照する。
外部参照のネットワーク・ファイル取得は依存機能で無効化する。表示できない制約や形式は
理由を表示し、回答を無効にする。検証エラーへ回答本文を複写しない。

URL 手続きは http(s) のリンクを明示的に開く。開いただけでは完了通知を送らず、
ユーザーが完了を確認してから accept を返す。辞退は decline、手続きのキャンセルは
cancel を返す。これはコマンド承認の turn 中断とは別の MCP 応答である。
カードは本文リンクの委譲ハンドラーの外側にも表示されるため、カード自身が
WebView 内遷移を止め、既存の `open-url` IPC へ明示操作を渡す。
`openai/userVerification` は未対応として説明し、辞退・キャンセルのみ返せる。

## Queue と実行中の入力

実行中の Enter は「今伝える」（turn/steer）。表示している turn ID を条件として送り、
その turn が終了・変更されていたら拒否を表示して本文を保持する。新しい turn への自動振替はしない。
「次に実行する」は別ボタンで thread/queue/add に渡す。

受理後の待機入力は Codex が唯一の所有者。VP は順序付きの表示用 snapshot と編集中の下書きを持つ。
各項目は native ID で編集・取消・並べ替えを指定する。添付や特殊入力の情報を失う編集は無効にする。
停止後は待機一覧を残し、「再開」で先頭を開始できる。正常終了後の次開始と、保存済みの Queue の
再接続時の扱いは Codex の状態に従う。
待機入力は直前のターン設定を引き継ぐため、待機入力が残る間はモデル・effort の変更を無効にする。

thread/queue/changed で snapshot を無効化し、単一の取得処理が全ページを読み直す。
ページ取得中に更新通知が来た場合は取得途中の一覧を捨てて読み直す。表示上限や未知形式のエラー時は
直前の一覧を残して操作を無効にする。古い一覧を使った並べ替えや別 thread への配送は拒否する。
取得エラーからは「一覧を再取得」で読み直せる。これは入力の再送ではない。

通常の conversation_submit の再起動・再試行経路とは別の conversation_codex_input を使う。
UI 呼び出し元の切断だけでは配送を途中破棄しない。stdin 書込だけで受理済みにせず native RPC の ACK を待つ。
stdin lock の取得から ACK までに 35 秒の期限を設ける。書込未完了で期限切れ・失敗になった場合は、
途中の JSONL を再利用しないよう host を終了扱いにし、子プロセスを停止する。
書込完了後の ACK 待ちだけが期限切れなら host を残して一覧を読み直す。
受理不明時は再送せず、入力と説明を残す。同じ clientUserMessageId の add は native 側で重複を防がない。
待機入力を会話本文へ先に挿入せず、native の開始と履歴に基づいて表示する。

## Status log

- 2026-09-13: Enter の steer と native Queue の追加・編集・取消・並べ替え・明示再開を実装。
  Team B（Moody Blues）のレビューで書込待ちの期限不足を検出し、停止した writer の回帰テストを
  RED → GREEN にした。子プロセスの終了も検証し、再レビューはソース上 PASS。
  修正後の workspace テストは成功。VP Chat での Queue 実機確認は引き続き未完了。

- 2026-09-13: MCP は Chat 実機で型を保持したフォーム回答（数値・false・配列）、辞退、
  キャンセル後の会話継続、URL のブラウザ表示と明示的な完了を確認した。一時 MCP 登録は削除済み。
  追加権限と MCP は draft PR #1127。Queue を別 branch `mako/codex-queue` で進め、
  Queue 実装後に Team B が全体をレビューする。

- Queue の事前実測（Codex CLI 0.154.0、隔離 thread / 実 app-server）:
  実行中の追加、編集、取消、並べ替えを確認。正常完了後は先頭が自動開始する。
  app-server 再起動と thread/resume 後も待機入力は保存され、編集後の本文と client ID が
  自動開始された次ターンに届いた。再開直後の空の Queue は消失の証拠にならない。
  同じ clientUserMessageId で再度 add すると別 ID の項目が増えるため、受理不明時に add を再送してはいけない。
  実測用の使い捨てスクリプトは TDD 対象外。製品の VP Chat での Queue 実機検収は未完了。
  turn/interrupt 後は待機項目が残り、5 秒間の観測中は自動開始しなかった。
  thread/queue/start で先頭の明示再開に成功。停止状態のまま再起動・resume した場合も
  項目が残り、30 秒間の観測中は自動開始しなかった。

- 2026-09-13: 独立権限は Chat で項目・期間を選んで許可し、指定ファイルの書き込み・読み取り・削除を確認。
  辞退では空の権限が返り、会話が継続することも実機確認済み。
- MCP の平坦フォーム・URL 手続き・辞退・キャンセルを実装。null / 過去 turn の相関情報を扱い、
  typed content と native ID の JSONL 往復を検証した。任意の選択の取り消しと空文字の選択肢を区別し、
  URL のクリックが既存 IPC に届くことも RED → GREEN で確認。
  `mise run test` 成功（主要ライブラリ 1,112 成功・除外 11）、WebView 603 成功、
  check / Clippy / fmt / typecheck も成功。MCP の実機表示・操作は確認待ち。

- 2026-09-13: PR #1126 の承認修正は Chat で許可とターン中断・再開を確認し、
  全 CI 成功後に nightly へ統合。次の実装単位として独立権限を進める。
- 独立権限の受信・部分許可・期間指定・辞退と、未知形式の許可無効化を実装。
  未対応要求と未知フィールドのテストが失敗することを確認してから修正した。
  `mise run test` は成功（主要ライブラリ 1,104 成功・除外 11）、WebView は 601 成功。
  check / Clippy / typecheck / fmt も成功。実機の表示・操作は確認待ち。
  現在の CLI の `codex features list` では `request_permissions_tool` が under development / false。
  実機検収は専用セッションで機能を有効にして行い、普段の設定は変更しない。

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
