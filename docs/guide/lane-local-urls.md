# Lane のローカル URL

Lane に開発サーバーなどへのリンクを保存できます。サーバーの停止や Agent の終了で
登録は消えません。新しく作った Lane にはコピーされません。

## CLI

登録済み repo または Lane のディレクトリで実行します。

```bash
vp lane url set preview http://localhost:5173 --label 'UI プレビュー'
vp lane url list
vp lane url probe preview
vp lane url rm preview
```

`set` は同名の登録を更新します。`--label` を省略すると既存の用途を保持し、
新規登録なら名前を用途にします。名前は小文字英数字で始まる英数字・`-`・`_`、64文字以内です。

別の Lane を指定する場合は、各コマンドに `--lane demo` または
`--lane vantage-point/demo` を付けます。root Lane は `--lane root` で指定できます。
既定の対象は実行ディレクトリから決まり、継承した `VP_REPO` / `VP_LANE` は使いません。

結果は JSON です。登録一覧の `id` が名前に対応します。以前 UI で登録した URL は
既存の UUID を名前としてそのまま扱えます。削除済みの名前への `rm` も成功します。

## MCP

ツール名は `lane_url` です。CLI と同じ保存先を使います。

```json
{"action":"set","name":"preview","url":"http://localhost:5173","label":"UI プレビュー","lane":"vantage-point/demo"}
```

```json
{"action":"list","lane":"vantage-point/demo"}
```

```json
{"action":"probe","name":"preview","lane":"vantage-point/demo"}
```

```json
{"action":"rm","name":"preview","lane":"vantage-point/demo"}
```

`lane` を省略すると MCP サーバーの実行ディレクトリから対象を決めます。
CLI / MCP の登録操作には GUI や daemon の起動は不要です。

## サイドバー

Lane の右クリックメニューから「ローカルURLを追加」を選び、名前・URL・用途を登録します。
登録がない Lane には URL 欄を表示しません。CLI / MCP からの変更は約3秒ごと、または
ウィンドウへのフォーカス時に読み直します。編集中は入力を維持し、保存時に外部変更が
見つかった場合は競合を表示します。再読み込み後に登録内容を確認してください。

接続確認は手動です。「確認」または `probe` を使います。HTTP 応答があったことと
アプリケーションの正常動作は別で、503 や 302 も応答として表示します。
リダイレクトやプロキシは使いません。

登録できる URL は認証情報を含まない loopback の HTTP(S) に限ります。
保存先は同じマシンの VP state ディレクトリ内 `lane-local-urls.json` です。
