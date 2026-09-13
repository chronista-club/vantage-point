# 68. GUI の終了待ちと重複起動の修正

> Status: Draft
> Task: mem_1CeyfXU9GYUp6tkrN6uzyY

## 原因

`app:swap` 後、ウィンドウを閉じると操作不能な画面とプロセスが残った。
旧 binary の PID は終了しており、残留したのは差し替え後の GUI だった。
macOS の sample は `EventLoop::run` の閉包破棄から `Boot._rt` の破棄へ進み、
Tokio の blocking pool の終了待ちで止まっていた。

メニュー転送が `spawn_blocking` 内で muda の global receiver を無期限に
`recv()` していた。ウィンドウ終了後は次のメニュー通知が来ず、runtime の破棄も終わらない。
通常の Quit とウィンドウの閉じるボタンは異なる終了経路なので、両者を同一視しない。

## 修正

muda 0.19 の `MenuEvent::set_event_handler` に、`AppEvent::MenuClicked` を
event loop へ転送する callback を各 GUI process の boot で一度登録する。
メニュー待ちの blocking task を作らない。daemon と会話プロセスの寿命は変更しない。

親だけを閉じて再起動した場合、存命の子ウィンドウを保存情報から再度復元する問題もあった。
`run` の boot 前に、state directory 内の `app-instances/<index>.lock` を OS の
非待機・排他 file lock で確保する。同じ番号の所有者が居るなら、画面作成や session 保存をせず終了する。
異なる番号と profile は独立。lock はプロセスの終了で解放される。lock file 自体は削除しない
（unlink して inode を変えると、既存所有者と新規所有者が同時に存在し得るため）。

## 検証

macOS の実機 probe は `scripts/probes/app-window-close.swift`。
起動済み GUI の PID を渡し、Accessibility の閉じるボタンを押して
5 秒以内にプロセスが終了することを確認する。実行には Accessibility 権限が必要。

```sh
swift -module-cache-path /private/tmp/vp-quit-swift-cache scripts/probes/app-window-close.swift <GUI_PID>
```

メニュー転送は File → New Window で追加の GUI が起動することを確認し、
その追加ウィンドウも probe で閉じる。通常の Quit は native 側の処理なので、
メニュー転送の検証には代用しない。

重複防止は `window_instance_*` で同番号の拒否、別番号・profile の独立、解放後の再取得を確認する。
実機では同じ primary の追加起動と、子を残して primary を閉じた後の再起動で、同番号のウィンドウが増えないことを確認する。

## Status log

- 2026-09-12: 実機 sample で終了待ちを特定。通常 Quit は成功、閉じるボタンの
  probe は旧実装で失敗（PID 35260 が 5 秒後も残留）。callback への変更を実装。
- 2026-09-12: callback 版を app:swap で反映し、閉じるボタンの実機 probe が成功
  （PID 76438）。native New Window から追加起動した PID 83401 も probe で終了した。
  親の再起動で instance 1 が重複することを確認し、OS lock を追加。重複防止の 2 テストは
  lock 導入前に失敗し、導入後に成功した。
- 2026-09-12: lock 版も app:swap で反映。追加の起動要求を 2 回行っても
  primary / secondary の PID は 19000 / 19006 のままだった。primary 19000 は
  close probe で終了し、再起動後は 41062 / 19006 の 2 process のみ。
  存命の secondary を重複復元しないことを実機確認した。
- 2026-09-12: check / workspace Clippy / fmt が成功。全体テスト再実行は
  1,414 成功・失敗 0。先行実行では端末監視の
  `reconcile_touches_only_the_swapped_slot_leaving_siblings_alone` が root 出力混入で失敗し、
  単独実行と他の build に重ねない全体再実行では成功した。間欠的な失敗の原因は未確定。
