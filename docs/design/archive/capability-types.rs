//! Stand Capability Type System
//!
//! AIエージェント能力の分類体系。JoJoスタンドの分類を参考にしつつ、
//! AI協働開発プラットフォームに適した軸で能力を整理する。
//!
//! ## 設計背景
//!
//! JoJoスタンドの分類軸:
//! - **射程距離**: 近距離パワー型、遠隔操作型、自動操縦型
//! - **操作法**: 任意型、半自律型、自動操縦型、独り歩き型
//! - **形態**: 人型、群体型、物質同化型、装着型
//!
//! AIエージェント能力の特性:
//! - **実行モデル**: 同期/非同期、リアルタイム性
//! - **自律性**: ユーザー主導/AI主導、判断の委任度
//! - **データフロー**: 入力/出力/双方向
//! - **統合形態**: スタンドアロン/協調/依存
//!
//! ## 設計原則
//!
//! 1. **多次元分類**: 単一の軸ではなく、複数の視点から能力を記述
//! 2. **段階的拡張**: Phase 1(トレイト) → Phase 2(プロトコル) → Phase 3(プラグイン)
//! 3. **型安全性**: Rust型システムで不正な組み合わせを防ぐ
//! 4. **実用性**: 100〜1000規模の能力を想定した設計

use serde::{Deserialize, Serialize};

// ============================================================================
// 実行モデル (Execution Model)
// ============================================================================

/// 能力の実行モデル
///
/// AIエージェント能力がどのように実行されるかを定義する。
/// リアルタイム性、応答速度、スループットの要件に影響する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionModel {
    /// 同期実行
    ///
    /// リクエストを受け取り、即座に結果を返す。
    /// ブロッキング処理を含む可能性がある。
    ///
    /// **特徴**:
    /// - レイテンシ: 低〜中
    /// - スループット: 低〜中
    /// - 適用例: 設定取得、状態確認、簡易計算
    Sync,

    /// 非同期実行（即時応答）
    ///
    /// リクエストを受け取り、非同期処理を開始後すぐに制御を返す。
    /// 結果はコールバック、イベント、Futureで取得する。
    ///
    /// **特徴**:
    /// - レイテンシ: 低（応答）、中〜高（完了）
    /// - スループット: 高
    /// - 適用例: ファイル操作、API呼び出し、Claude Agent通信
    Async,

    /// リアルタイムストリーム
    ///
    /// 継続的にデータを流し続ける。低レイテンシが求められる。
    ///
    /// **特徴**:
    /// - レイテンシ: 極低
    /// - スループット: 高
    /// - 適用例: MIDI入力、WebSocket通信、音声入力、センサーデータ
    Stream,

    /// バッチ処理
    ///
    /// まとまったデータを一括処理する。スループット重視。
    ///
    /// **特徴**:
    /// - レイテンシ: 高
    /// - スループット: 極高
    /// - 適用例: ログ解析、大量ファイル処理、定期レポート生成
    Batch,

    /// イベント駆動
    ///
    /// 外部イベントをトリガーとして実行される。
    /// 待機中はリソースを消費しない。
    ///
    /// **特徴**:
    /// - レイテンシ: 低〜中（イベント発生から実行まで）
    /// - スループット: 可変
    /// - 適用例: Git hook、ファイル監視、Webhook応答
    EventDriven,
}

// ============================================================================
// 自律性 (Autonomy Level)
// ============================================================================

/// 能力の自律性レベル
///
/// ユーザーとAIの間でどの程度の判断を委任するか。
/// JoJoスタンドの「操作法」分類に対応。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyLevel {
    /// 手動操作型（任意型）
    ///
    /// ユーザーの明示的な指示でのみ動作する。
    /// AIは実行手段を提供するが、判断はユーザーが行う。
    ///
    /// **制約**: ユーザー不在時は動作しない
    /// **強み**: 完全な制御、予測可能性
    /// **適用例**: ファイル読み書き、コマンド実行、設定変更
    Manual,

    /// 提案型（協調）
    ///
    /// AIが選択肢を提示し、ユーザーが選択する。
    /// Vantage Pointの「AI主導の選択肢UI」に対応。
    ///
    /// **制約**: 選択肢生成にAI推論が必要
    /// **強み**: ユーザーの意思決定を支援、学習機会
    /// **適用例**: コード修正候補、リファクタリング提案、次のアクション推薦
    Suggestive,

    /// 半自律型（委任）
    ///
    /// AIが判断・実行し、重要な分岐点でユーザーに確認を求める。
    /// Vantage Pointの「委任モード」に対応。
    ///
    /// **制約**: 判断基準の設定が必要
    /// **強み**: 効率的、定型作業の自動化
    /// **適用例**: テスト実行&修正、依存関係更新、コード整形
    SemiAutonomous,

    /// 完全自律型（自律）
    ///
    /// ユーザーの介入なしに目標達成まで実行する。
    /// JoJoスタンドの「独り歩き型」に対応。
    ///
    /// **制約**: 安全性検証が必須、エラーハンドリングが重要
    /// **強み**: 長時間実行可能、バックグラウンド処理
    /// **適用例**: CI/CD、定期レポート、監視&アラート
    FullyAutonomous,

    /// リアクティブ型（自動応答）
    ///
    /// 特定の条件・イベントに対して自動的に反応する。
    /// ユーザーは事前にルールを設定する。
    ///
    /// **制約**: ルール設計が重要、予期しない動作のリスク
    /// **強み**: 即応性、運用自動化
    /// **適用例**: Git pre-commit hook、ホットリロード、エラー検知&通知
    Reactive,
}

