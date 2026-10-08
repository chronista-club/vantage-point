# 機材ごとの MIDI 使用権

VP の常駐 MIDI I/O は midistage の protocol / profiles / Rust SDK を利用する。
物理ポートの所有は midistaged が行い、VP は現在の lease 専用の仮想ポートだけを開く。
設定・所有権・送信の正本は midistage repository の `schemas/midistage.kdl` と `docs/design/03-runtime-service.md`。

## 使用操作

Devices 面で「MIDI を有効にする」を押し、対応機材の「使用」を切り替える。
通常表示は Midistage の機材単位の一覧に統一する。OS の生ポート名と IN/OUT は「接続ポート詳細」に畳み、そこに旧直接接続の使用状態を重ねない。
別アプリの担当なら切り替え確認が出る。確認した revision を送り、確認後の所有者変更を上書きしない。
現在の VP の操作対象は ROTO / LPD8 / X-Touch / nanoKONTROL2。nanoKONTROL2 は標準 CC モード（ch1）の入力を扱い、ノブ・フェーダーは pane 比率、S ボタンは Scene 呼び出し／長押し保存に対応する。M/R・transport ボタンと nano の LED フィードバックは未対応。対応していない操作面は一覧に残して切り替えを無効にする。
CLI は同じ daemon-control の窓口を使う。

```sh
vp midi devices
vp midi on
vp midi use roto on --revision 3 --takeover
vp midi use roto off --revision 4
vp midi off
```

revision は `vp midi devices` の最新応答から取得する。`--takeover` は他アプリからの変更を明示的に承認するときだけ付ける。
全体 OFF は保存済みの機材割り当てを保持し、`vp-monitor` として状態だけを読む。全体 ON で `vp` の担当を再開する。
サービスが接続できないときは I/O を停止し、物理ポートへフォールバックしない。

## 解放順序

入力許可表を先に更新し、旧 listener の abort と join、ROTO セッションの停止・join、旧 output の破棄を終えてから Quiesced を返す。
別機材の listener とアプリ固有の lane / scene 割り当ては保持する。
service は物理送信 completion と後始末が終わるまで次の lease を与えない。

`devices/midi-use` の状態応答は `connected` / `enabled` / `snapshot` と、サービス状態を示す nullable な `error` を持つ。この `error` は RPC 失敗ではない。GUI はこの method の状態応答に限ってデータとして受け取り、通常の `{"error": ...}` だけの RPC 失敗（revision 不一致など）はエラーとして扱う。

新窓口 `devices/midi-use` は既存 `devices/midi` と同様、人の GUI/CLI 操作用であり MCP discovery には公開しない。
既存の前景実機診断コマンド（`vp midi roto demo` など）はこの常駐経路の移行対象外。共有サービス稼働中に直接実行しない。

## 検証境界

入力許可と解放順序のテスト、GUI の takeover 確認・revision 保持・全体 OFF・定期更新のテストを行う。
ブラウザでは機材を開かない fixture で表示と操作を観察する。これを物理 MIDI や稼働中 VP の実証とは扱わない。
共通 service 起動・Ladyland 反映・共有 VP daemon 更新後の音/LED/画面/往復切り替えは実機確認待ち。

### 2026-10-07 実装時の記録

- Rust の対象テスト31件（device registry 20、CLI/handler 2、所有ポート制限2、daemon KDL 7）を確認。
- WebView の対象テスト11件、TypeScript 型検査、WebView bundle、`vp-app` のビルドを確認。
- `vp-cli` のビルドと `vp midi use --help`、`cargo clippy -p vantage-point -p vp-app --all-targets -- -D warnings` を確認。
- ブラウザ fixture で takeover 確認と全体 OFF の設定保持表示を観察。定期更新で操作中の DOM を置き換えないこともテストで固定。
- midistage SDK は `0d3c3f5e22dac62c0e701daaa17fcd173f41df18` に固定。

稼働環境への反映には `CLAUDE.md` の server 更新手順が必要。`VP_SWAP_RESTART_DAEMON=1` は共有 daemon と各 repo/lane のセッションに影響するため、通常のビルド検証と分けて実施する。
