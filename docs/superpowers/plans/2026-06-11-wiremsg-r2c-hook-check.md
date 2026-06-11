# wiremsg R2-c: hook 注入 (チャネル B、`vp wire hook-check`) — 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** VP が spawn する claude session に wire 未読通知 hook を注入し、会話境界 (SessionStart / UserPromptSubmit) で未読在庫を additionalContext として届ける (チャネル B)。

**Architecture:** hook 実体は `vp wire hook-check` subcommand (配布物不要、決定 D2)。stdin の hook JSON から event 名を読み、`VP_PROJECT` / `VP_LANE` env (echoes task が tmux -e で per-session 注入済み) から自 wire address を導出し、TheWorld `/api/wire/unread-count` を直叩き (2s timeout)。未読 > 0 なら `hookSpecificOutput.additionalContext` を stdout に出す。**あらゆる失敗は silent 成功 (exit 0、出力なし) = fail-open** — 会話を邪魔しない。注入は echoes task の `CLAUDE_CMD` に `--settings '<hooks JSON>'` を足すだけ (`--settings <file-or-json>` は inline JSON 受理を確認済み)。

**設計 SSOT:** `mem_1CbvcJj4ppU3QKH9d7xMpT` (R2 設計確定)。R2-a の CLI primitives の薄い wrapper になる (改訂順序の狙い)。

---

### Task 0: lane 開始

- [ ] `git fetch origin nightly && git checkout -b mako/wiremsg-r2c-hook-check origin/nightly` (計画ファイルは stash で持ち越し)

### Task 1: `vp wire hook-check` subcommand (TDD)

**Files:**
- Modify: `crates/vantage-point/src/commands/wire.rs`

- [ ] **Step 1-1: 純関数 2 つのテストを書く (失敗確認)**

```rust
/// R2-c: VP_PROJECT / VP_LANE から自 wire address を導出
#[test]
fn hook_address_from_env_values() {
    assert_eq!(
        wire_address_from_env(Some("vp"), Some("conductor")).as_deref(),
        Some("agent@vp")
    );
    assert_eq!(
        wire_address_from_env(Some("vp"), Some("w1")).as_deref(),
        Some("agent@vp/w1")
    );
    // env 不足 = VP 外で起動された claude → None (fail-open)
    assert_eq!(wire_address_from_env(None, Some("conductor")), None);
    assert_eq!(wire_address_from_env(Some("vp"), None), None);
    assert_eq!(wire_address_from_env(Some(""), Some("conductor")), None);
}

/// R2-c: 未読ありのときだけ hookSpecificOutput JSON を作る
#[test]
fn hook_output_only_when_unread() {
    assert_eq!(build_hook_output("SessionStart", 0), None);
    let out = build_hook_output("UserPromptSubmit", 3).expect("3 件未読なら出力");
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext");
    assert!(ctx.contains("3 件"), "件数を含む: {ctx}");
    assert!(ctx.contains("wire_recv"), "受信導線を含む: {ctx}");
}
```

- [ ] **Step 1-2: 実装**

WireCommands に variant (hidden ではなく通常 subcommand、hook から `vp wire hook-check` で起動):

```rust
/// claude hook 実体 (R2-c、チャネル B): stdin の hook JSON を読み、未読 wire が
/// あれば additionalContext を stdout に出す。あらゆる失敗は silent 成功 (fail-open)
HookCheck,
```

実装関数 (純関数 + I/O 分離):

```rust
/// VP_PROJECT / VP_LANE の値から自 wire address を導出する (純関数)
///
/// conductor → `agent@<project>`、performer → `agent@<project>/<name>`。
/// env 不足/空 = VP 外で起動された claude → None (hook は何もしない)。
fn wire_address_from_env(project: Option<&str>, lane: Option<&str>) -> Option<String> {
    let project = project.filter(|s| !s.is_empty())?;
    let lane = lane.filter(|s| !s.is_empty())?;
    if lane == "conductor" {
        Some(format!("agent@{project}"))
    } else {
        Some(format!("agent@{project}/{lane}"))
    }
}

/// 未読件数から hookSpecificOutput JSON を作る (純関数)。未読 0 は None (出力なし)。
fn build_hook_output(event_name: &str, total: u64) -> Option<String> {
    if total == 0 {
        return None;
    }
    let context = format!(
        "📬 wire: {total} 件未読。 mcp__vantage-point__wire_recv で受信してください \
         (command category は処理後に mcp__vantage-point__wire_ack)。"
    );
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": context,
            }
        })
        .to_string(),
    )
}

/// hook 実体: 全エラー path で Ok(()) を返し何も出力しない (fail-open、会話を邪魔しない)
async fn hook_check() -> Result<()> {
    // stdin の hook JSON から event 名を取る (parse 失敗は fail-open)
    let mut input = String::new();
    use std::io::Read;
    let _ = std::io::stdin().read_to_string(&mut input);
    let event_name = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| {
            v.get("hook_event_name")
                .and_then(|e| e.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "UserPromptSubmit".to_string());

    let Some(agent) = wire_address_from_env(
        std::env::var("VP_PROJECT").ok().as_deref(),
        std::env::var("VP_LANE").ok().as_deref(),
    ) else {
        return Ok(()); // VP 外で起動された claude — 何もしない
    };

    // TheWorld 直叩き (qualified address なので proxy 不要)。hook は会話を block
    // しないよう短い timeout。daemon 不在等は silent 成功。
    let world_port = crate::config::Config::load()
        .map(|c| c.port_layout().world_port)
        .unwrap_or(crate::cli::WORLD_PORT);
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    let resp = client
        .post(format!(
            "http://127.0.0.1:{world_port}/api/wire/unread-count"
        ))
        .json(&serde_json::json!({ "agent": agent }))
        .send()
        .await;
    let total = match resp {
        Ok(r) => r
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("total").and_then(|t| t.as_u64()))
            .unwrap_or(0),
        Err(_) => return Ok(()), // daemon 不在 — silent 成功 (fail-open)
    };

    if let Some(out) = build_hook_output(&event_name, total) {
        println!("{out}");
    }
    Ok(())
}
```

