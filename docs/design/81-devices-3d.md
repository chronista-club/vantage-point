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
ライブ MIDI 値、外部モデルファイル、物理ポート操作は今回の範囲外。

## データと形

状態の正本は `MidiUseStatus`。別経路の `DeviceSnapshot` は従来の IN/OUT 情報一覧だけに使う。
表示用 profile は寸法比と control の種別・位置・安定 ID を保持し、Three.js や I/O に依存しない。
ID は機材内で一意（例 `strip.0.fader`、`pad.0`）。将来の値反映は `device_id + control ID` に結び付けられる。

X-Touch、LPD8 mk2、nanoKONTROL2、ROTO-CONTROL を primitive で模式的に組み、他は箱と名前にする。
X-Touch の 8 strip、LPD8 の 8 pad、ROTO の 8 slot は `midistage-profiles/src/device_profile.rs` の protocol 実装に対応する。
同ファイルは座標データを持たないため、実寸 CAD の再現とは主張しない。
nanoKONTROL2 は #1190 の標準 CC (ch1) 操作対応を引き継ぐ。LED 表示は未対応。

| 表示 | 条件 |
| --- | --- |
| 通電色 | 接続中・present・エラーなし・対応 profile・全体 ON・active・owner が VP・lease が現在の session |
| 半透明 | 接続している対応機材で上記以外（全体 OFF、他アプリ割当、切替中を含む） |
| 灰色 | サービス切断、機材未接続、機材エラー |
| wireframe | 接続中だが VP の MIDI 操作割当が未対応 |

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
- fixture ブラウザ観察と実 VP / WKWebView の確認は別の証拠。`app:swap` は lane から実行せず、mako が VP 外のターミナルで行う。

## Status log

- 2026-10-10: 初版。ユーザー裁定に合わせ上部・中央・下部の割り当てを分離。
- 2026-10-10: 型検査・対象37テスト・全体740テスト（`--maxWorkers=2`）を確認。既定並列の全体実行は fixture bundle を含むテストで timeout、並列数を抑えた再実行は全65ファイル通過。
- 2026-10-10: 独立ブラウザ fixture で相互選択・orbit・狭幅2列・同サイズvisibility切替中の状態更新・WebGL不可時の一覧維持を観察。Moody Blues の非表示復帰の指摘を回帰テスト付きで修正。実 VP / WKWebView は未確認。

- 2026-10-10: 下部を表示切替タブに変更。対象38テスト・型検査通過。独立ブラウザfixtureで個別機材の拡大表示と情報連動、Homeキーで全体復帰を観察。実VPは未確認。
