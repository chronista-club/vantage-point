# 64. Codex Console の会話継続

> **Status**: Draft
> **Related**: design 40、design 41、`mem_1CewV2A7phkmfC4rgi1NxC`（実装）、`mem_1CewUjDgufwJUsbtzY5dfG`（構想全文）
> **対象**: `crates/vantage-point/src/repo/agent_spawner.rs`, `crates/vantage-point/src/commands/wire.rs`, `crates/vantage-point/src/daemon/server.rs`, `crates/vantage-point/src/repo/lane/ops.rs`, `crates/vantage-point/src/lane/session_registry.rs`

## 体験と範囲

Console で始めた Codex を終了・再起動したとき、保存した thread を指定して同じ会話へ戻る。
Claude と Codex は各 engine の会話モデルに合わせて実装する。今回の対象は Codex TUI の ID 記録と再開。
VP Chat の履歴復元、GUI host の resume fallback、model/effort、subagent 表示は後続。

## 記録と再開

1. `codex_command` は subshell 内の Codex コマンドのみに `VP_HOOK_ENGINE=codex` を渡す。Console の親 shell には export しない。ユーザーが `codex` を shell 関数にしていても marker を親へ残さない。
2. 有効・信頼済みの VP plugin の `SessionStart` が `vp wire hook-check` を呼ぶ。
3. hook は native JSON の `session_id` と、VP の `repo / lane / session key`、報告元 `engine` を daemon に報告する。
4. daemon は lane label を address に変換し、報告フィールドを欠落・補完させず repo へ中継する。
5. repo は Codex 専用の `record_codex_conversation_in` に渡す。明示された session が実在し、engine が Codex で、ID が有効な UUID の場合だけ保存する。宛先不明を root へ丸めない。
6. 次回の Console 起動は、その session の `conversation` を指定して `codex resume '<id>'` を実行する。ID がない場合は新規起動。

Claude の報告は既存の記録入口と F1/F2 guard を維持する。`engine` 不在は既存 Claude hook の互換経路。
Codex の報告に Claude transcript の有無を適用しない。report は engine と宛先の一致を検証し、別 engine の Console への保存を拒否する。

保存先は既存の session registry。新しいファイル形式・migration は作らない。mutation lock と atomic save は既存のものを利用する。

## 失敗と既存会話の復旧

resume の非ゼロ終了を `|| codex` で新規作成へ変換しない。Codex のエラーを Console に残し、元 ID を保って shell に戻る。
復旧時はその Console 内で次を実行する。環境変数は、手動で選んだ会話も VP に記録するための起動指定。

```sh
# 既に VP が記録し損ねた会話を、Codex 自身の一覧から選ぶ
(VP_HOOK_ENGINE=codex codex resume)

# 明示した会話を再試行する
(VP_HOOK_ENGINE=codex codex resume '<thread-id>')
```

`--last` や rollout の更新時刻で他の会話を自動選択しない。新規に進む操作は `(VP_HOOK_ENGINE=codex codex)`。
VP session 自体の削除（名札の ×）は registry entry を削除する操作であり、TUI の終了や VP の再起動とは区別する。

## Codex 0.154.0 での実測と前提

- `hooks/list` で、VP plugin 0.24.0 の SessionStart hook が有効・信頼済みであることを確認した。
- 実 TUI の最初の発話で `SessionStart`（`source: startup`）を採取し、`session_id` と終了時に表示された `codex resume` の ID が一致した。
- その TUI を終了して ID 指定で再開し、履歴表示・前の返答の再回答を確認。再開側の hook（`source: resume`）も同じ ID を報告した。VP への送信先は検証用スタブで、VP アプリの往復検収とは別の観測。
- この検証の hook 環境には `CODEX_THREAD_ID` がなかった。これを ID の供給源にしない。
- hook の実行前に終了した未発話の TUI は、VP がまだ ID を持たないことがある。hook 信頼は Codex 側の `/hooks` でユーザーが管理する。VP は自動承認・trust bypass を行わない。
- fork / subagent と通常の Console 再開を同一視しない。今回の実測は通常の TUI 会話を対象とする。

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
- rollout JSONL の本文を読み解いて ID を発見する。

## Status log

- 2026-09-11: Console を先行する構想に GO。通常 TUI の hook ID を実測し、専用の保存入口と失敗時の ID 保持を実装。自動検証は通過、更新版 VP アプリの再起動は実機検収待ち。
