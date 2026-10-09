# 74. ブランドアイコンの正本と派生アセット

> **Status**: Active — 2026-10-09 Generator調整版を採用。アプリ配布への搭載は本変更を含む次回ビルドから。
> **Related**: `mem_1CfcmcA8nR5b4reRgFgbFj`（図案の検討と採用）
> **対象**: `assets/brand/`, `scripts/brand_assets.py`, `crates/vp-app/assets/icon.*`, portal `public/brand/v1/`

## 元図案の決定（2026-10-02）

以下はv7採用時の記録。現在の採用値は末尾の「2026-10-09 採用更新」と正本SVGのmetadataを参照。

山と太陽を引き継ぐD案のv7を採用。雪の境界を波線にし、谷を連続した輪郭へ整理。中央の内側線は16pxから12pxへ滑らかに絞る。波線は14px。丸は中心(166,40)、半径18、線幅14（マークのviewBoxは256角）。

| 部分 | Contrast darkのtoken | 採用したsRGB |
|---|---|---|
| 左山頂 | semantic-info | `#65d2d2` |
| 左山腹 | brand-primary | `#617de6` |
| 右山 | brand-secondary | `#e773d1` |
| 太陽の内側 | semantic-warning | `#d4a73e` |
| 背景の光 | semantic-success | `#37b880` |
| 輪郭 | text-primary | `#edeef5` |

色はcreo-ui 0.25.0のContrast darkから採用した固定値。背景の基底は`#242641`→`#0a0826`、緑のradial glowは中心(36%,30%)、半径72%、stopは0%/55%/100%・opacityは64%/26%/0%。柔らかい影を加える。UIテーマの更新でロゴの色を自動的に変更しない。

## 正本と生成

`assets/brand/source.svg` 一つに図案と背景を持つ。`vp-mark` groupを抽出するとヘッダ用の背景なしロゴになる。アプリ用とヘッダ用にパスを二重管理しない。

`scripts/brand_assets.py` はPython標準ライブラリとresvgを使用し、以下を決定的に生成する。追加のPythonパッケージは不要。

- `crates/vp-app/assets/icon.svg/png/icns/ico`: 既存のネイティブアプリの読み込み先。PNGは1024角、ICNSは標準/Retina、ICOは16/24/32/48/64/128/256のPNGエントリ。
- Mac専用の `icon-macos.svg/png` は1024角に824角の原図を中央配置し、四辺に100pxの透明余白を持つ。これは現行ICNS/NSImage経路の表示寸法補正で、Apple全形式共通の規定値ではない。ICNSの全解像度と実行時Dock表示はこの派生画像を使い、Web/Windows/tray用の画像は従来のままにする。
- `assets/brand/generated/`: 背景なしmark.svg、アプリSVG/PNG、32px favicon、180px touch icon、ライセンス、hash付きmanifest。
- `--portal <repo>` を指定した時のみ、portalの`public/brand/v1/`と`public/favicon.svg`へ同じデータを同期する。

portalのNavは`/brand/v1/mark.svg`を参照する。faviconとタッチアイコンもこの生成経路を使用する。新ブランドの図案を大きく改訂する際は配信版を上げる。`brand/studies/`は過去の検討資料として固定し、正式アセットの編集元にはしない。

## 操作

HomebrewのresvgとPython 3.9以降が必要。

```sh
brew install resvg
python3 scripts/brand_assets.py --portal /path/to/vantage-point-portal
python3 scripts/brand_assets.py --check --portal /path/to/vantage-point-portal
python3 -m unittest discover -s scripts/tests -p test_brand_assets.py
```

`--check`は生成結果とファイルを比較し、不一致がある場合はexit 1。ファイルは書き換えない。resvgの版はmanifestに記録する。レンダラー更新後に再生成して差分をレビューする。

## ライセンスと配布

Phosphor Mountainsを出発点にしているため、そのMIT表示を保持する。原図のライセンスは`assets/brand/PHOSPHOR-LICENSE.txt`。単独SVGにもmetadataとして全文を含め、Webの配信セットとネイティブアセットには同名ファイルを含める。

