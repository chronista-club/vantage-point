# 29. Windows 対応 — vp-app GUI までの実装プラン

> status: **plan**(2026-07-03 策定、実装 handoff 用)
> goal: **Windows で vp-app (GUI) が起動し、プロジェクトを開いて Echoes(claude)コンソール + Canvas が使える**
> 前提 branch: `nightly`(#503 で Windows build/test は通過済)
> 戦略上の位置づけ: Tier 2(community-supported、`docs/decisions/2026-04-19-strategy-summary.md`)だが、dogfooding 実機(Windows 11)で体験を磨くため一次対応する

---

## 0. 実機で確認済みの前提(2026-07-03、mito の Windows 11 機)

| 項目 | 実測値 | 含意 |
|------|--------|------|
| claude | `C:\Users\mito\.local\bin\claude.exe`(native installer) | `%USERPROFILE%\.local\bin` が PATH prefix に必要 |
| git-bash | `C:\Program Files\Git\bin\bash.exe` **あり** | stand script の実行エンジンに使える |
| PATH 上の bash | `...\WindowsApps\bash.exe`(**WSL stub**) | 素の `bash` 解決は WSL に化ける。System32/WindowsApps 除外が必須 |
| mise | winget 版 `mise.exe`、shims = `%LOCALAPPDATA%\mise\shims` あり | mise shims の Windows path は macOS と異なる |
| tmux | **なし** | tmux 経路は fallback(直起動 claude)に落とす |
| `HOME` | **未設定**(`USERPROFILE=C:\Users\mito`) | `std::env::var("HOME")` 依存コードは全て Windows で None になる |
| vp-paths | `dirs::home_dir()` 使用 → `C:\Users\mito\.config\vp\` 等に解決 | **変更不要**。XDG 統一方針が Windows でもそのまま機能する |

## 1. 診断 — 起動チェーンと破壊点

```
vp app start → vp-app(wry/WebView2) → daemon spawn → SP spawn → lane/stand spawn → claude
     ✅              ✅(要実機確認)        ⚠️ W1        ⚠️ W1        ❌ W2 + ⚠️ W3      ⚠️ W3
```

| ID | 破壊点 | ファイル | 症状 |
|----|--------|---------|------|
| **W1** | PATH 補強が Unix 専用 | `crates/vantage-point/src/spawn_env.rs:44`(SSOT)+ `crates/vp-app/src/daemon_launcher.rs:189`(レプリカ) | `:` 区切り hard-code のため Windows の `C:\...;D:\...` を `:` で split → **壊れた PATH を全 spawn chain に注入**。さらに prefix が `/opt/homebrew` 等 Unix 専用、`HOME` 未設定で user prefix が全滅 → claude.exe が見つからない |
| **W2** | stand script の shebang 直 exec | `crates/vantage-point/src/process/stand_spawner.rs:272-286` | `.mise/tasks/vp/stand/echoes`(bash script)を program として `PtySlot::spawn` に渡す。Windows の CreateProcess は shebang を解さず **spawn 失敗 → lane 即 Dead** |
| **W3** | echoes script 内の Unix 前提 | `.mise/tasks/vp/stand/echoes` | ① `LOGIN_SHELL="${SHELL:-/bin/zsh}"` — git-bash に `/bin/zsh` は無い ② tmux 不在(ただし **rc≠0 → 素 claude 直起動 fallback が既設計**。tmux 代替の実装は初期ゴールに不要) |
| W4 | 実機未検証領域 | ConPTY DSR / WebView2 / CJK / 入力二重化 | コードは cross-platform 実装済(portable-pty / wry)。Phase W2 の dogfood で潰す |

**#503 までに解決済み(再実装不要)**: `vp app start/stop`(commands/app.rs)、daemon_launcher の `CREATE_NEW_PROCESS_GROUP|DETACHED_PROCESS` 分岐、platform.rs(process alive/terminate/kill)、shell_detect.rs(git-bash 優先 + WSL 除外)、screenshot/windows.rs(PrintWindow+GDI)、file_watcher(テスト OS 分岐済)、vp-paths(dirs::home_dir が USERPROFILE を見る)。

---

## 2. Phase W1 — spawn chain のコード修正(Echoes コンソール到達)

### Task 1: `spawn_env::augment_path` の Windows 対応 【最優先・PATH 破壊の根治】

対象: [crates/vantage-point/src/spawn_env.rs](../../crates/vantage-point/src/spawn_env.rs)

- **区切り文字**: `:` hard-code(L45/52/53)を OS 別に。テスト容易性のため、純関数は separator を内部 cfg 分岐ではなく **`augment_path_with(base, home, sep, prefixes)` のような注入形**にして両 OS のロジックを同一バイナリでテストできる形を推奨(既存 5 テストは macOS 期待値なので Unix separator で維持)。
- **home 解決**: `augmented_spawn_path()` の `std::env::var("HOME")`(L63)を `dirs::home_dir()` ベースに(Windows で USERPROFILE を拾う)。`vantage-point` は `dirs` 依存済み。
- **Windows prefix セット**(`user_tool_prefixes` の cfg(windows) 版、優先順):
  1. `{home}\.local\bin` — claude native installer(実機確認済)
  2. `{LOCALAPPDATA}\mise\shims` — winget mise(実機確認済)
  3. `{home}\.cargo\bin` — vp 自身
  4. (homebrew 系 prefix は Windows では出さない)
- **pty_slot.rs L87** の `std::env::var("HOME")` も同じく home_dir ベースに揃える。

### Task 2: レプリカ解消 — `augment_path` を `vp-paths` へ移動 【推奨】

対象: [crates/vp-app/src/daemon_launcher.rs:172-220](../../crates/vp-app/src/daemon_launcher.rs)

`daemon_launcher.rs` は「crate 境界の都合で複製、変更時は両者を同期」と明記された手動同期レプリカ。今回両方を書き換える必要があり、かつ **`vp-paths` はまさに『vantage-point と vp-app の共有 SSOT』のために抽出された crate**(#506)なので、`augment_path` / `utf8_locale` を `vp-paths::spawn_env` に移してレプリカを削除する。
- `vantage_point::spawn_env` は `pub use vp_paths::spawn_env::*` で re-export(config.rs の前例と同じ手法)
- `vp-paths` に `dirs` 依存を追加(軽量なので方針違反なし)
- scope を絞るなら「両ファイル同期パッチ」でも可だが、三たび drift する未来が見えるので移動を推奨

### Task 3: `build_stand_command` — stand script を bash.exe 経由で exec

対象: [crates/vantage-point/src/process/stand_spawner.rs:272-299](../../crates/vantage-point/src/process/stand_spawner.rs)

- **Windows**: `program = <git-bash path>`, `args = [<script path>]` に組み替える(Unix は現行どおり script 直 exec)。git-bash は `C:\...` 形式の引数を MSYS path に自動変換するので script path はそのまま渡してよい。
- **git-bash 検出 helper**: `vp-app/src/shell_detect.rs` の git-bash 検出ロジック(標準 install path 2 候補 → PATH 内 bash.exe から System32/**WindowsApps** 除外)を **`vp-paths` に移して共有**(Task 2 と同じ理由。vp-app 側は re-export or 呼び替え)。
  - ⚠️ 現行 shell_detect の除外は `\windows\system32\` のみ。実機では `WindowsApps\bash.exe`(WSL stub)が PATH に居るので **`\windowsapps\` の除外を追加**すること。
- **git-bash 不在時**: stand spawn を親切なエラーで即 fail(「Git for Windows を入れてください」)。pwsh native な stand script は Phase W3 以降。
- **mise fallback 経路**(L290-297、install root 解決失敗時)も Windows では `mise.exe run ...` が同じ shebang 問題を踏まない(mise 自身が interpreter を解決する)ため現状維持で可。
- **adopt 経路**(`crate::tmux::session_exists`、stand_spawner L169)が Windows で必ず false を返す(= tmux socket を探しに行って即 false / panic しない)ことを確認。必要なら `cfg(windows)` で早期 return false。

### Task 4: echoes / shell / tmux script の git-bash 耐性

対象: [.mise/tasks/vp/stand/echoes](../../.mise/tasks/vp/stand/echoes) ほか

- `LOGIN_SHELL="${SHELL:-/bin/zsh}"` → `/bin/zsh` 不在環境(git-bash)を考慮した fallback chain に:
  `LOGIN_SHELL="${SHELL:-$(command -v zsh || command -v bash || echo /bin/sh)}"` 程度で十分。
- tmux 不在: 既存 fallback(`tmux_rc≠0 → cd $VP_CWD && eval $CLAUDE_CMD → exec $LOGIN_SHELL -l`)がそのまま効く想定。**変更不要の見込みだが、git-bash 上で `tmux` コマンド不在が rc≠0(127)で fallback に到達することを実機確認**。
- `vp lane last-session` / `vp wire hook-check` は PATH に vp.exe が居れば git-bash が `.exe` を自動解決する(Task 1 の `.cargo\bin` prefix で担保)。
- `shell` / `tmux` stand も同様に確認(shell は `exec $SHELL -l` 系のはず → git-bash で bash に落ちれば OK)。

### Task 5: Windows で `cargo test --workspace` green + clippy

- Task 1 の新テスト(Windows separator / prefix / home 解決)を含め全 green。
- `cargo clippy --workspace --all-targets` も Windows で通す(CI 昇格は W3 だが手元では常に確認)。

**W1 完了条件**: Windows 上で `cargo test --workspace` green、かつコードレビューで「spawn chain 全経路(daemon spawn / SP spawn / PtySlot)が Windows で壊れた PATH を注入しない」ことを説明できる。

---

## 3. Phase W2 — 実機 dogfood(この Windows 11 機)

ビルド → 起動 → 観察 → 修正のループ。`VP_PROFILE=dev` で state を分離して回す。

```powershell
cargo build --release -p vp-app; cargo build --release -p vp-cli
$env:VP_PROFILE = "dev"; $env:VP_APP_BIN = "target\release\vp-app.exe"
target\release\vp.exe app start
```

### 検証チェックリスト(観点別)

| # | 観点 | 期待 | 既知リスク |
|---|------|------|-----------|
| 1 | vp-app window 表示 | wry+tao で WebView2 window が出る | WebView2 Runtime 不在機(Win11 は標準搭載で低リスク)。web-bundle 読込 |
| 2 | daemon 自動起動 | vp-app が TheWorld(dev=32100)を spawn | daemon_launcher は実装済。log: `~/.local/state/vp-dev/log/` |
| 3 | プロジェクト登録・選択 | projects.kdl 読み書き、SP spawn(portless) | パスの `\` 正規化(`normalize_path_key`)の挙動 |
| 4 | **Echoes コンソール** | lane spawn → git-bash → (tmux fallback) → claude.exe が PTY で起動 | **最重要**。W1 の成果検証 |
| 5 | ConPTY 描画 | xterm.js に claude TUI が描画される | pty_slot.rs L310 コメント: ConPTY は DSR(`\x1b[6n`)応答が来るまで描画を止める。xterm.js が自動応答するか。NG なら term_attach/ws 層で DSR 応答を仕込む |
| 6 | キー入力 | 入力が通る、二重化(`a`→`aa`)しない | `VP_TERM_TRACE=1` で診断可能 |
| 7 | CJK 表示 | 日本語が `_` 化しない | tmux 非経由なので macOS の CJK 問題は踏まない見込み |
| 8 | Canvas / Paisley Park | WebView 内 Canvas 表示 | — |
| 9 | `vp app stop` | taskkill /F で停止 | graceful でない(既知、W3) |
| 10 | `vp shot` | PrintWindow で PNG 取れる | 実装済・未実測 |

各修正は小さく commit し、症状と根因を commit message に残す(このプロジェクトの流儀)。

---

## 4. Phase W3 — 品質・永続化(GUI ゴール達成後の後続)

初期ゴール外(W3-1 のみ **W2 直後の優先タスク**に格上げ、2026-07-03 決定)。他は着手前に優先度を再ヒアリングする。

1. **send-keys native 化【優先、W2 直後】**: メッセージング(wire nudge / handoff / delegation wake)の tmux 依存は tmux_actor 経由の `tmux_send_keys` dispatch **1 点に集約**されている。しかも `delivery_actor.rs:25` が「チャネル C(tmux 直)は native channels (A) へ移行予定のつなぎ」と明記済み — つまり既定路線の native 移行を **Windows で先行実施**する。lane の PTY は daemon(PtySlot)が直接所有しているので、send-keys 相当(literal text 投入 + Enter 別送)は `PtySlot::write` 2 回で同型に実装できる。macOS にも還元される。
   - 代替案(本線にしない): **MSYS2 tmux spike** — `pacman -S tmux` で入るが、Cygwin PTY 上の native claude.exe は TUI が壊れやすく(winpty が生まれた理由)、PTY 二重変換で DSR/リサイズ/CJK リスクが倍増。試すなら 30 分で見切る。 / **WSL backend** — 完全な Linux tmux だが claude・repo が WSL 側に住む前提の別アーキテクチャ(旧 `docs/archive/vp-app-hd-bridge.md` の復活)。escape hatch として保留。
   - 前提事実: tmux の native Windows port は存在しない(fork/Unix PTY/Unix socket 依存)。brew は Windows 非対応、winget/scoop/choco にも tmux は無い。
2. **session 永続化**: tmux 相当が Windows に無い。選択肢 = (a) detach 無しを受容(lane 再起動で `--resume` chain が既にある) / (b) WSL tmux / (c) daemon 側 PTY を生かしたまま re-attach する native 実装。**推奨 (a) → (c)**。claude 側の `--resume`/`--continue` が会話の永続化を既に担っているため、(a) でも体験の核は保てる。W3-1(send-keys native 化)と (c) は同じ「daemon = tmux の役割」路線で地続き。
3. **daemon 常駐化**: macOS LaunchAgent(`daemon/process.rs` の cfg(target_os="macos") 群)相当を Task Scheduler(`schtasks`)or スタートアップ登録で。
4. **tray**: `vp-app/src/tray.rs` の Windows 動作確認(tao の tray は Windows 対応)。
5. **graceful shutdown**: `vp app stop` を WM_CLOSE 送信に(taskkill /F は最終手段へ降格)。
6. **CI 昇格**: `.github/workflows/ci.yml` の check-windows(現状 main push 時の cargo check のみ)を PR でも実行 + clippy + test に拡張。
7. **配布: winget を本命に**(= brew cask の Windows 対応物、user 方針 2026-07-03: 「インストール・アプデは基本 brew cask 経由にしたい」)。
   | 役割 | Mac(現行) | Windows(等価) |
   |------|------------|----------------|
   | VP 本体 | `brew install --cask vantage-point` / `brew upgrade` | winget package(manifest 自動更新を release フローに組込、`winget-releaser` 等。release:mac → release:cask と同型) |
   | 前提ツール git-bash | — | `winget install Git.Git`(公式 manifest あり) |
   | claude | native installer | native installer(自己アップデート内蔵) |

   当面は `cargo install --path crates/vp-cli`(開発者向け)、winget manifest は release 方針決定後に整備。
8. **lane worktree の symlink**: `lane/commands.rs` の `symlink()` は Windows で Developer Mode / 管理者権限が必要。performer lane 作成が失敗する場合は junction fallback を検討。

---

## 5. 設計判断(handoff 先へ: 実装前に確認 or この推奨で進める)

| # | 論点 | 推奨 | 代替 |
|---|------|------|------|
| D1 | augment_path の SSOT 化 | **vp-paths へ移動**(#506 の思想に一致、手動同期レプリカを解消) | 両ファイル同期パッチ(最小 diff だが drift 再発) |
| D2 | Windows の shell 依存 | **git-bash 必須**(検出失敗時は明示エラー)。stand script 資産をそのまま生かす | pwsh native stand script(W3 以降の選択肢として保留) |
| D3 | tmux 代替 | **初期ゴールでは作らない**。echoes script の既存 fallback(素 claude 直起動)に乗る | — |
| D4 | dogfood profile | `VP_PROFILE=dev` で回す(port 32100 / `vp-dev` dir に分離) | — |
| D5 | メッセージングの Windows 対応(2026-07-03 決定) | **tmux を持ち込まず send-keys を native 化**(W3-1、`PtySlot::write`)。チャネル C → native 移行の既定路線を Windows で先行 | MSYS2 tmux spike(30 分見切り)/ WSL backend(escape hatch) |
| D6 | Windows 配布チャネル(2026-07-03 決定) | **winget を brew cask の対応物として本命に**(W3-7)。user 方針「インストール・アプデは基本 brew cask 経由」の Windows 翻訳 | scoop / installer 直配布 |

## 6. 実装順(依存関係)

```
Task 2(vp-paths へ spawn_env + git-bash 検出を移設)
  └→ Task 1(augment_path Windows 対応 + テスト)   ← D1 を先に決めると手戻りゼロ
  └→ Task 3(stand_spawner bash.exe 経由 exec)
        └→ Task 4(echoes script 耐性)
              └→ Task 5(test/clippy green)
                    └→ Phase W2 dogfood ループ
```

Task 1/3 は独立に見えるが、どちらも「git-bash / prefix の Windows 検出」を vp-paths に置く前提なので Task 2 を先頭に。

## 7. リスクと不確実性(高い順)

1. **ConPTY × xterm.js の DSR / 描画相性**(W2-#5) — 唯一コードで先回りできない未知数。pty_slot のテストは `\x1b[1;1R` 手動応答で通している。実機で詰まったらここが本丸。
2. **claude.exe の TUI が ConPTY 上で正しく動くか** — Windows Terminal 上では動作実績あり(このセッション自体が証拠)なので低〜中。
3. git worktree symlink 権限(performer lane) — conductor lane だけなら踏まない。
4. パス正規化(`normalize_path_key` の `\` / drive letter / 大文字小文字) — projects HashMap キーの同一性。W2 で観察。
