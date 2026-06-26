//! Agent 委譲 (delegation) — durable cross-agent future の v1 ローカル atom。
//!
//! doc 28 (`docs/design/28-agent-delegation.md`) §4 の atom を、最小往復で実機に接地する spike。
//! dogfood #1「A lane が実装中に B lane へ追加実装を委譲 → 完了報告 → block 解除 → 再開」を、
//! 人間の介入なしで自走させる。**v1 はローカルのみ**（same World / same SP / 2 lane）、federation
//! （hub / World 間）は別 Phase。ただし location-transparent に作る（不変条件 ↓）。
//!
//! ## state machine（nostos Bracket(enter/Active/exit) + 三相 Outcome）
//! ```text
//! delegate(doer, task) ─enter→ [Pending] ─wake doer→ [Active]
//!     [Active] ─doer complete(Done{result})    → [Done]   ─wake requester→ requester 再開
//!     [Active] ─doer complete(Failed{reason})   → [Failed] ─wake requester→ requester 判断
//!     [Active] ─doer complete(NeedsInput{q})    → [AwaitingResponse] ─wake requester→ A が質問を見る
//!     [AwaitingResponse] ─requester respond(ans) → [Active] ─wake doer→ B が回答付きで再開（loop）
//! ```
//! nostos 三相 Done / Reborn / Failed を完全実装（Reborn = NeedsInput、`respond` で Active へ loop）。
//! pull-hook・reconcile・timeout・wire-store backing・federation は doc 28 §7 staging の follow-up。
//!
//! ## federation 不変条件（v1 で焼き込む）
//! - record の `requester` / `doer` は**論理 wire address**（`agent@<project>` /
//!   `agent@<project>/<name>`）で持つ。raw tmux session は焼き込まない。
//! - wake は `address → (local lane session)` の resolution を介す（[`lane_query_for`] +
//!   [`AppState::nudge_lane`]）。後で `world-handle:` 接頭の remote 分岐を足すだけで federation 化
//!   できる（local は退化形）。

use serde::{Deserialize, Serialize};

use super::state::AppState;

/// 委譲 1 件の record（in-memory SP store のエントリ）。
///
/// `requester` / `doer` は論理 wire address（federation 不変条件）。spike では wire-store backing
/// を持たず `AppState.delegations` の in-memory map に置く（durable 化は follow-up）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Delegation {
    /// 委譲 id（`dlg-<uuid>`）。complete / respond が参照する future handle。
    pub id: String,
    /// 委譲を出した側 = 完了で wake される await 主。論理 wire address。
    pub requester: String,
    /// 委譲を受けた側 = delegate で wake される doer。論理 wire address。
    pub doer: String,
    /// 委譲タスクの内容（doer/requester 双方の wake prompt に同梱する最小文脈）。
    pub task: String,
    /// lifecycle 状態。
    pub state: DelegationState,
    /// 確定した Outcome（未確定なら None）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

/// 委譲の lifecycle 状態（nostos Bracket の spike 版縮約）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DelegationState {
    /// record 作成済、doer 未 wake。
    Pending,
    /// doer を wake 済、完了待ち（= requester が await 中）。
    Active,
    /// doer が NeedsInput で問い返した状態（= doer が await 中、requester の `respond` 待ち）。
    /// `respond` で Active へ戻る（nostos Reborn のループ点）。
    AwaitingResponse,
    /// doer が Done で完了。
    Done,
    /// doer が Failed で完了。
    Failed,
}

/// 委譲の Outcome（nostos 三相 Done / Reborn / Failed）。
///
/// serde は `{"kind":"done","result":"..."}` / `{"kind":"failed","reason":"..."}` /
/// `{"kind":"needsinput","question":"..."}` に写す（MCP `complete` tool が組み立てる wire shape）。
/// NeedsInput は終端でなく、requester の `respond` で Active に戻る会話の 1 手（Failed も「死」でなく
/// 交渉、NeedsInput は「進んだがもう 1 周」= Reborn）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum Outcome {
    /// タスク成功。`result` = requester に引き継ぐ結果の要約。
    Done { result: String },
    /// タスク失敗。`reason` = 失敗理由（requester が判断する材料）。
    Failed { reason: String },
    /// 進めるのに requester の入力が要る（nostos Reborn）。`question` = doer が requester に聞きたいこと。
    /// 非終端: requester が `respond(answer)` で回答すると doer が再 wake され Active に戻る。
    NeedsInput { question: String },
}

