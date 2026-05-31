# 11: Stand init_script system (mise task 路線)

> **Status**: completed (2026-05-03、 9 PR 連鎖で landed)。 PR-pre2 (VP-118) で `vp:stand:hd` → `vp:stand:echoes` rename、 本 doc 内 `hd` 表記は historical record として維持 (新 stand 名は `echoes`、 詳細は doc 12 §9 catalog 参照)。
> **Status (旧)**: draft v2 (2026-05-02、 mise file-based task 路線に rewrite)
> **Extends**: `mem_1CabUx6FLaRgoK2unJvk6q` (VP Lane init_script で scripted Stand entrypoint を一般化、 2026-04-29 design intent)
> **Supersedes**: 現行 `LaneStand` enum (HeavensDoor / TheHand) の二分構造、 および本 doc の v1 (Rust hard-code preset 案 ─ Section 5 Alternatives に縮約)
> **Out of scope**: 複数 Stand の同時稼働 (= split lane)、 Stand 状態 telemetry (Msgbox / discovery 側で扱う)、 任意 user script の sandboxing (single-user dev tool 前提)

---

## 0. このドキュメントの位置づけ

VP の Lane spawn 機構を **`LaneStand` enum (`HeavensDoor` / `TheHand`)** から **mise file-based task** に統合する設計を確定する。

決めること:
- Stand の実体 = mise task (`vp:stand:{name}`)、 ファイル配置 (`.mise/tasks/vp/stand/`)
- VP_* 環境変数の規約 (cwd / session / project / lane を mise task に渡す)
- task discovery (`mise tasks ls --json`) と sidebar 表示への接続
- Tier 区分 (shell / tmux / hd) と 3 preset task の中身
- per-project override のしくみ (mise の cascade を活用、 VP 側は無改修)
- wire format (HTTP API での stand 識別) の migration plan

決めないこと:
- mise が config.toml で sandbox を作る話 (single-user dev tool 前提で skip)
- 複数 Stand の同時稼働 / 動的切替
- Stand 別の特殊権限 (Msgbox や Capability 別管理は別 layer の話)

本 spec は **Phase 1 deliverable** で、 確定後 PR-A (`.mise/tasks/vp/stand/` ファイル群追加) → PR-B (VP コアを `mise run vp:stand:{name}` 呼出に切替、 enum 廃止) の 2 PR で実装する。

---

## 1. Goal / Non-goal

### Goal

1. **Stand の実体を mise file-based task に外出し** ─ `.mise/tasks/vp/stand/{name}` に各 Stand 1 ファイル、 polyglot (Bash / Ruby / Python / 任意 interpreter)、 standalone testable
2. **TH を `vp:stand:shell` に吸収** ─ `LaneStand::TheHand` 廃止、 `vp:stand:shell` task で代替
3. **既存 HD 挙動を 100% 保つ** ─ `vp:stand:hd` task が PR #250 後の HD と bit-for-bit 同等の init_script を実行
4. **per-project override を config 不要で成立** ─ mise の自然な cascade (`project_dir/.mise/tasks/vp/stand/hd` が workspace を上書き) で実現
5. **VP コア code を激減させる** ─ `Stand` struct / `LaneStandSpec` trait / `LlmStand` / `TheHand` 全削除、 残るのは `stand_name: String` と `mise run vp:stand:{name}` の呼出だけ
6. **wire format を新形式に更新**しつつ 1 release backward-compat shim を提供

### Non-goal

- mise の存在しない環境で動くこと ─ VP は dev tool、 user は mise を持っている前提
- 任意 user script の sandbox security model
- 動的 Stand 切替 (lane attach 後に Stand を変更)

---

## 2. 背景 ─ なぜ mise task に外出しするか

### 2.1 v1 design (Rust hard-code preset) の限界

v1 は `Stand` struct + preset constructor (`Stand::heavens_door()` 等) で表現していた。 これは:

