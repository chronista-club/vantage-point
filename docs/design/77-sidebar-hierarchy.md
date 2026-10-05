# サイドバーの階層と開閉

Status: implemented, native app verification pending (2026-10-05)
Memory: mem_1CfiPtheWd9pqJidqgjChF
Branch slug: sidebar-hierarchy

## 表示と操作

- CURRENTs は起動中・稼働中・停止処理中のプロジェクトを表示する。停止済み・未起動・エラーは下の「停止中」へ自動で移る。両セクションは同じスクロール領域に常時置く。判定は `RepoPaneState.state` と既存の `isRunningProcess` に従い、presence の一時的な通信切断では移動しない。
- プロジェクト見出しと main lane の行を統合する。プロジェクト名を親のラベルにし、選択すると main lane を開く。セッション名と agent 種類はツールチップ、セッション操作とプロジェクト操作は右クリックで確認できる。
- 左の矢印は子の開閉だけを行う。sub lane は一段内側に並び、畳むと sub lane 1本につき点1つを親行に残す。点は状態色を補助に使い、ツールチップに lane 名、集合のアクセシブル名に個数を持つ。展開中は点を出さない。
- lane 行の左の状態ドットと agent アイコンを外す。右に「作業中」「確認待ち」を表示する。branch は Git branch アイコンとともに残す。Board 新着、wire inbox、ショートカット番号も保持する。
- main は通常 dev-flow FSM の対象外なので、状態がない場合に pid だけから「作業中」を推定しない。入力待ちや状態が明示された場合は表示する。
- ACTIONs は native details で見出し1行と一覧を切り替える。子をアンマウントせず、入力ドラフトを保持する。
- Creo ID の操作は設定画面へ集約する。daemon は独立した下端表示を維持する。

## 保持する契約

`process:toggle` による開閉の永続化、repo/lane の D&D、選択 IPC、sub 作成、右クリックの既存操作は保持する。折りたたみ中の＋は開閉も展開へ進めてフォームを見せる。

## 検証

`sidebar-hierarchy.test.ts` は実コンポーネントを Solid で描画し、親行の選択、日文状態、点の個数、セクション移動、右クリック操作、ACTIONS ドラフト保持を確認する。ブラウザの280px幅プレビューで親子の位置と開閉を確認。インストール済みアプリへの差し替え後の検証は別途行う。
