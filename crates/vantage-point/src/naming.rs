//! Stand 命名体系 — 機能名と愛称の分離レイヤー
//!
//! コード内部は安定した機能名（id）を使い、UI/CLI/ログでは愛称（stand_name）を表示する。
//! 愛称を変更しても naming.rs だけの修正で済む。
//!
//! ## 使い方
//!
//! ```rust
//! use crate::naming::stands;
//!
//! // ログに愛称を表示
//! tracing::info!("{} 起動 (port {})", stands::WORLD.display(), port);
//!
//! // CLI ヘルプに愛称を使用
//! let desc = stands::PAISLEY_PARK.description();
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

/// Stand 定義一覧
pub mod stands {
    use super::StandAlias;

    // ─── システムレベル ──────────────────────────────

    /// 全 PP を統括管理する常駐デーモン
    pub const WORLD: StandAlias = StandAlias {
        id: "world",
        functional_name: "Process Manager",
        stand_name: "TheWorld",
        short: "W",
        emoji: "👑",
    };

    // ─── プロジェクトレベル（PP）─────────────────────

    /// プロジェクトの開発ナビゲーター（旧 Process）
    pub const PAISLEY_PARK: StandAlias = StandAlias {
        id: "pp",
        functional_name: "Project Server",
        stand_name: "Paisley Park",
        short: "PP",
        emoji: "🧭",
    };

    // ─── Capability（PP にぶら下がるスタンド能力）────

    /// 表示能力 — WebView / TUI パネル
    pub const CANVAS: StandAlias = StandAlias {
        id: "canvas",
        functional_name: "Display Engine",
        stand_name: "Canvas",
        short: "CV",
        emoji: "🎨",
    };

    /// AI エージェント能力 — Claude CLI
    pub const STAR_PLATINUM: StandAlias = StandAlias {
        id: "agent",
        functional_name: "AI Agent",
        stand_name: "Star Platinum",
        short: "SP",
        emoji: "⭐",
    };

    /// コード実行能力 — ProcessRunner
    pub const HEAVENS_DOOR: StandAlias = StandAlias {
        id: "runner",
        functional_name: "Code Runner",
        stand_name: "Heaven's Door",
        short: "HD",
        emoji: "📖",
    };

    /// 外部コントロール能力 — MIDI / MCP / tmux
    pub const HERMIT_PURPLE: StandAlias = StandAlias {
        id: "external",
        functional_name: "External Control",
        stand_name: "Hermit Purple",
        short: "HP",
        emoji: "🍇",
    };

    /// 全 Stand の一覧（イテレーション用）
    pub const ALL: &[&StandAlias] = &[
        &WORLD,
        &PAISLEY_PARK,
        &CANVAS,
        &STAR_PLATINUM,
        &HEAVENS_DOOR,
        &HERMIT_PURPLE,
    ];
}