// ============================================================================
// データフロー方向 (Data Flow Direction)
// ============================================================================

/// 能力のデータフロー方向
///
/// 能力が入力を受け取るか、出力を提供するか、双方向か。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataFlowDirection {
    /// 入力のみ（センサー型）
    ///
    /// 外部からデータを受け取り、Stand内部に伝える。
    ///
    /// **適用例**: MIDI入力、音声入力、ファイル監視、Webhook受信
    Input,

    /// 出力のみ（アクチュエータ型）
    ///
    /// Stand内部から外部へデータを送信する。
    ///
    /// **適用例**: ファイル書き込み、API呼び出し、通知送信、MIDI出力
    Output,

    /// 双方向（プロトコル型）
    ///
    /// 入出力を繰り返しながら対話的に処理する。
    ///
    /// **適用例**: WebSocket通信、Claude Agent対話、MCP通信、SSH接続
    Bidirectional,

    /// 変換のみ（パイプライン型）
    ///
    /// 入力を受け取り、加工して出力する。外部状態は持たない。
    ///
    /// **適用例**: データ整形、フォーマット変換、プロトコル変換
    Transform,
}

// ============================================================================
// 統合形態 (Integration Mode)
// ============================================================================

/// 能力の統合形態
///
/// 他の能力やシステムとどのように統合されるか。
/// JoJoスタンドの「形態」分類に対応。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationMode {
    /// スタンドアロン型
    ///
    /// 単独で完結する能力。他の能力に依存しない。
    ///
    /// **特徴**: 独立性が高い、テストしやすい
    /// **適用例**: ファイル読み書き、設定管理、ログ出力
    Standalone,

    /// 協調型（群体型）
    ///
    /// 複数の能力と協調して動作する。
    /// EventBusを通じて他の能力とメッセージをやり取りする。
    ///
    /// **特徴**: 柔軟な組み合わせ、疎結合
    /// **適用例**: MIDI入力 → AG-UI表示、Agent応答 → WebSocket配信
    Collaborative,

    /// 依存型
    ///
    /// 特定の能力に強く依存する。単独では動作しない。
    ///
    /// **特徴**: 密結合、特化した機能
    /// **適用例**: LPD8デバイス設定（MidiCapabilityに依存）
    Dependent,

    /// 装着型（拡張）
    ///
    /// 既存の能力を拡張・修飾する。
    /// JoJoスタンドの「装着型」に対応。
    ///
    /// **特徴**: プラグイン的、機能追加
    /// **適用例**: ロギング、パフォーマンス計測、エラーハンドリング
    Extension,

    /// ブリッジ型
    ///
    /// 異なる能力間のプロトコル変換を行う。
    ///
    /// **特徴**: 相互運用性、抽象化
    /// **適用例**: MCP-to-WebSocket、MIDI-to-HTTP、CLI-to-API
    Bridge,
}

// ============================================================================
// 射程距離 (Operational Range)
// ============================================================================

/// 能力の動作範囲
///
/// 能力がどこまで到達できるか。JoJoスタンドの「射程距離」に対応。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalRange {
    /// ローカル
    ///
    /// Stand自身のプロセス内で完結する。
    ///
    /// **制約**: 外部リソースにアクセスできない
    /// **強み**: 高速、安全
    /// **適用例**: メモリ内計算、データ変換、状態管理
    Local,

    /// ホスト
    ///
    /// 同一マシン内で動作する。
    ///
    /// **制約**: ネットワーク越しにはアクセスできない
    /// **強み**: ファイルシステム、ローカルプロセスへのアクセス
    /// **適用例**: ファイル操作、ローカルDB、ホストコマンド実行
    Host,

    /// ネットワーク
    ///
    /// ネットワーク越しに外部サービスと通信する。
    ///
    /// **制約**: 通信遅延、ネットワーク障害
    /// **強み**: 外部API、リモートリソースの活用
    /// **適用例**: Claude API、GitHub API、Webhook送信
    Network,

    /// グローバル
    ///
    /// デバイス間で同期・共有される。
    ///
    /// **制約**: 同期遅延、競合解決
    /// **強み**: シームレスな継続、マルチデバイス対応
    /// **適用例**: セッション同期（Mac ↔ iPad）、CRDT状態共有
    Global,
}