- **拡張性中位**: 新 Stand を追加するたび Rust rebuild が必要
- **polyglot 不可**: init_script が Rust の `String` field、 Bash 以外を書きづらい
- **per-project override が遠い**: Phase 2 で `config.toml` から読む計画だったが、 schema 設計と実装で時間がかかる
- **standalone test が遠い**: Stand 単独で「コマンドラインから動かして確認」 が出来ない

### 2.2 mise task に外出しすると何が変わるか

| 軸 | v1 (Rust hard-code) | **v2 (mise task)** |
|---|---|---|
| 柔軟性 | preset only | **mise.toml / .mise/tasks/ edit で追加** |
| 動的編集 | rebuild 必要 | **edit + lane respawn で反映** |
| polyglot | Bash の文字列のみ | **Bash / Ruby / Python / 任意 (shebang)** |
| per-project override | Phase 2 で別 spec 必要 | **mise の cascade で自然に成立** |
| 工数 | medium | **small** (VP 側 code 激減) |
| 新依存 | 0 | **0** (mise は既存) |
| 単独 test | 不可 | **`mise run vp:stand:hd` で確認可** |
| user mental model | 新概念 (Stand struct) | **既知 (mise task)** |
| VP コア code 量 | やや減 | **大幅減** (Stand struct 不要) |

8/9 軸で v2 が勝つ。 残 1 軸 (type safety) は tradeoff だが、 「runtime で stand 名 typo を検出」 path で十分カバーできる (Section 3.4)。

### 2.3 既存 VP インフラとの整合

VP は既に mise を heavy に使っている:

- `mise.toml` の `[tasks."vp:app"]` `[tasks."vp:daemon:stop"]` `[tasks."push:mac"]` `[tasks."push:win"]`
- user の dogfood loop が `mise run vp:app` 起点 (build → stop → start 一発)
- `mise.toml` の Rust toolchain / tools 管理

つまり mise = **VP 開発のオーケストレーション層として既に確立**。 Stand 起動をここに載せるのは layer 整合的。

### 2.4 既存 design intent との整合

`mem_1CabUx6FLaRgoK2unJvk6q` (2026-04-29) の「The Hand に init_script param、 既存 HD `claude -c || claude` pattern の generalize」 intent を、 **更に進めて Stand 自体を VP の責務から外す** 終点版。 「VP は Lane の orchestration、 Stand の中身は mise が握る」 という責務分離。

---

## 3. Design

### 3.1 Stand = mise file-based task

各 Stand を `.mise/tasks/vp/stand/{name}` に **1 ファイル**として配置。 ファイル名 = Stand 名。 mise が subdirectory を `:` separator として task 名を組み立てるため、 task 名は `vp:stand:hd` / `vp:stand:shell` 等になる。

```
vantage-point/
├── .mise/
│   └── tasks/
│       └── stand/
│           ├── hd            # Bash, Heaven's Door (Claude TUI auto-launch)
│           ├── shell         # Bash, bare shell (旧 TheHand 相当)
│           ├── tmux          # Bash, tmux session attach のみ (Tier 1)
│           ├── opus.rb       # Ruby, Claude Opus 4.7 xhigh (将来例)
│           └── README.md     # 命名規則 / VP_* env 仕様 / metadata convention
├── mise.toml                 # 既存 (app, nuke, push:mac) は維持
└── crates/
    └── ...
```

各ファイルは:

- shebang で interpreter 指定 (`#!/usr/bin/env bash` / `ruby` / `python`)
- 先頭コメントで `#MISE description="..."` を mise が parse
- VP 専用 metadata は `#VP icon="..."` `#VP tier=2` で自前 parse
- 末尾に `exec tmux ...` 等で PTY を直接 take over (background 不要)

### 3.2 Tier 区分 (JoJo 演目 metaphor)

