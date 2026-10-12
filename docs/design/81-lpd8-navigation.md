# 81. LPD8 の Window・Lane・ピン操作

> **Status**: Implemented; UI・実機確認待ち（8 ピン化・Editor の MIDI タブを含む）
> **Related**: `mem_1Cfq8x3H5HHVsLDmvhYSKf`, `mem_1CfvxBJqJvVjZk5FEqZ98b`, design 49, design 78
> **対象**: `crates/vp-app/src/app/midi_target.rs`, `crates/vp-app/src/app/mod.rs`, `crates/vp-app/src/midi_pins.rs`, `crates/vp-app/webview/midi-navigation.ts`, `crates/vp-app/webview/midi-pins.ts`, `crates/vp-app/webview/src/sidebar/midi-navigation.ts`, `crates/vp-app/webview/editor/midi-settings.ts`

LPD8 の操作は、最後にアクティブになった VP Window を対象とする。他アプリにフォーカスが移っても対象を保持する。対象 Window を閉じた後は、別の VP Window がアクティブになるまで操作を止める。MIDI 入力では OS のフォーカスを奪わない。

## 入力と配送

各 Window の device 購読は接続状態の表示にも使われるため維持する。主イベントループで control_event だけを対象 Window の購読分に限定する。他の購読分を対象 Window に転送してはいけない（同じ操作が複数回実行される）。

WebView では cancelable な `vp:midi-navigation` を先に配送する。sidebar bundle が左上ノブを消費したら、gallery のペイン幅操作へ流さない。sidebar store は別 bundle のため、main bundle に直接 import して複製しない。

## 左上ノブ

CURRENTs に属する repo を sidebar の並び順でたどり、各 repo の開発起点 Lane → その他の Lane を収集する。折りたたみは対象数に影響しない。PAUSED と追加 session 行は数えない。選択は既存の `lane:select` IPC に接続する。

入力は 0〜127 を Lane 数で均等分割する。左端は先頭、右端は末尾。最大128 Lane まではすべて選択可能。それ以上では一部が選択できない。同じ Lane 区間での連続入力は送信しない。

## Pad とピン・色

ユーザー裁定: 「別のLaneだね。特定のこちらで#1~#4を好きにピンできる」→ 2026-10-12「この際ピン１～８にしよう。」。会話セッションや Lane/Runner 等の画面タブではなく、任意の Lane を 8 枠へ固定する。

物理 Pad 1〜8（内部 index 0〜7、Note 36〜43）がそのままピン #1〜#8 に対応する。押下だけで選択し、release まで再選択しない。未割当・不在の Lane は操作しないが、pad 入力は消費する（gallery へ流さない）。LPD8 の pad は gallery の Scene slot に写さない（`fleet.ts`）。Scene slot は nanoKONTROL の button に残る。knob の gallery mapping は従来どおり。

CURRENTs の見出し下に 8 枠を 4 × 2 で表示する。並びは LPD8 の物理配置と同じ向き: 上段 = pad 5〜8（ピン #5〜#8）、下段 = pad 1〜4（ピン #1〜#4）。空枠クリックで今選んでいる Lane を登録、登録済み枠クリックで選択（⌘N と同じ関数）、枠の hover で出る ✕ で解除。枠の右クリックでも登録・色の変更・解除ができる。Lane 行の右クリックからは「空いている最初の枠に登録」/「登録を解除」。同じ Lane を別枠へ登録すると元の枠は空く。sub lane も登録できる。

### 番号はピンが出どころ（2026-10-13）

**sidebar の `#N` = ⌘N = `⌘ hold l` の N = ピン #N = LPD8 pad N**。番号を決めるのはピンへの登録だけで、ピン #N に登録した lane の行（lead なら親行、sub なら sub の行）に `#N` が出る。登録していない行には番号が出ない。⌘N と pad N はピン #N の lane を開く（畳んだ repo は開いてから選ぶ）。未登録・登録した lane が今いない番号は何もしない（近くの lane に振り替えない）。

実装の規律: 番号 ⇄ lane の変換は `midi-pins.ts` の `pinNumberOf` / `pinTargetOf` だけが持つ。LaneRow の `#N` は `pinNumberOf`、⌘N / `⌘ hold l` / pad N は `handlers.ts` の `selectLaneByNumber`（中で `pinTargetOf`）を通す。pad の handler は番号を渡すだけで、宛先を自分で引かない。

経緯: 旧仕様は CURRENTs の並び順で root lane に `#1`〜`#9` を振り（`lane.ts` の `shortcutNumberOf`。⌘N と `⌘ hold l` も同じ番号、design 18）、ピンは別の番号だった。mako「サイドバーと#1−8とcmd+1~8の場所を、サイドバーの番号に合わせるのと、Laneの解除ができないね」→ いったん「並び順から自動で #1〜#8」を検討 → mako「今選択中のLaneを登録でOK。、みたいにしてしたい」で主従を逆にし、番号の出どころをピンにした。番号が 8 までになり、並び順では番号が付かなくなった（⌘9 は何もしない）。解除が見えなかった問題は枠の hover ✕ で解消（旧 Lane 行の右クリックは #1〜#4 にしか効かなかった）。

