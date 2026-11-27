//! 各エンジンの生 JSON → [`EngineUsage`] / [`UsageWindow`]（calculations 層、純関数）。
//!
//! 入力の形は 2026-11-24 の実測（creo `mem_1ChMJqaRP2SunfbmEd6h1n`、版 claude 2.1.330 /
//! codex-cli 0.171.0 / agy 1.5.16 / grok 1.0.92）。fixture は `testdata/`（個人情報は偽値）。
//!
//! 単位の罠（ここで吸収する）:
//!
//! | 入力 | 使用率 | リセット |
//! |---|---|---|
//! | Claude `~/.claude.json` の `cachedUsageUtilization` | `utilization` / `percent` = 0–100 | ISO 8601 |
//! | Claude stream-json の `rate_limit_event` | `utilization` = **0–1** | epoch **秒** |
//! | Codex `account/rateLimits/*` | `usedPercent` = 0–100 | epoch **秒** |
//! | agy `/usage` | `remaining_fraction` = **残量**の 0–1 | ISO 8601 |
//! | Grok `_x.ai/billing` | `creditUsagePercent` = 0–100 | ISO 8601（期間の終わり） |
//!
//! 個人情報: どの関数も **読む field を名指し**で拾う（object を丸ごと写さない）。account id /
//! email / team id は入力に在っても出力に出る経路が無い（`*_never_leaks_*` test で固定）。

use serde_json::Value;

use super::model::{EngineUsage, UsageEngine, UsageSource, UsageWindow};

/// ISO 8601（RFC 3339）→ epoch ms。読めなければ None。
pub fn iso_to_ms(value: &Value) -> Option<u64> {
    let s = value.as_str()?;
    let t = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    u64::try_from(t.timestamp_millis()).ok()
}

