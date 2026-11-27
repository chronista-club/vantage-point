//! engine usage meter の vp-app 側 — daemon "daemon-usage" の snapshot の写しと、tray の表示文字列。
//!
//! 写し（`UsageSnapshot` / `EngineUsage` / `UsageWindow`）は daemon の
//! `vantage_point::usage::model` と同じ JSON 形（vp-app は vantage-point crate に依存しないので
//! `daemon_wire.rs` と同じく手で写す）。`severity` / `source` は文字列で受ける — daemon が
//! 値を増やしても vp-app が decode で落ちない（知らない severity は webview の `severityOf` が
//! % から決め直す。tray は severity を使わない）。
//!
//! ts-rs で webview にも同じ型を出す（`SidebarState.engine_usage` 経由で sidebar store に入る）。
//! epoch ms は JSON 上ただの number なので `ts(type = "number")`（既定の bigint にしない）。
//!
//! - data: 写しの型
//! - calculations: [`summary_window`] / [`tray_title`] / [`tray_line`]（純関数、単体 test 対象）
//!
//! spec: creo `mem_1ChMLKCMUvC73JHTLjG3xg`

use serde::{Deserialize, Serialize};
#[cfg(test)]
use ts_rs::TS;

/// daemon が配る全体像（`engines` は claude / codex / agy / grok / muse の固定順）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct UsageSnapshot {
    #[serde(default)]
    pub engines: Vec<EngineUsage>,
    /// 最後に変わった時刻（epoch ms）。
    #[serde(default)]
    #[cfg_attr(test, ts(type = "number"))]
    pub updated_at_ms: u64,
}

/// 1 engine 分。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct EngineUsage {
    /// `claude` / `codex` / `agy` / `grok` / `muse`（lane の agent 名と同じ）。
    pub engine: String,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub windows: Vec<UsageWindow>,
    #[serde(default)]
    #[cfg_attr(test, ts(type = "number | null"))]
    pub fetched_at_ms: Option<u64>,
    /// `poll` / `event` / `cache` / `none`。
    #[serde(default)]
    pub source: String,
    /// 直近の取得に失敗した理由（前回の窓は残っていることがある）。
    #[serde(default)]
    pub error: Option<String>,
}

/// 1 つの枠。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS), ts(export, export_to = "webview/src/generated/"))]
pub struct UsageWindow {
    pub id: String,
    pub label: String,
    /// 使用率 %（0–100）。
    pub used_pct: f32,
    #[serde(default)]
    #[cfg_attr(test, ts(type = "number | null"))]
    pub resets_at_ms: Option<u64>,
    /// `normal` / `warning` / `critical`。
    #[serde(default)]
    pub severity: String,
    /// None = engine 全体の枠、Some = モデル別・グループ別の部分枠。
    #[serde(default)]
    pub scope: Option<String>,
}

/// menu bar に出す短い名前（MachineStrip のチップ・ゲージと同じ — `usage.ts::usageShortName`）。
pub fn short_name(engine: &str) -> &str {
    match engine {
        "claude" => "CC",
        "codex" => "Cdx",
        "agy" => "Gem",
        "grok" => "Grk",
        "muse" => "Muse",
        other => other,
    }
}

/// tray のメニュー行に出す名前。
pub fn display_name(engine: &str) -> &str {
    match engine {
        "claude" => "Claude",
        "codex" => "Codex",
        "agy" => "Gemini (agy)",
        "grok" => "Grok",
        "muse" => "Muse",
        other => other,
    }
}

/// その engine の代表の枠 = **engine 全体の枠（scope なし）の中で最も使っているもの**。
/// 全体の枠が 1 つも無い時だけ、部分枠の最悪値に倒す。同率は先に並んでいる方
/// （webview の `summaryWindow` と同じ規則 — 違うと tray と MachineStrip で別の窓を出す）。
pub fn summary_window(usage: &EngineUsage) -> Option<&UsageWindow> {
    let worst = |scoped: bool| {
        first_max(
            usage.windows.iter().filter(|w| w.scope.is_some() == scoped),
            |w| w.used_pct,
        )
    };
    worst(false).or_else(|| worst(true))
}

/// 最大値のうち**最初の 1 つ**（`Iterator::max_by` は同率で最後を返すので使わない）。
fn first_max<T>(items: impl Iterator<Item = T>, key: impl Fn(&T) -> f32) -> Option<T> {
    items.fold(None, |best, item| match best {
        Some(b) if key(&b) >= key(&item) => Some(b),
        _ => Some(item),
    })
}