Macの`release:mac`は生成物の一致を検査してからビルドし、`.app/Contents/Resources/Phosphor-LICENSE.txt`も同梱する。Windowsの配布を行う際もライセンス文を製品の配布物に同梱すること。

現在インストールされている署名済み`.app`の中身を手で差し替えない。新アイコンは通常のビルド・署名・配布、または開発用の`app:swap`を経て反映する。

## やってはいけない

- portal、favicon、PNG、ICNS、ICOを個別に描き直さない。
- 小サイズ向け補正を黙って混ぜない。必要になった時は正本に用途を定義し、生成側へ追加する。
- ICNSだけ余白を直して実行時Dock PNGを据え置かない。起動時のNSImage設定で大きい画像に戻ってしまう。
- 検討画像のURLを上書きして過去の会話の参照を変えない。
- 出典を消して完全に無由来の図案と扱わない。

## Status log

- 2026-10-02: ユーザー「OKこれでいこう」でv7を正式採用。正本、生成器、派生物、portal同期経路を確定。
- 2026-10-05: Dockの外形が他アプリより大きいという実機報告から、Mac専用の透明余白を導入。原図・Web・Windows画像は維持。生成検証済み、反映後のDock実機比較は未確認。

## パラメトリック編集（2026-10-08）

`scripts/brand-editor/` にローカルの比較エディタを置く。正本を読み込み、白い輪郭の外周・雪・中央上/下・太陽、雪の波の深さ、白の色、角の結合を独立に調整する。中央の仕切りは山の共有辺に沿うリボンとして生成し、上から下へsmoothstepで幅を変える。現行プリセットでは既存パスを保持する。

```sh
python3 -m http.server 12087 --bind 127.0.0.1
# http://127.0.0.1:12087/scripts/brand-editor/
node --test scripts/brand-editor/model.test.mjs
```

- 左に正本、右に調整中のアイコン。16/32/64/128pxと背景なしのプレビューも表示する。
- 「色を広く見せる」は外周12、雪8、中央12→8、太陽8の検討プリセット。正式採用の変更ではない。
- SVG保存は通常の描画要素と `metadata#vp-brand-parameters`（version=1、values）を持つ。再読込は検証済みのパラメータのみを採用し、インポートされた任意のSVGコードは実行・表示しない。
- 採用時には保存したSVGを `assets/brand/source.svg` に置き、既存の `scripts/brand_assets.py` で全形式を再生成する。生成物を直接編集しない。
- ブラウザを閉じる前にSVGを保存する。調整途中の値は自動で正本に書き込まない。

Status: 編集ツールを追加。図案は下記2026-10-09採用更新を参照。Auth0への公開反映は別途。

太陽は直径・横位置・縦位置も調整可能。背景はグラデーションの左上/右下の色、光の色/強さを独立に持つ。同じタブではsessionStorageに調整途中の値を保持する。SVGの保存にはPhosphorのライセンス全文も含める。インポート時の旧形式（太陽・背景パラメータなし）は現在の既定値で補完する。

背景の光の中心は `glowX` / `glowY`（0〜100%、既定36/30）で調整する。左上を0/0、右下を100/100とし、SVGのradialGradientのcx/cyに反映する。旧保存値に中心指定がなければ既定値で補完する。


### 2026-10-09 採用更新

ユーザー指定の `~/Downloads/vantage-point-parametric (4).svg` をバイト一致で正本に採用。SHA256: `b5c3938a3d599e3a86d85fe39139a0839fdf17c0e6a16bf5e93d5aabe3ffc04e`。冒頭のv7値は元図案の記録とし、現行値はSVGのmetadataを正とする。

外周14、雪8.5、中央14→6.5、太陽輪郭7.5、波深さ12、線色#fffbf5。太陽は直径42・位置143/42.5。背景光#7d8bf2、強さ94%、中心52%/36%。山の配色と背景の基底2色は従来どおり。

生成manifestは古いv7画像を現在の採用元として指す `approvedStudy` を廃し、本designを参照する。Generatorの「現行に戻す」は正本内のパラメータを使う。
