# 81. Devices の 3D 展開と情報エリア

> **Status**: Draft — WKWebView 実機確認待ち
> **Related**: `mem_1CfsA67B5KTNTQgChBcFbM`、[機材ごとの MIDI 使用権](midi-device-handoff.md)
> **対象**: `crates/vp-app/webview/midi-use.ts`、`midi-device-layout.ts`、`midi-device-scene.ts`、`midi-device-view.ts`

## 配置と範囲

Devices を上部・中央・下部の 3 エリアに分け、各エリアの表示内容を決める。
今回の割り当ては上部が空、中央が 3D の機材展開、下部が使用設定・状態と既存の IN/OUT 一覧。
空の上部にプレースホルダーや余白を足さない。既存 `#device-list` を下部へ移し、描画入口は維持する。

3D は機材を識別して選ぶ面。使用権の変更は下部の既存チェックボックスと明示的な引継ぎ確認で行う。
クリックで選んでも MIDI コマンドを送らない。機材名ボタンはキーボードでも選択できる。
drag で orbit、wheel で zoom。「表示を整える」で現在の表示対象を俯瞰 45° に戻す。
下部先頭の「全体＋機材名」タブで中央の表示対象と下部の使用設定・状態を切り替える。
個別タブではその機材だけを原点に配置してカメラを fit し、「全体」で機材群と既存 IN/OUT 一覧に戻る。
左右矢印・Home・End でも切替可能。選択機材が snapshot から消えた場合は全体へ戻る。
ライブ MIDI 値と物理ポート操作は範囲外。外部モデルは下記の Midistage gear 読み込みで扱う。

## データと形

状態の正本は `MidiUseStatus`。別経路の `DeviceSnapshot` は従来の IN/OUT 情報一覧だけに使う。
表示用 profile は寸法比と control の種別・位置・安定 ID を保持し、Three.js や I/O に依存しない。
ID は機材内で一意（例 `strip.0.fader`、`pad.0`）。将来の値反映は `device_id + control ID` に結び付けられる。

資産の読み込み前・不在・検証失敗時は、X-Touch、LPD8 mk2、nanoKONTROL2、ROTO-CONTROL を primitive で模式的に組み、他は箱と名前にする。
X-Touch の 8 strip、LPD8 の 8 pad、ROTO の 8 slot は `midistage-profiles/src/device_profile.rs` の protocol 実装に対応する。
同ファイルは座標データを持たないため、実寸 CAD の再現とは主張しない。
nanoKONTROL2 は #1190 の標準 CC (ch1) 操作対応を引き継ぐ。LED 表示は未対応。

| 表示 | 条件 |
| --- | --- |
| 通電色 | 接続中・present・エラーなし・対応 profile・全体 ON・active・owner が VP・lease が現在の session |
| 半透明 | 接続している対応機材で上記以外（全体 OFF、他アプリ割当、切替中を含む） |
| 灰色 | サービス切断、機材未接続、機材エラー |
| 彩度を落とした不透明な GLB（簡略模型では薄色） | 接続中だが VP の MIDI 操作割当が未対応 |

選択はこれらの状態とは独立した輪郭と一覧のハイライトで表す。
状態更新で model を再生成しない。機材の ID・profile・順序が変わった場合のみ作り直し、全体を収め直す。

## 描画と寿命

3D view と canvas は設定フォームの再描画から分離する。定期 status 更新や引継ぎ確認でカメラを失わない。
静的 scene は常時 loop せず、操作・選択・状態・サイズの変化時に RAF を一度だけ予約する。
IntersectionObserver、ResizeObserver、document visibility で表示を判定し、非表示で予約を取り消す。
VP は全面サイズを保って `visibility:hidden` にするため、祖先要素の style/class/hidden も監視する。
モデルのラベル等の子孫 style は監視しない（描画による再通知ループを避ける）。
描画直前にも pane の接続・寸法・可視性を再確認する。
OrbitControls の damping は無効。非表示時に惰性 animation を残さない。

pane の除去は既存 polling timer が検知して view を dispose する。再 mount 時にも前の view を破棄する。
observer、event listener、controls、geometry、material、renderer を解除する。
WebGL を初期化できない場合や context loss 中も、下部の一覧と使用設定は維持する。

## 検証

- profile の control 数・ID・筐体内配置、状態の優先順位、camera の全体 fitting、素材の更新と資源解放を GPU なしでテスト。
- frame queue の非表示 cancel、再表示時の描画、dispose、on-demand をテスト。
- 3 エリア、canvas 容器の保持、選択が MIDI 操作を起こさないことと既存 takeover を DOM テスト。
- fixture ブラウザ観察と実 VP / WKWebView の確認は別の証拠。lane 内での swap は `app:swap:detached` を使える。全 lane を落とす本番の通し確認は lead が mako の合図で行う。

