# repo-proxy から session を作成・削除し、表示を切り替える

`repo-proxy` は、最初の request に `repo_path` を入れると `subscribe` なしで利用できる。
対象 repo は daemon 内で起動済みであること。protocol v0.5.0 の KDL 公開面は
`lanes_list`、`session_set_mode`、`conversation_session_create`、
`conversation_session_remove`。既存 client の `subscribe {repo_path}` 方式も利用できる。

## chat session の作成・削除

まず `lanes_list {repo_path}` で対象 lane と `sessions.root` / `sessions.focused` を確認する。
以下の `repo_path` は repo の絶対 path、`lane` は `address.repo/address.name`。

- 作成: `conversation_session_create {repo_path, lane, agent: "agy", focus: false}`。
  成功応答は `{status: "ok", lane, session}`。新しい session は chat（gui）で、作成操作そのものは
  engine を起動しない。発言時に起動するが、GUI が開いていると pane の attach や注視で
  発言前に起動することがある。console の session を作る口ではない。
- `agent` は任意で、省略すると lane の agent を使う。指定可能なのは
  `claude` / `codex` / `grok` / `opencode` / `agy` / `vpcode`。lane の agent が
  `shell` など未対応なら省略時もエラーになるので、対応する agent を明示する。
- `focus` は任意で既定 true。true は新 session に lane の注視を移し、false は作成操作では元の
  注視を保つ。ただし GUI 側の操作で後から注視が移ることがある。
- 削除: `conversation_session_remove {repo_path, lane, session}`。作成応答の `session` を使う。
  session key は 1〜4294967295 の整数。成功応答は `{status: "ok", lane, session, focused}`。
  稼働中の engine / terminal も閉じる。`focused` は削除後の注視先 key で、注視中の session を
  閉じると残りの先頭へ移る。root session の削除と lane 不在は Error frame で断られる（理由は payload の `error`）。
- 既に無い session の削除は `{status: "absent", lane, session, focused: null}` で冪等に成功する。
  この null は lane に注視先が無いという意味ではない。現在の注視先は `lanes_list` で読む。

## merge 後の実機確認

この変更を含む daemon を更新・再起動した後、lead が実施する。lane の自動検証では
常駐 daemon の更新や実 session の作成・削除・切替は行っていない。

1. MCP 登録 `unison-vp`（`unison-mcp --config ~/.config/unison/vp.json`）を再接続して
   discovery を取り直し、repo-proxy に上記 4 method の型付き tool が現れることを確認する。
2. `lanes_list {repo_path: "対象 repo の絶対 path"}` を呼び、応答の `lanes[]` から
   対象の `address` と `sessions.sessions[]` を読む。lane は `address.repo/address.name`、
   session は `key`。`mode` と `chat_capable` も確認し、chat に切り替えられる検証用 session を選ぶ。
3. `session_set_mode {repo_path, lane, session, mode: "gui"}` を呼ぶ。
   応答の `status: "ok"` と同じ lane / session / mode を確認する。
4. `lanes_list` を再度呼び、同じ session の mode が `gui` になり、GUI に chat pane が出ることを確認する。
   `mode: "tui"` でも往復し、console pane と一覧の mode が一致することを確認する。
   検証後は元の mode に戻す。
5. 既存 vp-app / CLI の明示 subscribe 経路で一覧・操作が引き続き動くことも確認する。

作成・削除は次の順で確認する。作成単体の確認には、GUI が attach していない検証用 lane を使う。

1. 元の `sessions.root` と `sessions.focused` を控え、型付き tool で
   `conversation_session_create {repo_path, lane, agent: "agy", focus: false}` を呼ぶ。
   `status: "ok"` と返された session key を控える。
2. GUI から開く前に `lanes_list` でその key の `agent: "agy"` / `mode: "gui"` と
   注視先が変わらないことを確認し、作成操作自体では engine が起動していないことを daemon log で確かめる。
   次に GUI で開き、chat pane が現れることを確認する。この attach や注視操作では、発言前でも
   engine が起動することがある。注視先が変わった場合も作成時の `focus: false` と区別し、log の順序を記録する。
3. 型付き tool で、作成した key を `conversation_session_remove` に渡す。
   `status: "ok"` / 削除後の `focused`、一覧と GUI からその session が消えたことを確認する。
   同じ key を再度削除し、`status: "absent"` / `focused: null` を確認する。
4. chat 対応 agent の lane で `agent` と `focus` を省略して作成し、lane の agent を引き継ぎ、
   新 session が注視されることを確認する。GUI attach または発言に応じた engine の起動を確かめ、
   作成した session を削除する。元の注視先と違えば GUI で元に戻す。
5. 控えた root key が現在も root であることを一覧で読み直し、その削除が Error frame で断られて
   root が残ることを確認する。実機確認は lead が管理する検証用 lane / session で行う。

CLI の単発呼び出しでも同じ入口を確認できる（`unison call --help` で確認済みの形）。
以下は read-only の例。path は対象 repo に置き換える。

```bash
unison call 'quic://[::1]:32000' --channel repo-proxy --method lanes_list \
  --payload '{"repo_path":"/absolute/path/to/repo"}' --timeout 5000
```

`repo_path` を省略した場合は、無言で待たずに Error frame で断る。typed MCP tool は必須引数を
先に検証することがあるため、daemon 側のエラーは次の直接呼び出しで確認する。

```bash
unison call 'quic://[::1]:32000' --channel repo-proxy --method lanes_list \
  --payload '{}' --timeout 5000
```

空文字・文字列以外も同じエラーになる。未起動 repo は `repo 未起動` で断る。
repo の handler が Err で返した失敗（未起動 repo、引数の誤り、未知の method など）は Error frame で
返る（payload は `{error: "理由"}`。protocol v0.5.0 から。それより前の daemon は成功応答に `error` を
詰めていた）。ただし `layout_get` / `layout_set` / `layout_history` は vp-app の webview の評価結果を
そのまま成功応答として返すので、評価の timeout 等では成功応答の中に `error` が入ることがある。
永続 channel では最初の repo_path に宛先が固定される。別 repo を操作する場合は
新しい channel を開く。
