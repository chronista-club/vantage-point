# 83. lane からの detached swap

> **Status**: Draft — 本番の通し確認待ち
> **Related**: `mem_1Cg5ZMy8oPgcvmM1oq8X5W`
> **対象**: `.mise/tasks/app/swap-detached`, `.mise/tasks/app/swap`, `scripts/mise/swap_detached.rb`

lane の session は daemon の子なので、再起動を自分で最後まで実行できない。macOS の GUI domain に一回きりの launchd job を登録し、受付側は即 return する。

## 実行

```sh
mise run app:swap:detached
VP_SWAP_RESTART_DAEMON=1 mise run app:swap:detached
```

`app:swap:detached` は file task `app:swap-detached` の alias。plugin の guard が許可するのは上記二つの完全一致だけ。shell の連結・追記は許可しない。

受付時に `~/.local/state/vp/swap/active.lock` を mkdir で確保する。二重受付は拒否する。`XDG_STATE_HOME` があればログの基底に使う。日時と乱数で名前を付けた JSON・plist・log を残す。plist は RunAtLoad=true、KeepAlive=false、AbandonProcessGroup=true。終了結果を `.json.result` に保存し、lock を解放してから自身を bootout する。

launchd の環境を `env -i` で捨て、login shell から PATH を構成する。lane identity は環境として継承せず、JSON に wire の宛先だけを保持する。再起動フラグと `VP_PROFILE` は引き継ぎ、既存 swap に渡す。アプリの差し替え先と daemon の選び方は既存 swap に従う（profile を指定しても別の .app を作る機能ではない）。

実行側は既存 `mise run app:swap` を呼ぶ。build・署名・GUI 停止・差し替え後、再起動の直前に既存 CLI `vp repos list` で稼働 repo を列挙し、それぞれの cwd で `vp lane ls --detail` を読む。全 lane へ wire event を送り、配信要求が全件成功してから30秒待つ。列挙や配信が失敗したら再起動しない。CLI の表示形式が変わり稼働 repo を列挙できなくなった場合も失敗として止まる。

完了・失敗、対象 version / commit、ログの場所を起動元 lane へ送る。daemon / webview readiness の timeout は detached 経路では失敗にする。通知が失敗してもログと結果 JSON は残す。

## 復旧と境界

- 受付成功はジョブ完了ではない。log と `.result` または wire 通知で判断する。
- worker 起動前の shell エラー・強制終了で lock が残った場合、log と `launchctl print gui/$(id -u)/<ログに対応する label>` で実行中でないことを確認してから空の `active.lock` を `rmdir` する。自動で他の実行を奪わない。
- 既存の直接 `app:swap` はこの受付 lock を使わない。同時実行しない。
- 実行中は対象 worktree を変更・削除しない。通知の commit は開始時の source HEAD。
- 本番の daemon 再起動を含む通し確認は merge 後、lead が mako の合図で実施する。

## 検証

`ruby scripts/mise/tests/swap_detached_test.rb` で一回きりの設定・環境・lock・登録失敗・予告順序を検査する。実機の launchd では無害な dummy を登録し、即 return、ログ、lock 解放、PATH、Developer ID 署名、完了通知を確認する。実際の swap / daemon 再起動の証拠とは分ける。

## Status log

- 2026-10-17: detached 受付と既存 swap 接続を追加。dummy と配信を確認。本番 swap は未実行。
