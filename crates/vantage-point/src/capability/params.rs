//! Stand Capability Parameters (スタンドパラメータ)
//!
//! JoJo's Bizarre Adventureのスタンド能力パラメータシステムを参考に、
//! AIエージェント能力を定量的に評価するための6パラメータシステム。
//!
//! ## 設計思想
//!
//! - **視覚的直感性**: A〜Eの5段階で能力を一目で把握できる
//! - **拡張可能性**: 100〜1000規模の能力を想定し、比較可能な指標を提供
//! - **段階的計測**: 静的定義→動的計測→学習ベースへと進化
//!
//! ## 参考文献
//!
//! - JoJo Stand Parameters: <https://jojowiki.com/Stand_Stats>

use serde::{Deserialize, Serialize};
use std::fmt;

// =============================================================================
// Rank (ランク): A〜Eの5段階評価
// =============================================================================

/// 能力パラメータのランク (A〜Eの5段階 + 特殊ランク)
///
/// JoJoのスタンドパラメータと同様、能力の各側面を視覚的に評価する。
/// 数値範囲は設計ガイドライン。実装により調整可能。
///
/// ## 順序について
///
/// enum定義の順序が比較演算子（<, >, <=, >=）の順序を決定します。
/// None < Unknown < E < D < C < B < A の順序で定義されています。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Rank {
    /// None: 該当なし (その能力がこのパラメータを持たない)
    None,
    /// ?: 測定不能 (無限、条件付き、未定義)
    Unknown,
    /// E: 最低 (例: 破壊力1-20、スピード0.1-1秒)
    E,
    /// D: 低 (例: 破壊力21-40、スピード1-3秒)
    D,
    /// C: 中 (例: 破壊力41-60、スピード3-10秒)
    C,
    /// B: 高 (例: 破壊力61-80、スピード10-30秒)
    B,
    /// A: 最高 (例: 破壊力81-100、スピード30秒以上)
    A,
}

impl Rank {
    /// ランクを数値化 (E=1, D=2, C=3, B=4, A=5, ?=0, None=0)
    pub fn to_score(self) -> u8 {
        match self {
            Self::E => 1,
            Self::D => 2,
            Self::C => 3,
            Self::B => 4,
            Self::A => 5,
            Self::Unknown | Self::None => 0,
        }
    }

