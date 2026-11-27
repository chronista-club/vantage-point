//! usage の正規化済みデータ（data 層）。
//!
//! 5 エンジンの「単位の罠」（使用率 0–100 / 0–1 / 残量の割合 0–1、epoch 秒 / ISO 8601）は
//! [`super::parse`] がここへ写す時に吸収する。この型に入った時点で:
//!
//! - `used_pct` は **使用率 %**（`0.0..=100.0`、小数 1 桁に丸め済み）。残枠 % は `100 - used_pct`
//! - `resets_at_ms` は epoch **ミリ秒**
//! - account id / メールアドレスなどの個人情報は**入っていない**（parse が読む field を
//!   白リストで絞っているので、入る経路が無い）
//!
//! wire（daemon → vp-app）の形もこの型の serde そのまま。vp-app 側は `crate::usage` に写しを持つ。

use serde::{Deserialize, Serialize};

/// usage を集める engine。並び（[`Self::ALL`]）がそのまま表示順。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageEngine {
    Claude,
    Codex,
    Agy,
    Grok,
    Muse,
}

impl UsageEngine {
    /// 表示順。snapshot の `engines` もこの順に並ぶ。
    pub const ALL: [UsageEngine; 5] =
        [Self::Claude, Self::Codex, Self::Agy, Self::Grok, Self::Muse];

    /// wire 上の名前（lane の agent 名と一致させる — sub lane 作成の picker が名前で引くため）。
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Agy => "agy",
            Self::Grok => "grok",
            Self::Muse => "muse",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.name() == name)
    }
}

/// 窓の逼迫度。閾値は [`Severity::from_used_pct`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[default]
    Normal,
    Warning,
    Critical,
}

/// warning に入る使用率 %。
pub const WARNING_PCT: f32 = 80.0;
/// critical に入る使用率 %。
pub const CRITICAL_PCT: f32 = 95.0;

impl Severity {
    /// `< 80` = normal、`>= 80` = warning、`>= 95` = critical。
    pub fn from_used_pct(used_pct: f32) -> Self {
        if used_pct >= CRITICAL_PCT {
            Self::Critical
        } else if used_pct >= WARNING_PCT {
            Self::Warning
        } else {
            Self::Normal
        }
    }
}

/// その engine の値がどこから来たか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageSource {
    /// daemon が能動的に問い合わせた（agy / codex / grok の probe）。
    Poll,
    /// 会話中に流れてきた受動の通知（Claude の `rate_limit_event` / Codex の
    /// `account/rateLimits/updated`）。
    Event,
    /// 誰かが書いた cache を読んだ（Claude の `~/.claude.json`、daemon 再起動前の保存分）。
    Cache,
    /// まだ一度も取れていない。
    #[default]
    None,
}

/// 1 つの枠（5 時間窓 / 週窓 / モデル別の枠 …）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// engine 内で一意な id（例 `weekly` / `5h` / `gemini-weekly` / `model:fable`）。
    /// 受動の差分はこの id で既存の窓に重ねる。
    pub id: String,
    /// 表示用の短い名前。
    pub label: String,
    /// 使用率 %（`0.0..=100.0`）。
    pub used_pct: f32,
    /// リセット時刻（epoch ms）。不明なら None。
    pub resets_at_ms: Option<u64>,
    pub severity: Severity,
    /// engine 全体の枠なら None、モデル別・グループ別の部分枠なら その名前。
    ///
    /// サマリの %（tray / MachineStrip / picker）は **None の窓の最悪値**で決める。部分枠を
    /// 混ぜると「Fable だけ使い切った」「agy の中の Claude 枠が 0」でエンジン全体が
    /// 100% に見えてしまい、エンジン選択の判断を誤らせるため。
    pub scope: Option<String>,
}

impl UsageWindow {
    /// 使用率を `0..=100` に収めて小数 1 桁に丸め、severity を付けて作る。
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        used_pct: f64,
        resets_at_ms: Option<u64>,
    ) -> Self {
        let used_pct = normalize_pct(used_pct);
        Self {
            id: id.into(),
            label: label.into(),
            used_pct,
            resets_at_ms,
            severity: Severity::from_used_pct(used_pct),
            scope: None,
        }
    }

    /// 部分枠にする（[`Self::scope`]）。
    pub fn scoped(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }
}

/// 使用率を `0.0..=100.0` に収め、小数 1 桁に丸める。NaN は 0 に倒す。
pub fn normalize_pct(used_pct: f64) -> f32 {
    if !used_pct.is_finite() {
        return 0.0;
    }
    let clamped = used_pct.clamp(0.0, 100.0);
    ((clamped * 10.0).round() / 10.0) as f32
}