/// `88.0` → `88`、`17.3` → `17`（menu bar / 一覧は整数で足りる）。
fn pct_text(used_pct: f32) -> String {
    format!("{}", used_pct.round() as i64)
}

/// menu bar の文字（`TrayIcon::set_title`）。**最も逼迫している engine だけ**を出す
/// （`Cdx 95%`）。4 つ並べると menu bar を食いすぎるので、一覧はメニューの中に置く。
/// まだどれも取れていなければ None（icon だけにする）。
pub fn tray_title(snapshot: &UsageSnapshot) -> Option<String> {
    first_max(
        snapshot
            .engines
            .iter()
            .filter_map(|e| summary_window(e).map(|w| (e, w))),
        |(_, w)| w.used_pct,
    )
    .map(|(e, w)| format!("{} {}%", short_name(&e.engine), pct_text(w.used_pct)))
}

/// epoch ms → 表示用の時刻（`11/26 09:59`、`offset` の時刻帯で）。
pub fn format_reset_at(resets_at_ms: u64, offset: chrono::FixedOffset) -> Option<String> {
    let at = chrono::DateTime::from_timestamp_millis(i64::try_from(resets_at_ms).ok()?)?;
    Some(at.with_timezone(&offset).format("%m/%d %H:%M").to_string())
}

