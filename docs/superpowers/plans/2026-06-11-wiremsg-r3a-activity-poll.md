# wiremsg R3-a: CC activity poll 供給 + delivery policy 精密化 — 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `claude agents --json` を TheWorld の delivery loop に供給し、設計の policy table を精密化する — command × idle/waiting → 即 nudge、busy → 待つ(idle 遷移で nudge)、session 不在 → offline pending。poll 不能時は R2-b の degraded 挙動(Running → nudge)に自動 fallback。

**Architecture:** 新 module `process/cc_activity.rs` に「JSON parse(純関数)+ poll(I/O、5s timeout)」を置き、`DeliveryActor::pulse` が pulse ごとに poll して **cwd で lane と照合**(interactive session の `cwd` ↔ `LaneInfo.cwd`)。判定は純関数 `recipient_readiness` に隔離。`sessionId` も同時に収穫して activity に保持(R3-b の `--resume` 再利用の土台、今回は保存まではしない)。Research Preview の schema 変動リスクは「全パース防御 + 失敗 = degraded fallback」で抱える。

**Tech Stack:** Rust (Tokio, tokio::process)、claude CLI 2.1.170+(schema 実機確認済 2.1.173)

**設計 SSOT:** `mem_1CbvcJj4ppU3QKH9d7xMpT`(policy table / D4 LaneActivity)+ `mem_1CbXZyCiqrdgteGhRFDaHW`(Phase A、`agents --json` schema 裏取り)。「poll 移行(低リスク・read-only)から始めるのが筋」の実行。

**実機確認済み schema(2.1.173):** top-level は配列。interactive = `{pid, cwd, kind:"interactive", sessionId, status: idle|busy|waiting}` / background = `{cwd, kind:"background", sessionId, state: done|failed|blocked, id, name}`。

---

### Task 0: lane 開始 + impact

- [ ] `git checkout -b mako/wiremsg-r3a-activity-poll origin/nightly`(計画は stash 持ち越し)
- [ ] impact: `pulse`, `pick_tmux_session`(upstream)

### Task 1: `process/cc_activity.rs` 新設(TDD)

**Files:**
- Create: `crates/vantage-point/src/process/cc_activity.rs`
- Modify: `crates/vantage-point/src/process/mod.rs`

- [ ] **Step 1-1: 失敗するテスト(parse 純関数)**

```rust
#[test]
fn parse_extracts_interactive_sessions() {
    let json = r#"[
        {"pid":null,"cwd":"/r/nexus","kind":"background","sessionId":"bg-1","state":"blocked","id":"x","name":"t"},
        {"pid":123,"cwd":"/r/vp","kind":"interactive","sessionId":"s-1","status":"idle"},
        {"pid":124,"cwd":"/r/nexus","kind":"interactive","sessionId":"s-2","status":"busy"},
        {"pid":125,"cwd":"/r/hub","kind":"interactive","sessionId":"s-3","status":"waiting"},
        {"pid":126,"cwd":"/r/odd","kind":"interactive","sessionId":"s-4","status":"brand-new-state"}
    ]"#;
    let sessions = parse_agents_json(json).expect("parse");
    assert_eq!(sessions.len(), 4, "interactive のみ (background は対象外)");
    assert_eq!(sessions[0].cwd, "/r/vp");
    assert_eq!(sessions[0].state, CcState::Idle);
    assert_eq!(sessions[0].session_id.as_deref(), Some("s-1"));
    assert_eq!(sessions[1].state, CcState::Busy);
    assert_eq!(sessions[2].state, CcState::Waiting);
    // 未知 status は Busy 扱い (保守的 = nudge しない方に倒す)
    assert_eq!(sessions[3].state, CcState::Busy);
}

#[test]
fn parse_rejects_malformed() {
    assert!(parse_agents_json("not json").is_none());
    assert!(parse_agents_json(r#"{"agents":[]}"#).is_none()); // 配列以外は schema 変動とみなす
    assert_eq!(parse_agents_json("[]").expect("空配列は valid").len(), 0);
}

#[test]
fn activity_lookup_matches_by_cwd() {
    let sessions = vec![CcSession {
        cwd: "/r/vp".into(),
        state: CcState::Idle,
        session_id: Some("s-1".into()),
    }];
    assert_eq!(state_for_cwd(&sessions, "/r/vp"), Some(CcState::Idle));
    assert_eq!(state_for_cwd(&sessions, "/r/other"), None);
}
```

- [ ] **Step 1-2: 実装**