    /// 数値からランクを推定 (0-100スケール)
    pub fn from_score(score: u8) -> Self {
        match score {
            0 => Self::None,
            1..=20 => Self::E,
            21..=40 => Self::D,
            41..=60 => Self::C,
            61..=80 => Self::B,
            81..=100 => Self::A,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for Rank {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::E => write!(f, "E"),
            Self::D => write!(f, "D"),
            Self::C => write!(f, "C"),
            Self::B => write!(f, "B"),
            Self::A => write!(f, "A"),
            Self::Unknown => write!(f, "?"),
            Self::None => write!(f, "-"),
        }
    }
}

// =============================================================================
// Stand Capability Parameters (6パラメータ)
// =============================================================================

/// Stand Capability の6パラメータ
///
/// JoJoスタンドの6パラメータをAIエージェント能力向けに再定義。
///
/// ## パラメータ設計
///
/// 1. **破壊力 (Power)**: 能力の影響力・変更範囲
/// 2. **スピード (Speed)**: 応答速度・実行速度
/// 3. **射程距離 (Range)**: 能力の適用範囲・統合度
/// 4. **持続力 (Stamina)**: 継続動作時間・セッション寿命
/// 5. **精密動作性 (Precision)**: 制御精度・エラー率
/// 6. **成長性 (Potential)**: 拡張性・学習可能性
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityParams {
    /// 破壊力 (Power): 能力の影響力・変更範囲
    ///
    /// **AIエージェント能力への適用**:
    /// - 能力が扱えるデータ量・ファイル数
    /// - 能力が引き起こせる変更の規模
    /// - 能力の計算リソース消費量
    ///
    /// **測定基準**:
    /// - E: 単一ファイル/少量データ (1-10 files)
    /// - D: 小規模プロジェクト (10-100 files)
    /// - C: 中規模プロジェクト (100-1000 files)
    /// - B: 大規模プロジェクト (1000-10000 files)
    /// - A: 超大規模/複数プロジェクト横断 (10000+ files)
    pub power: Rank,

    /// スピード (Speed): 応答速度・実行速度
    ///
    /// **AIエージェント能力への適用**:
    /// - 能力の初期化時間
    /// - 能力の平均応答時間
    /// - 能力のスループット (処理量/秒)
    ///
    /// **測定基準**:
    /// - E: 非常に遅い (>30秒/操作)
    /// - D: 遅い (10-30秒/操作)
    /// - C: 普通 (3-10秒/操作)
    /// - B: 速い (1-3秒/操作)
    /// - A: 即座 (<1秒/操作、リアルタイム)
    pub speed: Rank,

    /// 射程距離 (Range): 能力の適用範囲・統合度
    ///
    /// **AIエージェント能力への適用**:
    /// - 能力が統合できるサービス数
    /// - 能力が対応できるプロトコル/API数
    /// - 能力の動作スコープ (ローカル/リモート)
    ///
    /// **測定基準**:
    /// - E: 単一サービス/ローカルのみ
    /// - D: 2-3サービス統合
    /// - C: 5-10サービス統合
    /// - B: 10-50サービス統合
    /// - A: 無制限統合/プロトコル非依存
    pub range: Rank,

    /// 持続力 (Stamina): 継続動作時間・セッション寿命
    ///
    /// **AIエージェント能力への適用**:
    /// - 能力の連続稼働時間
    /// - 能力のメモリリーク耐性
    /// - 能力の再起動頻度の必要性
    ///
    /// **測定基準**:
    /// - E: 短時間のみ (数分)
    /// - D: 短期セッション (数時間)
    /// - C: 1日稼働
    /// - B: 1週間稼働
    /// - A: 無期限稼働可能 (月単位、年単位)
    pub stamina: Rank,

    /// 精密動作性 (Precision): 制御精度・エラー率
    ///
    /// **AIエージェント能力への適用**:
    /// - 能力の出力精度 (成功率)
    /// - 能力のエラー処理能力
    /// - 能力の入力検証厳密性
    ///
    /// **測定基準**:
    /// - E: 不安定 (エラー率 >20%)
    /// - D: やや不安定 (エラー率 10-20%)
    /// - C: 安定 (エラー率 3-10%)
    /// - B: 高精度 (エラー率 1-3%)
    /// - A: 超高精度 (エラー率 <1%、形式検証済み)
    pub precision: Rank,

    /// 成長性 (Potential): 拡張性・学習可能性
    ///
    /// **AIエージェント能力への適用**:
    /// - 能力の設定カスタマイズ性
    /// - 能力の学習/適応機能の有無
    /// - 能力のプラグイン/拡張機構
    ///
    /// **測定基準**:
    /// - E: 固定機能のみ
    /// - D: 設定ファイルで調整可能
    /// - C: API/Hook で拡張可能
    /// - B: プラグイン機構あり
    /// - A: 自己学習・自己改善機能あり
    pub potential: Rank,
}

impl CapabilityParams {
    /// デフォルトパラメータ (全てC: 中程度)
    pub fn balanced() -> Self {
        Self {
            power: Rank::C,
            speed: Rank::C,
            range: Rank::C,
            stamina: Rank::C,
            precision: Rank::C,
            potential: Rank::C,
        }
    }

    /// 全パラメータをNoneに設定 (未測定)
    pub fn none() -> Self {
        Self {
            power: Rank::None,
            speed: Rank::None,
            range: Rank::None,
            stamina: Rank::None,
            precision: Rank::None,
            potential: Rank::None,
        }
    }

    /// 総合スコアを計算 (0-30)
    pub fn total_score(&self) -> u8 {
        self.power.to_score()
            + self.speed.to_score()
            + self.range.to_score()
            + self.stamina.to_score()
            + self.precision.to_score()
            + self.potential.to_score()
    }

    /// パラメータを配列として取得 (視覚化用)
    pub fn as_array(&self) -> [Rank; 6] {
        [
            self.power,
            self.speed,
            self.range,
            self.stamina,
            self.precision,
            self.potential,
        ]
    }

    /// パラメータ名の配列
    pub const PARAM_NAMES: [&'static str; 6] = [
        "Power",
        "Speed",
        "Range",
        "Stamina",
        "Precision",
        "Potential",
    ];

    /// パラメータ名（日本語）の配列
    pub const PARAM_NAMES_JP: [&'static str; 6] = [
        "破壊力",
        "スピード",
        "射程距離",
        "持続力",
        "精密動作性",
        "成長性",
    ];
}

impl fmt::Display for CapabilityParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Stand Capability Parameters:")?;
        writeln!(f, "  Power:     {} (破壊力)", self.power)?;
        writeln!(f, "  Speed:     {} (スピード)", self.speed)?;
        writeln!(f, "  Range:     {} (射程距離)", self.range)?;
        writeln!(f, "  Stamina:   {} (持続力)", self.stamina)?;
        writeln!(f, "  Precision: {} (精密動作性)", self.precision)?;
        writeln!(f, "  Potential: {} (成長性)", self.potential)?;
        writeln!(f, "  Total Score: {}/30", self.total_score())
    }
}

