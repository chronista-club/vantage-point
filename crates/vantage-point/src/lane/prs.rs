//! lane の repo の open PR 一覧（doc 82 — R sidebar の中央の節）。
//!
//! daemon-control `lane/prs` の実体。裏は当面 `gh pr list` を lane の cwd で実行する。
//!
//! ## 「許容 ≠ 依存」（CLAUDE.md 依存境界）
//!
//! `gh` は product の依存ではない。**不在なら `available:false`** を返し、GUI は PR 節ごと
//! 出さない。gh はあるが失敗した（GitHub remote でない / 未認証 / timeout）場合は
//! `available:true` + `error` で返す — どちらも daemon を落とさない。
//!
//! ## 応答は gh の形を漏らさない
//!
//! GUI が受け取るのは正規化済みの [`PrSummary`]（CI と review は語彙 4 つずつに畳む）。
//! 裏を GitHub API に差し替えても GUI 側は変わらない（doc 82 §契約）。
//!
//! ## 層の分離
//!
//! - **data**: [`LanePrs`] / [`PrSummary`]
//! - **calculations**: [`parse_gh_pr_list`] / [`ci_state`] / [`review_state`]（純関数、test 済）
//! - **actions**: [`list_open_prs`]（gh / git の spawn）

use serde::Serialize;
use std::path::Path;
use std::time::Duration;

/// gh に渡す `--json` の field 群。parse 側（[`parse_gh_pr_list`]）が読むものと 1 対 1。
const GH_FIELDS: &str = "number,title,isDraft,headRefName,url,statusCheckRollup,reviewDecision";

/// gh の応答待ちの上限。GitHub が遅い時に RPC を握り続けない（GUI は 60 秒ごとに取り直す）。
const GH_TIMEOUT: Duration = Duration::from_secs(20);

/// `lane/prs` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanePrs {
    /// 一覧を引く手段（gh）が在るか。false なら GUI は PR 節ごと出さない。
    pub available: bool,
    /// lane の cwd の現在の branch（detached / 不明なら None）。GUI が「この lane の PR」を
    /// 先頭固定するための材料 — 判定そのものは GUI の純関数が持つ（表示の都合なので）。
    pub branch: Option<String>,
    /// open PR（gh の並び = 新しい順）。
    pub prs: Vec<PrSummary>,
    /// 取得に失敗した理由（gh はあるが動かなかった）。成功時は省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// PR 1 件（GUI の 2 段の行 1 つ分）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub draft: bool,
    /// head branch 名。
    pub head: String,
    /// CI の畳み込み: `pass` | `fail` | `pending` | `none`（check が 1 つも無い）。
    pub ci: &'static str,
    /// review の状態: `approved` | `changes_requested` | `review_required` | `none`。
    pub review: &'static str,
}

// ============================================================================
// calculations
// ============================================================================

/// `statusCheckRollup` を 1 語に畳む。
///
/// 優先順位は **fail > pending > pass**（1 つでも落ちていれば赤、走っている物があれば黄）。
/// 要素は 2 種類ある — `CheckRun`（`status` + `conclusion`）と `StatusContext`（`state`）。
/// 知らない形は数えない（版ズレで全 PR が赤くなるより、黙って除外する方が害が小さい）。
pub fn ci_state(rollup: &serde_json::Value) -> &'static str {
    let Some(items) = rollup.as_array() else {
        return "none";
    };
    let (mut fail, mut pending, mut pass) = (false, false, false);
    for it in items {
        let s = |k: &str| it.get(k).and_then(|v| v.as_str()).unwrap_or("");
        if let Some(state) = it.get("state").and_then(|v| v.as_str()) {
            // StatusContext
            match state {
                "FAILURE" | "ERROR" => fail = true,
                "PENDING" | "EXPECTED" => pending = true,
                "SUCCESS" => pass = true,
                _ => {}
            }
            continue;
        }
        // CheckRun: 完了前は conclusion が空
        if !s("status").is_empty() && s("status") != "COMPLETED" {
            pending = true;
            continue;
        }
        match s("conclusion") {
            "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => {
                fail = true
            }
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => pass = true,
            _ => {}
        }
    }
    if fail {
        "fail"
    } else if pending {
        "pending"
    } else if pass {
        "pass"
    } else {
        "none"
    }
}

/// `reviewDecision` を語彙に写す（gh は未設定を空文字で返す）。
pub fn review_state(decision: &str) -> &'static str {
    match decision {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes_requested",
        "REVIEW_REQUIRED" => "review_required",
        _ => "none",
    }
}

/// `gh pr list --json …` の stdout を [`PrSummary`] 列に写す。
///
/// number / url の無い要素は捨てる（click で開けない行を出さない）。
pub fn parse_gh_pr_list(stdout: &str) -> Result<Vec<PrSummary>, String> {
    let v: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| format!("gh の出力を読めない: {e}"))?;
    let arr = v
        .as_array()
        .ok_or_else(|| "gh の出力が配列でない".to_string())?;
    Ok(arr
        .iter()
        .filter_map(|p| {
            let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let number = p.get("number").and_then(|v| v.as_u64())?;
            let url = s("url");
            if url.is_empty() {
                return None;
            }
            Some(PrSummary {
                number,
                title: s("title"),
                url,
                draft: p.get("isDraft").and_then(|v| v.as_bool()).unwrap_or(false),
                head: s("headRefName"),
                ci: ci_state(
                    p.get("statusCheckRollup")
                        .unwrap_or(&serde_json::Value::Null),
                ),
                review: review_state(&s("reviewDecision")),
            })
        })
        .collect())
}