match arm: `WireCommands::HookCheck => hook_check().await,`

- [ ] **Step 1-3: green 確認 + 手動 smoke**

```bash
cargo test -p vantage-point --lib commands::wire
# 手動: 未読を作って hook-check を env 付きで叩く
cargo run -p vp-cli -- wire send -t agent@hook-e2e -b 'test' -f tester@e2e
echo '{"hook_event_name":"SessionStart"}' | VP_PROJECT=hook-e2e VP_LANE=conductor cargo run -p vp-cli -- wire hook-check
# 期待: {"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"📬 wire: 1 件未読。..."}}
echo '{}' | VP_PROJECT=no-such VP_LANE=conductor cargo run -p vp-cli -- wire hook-check
# 期待: 出力なし exit 0
```

- [ ] **Step 1-4: Commit** — `feat(wire): vp wire hook-check — 会話境界の未読通知 hook 実体 (R2-c、チャネル B)`

### Task 2: echoes task に hooks 注入 (決定 D2)

**Files:**
- Modify: `.mise/tasks/vp/stand/echoes` (CLAUDE_CMD 組み立て部)

- [ ] **Step 2-1: CLAUDE_CMD に --settings を追加**

```bash
# R2-c (チャネル B、決定 D2): wire 未読通知 hook を VP が spawn 時に注入する。
# dotfile 非依存で箱から動く (#512/#515 と同じ「VP が自前担保」哲学)。
# hook 実体は `vp wire hook-check` (vp CLI 同梱、配布物不要)。会話境界
# (SessionStart / UserPromptSubmit) で未読 wire を additionalContext 通知し、
# daemon 不在やエラー時は silent 成功 (fail-open、会話を邪魔しない)。
# JSON に single quote が無いため '$VAR' 埋め込みで安全に quote できる。
WIRE_HOOKS='{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"vp wire hook-check"}]}],"UserPromptSubmit":[{"hooks":[{"type":"command","command":"vp wire hook-check"}]}]}}'
if [ "${VP_LANE:-}" = "conductor" ]; then
    CLAUDE_CMD="claude --continue --settings '$WIRE_HOOKS' || claude --settings '$WIRE_HOOKS'"
else
    CLAUDE_CMD="claude --settings '$WIRE_HOOKS'"
fi
```

(既存の CC 2.1 insulate コメントは維持し、CLAUDE_CMD 行だけ差し替え)

- [ ] **Step 2-2: quote 検証** — `bash -n .mise/tasks/vp/stand/echoes` + `echo "$CLAUDE_CMD"` 目視
- [ ] **Step 2-3: Commit** — `feat(wire): echoes spawn に wire hook を --settings 注入 (R2-c、決定 D2)`

### Task 3: 検証 + docs

- [ ] `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets` / `cargo test --workspace` green
- [ ] CLAUDE.md の wire CLI 行に hook-check 追記、docs/spec/02-capability.md に R2-c チェック行追加
- [ ] gitnexus analyze + detect_changes (compare origin/nightly)
- [ ] Commit

### Task 4: E2E + 出荷

- [ ] **E2E**: `cargo install --path crates/vp-cli`。(a) hook-check 単体: 未読あり/なし/env なしの 3 path を実バイナリで確認。(b) 実 spawn: `vp lane new` 等で performer を 1 本立て、tmux capture で claude が正常起動していること (--settings が起動を壊さない) を確認。spawn された session 内で hook が効くことは session の文脈注入なので、(a) の単体検証 + 起動無事で十分とする
- [ ] team-b (moody-blues) レビュー → 対応 → PR (base nightly) → auto-merge → creo work_log + nightly 戻し

## Self-Review 済み

- 設計メモの R2-c 仕様 (stdin hook JSON / VP_PROJECT・VP_LANE 導出 / TheWorld unread-count / fail-open) を全て反映
- `wire_address_from_env` の lane 値は echoes task の `lane_label` (conductor / performer 名 / unnamed) と一致 — unnamed は `agent@<project>/unnamed` になり R2-b の delivery 側 display 変換とも整合
- 注入は echoes のみ (shell / tmux stand は claude を起動しないので対象外)
