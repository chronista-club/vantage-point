# 74. ブランドアイコンの正本と派生アセット

> **Status**: Active — 図案採用・生成経路確定。アプリ配布への搭載は本変更を含む次回ビルドから。
> **Related**: `mem_1CfcmcA8nR5b4reRgFgbFj`（図案の検討と採用）
> **対象**: `assets/brand/`, `scripts/brand_assets.py`, `crates/vp-app/assets/icon.*`, portal `public/brand/v1/`

## 決定

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