ピン（登録した lane）・色・未選択の明るさは **app 全体で 1 組**として state zone の `midi-pins.json`（`crate::midi_pins`）に保存する（`{ slots, colors, dimGain }`）。どの window で変えても（sidebar の右クリック / ピン枠 click / Editor の MIDI タブ）、開いている全 window の sidebar と Editor に即配る。pad を押した結果（lane 切替）が起きる先は従来どおり最後にアクティブにした window（`app/midi_target.rs`）で、window ごとの上書きは持たない。既定色は #1〜#4 が teal / blue / violet / amber、#5〜#8 が lime / coral / green / magenta。選択中は設定色、未選択は `dimGain`（既定 25%）を掛けた明るさ、不在・未割当は消灯。RGB は既存 feedback の optional color として送信する。ピンが有効な間、LPD8 の 8 pad の LED はすべてピンが持つ。フィードバックも対象 Window だけを受理し、非対象の watch をクリアする。

旧形式（4 ピン、`dimGain` なし）は読める: #1〜#4 をそのまま pad 1〜4 に載せ、#5〜#8 は空 + 既定色、`dimGain` は 25%。`dimGain` だけが壊れている（0〜1 の数でない）ときはピンと色を残して 25% に戻す。

### window ごとでなく app 全体にした理由（2026-10-13）

最初はウィンドウ別（`SessionState.midi_navigation`）に保存していた。mako「あとWindowごとに持ってる？この八箇所のデータは」「これは悩んでるんだよね。共通で行くか、WIndow毎に分けるかで。」→ 共通の推奨に「進めて」。理由は 3 つ: LPD8 は 1 台で pad に文字が無く、指は位置で覚えるので、window ごとだと前面の window によって同じ pad の意味が変わる / lane は window の持ち物でない（どの window からも同じ lane を開ける）/ LED は 1 組しかない。GUI の関心事なので daemon / `settings.kdl` には載せない（daemon 再起動を要求しない）。1 プロセス・複数 window（design 75）なので、値は in-process に 1 つ持ち、保存のたびに全 window の webview へ撃つ。file は window slot と無関係なので `session.*` と分けた。

移行: `midi-pins.json` が無ければ、旧 window file の `midi_navigation` から 1 組を採用して即書く（`session.json` = slot 0 を優先、無ければ番号の小さい順で最初の非 null。閉じた window の file も対象）。旧 field は読み込み専用（`skip_serializing`）で、以後 session file には書かない。**旧 field の定義は当面残す**: 移行は `midi-pins.json` が無い時にだけ走るので、消すと「新版を一度も起動していない環境」のピンが失われる。新版が 1 release 以上配られた後に、field と `legacy_midi_navigation_in` ごと消してよい（session file に残った旧 key は serde が無視する）。

