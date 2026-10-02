# Vantage Point ブランド正本

**図案の唯一の編集元は `source.svg`。** 2026-10-02に採用した4色の山・太陽と、緑の光がにじむアプリ背景を含む。`vp-mark` groupが背景なしのロゴ、外側の構成がアプリアイコン。

生成方法・配色・同期先は [design 74](../../docs/design/74-brand-identity.md)。`generated/` と `crates/vp-app/assets/icon.*`、portal側のファイルは生成物であり、直接変更しない。

```sh
python3 scripts/brand_assets.py
python3 scripts/brand_assets.py --check
python3 scripts/brand_assets.py --portal /path/to/vantage-point-portal
```

採用元: https://vantage-point.app/brand/studies/2026-10-02-v7/app-icon.png

Phosphor Mountainsを出発点として再設計した図案。`PHOSPHOR-LICENSE.txt` の表示を保持する。独占的な図案であることや商標登録可能性を保証するものではない。
