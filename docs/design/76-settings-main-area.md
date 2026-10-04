# 76. 設定をメイン領域で開く

> **Status**: Draft
> **Related**: [doc 59](./59-settings-page.md)、`mem_1Cfgup9xPfPHvwJU2gSDUM`
> **対象**: `crates/vp-app/webview/src/sidebar/SettingsPanel.tsx`

## 目的と範囲

左下の設定入口はそのままに、設定本文を中央の広い領域に表示する。
既存の項目・保存 IPC・確定値の反映を維持する。新しい設定項目や全面改装は含めない。

## 構成

SettingsPanel は sidebar bundle の singleton のまま、Solid Portal で `#host` に描画する。
設定表示中も既存 pane の DOM、会話、入力、レイアウトを保持する。背後の host の子要素は
inert にして入力対象から外す。寸法と mount を維持するため display:none は使わない。
背景は不透明な設定面で覆い、左の lane ナビゲーションは操作できる。

「作業に戻る」と Esc で閉じ、開く前の focus を復元する。
選択 lane または repo component が変わったら設定を閉じる。その際、旧 lane に focus を戻さない。
同じ lane の定期的な state push は閉じる理由にならない。
設定は一時的な app view で、lane の pane 配置や永続化データには入れない。

## 幅と操作

host 全体を使い、本文は最大 960px で中央に置く。狭い host では余白を縮め、
入力行を折り返し、本文だけ縦スクロールさせる。上部の戻る操作は常に残す。
設定表示中に追加された host の子も inert にし、閉じる際は元の inert 状態へ戻す。

## 検証

実 Solid コンポーネントの DOM テストで中央への描画、元 DOM と入力の保持、focus 復帰、
lane 切替、同じ lane の再配信、既存の保存 IPC を確かめる。
実機では広い窓と狭い窓、sidebar の full/slim、会話入力中と pane 分割中を確認する。
実機確認と統合は root 側で行い、この lane では app:swap や共有 daemon 再起動をしない。

### 実機での確認手順（root 側）

1. この branch の bundle を生成したアプリを用意する。起動・差し替えのタイミングは root が判断する。
2. lane の会話欄に未送信の文を入れ、pane を分割してスクロール位置を変える。
3. 左下の「設定」を開き、中央に既存項目が出ること、左の lane 一覧が操作できることを確認する。
4. 「作業に戻る」と Esc をそれぞれ試し、入力・スクロール・pane 配置が保たれることを確認する。
5. 設定を開いたまま別 lane を選び、その lane の作業画面へ移ることを確認する。
6. 設定を開いて sidebar を slim にし、窓を縮める。入力欄の折り返しと本文の縦スクロール、戻るボタンを確認する。
7. 初期フォルダ等を変更して開き直し、保存値を確認して元に戻す。更新・daemon 再起動ボタンはこの確認では実行しない。

## Status log

- 2026-10-04: 設定本文の Portal 化と作業状態を保持する設計を記録。
