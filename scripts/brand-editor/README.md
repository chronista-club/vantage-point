# VP Brand Studio

ロゴを調整し、編集値を含むSVGとして保存するローカルGenerator。
Python 3とES Modules対応ブラウザで起動できる。npm installやVPアプリの起動は不要。

リポジトリのルートで実行する:

```sh
python3 -m http.server 12087 --bind 127.0.0.1
```

<http://127.0.0.1:12087/scripts/brand-editor/> を開く。終了はターミナルでCtrl+C。
ポートが使用中なら別の番号を指定し、URLも同じ番号にする。

初回は `assets/brand/source.svg` の採用値から開始する。「現行に戻す」もこの値を使う。
線幅・太陽・背景を調整して「SVGを保存」し、次回は「SVGを開く」で続きから編集する。
同じタブ内の一時保存はsessionStorageにあるため、長期保存には書き出したSVGを使う。
本ディレクトリと `assets/brand/source.svg`、`assets/brand/PHOSPHOR-LICENSE.txt` は
同じリポジトリ内の相対配置で使用する。

採用するSVGを `assets/brand/source.svg` に置き、リポジトリのルートで実行する:

```sh
python3 scripts/brand_assets.py
python3 scripts/brand_assets.py --check
```

画像生成の依存は [ブランド設計](../../docs/design/74-brand-identity.md) を参照。
生成物は直接編集しない。ブラウザの保存操作だけでは正本は更新されない。

モデルの検証にはNode.jsを使う:

```sh
node --test scripts/brand-editor/model.test.mjs
python3 -B -m unittest discover -s scripts/tests -p test_brand_assets.py
```
