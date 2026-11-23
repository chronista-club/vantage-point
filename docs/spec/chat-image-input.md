# Chat の画像入力

Status: Codex 対応を実装・実機確認済み（2026-09-15）。agy 対応を実装・engine の実機確認済み（2026-11-17）。Grok 対応を実装・VP adapter 経由で実機確認済み（2026-11-23、GUI は実機確認待ち）

Chat の入力欄に画像を貼り付け、本文と一緒に対応エンジンへ渡せるようにする。
エンジンごとの能力表は `EngineKind::image_capable` を正本とし、session roster を通じて UI へ配信する。
Console 自体が画像を受け取れることと、VP Chat の画像対応は別の能力である。

| エンジン | Chat の画像入力 | 送信形式 |
|---|---|---|
| Claude | 対応済み | stream-json の image content block |
| Codex | 対応・実機確認済み | app-server の image input（data URL） |
| agy | 実装済み・engine の実機確認済み（Gemini 3.8 Flash。GUI は未確認、doc 88 §6.4）。host を共有 crate に替えた後（2026-11-20、doc 88 §8）の実機の確認は、共有 crate の側の E2E（`vendor/agent-chat/crates/acp/tests/agy_e2e.rs` の `e2e_config_switch_and_image_prompt`。agent-chat の lane が revision `8ec120e` の中身で回して通過、と報告）。VP の adapter 経由では回していない | ACP の image content block |
| Grok | 実装済み・VP adapter 経由で実機確認済み（grok 1.0.92 / grok-4.7、256×256 PNG、1 turn）。`e2e_real_grok_image_prompt`（普段は ignored）を 1 回実行し、赤を認識。GUI は実機確認待ち | ACP の image content block |
| OpenCode / vpcode | 未対応 | — |

Grok の事前実測（2026-11-23、grok 1.0.89、`mem_1ChEhaHVsXPkjVuqpuadCZ` の結果 C）:

- `promptCapabilities.image: true`。256×256 の赤い PNG は grok-4.7（2 回）、grok-4.7-build-fast、grok-4.6（各 1 回）で Red と回答した。
- 8×8 の赤い PNG は Grok 側で小さすぎるとして使われなかった（3 回、turn は `end_turn`、error 無し）。拒否する境目、CLI と model のどちらが拒否するかは未実測。
- grok-4.5 は同じ 256×256 画像に Orange と回答した（1 回の取り違え）。画像を送れることは、認識の正確さを保証しない。

VP は小さい画像を拡大・事前拒否しない。実機 E2E は実測済みの 256×256 PNG を使い、色を本文に書かずに質問する。

## 成功条件

- 画像対応 session の入力欄では、貼り付けた画像のプレビューと取消操作を表示する。
- 本文と添付画像を通常送信できる。現在は本文の入力を必須とする。
- Codex の「今伝える」「次に実行する」でも同じ添付を送る。画像は復号後 1 枚 5 MiB まで受け付け、ID / 本文の文字列上限を画像データへ流用しない。
- 送信失敗・結果不明時は、本文と添付を保持し、明示操作で入力欄へ戻せる。
- Codex の画像付き Queue 項目は、画像を欠落させる本文だけの編集を許さない。順序変更・取消は native Queue に委ねる。
- VP 独自のファイル・transcript へ画像データを永続化しない。送信先ハーネスによる履歴の保存は、そのハーネスの管理範囲となる。

画像のみの送信、再起動を越える添付下書きの保存、未対応エンジンへの画像転送は今回の対象に含めない。
