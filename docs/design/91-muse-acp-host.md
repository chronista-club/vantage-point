# 91. Muse Code の ACP / TUI host

> **Status**: Draft
> **Related**: design 88 (`88-agy-acp-host.md`)、`mem_1ChTGsc5BoJHRTqsjN5M4p`
> **対象**: `conversation/engine.rs`, `conversation/acp_host.rs`, `conversation/muse_settings.rs`, `repo/agent_spawner.rs`, `repo/conversation_ops.rs`, `repo/lane/pool.rs`, `vp-app/webview/agy-settings-panel.tsx`

Meta Muse Code を `muse` engine として選択できるようにする。TUI は CLI を起動し、Chat は既存の `AcpAgentHost` / 共有 `agent-chat-acp::AcpClientHost` で常駐駆動する。protocol 実装は増やさない。サブスク usage meter の Muse probe は今回の対象外。

## 接続の契約

起票時の lead 実測（Muse Code 1.4.25、2026-11-27、上記 memory）:

- `initialize` は ACP v1。`loadSession: true`、画像対応、HTTP MCP 対応を返す。
- 会話 ID は UUID 形式。`session/new` / `session/load` は `configOptions` を返す。
- Model 候補は `muse-wonder-1.0` / `muse-ember-1.0` / `muse-spark-1.3` / `muse-spark-1.2`。
- Mode 候補は `allowAll` / `promptUnmatched` / `onRequest` / `denyUnmatched`。既定は `onRequest`。
- `session/set_config_option` で Model / Mode を切り替える。応答と `config_option_update` / `current_mode_update` は標準 ACP の形。
- `available_commands_update`、`session_info_update` も標準 ACP の形で届く。

上記は版付きの観測で、固定 catalog ではない。VP は接続した engine の候補と現在値を表示する。
この lane では `muse --version` / `muse --help` / `muse acp --help` を再確認した。ログイン済み実 CLI への prompt は検証として送信していない。

## 起動と会話の保存

| 面 | 起動 |
|---|---|
| TUI 新規 | `muse --yolo [--model '<model>']` |
| TUI 再開 | `muse --yolo [--model '<model>'] resume '<id>' || muse --yolo [--model '<model>']` |
| Chat | `muse acp --disable-sandbox --trust-workspace` |

ACP の CLI 解決は PATH を先に見て、`$HOME/.local/bin/muse` を fallback に使う。TUI は既存の login shell に command を渡す。
`--yolo` は approval と sandbox を無効にして workspace を trust する起動方針。Chat の permission request は既存 ACP host の自動許可方針を使う。Mode は engine 側の設定として表示・保存する。

会話 ID は registry の session entry に保存する。TUI は英数・ハイフンの ID を引用して渡し、先頭ハイフンや shell 記号を含む ID は再開に使わない。model も shell に渡す前に形式を検証する。ACP load の失敗は共有 host の `NewSession` fallback で新規会話に切り替わり、新 ID を registry に記録する。

画像は `ChatHost::Muse` から ACP の image content block に渡す。履歴は ACP `session/load` と host の replay port が正であり、VP の replay log から読み直さない。

## 設定と UI

`EngineSettings::Muse(MuseSettings)` は `{"muse":{"model":"…","mode":"…"}}` の形。未選択の軸は省略する。lane 作成時の model 指定もこの型へ保存する。

- spawn / load 確定後に保存済みの model → mode を `config_intent` として再適用する。
- 設定切替は Agy と共通の handler で host 応答を待つ。成功した軸だけを registry に保存し、他の保存済み軸は engine の現在値に揃える。途中で拒否された軸は保存しない。
- 別 engine の設定を Muse session に当てる要求は host handle の型確認で拒否する。
- roster の `acp_config` に候補・現在値・失敗理由を載せる。WebView は Agy のパネルを共用し、Muse では Model / Mode を表示、送信タグを `muse` にする。
- 表示名は Muse、session chip は `muse`。ロゴは `ph:meta-logo` / `ph:meta-logo-fill`（Iconify API で存在確認）。

モデルや Mode を増やすために固定の候補表を追加しない。未知の設定軸は、永続する契約を追加するまでパネルに出さない。

## 検証と実機確認

単体テストは fake ACP agent または純粋関数を使う。engine 登録、TUI の新規・再開・model・不正入力、settings の serde、ACP の新規・load 後の intent 再適用、画像 content、履歴、registry 保存と切替失敗、WebView の Model 切替と roster 反映を対象とする。

実機確認待ち: 新版を lead 側で適用後、Muse session を作り、Chat で送受信・画像・Model / Mode 切替、TUI への切替と同じ会話の resume、Chat を開き直した際の履歴と設定の維持を確認する。CLI のログイン失効や使用枠不足の表示も実環境で確認する。この lane から daemon 再起動・app 差替えは行わない。

## Status log

- 2026-11-27: Muse の TUI / ACP 接続と Model / Mode 意図を追加。実機への適用前。
