# wiremsg R3-b: cc_session_id 保持 + conductor `--resume` 化 — 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** lane ごとに CC session id を永続化し、conductor spawn を `claude --resume <保存 id>` に切り替えて `--continue` の Agent View dashboard 罠を構造的に回避する(Phase B insulate の上位互換)。`LaneInfo.cc_session_id` で可視化し、R3-c の `--bg` session 管理の土台にする。

**Architecture(収穫は hook、読み出しは CLI):** spawn 時に旧 session は死んでいて `agents --json` に出ない — そこで **R2-c で注入済みの SessionStart hook が自分の session_id(hook stdin JSON に含まれる)を state file に記録する**(`vp_state_dir()/cc_sessions/<project>__<lane>`)。echoes task は spawn 時に新設の `vp lane last-session`(env `VP_PROJECT`/`VP_LANE` から導出)で読み、id があれば `--resume '<id>' || claude`、無ければ従来の `--continue || claude`(migration: 初回記録後は --resume 経路に乗る)。`LaneInfo.cc_session_id` は GET /api/lanes 時に state file を lazy read(`performer_status` と同じ前例)。

**設計 SSOT:** `mem_1CbuxQuNRwHBiZgBVUWVfN` §user input 2(LaneInfo.cc_session_id、user 明示)+ `mem_1CbXZyCiqrdgteGhRFDaHW`(--resume = dashboard 罠回避、--bg 継続も --resume)。

**実装判断(設計範囲内):**
1. 記録は `vp wire hook-check` 内(SessionStart 時のみ)— R2-c で全 VP spawn session に注入済みの既存 hook に相乗り。プロセス追加ゼロ、fail-open 維持。wire 専用名に lane 記録が同居する点は doc コメントで明示
2. id なし時の fallback は従来 `--continue || claude` を維持(挙動不変の migration。一度 session が立てば hook が記録し、以後 `--resume` が罠を回避)
3. `--resume <id>` 失敗(session 消失・purge)時の fallback は fresh `claude`(設計メモの `--resume <保存 id> || claude` どおり)

---

### Task 0: lane 開始

- [ ] `git checkout -b mako/wiremsg-r3b-session-resume origin/nightly`(計画 stash 持ち越し)

### Task 1: `lane/cc_session.rs` — state file の read/write(TDD)

**Files:**
- Create: `crates/vantage-point/src/lane/cc_session.rs`
- Modify: `crates/vantage-point/src/lane/mod.rs`(`pub mod cc_session;`)

- [ ] **Step 1-1: 失敗するテスト**

```rust
#[test]
fn file_name_sanitizes_project_and_lane() {
    let p = session_file_in(Path::new("/base"), "creo.memories", "conductor");
    assert_eq!(p, Path::new("/base/cc_sessions/creo.memories__conductor"));
    // path separator / .. は - に潰す (traversal 防止)
    let p = session_file_in(Path::new("/base"), "a/b", "../evil");
    assert_eq!(p, Path::new("/base/cc_sessions/a-b__..-evil").to_path_buf());
}

#[test]
fn record_and_last_roundtrip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    record_in(tmp.path(), "vp", "conductor", "0196-session-id").expect("record");
    assert_eq!(
        last_in(tmp.path(), "vp", "conductor").as_deref(),
        Some("0196-session-id")
    );
    // 未記録 lane は None
    assert_eq!(last_in(tmp.path(), "vp", "w1"), None);
    // 上書き (最新が勝つ)
    record_in(tmp.path(), "vp", "conductor", "newer-id").expect("record 2");
    assert_eq!(
        last_in(tmp.path(), "vp", "conductor").as_deref(),
        Some("newer-id")
    );
}

#[test]
fn last_rejects_garbage() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // 空 / 空白のみ / 改行混入は None (壊れた file を resume に渡さない)
    std::fs::create_dir_all(tmp.path().join("cc_sessions")).unwrap();
    std::fs::write(tmp.path().join("cc_sessions/vp__conductor"), "  \n").unwrap();
    assert_eq!(last_in(tmp.path(), "vp", "conductor"), None);
}
```

注: dev-dependencies に `tempfile` が無ければ追加(workspace に既にある可能性が高い — 確認して流用)。

- [ ] **Step 1-2: 実装**