| Tier | task | init 動作 | 比喩 | 用途 |
|-------|------|-----------|------|------|
| 0 | `vp:stand:shell` | `exec $SHELL -l` のみ | 舞台の床 | shell tinkering、 一時的作業 |
| 1 | `vp:stand:tmux` | tmux server 起動 + new-session attach | 副舞台を仕込む | 監視 / 別 lane からの send-keys 受け / log tail |
| 2 | `vp:stand:hd` | tmux + claude auto-launch | 役者を呼ぶ | AI 駆動の主作業 (現 HeavensDoor) |

#### 3.2.1 `.mise/tasks/vp/stand/shell` (Tier 0)

```bash
#!/usr/bin/env bash
#MISE description="Bare login shell (旧 TheHand)"
#VP icon="🤚"
#VP tier=0

set -euo pipefail
exec "${SHELL:-/bin/zsh}" -l
```

#### 3.2.2 `.mise/tasks/vp/stand/tmux` (Tier 1)

```bash
#!/usr/bin/env bash
#MISE description="tmux session attached, no LLM (Tier 1)"
#VP icon="🎭"
#VP tier=1

set -euo pipefail

tmux start-server 2>/dev/null || true
tmux set-option -g focus-events on 2>/dev/null || true
tmux set-option -g escape-time 0 2>/dev/null || true
tmux set-option -ga terminal-overrides ',xterm-256color:Tc' 2>/dev/null || true

exec tmux new-session -A -c "$VP_CWD" -s "$VP_SESSION"
```

#### 3.2.3 `.mise/tasks/vp/stand/hd` (Tier 2、 現 HeavensDoor 相当)

```bash
#!/usr/bin/env bash
#MISE description="Heaven's Door — Claude TUI auto-launch with tmux 副舞台"
#VP icon="📖"
#VP tier=2

set -euo pipefail

tmux start-server 2>/dev/null || true
tmux set-option -g focus-events on 2>/dev/null || true
tmux set-option -g escape-time 0 2>/dev/null || true
tmux set-option -ga terminal-overrides ',xterm-256color:Tc' 2>/dev/null || true

# tmux 副舞台 + claude auto-launch (--continue → 失敗時新規 session)
# tmux 不在環境では外側 fallback で素 claude に降格
exec tmux new-session -A -c "$VP_CWD" -s "$VP_SESSION" \
    'claude --continue || claude' \
    || (claude --continue || claude)
```

これは PR #244 / #250 で確定した init_script を **bit-for-bit 同等**に shell file 化したもの。

#### 3.2.4 将来例 `.mise/tasks/vp/stand/opus.rb`

```ruby
#!/usr/bin/env ruby
#MISE description="Claude with Opus 4.7 xhigh thinking"
#VP icon="🧠"
#VP tier=2

cwd     = ENV.fetch('VP_CWD')
session = ENV.fetch('VP_SESSION')
model   = ENV['CLAUDE_MODEL'] || 'claude-opus-47-xhigh'
xhigh   = ENV['CC_XHIGH'] == '1'

claude_args = [
  "--model #{model}",
  ("--thinking-effort xhigh" if xhigh),
].compact.join(' ')

exec %Q{tmux new-session -A -c "#{cwd}" -s "#{session}" "claude #{claude_args} || claude"}
```

Ruby 採用例 ─ env や条件分岐で動的に args 組立、 Bash の `[ -z ]` 判定地獄を避ける。

### 3.3 ENV 規約 (VP → mise task)

VP は Stand 起動時に以下の環境変数を mise task に渡す:

| ENV | 意味 | 例 |
|---|---|---|
| `VP_CWD` | Lane の working directory (project_dir) | `/Users/makoto/repos/vantage-point` |
| `VP_SESSION` | tmux session 名 (sanitize 済) | `vp-vantage-point-lead-hd` |
| `VP_PROJECT` | project 識別子 | `vantage-point` |
| `VP_LANE` | lane label (lead / worker name) | `lead` / `sub` |