/// 論理 wire address（`agent@...`）を、[`AppState::resolve_lane_session`] が解する
/// lane address query（`<project>/conductor` / `<project>/performer/<name>`）に翻訳する。
///
/// これが resolution の **local 分岐**（= federation 不変条件の swappable 層）。後で
/// `world-handle:` 接頭を見て remote World に振る分岐を足すだけで federation 化できる。
/// `delivery_actor::lane_identity_from_agent` と同じ wire→lane 分解則:
/// - `agent@<project>`        → `<project>/conductor`
/// - `agent@<project>/<name>` → `<project>/performer/<name>`
///
/// 既に bare lane form で渡された場合（`<project>/conductor` 等）は素通しする
/// （probe や test が lane address を直接撃てるように、tolerant に受ける）。
pub(crate) fn lane_query_for(addr: &str) -> String {
    let rest = addr.strip_prefix("agent@").unwrap_or(addr);
    match rest.split_once('/') {
        // 既に lane form（conductor / performer/... / 旧 lead / wing）なら素通し。
        Some((_, tail))
            if tail == "conductor"
                || tail == "lead"
                || tail.starts_with("performer/")
                || tail.starts_with("wing/") =>
        {
            rest.to_string()
        }
        // `agent@<project>/<name>` → performer lane。
        Some((project, name)) => format!("{project}/performer/{name}"),
        // `agent@<project>` → conductor lane。
        None => format!("{rest}/conductor"),
    }
}

/// doer を起こす wake prompt（doc 28 §3-3 = 単体で行動できる resumable continuation）。
///
/// context を失っていても「何が起きた / 何が期待 / 続きの在処（= complete の撃ち方）」が
/// 自己完結する 1 行 packet。改行は入れない（send-keys が Enter を別送するため、本文中の
/// 改行はプロンプト途中送信を招く）。
fn doer_wake_prompt(id: &str, requester: &str, task: &str) -> String {
    format!(
        "[委譲 {id}] {requester} からタスクを委譲されました: {task} — 完了したら \
         mcp__vantage-point__complete(id=\"{id}\", outcome=\"done\", result=\"<結果の要約>\") で \
         報告してください (失敗時は outcome=\"failed\", result=\"<失敗理由>\"、進めるのに確認が要る \
         ときは outcome=\"needs_input\", result=\"<質問>\" で requester に聞けます)。"
    )
}

/// requester を起こす wake prompt（Outcome 同梱の resumable continuation）。
fn requester_wake_prompt(id: &str, doer: &str, task: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Done { result } => format!(
            "[委譲 {id} → Done] {doer} に委譲したタスク「{task}」が完了しました。結果: {result}。\
             委譲待ちで中断していた作業を再開してください。"
        ),
        Outcome::Failed { reason } => format!(
            "[委譲 {id} → Failed] {doer} に委譲したタスク「{task}」が失敗しました。理由: {reason}。\
             再委譲・自分で実施・スコープ変更などを判断してください。"
        ),
        Outcome::NeedsInput { question } => format!(
            "[委譲 {id} ← 質問] {doer} がタスク「{task}」を進めるのに確認したいことがあります: {question} \
             — mcp__vantage-point__respond(id=\"{id}\", answer=\"<回答>\") で回答すると doer が再開します。"
        ),
    }
}

/// doer を再び起こす wake prompt（requester の `respond` の answer 同梱）。
///
/// NeedsInput で問い返した doer に、requester の回答を注入して Active に戻す（loop の continuation）。
fn respond_wake_prompt(id: &str, task: &str, answer: &str) -> String {
    format!(
        "[委譲 {id} → 回答] あなたが聞いた件への回答: {answer} — タスク「{task}」の続きを進め、\
         完了したら mcp__vantage-point__complete(id=\"{id}\", outcome=\"done\", result=\"<結果>\") で \
         報告してください。"
    )
}