/// 1 engine 分の usage。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineUsage {
    /// [`UsageEngine::name`]。
    pub engine: String,
    /// plan の表示名（例 `Ultra` / `pro` / `X Premium`）。
    pub plan: Option<String>,
    /// rate limit の tier（例 `default_claude_max_20x`）。出せる engine だけ。
    pub tier: Option<String>,
    pub windows: Vec<UsageWindow>,
    /// 値そのものが取得された時刻（epoch ms）。cache 由来なら cache の取得時刻。
    pub fetched_at_ms: Option<u64>,
    pub source: UsageSource,
    /// 直近の取得に失敗した理由（未インストール / 未ログイン / timeout …）。
    /// 失敗しても前回の `windows` は残す（古い値でも無いよりよい — 表示は fail-open）。
    pub error: Option<String>,
}

impl EngineUsage {
    /// 何も取れていない engine。
    pub fn empty(engine: UsageEngine) -> Self {
        Self {
            engine: engine.name().to_string(),
            plan: None,
            tier: None,
            windows: Vec::new(),
            fetched_at_ms: None,
            source: UsageSource::None,
            error: None,
        }
    }

    /// 取得に失敗しただけの engine（前回値と重ねる前の素の形）。
    pub fn failed(engine: UsageEngine, error: impl Into<String>) -> Self {
        Self {
            error: Some(error.into()),
            ..Self::empty(engine)
        }
    }
}

/// daemon が配る全体像。`engines` は常に [`UsageEngine::ALL`] の 5 件・同じ順。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub engines: Vec<EngineUsage>,
    /// snapshot が最後に変わった時刻（epoch ms）。0 = 一度も変わっていない。
    pub updated_at_ms: u64,
}

impl Default for UsageSnapshot {
    fn default() -> Self {
        Self {
            engines: UsageEngine::ALL
                .into_iter()
                .map(EngineUsage::empty)
                .collect(),
            updated_at_ms: 0,
        }
    }
}

impl UsageSnapshot {
    pub fn engine(&self, engine: UsageEngine) -> Option<&EngineUsage> {
        self.engines.iter().find(|e| e.engine == engine.name())
    }

    /// engine の entry を差し替える（並びは保つ。無ければ足す）。
    pub fn put(&mut self, usage: EngineUsage) {
        match self.engines.iter_mut().find(|e| e.engine == usage.engine) {
            Some(slot) => *slot = usage,
            None => self.engines.push(usage),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// wire 契約: daemon が送る JSON の形を fixture に固定する。vp-app の写し
    /// （`crates/vp-app/src/usage.rs`）は同じ fixture を decode する test を持つので、どちらかの
    /// field 名・型・enum の綴りが変わると片方が赤くなる（crate 依存が無いので型では繋がらない）。
    #[test]
    fn muse_is_a_known_usage_engine() {
        assert_eq!(
            UsageEngine::from_name("muse").map(UsageEngine::name),
            Some("muse")
        );
        assert_eq!(UsageSnapshot::default().engines.len(), 5);
    }

    #[test]
    fn snapshot_wire_shape_is_pinned() {
        let mut snapshot = UsageSnapshot::default();
        snapshot.put(EngineUsage {
            plan: Some("Ultra".into()),
            tier: Some("default_claude_max_20x".into()),
            windows: vec![
                UsageWindow::new("weekly", "weekly", 88.0, Some(1_795_654_799_983)),
                UsageWindow::new(
                    "model:fable",
                    "weekly · Fable",
                    100.0,
                    Some(1_795_654_799_983),
                )
                .scoped("Fable"),
            ],
            fetched_at_ms: Some(1_795_502_717_111),
            source: UsageSource::Cache,
            ..EngineUsage::empty(UsageEngine::Claude)
        });
        snapshot.put(EngineUsage {
            plan: Some("pro".into()),
            windows: vec![UsageWindow::new(
                "weekly",
                "weekly",
                95.0,
                Some(1_795_986_631_000),
            )],
            fetched_at_ms: Some(1_795_502_800_000),
            source: UsageSource::Event,
            ..EngineUsage::empty(UsageEngine::Codex)
        });
        snapshot.put(EngineUsage {
            windows: vec![UsageWindow::new(
                "gemini-5h",
                "Gemini · 5h",
                17.5,
                Some(1_795_474_554_000),
            )],
            fetched_at_ms: Some(1_795_502_800_000),
            source: UsageSource::Poll,
            ..EngineUsage::empty(UsageEngine::Agy)
        });
        snapshot.put(EngineUsage::failed(
            UsageEngine::Grok,
            "grok が見つかりません（未インストール）",
        ));
        snapshot.put(EngineUsage {
            plan: Some("Muse Code Everyday Usage".into()),
            windows: vec![
                UsageWindow::new("5h", "5h", 70.0, Some(1795792190000)),
                UsageWindow::new("weekly", "weekly", 23.0, Some(1795996800000)),
            ],
            fetched_at_ms: Some(1795775207653),
            source: UsageSource::Poll,
            ..EngineUsage::empty(UsageEngine::Muse)
        });
        snapshot.updated_at_ms = 1_795_502_800_001;

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/usage/testdata/snapshot_wire.json");
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), fixture);
    }
}
