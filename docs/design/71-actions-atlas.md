# 71. Project と独立したメモ・Action

> **Status**: Implemented — 主要導線の実機確認済み
> **Related**: design 57、58、`mem_1Cf1r1bEcTcknGH3Xk3naa`
> **対象**: `creo/client.rs`, `webview/src/sidebar/actions-panel/`

思いつきを捕まえる一覧は VP 全体で共通にする。Project の選択から一覧・下書き・保存先を導出しない。
Atlas は一件の帰属、Project は実行する場所として分ける。

## 体験

- メモを書き、明示的に Atlas を選んで保存する。未選択のまま別の Atlas に保存しない。
- 一覧には所属 Atlas を表示する。まずはこの面で捕まえた記憶を対象とする。
- 捕捉時に todo へ分類する必要はない。未整理と完了できる todo を区別する。
- Project を切り替えても入力と選択は維持する。保存に失敗した入力も残し、理由を表示する。
- Project / セッションへの着手連携は後続の設計で扱う。

## 接続の契約

Creo が記憶と帰属の正本。daemon が認証と HTTP を担い、WebView へトークンを渡さない。
API の一覧・作成・状態変更は Creo の現行実装と照合する。旧 `tags=vp-actions` は
現行 API で無視されるため、対象の絞り込みには使わない。
`status` は REST の互換表現として残るが、完了は todo の印から導出される。
lock 中の記憶や、操作権限のない Atlas に対する失敗を成功扱いにしない。

既存の本文・帰属を推測で変更せず、部分取得を全件取得と扱わない。

## 検証

- Atlas / kind / lock が API から UI まで失われない。
- Project 切替は一覧と捕捉中の下書きに影響しない。
- 保存先未選択・無効な保存先・HTTP エラー時に入力が残る。
- 保存した Atlas と再取得した帰属が一致し、対象外の記憶を表示しない。

## Status log

- 2026-09-14: Project からの独立と、入力時の Atlas 指定を合意。実装に着手。
- 2026-09-15: ユーザー実機で Atlas 指定保存、Project 切替時の下書き・選択・一覧保持、旧 ACTIONS の取り込みを確認。

## 一覧の帰属と保存の復旧

継続的な表示ゲートはユーザー所有の `vp-actions` ラベルだけとする。
Atlas は保存先として扱い、一覧取得を Atlas / Project で絞らない。
ラベル未作成時は空一覧とし、無条件の記憶一覧へフォールバックしない。
`labelIds` で全ページを読み、途中の失敗を空一覧として適用しない。

作成は `POST /api/memories` の `atlasId` / `slug` / `metadata.vp.client_id` と、
`POST /api/memories/:id/labels` の `labelId` を使う二段階となる。
固定 slug は Creo の DB で大域一意。応答不明時の GET は表示名解決にも
フォールバックするため、slug・client_id・所有者・Atlas をすべて再照合する。
作成済み ID をローカルに原子的に記録し、ラベル付与失敗で新規作成に戻らない。

保存記録は接続先とアカウントで分離する。JWT の issuer / subject は
ローカル記録を分ける識別子としてのみ使い、権限の判断は Creo に任せる。
WebView へトークンを渡さず、scope のダイジェストだけを往復する。
アカウントを切り替えた後に到着した古い保存要求は拒否する。

旧 `metadata.legacy_tags` の `vp-actions` が移行候補。`metadata.vp` だけを
帰属の証拠にはしない（旧 API の無条件取得で混入した可能性がある）。
既存の本文・帰属を変更する自動移行は行わない。

「以前の ACTIONS を取り込む」の明示操作で、所有者と旧タグが一致する記憶へ
ラベルだけを付ける。候補 ID と進捗を先に記録し、失敗後は残りから再開する。
旧タグは `metadata.legacy_tags`、それが配列でない旧形式では `metadata.tags` を読む。

下書きと Atlas 選択はアカウント別にアプリ内で保持する。未送信の下書きは
アプリ終了を越えて永続化しない。保存要求は daemon の outbox に永続化し、
次の同期でも再試行する。作成結果の ID が戻るまでは行の削除を無効にする。
並べ替えは旧 bucket にかかわらず一覧全体の順序として扱う。
旧 bucket 間で順序が重複する場合は再採番する。ロック行が含まれる場合は
変更前に止め、Creo 側でのロック解除が必要な理由を表示する。
取り込み対象が途中で消えた場合は、記憶取得でも 404 を確認してから次へ進む。

## Creo の Atlas サイドバーを保存先に使う

Creo [spec 33](https://github.com/chronista-club/creo-memories/blob/nightly/docs/spec/33-sidebar-atlas-tree.md)
の見え方を読み取る。VP は読むだけで、並び・非表示・親子関係の編集は Creo で行う。

- 保存先を押すと popover に Atlas の木を全展開する。Creo `AtlasRailPeek` の通常行と同じく、
  アイコンを足さず、16px の字下げ、名前、共有元の title、選択中の強調で示す。
- `GET /api/atlas` の `parentId` を使い、`path` から親を推定しない。この応答に `isApps` / `createdAt` も含まれるため、親ごとの追加取得は不要。
- daemon は catalog・一覧と並列に `GET /api/app-settings/creo/sidebar` を読む。
  本物の親子に `order` / `hidden` を重ね、廃止された `placement` は読まない。
  未指定の兄弟は Creo と同じく catalog の応答順。`/apps` と非表示 Atlas の子孫は隠す。
  未知 ID と項目単位の壊れた値は捨て、全体が壊れた設定・取得失敗では既定の木と一言を表示する。
- 書けない Atlas も構造のため残すが選択はできない。選択中の Atlas が hidden になった場合は
  その行を選択肢の末尾に残す。選択欄には path を表示し、Project 切替や reload で選択を変えない。
- 木の純関数は Atlas の最小 interface と settings だけを受け、表示順の行を返す。
  VP の store や生成型を参照しない。creo-ui への移設は別の変更とする。
- Creo の編集・移動・非表示変更・「すべての Atlas」への移動は表示しない。
  VP では行の選択が保存先を決め、↑↓ / Home / End で移動、Enter で選択、Esc で閉じる。

### 一括 reload

ACTIONs 見出し右端の reload は、起動時と同じ daemon の取得入口で一覧・catalog・並び設定を
取り直す。通信中は回転表示し、連打を一回にまとめる。クリックは summary の開閉に伝播させない。
下書きと選択、保存待ちの行を初期化しない。保存と読み込みは同じ門を通り、保存待ちは daemon の
outbox と既存の受信合成で保持する。取得失敗は理由を表示し、直前の一覧を維持する。

- 2026-10-14: `mem_1Cg1m3LoeDu62UCKVT5UcA`。Atlas popover と一括 reload を実装。
  WebView 801 テストと認証付き sidebar API の読み取り確認が通過。
  Rust workspace は既知の terminal_ops 不安定テスト1件を除いて通過。
  実 VP への差し替えと表示確認は lead が行う。