// ============================================================================
// 能力タイプ定義 (Capability Type)
// ============================================================================

/// Stand Capability Type
///
/// 能力を多次元的に分類する型。
///
/// ## 設計思想
///
/// 単一の列挙型ではなく、複数の軸（フィールド）で能力を記述することで:
/// - より詳細な能力の特性表現
/// - 動的な能力検索・フィルタリング
/// - 将来の拡張性（新しい軸の追加）
///
/// ## 使用例
///
/// ```rust
/// use vantage_point::capability::types::*;
///
/// // MIDI入力能力
/// let midi_input = CapabilityType {
///     execution: ExecutionModel::Stream,
///     autonomy: AutonomyLevel::Reactive,
///     data_flow: DataFlowDirection::Input,
///     integration: IntegrationMode::Collaborative,
///     range: OperationalRange::Host,
/// };
///
/// // Claude Agent通信能力
/// let claude_agent = CapabilityType {
///     execution: ExecutionModel::Async,
///     autonomy: AutonomyLevel::Suggestive,
///     data_flow: DataFlowDirection::Bidirectional,
///     integration: IntegrationMode::Standalone,
///     range: OperationalRange::Network,
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CapabilityType {
    /// 実行モデル
    pub execution: ExecutionModel,

    /// 自律性レベル
    pub autonomy: AutonomyLevel,

    /// データフロー方向
    pub data_flow: DataFlowDirection,

    /// 統合形態
    pub integration: IntegrationMode,

    /// 動作範囲
    pub range: OperationalRange,
}

impl Default for CapabilityType {
    /// デフォルトは汎用的な能力タイプ
    /// - 非同期実行
    /// - リアクティブ（受け身）
    /// - 双方向データフロー
    /// - 協調型
    /// - ホスト範囲
    fn default() -> Self {
        Self {
            execution: ExecutionModel::Async,
            autonomy: AutonomyLevel::Reactive,
            data_flow: DataFlowDirection::Bidirectional,
            integration: IntegrationMode::Collaborative,
            range: OperationalRange::Host,
        }
    }
}

impl CapabilityType {
    /// MIDI入力能力の典型的な型定義
    ///
    /// - リアルタイムストリーム処理
    /// - イベント駆動で自動応答
    /// - 入力専門
    /// - 他の能力と協調（EventBusで配信）
    /// - ホスト範囲（USBデバイス）
    pub fn midi_input() -> Self {
        Self {
            execution: ExecutionModel::Stream,
            autonomy: AutonomyLevel::Reactive,
            data_flow: DataFlowDirection::Input,
            integration: IntegrationMode::Collaborative,
            range: OperationalRange::Host,
        }
    }

    /// Claude Agent能力の典型的な型定義
    ///
    /// - 非同期実行（API呼び出し）
    /// - 提案型（選択肢を提示）
    /// - 双方向対話
    /// - スタンドアロン
    /// - ネットワーク通信
    pub fn claude_agent() -> Self {
        Self {
            execution: ExecutionModel::Async,
            autonomy: AutonomyLevel::Suggestive,
            data_flow: DataFlowDirection::Bidirectional,
            integration: IntegrationMode::Standalone,
            range: OperationalRange::Network,
        }
    }

    /// WebSocket配信能力の典型的な型定義
    ///
    /// - リアルタイムストリーム
    /// - 手動操作（明示的なメッセージ送信）
    /// - 出力専門
    /// - ブリッジ型（内部イベント → WebSocket）
    /// - ローカル〜ネットワーク
    pub fn websocket_broadcast() -> Self {
        Self {
            execution: ExecutionModel::Stream,
            autonomy: AutonomyLevel::Manual,
            data_flow: DataFlowDirection::Output,
            integration: IntegrationMode::Bridge,
            range: OperationalRange::Network,
        }
    }

    /// ファイル監視能力の典型的な型定義
    ///
    /// - イベント駆動
    /// - リアクティブ（自動検知&通知）
    /// - 入力専門
    /// - 協調型
    /// - ホスト範囲
    pub fn file_watcher() -> Self {
        Self {
            execution: ExecutionModel::EventDriven,
            autonomy: AutonomyLevel::Reactive,
            data_flow: DataFlowDirection::Input,
            integration: IntegrationMode::Collaborative,
            range: OperationalRange::Host,
        }
    }