```rust
//! CC activity poll (R3-a / Phase A、 設計 mem_1CbXZyCiqrdgteGhRFDaHW)
//!
//! `claude agents --json` で live session を取得し、 delivery loop に
//! LaneActivity (設計 D4) を供給する。 **read-only の poll のみ** — dispatch (--bg) は
//! R3-c。 Research Preview の schema 変動は「全パース防御 + 失敗 = None」で抱え、
//! 呼び出し側 (DeliveryActor) は None を degraded mode (R2-b 挙動) に fallback する。

/// interactive session の CC 状態 (agents --json の status)
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CcState {
    Idle,
    Busy,
    Waiting,
}

/// 1 interactive session (delivery 判定 + R3-b の resume 土台に使う最小構成)
#[derive(Debug, Clone)]
pub(crate) struct CcSession {
    pub cwd: String,
    pub state: CcState,
    /// `claude --resume <id>` 再利用の収穫経路 (R3-b で LaneInfo へ保存予定)
    pub session_id: Option<String>,
}

/// agents --json の出力を parse する (純関数)。 interactive のみ返す。
///
/// 防御方針: top-level が配列でない / JSON でない → None (= schema 変動とみなし
/// degraded fallback)。 entry 単位の欠落 field は skip、 未知 status は Busy 扱い
/// (保守的 = nudge しない方に倒す。 誤 nudge より誤待機の方が無害 — 30s tick で再判定される)。
pub(crate) fn parse_agents_json(raw: &str) -> Option<Vec<CcSession>> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let arr = v.as_array()?;
    let mut out = Vec::new();
    for item in arr {
        if item.get("kind").and_then(|k| k.as_str()) != Some("interactive") {
            continue;
        }
        let Some(cwd) = item.get("cwd").and_then(|c| c.as_str()) else {
            continue;
        };
        let state = match item.get("status").and_then(|s| s.as_str()) {
            Some("idle") => CcState::Idle,
            Some("waiting") => CcState::Waiting,
            _ => CcState::Busy,
        };
        out.push(CcSession {
            cwd: cwd.to_string(),
            state,
            session_id: item
                .get("sessionId")
                .and_then(|s| s.as_str())
                .map(String::from),
        });
    }
    Some(out)
}

/// cwd で session を引く (純関数)。 同一 cwd に複数 session がある場合は
/// 最初の 1 件 (agents --json の列挙順) を使う。
pub(crate) fn state_for_cwd(sessions: &[CcSession], cwd: &str) -> Option<CcState> {
    sessions.iter().find(|s| s.cwd == cwd).map(|s| s.state)
}

/// `claude agents --json` を実行して parse する (I/O)。 あらゆる失敗は None
/// (= degraded fallback)。 hook 同様 delivery を block しないよう 5s timeout。
pub(crate) async fn poll_cc_activity() -> Option<Vec<CcSession>> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new("claude")
            .args(["agents", "--json"])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_agents_json(&String::from_utf8_lossy(&output.stdout))
}
```

- [ ] **Step 1-3: mod 宣言 + green 確認** — `pub(crate) mod cc_activity;`、`cargo test -p vantage-point --lib cc_activity` PASS (3 tests)
- [ ] **Step 1-4: Commit** — `feat(wire): cc_activity poll — agents --json の LaneActivity 供給 (R3-a / Phase A)`

### Task 2: DeliveryActor の policy 精密化(TDD)

**Files:**
- Modify: `crates/vantage-point/src/process/delivery_actor.rs`

- [ ] **Step 2-1: 失敗するテスト(readiness 純関数)**

```rust
#[test]
fn readiness_with_activity_follows_policy_table() {
    use crate::process::cc_activity::CcState;
    // activity 供給あり: idle / waiting → Ready、busy → Busy (待つ)、session 不在 → Offline
    let act = Some(Some(CcState::Idle));
    assert_eq!(recipient_readiness(true, act), Readiness::Ready);
    assert_eq!(
        recipient_readiness(true, Some(Some(CcState::Waiting))),
        Readiness::Ready
    );
    assert_eq!(
        recipient_readiness(true, Some(Some(CcState::Busy))),
        Readiness::Busy
    );
    assert_eq!(recipient_readiness(true, Some(None)), Readiness::Offline);
}

#[test]
fn readiness_degraded_falls_back_to_lane_running() {
    // poll 不能 (None): R2-b の degraded 挙動 — lane Running なら Ready
    assert_eq!(recipient_readiness(true, None), Readiness::Ready);
    assert_eq!(recipient_readiness(false, None), Readiness::Offline);
    // activity があっても lane (tmux session) が無ければ nudge 不能
    use crate::process::cc_activity::CcState;
    assert_eq!(
        recipient_readiness(false, Some(Some(CcState::Idle))),
        Readiness::Offline
    );
}
```