/// `delegate(doer, task, requester) → {id}`（doc 28 §4 / 動詞 1）。
///
/// id 採番 → record(Pending) を store → doer を wake → Active に遷移して `{id}` を返す。
/// wake 失敗（doer lane 不在 / tmux 不在）は graceful に握る（record は durable、取りこぼしは
/// reconcile / pull-hook が後で拾う = follow-up）。`woke` で wake 成否を返す。
pub(crate) async fn handle_delegate(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let doer = payload
        .get("doer")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("delegate: 'doer' required")?
        .to_string();
    let task = payload
        .get("task")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("delegate: 'task' required")?
        .to_string();
    let requester = payload
        .get("requester")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("delegate: 'requester' required")?
        .to_string();

    let id = format!("dlg-{}", uuid::Uuid::new_v4());

    // record(Pending) を store。requester / doer は論理 wire address のまま保持。
    {
        let mut map = state.delegations.write().await;
        map.insert(
            id.clone(),
            Delegation {
                id: id.clone(),
                requester: requester.clone(),
                doer: doer.clone(),
                task: task.clone(),
                state: DelegationState::Pending,
                outcome: None,
            },
        );
    }

    // doer を wake（resumable continuation を送る）。
    let prompt = doer_wake_prompt(&id, &requester, &task);
    let woke = state.nudge_lane(&doer, &prompt).await;

    // wake 成否に関わらず Active に遷移（durable record の await を登録した状態）。
    {
        let mut map = state.delegations.write().await;
        if let Some(d) = map.get_mut(&id) {
            d.state = DelegationState::Active;
        }
    }

    state.send_debug(
        "agent",
        &format!("delegate {id}: {requester} → {doer} (woke={woke})"),
        None,
    );

    Ok(serde_json::json!({ "id": id, "state": "active", "woke": woke }))
}

/// `complete(id, outcome)`（doc 28 §4 / 動詞 2）。
///
/// record を Done / Failed に更新 → requester を wake（Outcome 同梱）。未知 id は Err。
/// requester の wake 失敗は graceful（`woke` で返す）。
pub(crate) async fn handle_complete(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("complete: 'id' required")?
        .to_string();
    let outcome: Outcome = payload
        .get("outcome")
        .cloned()
        .ok_or_else(|| "complete: 'outcome' required".to_string())
        .and_then(|v| {
            serde_json::from_value(v).map_err(|e| format!("complete: invalid outcome ({e})"))
        })?;

    // record を更新（未知 id は Err）。wake prompt に requester / doer / task が要る。
    let (requester, doer, task, new_state) = {
        let mut map = state.delegations.write().await;
        let d = map
            .get_mut(&id)
            .ok_or_else(|| format!("complete: unknown delegation id: {id}"))?;
        let new_state = match &outcome {
            Outcome::Done { .. } => DelegationState::Done,
            Outcome::Failed { .. } => DelegationState::Failed,
            // NeedsInput は非終端: doer が await に入り、requester の respond を待つ。
            Outcome::NeedsInput { .. } => DelegationState::AwaitingResponse,
        };
        d.state = new_state;
        d.outcome = Some(outcome.clone());
        (
            d.requester.clone(),
            d.doer.clone(),
            d.task.clone(),
            new_state,
        )
    };

    // requester を wake（Outcome を同梱した resumable continuation。NeedsInput は質問を届ける）。
    let prompt = requester_wake_prompt(&id, &doer, &task, &outcome);
    let woke = state.nudge_lane(&requester, &prompt).await;

    let state_str = match new_state {
        DelegationState::Done => "done",
        DelegationState::Failed => "failed",
        DelegationState::AwaitingResponse => "awaiting_response",
        // delegate→Active 経由でしか complete に来ないので Pending/Active は不到達。
        _ => "active",
    };

    state.send_debug(
        "agent",
        &format!("complete {id}: {state_str} → wake {requester} (woke={woke})"),
        None,
    );

    Ok(serde_json::json!({ "id": id, "state": state_str, "woke": woke }))
}