// =============================================================================
// 具体例: MIDI Capability のパラメータ
// =============================================================================

/// MIDI Capability のパラメータ設定例
///
/// ## 分析
///
/// - **Power (破壊力)**: D
///   - MIDI入出力は軽量。大規模な変更は引き起こさない。
///   - 制御対象: 8パッド + 8ノブ = 16個の物理コントロール
///   - データ量: MIDIメッセージは3バイト程度
///
/// - **Speed (スピード)**: A
///   - MIDI入力は即座に処理 (<1ms)
///   - リアルタイム性が高い
///   - 遅延が許されない用途 (演奏、ライブ操作)
///
/// - **Range (射程距離)**: C
///   - USBローカル接続が基本
///   - 将来的にネットワークMIDI対応可能
///   - 複数デバイス同時接続は可能だが、現状は単一デバイス想定
///
/// - **Stamina (持続力)**: A
///   - MIDI接続は安定。長時間稼働可能。
///   - メモリリークなし、リソース消費少
///   - デーモンとして常駐可能
///
/// - **Precision (精密動作性)**: B
///   - MIDIプロトコル自体は確実 (7bit精度)
///   - デバイス固有の挙動差異あり
///   - SysEx解析は実装依存
///
/// - **Potential (成長性)**: B
///   - 新規デバイス定義を追加可能
///   - デバイスプロファイル (JSON/TOML) で拡張
///   - LEDフィードバック、SysExカスタマイズ可能
pub const MIDI_CAPABILITY_PARAMS: CapabilityParams = CapabilityParams {
    power: Rank::D,
    speed: Rank::A,
    range: Rank::C,
    stamina: Rank::A,
    precision: Rank::B,
    potential: Rank::B,
};

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rank_ordering() {
        assert!(Rank::A > Rank::B);
        assert!(Rank::B > Rank::C);
        assert!(Rank::C > Rank::D);
        assert!(Rank::D > Rank::E);
        assert!(Rank::E > Rank::None);
    }

    #[test]
    fn test_rank_score() {
        assert_eq!(Rank::A.to_score(), 5);
        assert_eq!(Rank::B.to_score(), 4);
        assert_eq!(Rank::C.to_score(), 3);
        assert_eq!(Rank::D.to_score(), 2);
        assert_eq!(Rank::E.to_score(), 1);
        assert_eq!(Rank::None.to_score(), 0);
        assert_eq!(Rank::Unknown.to_score(), 0);
    }

    #[test]
    fn test_rank_from_score() {
        assert_eq!(Rank::from_score(0), Rank::None);
        assert_eq!(Rank::from_score(10), Rank::E);
        assert_eq!(Rank::from_score(30), Rank::D);
        assert_eq!(Rank::from_score(50), Rank::C);
        assert_eq!(Rank::from_score(70), Rank::B);
        assert_eq!(Rank::from_score(90), Rank::A);
        assert_eq!(Rank::from_score(200), Rank::Unknown);
    }

    #[test]
    fn test_balanced_params() {
        let params = CapabilityParams::balanced();
        assert_eq!(params.total_score(), 18); // C×6 = 3×6 = 18
    }

    #[test]
    fn test_midi_capability_params() {
        // MIDI能力パラメータの妥当性チェック
        let params = MIDI_CAPABILITY_PARAMS;

        // スピードと持続力が高いことを確認
        assert_eq!(params.speed, Rank::A);
        assert_eq!(params.stamina, Rank::A);

        // 破壊力は低め
        assert_eq!(params.power, Rank::D);

        // 総合スコアは17 (D=2, A=5, C=3, A=5, B=4, B=4)
        assert_eq!(params.total_score(), 23);
    }

    #[test]
    fn test_params_display() {
        let params = MIDI_CAPABILITY_PARAMS;
        let display = format!("{}", params);

        // 日本語パラメータ名が含まれることを確認
        assert!(display.contains("破壊力"));
        assert!(display.contains("スピード"));
        assert!(display.contains("Total Score"));
    }

    #[test]
    fn test_params_as_array() {
        let params = MIDI_CAPABILITY_PARAMS;
        let array = params.as_array();

        assert_eq!(array.len(), 6);
        assert_eq!(array[0], Rank::D); // Power
        assert_eq!(array[1], Rank::A); // Speed
    }

    #[test]
    fn test_serialization() {
        let params = MIDI_CAPABILITY_PARAMS;

        // JSON シリアライズ
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"power\":\"D\""));
        assert!(json.contains("\"speed\":\"A\""));

        // JSON デシリアライズ
        let deserialized: CapabilityParams = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, params);
    }
}
