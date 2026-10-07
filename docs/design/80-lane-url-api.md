# 80. Lane の永続リンクを CLI・MCP・UI で共有する

> Status: Implemented in wip/lane-url-api — native app integration pending
> Memory: mem_1Cfnew6md9ar3KhUAJPpmW
> Related: [79](79-sidebar-session-identity.md) / [利用ガイド](../guide/lane-local-urls.md)

## 合意

URL は Lane に残るリンク集とする。停止・Agent 終了で消さず、明示削除まで保持する。
新しい Lane に自動コピーしない。登録と接続状態は区別し、probe は手動に限定する。
CLI/MCP/UI が同じ登録を扱い、外部で登録した変更もサイドバーへ反映する。

## 契約

- `vp lane url set <name> <url> [--label <label>] [--lane <lane>]`
- `vp lane url list [--lane <lane>]`
- `vp lane url rm <name> [--lane <lane>]`
- `vp lane url probe <name> [--lane <lane>]`
- MCP `lane_url`: `action: set | list | rm | probe` と同じ引数。JSON で結果を返す。

name は Lane 内の安定キー。小文字英数字で始まる英数字・ハイフン・アンダースコア、64文字以内。
同名 set は URL を更新する。label 省略時は既存用途を維持し、新規なら name を用途にする。
rm は未登録でも成功して空振りを許す。probe は登録済みの名前のみ対象。
既存 UI の UUID はそのまま name として扱い、データを書き換える移行は行わない。

現在の作業ディレクトリと登録 repo から対象を解決する。継承した VP_REPO/VP_LANE が
別作業台を指すことがあるため cwd を正とする。明示指定は Lane 名または repo/Lane。
root/main は root Lane を指す。存在しない repo/Lane への書込みはエラーにする。

## 構成

保存・検証・probe の実装を軽量 `vp-local-urls` crate で共有する。GUI の重量依存を
CLI/MCP へ持ち込まない。保存先・repo絶対パス+Lane名のキーは design 79 と同じ。
GUI の旧 API は共有crateを re-export し、既存呼出しと保存形式を保つ。
CLI/MCP は同じマシンの store を操作するので daemon の起動を必須にしない。

ロックの中で最新データを読んで named upsert/remove を適用し、他の登録を保持する。
UI の全件保存は従来どおり expected による競合検出を使う。
UI は登録内容だけを定期的に読み直す。入力中は反映を延期して draft と expected を保持する。
到達状態を自動 probe しない。登録内容が変わった場合は古い probe 結果を破棄する。

## 検証

名前による重複しない更新、永続化、Lane/repo分離、同時書込み、旧UUID互換、
CLIのcwd/明示対象、MCP router/呼出し、UI外部変更反映・draft競合保護をテストする。
実在する登録や会話を検証用に削除しない。fixture の config/state を隔離する。

## 実装時の観測

- shared store 8テスト・アプリの既存URL 5テスト成功。旧UI互換、UUID、同時更新、競合保護、Lane分離を検証。
- CLI subprocess と MCP stdio を隔離 config/state で実行し、相互の登録・更新・削除と503の手動probeを確認。
- WebView: 63ファイル709テスト、型検査、bundle成功。Chromeのfixture画面で名前付き登録と表示を確認。
- workspace全体の `cargo clippy --workspace --all-targets -- -D warnings` 成功。
- インストール済みアプリ・MCPサーバーへの差し替えは未実施。実アプリと外部登録の連動は統合後に確認する。