```rust
//! lane ごとの CC session id 永続化 (R3-b、 設計 mem_1CbXZyCiqrdgteGhRFDaHW)
//!
//! `claude --continue` は「cwd の最新 session」を拾うため、 background session が
//! 居ると Agent View dashboard を開いて send-keys が詰まる (CC 2.1 罠)。
//! 特定 session を `--resume <id>` で指名すれば構造的に回避できる — その id を
//! lane 単位で保持するのが本 module。
//!
//! - **書き手**: `vp wire hook-check` (SessionStart で自 session_id を記録 — spawn 時に
//!   旧 session は agents --json に出ないため、 生きているうちに自己申告させる)
//! - **読み手**: `vp lane last-session` (echoes task が spawn 時に呼ぶ) /
//!   GET /api/lanes の lazy populate (可視化)
//! - 置き場: `vp_state_dir()/cc_sessions/<project>__<lane>` (1 lane 1 file 1 行)

use std::path::{Path, PathBuf};

/// file 名に使えない文字を潰す (path traversal / separator 防止)
fn sanitize(part: &str) -> String {
    part.chars()
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect()
}

/// state base dir 配下の session file path (純関数、 テスト用に base 注入)
pub fn session_file_in(base: &Path, project: &str, lane: &str) -> PathBuf {
    base.join("cc_sessions")
        .join(format!("{}__{}", sanitize(project), sanitize(lane)))
}

/// session id を記録する (上書き、 1 行)
pub fn record_in(base: &Path, project: &str, lane: &str, session_id: &str) -> std::io::Result<()> {
    let path = session_file_in(base, project, lane);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, session_id)
}

/// 最後に記録された session id を返す。 無い / 空 / 空白のみは None。
pub fn last_in(base: &Path, project: &str, lane: &str) -> Option<String> {
    let raw = std::fs::read_to_string(session_file_in(base, project, lane)).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains(char::is_whitespace) {
        return None;
    }
    Some(trimmed.to_string())
}

/// 本番 base (vp_state_dir) での record (hook-check から呼ぶ)
pub fn record(project: &str, lane: &str, session_id: &str) -> std::io::Result<()> {
    record_in(&crate::config::vp_state_dir(), project, lane, session_id)
}

/// 本番 base (vp_state_dir) での last (`vp lane last-session` / lazy populate から呼ぶ)
pub fn last(project: &str, lane: &str) -> Option<String> {
    last_in(&crate::config::vp_state_dir(), project, lane)
}
```

注: `vp_state_dir` の re-export 経路は `crate::config`(CLAUDE.md: `vantage_point::config` は vp_paths を pub use)— 実名を確認して合わせる。

- [ ] **Step 1-3: green + Commit** — `feat(lane): cc_session — lane 単位の CC session id 永続化 (R3-b)`

### Task 2: hook-check が SessionStart で session_id を記録

**Files:**
- Modify: `crates/vantage-point/src/commands/wire.rs`(hook_check 内 + doc)

- [ ] **Step 2-1: hook_check の event 抽出部を拡張**

stdin JSON から `session_id` も取り、SessionStart のときだけ記録(best-effort):

```rust
    let parsed = serde_json::from_str::<serde_json::Value>(&input).ok();
    let event_name = parsed
        .as_ref()
        .and_then(|v| v.get("hook_event_name").and_then(|e| e.as_str()))
        .unwrap_or("UserPromptSubmit")
        .to_string();

    // ... wire_address_from_env の後 (project/lane が取れた場合のみ) ...

    // R3-b: SessionStart で自 session_id を記録する (cc_session module doc 参照)。
    // wire 通知とは独立の lane 管理だが、 全 VP spawn session に注入済みの本 hook に
    // 相乗りする (プロセス追加ゼロ)。 失敗は無視 (fail-open)。
    if event_name == "SessionStart"
        && let Some(sid) = parsed
            .as_ref()
            .and_then(|v| v.get("session_id").and_then(|s| s.as_str()))
        && let (Ok(project), Ok(lane)) = (std::env::var("VP_PROJECT"), std::env::var("VP_LANE"))
    {
        let _ = crate::lane::cc_session::record(&project, &lane, sid);
    }
```

(実装時は既存の wire_address_from_env 呼び出しと変数を整理して重複 env 読みを避ける)

- [ ] **Step 2-2: 単体 smoke**

```bash
cargo build -p vp-cli -q
echo '{"hook_event_name":"SessionStart","session_id":"r3b-test-sid"}' | VP_PROJECT=r3b-test VP_LANE=conductor ./target/debug/vp wire hook-check
cat "${XDG_STATE_HOME:-$HOME/.local/state}/vp/cc_sessions/r3b-test__conductor"  # → r3b-test-sid
```

- [ ] **Step 2-3: Commit** — `feat(wire): hook-check が SessionStart で cc session id を記録 (R3-b)`

### Task 3: `vp lane last-session` + echoes の `--resume` 化

**Files:**
- Modify: `crates/vp-cli/src/main.rs`(LaneCommands に variant + dispatch)
- Modify: `.mise/tasks/vp/stand/echoes`(conductor 分岐)

- [ ] **Step 3-1: LaneCommands に追加**

```rust
    /// この lane の最後の CC session id を表示 (R3-b、 echoes spawn の --resume 用)
    ///
    /// project / lane は flag 優先、 無ければ VP_PROJECT / VP_LANE env から導出。
    /// 未記録なら何も出力せず exit 0 (caller は空文字で fallback 判定)。
    LastSession {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        lane: Option<String>,
    },
```

dispatch:

```rust
        LaneCommands::LastSession { project, lane } => {
            let project = project.or_else(|| std::env::var("VP_PROJECT").ok());
            let lane = lane.or_else(|| std::env::var("VP_LANE").ok());
            if let (Some(p), Some(l)) = (project, lane)
                && let Some(id) = vantage_point::lane::cc_session::last(&p, &l)
            {
                println!("{id}");
            }
            Ok(())
        }
```