quoting 規約: task 内では `"$VP_CWD"` のように **必ず double-quote**で囲む (空白や特殊文字対応)。 single-quote の中に `"$VP_*"` を埋める tmux command の場合は `"..."` で外側を組み立てて `'...'` で内 cmd を括る pattern (preset の `vp:stand:hd` 参照)。

### 3.4 task discovery

VP は起動時 / sidebar 表示時に `mise tasks ls --json` を呼び出して `vp:stand:` prefix の task を列挙:

```rust
pub fn list_available_stands(project_dir: &Path) -> Result<Vec<StandInfo>> {
    let output = std::process::Command::new("mise")
        .args(["tasks", "ls", "--json"])
        .current_dir(project_dir)
        .output()?;
    let tasks: Vec<MiseTask> = serde_json::from_slice(&output.stdout)?;
    Ok(tasks.into_iter()
        .filter(|t| t.name.starts_with("vp:stand:"))
        .map(|t| StandInfo {
            name: t.name.strip_prefix("vp:stand:").unwrap().to_string(),
            description: t.description.clone(),
            // file path から #VP icon=... を grep で抽出 (optional)
            icon: extract_vp_icon(&t.file).unwrap_or_else(|| default_icon(&t.name)),
        })
        .collect())
}
```

mise の cascade 効果で、 project_dir で実行すれば自動的に project-local Stand も含まれる。

### 3.5 spawn 経路

`crates/vantage-point/src/process/stand.rs` (新規 module、 `stand_spec.rs` 廃止):

```rust
pub fn build_stand_command(
    stand_name: &str,
    addr: &LaneAddress,
    project_dir: &Path,
) -> StandCommand {
    let session = addr.tmux_session_name(stand_name);
    StandCommand {
        program: "mise".into(),
        args: vec!["run".into(), format!("vp:stand:{}", stand_name)],
        env: vec![
            ("VP_CWD".into(), project_dir.to_string_lossy().into()),
            ("VP_SESSION".into(), session),
            ("VP_PROJECT".into(), addr.project.clone()),
            ("VP_LANE".into(), addr.lane_label_str().into()),
        ],
        // initial_input は不要 ─ mise task が直接 PTY を take over する
    }
}
```

`PtySlot::spawn` は cwd 引数で project_dir を渡すよう変更 (mise が cascade を効かせるため)。

### 3.6 tmux session 命名

PR #245 で確立した規則を継続:

```
vp-{project}-{lane_label}-{stand_name}
```

`stand_name` は task 名 `vp:stand:{name}` の `name` 部分そのまま。 例: `vp-vantage-point-lead-hd`。

`LaneAddress::tmux_session_name(stand_name: &str)` の signature に変更 (`&LaneStand` → `&str`):

```rust
pub fn tmux_session_name(&self, stand_name: &str) -> String {
    let lane_label = match (self.kind, self.worker_name.as_deref()) {
        (LaneKind::Lead, _) => "lead",
        (LaneKind::Worker, Some(n)) => n,
        (LaneKind::Worker, None) => "unnamed",
    };
    sanitize(&format!("vp-{}-{}-{}", self.project, lane_label, stand_name))
}
```

### 3.7 wire format

#### output (新形式)

```json
{"stand": "hd"}
```

stand_name そのものを文字列で渡す。 `Stand` struct も struct 内 metadata も wire には含めない (icon / description は VP が `mise tasks ls --json` で別経路で取得)。

#### input (legacy shim 削除済 2026-05-03)

> **2026-05-03 改訂**: 当初は 1 release 期間の deprecation shim として `"heavens_door"` → `"hd"` / `"the_hand"` → `"shell"` の `migrate_legacy_stand` 関数を accept していたが、 **VP は user 1 人 + ccws worker のみで vp-app + daemon が常に同 binary で deploy される構成のため、 外部 client が旧 wire format で来る window が実質ゼロ**と判断、 PR #257 と同 day に shim を削除した。
>
> wire 上は新 stand 名 (`hd` / `shell` / `tmux` / 任意 `vp:stand:*` task 名) のみ accept、 旧名は **400 Bad Request** または default fallback (= `config.default_stand_or_hd()` = "hd") に乗る。

