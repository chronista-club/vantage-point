# 89 — engine usage meter（4 エンジンのサブスク usage を machine scope で集約して表示する）

- spec: creo `mem_1ChMLKCMUvC73JHTLjG3xg` / 実測: `mem_1ChMJqaRP2SunfbmEd6h1n`
- 段: C（daemon に channel を足し、vp-app の GUI と tray にまたがる）

## 何のためか

mako は Claude Code / Codex / Gemini（`agy`）/ Grok のサブスクを併用し、残枠を見ながら
どのエンジンで作業するかを決めている。毎回それぞれの `/usage` を開く手間を無くすため、
daemon が 4 エンジンの使用率・リセット時刻・plan を**枠を消費せずに**集め、3 か所に出す:

1. sidebar 最下段の MachineStrip（`webview/src/sidebar/DaemonWidget.tsx`）
2. sub lane 作成の engine 選択（`webview/src/sidebar/AddSub.tsx`）
3. macOS の menu bar（`crates/vp-app/src/tray.rs`）

## 置き場所

usage は repo にも lane にも依らないアカウント単位の事実なので、daemon（machine scope）が 1 つ持つ。

```
crates/vantage-point/src/usage/
  model.rs    data      正規化済みの型（UsageSnapshot / EngineUsage / UsageWindow）
  parse.rs    calc      各エンジンの生 JSON → model（単位の罠はここで吸収）
  service.rs  calc+act  いつ問い合わせるか / 値の重ね方 / watch で配信 / state zone へ保存
  probe.rs    act       CLI・file に問い合わせる（trait — test は偽物を注入）
  passive.rs  act       会話中の rate limit 通知を流し込む口
```

配信は push channel `daemon-usage`（`daemon/server.rs` の `register_usage_channel`）。
`daemon-device` と同じく**接続 = 購読**で、繋いだ直後に snapshot を 1 回、以後は変わるたびに全量を
送り直す（冪等）。client は上りの `refresh` event で手動 refresh を頼む（応答は返さない —
結果は下りの snapshot で届く）。

vp-app は購読を **app に 1 本**張る（tray が app に 1 つなので）。`AppEvent::UsageUpdated` を
event loop が window へ振り分ける前に拾い、tray と全 window の `SidebarState.engine_usage` に配る。

## 取り方（副作用ガード）

どの経路も turn を起こさない（枠を 1 トークンも使わない）。

| engine | 能動（probe） | 受動 |
|---|---|---|
| claude | `~/.claude.json` の `cachedUsageUtilization` と `oauthAccount.planDisplayName` / `organizationRateLimitTier` を**読むだけ**（`CLAUDE_CONFIG_DIR` があればその下。mtime が同じなら読まない） | VP の chat（gui）の stream-json に流れる `rate_limit_event`（`conversation/host.rs` の stdout ポンプで translator の手前で覗く） |
| codex | `codex app-server` → `initialize` → `account/rateLimits/read {excludeResetCreditDetails:true}` → kill | `account/rateLimits/updated`（`conversation/codex_host.rs` の notification 分岐。sparse なので窓 id で重ねる） |
| agy | `agy -p "/usage" --output-format json` | — |
| grok | `grok agent stdio` → ACP `initialize` → `_x.ai/billing` → kill（`subscription_tier` もこの応答に入るので、メールアドレスを含む `_x.ai/auth/check_subscription` は呼ばない） | — |

- **claude は一度も spawn しない**（`claude -p` は hook が 8 本走る）。Keychain の OAuth token で
  `/api/oauth/usage` を直接叩くこともしない（refresh token の回転で CC のログインを壊しうる）
- probe の子プロセスは cwd = temp dir（project 単位の MCP / hook を拾わない）、`VP_*` などの継承 env を
  剥がし、timeout（codex / agy 25 秒、grok 45 秒）で kill する
- test は本物の CLI を起動しない（probe は trait、`DaemonState` の test 用は `UsageService::inert()`）。
  本物に当てる smoke は `#[ignore]` + env gate（`usage::probe::tests::real_cli_probe_smoke`）

### いつ問い合わせるか（`service::due_engines`）

