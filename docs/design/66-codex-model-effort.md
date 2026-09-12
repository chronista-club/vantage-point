# 66. Codex Chat の model × effort 選択

> Status: Implemented — 実機検収待ち
> Task: mem_1CexxKRvy7R87G6RL6RzuX
> Related: design 41、65

Chat の次の送信に使う model と reasoning effort を session 単位で選ぶ。
Codex の機能に合わせた専用状態を持ち、既存 Claude の model 変更による host 再起動は流用しない。

## 情報源

Codex 0.154.0 の生成 schema と実 `model/list` を確認した。実測では 6 モデルが返り、
effort の範囲と既定値はモデルによって異なった。固定のモデル一覧は作らない。
[公式 App Server](https://learn.chatgpt.com/docs/app-server) の Models / Turns 節を参照。
`turn/start` の model / effort は、指定すると以後の thread の既定にもなる。

## 設計

- 各 host の `initialize` 後に `model/list` を取得する。cursor があれば続きを読み、
  全ページが揃った候補を公開する。取得失敗だけでは会話の再開を止めない。
  取得全体は15秒でタイムアウトし、空候補・取得失敗の後は Chat の再表示時に再取得する。
  再取得の状態変更・世代更新・採番は同一 lock 内で行い、旧タイマーを無効にする。
- `thread/start` / `thread/resume` 応答直下の model / reasoningEffort を保持する。
  paginated resume 後の `thread/read` に同じ値があるとは仮定しない。
- `SessionEntry.codex_selection` に model / effort のペアを保存する。未設定なら
  native の設定を尊重する。初版は明示ペアの選択を提供し、設定解除操作は含めない。
- host が ready、turn が idle、送信 queue が空の場合にだけ変更を受け付ける。
  同一 lock で候補検証・保存・次送信の設定を更新し、会話を再起動しない。
- 選択は次の `turn/start` にセットで渡す。保存された組合せが失効したら明示的に
  エラーを返し、別モデルへ黙って切り替えない。候補から選び直せる状態を保つ。
  保存ペアがある状態で候補が未確定なら、その送信も未実行として再送を案内する。
  設定による送信拒否は request 単位の送信結果へ返し、host 再生成や通常 Error にしない。
- UI は「次の送信」の選択を表示する。モデルを変える際は、そのモデルの既定 effort と
  セットで送る。選択前の native 設定と VP に保存した選択は別の状態とする。
  保存結果を待つ間は表示を確定値に戻し、拒否された値が選択済みとして残るのを防ぐ。
- 設定専用 `codex_config` イベントで候補・設定・request ID 付き結果を配送する。
  設定の拒否で会話の streaming や type-ahead を変更しない。
  request ID 付き応答は結果のみを扱い、設定状態の正本は host の snapshot とする。
  先行要求の遅れた応答が、別 client の後続設定を上書きしない。
- 履歴 snapshot が届いても最新の設定 snapshot を session ごとに保持する。
  再接続時は host が履歴と設定を再送する。

## 範囲と検証

対象は Chat。Console 自身のモデル picker は変更しない。Chat での選択は送信時に
native thread へ反映されるため、選ぶだけで Console の設定も更新したとは表示しない。
Plan、承認、画像、service tier、subagent は別段階。

候補のページング・不正応答、モデル別 effort、保存と送信の競合、busy 拒否、
再開時の設定保持、再接続時の候補表示を回帰テストで確認する。
実機での picker 操作と Console 往復後の設定確認は更新版で検収する。

## 実機検収

app と daemon の両方を更新して確認する。

- モデル変更で、そのモデルの既定 effort と候補に揃う。
- 応答中は変更できず、応答後の選択が次の Chat 送信に使われる。
- Console 往復・VP 再起動後も会話を継続でき、保存した選択が表示される。
