# 79. Lane のセッション識別とローカル URL

> **Status**: Implemented — native アプリでの操作確認待ち
> **Related**: design 58、design 77-sidebar-hierarchy、`mem_1CfkeiUePgFsbYoGtTeDpq`
> **対象**: `crates/vp-app/webview/src/sidebar/LaneRow.tsx`、`crates/vp-app/src/local_urls.rs`

## 要件

Lane 行は状態ポイント、代表セッションの Agent アイコン、タイトルの順に並べる。
Agent の正本は registry の root entry。registry 不在時だけ lane.agent へ縮退する。
追加セッションには Agent、追加セッションという役割、session 番号、モードを表示する。
行の選択は対象 Lane と session に向け、終了は対象を表示した二段階確認を経る。
代表セッションの終了は UI と既存 registry の両方で拒否する。

Lane ごとに複数のローカル URL と用途を登録・編集・削除し、再起動後に復元する。
登録 URL はサイドバーから外部ブラウザで開ける。自動ポート探索は行わない。

## 構成

セッション終了は既存 `conversation:session_remove` と backend の reconcile を再利用する。
TUI / GUI の両方に効く。失敗は Lane と session を添えて sidebar のエラー面へ返す。
実在する session #57 は検証に使わない。

ローカル URL の操作は vp-app の native IPC で処理する。永続化の鍵は repo の絶対パスと
Lane 名の組。単なる repo 表示名を鍵にしない。保存成功時だけ UI の確定値を更新し、
壊れた保存内容や書き込み失敗はエラーとして返す。

URL は http / https、localhost / 127.0.0.0/8 / ::1 に限定し、資格情報を拒否する。
native の HTTP HEAD で手動確認する。自動 poll は行わず、timeout を設け、環境の HTTP
proxy と redirect を無効にする。localhost は loopback に解決する。HTTP エラー応答も
「応答あり」であり、アプリの正常稼働とは断定しない。接続拒否だけを「接続拒否（停止の可能性）」、
timeout・TLS・その他の失敗を「確認失敗」、未実行を「未確認」と表示する。
状態は保存しない。編集・再起動後は未確認へ戻す。

保存先は `vp_paths::vp_state_dir()/lane-local-urls.json`。同時書き込みは lock file で
直列化し、保存時に読み込み時点の entries と比較して他 window の変更を上書きしない。
URL は最大16件 / Lane、用途は120文字以内。Lane 名や repo のパスを変えた場合は
別の保存キーになるため、自動移行しない。

## 使い方

登録がある Lane だけ「ローカル URL」の節を表示する。初回追加は Lane 行の右クリックから
「ローカルURLを追加」を選ぶ。追加中・読み込みエラー時は操作欄を表示する。
登録後は節内の「URLを追加」も使える。最後の URL を削除すると節を隠す。
用途のリンクは外部ブラウザを開き、「確認」はその場で HTTP HEAD を実行する。
「編集」「削除」は登録内容を変更する。保存競合や読み込み失敗時は表示された
エラーを確認し、「再読み込み」で最新の登録を取得する。

追加セッションは行を選ぶと対応する pane を表示する。格納済みなら、その明示選択で
復元する。× を押して表示された session 番号を確認し、もう一度押すと終了する。
Esc またはボタンから focus を外すと確認を取り消す。

表示 fixture は `crates/vp-app/webview` で `bun preview/sidebar-sessions.mjs 12891` を実行し、
`http://127.0.0.1:12891/` で確認できる。fixture の保存先はブラウザの localStorage。
実際の native 永続化・接続状態とは別で、実セッションには接続しない。

## やってはいけない

- CORS / opaque response / timeout を停止と断定しない。
- redirect 先を確認しない。未登録 URL を探索しない。
- UI から終了成功を先取りして行を消さない。registry snapshot を待つ。
- 別 lane / root checkout を変更しない。app swap、daemon restart、merge は本作業に含めない。

## 検証

- 実 Solid コンポーネントで registry の Agent、追加行の識別、選択先、終了確認、root 保護。
- URL 制約、複数登録、更新削除、Lane 分離、保存復元、保存失敗。
- loopback fixture による応答 / 接続拒否 / redirect 非追跡。
- 独立 preview で表示・操作を観測。native アプリでの最終確認は別途。

## Status log

- 2026-10-06: 合意済み仕様を受領。既存セッション終了契約と native 確認方針を記録。
- 2026-10-06: 実装・回帰テストと独立 preview の操作確認。app swap と daemon restart は未実施。