- [ ] **Step 3-2: echoes task の conductor 分岐を --resume 化**

```bash
if [ "${VP_LANE:-}" = "conductor" ]; then
    # R3-b: 前回 session を指名 resume する (CC 2.1 の Agent View dashboard 罠の構造的回避)。
    # id は SessionStart hook (vp wire hook-check) が記録した lane 単位の保存値。
    # 未記録 (初回 / 移行直後) は従来の --continue chain を維持 — 一度 session が立てば
    # hook が記録し、 以後この分岐は --resume 側に乗る。 resume 失敗 (session 消失) は
    # fresh claude に fallback (設計 mem_1CbXZyCiqrdgteGhRFDaHW)。
    RESUME_ID="$(vp lane last-session 2>/dev/null || true)"
    if [ -n "$RESUME_ID" ]; then
        CLAUDE_CMD="claude --resume '$RESUME_ID' --settings '$WIRE_HOOKS' || claude --settings '$WIRE_HOOKS'"
    else
        CLAUDE_CMD="claude --continue --settings '$WIRE_HOOKS' || claude --settings '$WIRE_HOOKS'"
    fi
else
    CLAUDE_CMD="claude --settings '$WIRE_HOOKS'"
fi
```

(`bash -n` + RESUME_ID に quote が混ざらない前提の確認 — session id は uuid 形式、last_in が whitespace 混入を弾く。single quote 混入だけ追加で防ぐ: last_in の検証を「英数と - のみ許可」に強化しても良い → 実装時に `chars().all(|c| c.is_ascii_alphanumeric() || c == '-')` 検証を last_in に足す)

- [ ] **Step 3-3: Commit** — `feat(lane): vp lane last-session + echoes conductor を --resume 化 (R3-b、dashboard 罠の構造的回避)`

### Task 4: `LaneInfo.cc_session_id` の可視化(lazy populate)

**Files:**
- Modify: `crates/vantage-point/src/process/lanes_state.rs`(field 追加)
- Modify: `crates/vantage-point/src/process/routes/lanes.rs`(GET 時 lazy read、performer_status と同じ位置)
- Modify: LaneInfo を構築している全箇所(`cc_session_id: None` 追加 — コンパイラに従う)

- [ ] **Step 4-1: field 追加**

```rust
    /// R3-b: この lane の最後の CC session id (state file の lazy read、 registry には保存しない)。
    /// `--resume` 再利用 (echoes) と R3-c の --bg session 管理の土台。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc_session_id: Option<String>,
```

- [ ] **Step 4-2: routes/lanes.rs の GET handler で populate**(performer_status の隣)

```rust
        lane.cc_session_id =
            crate::lane::cc_session::last(&lane.address.project_name(), &lane_label);
```

注: project 名と lane label の取得は `LaneAddress` の実 API(stand_spawner の `lane_label` と同じ導出)に合わせる。lane_label が private なら同等 logic を cc_session 側に寄せるか pub(crate) 化。

- [ ] **Step 4-3: build + 全テスト green + Commit** — `feat(lane): LaneInfo.cc_session_id を /api/lanes で可視化 (R3-b)`

### Task 5: 検証 + docs + E2E + 出荷

- [ ] fmt / clippy / test workspace green、gitnexus analyze + detect_changes(compare)
- [ ] docs/spec/02-capability.md(または lane 系 spec)に R3-b 1 行、CLAUDE.md は変更不要(lane CLI 詳細は載せていない)
- [ ] **E2E(deterministic resume の実証)**:
  1. install。detached tmux + echoes 相当 env で session A を起動 → 「合言葉は ルビコン と覚えて」と入力 → hook が state file に session A の id を記録したことを確認(`vp lane last-session`)
  2. session A を kill → echoes task の conductor 分岐そのままの CLAUDE_CMD を組んで再起動(`--resume <id>`)→ 「合言葉は？」→ **ルビコン** が返れば deterministic resume 成立
  3. `vp lane ls --detail`(SP 稼働 lane)で cc_session_id が見えることを確認
- [ ] team-b → 対応 → PR(base nightly)→ auto-merge(不発 flake に注意: checks green 確認後に直接 merge fallback)→ creo work_log + nightly 戻し + install

## Self-Review 済み

- user 明示 2 点(LaneInfo.cc_session_id / --resume 再利用)を Task 4 / Task 3 でカバー。収穫経路は設計メモの「Phase A poll」から「SessionStart hook 自己申告」に変更 — poll は死んだ session を見られないため spawn 時読みに使えず、hook の方が正確(stdin に自 session_id)。R2-c の注入済み hook に相乗りするため追加コストゼロ。この deviation は PR に明記
- `--resume` の quote 安全: last_in が whitespace を弾き、実装で英数+ハイフン検証を追加
- migration: 初回(id 未記録)は従来挙動のまま。breaking change なし