/// epoch 秒 → epoch ms。
pub fn epoch_secs_to_ms(value: &Value) -> Option<u64> {
    value.as_u64().map(|s| s.saturating_mul(1000)).or_else(|| {
        value
            .as_f64()
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(|s| (s * 1000.0) as u64)
    })
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Claude
// ---------------------------------------------------------------------------

/// `cachedUsageUtilization.utilization` の名前付き窓（`/api/oauth/usage` の形）。
///
/// field 名にはコード名のような key（`cinder_cove` 等）が大量にあるが、意味が公開されて
/// いないので頼らない。読むのはこの表と `limits[]` の既知の全体枠・model 別枠。
const CLAUDE_NAMED_WINDOWS: [(&str, &str, &str, Option<&str>); 4] = [
    ("five_hour", "5h", "5h", None),
    ("seven_day", "weekly", "weekly", None),
    (
        "seven_day_opus",
        "model:opus",
        "weekly · Opus",
        Some("Opus"),
    ),
    (
        "seven_day_sonnet",
        "model:sonnet",
        "weekly · Sonnet",
        Some("Sonnet"),
    ),
];

/// `~/.claude.json`（Claude Code 自身が書く設定 file）→ Claude の usage。
///
/// 使うのは `cachedUsageUtilization`（CC が usage を取るたびに書く cache、`fetchedAtMs` 付き）と
/// `oauthAccount.planDisplayName` / `organizationRateLimitTier` だけ。能動取得が失敗した時の
/// fallback。古い時刻もそのまま返し、service 側の期限切れ判定・新旧比較を通す。
pub fn claude_from_config(config: &Value) -> EngineUsage {
    let mut usage = EngineUsage::empty(UsageEngine::Claude);
    let account = &config["oauthAccount"];
    usage.plan = str_field(account, "planDisplayName");
    usage.tier = str_field(account, "organizationRateLimitTier");

    let cache = &config["cachedUsageUtilization"];
    let Some(util) = cache.get("utilization").filter(|v| v.is_object()) else {
        usage.error = Some(
            "usage の cache がまだありません（Claude Code で usage を表示すると書かれます）"
                .to_string(),
        );
        return usage;
    };
    usage.source = UsageSource::Cache;
    usage.fetched_at_ms = cache["fetchedAtMs"].as_u64();
    usage.windows = claude_windows(util);
    usage
}

/// get_usage と cache の同じ形式を共通で読む。
fn claude_windows(util: &Value) -> Vec<UsageWindow> {
    let mut windows = Vec::new();
    for (key, id, label, scope) in CLAUDE_NAMED_WINDOWS {
        let w = &util[key];
        let Some(pct) = w["utilization"].as_f64() else {
            continue;
        };
        let window = UsageWindow::new(id, label, pct, iso_to_ms(&w["resets_at"]));
        windows.push(match scope {
            Some(s) => window.scoped(s),
            None => window,
        });
    }

    // 名前付き窓が無い時は limits の全体枠を補う。同じ id は先に入った方を残す。
    for limit in util["limits"].as_array().into_iter().flatten() {
        let Some(pct) = limit["percent"].as_f64() else {
            continue;
        };
        let group = limit["group"].as_str().unwrap_or("limit");
        let reset = iso_to_ms(&limit["resets_at"]);
        let window = if let Some(model) = limit["scope"]["model"]["display_name"].as_str() {
            UsageWindow::new(
                format!("model:{}", model.to_lowercase()),
                format!("{group} · {model}"),
                pct,
                reset,
            )
            .scoped(model)
        } else if limit["scope"].is_null()
            || limit["scope"]
                .as_object()
                .is_some_and(|scope| scope.values().all(Value::is_null))
        {
            let kind = limit["kind"].as_str().unwrap_or("");
            let id = match (kind, group) {
                ("weekly_all" | "seven_day", _) | (_, "weekly") => "weekly",
                ("five_hour" | "five_hour_all" | "5h" | "session_all", _)
                | (_, "five_hour" | "5h" | "session") => "5h",
                _ => continue,
            };
            UsageWindow::new(id, id, pct, reset)
        } else {
            // 未知の scope を engine 全体の枠に昇格させない。
            continue;
        };
        if windows.iter().any(|w| w.id == window.id) {
            continue;
        }
        windows.push(window);
    }
    windows
}

/// stream-json の `rate_limit_event` 行 → 窓 1 つ（受動の差分）。
///
/// `rate_limit_info.utilization` は **0–1 の割合**（`/usage` 系の 0–100 と単位が違う）。
/// utilization を持たない event（`status` だけの古い形、testdata の `turn_plain.jsonl`）は
/// 割合が分からないので None — 既存の値を上書きしない。
pub fn claude_rate_limit_window(event: &Value) -> Option<UsageWindow> {
    if event["type"] != "rate_limit_event" {
        return None;
    }
    let info = &event["rate_limit_info"];
    let raw = info["utilization"].as_f64()?;
    // 実測は 0–1。将来 0–100 に揃えられた時に 1% 未満へ潰さないための保険（1 を超えたら % とみなす）。
    let pct = if raw <= 1.0 { raw * 100.0 } else { raw };
    let kind = info["rateLimitType"].as_str().unwrap_or("unknown");
    let resets = epoch_secs_to_ms(&info["resetsAt"]);
    let window = match CLAUDE_NAMED_WINDOWS.iter().find(|(key, ..)| *key == kind) {
        Some((_, id, label, scope)) => {
            let w = UsageWindow::new(*id, *label, pct, resets);
            match scope {
                Some(s) => w.scoped(*s),
                None => w,
            }
        }
        None => UsageWindow::new(kind, kind, pct, resets).scoped(kind),
    };
    Some(window)
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

/// `windowDurationMins` → (id, label)。300 = 5h、10080 = weekly。
fn codex_window_name(mins: Option<u64>, slot: &str) -> (String, String) {
    match mins {
        Some(300) => ("5h".into(), "5h".into()),
        Some(1440) => ("daily".into(), "daily".into()),
        Some(10080) => ("weekly".into(), "weekly".into()),
        Some(m) if m > 0 && m % 60 == 0 => (format!("{}h", m / 60), format!("{}h", m / 60)),
        Some(m) if m > 0 => (format!("{m}m"), format!("{m}m")),
        _ => (slot.to_string(), slot.to_string()),
    }
}

/// `RateLimitSnapshot`（`primary` / `secondary`）→ 窓。null の窓は出さない（sparse update では
/// 「その窓は今回の通知に無い」の意味なので、既存の値を消さない）。
fn codex_snapshot_windows(snapshot: &Value, scope: Option<&str>) -> Vec<UsageWindow> {
    let mut out = Vec::new();
    for slot in ["primary", "secondary"] {
        let w = &snapshot[slot];
        let Some(pct) = w["usedPercent"].as_f64() else {
            continue;
        };
        let (id, label) = codex_window_name(w["windowDurationMins"].as_u64(), slot);
        let resets = epoch_secs_to_ms(&w["resetsAt"]);
        out.push(match scope {
            Some(s) => UsageWindow::new(format!("{s}:{id}"), format!("{s} · {label}"), pct, resets)
                .scoped(s),
            None => UsageWindow::new(id, label, pct, resets),
        });
    }
    out
}

/// 主 limit の id（2026-11-24 実測で `codex`）。これ以外の limit（model 別の枠）は部分枠にする。
const CODEX_MAIN_LIMIT: &str = "codex";

/// limit の名乗り: `limitName` → `normalModelSlug` → limit id の順（read / update で同じ）。
fn codex_limit_name<'a>(snapshot: &'a Value, limit_id: &'a str) -> &'a str {
    snapshot["limitName"]
        .as_str()
        .or_else(|| snapshot["normalModelSlug"].as_str())
        .unwrap_or(limit_id)
}

/// `account/rateLimits/read` の result → Codex の usage（能動 poll の全量）。
///
/// `rateLimits` が engine 全体の枠。`rateLimitsByLimitId` に別の limit（model 別の枠）が
/// あれば部分枠として足す（主 limit と同じ id は重複なので飛ばす）。
pub fn codex_from_rate_limits_read(result: &Value) -> EngineUsage {
    let mut usage = EngineUsage::empty(UsageEngine::Codex);
    let main = &result["rateLimits"];
    if !main.is_object() {
        usage.error = Some("rate limit の応答に rateLimits がありません".to_string());
        return usage;
    }
    usage.source = UsageSource::Poll;
    usage.plan = str_field(main, "planType");
    usage.windows = codex_snapshot_windows(main, None);

    let main_id = main["limitId"].as_str();
    if let Some(by_id) = result["rateLimitsByLimitId"].as_object() {
        for (limit_id, snap) in by_id {
            if Some(limit_id.as_str()) == main_id {
                continue;
            }
            let name = codex_limit_name(snap, limit_id);
            usage
                .windows
                .extend(codex_snapshot_windows(snap, Some(name)));
        }
    }
    usage
}

/// `account/rateLimits/updated` の params → (窓の差分, plan)。
///
/// 通知は **sparse**（codex の型 doc: 「直前の read の結果に merge せよ。null の metadata は
/// 以前の値を消さない」）。なので返すのは「在った窓」と「在った plan」だけ。
///
/// 主 limit（[`CODEX_MAIN_LIMIT`]、`limitId` 無しも主とみなす）以外の通知は、read 側の
/// `rateLimitsByLimitId` と同じ id / scope の部分枠にする — engine 全体の `weekly` を別 limit の
/// 値で上書きしない。
pub fn codex_rate_limits_update(params: &Value) -> Option<(Vec<UsageWindow>, Option<String>)> {
    let snapshot = &params["rateLimits"];
    if !snapshot.is_object() {
        return None;
    }
    let scope = match snapshot["limitId"].as_str() {
        None | Some(CODEX_MAIN_LIMIT) => None,
        Some(limit_id) => Some(codex_limit_name(snapshot, limit_id)),
    };
    let windows = codex_snapshot_windows(snapshot, scope);
    let plan = str_field(snapshot, "planType");
    if windows.is_empty() && plan.is_none() {
        return None;
    }
    Some((windows, plan))
}

// ---------------------------------------------------------------------------
// agy（Gemini）
// ---------------------------------------------------------------------------

/// group 名を短くする（`Gemini Models` → `Gemini`、`Claude and GPT models` → `Claude/GPT`）。
fn agy_short_group(name: &str) -> String {
    let trimmed = name
        .strip_suffix(" Models")
        .or_else(|| name.strip_suffix(" models"))
        .unwrap_or(name);
    trimmed.replace(" and ", "/")
}

/// `agy -p "/usage" --output-format json` の出力 → agy の usage。
///
/// `remaining_fraction` は **残量**の割合（0–1）なので `used = (1 - remaining) * 100`。
/// group が 2 つある（Gemini 群 / Claude・GPT 群）。agy を「Gemini のエンジン」として見るので、
/// `Gemini` で始まる group を engine 全体の枠、それ以外を部分枠にする。
pub fn agy_from_usage(output: &Value) -> EngineUsage {
    let mut usage = EngineUsage::empty(UsageEngine::Agy);
    let Some(groups) = output["command"]["data"]["groups"].as_array() else {
        let status = output["status"].as_str().unwrap_or("不明");
        usage.error = Some(format!(
            "agy /usage の応答に usage がありません（status: {status}）"
        ));
        return usage;
    };
    usage.source = UsageSource::Poll;
    for group in groups {
        let name = group["name"].as_str().unwrap_or("");
        let short = agy_short_group(name);
        let primary = name.starts_with("Gemini");
        for bucket in group["buckets"].as_array().into_iter().flatten() {
            let Some(remaining) = bucket["remaining_fraction"].as_f64() else {
                continue;
            };
            let window_name = bucket["window"]
                .as_str()
                .or_else(|| bucket["name"].as_str())
                .unwrap_or("limit");
            let id = bucket["id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("{}-{window_name}", short.to_lowercase()));
            let window = UsageWindow::new(
                id,
                format!("{short} · {window_name}"),
                (1.0 - remaining) * 100.0,
                iso_to_ms(&bucket["reset_time"]),
            );
            usage.windows.push(if primary {
                window
            } else {
                window.scoped(short.clone())
            });
        }
    }
    usage
}

// ---------------------------------------------------------------------------
// Grok
// ---------------------------------------------------------------------------

/// `USAGE_PERIOD_TYPE_WEEKLY` → `weekly`。
fn grok_period_name(period_type: Option<&str>) -> String {
    match period_type {
        Some(t) => t
            .strip_prefix("USAGE_PERIOD_TYPE_")
            .unwrap_or(t)
            .to_lowercase(),
        None => "period".to_string(),
    }
}

/// ACP 拡張 `_x.ai/billing` の result → Grok の usage。
///
/// `subscription_tier` もこの応答に入っているので、メールアドレスを含む
/// `_x.ai/auth/check_subscription` は呼ばない。
pub fn grok_from_billing(result: &Value) -> EngineUsage {
    let mut usage = EngineUsage::empty(UsageEngine::Grok);
    usage.plan = str_field(result, "subscription_tier");
    let config = &result["config"];
    let Some(pct) = config["creditUsagePercent"].as_f64() else {
        usage.error = Some("billing の応答に creditUsagePercent がありません".to_string());
        return usage;
    };
    usage.source = UsageSource::Poll;
    let period = &config["currentPeriod"];
    let name = grok_period_name(period["type"].as_str());
    let resets = iso_to_ms(&period["end"]).or_else(|| iso_to_ms(&config["billingPeriodEnd"]));
    usage
        .windows
        .push(UsageWindow::new(name.clone(), name, pct, resets));
    usage
}

/// stream-json の control_response 全体 → Claude の usage。
pub fn claude_from_get_usage(response: &Value, config: Option<&Value>) -> EngineUsage {
    let mut usage = EngineUsage::empty(UsageEngine::Claude);
    let envelope = &response["response"];
    let body = &envelope["response"];
    let account = config.map(|c| &c["oauthAccount"]).unwrap_or(&Value::Null);
    usage.plan =
        str_field(account, "planDisplayName").or_else(|| str_field(body, "subscription_type"));
    usage.tier = str_field(account, "organizationRateLimitTier");
    if envelope["subtype"] == "error" {
        usage.error =
            Some(str_field(envelope, "error").unwrap_or_else(|| "get_usage に失敗しました".into()));
        return usage;
    }
    if envelope["subtype"] != "success" || !body["rate_limits"].is_object() {
        usage.error = Some("get_usage の応答に rate_limits がありません".into());
        return usage;
    }
    usage.source = UsageSource::Poll;
    usage.windows = claude_windows(&body["rate_limits"]);
    usage
}

/// `muse usage --json` → Muse の usage。時刻はすべて epoch ms。
pub fn muse_from_usage(output: &Value) -> EngineUsage {
    let mut usage = EngineUsage::empty(UsageEngine::Muse);
    let allowance = &output["allowance"];
    if !allowance.is_object() {
        let details: Vec<_> = ["reason", "hint"]
            .into_iter()
            .filter_map(|key| str_field(output, key))
            .collect();
        usage.error = Some(if details.is_empty() {
            "muse usage の応答に allowance がありません".into()
        } else {
            details.join(": ")
        });
        return usage;
    }
    usage.plan = str_field(allowance, "tier");
    usage.source = UsageSource::Poll;
    usage.fetched_at_ms = allowance["asOfMs"].as_u64();
    for (key, id) in [("window", "5h"), ("weekly", "weekly")] {
        let w = &allowance[key];
        if let Some(pct) = w["usedPercent"].as_f64() {
            usage
                .windows
                .push(UsageWindow::new(id, id, pct, w["resetsAtMs"].as_u64()));
        }
    }
    usage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::model::Severity;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/usage/testdata")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json")
    }

    fn window<'a>(usage: &'a EngineUsage, id: &str) -> &'a UsageWindow {
        usage
            .windows
            .iter()
            .find(|w| w.id == id)
            .unwrap_or_else(|| panic!("窓 {id} が無い: {:?}", usage.windows))
    }

    /// fixture に埋めた偽の個人情報が出力の JSON に 1 つも出ないこと。
    fn assert_no_pii(usage: &EngineUsage, secrets: &[&str]) {
        let json = serde_json::to_string(usage).unwrap();
        for secret in secrets {
            assert!(!json.contains(secret), "{secret} が漏れている: {json}");
        }
    }

    // mem_1ChUDBkLpW2eUivtimWNNM: active probe とキャッシュが同じ窓を読む。
    #[test]
    fn claude_get_usage_reads_limits_and_supplements_account_metadata() {
        let config = fixture("claude_config.json");
        let response = serde_json::json!({"type":"control_response","response":{
            "subtype":"success","request_id":"usage-1","response":{
                "subscription_type":"max", "rate_limits_available":true,
                "rate_limits": config["cachedUsageUtilization"]["utilization"].clone()
            }
        }});
        let usage = claude_from_get_usage(&response, Some(&config));
        assert_eq!(usage.source, UsageSource::Poll);
        assert_eq!(usage.plan.as_deref(), Some("Ultra"));
        assert_eq!(usage.tier.as_deref(), Some("default_claude_max_20x"));
        assert_eq!(usage.windows, claude_from_config(&config).windows);
        assert_eq!(usage.fetched_at_ms, None, "古い cache の時刻は採用しない");
        assert_eq!(
            claude_from_get_usage(&response, None).plan.as_deref(),
            Some("max")
        );
        assert_eq!(usage.error, None);
    }

    #[test]
    fn claude_get_usage_reports_protocol_errors_and_unavailable_limits() {
        let response = serde_json::json!({"type":"control_response","response":{
            "subtype":"error","error":"not logged in"
        }});
        assert_eq!(
            claude_from_get_usage(&response, None).error.as_deref(),
            Some("not logged in")
        );
        let unavailable = serde_json::json!({"response":{"subtype":"success","response":{
            "subscription_type":"pro", "rate_limits_available":false
        }}});
        assert!(claude_from_get_usage(&unavailable, None).error.is_some());
        assert!(claude_from_get_usage(&Value::Null, None).error.is_some());
    }

    #[test]
    fn muse_usage_reads_percentage_and_millisecond_timestamps() {
        let input = serde_json::json!({"schemaVersion":1,"allowance":{
            "tier":"Muse Code Everyday Usage",
            "window":{"usedPercent":70,"resetsAtMs":1795792190000u64},
            "weekly":{"usedPercent":23,"resetsAtMs":1795996800000u64},
            "asOfMs":1795775207653u64,"source":"mint"
        },"reason":null,"hint":null,"email":"person@example.com"});
        let usage = muse_from_usage(&input);
        assert_eq!(usage.engine, "muse");
        assert_eq!(usage.plan.as_deref(), Some("Muse Code Everyday Usage"));
        assert_eq!(usage.source, UsageSource::Poll);
        assert_eq!(usage.fetched_at_ms, Some(1795775207653));
        assert_eq!(window(&usage, "5h").used_pct, 70.0);
        assert_eq!(window(&usage, "5h").resets_at_ms, Some(1795792190000));
        assert_eq!(window(&usage, "weekly").used_pct, 23.0);
        assert_eq!(window(&usage, "weekly").resets_at_ms, Some(1795996800000));
        assert!(usage.windows.iter().all(|w| w.scope.is_none()));
        assert_no_pii(&usage, &["person@example.com"]);
    }

    #[test]
    fn muse_usage_missing_allowance_reports_reason_and_hint() {
        let usage = muse_from_usage(
            &serde_json::json!({"allowance":null,"reason":"unauthenticated","hint":"Run muse login"}),
        );
        let error = usage.error.unwrap();
        assert!(error.contains("unauthenticated"));
        assert!(error.contains("Run muse login"));
        assert!(muse_from_usage(&Value::Null).error.is_some());
        let partial =
            muse_from_usage(&serde_json::json!({"allowance":{"window":{"usedPercent":0}}}));
        assert_eq!(partial.windows.len(), 1);
        assert_eq!(partial.windows[0].used_pct, 0.0);
        assert_eq!(partial.windows[0].resets_at_ms, None);
    }

    #[test]
    fn iso_and_epoch_seconds_both_become_epoch_ms() {
        // 2026-11-26T00:59:59Z = 1795654799 秒
        assert_eq!(
            iso_to_ms(&Value::from("2026-11-26T00:59:59.983079+00:00")),
            Some(1_795_654_799_983)
        );
        assert_eq!(
            iso_to_ms(&Value::from("2026-11-30T20:19:38Z")),
            Some(1_796_069_978_000)
        );
        assert_eq!(
            epoch_secs_to_ms(&Value::from(1_795_986_631u64)),
            Some(1_795_986_631_000)
        );
        assert_eq!(iso_to_ms(&Value::from("not a date")), None);
        assert_eq!(iso_to_ms(&Value::Null), None);
    }

    #[test]
    fn claude_cache_reads_weekly_and_model_scoped_windows() {
        let usage = claude_from_config(&fixture("claude_config.json"));
        assert_eq!(usage.engine, "claude");
        assert_eq!(usage.source, UsageSource::Cache);
        assert_eq!(usage.fetched_at_ms, Some(1_795_502_717_111));
        assert_eq!(usage.plan.as_deref(), Some("Ultra"));
        assert_eq!(usage.tier.as_deref(), Some("default_claude_max_20x"));
        assert_eq!(usage.error, None);

        // five_hour は null → 窓を作らない
        assert!(usage.windows.iter().all(|w| w.id != "5h"));

        let weekly = window(&usage, "weekly");
        assert_eq!(weekly.used_pct, 88.0);
        assert_eq!(weekly.severity, Severity::Warning);
        assert_eq!(weekly.resets_at_ms, Some(1_795_654_799_983));
        assert_eq!(weekly.scope, None, "週の全体枠は engine 全体の枠");

        let fable = window(&usage, "model:fable");
        assert_eq!(fable.used_pct, 100.0);
        assert_eq!(fable.severity, Severity::Critical);
        assert_eq!(fable.label, "weekly · Fable");
        assert_eq!(fable.scope.as_deref(), Some("Fable"), "model 別枠は部分枠");

        // weekly_all（scope なし）は seven_day と同じものなので二重に出さない
        assert_eq!(usage.windows.len(), 2, "{:?}", usage.windows);
    }

    // mem_1ChUDBkLpW2eUivtimWNNM
    #[test]
    fn claude_limits_fallback_recovers_overall_windows() {
        let config = serde_json::json!({"cachedUsageUtilization":{"utilization":{
            "seven_day": null, "five_hour": null,
            "limits": [
                {"kind":"weekly_all","group":"weekly","percent":6,"scope":null},
                {"kind":"five_hour","group":"session","percent":18,"scope":null},
                {"kind":"weekly_scoped","group":"weekly","percent":1,"scope":{"model":{"display_name":"Fable"}}}
            ]
        }}});
        let usage = claude_from_config(&config);
        assert_eq!(window(&usage, "weekly").used_pct, 6.0);
        assert_eq!(window(&usage, "weekly").scope, None);
        assert_eq!(window(&usage, "5h").used_pct, 18.0);
        assert_eq!(
            window(&usage, "model:fable").scope.as_deref(),
            Some("Fable")
        );
    }

    #[test]
    fn claude_limits_empty_scope_is_overall_but_unknown_model_is_not() {
        let config = serde_json::json!({"cachedUsageUtilization":{"utilization":{"limits":[
            {"kind":"weekly_all","group":"weekly","percent":6,"scope":{"model":null,"surface":null}},
            {"kind":"weekly_scoped","group":"weekly","percent":99,"scope":{"model":{"id":"unknown"}}},
            {"kind":"session_scoped","group":"session","percent":99,"scope":{"surface":"chat"}}
        ]}}});
        let usage = claude_from_config(&config);
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(window(&usage, "weekly").used_pct, 6.0);
        assert_eq!(window(&usage, "weekly").scope, None);
    }

    #[test]
    fn claude_cache_never_leaks_account_identifiers() {
        let usage = claude_from_config(&fixture("claude_config.json"));
        assert_no_pii(
            &usage,
            &[
                "11111111-2222-3333-4444-555555555555",
                "someone@example.com",
                "66666666-7777-8888-9999-000000000000",
                "Example Org",
                "Someone",
            ],
        );
    }

    #[test]
    fn claude_without_cache_keeps_plan_and_reports_missing() {
        let config = serde_json::json!({
            "oauthAccount": { "planDisplayName": "Max", "emailAddress": "x@example.com" }
        });
        let usage = claude_from_config(&config);
        assert_eq!(usage.plan.as_deref(), Some("Max"));
        assert!(usage.windows.is_empty());
        assert_eq!(usage.source, UsageSource::None);
        assert!(usage.error.is_some());
    }

    #[test]
    fn claude_rate_limit_event_utilization_is_a_fraction() {
        let event = serde_json::json!({
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "allowed_warning",
                "resetsAt": 1_795_654_799u64,
                "rateLimitType": "seven_day",
                "utilization": 0.9,
                "overageStatus": "rejected",
                "isUsingOverage": false
            },
            "uuid": "u-1",
            "session_id": "s-1"
        });
        let w = claude_rate_limit_window(&event).expect("window");
        assert_eq!(w.id, "weekly");
        assert_eq!(w.used_pct, 90.0, "0–1 の割合を % に直す");
        assert_eq!(
            w.resets_at_ms,
            Some(1_795_654_799_000),
            "epoch 秒を ms に直す"
        );
        assert_eq!(w.severity, Severity::Warning);

        let five = serde_json::json!({
            "type": "rate_limit_event",
            "rate_limit_info": { "rateLimitType": "five_hour", "utilization": 0.12, "resetsAt": 1 }
        });
        assert_eq!(claude_rate_limit_window(&five).unwrap().id, "5h");
    }

    #[test]
    fn claude_rate_limit_event_without_utilization_is_ignored() {
        // testdata/turn_plain.jsonl:37 と同じ形（uuid / session_id は仮の値。status だけで割合が無い）
        let event: Value = serde_json::from_str(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1783546200,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false},"uuid":"ad6d04cc","session_id":"2f2020bf"}"#,
        )
        .unwrap();
        assert_eq!(claude_rate_limit_window(&event), None);
        // 別 type の行も素通し
        assert_eq!(
            claude_rate_limit_window(&serde_json::json!({"type":"assistant"})),
            None
        );
    }

    #[test]
    fn codex_read_reads_weekly_window_and_plan() {
        let usage = codex_from_rate_limits_read(&fixture("codex_rate_limits_read.json"));
        assert_eq!(usage.engine, "codex");
        assert_eq!(usage.source, UsageSource::Poll);
        assert_eq!(usage.plan.as_deref(), Some("pro"));
        assert_eq!(
            usage.windows.len(),
            1,
            "secondary は null / by_id は主 limit と同じ"
        );
        let weekly = window(&usage, "weekly");
        assert_eq!(weekly.used_pct, 95.0);
        assert_eq!(weekly.severity, Severity::Critical);
        assert_eq!(weekly.resets_at_ms, Some(1_795_986_631_000));
        assert_no_pii(&usage, &["acct-redacted-0000"]);
    }

    #[test]
    fn codex_read_names_windows_by_duration_and_adds_other_limits_as_scoped() {
        let result = serde_json::json!({
            "rateLimits": {
                "limitId": "codex",
                "primary": { "usedPercent": 12, "windowDurationMins": 300, "resetsAt": 100 },
                "secondary": { "usedPercent": 40, "windowDurationMins": 10080, "resetsAt": 200 },
                "planType": "plus"
            },
            "rateLimitsByLimitId": {
                "codex": { "limitId": "codex" },
                "codex_spark": {
                    "limitId": "codex_spark",
                    "limitName": "Spark",
                    "primary": { "usedPercent": 70, "windowDurationMins": 10080, "resetsAt": 300 }
                }
            }
        });
        let usage = codex_from_rate_limits_read(&result);
        assert_eq!(window(&usage, "5h").used_pct, 12.0);
        assert_eq!(window(&usage, "weekly").used_pct, 40.0);
        let spark = window(&usage, "Spark:weekly");
        assert_eq!(spark.scope.as_deref(), Some("Spark"));
        assert_eq!(spark.label, "Spark · weekly");
    }

    #[test]
    fn codex_update_is_sparse() {
        let (windows, plan) =
            codex_rate_limits_update(&fixture("codex_rate_limits_updated.json")).expect("update");
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id, "weekly");
        assert_eq!(windows[0].used_pct, 96.0);
        assert_eq!(
            plan, None,
            "null の planType は「今回は無い」で、以前の値を消さない"
        );
        assert_eq!(codex_rate_limits_update(&serde_json::json!({})), None);
    }

    /// moody-blues #3: 主 limit（`codex`）以外の limit の通知は部分枠として重ねる — read 側の
    /// `rateLimitsByLimitId` と同じ id / scope になり、engine 全体の `weekly` を上書きしない。
    #[test]
    fn codex_update_for_another_limit_is_scoped() {
        let params = serde_json::json!({
            "rateLimits": {
                "limitId": "codex_spark",
                "limitName": "Spark",
                "primary": { "usedPercent": 70, "windowDurationMins": 10080, "resetsAt": 300 },
                "planType": null
            }
        });
        let (windows, _) = codex_rate_limits_update(&params).expect("update");
        assert_eq!(windows[0].id, "Spark:weekly");
        assert_eq!(windows[0].scope.as_deref(), Some("Spark"));

        // limitName が無ければ normalModelSlug → limitId の順で名乗る（read 側と同じ）
        let unnamed = serde_json::json!({
            "rateLimits": { "limitId": "codex_x", "primary": { "usedPercent": 1, "windowDurationMins": 300 } }
        });
        assert_eq!(
            codex_rate_limits_update(&unnamed).unwrap().0[0].id,
            "codex_x:5h"
        );
    }

    #[test]
    fn agy_converts_remaining_fraction_to_used_pct() {
        let usage = agy_from_usage(&fixture("agy_usage.json"));
        assert_eq!(usage.engine, "agy");
        assert_eq!(usage.source, UsageSource::Poll);
        assert_eq!(usage.plan, None, "agy は tier 名を出さない");
        assert_eq!(usage.windows.len(), 4);

        let weekly = window(&usage, "gemini-weekly");
        assert_eq!(weekly.used_pct, 10.0, "残 0.8998 → 使用 10.0%");
        assert_eq!(weekly.label, "Gemini · weekly");
        assert_eq!(weekly.scope, None, "Gemini 群は engine 全体の枠");
        assert_eq!(weekly.resets_at_ms, Some(1_796_069_978_000));

        assert_eq!(window(&usage, "gemini-5h").used_pct, 17.3);

        let third_party = window(&usage, "3p-5h");
        assert_eq!(third_party.used_pct, 100.0, "残 0 → 使用 100%");
        assert_eq!(third_party.scope.as_deref(), Some("Claude/GPT"));
        assert_eq!(third_party.label, "Claude/GPT · 5h");
        assert_no_pii(&usage, &["conv-redacted-0000"]);
    }

    #[test]
    fn agy_error_output_is_reported() {
        let usage = agy_from_usage(&serde_json::json!({"status": "ERROR"}));
        assert!(usage.windows.is_empty());
        assert!(usage.error.as_deref().unwrap().contains("ERROR"));
    }

    #[test]
    fn grok_billing_reads_weekly_period_and_tier() {
        let usage = grok_from_billing(&fixture("grok_billing.json"));
        assert_eq!(usage.engine, "grok");
        assert_eq!(usage.source, UsageSource::Poll);
        assert_eq!(usage.plan.as_deref(), Some("X Premium"));
        let weekly = window(&usage, "weekly");
        assert_eq!(weekly.used_pct, 12.0);
        assert_eq!(weekly.severity, Severity::Normal);
        // 2026-11-26T06:52:24.411665Z
        assert_eq!(weekly.resets_at_ms, Some(1_795_675_944_411));
    }

    #[test]
    fn grok_billing_without_percent_is_an_error() {
        let usage = grok_from_billing(&serde_json::json!({"config": {}}));
        assert!(usage.windows.is_empty());
        assert!(usage.error.is_some());
    }

    #[test]
    fn severity_thresholds_and_pct_normalization() {
        assert_eq!(Severity::from_used_pct(79.9), Severity::Normal);
        assert_eq!(Severity::from_used_pct(80.0), Severity::Warning);
        assert_eq!(Severity::from_used_pct(94.9), Severity::Warning);
        assert_eq!(Severity::from_used_pct(95.0), Severity::Critical);
        let w = UsageWindow::new("x", "x", 123.4, None);
        assert_eq!(w.used_pct, 100.0, "100 を超えたら 100 に収める");
        assert_eq!(UsageWindow::new("x", "x", -5.0, None).used_pct, 0.0);
        assert_eq!(UsageWindow::new("x", "x", f64::NAN, None).used_pct, 0.0);
        assert_eq!(UsageWindow::new("x", "x", 17.347, None).used_pct, 17.3);
    }
}
