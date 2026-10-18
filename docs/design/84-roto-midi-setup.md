# 84. ROTO MIDI モード連携 — pin から「VP」setup を作る

Status: VP 側を実装。実機書き込み・反映は未検証。
Task: `mem_1Cg9QiQeVsk7KD3fcwhXqh`、段 C。

## 契約

Serial ポートは midistage だけが開く。VP は MIDI モードの入力を受け、
midistage の lease を持つときだけ setup **6** を登録・適用する。
setup 名は `VP`。番号は Ladyland の 0–3 および掃除対象 4–5 と重ならない。

| 本体 | 表示 | MIDI | 色 |
|---|---|---|---|
| switch 1–7 | pin #1–#7 の lane 名 | `VP_ROTO_SETUP` の channel / buttons / press / release、PUSH | 保存した pin 色の最近傍パレット、消灯は暗くする |
| switch 8 | `MIC` | 同じ契約の button 8、PUSH | 点灯は赤、消灯は暗赤 |
| knobs 全席 | なし | 未設定（CLEAR） | なし |
| switch 9–32 | なし | 未設定（CLEAR） | なし |

CC・チャンネル・押下/解放値の正本は `roto_midi::VP_ROTO_SETUP`。
入力 parser と setup 生成が同じ定数を参照する。
表示名は印刷可能な ASCII の先頭 12 字。非 ASCII / 制御文字のある名前は slug、
slug も使えなければ `#N`。空 pin は `#N` と暗色にする。
現在の保存 pin は表示名を持たないため、本番では address 末尾の lane slug を使う。
元の色は sidebar / LPD8 と共有する `midi-pins.json` の色を使い、
`midistage_profiles::roto_palette::closest_index` で 83 色へ写す。

当初の「名前を表示するが何も送らない knob」は相手 schema に表現が無い。
2026-10-18 の lead 回答（wire `01a14e33-c1ee-7469-ade0-3b5d16f6555c`）により
今回は `knobs=[]` とし、ノブの役割・表示は次の task で決める。

## 適用と失敗

1. 起動時を含め、ROTO の active lease が自分の session に属するか確認する。
2. `SetDeviceSetups([6])` で登録する。
3. 生成内容の hash が保存済みと違うときだけ `ApplyDeviceSetup` を要求する。
   要求前に古い成功 hash を永続 cache から消す。消せなければ要求しない。
   部分的に失敗した後で元の pin に戻したときも、再適用を省略しないため。
4. 受付の `configuring` と、その解除・`config_error`・`current_setup` を観測する。
   受け付けただけでは成功 hash を保存しない。
5. pin の変更は最新の内容にまとめ、2 秒間変更が止まってから適用する。
   lease を持たない間の変更は次に取得したときに反映する。

RPC は registry lock の外で進め、lease 失効を受けたときの既存の
入力停止・join・Quiesced の順序を妨げない。
同じ lease / 内容で失敗した要求を polling のたびに再送しない。
`not_implemented` / サービス不在 / 他の設定処理中でも VP の他の機能を止めない。

### pin 保存の取り込みと cache

既存の pin broadcast は GUI の window 内だけに配る仕組み。
daemon は同じ state zone の `midi-pins.json` を lease の snapshot polling に合わせて読み、
内容が変わったときにだけ再生成する。IPC の契約を増やさず、GUI の再接続や
daemon 起動時にも同じ保存値から復元できる。読み取りは GUI の上限と同じ 16 KiB に制限する。
読めない / 壊れた保存値は warn を出して最後の正常値を保持し、未読なら適用しない。
保存ファイルが無いときは空 pin と MIC で始める。

cache は state zone の `roto-setup-hashes.json`（version / setup / device ID → hash）。
一時ファイルから rename する。成功 hash は `configuring` の解除を確認してから保存する。
RPC 応答の sequence 以下の snapshot は完了判定に使わず、lease が変わった RPC の結果も捨てる。
要求は機材ごとに最大 1 本、受付を待つ上限は 4 秒。書き込み自体の完了は snapshot で追う。

## 制約と実機確認

- SDK は midistage `03793029fbc726c1b2431ea66862c46cdb398424` に固定。
  この版の Apply は `not_implemented` を返すため、現時点では実機の内容を更新できない。
  対応版への更新と実機確認は別途必要。
- 本体 SEL での手動 setup 切替は midistage に見えない。
- Serial を開いて書き込む間は ROTO の MIDI が止まる。
- ローカル hash は VP が最後に成功を確認した内容。外部ツールでの書き換えを検出するものではない。
- この lane では実機 lease 取得・解放、Serial 書き込み、daemon 再起動、app swap を行わない。
- midistage の実機検証と mako の GO の後、`vp midi on` → snapshot の `setup_owners.vp=[6]` /
  `current_setup=6` → ボタン 1–7 の jump / 8 の押下中録音、表示名・色の対応を確認する。
  MIDI 復帰と挿し直しの要否も記録する。

## 自動検証

生成、CC 正本、ラベル・色、hash、2 秒 debounce、lease 喪失、失敗時の再送抑制、
成功 cache の永続化・無効化を `roto_setup` の unit test で確認する。
固定 SDK の `protocol_fixture` をビルドすると、架空の ROTO と一時 state directory だけで
登録・自動選択・Apply の未実装エラーを検証できる。

```sh
MIDISTAGE_TEST_SERVER=/path/to/0379302/protocol_fixture \
  cargo test -p vantage-point --lib roto_setup -- --include-ignored
```

fixture の endpoint だけを読み、`native_midi=false` で接続する。
実サービスや実機の成功をこの結果から推定しない。