[Akai公式のPad番号説明](https://support.akaipro.com/en/support/solutions/articles/69000867341-lpd8-mkii-how-to-program-pads-in-a-daw-or-plugin)に従い上段を5〜8としている。

### Editor の MIDI タブ（design 78）

色と未選択の明るさは Editor（Ctrl+Shift+E）の MIDI タブでも調整できる。各項目に「既定に戻す」がある。保存先は THEME の localStorage ではなく上の `midi-pins.json`（app 全体）。

経路は一方向: Editor（main bundle）→ IPC `midi:navigation` → Rust が app 全体の 1 組として保存し、**開いている全 window** の両 bundle へ `vp:midi-settings` で配り直す（`app::push_midi_settings`。起動時の `WebviewReady` と同じ script）→ sidebar の `midi-pin-store` とピン帯・LED が追随する。sidebar の右クリック「色の変更」も同じ IPC なので、Editor 側の表示も同じ配り直しで追随する。Editor は sidebar bundle の store を import しない。

- Editor は保存値（`__vpMidiSettings` / `vp:midi-settings`）が届くまで書かない。既定値で送ると保存済みのピンを上書きする。
- スライダーを動かしている間に古い値の配り直しが後から届いても巻き戻さない（送信中の値の列を持ち、最新の echo まで待つ）。

LPD8 mk2 の RGB LED 更新（model 0x4C）は Program 設定の書き換えとは別。

## mk2 の program と SysEx

初代 LPD8（model 0x75）と mk2（model 0x4C）は program 設定の SysEx が別物。`vp midi lpd8 write` / `switch` は identity request の reply で機種を見分けて経路を選ぶ（`midi::lpd8_identity`）。初代の経路（`midi::lpd8`）は残してある。

mk2 の形（2026-10-12 に実機で probe、`midi::lpd8_mk2`）:

| 用途 | SysEx |
|---|---|
| identity request | `F0 7E 7F 06 01 F7` → reply `F0 7E 00 06 02 47 4C …`（manufacturer 0x47 / model 0x4C） |
| 読み出し要求 | `F0 47 7F 4C 03 00 01 <prog 1-4> F7` |
| 読み出し応答（173 byte） | `F0 47 7F 4C 03 01 29 <prog> <body 164 byte> F7` |
| 書き込み | `F0 47 7F 4C 01 01 29 <prog> <body 164 byte> F7`（**応答は無い**） |

body 164 byte の配置:

| 位置 | 長さ | 中身 |
|---|---|---|
| head | 4 | 未解読（先頭は channel と思われる）。そのまま持ち回す |
| pad × 8 | 16 ずつ | note, cc, pc, ch（実機では 0x10）, on-RGB 6, off-RGB 6（R/G/B 各 2 byte = 上位 1 bit + 下位 7 bit） |
| knob × 8 | 4 ずつ | cc, ch（実機では 0x10）, min, max |

`vp midi lpd8 write`（mk2）の手順:

1. 対象 program を読み出し、`vp_state_dir()/backup/lpd8-mk2-program<N>-<日時>.txt` に保存する（保存できなければ書かない）
2. 書く body は機材の今の中身に依存させず、`BASE_PROGRAM` に VP の割り当て（pad i の note = 36 + i、knob i の cc = 70 + i）を重ねて作る。4 program のどれに書いても同じ body になる。既に一致していれば書かない
3. 書いた後に読み戻し、body 164 byte が一致したときだけ「書き込みました（読み戻しで確認）」と表示する。一致しなければ非 0 で終了する

対象は `--program 1-4`（既定 1）、`--all` で 4 つ全部。VP の配置は Program 1 に書くのを基本とする。

`BASE_PROGRAM` は 2026-10-12 時点で機材の Program 1 に入っていた中身（mako 裁定で土台に採用。工場出荷時の値は探さない）。

**Program の切替は本体操作**。mk2 の active program を SysEx で切り替える命令は未確認で、総当たりで探さない。`vp midi lpd8 switch` は mk2 では「PROG SELECT を押しながら pad <N>」と案内して終了コード 2 で終わる。初代の `switch` と `write` は確かめる手段が無いので「送信しました（確認はできません）」と表示する。

## 検証

Lane 順序・両端・1〜128件の全到達・連続入力抑制、番号 ⇄ ピンの変換と pad N = ⌘N の同一宛先、ピン帯の登録 / hover ✕ 解除、sidebar IPC 連携、Window 対象の切替・閉鎖、8 ピンの pad 対応と 4 × 2 の並び、旧 4 ピン session の読込み、`dimGain` の LED 反映、Editor → IPC の JSON・配り直しからの復元・遅れた echo の無視を自動検証する。実機の送信ノート／CC と、複数 Window の操作・Pad 色は別途実機確認する。

## Status log

- 2026-10-09: GO。Window 対象管理、Lane ノブ、4つの Lane ピンと色の保存を実装。自動テスト・型検査・bundle 検証済み。UI は Computer Use 接続不可のため目視未確認。アプリ差し替え・daemon 更新・実機検収前。
- 実機設定前提: 現行の入力 profile は Note 36〜43 / CC 70〜77 を認識する。Program 1 が実際に送る値は未観測。mk2 の Program EEPROM 書換えは未実装・未実施で、今回の色は VP 接続中の LED 投影である。

- 2026-10-11: 旧 PR #1190 の merge 後、LPD8 差分のみを `wip/lpd8-navigation` に分離し、nightly v0.84.0 へ rebase。アプリ差し替え・daemon 再起動はこの PR の作業に含めない。
- 2026-10-12: mk2 の program SysEx（0x4C）を実装（`wip/lpd8-mk2-program`）。identity で機種判定、write は `BASE_PROGRAM`（2026-10-12 時点の Program 1）に VP 割り当てを重ねて書き、読み戻しで確認。switch は mk2 では本体操作の案内。初代の嘘の成功表示を「送信しました（確認はできません）」に。実機への書き込みはこの lane では実施せず、merge 後に lead が行う。
- 2026-10-12: ピンを 8 枠に（pad 1〜8 = ピン #1〜#8、LPD8 pad の Scene slot mapping を撤去、sidebar は 4 × 2）。未選択の明るさを `dimGain` として保存し、Editor に MIDI タブ（8 色 + 明るさ）を追加。`midi:navigation` の保存後に Rust が `vp:midi-settings` を配り直す。`vp midi lpd8 write` の PAD 説明を「PAD 1-8 = Lane ピン #1〜#8」に。自動テスト済み、アプリ差し替え・実機確認待ち。
- 2026-10-13: ピン・色・`dimGain` を window ごとから app 全体の 1 組へ（`midi-pins.json`、旧 window 別の値から 1 度だけ移行、保存のたびに全 window へ配る）。番号の出どころをピンに一本化（sidebar `#N` / ⌘N / `⌘ hold l` / pad N、旧 CURRENTs 並び順の番号を撤去）、ピン帯に hover ✕ の解除、文言を「固定」→「登録」に。自動テスト済み、アプリ差し替え・複数 window の実機確認待ち。
