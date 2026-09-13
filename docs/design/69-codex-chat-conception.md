# 69. Codex Chat — 非同期の会話と入力の設計調査

> **Status**: 質問表示・回答配送・Chat / Console 往復時の保持を実機確認済み。通常入力と Queue は Conception
> **Related**: design 41、64、65、66、67、`mem_1CeySwxuoVc17bGLnU5Np3`
> **対象**: `conversation/codex_host.rs`, `conversation/codex_rpc_translate.rs`, `conversation/codex_history.rs`, `conversation/codex_interactions.rs`, `conversation/event.rs`, `webview/codex-interactions.tsx`

Codex が処理を続けながら質問し、人が途中で回答・指示を渡せる Chat を目指す。
今回の実機テストでは、選択式質問が本文の箇条書きとなり、後続の最終回答も連結された。
カードの見た目に加え、発話の単位・入力の配送・履歴の復元を一緒に設計する。

## 調査の根拠と限界

- 実行バイナリ: `codex-cli 0.154.0`。
- 同バイナリの `codex app-server generate-ts --experimental --out <一時ディレクトリ>` で型を生成。
- この実機テストの rollout と、別の一時 app-server からの read-only `thread/read` を照合。
  `thread/resume` や `turn/start` は実行せず、調査用プロセスは終了した。
- VP の対象コードを確認。GitNexus query は索引が HEAD の 1 commit 前と報告したが、
  当該 commit の差分は GUI window lifecycle と design 68 で、調査対象の conversation は変更なし。
- ローカル `/Users/makomac/repos/codex` は `3151954`（2026-07-16）。TUI の Queue / pending steer
  の参考にはなるが、現在の 0.154.0 と同一ソースとは確認できない。現行 Console の仕様と断言しない。
