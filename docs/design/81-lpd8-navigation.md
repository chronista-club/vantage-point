# 81. LPD8 の Window・Lane・ピン操作

> **Status**: Implemented; UI・実機検収待ち
> **Related**: `mem_1Cfq8x3H5HHVsLDmvhYSKf`, design 49
> **対象**: `crates/vp-app/src/app/midi_target.rs`, `crates/vp-app/src/app/mod.rs`, `crates/vp-app/webview/midi-navigation.ts`, `crates/vp-app/webview/src/sidebar/midi-navigation.ts`

LPD8 の操作は、最後にアクティブになった VP Window を対象とする。他アプリにフォーカスが移っても対象を保持する。対象 Window を閉じた後は、別の VP Window がアクティブになるまで操作を止める。MIDI 入力では OS のフォーカスを奪わない。

## 入力と配送

各 Window の device 購読は接続状態の表示にも使われるため維持する。主イベントループで control_event だけを対象 Window の購読分に限定する。他の購読分を対象 Window に転送してはいけない（同じ操作が複数回実行される）。

WebView では cancelable な `vp:midi-navigation` を先に配送する。sidebar bundle が左上ノブを消費したら、gallery のペイン幅操作へ流さない。sidebar store は別 bundle のため、main bundle に直接 import して複製しない。

## 左上ノブ

CURRENTs に属する repo を sidebar の並び順でたどり、各 repo の開発起点 Lane → その他の Lane を収集する。折りたたみは対象数に影響しない。PAUSED と追加 session 行は数えない。選択は既存の `lane:select` IPC に接続する。

入力は 0〜127 を Lane 数で均等分割する。左端は先頭、右端は末尾。最大128 Lane まではすべて選択可能。それ以上では一部が選択できない。同じ Lane 区間での連続入力は送信しない。

## Pad とピン・色

ユーザー裁定: 「別のLaneだね。特定のこちらで#1~#4を好きにピンできる」。会話セッションや Lane/Runner 等の画面タブではなく、任意の Lane を4枠へ固定する。

上段の物理 Pad 5〜8（内部 index 4〜7）がピン #1〜#4 に対応する。押下だけで選択し、release まで再選択しない。未割当・不在の Lane は操作しない。下段の Pad は既存 gallery mapping を維持する。

CURRENTs の見出し下に4枠を表示する。空枠クリックで現在の Lane を固定、割当済み枠クリックで選択。枠の右クリックで現在 Lane への変更・色の変更・解除。Lane 行の右クリックからも固定できる。同じ Lane を別枠へ割り当てると元の枠は空く。

ピンと色は `SessionState.midi_navigation` にウィンドウ別で保存する。既定色は teal / blue / violet / amber。選択中は設定色、未選択は25%の明るさ、不在・未割当は消灯。RGB は既存 feedback の optional color として送信し、旧 payload の amber/off 動作を保持する。フィードバックも対象 Window だけを受理し、非対象の watch をクリアする。

[Akai公式のPad番号説明](https://support.akaipro.com/en/support/solutions/articles/69000867341-lpd8-mkii-how-to-program-pads-in-a-daw-or-plugin)に従い上段を5〜8としている。

LPD8 mk2 の RGB LED 更新（model 0x4C）は Program 設定の書き換えとは別。旧 `vp midi lpd8 write`（model 0x75）を mk2 の設定書き換えとして使用してはいけない。

## 検証

Lane 順序・両端・1〜128件の全到達・連続入力抑制、sidebar IPC 連携、Window 対象の切替・閉鎖を自動検証する。実機の送信ノート／CC と、複数 Window の操作・Pad 色は別途実機確認する。

## Status log

- 2026-10-09: GO。Window 対象管理、Lane ノブ、4つの Lane ピンと色の保存を実装。自動テスト・型検査・bundle 検証済み。UI は Computer Use 接続不可のため目視未確認。アプリ差し替え・daemon 更新・実機検収前。
- 実機設定前提: 現行の入力 profile は Note 36〜43 / CC 70〜77 を認識する。Program 1 が実際に送る値は未観測。mk2 の Program EEPROM 書換えは未実装・未実施で、今回の色は VP 接続中の LED 投影である。

- 2026-10-11: 旧 PR #1190 の merge 後、LPD8 差分のみを `wip/lpd8-navigation` に分離し、nightly v0.84.0 へ rebase。アプリ差し替え・daemon 再起動はこの PR の作業に含めない。