| engine | daemon 起動時 | 定期 | 手動 Refresh |
|---|---|---|---|
| claude | file を読む | 60 秒ごと（file を読むだけなので誰も見ていなくても） | 毎回 |
| codex | spawn | 15 分ごと（購読者がいる間だけ） | 30 秒以内の連打はまとめる |
| agy | spawn | 10 分ごと（購読者がいる間だけ） | 30 秒以内の連打はまとめる |
| grok | しない | しない | 60 秒以内の連打はまとめる |

- 受動の通知が来た engine は、定期 poll の時計を進める（通知で足りている間は spawn しない）
- snapshot は `~/.local/state/vp/usage-snapshot.json`（dev は `vp-dev`）に保存し、daemon 再起動後は
  cache として出す。grok は手動の時しか問い合わせないので、これが無いと再起動のたびに `—` に戻る

### 値の重ね方

- 能動の結果は置き換える。ただし**失敗した時は前回の窓を残して** error だけ付ける（古い値でも無いよりよい）
- 取得時刻が今の値より古い結果（Claude の file cache が、受動 event で得た値より古い）は捨てる
- リセット時刻を過ぎた窓は落とす（過ぎた後の使用率は分からない — その値はもう事実でない）

## 正規化（単位の罠）

| 入力 | 使用率 | リセット |
|---|---|---|
| Claude `cachedUsageUtilization` | 0–100 | ISO 8601 |
| Claude `rate_limit_event` | **0–1** | epoch **秒** |
| Codex `account/rateLimits/*` | 0–100 | epoch **秒** |
| agy `/usage` | **残量**の 0–1 | ISO 8601 |
| Grok `_x.ai/billing` | 0–100 | ISO 8601（期間の終わり） |

→ model では `used_pct: f32`（0–100、小数 1 桁）と `resets_at_ms: Option<u64>`（epoch ms）に揃える。
severity は `< 80` normal / `>= 80` warning / `>= 95` critical。

個人情報（account id / email / team id）は parse が読む field を名指しで拾うので model に入る経路が無い
（`claude_cache_never_leaks_account_identifiers` 等で固定）。

## 代表の枠（サマリの %）

窓には `scope` がある。None = engine 全体の枠、Some = モデル別・グループ別の部分枠
（Claude の Fable 枠、agy の Claude/GPT 群、Codex の別 limit）。

**サマリの % は engine 全体の枠の最悪値**で、全体の枠が無い時だけ部分枠の最悪値に倒す。
「Fable だけ使い切った」「agy の中の Claude/GPT 枠が 0」でエンジン全体が 100% に見えると、
どのエンジンで作業するかの判断を誤らせるため。規則は Rust（`vp-app/src/usage.rs::summary_window`、
tray 用）と TS（`webview/src/sidebar/usage.ts::summaryWindow`）の 2 か所にあり、同じ形の test を持つ。

## 表示

| 面 | 畳んだ / 一目 | 詳細 |
|---|---|---|
| MachineStrip | サマリ行の右端に最も逼迫しているエンジン（`Cdx 95%`、severity で色） | 展開すると 4 エンジンのゲージ、窓ごとの % とリセットまでの時間、取得失敗の理由、「更新」ボタン |
| engine 選択 | `claude (88%)`。未取得の engine は名前だけ（fail-open） | option の tooltip に「weekly 88% 使用 · あと 1d 18h でリセット」 |
| tray | menu bar に最も逼迫しているエンジン（`Cdx 95%`。未取得なら icon だけ） | メニューに 4 行（`Codex — 95% weekly · resets 11/30 06:10`）と `Refresh Usage` |

## 未確認・後回し

- Grok の値を稼働中の ACP session から相乗りで取る経路（`vendor/agent-chat` の `AcpClientHost` に拡張
  method の口が要る = 2 repo にまたがる）
- Claude の `cachedUsageUtilization` が CC によってどれくらいの頻度で書き直されるか（未計測）
- `daemon-control` に `usage/get` を足して CLI / MCP から読む口（今は GUI だけ）
- Swift の `apple/VantagePointAgent` への表示（出荷物ではないので対象外）