    /// 判断が必要かどうか
    ///
    /// `Suggestive`, `SemiAutonomous`, `FullyAutonomous`は
    /// AI推論・判断が必要。
    pub fn requires_ai_reasoning(&self) -> bool {
        matches!(
            self.autonomy,
            AutonomyLevel::Suggestive
                | AutonomyLevel::SemiAutonomous
                | AutonomyLevel::FullyAutonomous
        )
    }

    /// リアルタイム性が要求されるか
    ///
    /// `Stream`, `EventDriven`は低レイテンシが重要。
    pub fn is_realtime(&self) -> bool {
        matches!(
            self.execution,
            ExecutionModel::Stream | ExecutionModel::EventDriven
        )
    }

    /// 外部通信が必要か
    ///
    /// `Network`, `Global`はネットワーク通信を行う。
    pub fn requires_network(&self) -> bool {
        matches!(
            self.range,
            OperationalRange::Network | OperationalRange::Global
        )
    }

    /// 他の能力と協調するか
    ///
    /// `Collaborative`, `Bridge`はEventBusを使う。
    pub fn is_collaborative(&self) -> bool {
        matches!(
            self.integration,
            IntegrationMode::Collaborative | IntegrationMode::Bridge
        )
    }
}

// ============================================================================
// 能力の制約と強み (Constraints & Strengths)
// ============================================================================

/// 能力タイプごとの制約と強みを文字列で取得するヘルパー
impl CapabilityType {
    /// この能力タイプの主な制約を列挙
    pub fn constraints(&self) -> Vec<&'static str> {
        let mut result = Vec::new();

        match self.execution {
            ExecutionModel::Sync => result.push("ブロッキング処理の可能性"),
            ExecutionModel::Stream => result.push("継続的なリソース消費"),
            ExecutionModel::Batch => result.push("高レイテンシ"),
            _ => {}
        }

        match self.autonomy {
            AutonomyLevel::Manual => result.push("ユーザー不在時は動作しない"),
            AutonomyLevel::FullyAutonomous => result.push("安全性検証が必須"),
            AutonomyLevel::Reactive => result.push("ルール設計が重要"),
            _ => {}
        }

        match self.range {
            OperationalRange::Network | OperationalRange::Global => {
                result.push("通信遅延・障害のリスク")
            }
            _ => {}
        }

        match self.integration {
            IntegrationMode::Dependent => result.push("他の能力への依存"),
            IntegrationMode::Collaborative => result.push("EventBusへの依存"),
            _ => {}
        }

        result
    }

    /// この能力タイプの主な強みを列挙
    pub fn strengths(&self) -> Vec<&'static str> {
        let mut result = Vec::new();

        match self.execution {
            ExecutionModel::Sync => result.push("シンプルな制御フロー"),
            ExecutionModel::Async => result.push("高スループット"),
            ExecutionModel::Stream => result.push("低レイテンシ"),
            ExecutionModel::Batch => result.push("大量データ処理"),
            ExecutionModel::EventDriven => result.push("リソース効率"),
        }

        match self.autonomy {
            AutonomyLevel::Manual => result.push("完全な制御、予測可能"),
            AutonomyLevel::Suggestive => result.push("意思決定支援"),
            AutonomyLevel::SemiAutonomous => result.push("効率的な自動化"),
            AutonomyLevel::FullyAutonomous => result.push("長時間実行可能"),
            AutonomyLevel::Reactive => result.push("即応性"),
        }

        match self.integration {
            IntegrationMode::Standalone => result.push("独立性、テストしやすい"),
            IntegrationMode::Collaborative => result.push("柔軟な組み合わせ"),
            IntegrationMode::Extension => result.push("既存機能の拡張"),
            _ => {}
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_midi_input_characteristics() {
        let midi = CapabilityType::midi_input();

        assert!(midi.is_realtime());
        assert!(midi.is_collaborative());
        assert!(!midi.requires_network());
        assert!(!midi.requires_ai_reasoning());
    }

    #[test]
    fn test_claude_agent_characteristics() {
        let agent = CapabilityType::claude_agent();

        assert!(agent.requires_ai_reasoning());
        assert!(agent.requires_network());
        assert!(!agent.is_realtime());
        assert!(!agent.is_collaborative());
    }

    #[test]
    fn test_constraints_and_strengths() {
        let midi = CapabilityType::midi_input();

        assert!(!midi.constraints().is_empty());
        assert!(!midi.strengths().is_empty());
    }
}
