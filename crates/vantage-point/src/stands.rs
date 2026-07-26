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
//! tracing::info!("{} 起動 (port {})", stands::DAEMON.display(), port);
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
    /// 表示用文字列: "👑 daemon"
    pub fn display(&self) -> String {
        format!("{} {}", self.emoji, self.stand_name)
    }

    /// CLI ヘルプ用の説明: "daemon（Process Manager）"
    pub fn description(&self) -> String {
        format!("{}（{}）", self.stand_name, self.functional_name)
    }

    /// ログ用の短い表記: "[daemon]"
    pub fn log_prefix(&self) -> String {
        format!("[{}]", self.stand_name)
    }
}

// ─── システムレベル ──────────────────────────────────

/// 全 repo を統括管理する常駐デーモン
pub const DAEMON: StandAlias = StandAlias {
    id: "daemon",
    functional_name: "Process Manager",
    stand_name: "Daemon",
    short: "D",
    emoji: "⚙️",
};

// ─── repoレベル ──────────────────────────────

/// repo（repository 単位）の runtime — 各 Stand が同居する場
pub const REPO: StandAlias = StandAlias {
    id: "repo",
    functional_name: "Repo Runtime",
    stand_name: "Repo",
    short: "RP",
    emoji: "📦",
};

// ─── Capability（Process にぶら下がるスタンド能力）───

/// 情報ナビゲーション能力 — ユーザーと AI に最適な情報を届ける
/// doc 52 §1/§6: id は `board`（貼る台）。旧 `canvas` は「描く」= 将来の canvas 著述機能に
/// 予約するため退去した（address は `board@repo/lane`）。
pub const BOARD: StandAlias = StandAlias {
    id: "board",
    functional_name: "Information Navigator",
    stand_name: "Board",
    short: "BD",
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

/// コード実行能力 — code runner（Ruby VM / ProcessRunner）
pub const RUNNER: StandAlias = StandAlias {
    id: "runner",
    functional_name: "Code Runner",
    stand_name: "Runner",
    short: "RN",
    emoji: "🌿",
};

/// デバイス集約能力 — machine scope の物理 device registry / hot-plug / routing（DeviceRegistry 🧲）
///
/// epic v3.1 (E2) で旧 External Control stand の machine 座を継承し、device 集約 registry に
/// 発展。per-lane の双方向 I/O は [`DEVICE_IO`] が担う。
/// 設計 SSOT: `docs/design/23-bastet-justice-stand-wiring.md`。
pub const DEVICES: StandAlias = StandAlias {
    id: "devices",
    functional_name: "Device Registry",
    stand_name: "Devices",
    short: "DV",
    emoji: "🧲",
};

/// デバイス I/O 能力 — Lane scope の双方向 device endpoint（Device I/O 🌫️）
///
/// epic v3.1 (E3) 新設。per-lane の双方向 I/O endpoint。
/// lane state → 機材 LED/LCD projection（output）と 機材 → active Lane command（input）を担う。
/// `LaneStandHost`（`stand_kind="device_io"`）として Lane に host される。
pub const DEVICE_IO: StandAlias = StandAlias {
    id: "device_io",
    functional_name: "Device I/O",
    stand_name: "Device I/O",
    short: "IO",
    emoji: "🌫️",
};

// 旧 file-backed 永続化レイヤーは退役 — 永続は SurrealDB (vpdb) に一本化。
// board pane state は pane_contents table が担う (file-backed DISC 層は撤去)。

/// 全 Stand の一覧（イテレーション用）
pub const ALL: &[&StandAlias] = &[
    &DAEMON, &REPO, &RUNNER, &BOARD, &ECHOES, &DEVICES, &DEVICE_IO,
];