/// tray メニューの 1 行（engine 1 つ）。
///
/// - 取れている: `Codex — 95% weekly · resets 11/30 06:10`
/// - 取れていない: `Grok — (Refresh で取得)`、失敗していれば理由を短く
pub fn tray_line(usage: &EngineUsage, offset: chrono::FixedOffset) -> String {
    let name = display_name(&usage.engine);
    match summary_window(usage) {
        Some(w) => {
            let reset = w
                .resets_at_ms
                .and_then(|ms| format_reset_at(ms, offset))
                .map(|t| format!(" · resets {t}"))
                .unwrap_or_default();
            format!("{name} — {}% {}{reset}", pct_text(w.used_pct), w.label)
        }
        None => match usage.error.as_deref() {
            Some(error) => format!("{name} — {}", truncate(error, 40)),
            None => format!("{name} — (Refresh で取得)"),
        },
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn muse_uses_its_display_name() {
        assert_eq!(short_name("muse"), "Muse");
        assert_eq!(display_name("muse"), "Muse");
    }

    fn window(id: &str, pct: f32, scope: Option<&str>) -> UsageWindow {
        UsageWindow {
            id: id.into(),
            label: id.into(),
            used_pct: pct,
            resets_at_ms: None,
            severity: "normal".into(),
            scope: scope.map(str::to_string),
        }
    }

    fn engine(name: &str, windows: Vec<UsageWindow>) -> EngineUsage {
        EngineUsage {
            engine: name.into(),
            windows,
            source: "poll".into(),
            ..Default::default()
        }
    }

    fn jst() -> chrono::FixedOffset {
        chrono::FixedOffset::east_opt(9 * 3600).unwrap()
    }

    /// daemon が送る JSON（`vantage_point::usage::model` の serde そのまま）を decode できること。
    /// daemon 側の field 追加・未知の severity で落ちないことも。
    #[test]
    fn decodes_daemon_snapshot_json() {
        let json = serde_json::json!({
            "engines": [{
                "engine": "claude",
                "plan": "Ultra",
                "tier": "default_claude_max_20x",
                "windows": [{
                    "id": "weekly", "label": "weekly", "used_pct": 88.0,
                    "resets_at_ms": 1_795_654_799_983u64, "severity": "warning", "scope": null
                }, {
                    "id": "model:fable", "label": "weekly · Fable", "used_pct": 100.0,
                    "resets_at_ms": null, "severity": "someday-new", "scope": "Fable"
                }],
                "fetched_at_ms": 1_795_502_717_111u64,
                "source": "cache",
                "error": null,
                "future_field": 1
            }],
            "updated_at_ms": 1_795_502_800_000u64
        });
        let snapshot: UsageSnapshot = serde_json::from_value(json).unwrap();
        let claude = &snapshot.engines[0];
        assert_eq!(claude.plan.as_deref(), Some("Ultra"));
        assert_eq!(claude.windows[1].scope.as_deref(), Some("Fable"));
        assert_eq!(claude.windows[0].resets_at_ms, Some(1_795_654_799_983));
    }

    /// wire 契約: daemon 側（`vantage_point::usage::model` の `snapshot_wire_shape_is_pinned`）が
    /// 固定した JSON をそのまま decode でき、写し直しても同じ JSON に戻ること。
    #[test]
    fn decodes_and_reencodes_the_daemon_wire_fixture() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../vantage-point/src/usage/testdata/snapshot_wire.json");
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let snapshot: UsageSnapshot = serde_json::from_value(fixture.clone()).unwrap();
        assert_eq!(snapshot.engines.len(), 5);
        let muse = snapshot
            .engines
            .iter()
            .find(|e| e.engine == "muse")
            .unwrap();
        assert!(tray_line(muse, chrono::FixedOffset::east_opt(0).unwrap()).starts_with("Muse"));
        assert_eq!(summary_window(muse).unwrap().used_pct, 70.0);
        assert_eq!(
            tray_title(&snapshot).as_deref(),
            Some("Cdx 95%"),
            "Fable 100% は部分枠なのでサマリにしない"
        );
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), fixture);
    }

    #[test]
    fn summary_ignores_scoped_windows_unless_nothing_else() {
        let claude = engine(
            "claude",
            vec![
                window("weekly", 88.0, None),
                window("model:fable", 100.0, Some("Fable")),
            ],
        );
        assert_eq!(
            summary_window(&claude).unwrap().id,
            "weekly",
            "Fable だけ使い切っても Claude 全体は 88%"
        );
        let agy = engine(
            "agy",
            vec![
                window("gemini-weekly", 10.0, None),
                window("gemini-5h", 17.3, None),
                window("3p-5h", 100.0, Some("Claude/GPT")),
            ],
        );
        assert_eq!(
            summary_window(&agy).unwrap().id,
            "gemini-5h",
            "全体枠の最悪値"
        );
        let only_scoped = engine(
            "x",
            vec![window("a", 3.0, Some("A")), window("b", 9.0, Some("B"))],
        );
        assert_eq!(summary_window(&only_scoped).unwrap().id, "b");
        assert!(summary_window(&engine("grok", vec![])).is_none());
    }

    /// sticky-fingers #4: 同率の時は**先に並んでいる方**（TS の `summaryWindow` / `worstGauge` と
    /// 同じ規則）。違うと tray と MachineStrip で別のエンジン・別のリセット時刻を出す。
    #[test]
    fn ties_pick_the_first_like_the_webview() {
        let claude = engine(
            "claude",
            vec![window("5h", 100.0, None), window("weekly", 100.0, None)],
        );
        assert_eq!(summary_window(&claude).unwrap().id, "5h");
        let snapshot = UsageSnapshot {
            engines: vec![claude, engine("codex", vec![window("weekly", 100.0, None)])],
            updated_at_ms: 1,
        };
        assert_eq!(tray_title(&snapshot).as_deref(), Some("CC 100%"));
    }

    #[test]
    fn tray_title_shows_the_most_pressed_engine() {
        let snapshot = UsageSnapshot {
            engines: vec![
                engine("claude", vec![window("weekly", 88.0, None)]),
                engine("codex", vec![window("weekly", 95.0, None)]),
                engine("agy", vec![window("gemini-5h", 17.3, None)]),
                engine("grok", vec![]),
            ],
            updated_at_ms: 1,
        };
        assert_eq!(tray_title(&snapshot).as_deref(), Some("Cdx 95%"));
        assert_eq!(
            tray_title(&UsageSnapshot::default()),
            None,
            "未取得は icon だけ"
        );
        let empty = UsageSnapshot {
            engines: vec![engine("grok", vec![])],
            updated_at_ms: 0,
        };
        assert_eq!(tray_title(&empty), None);
    }

    #[test]
    fn tray_line_formats_pct_label_and_local_reset_time() {
        let mut w = window("weekly", 95.0, None);
        // 2026-11-29T21:10:31Z = JST 11/30 06:10
        w.resets_at_ms = Some(1_795_986_631_000);
        assert_eq!(
            tray_line(&engine("codex", vec![w]), jst()),
            "Codex — 95% weekly · resets 11/30 06:10"
        );
        assert_eq!(
            tray_line(&engine("agy", vec![window("gemini-5h", 17.3, None)]), jst()),
            "Gemini (agy) — 17% gemini-5h"
        );
    }

    #[test]
    fn tray_line_without_values_is_fail_open() {
        assert_eq!(
            tray_line(&engine("grok", vec![]), jst()),
            "Grok — (Refresh で取得)"
        );
        let failed = EngineUsage {
            error: Some("grok が見つかりません（未インストール）".into()),
            ..engine("grok", vec![])
        };
        assert_eq!(
            tray_line(&failed, jst()),
            "Grok — grok が見つかりません（未インストール）"
        );
        let long = EngineUsage {
            error: Some("あ".repeat(60)),
            ..engine("codex", vec![])
        };
        assert!(tray_line(&long, jst()).ends_with('…'));
    }
}