// ============================================================================
// actions
// ============================================================================

/// lane の cwd で open PR を引く（`gh` を PATH から探す）。
pub async fn list_open_prs(cwd: &Path) -> LanePrs {
    list_open_prs_with("gh", cwd).await
}

/// [`list_open_prs`] の本体。`program` を差し替えられるのは「gh 不在」を test で作るため。
pub async fn list_open_prs_with(program: &str, cwd: &Path) -> LanePrs {
    let branch = current_branch(cwd).await;
    let mut cmd = tokio::process::Command::new(program);
    cmd.args([
        "pr", "list", "--state", "open", "--limit", "50", "--json", GH_FIELDS,
    ])
    .current_dir(cwd)
    // launchd 起動の daemon は shell の PATH を持たない — brew の gh が見えるよう補強する
    .env("PATH", crate::spawn_env::augmented_spawn_path())
    // 対話 prompt（未認証時の login 誘導等）で RPC を握らない
    .env("GH_PROMPT_DISABLED", "1")
    .stdin(std::process::Stdio::null())
    .kill_on_drop(true);
    let out = match tokio::time::timeout(GH_TIMEOUT, cmd.output()).await {
        Err(_) => return failed(branch, "gh が応答しない（timeout）".into()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return LanePrs {
                available: false,
                branch,
                prs: Vec::new(),
                error: None,
            };
        }
        Ok(Err(e)) => return failed(branch, format!("gh を起動できない: {e}")),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if msg.is_empty() {
            format!("gh が失敗した（{}）", out.status)
        } else {
            msg
        };
        return failed(branch, msg);
    }
    match parse_gh_pr_list(&String::from_utf8_lossy(&out.stdout)) {
        Ok(prs) => LanePrs {
            available: true,
            branch,
            prs,
            error: None,
        },
        Err(e) => failed(branch, e),
    }
}

fn failed(branch: Option<String>, error: String) -> LanePrs {
    LanePrs {
        available: true,
        branch,
        prs: Vec::new(),
        error: Some(error),
    }
}

/// cwd の現在の branch。detached HEAD（`HEAD` が返る）や git 不在は None。
async fn current_branch(cwd: &Path) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!b.is_empty() && b != "HEAD").then_some(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ci_folds_fail_over_pending_over_pass() {
        let run = |status: &str, conclusion: &str| json!({"__typename":"CheckRun","status":status,"conclusion":conclusion});
        assert_eq!(ci_state(&json!([])), "none");
        assert_eq!(ci_state(&serde_json::Value::Null), "none");
        assert_eq!(ci_state(&json!([run("COMPLETED", "SUCCESS")])), "pass");
        assert_eq!(
            ci_state(&json!([
                run("COMPLETED", "SUCCESS"),
                run("IN_PROGRESS", "")
            ])),
            "pending"
        );
        assert_eq!(
            ci_state(&json!([
                run("IN_PROGRESS", ""),
                run("COMPLETED", "FAILURE")
            ])),
            "fail"
        );
        // SKIPPED / NEUTRAL は通過扱い（必須でない job が赤く見えない）
        assert_eq!(
            ci_state(&json!([
                run("COMPLETED", "SKIPPED"),
                run("COMPLETED", "NEUTRAL")
            ])),
            "pass"
        );
    }

    #[test]
    fn ci_reads_status_context_too() {
        let ctx = |state: &str| json!({"__typename":"StatusContext","state":state});
        assert_eq!(ci_state(&json!([ctx("SUCCESS")])), "pass");
        assert_eq!(ci_state(&json!([ctx("PENDING")])), "pending");
        assert_eq!(ci_state(&json!([ctx("ERROR"), ctx("SUCCESS")])), "fail");
    }

    #[test]
    fn review_maps_empty_to_none() {
        assert_eq!(review_state("APPROVED"), "approved");
        assert_eq!(review_state("CHANGES_REQUESTED"), "changes_requested");
        assert_eq!(review_state("REVIEW_REQUIRED"), "review_required");
        assert_eq!(review_state(""), "none");
    }

    #[test]
    fn parse_normalizes_gh_shape() {
        let stdout = json!([
            {"number": 12, "title": "feat: x", "isDraft": true, "headRefName": "review/x",
             "url": "https://github.com/o/r/pull/12", "reviewDecision": "",
             "statusCheckRollup": [{"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"}]},
            // url の無い要素は捨てる（開けない行を出さない）
            {"number": 13, "title": "broken"}
        ])
        .to_string();
        let prs = parse_gh_pr_list(&stdout).unwrap();
        assert_eq!(
            prs,
            vec![PrSummary {
                number: 12,
                title: "feat: x".into(),
                url: "https://github.com/o/r/pull/12".into(),
                draft: true,
                head: "review/x".into(),
                ci: "pass",
                review: "none",
            }]
        );
        assert!(parse_gh_pr_list("not json").is_err());
        assert!(parse_gh_pr_list("{}").is_err());
    }

    #[tokio::test]
    async fn missing_gh_is_unavailable_not_error() {
        let dir = std::env::temp_dir();
        let r = list_open_prs_with("vp-test-no-such-gh-binary", &dir).await;
        assert!(!r.available);
        assert!(r.error.is_none());
        assert!(r.prs.is_empty());
    }
}
