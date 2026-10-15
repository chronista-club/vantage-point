# 80. Lane の永続リンクを CLI・MCP・UI で共有する

> Status: Lane リンクへの改編を wip/lane-links で実装 — native app integration pending
> Memory: mem_1Cfnew6md9ar3KhUAJPpmW / mem_1Cg4Mh2158eTTSiPPymwWp（Lane リンクへの改編）
> Related: [79](79-sidebar-session-identity.md) / [利用ガイド](../guide/lane-local-urls.md)

## 合意

URL は Lane に残るリンク集とする。停止・Agent 終了で消さず、明示削除まで保持する。
新しい Lane に自動コピーしない。登録と接続状態は区別する。
CLI/MCP/UI が同じ登録を扱い、外部で登録した変更も R sidebar の Lane 節へ反映する。
登録 URL の probe は手動、出力から検出した URL は表示中に自動確認する（下記の追加裁定）。

## Lane リンクへの追加裁定

mako の原文:

> 「これがしょっちゅう出るようになった。この機能使ってないから生まれ変わらせたいな。」
> 「リンク集の方はその方向で進めよう」= GO

旧 sidebar の各 lane 行に出る「ローカル URL」「確認が必要」と 8 秒 timeout のエラーが契機。
この GO により、当初の非対象「自動ポート探索」（design 79）の範囲を更新し、**出力に現れた
loopback URL の自動検出**を加える。ポート範囲の走査は行わない。

UI は各 lane 行から外し、[design 82](82-r-sidebar-prs.md) の R sidebar 上起点「Lane」節に
置く。表示中 lane の 1 件分だけ、次の順に並べる。

1. console / chat 出力で見つかった URL。応答を点で示し、「登録」で登録フォームへ渡す
2. この lane の PR。PR 一覧の同じ応答と `orderPrs` の先頭固定結果を使う
3. 手で登録した URL。追加・編集・削除と保存の競合検出を維持する

検出だけでは保存しない。URL の点は HTTP 応答の有無であり、アプリの正常動作を保証しない。

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
UI は **R sidebar と Lane 節が両方開いている時だけ**登録内容を読み直す（初回・3 秒ごと・focus）。
入力中は反映を延期して draft と expected を保持する。登録内容が変わった場合は古い probe 結果を破棄する。
lane の変更・節を閉じた後に遅れて届く結果は generation で捨て、次の要求を発行しない。

### 検出と購読

- 正規化と履歴は `webview/lane-link-discovery.ts` に集約。PTY は `term.ts::handleOutput` の
  replay / live、chat は renderer より手前の `console.ts::handleEvent` の text / tool 出力から拾う。
  user 入力と tool 引数は拾わない。追加の daemon 購読・保存・全 lane への取得要求は不要
- ANSI を除去し、HTTP(S) の `localhost` / `127.0.0.0/8` / `[::1]` のみ採用する。
  出力の `0.0.0.0` は接続先 `127.0.0.1` へ写す。URL パーサーで末尾 `/`、既定 port 等を正規化する
- 最近 32 URL / lane、64 lane まで保持。未完 token は stream ごとに 16 KiB、16 stream / lane まで。
  PTY の UTF-8 / URL は chunk 間をつなぎ、改行しない末尾は 250 ms の idle 後に確定する
- 検出 URL は Lane 節の表示中に順番に probe する。「再確認」で再取得できる。
  UI 専用 `probe_url` は保存を経由せず、既存 `local_urls::probe` と同じ検証・proxy / redirect 拒否を使う。
  CLI / MCP の `probe <name>` は登録名を取る従来の契約のまま
- PR 取得は既存 `createPrPoller` 1 本。Lane 節または PR 節が表示中なら動き、両方開いても重複しない
- `lane-links-bridge.ts` は main の表示状態・検出結果・PR と sidebar bundle の登録 UI を接続する。
  最後の表示を保持し、bundle 評価順を問わず復元する。IPC 相関 map は sidebar bundle から動かさない

### timeout の調査で確認した範囲

旧 UI の fixture では 4 lane 行を mount すると初回 4 要求、次の refresh で計 8 要求。
新 UI は左の 4 行から 0 要求、R の Lane 節を開いた時に表示中 lane のみ 1 要求になる。
旧 native load は daemon RPC / probe を通らず、共有 JSON を直接読む。現在の CLI の list は
2 lane で 22–28 ms だった。実アプリの旧欄に timeout 表示が残ることも観測したが、
**全 lane の要求集中が実機の 8 秒超過を直接起こしたという因果関係は未立証**。
不要な取得の除去と timeout の再現原因は区別し、差し替え後の実機で追う。

## 検証

名前による重複しない更新、永続化、Lane/repo分離、同時書込み、旧UUID互換、
CLIのcwd/明示対象、MCP router/呼出し、UI外部変更反映・draft競合保護をテストする。
実在する登録や会話を検証用に削除しない。fixture の config/state を隔離する。

## 永続 URL API 実装時の観測（前身 lane-url-api）

- shared store 8テスト・アプリの既存URL 5テスト成功。旧UI互換、UUID、同時更新、競合保護、Lane分離を検証。
- CLI subprocess と MCP stdio を隔離 config/state で実行し、相互の登録・更新・削除と503の手動probeを確認。
- WebView: 63ファイル709テスト、型検査、bundle成功。Chromeのfixture画面で名前付き登録と表示を確認。
- workspace全体の `cargo clippy --workspace --all-targets -- -D warnings` 成功。
- インストール済みアプリ・MCPサーバーへの差し替えは未実施。実アプリと外部登録の連動は統合後に確認する。

## Status log

- 2026-10-16: `wip/lane-links` で UI を R sidebar の Lane 節へ移し、出力 URL の自動検出と PR 共用を追加。
  CLI/MCP と永続保存形式は維持。native app の差し替え・再起動は lead が担当し、実機確認待ち。