/// `respond(id, answer)`（doc 28 §4 / 動詞 3）。
///
/// NeedsInput(=Reborn) で問い返した doer に requester の回答を注入 → doer を再 wake、Active に戻す。
/// 未知 id は Err。状態が AwaitingResponse でないときも回答自体は届けて Active に戻す（lenient: 例えば
/// push 取りこぼし後の手動 respond でも前進させる。厳密な遷移ガードは reconcile/state 機 follow-up）。
/// doer の wake 失敗は graceful（`woke` で返す）。
pub(crate) async fn handle_respond(
    state: &AppState,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("respond: 'id' required")?
        .to_string();
    let answer = payload
        .get("answer")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("respond: 'answer' required")?
        .to_string();

    // record を Active に戻す（未知 id は Err）。doer の再 wake に task が要る。
    let (doer, task) = {
        let mut map = state.delegations.write().await;
        let d = map
            .get_mut(&id)
            .ok_or_else(|| format!("respond: unknown delegation id: {id}"))?;
        d.state = DelegationState::Active;
        // NeedsInput の outcome は回答で消費される（次の complete まで未確定）。
        d.outcome = None;
        (d.doer.clone(), d.task.clone())
    };

    // doer を再 wake（answer 同梱の continuation）。
    let prompt = respond_wake_prompt(&id, &task, &answer);
    let woke = state.nudge_lane(&doer, &prompt).await;

    state.send_debug(
        "agent",
        &format!("respond {id}: → wake {doer} (woke={woke})"),
        None,
    );

    Ok(serde_json::json!({ "id": id, "state": "active", "woke": woke }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_query_wire_conductor_to_lane() {
        assert_eq!(lane_query_for("agent@vp"), "vp/conductor");
    }

    #[test]
    fn lane_query_wire_performer_to_lane() {
        assert_eq!(lane_query_for("agent@vp/feat-api"), "vp/performer/feat-api");
    }

    #[test]
    fn lane_query_bare_lane_form_passthrough() {
        // 既に lane form のものは翻訳せず素通し（probe / test が直接撃てる）。
        assert_eq!(lane_query_for("vp/conductor"), "vp/conductor");
        assert_eq!(lane_query_for("vp/performer/x"), "vp/performer/x");
        // 旧 lead / wing も resolve 側が受理するので素通し。
        assert_eq!(lane_query_for("vp/lead"), "vp/lead");
        assert_eq!(lane_query_for("vp/wing/x"), "vp/wing/x");
    }

    #[test]
    fn outcome_serde_roundtrip() {
        let done = Outcome::Done {
            result: "ok".to_string(),
        };
        let v = serde_json::to_value(&done).unwrap();
        assert_eq!(v, serde_json::json!({"kind": "done", "result": "ok"}));
        let back: Outcome = serde_json::from_value(v).unwrap();
        assert!(matches!(back, Outcome::Done { result } if result == "ok"));

        let failed: Outcome =
            serde_json::from_value(serde_json::json!({"kind": "failed", "reason": "boom"}))
                .unwrap();
        assert!(matches!(failed, Outcome::Failed { reason } if reason == "boom"));

        // NeedsInput(=Reborn): `{"kind":"needsinput","question":"..."}`
        let ni = Outcome::NeedsInput {
            question: "どの DB?".to_string(),
        };
        let v = serde_json::to_value(&ni).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"kind": "needsinput", "question": "どの DB?"})
        );
        let back: Outcome = serde_json::from_value(v).unwrap();
        assert!(matches!(back, Outcome::NeedsInput { question } if question == "どの DB?"));
    }

    #[test]
    fn doer_prompt_is_single_line_and_self_contained() {
        let p = doer_wake_prompt("dlg-1", "agent@vp", "DB schema を書く");
        assert!(!p.contains('\n'), "wake prompt は単一行 (Enter 別送のため)");
        assert!(p.contains("dlg-1"));
        assert!(p.contains("DB schema を書く"));
        assert!(p.contains("mcp__vantage-point__complete"));
    }
}