- [ ] **Step 2-2: 実装**

```rust
/// 受信者の配信準備状態 (設計 policy table の lane 状態軸)
#[derive(Debug, PartialEq)]
enum Readiness {
    /// idle / waiting (or degraded で Running) — 即 nudge してよい
    Ready,
    /// busy — 待つ (idle 遷移を次 pulse で拾う。 台帳は進めない)
    Busy,
    /// lane 不在 / session 不在 — pending 保持 (Phase A 後にチャネル D で配信)
    Offline,
}

/// 受信者の readiness を判定する (純関数)
///
/// - `lane_nudgeable`: lane registry で Running かつ tmux session あり (= send-keys 可能)
/// - `activity`: 外側 None = poll 不能 (degraded fallback)、
///   内側 None = poll は成功したが当該 cwd に interactive session 不在 (= offline)
fn recipient_readiness(
    lane_nudgeable: bool,
    activity: Option<Option<crate::process::cc_activity::CcState>>,
) -> Readiness {
    use crate::process::cc_activity::CcState;
    if !lane_nudgeable {
        return Readiness::Offline;
    }
    match activity {
        // degraded (R2-b 挙動): Running なら nudge
        None => Readiness::Ready,
        Some(None) => Readiness::Offline,
        Some(Some(CcState::Idle)) | Some(Some(CcState::Waiting)) => Readiness::Ready,
        Some(Some(CcState::Busy)) => Readiness::Busy,
    }
}
```

pulse の変更点:
1. 冒頭(pending 非空のとき)で `let activity = crate::process::cc_activity::poll_cc_activity().await;` を 1 回
2. lane 解決を「session 名 + cwd」を返す形に変更(`pick_tmux_session` → `pick_nudge_target(&lanes, &lane_display) -> Option<(String /*session*/, String /*cwd*/)>` に改名・拡張、既存テスト 2 本も追従)
3. 判定: `let act_view = activity.as_ref().map(|s| crate::process::cc_activity::state_for_cwd(s, &cwd)); match recipient_readiness(target.is_some(), act_view) { Ready => 既存 decide_nudge 経路, Busy | Offline => continue (台帳は進めない) }`
4. nudge ログに readiness / activity の有無を含める(degraded か精密かを後から判別できるように)

- [ ] **Step 2-3: green 確認** — `cargo test -p vantage-point --lib delivery` PASS(既存 7 + 新 2)
- [ ] **Step 2-4: Commit** — `feat(wire): delivery policy 精密化 — busy は待ち/idle・waiting は即 nudge (R3-a、policy table 完成)`

### Task 3: 検証 + docs

- [ ] fmt / clippy / test workspace green
- [ ] docs/spec/02-capability.md の delivery loop 行を更新(activity poll 供給 + busy 待ち を追記)、設計メモの D4 が実装されたことを PR 本文に記載
- [ ] gitnexus analyze + detect_changes(compare)
- [ ] Commit

### Task 4: E2E + 出荷

- [ ] **E2E**:
  1. install + restart-all
  2. `claude agents --json` で nexus conductor が idle であることを確認 → command 送信 → 数秒で nudge 着弾(Ready 経路、TheWorld ログに activity 付き判定が出る)
  3. busy 検証: nexus conductor に長めの処理を依頼して busy 化(または自 conductor=busy の状態で自分宛 command を送り、busy の間 nudge が来ず、idle 遷移後の pulse で届くことを確認)
  4. ack して停止確認
- [ ] team-b → 対応 → PR(base nightly)→ auto-merge → creo work_log + nightly 戻し + 新バイナリ

## Self-Review 済み

- policy table(設計)の command 行 3 状態(waiting/idle → C 即時、busy → 待つ、offline → pending)を `recipient_readiness` で全カバー。degraded fallback は R2-b user 確定挙動と同一
- 未知 status → Busy(nudge しない側)に倒す保守設計 — Research Preview の新 status 追加で誤 nudge しない
- `sessionId` は CcSession に収穫済みだが保存は R3-b(LaneInfo.cc_session_id + `--resume`)へ意図的 deferral
- cwd 照合は完全一致(LaneInfo.cwd と agents --json cwd は双方 claude/VP が同じ絶対 path を使う想定)。不一致が観測されたら canonicalize を検討(E2E で確認)
