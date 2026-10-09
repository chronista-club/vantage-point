# Vantage Point ブランド正本

**図案の唯一の編集元は `source.svg`。** 2026-10-09に採用したGenerator調整版。4色の山・太陽と、青紫の光がにじむアプリ背景を含む。`vp-mark` groupが背景なしのロゴ、外側の構成がアプリアイコン。

生成方法・配色・同期先は [design 74](../../docs/design/74-brand-identity.md)。`generated/` と `crates/vp-app/assets/icon.*`、portal側のファイルは生成物であり、直接変更しない。

```sh
python3 scripts/brand_assets.py
python3 scripts/brand_assets.py --check
python3 scripts/brand_assets.py --portal /path/to/vantage-point-portal
```

採用元: Generatorから書き出した `vantage-point-parametric (4).svg`（2026-10-09）。調整値は `source.svg` のmetadataに保存。

元図案: https://vantage-point.app/brand/studies/2026-10-02-v7/app-icon.png

Phosphor Mountainsを出発点として再設計した図案。`PHOSPHOR-LICENSE.txt` の表示を保持する。独占的な図案であることや商標登録可能性を保証するものではない。

Mac用の `crates/vp-app/assets/icon-macos.svg/png` と `icon.icns` は外周に透明余白を加えた派生物。Web/Windows用は原図の外形を保つ。Mac用も同じ生成コマンドで更新する。

## Generatorを起動して再編集する

リポジトリのルートで `python3 -m http.server 12087 --bind 127.0.0.1` を実行し、
<http://127.0.0.1:12087/scripts/brand-editor/> を開く。
現行と比較して線幅を調整し「SVGを保存」。保存SVGを「SVGを開く」で再編集できる。
採用する図案を決めたら、そのSVGを `source.svg` に置き、上記の生成コマンドを実行する。
必要環境・再開・検証の手順は [Brand Studio README](../../scripts/brand-editor/README.md) を参照。
