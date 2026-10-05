# 75. 単一アプリ・複数ウィンドウ

> **Status**: Draft（実装・検証中）
> **Related**: design 27、design 30、design 60、`mem_1Cfd9HUeYqn4iKm5YXvgsj`
> **対象**: `crates/vp-app/src/app/{mod,boot,instance_guard,persist,on_window}.rs`, `event_proxy.rs`, `menu.rs`, `tray.rs`, `daemon/subscriptions.rs`

## 要件

同じ profile の GUI は1プロセス。ウィンドウを3枚開いても Dock / ⌘Tab / メニューバーのトレイは1つにする。
⌘N は同じアプリにウィンドウを追加し、各ウィンドウのlane選択・geometry・shell layoutは独立して保存する。
⌘W / 赤ボタンは対象ウィンドウだけを閉じる。⌘Q / Quit は全ウィンドウの状態を保存してアプリを終了する。
macOS は最後のウィンドウを閉じてもアプリを維持し、Dockの再表示・⌘N・トレイの New Window から再度開ける。
daemon と lane engine の寿命は GUI と独立であり、この移行では変更しない。

## 所有と配送

- `AppResources`: event loop と同じ寿命。runtime / log guard / menu / tray / daemon connection manager を1回生成。
- `Boot` + `UiState`: native WindowId ごとの資源と可変状態。WebView は親 Window より先に破棄する。
- `EventLoopProxy`: 生成元の WindowId を非同期応答・IPC・購読結果に付与する。メニューだけはアプリ全体宛てで、実行時のfocused windowへ配送する。
- 閉じたウィンドウのproxyは全cloneを無効化。生成ごとにscope番号を付け、OSがWindowIdを再利用しても古いqueueは新windowに届かない。遅れて届いたイベントは破棄し、別ウィンドウへ振り替えない。
- per-window subscriptionsはclose通知を監視し、通信が無い場合も終了する。確立済みUnison channelは明示的にcloseする。

## 保存と互換性

`session.json` / `session.<N>.json` を引き続き使用する。番号はプロセス番号ではなく永続ウィンドウslot。
明示closeは対象だけ `open=false`、アプリquitは開いているslotの `open=true` を保つ。起動時は保存済みopen slotを同じevent loopで復元する。
primary slotを明示closeしsecondaryだけで終了した場合、次回もsecondaryだけを復元。全slotが閉じていればslot 0を開く。
旧 `VP_APP_INSTANCE` による別プロセス起動は廃止。同じstate directoryの起動は既存 `app-instances/0.lock` で排他し、profile間は分離する。

## やってはいけない

- 新規ウィンドウのために `current_exe().spawn()` しない。
- 非同期応答を「現在focusedなwindow」へ配送しない。発生元と応答先は同じwindow。
- window closeでevent loop / runtime / shared daemon connectionを止めない。
- channel確立後のfutureを単にdropして購読終了とみなさない。recv taskとstreamをcloseする。

## 検証

アプリ排他、宛先固定、close後の遅延応答破棄、idle購読の終了、closeとquitの復元差分を自動検証する。
macOS実機では3windowのPIDとトレイ数、個別操作、⌘W後の残りwindow、⌘Q後の復元、0windowからの再表示を確認する。

## Status log

- 2026-10-02: ユーザーGO。ウィンドウごとの別プロセス方式から移行開始。
