//! Stand 命名体系 — 機能名と愛称の分離レイヤー
//!
//! コード内部は安定した機能名（id）を使い、UI/CLI/ログでは愛称（stand_name）を表示する。
//! 愛称を変更しても stands.rs だけの修正で済む。
//!
//! ## 使い方
//!
//! ```rust,ignore
//! use vantage_point::stands;
//!
//! tracing::info!("{} 起動 (port {})", stands::WORLD.display(), port);
//! ```

/// Stand の愛称定義
///
/// `id` はコード内部で使う安定キー（リネーム不要）。
/// `stand_name` / `short` / `emoji` は UI/CLI 表示用（自由に変更可能）。
#[derive(Debug, Clone)]
pub struct StandAlias {
    /// 安定キー（コード内部・設定ファイル・API パス）
    pub id: &'static str,
    /// 機能名（技術的な説明）
    pub functional_name: &'static str,
    /// Stand 愛称（JoJo メタファー）
    pub stand_name: &'static str,
    /// 短縮形（TUI ヘッダ等）
    pub short: &'static str,
    /// 絵文字
    pub emoji: &'static str,
}

impl StandAlias {
    /// 表示用文字列: "👑 TheWorld"
    pub fn display(&self) -> String {
        format!("{} {}", self.emoji, self.stand_name)
    }

    /// CLI ヘルプ用の説明: "TheWorld（Process Manager）"
    pub fn description(&self) -> String {
        format!("{}（{}）", self.stand_name, self.functional_name)
    }

    /// ログ用の短い表記: "[TheWorld]"
    pub fn log_prefix(&self) -> String {
        format!("[{}]", self.stand_name)
    }
}

// ─── システムレベル ──────────────────────────────────

/// 全 PP を統括管理する常駐デーモン
pub const WORLD: StandAlias = StandAlias {
    id: "world",
    functional_name: "Process Manager",
    stand_name: "TheWorld",
    short: "W",
    emoji: "👑",
};

// ─── プロジェクトレベル ──────────────────────────────

/// プロジェクトの主人公 — TUI 統合ビュー + 各 Stand が同居する場
pub const STAR_PLATINUM: StandAlias = StandAlias {
    id: "process",
    functional_name: "Project Core",
    stand_name: "Star Platinum",
    short: "SP",
    emoji: "⭐",
};

// ─── Capability（Process にぶら下がるスタンド能力）───

/// 情報ナビゲーション能力 — ユーザーと AI に最適な情報を届ける（Paisley Park）
pub const PAISLEY_PARK: StandAlias = StandAlias {
    id: "canvas",
    functional_name: "Information Navigator",
    stand_name: "Paisley Park",
    short: "PP",
    emoji: "🧭",
};

/// コーディングアシスタント能力 — Claude CLI オーケストレーター
///
/// PR-pre2 (VP-118): Heaven's Door (岸辺露伴の「読み書き」) → Echoes (広瀬康一) に rename。
/// 動機: zsh → tmux → claude の chain spawn が Echoes Act 1/2/3 進化と完璧 fit、
/// terminal の echo (反響) 構造とも literal に一致。 emoji 💬 = prompt/response 対話型。
pub const ECHOES: StandAlias = StandAlias {
    id: "agent",
    functional_name: "Coding Assistant",
    stand_name: "Echoes",
    short: "EC",
    emoji: "💬",
};

/// コード実行能力 — 動的生命注入エンジン（Ruby VM / ProcessRunner）
pub const GOLD_EXPERIENCE: StandAlias = StandAlias {
    id: "runner",
    functional_name: "Code Runner",
    stand_name: "Gold Experience",
    short: "GE",
    emoji: "🌿",
};

/// シェルターミナル能力 — 直接操作ターミナル
pub const THE_HAND: StandAlias = StandAlias {
    id: "shell",
    functional_name: "Shell Terminal",
    stand_name: "The Hand",
    short: "TH",
    emoji: "✋",
};

/// デバイス集約能力 — World scope の物理 device registry / hot-plug / routing（Bastet 🧲）
///
/// epic v3.1 (E2) で旧 Hermit Purple 🍇 の World 座（`hermit_purple@world`）を継承し、
/// 「磁力で device を集約する」 registry に発展。per-lane の双方向 I/O は [`JUSTICE`] が担う。
/// 設計 SSOT: `docs/design/23-bastet-justice-stand-wiring.md`。
pub const BASTET: StandAlias = StandAlias {
    id: "bastet",
    functional_name: "Device Registry",
    stand_name: "Bastet",
    short: "BS",
    emoji: "🧲",
};

/// デバイス I/O 能力 — Lane scope の双方向 device endpoint（Justice 🌫️）
///
/// epic v3.1 (E3) 新設。「霧で機器に侵入する」per-lane の双方向 I/O endpoint。
/// lane state → 機材 LED/LCD projection（output）と 機材 → active Lane command（input）を担う。
/// `LaneStandHost`（`stand_kind="justice"`）として Lane に host される。
pub const JUSTICE: StandAlias = StandAlias {
    id: "justice",
    functional_name: "Device I/O",
    stand_name: "Justice",
    short: "JS",
    emoji: "🌫️",
};

// 旧 Whitesnake 🐍 (永続化レイヤー) は退役 — 永続は SurrealDB (vpdb) に一本化。
// PP pane state は pane_contents table が担う (file-backed DISC 層は撤去)。

/// 全 Stand の一覧（イテレーション用）
pub const ALL: &[&StandAlias] = &[
    &WORLD,
    &STAR_PLATINUM,
    &GOLD_EXPERIENCE,
    &PAISLEY_PARK,
    &ECHOES,
    &THE_HAND,
    &BASTET,
    &JUSTICE,
];
