# repo-proxy から session の表示を切り替える

`repo-proxy` は、最初の request に `repo_path` を入れると `subscribe` なしで利用できる。
対象 repo は daemon 内で起動済みであること。KDL の公開面は `lanes_list` と
`session_set_mode`。既存 client の `subscribe {repo_path}` 方式も利用できる。

## merge 後の実機確認

この変更を含む daemon を更新・再起動した後、lead が実施する。lane の自動検証では
常駐 daemon の更新や実 session の切替は行っていない。

1. MCP 登録 `unison-vp`（`unison-mcp --config ~/.config/unison/vp.json`）を再接続して
   discovery を取り直し、repo-proxy の `lanes_list` / `session_set_mode` が現れることを確認する。
2. `lanes_list {repo_path: "対象 repo の絶対 path"}` を呼び、応答の `lanes[]` から
   対象の `address` と `sessions.sessions[]` を読む。lane は `address.repo/address.name`、
   session は `key`。`mode` と `chat_capable` も確認し、chat に切り替えられる検証用 session を選ぶ。
3. `session_set_mode {repo_path, lane, session, mode: "gui"}` を呼ぶ。
   応答の `status: "ok"` と同じ lane / session / mode を確認する。
4. `lanes_list` を再度呼び、同じ session の mode が `gui` になり、GUI に chat pane が出ることを確認する。
   `mode: "tui"` でも往復し、console pane と一覧の mode が一致することを確認する。
   検証後は元の mode に戻す。
5. 既存 vp-app / CLI の明示 subscribe 経路で一覧・操作が引き続き動くことも確認する。

CLI の単発呼び出しでも同じ入口を確認できる（`unison call --help` で確認済みの形）。
以下は read-only の例。path は対象 repo に置き換える。

```bash
unison call 'quic://[::1]:32000' --channel repo-proxy --method lanes_list \
  --payload '{"repo_path":"/absolute/path/to/repo"}' --timeout 5000
```

`repo_path` を省略した場合は、無言で待たずに `error` を返す。typed MCP tool は必須引数を
先に検証することがあるため、daemon 側のエラーは次の直接呼び出しで確認する。

```bash
unison call 'quic://[::1]:32000' --channel repo-proxy --method lanes_list \
  --payload '{}' --timeout 5000
```

空文字・文字列以外も同じエラーになる。未起動 repo は `repo 未起動` を返す。
成功応答と、response payload 内の `error` を区別すること。
永続 channel では最初の repo_path に宛先が固定される。別 repo を操作する場合は
新しい channel を開く。
