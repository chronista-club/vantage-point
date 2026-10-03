# 64. Codex Console の会話継続

> **Status**: Draft
> **Related**: design 40、design 41、`mem_1CewV2A7phkmfC4rgi1NxC`（実装）、`mem_1CewUjDgufwJUsbtzY5dfG`（構想全文）
> **対象**: `crates/vantage-point/src/repo/agent_spawner.rs`, `crates/vantage-point/src/commands/wire.rs`, `crates/vantage-point/src/daemon/server.rs`, `crates/vantage-point/src/repo/lane/ops.rs`, `crates/vantage-point/src/lane/session_registry.rs`

## 体験と範囲

Console で始めた Codex を終了・再起動したとき、保存した thread を指定して同じ会話へ戻る。
Claude と Codex は各 engine の会話モデルに合わせて実装する。今回の対象は Codex TUI の ID 記録と再開。
VP Chat の履歴復元、GUI host の resume fallback、model/effort、subagent 表示は後続。

## 記録と再開

1. `codex_command` は選択済みの login shell に合わせ、Codex コマンドのみに `VP_HOOK_ENGINE=codex` を渡す。fish 3.1+ では command 単位の variable override、それ以外の POSIX 系 shell では subshell を使う。Console の親 shell には export せず、ユーザーの `codex` 関数・alias を利用する。
2. 有効・信頼済みの VP plugin の `SessionStart` が `vp wire hook-check` を呼ぶ。
3. hook は native JSON の `session_id` と、VP の `repo / lane / session key`、報告元 `engine` を daemon に報告する。
4. daemon は lane label を address に変換し、報告フィールドを欠落・補完させず repo へ中継する。
5. repo は Codex 専用の `record_codex_conversation_in` に渡す。明示された session が実在し、engine が Codex で、ID が有効な UUID の場合だけ保存する。宛先不明を root へ丸めない。
6. 次回の Console 起動は、その session の `conversation` を指定して `codex resume --no-daemon '<id>'` を実行する。ID がない場合は新規起動。

Claude の報告は既存の記録入口と F1/F2 guard を維持する。`engine` 不在は既存 Claude hook の互換経路。
Codex の報告に Claude transcript の有無を適用しない。report は engine と宛先の一致を検証し、別 engine の Console への保存を拒否する。

保存先は既存の session registry。新しいファイル形式・migration は作らない。mutation lock と atomic save は既存のものを利用する。

## 失敗と既存会話の復旧

resume の非ゼロ終了を `|| codex` で新規作成へ変換しない。Codex のエラーを Console に残し、元 ID を保って shell に戻る。
復旧時はその Console 内で次を実行する。環境変数は、手動で選んだ会話も VP に記録するための起動指定。以下は sh / bash / zsh の場合。

```sh
# 既に VP が記録し損ねた会話を、Codex 自身の一覧から選ぶ
(VP_HOOK_ENGINE=codex codex resume --no-daemon)

# 明示した会話を再試行する
(VP_HOOK_ENGINE=codex codex resume --no-daemon '<thread-id>')
```

