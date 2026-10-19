# 84. ROTO MIDI モード連携 — pin から「VP」setup を作る

Status: VP 側を実装。実機書き込み・反映は未検証。
Task: `mem_1Cg9QiQeVsk7KD3fcwhXqh`（setup / lease、段 C）、`mem_1CgAPGP3R4Gf4UNFBCWpTY`（T8 通常化、段 B）。
裁定: `mem_1CgAPJPpcCwy2s1QohP3Zj`、2026-10-19 のノブ未割当裁定。

## 契約

Serial ポートは midistage だけが開く。VP は MIDI モードの入力を受け、
midistage の lease を持つときだけ setup **内部 index 8** を登録・適用する。
ROTO の画面表示は index + 1 なので、**SETUP 09** に対応する。
setup 名は `VP`。番号の正本は `roto_setup::SETUP`。

| 本体 | 表示 | MIDI | 色 |
|---|---|---|---|
| switch 1–8 | pin #1–#8 の lane slug | `VP_ROTO_SETUP` の channel / buttons / press / release、PUSH | 保存した pin 色の最近傍パレット、消灯は暗くする |
| knobs 全席 | なし | 未設定（CLEAR） | なし |
| switch 9–32 | なし | 未設定（CLEAR） | なし |

CC・チャンネル・押下/解放値の正本は `roto_midi::VP_ROTO_SETUP`。
入力 parser と setup 生成が同じ定数を参照する。
表示名は印刷可能な ASCII の先頭 12 字。非 ASCII / 制御文字のある名前は slug、
slug も使えなければ `#N`。空 pin は `#N` と暗色にする。
現在の保存 pin は表示名を持たないため、本番では address 末尾の lane slug を使う。
元の色は sidebar / LPD8 と共有する `midi-pins.json` の色を使い、
`midistage_profiles::roto_palette::closest_index` で 83 色へ写す。

knob は未割当（裁定 2026-10-19）。`knobs=[]` とし、全席を CLEAR のままにする。

## T1〜T8 の入力

switch 1–8 は LPD8 / ⌘1–8 と同じ `selectLaneByNumber` を通して pin #1–#8 の lane へ飛ぶ。
未登録・不在の lane は何もしない。押下1回で1回だけ jump し、release は次の押下に備える。
T8 の MIC / PTT は廃止し、ノブ・touch・transport は操作に割り当てない。
ROTO 専用の音声入口を撤去し、キーボードの右 Option 用に録音 registry と開始条件を保持する。
window 終了時の停止処理はキーボード側で保持する。

## 適用と失敗

1. 起動時を含め、ROTO の active lease が自分の session に属するか確認する。
2. `SetDeviceSetups([SETUP])`（現在は `[8]`）で登録する。
3. 生成内容の hash が保存済みと違うときだけ `ApplyDeviceSetup` を要求する。
   要求前に古い成功 hash を永続 cache から消す。消せなければ要求しない。
   部分的に失敗した後で元の pin に戻したときも、再適用を省略しないため。
4. 受付の `configuring=true` の後、同じ lease の新しい snapshot で
   `configuring=false` かつ `config_error` なしを確認して成功 hash を保存する。
   Apply は選択中の setup を変えないため、`current_setup` は完了条件に使わない。
5. pin の変更は最新の内容にまとめ、2 秒間変更が止まってから適用する。
   lease を持たない間の変更は次に取得したときに反映する。

RPC は registry lock の外で進め、lease 失効を受けたときの既存の
入力停止・join・Quiesced の順序を妨げない。
同じ lease / 内容で失敗した Apply は polling のたびに再送しない。
Register の失敗（RPC エラー・timeout・task 終了エラー・登録結果の不一致）は
完了観測時点から 1・2・4・8・16・30 秒、以降は 30 秒間隔で再試行する。
pin 編集でこの待ち時間を短縮せず、lease を失えば中止し、新しい lease では初期化する。
登録成功まで Apply は送らない。
`not_implemented` / サービス不在 / 他の設定処理中でも VP の他の機能を止めない。

### pin 保存の取り込みと cache

既存の pin broadcast は GUI の window 内だけに配る仕組み。
daemon は同じ state zone の `midi-pins.json` を lease の snapshot polling に合わせて読み、
内容が変わったときにだけ再生成する。IPC の契約を増やさず、GUI の再接続や
daemon 起動時にも同じ保存値から復元できる。読み取りは GUI の上限と同じ 16 KiB に制限する。
読めない / 壊れた保存値は warn を出して最後の正常値を保持し、未読なら適用しない。
保存ファイルが無いときは全8列を空 pin（`#N` / 暗色）として始める。

cache は state zone の `roto-setup-hashes.json`（version / setup / device ID → hash）。
setup が `SETUP` と一致する cache だけを読み、hash にも setup 番号を含める。
旧 index 6（画面 SETUP 07）の cache は再利用せず、実機の旧席の後始末は midistage 側で行う。
一時ファイルから rename する。成功 hash は `configuring` の解除を確認してから保存する。
RPC 応答の sequence 以下の snapshot は完了判定に使わず、lease が変わった RPC の結果も捨てる。
要求は機材ごとに最大 1 本、受付を待つ上限は 4 秒。書き込み自体の完了は snapshot で追う。
cache を捨てて再適用する場合は、daemon 停止中にこの state zone の
`roto-setup-hashes.json` を削除する。次回起動時に空の cache として読み込む。

## 制約と実機確認

- SDK は Apply 実装を含む midistage `9bb2536230d85495139ac95354e4a72340422c21` に固定。
  実機書き込みには、稼働中の midistaged も Apply 対応版である必要がある。
- 本体 SEL での手動 setup 切替は midistage に見えない。
- Serial を開いて書き込む間は ROTO の MIDI が止まる。T8 の名前・色の変更で hash が変わるため、
  merge 後の初回 lease で 1 回 Apply する。以後は pin の変更時に反映する。
- ローカル hash は VP が最後に成功を確認した内容。外部ツールでの書き換えを検出するものではない。
- この lane では実機 lease 取得・解放、Serial 書き込み、daemon 再起動、app swap を行わない。
- midistage の実機検証と mako の GO の後、`vp midi on` → snapshot の `setup_owners.vp=[8]` /
  `current_setup=8`（画面 SETUP 09）→ ボタン 1–8 の lane jump、表示名・色の対応を確認する。
  MIDI 復帰と挿し直しの要否も記録する。

## 自動検証

生成、CC 正本、ラベル・色、hash、2 秒 debounce、lease 喪失、失敗時の再送抑制、
成功 cache の永続化・無効化を `roto_setup` の unit test で確認する。
固定 SDK の `protocol_fixture` をビルドすると、架空の ROTO と一時 state directory だけで
登録・自動選択・Apply 成功後の hash 保存と同じ内容の再送抑制を検証できる。
`not_implemented` の失敗抑制は `unsupported_apply_is_attempted_once_and_never_cached`
の fake transport unit test で引き続き確認する。

```sh
MIDISTAGE_TEST_SERVER=/path/to/9bb2536/protocol_fixture \
  cargo test -p vantage-point --lib roto_setup -- --include-ignored
```

fixture の endpoint だけを読み、`native_midi=false` で接続する。
実サービスや実機の成功をこの結果から推定しない。

## Status log

- 2026-10-19: T8 を pin #8 に揃え、ROTO の MIC / PTT を廃止。ノブは未割当のまま。
  Apply・ボタン入力・表示の実機確認は lead で行う。