- [公式 app-server 文書](https://learn.chatgpt.com/docs/app-server) で `turn/steer` の
  active turn / expectedTurnId 条件を確認。非同期質問と Queue の細部は生成型・実測を優先する。
- Schema にメソッドがあることは、配送・永続化・競合処理の実機検証済みを意味しない。

## 発見: 質問は一つの protocol ではない

| 種類 | 観測した構造 | 応答・寿命の扱い |
|---|---|---|
| Server request の質問 | `item/tool/requestUserInput`、RPC id、質問ごとの id、`isBlocking` | 元 RPC id へ質問 id → answers の response。design 67 の対象。blocking は boolean で判定 |
| 非同期質問 | `agentMessage`、item id、`delivery: "async"`、`questions: [{title, options}]` | 専用 RPC response 用の質問 id はない。通常入力による回答を候補とするが、現行 Console の回答組み立ては未確認 |
| 実行・ファイル変更の承認 | `item/commandExecution/requestApproval` / `item/fileChange/requestApproval` | 元 RPC id に許可・拒否。未回答要求の寿命と履歴表示を区別 |
| 独立権限 / MCP elicitation | 生成 ServerRequest 型に存在 | design 67 の対象外。表示・応答・秘密入力の仕様を別途確認してから拡張 |

非同期質問の実測を識別情報を省いて示す:

```json
{
  "type": "agentMessage",
  "id": "<item-id>",
  "text": "テスト 1/2：…\n- 正常に表示されています\n- 表示が崩れています",
  "phase": "final_answer",
  "delivery": "async",
  "questions": [{
    "title": "テスト 1/2：…",
    "options": ["正常に表示されています", "表示が崩れています"]
  }]
}
```

`thread/read` は完了済み turn にもこの構造を返した。構造が履歴に残ることは確認済み。
ただし、未回答かどうかを示すフィールドはこの item にはない。残存を「未回答」と解釈しない。

## 今回の不具合の経路

修正前の `codex_rpc_translate.rs` は `item/agentMessage/delta` を `MessageChunk {text}` に変換し、
`item/completed` の agentMessage も delta 未受信なら text だけを fallback 出力していた。
`delivery` / `questions` を保持する経路がなく、発話 item id も `MessageChunk` にはなかった。
そのため質問は通常本文になり、後続の本文との境界も失われていた。

以前の「質問直後にターンを終了したからカードが消えた」という説明は、今回の原因説明として撤回。
Server request の失効ルールを、非同期 AgentMessage にそのまま適用してはいけない。

## 入力には三つの配送がある

| 配送 | 確認できた API / 実装 | VP の現在地 |
|---|---|---|
| 新しい仕事を始める | `turn/start` | idle で使用 |
| 今の仕事へ伝える | `turn/steer` + `expectedTurnId`、生成型に `clientUserMessageId` | 非同期質問への回答で使用。通常入力は引き続き host の VecDeque に入る |
| 次の仕事として待たせる | 生成型に `thread/queue/add,list,update,delete,reorder,start` | VP 独自 Queue を idle 時に turn/start で排出 |

Native Queue の生成型には submission id、input、clientUserMessageId がある。
変更通知 `thread/queue/changed` は threadId のみで、一覧取得はページング対応。
追加が自動実行を意味するか、再起動で残るか、Console と共有されるかは未検証。

TUI の旧ローカルソースでは queued_user_messages と pending_steers を別々に持ち、
committed user message と照合して pending steer を取り除く実装がある。
「送信ボタンを押した」「受理された」「実際に会話へ反映された」は異なる状態として扱う価値がある。

## Conception の提案

### 会話の表示

- Codex の発話は `(thread, turn, item)` 単位で保持する。delta と completed と履歴 snapshot を
  同一 item に反映し、連結や二重表示を防ぐ。
- 非同期質問は会話内のカードにする。質問ごとの選択肢と自由入力、明示的な送信操作を用意する。
  選択しただけでは送信しない。複数質問には質問文と回答を対応づけて送る。
- 回答待ちでも agent の進行を表示し続ける。未回答の候補へ戻れる小さな導線を入力欄付近に置く。
  過去の質問を復元しただけで未回答数へ加算しない。
- 承認は操作対象・許可範囲を明示した別カードで扱う。通常メッセージ送信で承認済みにしない。

### 回答と追加指示

- 非同期質問への「回答する」は、active なら steer、idle なら start を第一候補とする。
  応答を現在の仕事に間に合わせるのが目的。質問 item との関連は VP が保持する。
- 通常入力は「今伝える」と「次に実行」を区別できるようにする。既定動作はまだ裁定していない。
- Queue は一覧から編集・取消できる候補。native Queue の動作確認後、状態の所有者を決める。
  VP と Codex の二つの Queue に同じ入力を重複登録しない。
- 明示的な拒否と通信断による成否不明を区別する。steer と turn 完了が競合したとき、
  成否不明の入力を自動で start し直さない。client id と履歴で照合する方法を検証する。

### 段階的に作る候補

1. **発話と質問の保持**: item 境界・構造化質問・履歴復元・本文との重複防止。
2. **回答を届ける**: steer / start、送信状態、失敗時の入力保持、回答と質問の対応。
3. **Queue の操作**: native API の検証後に一覧・編集・取消と配送の既定を確定。
4. **承認とモード**: effective approval/sandbox の可視化、Plan、独立権限、MCP elicitation。

## 実装前に残す調査と検収条件

- 現行 Console で非同期質問への回答がどの user input / metadata になるかを観測する。
- 隔離した test thread で steer の成功・turn 競合・通信断を検証する。
- Native Queue の追加・編集・取消・順序・開始・再接続・再起動の挙動を検証する。
- 同一 item の delta/completed/replay、複数質問、ターンをまたぐ回答、通常入力で答えた場合、
  同文の別質問を UI と host の契約テストにする。自由文への回答済み推定を勝手に導入しない。
- 承認カードは質問カードと別に実機検収する。今回の `never` の由来も未解決であり、
  現行ソースが上書きしないことだけで実機の effective 設定を説明しない。

## Status log

- 2026-09-13: ユーザーの「まずは conception、その前に調査」に続く GO で調査を実施。
  生成型と read-only thread/read で非同期質問を確認。提案と未検証項目を記録。
  production code は変更していない。配送の既定・Queue の所有者・回答状態の復元は未確定。
- 2026-09-13: ユーザーが「質問を表示して回答が届く縦の流れ、steer を含む」の段階案に GO。
  裁定原文:「OK。一歩一歩進められれば、私は全然急いでないので大丈夫。強く美しい構造・プロダクトにするのが一番優先。」
  最初の隔離 thread 実測を下記に記録。質問カードの実装・検収完了は意味しない。
- 2026-09-13: 質問表示から steer / start による回答配送まで実装。
  `mbx test -p vantage-point --lib --test conversation_event_fixtures -- --test-threads=1` は
  ライブラリ 1,096 成功・除外 11、契約 fixture 3 成功、失敗 0。
  WebView の `bun run test` は 598 成功、`bun run typecheck` / `bun run build` と
  `cargo fmt --all -- --check` も成功。実カードの DOM 操作は自動検証済み、実機検収は未実施。
  `mbx clippy -p vantage-point -p vp-app --all-targets -- -D warnings` も成功。

## 実測: active steer と idle start

再現手順: `python3 scripts/probes/codex-steer.py`（Python 3 / 認証済み Codex が必要、モデル利用あり）。
スクリプトは一時 cwd と ephemeral thread を作り、read-only sandbox で文字列応答だけを要求する。
モデルはユーザー設定を継承。既存 thread の resume、VP daemon 再起動、設定ファイル変更は行わない。
ユーザーの Codex 設定・plugin hook は継承されるので、完全に無設定の環境ではない。
送受信と stderr と report は実行時に表示する一時ディレクトリへ保存する。

2026-09-13、Codex 0.154.0 / gpt-6-astra、実行終了コード 0:

| 観測 | 結果 |
|---|---|
| active turn と違う expectedTurnId | `-32600`、expected active turn id の不一致として拒否 |
| 正しい active turn への steer | 同じ turnId で受理。ランダムな STEER token を含む agentMessage が届いた |
| 入力の識別 | clientUserMessageId が userMessage.clientId として観測された |
| turn/completed を受けた後の steer | `-32600`、`no active turn to steer` として拒否 |
| idle で turn/start | 異なる turnId で開始。ランダムな NEXT token が出力された |

この結果で active / idle の基本配送と明示的な拒否を確認できた。
steer の受理だけで「回答が届いた」とせず、出力と client id を別々に観測した。
ただし、今回の token 出力検証は一般的な意味理解や全モデルの動作を保証するものではない。

残る検証: 完了と送信が同時に進む競合、受理前後の通信断、再接続後の入力照合、
非同期質問カードからの実回答、native Queue の動作。
この probe は正常応答・明示的拒否を確かめるためのもので、タイミング競合を強制する test ではない。

## 第一段階の実装契約

- `CodexMessage` は turn / item のキー、本文、構造化質問を持つ。delta は追記し、
  completed と履歴 snapshot は同一発話を置換する。後続の別発話とは連結しない。
- `codex_async_questions.rs` は live の未回答を管理する。server request の RPC 応答とは
  分離し、turn 完了後も回答先 host を保持する。過去の質問は読み取り専用で復元する。
- UI は発話位置に質問カードを表示し、選択・自由入力は session の下書きに保持する。
  選択だけでは送信せず、全質問の回答を確認して明示的に送る。未回答カードへの導線を置く。
  非 blocking 質問で実行中表示を「承認待ち」に変えない。
- 回答は質問文と回答を対応づけた通常の user input として送る。active なら現在の
  `expectedTurnId` を付けた `turn/steer`、idle なら設定を継承した `turn/start`。
  `clientUserMessageId` を採番し、実際の native userMessage を一度だけ本文に反映する。
- 回答の受理は JSON-RPC 成功応答で確定する。stdin の書込成功だけではカードを消さない。
  明示的拒否なら下書きを残して再操作可能にし、タイムアウト・切断・不正な応答では
  成否不明として再送を無効にする。自動で steer から start へ送り直さない。
- 切断したカードと下書きは現在の画面に残し、送信中表示を解除する。「回答を見送る」は
  ローカルの未回答管理から取り除く操作であり、Codex の作業を止める応答ではない。
- 同じ daemon / WebView 内の Chat / Console 往復では未回答と下書きを保持する。
  daemon / WebView 再起動を越える永続化、通常入力からの回答済み推定は未実装。

### 検収

自動検証は発話の境界・履歴、下書き保持、実カード DOM の選択と自由入力、実 JSONL
reader を通した active / idle × 受理 / 拒否 / 切断を対象とする。
Rust ↔ TypeScript のイベント fixture、既存テスト、型チェック、bundle と Clippy も確認する。

最初の自動検証時点で実機確認待ちだった項目（結果は後続の実機検収に記録）:

1. live の質問が発話位置に選択式カードとして現れ、後続の本文と分離している。
2. 実行中に回答し、入力が一度だけ会話に現れ、Codex が回答を受け取る。
3. turn 完了後にも質問へ戻り、回答から次の turn を開始できる。
4. Chat の往復で下書きを保持し、過去の質問を未回答として再発火しない。

最初の自動検証時点では、VP daemon の再起動・アプリへの配布を行っていなかった。

## 実機検収で見つかった寿命の不整合（2026-09-13）

上記の自動検証後、ユーザーが app:swap と daemon 再起動を実施。
質問カードの表示、明示的送信、turn 終了後の回答受信を確認した。
実行中の検証では assistant が sleep 中に回答を受信し、同じ turn の作業を継続できた。
この実機操作では RPC wire を別途採取していないため、配送メソッドの直接観測とは区別する。

Chat → Console → Chat 往復では、未送信のカードが「質問の履歴」になり入力欄が消えた。
ユーザー提供画像 `Vantage Point 2026-09-13 01.29.37.png` で確認。検収項目 4 は失敗。

修正前の原因のコード経路:

- mode 変更後、`reconcile_lane` が Chat 対象でなくなった session の engine を
  `drop_chat_engine_by_key` で除去する。
- `ChatEngineSlot::drop` → `CodexAgentHost::stop` が未回答の async 台帳を clear する。
- 復帰時の新 host は空の台帳を持つ。履歴の構造化質問だけでは live 未回答を作らないため、
  UI は読み取り専用カードを表示する。
- `foldCodexInteractions` は新 snapshot にない要求の下書きを削除する。
  したがって表示 component の再マウントだけを修正しても解決しない。

既存の下書きテストは同一 host での履歴 snapshot を再生しており、実際の mode 切替に
伴う host 終了・再生成を含んでいなかった。host 再生成を対象外としながら Chat / Console
往復を検収条件にした、実装契約と操作モデルの不整合である。

修正設計の条件:

- VP が live で受け取った非同期質問の未回答状態は、host の寿命から分離して会話に紐付ける。
  下書きの識別も host 世代に依存させない。
- Console 切替では状態を保持し、同一会話を Chat で再開できた後に回答先を再接続する。
  別会話・別 session へ移った場合には古い質問を配送しない。
- native の承認・server request の応答権限は引き継がない。履歴の質問をすべて未回答に
  する方式も採らない。送信途中の切替は成否不明を保持し、自動再送しない。
- 回帰テストは mode 切替 → host 破棄 → 再生成 → snapshot → 下書き・回答の復帰までを含む。

### 往復時の保持 — 実装（ユーザー GO、2026-09-13）

daemon 内の `LanePool` が VP session ごとの質問保管領域を所有する。host が終了するとき、
VP が live で受信した非同期質問だけを native thread ID とともに預ける。新 host は同じ
thread の再開・履歴取得に成功してから引き取る。再開前は同じ ID の操作不能なカードを
配信し、空 snapshot による UI の下書き削除を避ける。

送信途中の質問は成否不明として引き継ぎ、再送可能には戻さない。通常の server request
や承認は引き継がない。別 thread への切替、session 削除、lane 削除・リセットで保管状態を
取り除く。保管先はメモリのみで、daemon / WebView 再起動を越える永続化は今回の対象外。

`codex_question_session.rs` の `CodexQuestionSession` が保管領域、`codex_async_questions.rs`
が質問の状態遷移を担当する。`LanePool` は `(lane, session)` で保管領域を所有し、新 host
へ渡す。終了時の snapshot と起動準備中の snapshot は同じ質問 ID を保持し、同一 thread
の検証成功時に操作可能な台帳を引き取る。再開失敗では保管状態を消さず、再開準備中に
見送った質問は保管領域からも削除する。

回帰検証では `ChatEngineSlot::drop` による host 終了から、新しい host の履歴検証・
質問の復帰までを実行した。未送信 / 送信途中、native 承認の失効、別 thread / session の
隔離、再開失敗後の再試行、準備中の見送り、mode 切替と session/lane 削除・reset を確認。
UI は同じ ID の停止・再開 snapshot を受け、選択と自由入力を保持して回答へ使えることを確認。
修正後、ユーザーの「再開しました」を受けて新しい質問カードで往復テストを案内した。
往復後に保持されていれば送信する手順に対し、「選択肢 A を保持」と自由入力「Test123」の
カード回答を受信した（2026-09-13）。このユーザー操作結果を、往復保持と復帰後の回答送信の
実機検収結果として記録する。画面遷移自体の録画・RPC wire の追加採取は行っていない。

自動検証（2026-09-13、往復保持の修正後）:

- 回帰テストは修正前に、終了後の質問 ID が `null` になる失敗を確認した。
  修正後の `cargo test --offline -p vantage-point --lib question_ -- --test-threads=1` は
  11 成功・失敗 0・除外 1。
- 同じビルドのテストバイナリで `conversation::` を実行: 154 成功・失敗 0・除外 4。
  `repo::lane::pool::tests` を実行: 39 成功・失敗 0。
- WebView は `bun run test` 599 成功。`bun run typecheck` と
  `cargo fmt --all -- --check` も成功。
- `cargo clippy --offline -p vantage-point -p vp-app --all-targets -- -D warnings` も成功。

最終レビューでは、idle で最後の質問を見送っても pump の稼働フラグが残る経路を
回帰テストで再現した。見送り時に実際の turn 状態を含む履歴 snapshot を再配信し、
空の質問一覧だけでは解除できなかった休止抑止を解除する。実行中なら稼働状態を保つ。

この修正後、`async_question_dismissal_resynchronizes_turn_activity` は成功。
commit 前の `mise run test`、`mise run check`、
`mise exec -- mbx clippy --workspace --all-targets -- -D warnings`、
`cargo fmt --all -- --check` はすべて終了コード 0。
`mise run claims -- --base HEAD` の指摘を設計の時点・実装・検証結果に照合し、
GitNexus の差分解析で会話イベント・履歴・session 管理の変更範囲を確認した。