`--last` や rollout の更新時刻で他の会話を自動選択しない。新規に進む操作は `(VP_HOOK_ENGINE=codex codex --no-daemon)`。
fish 3.1+ では外側の括弧を付けず、`VP_HOOK_ENGINE=codex codex resume --no-daemon`、`VP_HOOK_ENGINE=codex codex resume --no-daemon '<thread-id>'`、`VP_HOOK_ENGINE=codex codex --no-daemon` を使う。fish の variable override は関数・alias の解決と親環境を保つ（[公式仕様](https://fishshell.com/docs/current/language.html#overriding-variables-for-a-single-command)）。

VP session 自体の削除（名札の ×）は registry entry を削除する操作であり、TUI の終了や VP の再起動とは区別する。

## Console → Chat の writer 所有権

Console は新規・再開とも `--no-daemon` を指定する。共有 Codex daemon に writer を預けると、PTY 終了後も daemon 内の会話が残り、独立した Chat app-server の `thread/resume` と衝突するため。Console 自身のライフサイクルで writer を解放する。

この起動には `--no-daemon` 対応の Codex CLI が必要（0.160.0 の新規・resume 両方の help で確認）。未対応 CLI の非ゼロ終了を、フラグなし起動や新規会話で隠さない。CLI を更新して同じ ID を再開する。

更新前の Console が既に共有 daemon に会話を残した場合、起動引数の変更だけではその writer を奪取しない。Chat の衝突メッセージは、同じ会話を開いている Codex 側で処理を完了または中断し、`/quit` で終了した後に再試行するよう案内する。更新前の共有 daemon に戻す場合は Console の shell で `VP_HOOK_ENGINE=codex codex resume '<thread-id>'`（`--no-daemon` なし）を使う。専用起動を繰り返しても旧 writer は解放されない。元 ID を保持し、入力は自動再送しない。共有 daemon の強制終了・lock 削除・履歴 DB の編集は行わない。

### 応答完了後の切替

Codex Console → Chat の確認は「今すぐ切り替える」「応答完了後に切り替える」「キャンセル」の3択。完了待ちは現在の window / lane / session に結び、取消・window 終了で予約を捨てる。確認中に session の会話 ID や mode が変わった場合も切替要求を送らない。

起動時に `tui.notifications=["agent-turn-complete"]`、`tui.notification_method="osc9"`、`tui.notification_condition="always"` を指定し、Codex の端末完了通知を受ける（[公式仕様](https://learn.chatgpt.com/docs/config-file/config-advanced#notifications)）。ユーザーの hook / notify 設定や trust は変更しない。Stop hook は別 hook によって処理が継続し得るため、完了扱いしない。

terminal pump は replay と新しい出力を `live` で区別し、WebView の待機処理は新しい出力の OSC 9 だけを受け取る。通知は chunk 境界を跨いでも読む。別 lane / session や replay の通知では切り替えない。旧 sender の `live` 欠落は false とする。既に応答が終了している場合は「今すぐ切り替える」を選ぶ。通知のない旧 Console や失敗した応答を、時間経過だけで完了と推定しない。待機はユーザーが取り消せる。

## Codex 0.154.0 での実測と前提

- `hooks/list` で、VP plugin 0.24.0 の SessionStart hook が有効・信頼済みであることを確認した。
- 実 TUI の最初の発話で `SessionStart`（`source: startup`）を採取し、`session_id` と終了時に表示された `codex resume` の ID が一致した。
- その TUI を終了して ID 指定で再開し、履歴表示・前の返答の再回答を確認。再開側の hook（`source: resume`）も同じ ID を報告した。VP への送信先は検証用スタブで、VP アプリの往復検収とは別の観測。
- この検証の hook 環境には `CODEX_THREAD_ID` がなかった。これを ID の供給源にしない。
- hook の実行前に終了した未発話の TUI は、VP がまだ ID を持たないことがある。hook 信頼は Codex 側の `/hooks` でユーザーが管理する。VP は自動承認・trust bypass を行わない。
- fork / subagent と通常の Console 再開を同一視しない。今回の実測は通常の TUI 会話を対象とする。
- fish 4.9.3 からも実 Codex TUI を指名 resume し、履歴・前の返答・正常終了を確認した。native SessionStart の環境に `VP_HOOK_ENGINE=codex` と `VP_SESSION_KEY=2` が届いた。VP 呼び出し先は引き続き検証用スタブで、実 daemon と更新アプリの往復検収は別途必要。

公式仕様: [Hooks](https://learn.chatgpt.com/docs/hooks)、[App Server](https://learn.chatgpt.com/docs/app-server)。

## 検証

- 再開コマンドの失敗が失敗のまま残ること（実 shell で検証）。
- native hook の報告元と明示 session が配送を通って保持されること。
- 二つ目の Console の報告が自身に保存され、次回の起動コマンドへ引き継がれること。
- 不明な session / 別 engine / 不正 ID の報告で registry が変わらないこと。
- Claude の既存報告・再開保護と Console 起動の回帰。
- 実アプリを終了・再起動して同じ会話へ戻る検収は、自動テストと別に行う。

## やってはいけない

- `IgnoredNonClaude` を取り去って Codex の報告を Claude の記録 policy に混ぜる。
- 配送途中で不正な engine / session フィールドを落とし、Claude / root の互換経路へ変換する。
- resume 失敗時に元 ID を上書きする。
- `hooks/list` の enabled/trusted を hook 発火・配送成功の証拠とする。
- marker を単なる `VAR=value function` で限定したつもりになる（POSIX shell の関数では親へ残り得る）。
- fish に POSIX subshell の `(command)` を注入する（fish では command substitution になり、起動できない）。
- rollout JSONL の本文を読み解いて ID を発見する。

## Status log

- 2026-09-11: Console を先行する構想に GO。通常 TUI の hook ID を実測し、専用の保存入口と失敗時の ID 保持を実装。自動検証は通過、更新版 VP アプリの再起動は実機検収待ち。

- 2026-10-04: Console を `--no-daemon` で専有起動し、writer 衝突時の復旧案内と切替確認・完了待ちを追加。Codex 0.160.0 と隔離したローカル model fixture で OSC 9、および idle / 応答中の PTY 終了後の同一 thread 再開を実測。実 VP アプリの往復検収は別途行う。