## Midistage gear の実行時読み込み

関連 task: `mem_1Cg1no4SEaGkCYaF3VtCDk`。
macOS では `~/Library/Application Support/Midistage/gear/` の配布物を読む。
資産の制作・更新は Midistage 側が所有し、VP repo に GLB / JSON のコピーは入れない。
`manifest.json` の schema 1 と、選択機種の GLB / JSON の SHA-256・bytes を使用する。
`profile_id` は基本的に同名 ID に対応し、互換表記 `nanokontrol2` は `nanokontrol` に寄せる。

`vp-asset://app/gear/manifest.json` と `vp-asset://app/gear/<id>.<sha256>.<glb|json>` を
アプリの custom protocol で配信する。Rust は ID の文字種・長さと拡張子を限定し、
canonical path が gear dir 内であること、manifest の schema、要求された hash、サイズと
実ファイルの SHA-256 を確認する。manifest も dir 外の symlink は拒否する。
読み込みは上限付き（manifest 1 MiB、各資産 32 MiB）。I/O とハッシュ計算は
共有 runtime の blocking worker へ送り、native UI thread では行わない。
URL の hash によって JSON / GLB を同じ manifest snapshot に固定する。
GLB 内の外部 buffer / image URI は許可しない（埋め込み画像は使用可能）。

pane が見える frame で初めて取得し、個別タブではその機材だけ、全体では表示対象を
一台ずつ読み込む。非表示の間は新しい読み込みを始めない。取得中に pane が隠れても
予約済み RAF は取り消し、取得済みデータは保持する。
manifest と取得結果は WebView 内で保持し、GLB / JSON の hash の組で解析結果を再利用する。
欠落・破損・非対応 schema・解析失敗は簡略模型を保持する。maru 等の資産不在も同じ扱い。
失敗結果もこの WebView の間は保持するため、配布物を入れ替えた後の再取得には WebView reload が必要。

GLB はメートル・Y up、JSON は mm・x 右 / z 手前。既存 scene の単位を dm として
GLB を 10 倍、JSON を 1/100 に変換する。部品 node の名前は変えず、JSON の
`parts[].center` を平面位置の正本として適用する。高さ・回転・表面形状は GLB を保つ。
`control` がある部品だけ `kind.index` を解析して `userData.controlId/controlEvent` に付ける。
`knob / knob_touch / fader / fader_touch / button / pad`、index 0–255 を受け付ける。
control が無い装飾や鍵盤には推測で MIDI 対応を足さない。GLB 使用時に手組みの部品座標を混ぜない。
機材単位の raycast は親の device ID を辿り、既存のタブ・一覧との選択連動を維持する。

MIDI 割当対応と GLB の有無は独立。MiniLab 等の割当未対応機材も形を描く。
未対応の GLB は色を落として示す。半透明で未対応を表すと鍵盤や筐体が透けて形を読めないため、
不透明を保つ。選択輪郭は状態表示と別。モデル破棄時は view が所有する geometry / material / texture を解放する。

独立 Chrome fixture で MiniLab・X-Touch・nanoKONTROL の表示、隠れた pane で取得しないこと、
再表示時の取得、再選択時のキャッシュを観察。初回 X-Touch を含む最大 long task は 208 ms
（このマシンの単発測定、性能上限の保証ではない）。実 VP / WKWebView の見た目・応答性は lead の swap 後に確認する。

## Status log

- 2026-10-10: 初版。ユーザー裁定に合わせ上部・中央・下部の割り当てを分離。
- 2026-10-10: 型検査・対象37テスト・全体740テスト（`--maxWorkers=2`）を確認。既定並列の全体実行は fixture bundle を含むテストで timeout、並列数を抑えた再実行は全65ファイル通過。
- 2026-10-10: 独立ブラウザ fixture で相互選択・orbit・狭幅2列・同サイズvisibility切替中の状態更新・WebGL不可時の一覧維持を観察。Moody Blues の非表示復帰の指摘を回帰テスト付きで修正。実 VP / WKWebView は未確認。

- 2026-10-10: 下部を表示切替タブに変更。対象38テスト・型検査通過。独立ブラウザfixtureで個別機材の拡大表示と情報連動、Homeキーで全体復帰を観察。実VPは未確認。

- 2026-10-14: Midistage GLB / 部品表の検証付き遅延読み込みと簡略模型 fallback を追加。実 VP 確認待ち。
