# 機材ごとの MIDI 使用権

VP の常駐 MIDI I/O は midistage の protocol / profiles / Rust SDK を利用する。
物理ポートの所有は midistaged が行い、VP は現在の lease 専用の仮想ポートだけを開く。
設定・所有権・送信の正本は midistage repository の `schemas/midistage.kdl` と `docs/design/03-runtime-service.md`。

## 使用操作

Devices 面で「MIDI を有効にする」を押し、対応機材の「使用」を切り替える。
通常表示は Midistage の機材単位の一覧に統一する。OS の生ポート名と IN/OUT は「接続ポート詳細」に畳み、そこに旧直接接続の使用状態を重ねない。
別アプリの担当なら切り替え確認が出る。確認した revision を送り、確認後の所有者変更を上書きしない。
現在の VP の操作対象は ROTO / LPD8 / X-Touch / nanoKONTROL2。nanoKONTROL2 は標準 CC モード（ch1）の入力を扱い、ノブ・フェーダーは pane 比率、S ボタンは Scene 呼び出し／長押し保存に対応する。M/R・transport ボタンと nano の LED フィードバックは未対応。対応していない操作面は一覧に残して切り替えを無効にする。
CLI は同じ daemon-control の窓口を使う。

```sh
vp midi devices
vp midi on
vp midi use roto on --revision 3 --takeover
vp midi use roto off --revision 4
vp midi off
```

revision は `vp midi devices` の最新応答から取得する。`--takeover` は他アプリからの変更を明示的に承認するときだけ付ける。
全体 OFF は保存済みの機材割り当てを保持し、`vp-monitor` として状態だけを読む。全体 ON で `vp` の担当を再開する。
サービスが接続できないときは I/O を停止し、物理ポートへフォールバックしない。

## 解放順序

入力許可表を先に更新し、旧 listener の abort と join、ROTO セッションの停止・join、旧 output の破棄を終えてから Quiesced を返す。
別機材の listener とアプリ固有の lane / scene 割り当ては保持する。
service は物理送信 completion と後始末が終わるまで次の lease を与えない。

`devices/midi-use` の状態応答は `connected` / `enabled` / `snapshot` と、サービス状態を示す nullable な `error` を持つ。この `error` は RPC 失敗ではない。GUI はこの method の状態応答に限ってデータとして受け取り、通常の `{"error": ...}` だけの RPC 失敗（revision 不一致など）はエラーとして扱う。

新窓口 `devices/midi-use` は既存 `devices/midi` と同様、人の GUI/CLI 操作用であり MCP discovery には公開しない。
既存の前景実機診断コマンド（`vp midi roto demo` など）はこの常駐経路の移行対象外。共有サービス稼働中に直接実行しない。

## 検証境界

入力許可と解放順序のテスト、GUI の takeover 確認・revision 保持・全体 OFF・定期更新のテストを行う。
ブラウザでは機材を開かない fixture で表示と操作を観察する。これを物理 MIDI や稼働中 VP の実証とは扱わない。
共通 service 起動・Ladyland 反映・共有 VP daemon 更新後の音/LED/画面/往復切り替えは実機確認待ち。

### 2026-10-07 実装時の記録

- Rust の対象テスト31件（device registry 20、CLI/handler 2、所有ポート制限2、daemon KDL 7）を確認。
- WebView の対象テスト11件、TypeScript 型検査、WebView bundle、`vp-app` のビルドを確認。
- `vp-cli` のビルドと `vp midi use --help`、`cargo clippy -p vantage-point -p vp-app --all-targets -- -D warnings` を確認。
- ブラウザ fixture で takeover 確認と全体 OFF の設定保持表示を観察。定期更新で操作中の DOM を置き換えないこともテストで固定。
- midistage SDK は `0d3c3f5e22dac62c0e701daaa17fcd173f41df18` に固定。

稼働環境への反映には `CLAUDE.md` の server 更新手順が必要。`VP_SWAP_RESTART_DAEMON=1` は共有 daemon と各 repo/lane のセッションに影響するため、通常のビルド検証と分けて実施する。

## daemon が仮想ポートを見られる条件（2026-10-14）

CoreMIDI は process ごとに endpoint 一覧の cache を持ち、更新の通知を
**その process で最初に `MIDIClientCreate` した時点の current run loop** にだけ届ける。
daemon は tokio の worker thread で midir を呼ぶので run loop が回っておらず、
一覧は最初の client を作った瞬間で止まる。

midistaged は lease を受けるたびに仮想ポートを作り直す（名前に lease token が入る）。
そのため仮想ポートは毎回 daemon の起動**後**に現れ、止まった一覧には載らない。
#1185（2026-10-08 JST merge）で daemon が開く先を物理ポートから仮想ポートだけに絞ってから、
LPD8 の input listener も ROTO も開けていなかった（daemon log では 10/03 が最後の
`input listener started` / `ROTO connected`、10/08 04:55Z から毎回 `接続失敗`）。

対処: `coremidi_notify::start()` が daemon の MIDI 初期化の先頭（`MachineCapabilities::with_devices`）で
専用 thread を立て、そこで process 最初の client を作って `CFRunLoopRun()` を回し続ける。
daemon の MIDI 入口（列挙 / input listener / feedback output / ROTO）は未起動なら 1 度 warn する。
input listener は、通知が届くまでの数百 ms を吸う短い開き直し（100ms から倍々、上限 2s、5s で断念）を持つ。

実測（midir 0.11 だけの小さな binary。別 process が `create_virtual` した仮想 source を 500ms ごとに列挙）:

| 条件 | 結果 |
|---|---|
| 先に main 以外の thread で client を作り、run loop は回さない（daemon と同じ条件） | 10 秒たっても見えない |
| 仮想ポートができた後で初めて client を作る / 別 process の `vp midi ports` | すぐ見える |
| process 最初の client を専用 thread で作り、その thread で `CFRunLoopRun()` | 約 600ms で見える |
| 別 thread で先に client を作った後に、run loop 付きの thread を立てる | 見えない（通知先は最初の client で決まる） |

`vp midi …` CLI は今回の対象外（daemon 経路だけ）。

## Status log

### 2026-10-16 — 抜線時の listener 解放と接続待ち

対象: `crates/vantage-point/src/devices.rs`。起票: `mem_1CfzdHyAdTrQu89kb7BAtm`。

- discovery と agent の抜線通知は物理名を受け取る。listener map の key は lease 専用の仮想名なので、`MidiAccess::input` で解決し、abort と join が済んでから抜線を配信する。仮想名の文字列形式を呼び元で解釈しない。
- listener map のロック内で仮想名を解決する。snapshot 更新も同じロック内で許可表を更新し、旧 listener を join する。ロック待ちの前に解決した古い名前を挿入したり、旧接続の解放前に新接続を登録したりしない。
- task が生きていても port を開けるまでは `held=false / connecting`（接続待ち）。開けたら `held=true / listener`、失敗・終了後は `held=false / idle`。接続結果を既存の device delta で配信し、sidebar の取得台数に反映する。Devices の主表示は従来どおり Midistage の lease 状態を使う。
- 自動テストでは物理名による停止、接続の drop、再接続後の本数、lease 更新との競合、接続待ちから成功への状態配信を確認する。実機の抜き差し・二重入力の観測は daemon 反映後に別途行う。

修正前は抜線による map 削除が空振りし、lease 更新まで旧 task が残る。通常の snapshot 更新には旧 task の abort/join があるが、join 中に別経路の ensure が新接続を登録できるため、一時的な接続の重なりをコード上は排除できなかった。物理 MIDI が実際に二重配信されたという観測はない。

### 2026-10-18 — daemon 終了時の Midistage session close

対象: `midi_use.rs`、`daemon/machine_capabilities.rs`、`repo/server.rs`、`daemon/process.rs`。
起票: `mem_1Cg8P1aoBWatWQaaVTMXo5`。

- SIGTERM / SIGINT / HTTP shutdown は同じ cancellation token に合流する。daemon は MIDI 接続 task の所有権を保持し、HTTP の drain・常駐 task・repo の終了待ちより先に MIDI の終了処理を開始する。
- 接続 task の再接続を止め、task の終了を待つ → `release_fleet` で取得 gate を閉じて input を abort/join、output を drop → client の `close()` を await、の順序にする。通常の fleet OFF は session を監視用に切り替えるので、daemon 終了専用の処理を使う。
- Snapshot の応答待ちは cancellation で中断する。Hello の途中は安易に abort しない。成功した session が registry に載る前でも、task の戻り値で client を回収して close する。接続 task の回収上限は 12 秒、超過時は abort/join と警告。回収不能な接続の remote close は保証しない。
- 固定依存 `midistage-client` revision `0d3c3f5` の実装は、transport 接続 5 秒・channel open 3 秒・Hello の応答待ち 3 秒の上限を持つ。Hello の送信待ちや接続失敗時の cleanup を含む `Client::connect()` 全体の上限ではない。VP の 12 秒は独立した回収期限であり、SDK が 11 秒以内に完了する保証には依存しない。`Client::close()` は channel close 後に transport disconnect を呼ぶ。後者は固定依存 `club-unison 2.0.0` で QUIC `CONNECTION_CLOSE` を要求する。VP 側は close 全体に 2 秒の上限を置き、成功時は `midistage client closed`、失敗・timeout 時は warning を記録する。成功ログは close API の完了を示し、remote 側の受信確認ではない。
- `vp daemon stop` の Unix 猶予は 500ms から最大 15 秒へ変更する（接続 task 12 秒 + close 2 秒 + 余裕）。PID が終了状態になれば早く戻り、上限では従来どおり強制終了する。Windows の HTTP graceful stop も同じ 15 秒を使う。従来の 500ms は既存の常駐 task の 2 秒・repo task の 8 秒の終了予算より短かった。repo 全体の後始末完了を 15 秒で保証する変更ではない。
- 生成する LaunchAgent plist に `ExitTimeOut` の指定はない。この Mac の `launchd.plist(5)` は既定値を system-defined とするため、固定 20 秒とは扱わない。VP と無関係の一時 LaunchAgent で `kickstart -k` の SIGTERM 到達を確認した。実際の VP daemon の swap はこの lane では行わない。

自動検証の対象は、loopback の実 QUIC client/server による disconnect、local release が先であること、Snapshot 応答待ち・Hello 途中・未接続での停止、task/close の timeout、隔離した子プロセスの graceful stop。物理 MIDI は使わない。

実機確認は lead の次回 swap で行う。旧 daemon の `Daemon graceful shutdown initiated` → `midistage client closed` → `Daemon stopped` を確認し、新 daemon の接続時刻と midistaged 側の Hello/session 記録を突き合わせる。`application already has a live session` の拒否が続かないこと、lpd8 / roto の lease を取り直すまでの時刻差を測る。timeout warning が出た場合は成功扱いにしない。Windows の実行検証と実機での再接続時間は未確認。