新 wire format で `stand` field は task 名そのまま:

```json
{"kind": "worker", "name": "feat-api", "stand": "hd"}
{"kind": "worker", "name": "feat-api", "stand": "shell"}
{"kind": "worker", "name": "feat-api", "stand": "opus-xhigh"}  // user 定義 stand
```

`stand` 省略時は server-side で `config.default_stand_or_hd()` (config 未設定なら `"hd"`) が適用される。

### 3.8 per-project override

> **PR-D (2026-05-03 改訂)**: 旧設計の filesystem cascade ベース override は破綻、 VP_PROJECT env による explicit dispatch に移行。

#### 旧設計の問題 (PR-D 以前)

mise の filesystem-tree cascade を活用、 VP 側は無改修で動作する想定だった:

```
~/repos/creo-memories/.mise/tasks/vp/stand/hd  ← project local override
~/repos/vantage-point/.mise/tasks/vp/stand/hd  ← workspace default
~/.config/mise/tasks/vp/stand/hd               ← global fallback (任意)
```

しかし dogfood で発覚: **mise の cascade は cwd → `/` の上方向のみ**、 sibling project 間 (vantage-point と creo-memories) は見えない。 vantage-point ローカルの task は他 project の cwd からは discover されず、 `mise ERROR no task vp:stand:hd found` で spawn 失敗した (PR #257 dogfood、 2026-05-02)。

#### PR-D (Z 系統): VP install root を spawn cwd に固定

VP は `crate::process::install_root::locate_install_root()` で **install root** (= `.mise/tasks/vp/stand/` の住処) を runtime 解決し、 `mise run vp:stand:{name}` の **spawn cwd を install root に固定**する。 user の project dir は env `VP_CWD` で task に渡され、 task script が `tmux new-session -c "$VP_CWD"` 等で runtime cwd を補正する。

```
binary 配置 → install root の解決
─────────────────────────────────────
target/release/vp        → walk-up で repo root
~/.cargo/bin/vp          → fallback (env / CARGO_MANIFEST_DIR)
.app/Contents/MacOS/vp   → .app/Contents/Resources/ (.dmg 配布想定)
env VP_INSTALL_ROOT      → 直接指定 (test / 配布 packaging script 用)
```

#### per-project override は VP_PROJECT で explicit dispatch

cascade による override は廃止、 各 task script 内で `case "$VP_PROJECT" in` 分岐を書く方式に移行。

例: creo-memories project の HD lane で rails console を併走したい場合 ─

```bash
# .mise/tasks/vp/stand/hd (vantage-point install root)
case "$VP_PROJECT" in
  creo-memories)
    exec tmux new-session -A -c "$VP_CWD" -s "$VP_SESSION" \
      'rails c | claude --continue || claude'
    ;;
  *)
    exec tmux new-session -A -c "$VP_CWD" -s "$VP_SESSION" \
      'claude --continue || claude'
    ;;
esac
```

trade-off: cascade の暗黙的優先順位を捨てる代わりに、 dispatch logic が **1 ファイルに集約**されて読みやすい / debug しやすい。 install root は single source of truth、 `~/.config/mise/tasks/` への展開や user による project local 設置等の install state divergence が発生しない。

---

## 4. Implementation plan

### 4.1 PR 分割

PR-A と PR-B を分ける理由 = 1 PR = 1 仮説原則。 task ファイル群の正しさ検証 (PR-A) と VP コア切替 (PR-B) を分けて regression を切り分け可能に。

#### PR-A: `.mise/tasks/vp/stand/` ファイル群追加 (VP コア未改修)

| File | Change |
|------|--------|
| `.mise/tasks/vp/stand/hd` (新規) | Tier 2 preset (HD 相当) |
| `.mise/tasks/vp/stand/shell` (新規) | Tier 0 preset (TH 相当) |
| `.mise/tasks/vp/stand/tmux` (新規) | Tier 1 preset (新規) |
| `.mise/tasks/vp/stand/README.md` (新規) | 命名規則 / VP_* env 仕様 / `#VP` metadata convention |
| `mise.toml` の `[tasks_dir]` 等 | 必要なら指定 (mise default で `.mise/tasks/` を読むはずなので変更不要が見込み) |

dogfood verification: PR-A merge 後、 user の terminal で `mise run vp:stand:hd` を実行 → 期待通り tmux + claude が起動するか確認。 VP コア未改修なので既存 HD lane は影響なし。

#### PR-B: VP コアを `mise run vp:stand:{name}` 呼出に切替

| File | Change |
|------|--------|
| `crates/vantage-point/src/process/stand.rs` (新規) | `build_stand_command` を mise 呼出 form に |
| `crates/vantage-point/src/process/stand_spec.rs` | **削除** |
| `crates/vantage-point/src/process/stand_spawner.rs` | `build_stand_command(stand_name: &str, ...)` の signature 変更、 旧 `LaneStand` 引数を文字列に |
| `crates/vantage-point/src/process/lanes_state.rs` | `LaneStand` enum **削除**、 `LaneInfo.stand: String`、 `tmux_session_name(&str)` |
| `crates/vantage-point/src/daemon/pty_slot.rs` 等 | `StandCommand` に `env: Vec<(String, String)>` field 追加、 `PtySlot::spawn_with_env` 経路 |
| HTTP API (`routes/lanes.rs`) | wire 形式は新 stand 名直接 accept (legacy shim は 2026-05-03 削除済) |
| vp-app sidebar (lanes JSON 受領側) | stand を文字列で扱う、 icon は `mise tasks ls --json` 経由で別取得 (新 endpoint `/api/stands` 検討) |
| 既存 test 全 rewrite | enum 経由 → 文字列 stand_name 経由に |

#### PR-C (任意): sidebar に「使える Stand」 dropdown

| File | Change |
|------|--------|
| 新 endpoint `GET /api/stands` | `list_available_stands(project_dir)` の結果を返す、 cache TTL 30s |
| vp-app sidebar | `+ Add Worker` 押下時に dropdown で 利用可能 Stand を選択 |

これは UX 改善で、 PR-B 後の追加 PR。 PR-B 単独でも CLI / 既存 wire format から stand 指定で動く。

### 4.2 Test plan

| Tier | Test |
|-------|------|
| **standalone** | `mise run vp:stand:hd` を user の terminal で実行 → tmux + claude が起動するか |
| **standalone** | `mise run vp:stand:shell` → bare shell 起動 |
| **standalone** | `mise run vp:stand:tmux` → tmux session に attach のみ、 claude 起動なし |
| **integration (VP)** | `build_stand_command("hd", ...)` の `StandCommand.program == "mise"` / `args == ["run", "vp:stand:hd"]` |
| **integration (VP)** | spawn 後、 PTY 内に tmux session が立ち、 claude が auto-launch (PR #244 / #250 と同等動作) |
| **integration (VP)** | `tmux_session_name("hd")` が `vp-{project}-{lane}-hd` 形式 |
| **wire compat** | `{"stand": "heavens_door"}` (legacy) を deserialize → "hd" に migrate、 deprecation log |
| **wire compat** | `{"stand": "the_hand"}` → "shell" に migrate |
| **discovery** | `list_available_stands(project_dir)` が `vp:stand:` prefix の mise task のみ返す |
| **per-project** | project_dir に `.mise/tasks/vp/stand/hd` を置いて override できる (workspace のは効かない) |
| **regression** | dogfood で 1 週間、 既存 HD lane 体験に変化なし |

### 4.3 Migration timeline

| 日付 | Item |
|------|------|
| 2026-05-02 | design doc v2 確定 (本 doc) |
| 2026-05-03 | PR-A (`.mise/tasks/vp/stand/` ファイル群) merge |
| 2026-05-04 | dogfood (`mise run vp:stand:hd` 単独で動作確認) |
| 2026-05-05〜06 | PR-B (VP コア切替 + enum 廃止 + wire shim) |
| 2026-05-07〜13 | dogfood 1 週間 (regression check) |
| 2026-05-15 | deprecation log 削除 (legacy wire format reject) |
| 2026-05-20+ | PR-C (sidebar dropdown) ─ 任意 |

---

## 5. Alternatives Considered

### A1: Rust hard-code preset (本 doc v1)

`Stand { name, short_name, icon, init_script: Option<InitScript> }` struct + preset constructor。 詳細は本 doc v1 参照 (git history で取得可)。

- Pro: type-safe、 速い、 Phase 2 で config 化 path はある
- Con: rebuild 必要、 polyglot 不可、 standalone test 不可、 per-project override に Phase 2 spec が必要
- **却下**: mise task path が 8/9 軸で勝つ (Section 2.2)

### A2: profiles.kdl (kdl crate を使った config 路線)

`profiles.kdl` で Stand を declarative 定義、 VP が起動時に読み込んで構築。 KDL crate は既に VP に入っている。

- Pro: declarative、 type に近い safety、 hot-reloadable
- Con: KDL schema 設計が必要、 polyglot 不可 (template only)、 mise 既存 infra を活用できない
- **却下**: mise task が同じ柔軟性を提供しつつ既存 infra 活用 + polyglot

### A3: Embedded Ruby DSL (mruby / magnus)

`stands/heavens_door.rb` で Ruby DSL を書き、 VP が embedded ruby で eval。

- Pro: 完全動的、 conditional 自然
- Con: mruby / magnus 等の embedded ruby 依存追加、 binary size 増、 起動 cost 増、 学習 / debug コスト
- **却下**: mise task で Ruby script を直接書ける ─ 同じ表現力を「embedded ruby なし」 で実現できる

### A4: 全部 String (lookup table)

`stand: String` を VP 内 lookup table で resolve。 init_script は内部 hash map に。

- Pro: 最もシンプル
- Con: lookup table が散逸、 user-defined 困難、 polyglot 不可
- **却下**: mise task が同じシンプルさでより柔軟

**採用**: **mise file-based task** (`.mise/tasks/vp/stand/{name}`、 本 doc v2 の design)。

---

## 6. Open Questions

| Q | 候補 | 暫定方針 |
|---|------|----------|
| `#VP icon=...` の parse 方法 | regex grep / sidecar `.toml` / VP 内 default lookup | regex grep を Phase 1、 default lookup を fallback。 sidecar は不要 |
| icon の default lookup | hd→📖 / shell→🤚 / tmux→🎭 / 不明→🎬 | このマッピングを stand.rs に const で持つ |
| mise binary 不在環境 | error / fallback / install 案内 | error + 「mise install をお願い」 のみ。 VP は dev tool なので user 側責務 |
| `mise tasks ls --json` の cache | 毎回呼ぶ / 30s cache / file watch | 30s cache (lazy invalidate)、 sidebar refresh で明示的更新可 |
| Mac App Store 配布時の mise | bundle / require / postinstall check | postinstall check (mise が PATH にあるか確認、 なければ案内) |
| stand 名の名前空間 / 衝突 | `vp:stand:` prefix で十分 / namespace 必要 | `vp:stand:` prefix のみで OK、 衝突は user の責任 |
| project-local stand の discovery | 既に mise cascade で OK | 改修不要、 動作確認のみ |

---

## 7. Decision log

- 2026-05-02 v1: TH 削除提案を user 発案、 ultrathink で評価して合意。 Rust hard-code preset 路線で design doc v1 起草。
- 2026-05-02 v2: user が「mise task で良くないか?」 と提案。 ultrathink で再評価、 8/9 軸で v2 が勝つことを確認 (Section 2.2)。 design doc を v2 に rewrite。
- 2026-05-02: 構造化方法は **mise file-based task** (`.mise/tasks/vp/stand/{name}`) を採用。 polyglot + per-project cascade + standalone test の 3 利点が決定打。
- 2026-05-02: namespace は **`vp:stand:{name}`** 採用 (中間案)。 user 提案の `vp:hd` (短い) と元案 `stand:hd` (semantic 明示) の両取り。 将来 `vp:lane:*` `vp:debug:*` 等 sub-namespace を追加できる拡張性を確保、 規約的に組める。
- 2026-05-02: PR 分割 (PR-A task ファイル群 / PR-B VP コア切替) を採用、 1 PR = 1 仮説原則。
- 2026-05-02: wire format shim 1 release を採用、 external user 影響軽微だが defensive に。

---

## 8. 規約 ─ 将来の `vp:*` namespace 展開

`vp:stand:*` を起点に、 VP の周辺 task を `vp:*` namespace で順次整理していける素地を作る。 Phase 1 では `vp:stand:*` のみ着手、 後続は機が熟したものから個別 PR で。

| namespace | 用途 | 例 |
|---|---|---|
| `vp:stand:*` | Lane で発動する Stand (本 doc) | `vp:stand:hd` / `vp:stand:shell` / `vp:stand:tmux` / `vp:stand:opus` |
| `vp:app:*` | GUI (vp-app: Rust + wry + xterm.js + creo-ui) | `vp:app:build` / `vp:app:start` / `vp:app:stop` / `vp:app` (build + stop + start alias) |
| `world:*` | TheWorld daemon = vp-cli (TheWorld + 全 CLI) | `world:build` / `world:start` / `world:stop` (cascade: SP+tmux) / `world` / `world:reset:sp` (SP sniper kill) — 2026-05-30 確定 |
| `vp:lane:*` | Lane orchestration (起動/停止/list) | `vp:lane:list` / `vp:lane:kill` |
| `vp:dev:*` | 開発 helper | `vp:dev:fmt` / `vp:dev:test` / `vp:dev:bench` |
| `vp:debug:*` | デバッグ task | `vp:debug:tmux-ls` / `vp:debug:capture` |
| `vp:release:*` | release / ship 関連 (既存 push:mac/win を移行候補) | `vp:release:mac` / `vp:release:notes` |

cascade vs sniper の使い分け:
- `world:stop` (cascade kill) ─ daemon + 背負ってる subsystem 全部 (SP + Lane tmux session) を一括停止。 「VP 開発環境シャットダウン」 用。
- `world:reset:sp` (sniper kill) ─ SP だけ叩き直す。 daemon は維持 (再起動が重い: TheWorld registry / SurrealDB / QUIC channel reinit) ので、 binary 入替後の re-register / ghost SP cleanup 用。

`vp:` 1 段の root namespace で grep 可能 (`mise tasks ls | grep '^vp:'` で VP 関連 task 全列挙)。 これは **VP の裏 CLI** として、 既存 Rust CLI (compiled) と並ぶ scripted layer になる。

## 関連 memory / PR / doc

- `mem_1CabUx6FLaRgoK2unJvk6q` ─ VP Lane init_script 一般化 (2026-04-29 design intent)
- `mem_1CaSmvKgsX2AQxRYFYgNM3` ─ Lead pane shell (TheHand path) の現在仕様
- `mem_1CaTpCQH8iLJ2PasRcPjHv` ─ Architecture v4: Process recursive、 9 component minimum
- PR #245 ─ tmux session 命名 `vp-{project}-{lane}-{stand_short}` 規則確立
- PR #244 ─ tmux new-session に `-c {cwd}` 追加 (cwd 継承罠回避)
- PR #250 ─ xterm.js v5.5.0 → v6.0.0 update
- doc 10 (`docs/design/10-kdl-log-spec.md`) ─ KDL log emission spec、 KDL を VP で扱う前例
