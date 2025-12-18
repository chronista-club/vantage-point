//! Capability Synergy System
//!
//! JoJo's Bizarre Adventure のスタンド能力連携システムにインスパイアされた、
//! Stand Capability間の協調システム。
//!
//! ## コンセプト
//!
//! JoJoスタンドは単独でも強力だが、複数のスタンドが連携することで
//! さらに強力な効果を発揮する。Vantage Pointにおいても同様に、
//! 各Capabilityは単独で機能するが、連携することでより強力な開発体験を生み出す。
//!
//! ### 参考: JoJoスタンド連携パターン
//!
//! 1. **弱点補完型**: 一方の弱点を他方が補う（例: 攻撃特化 + 防御特化）
//! 2. **情報共有型**: 偵察役 + 戦闘役の分業（例: ハイエロファント・グリーン + スターフィナム）
//! 3. **条件達成型**: 前提条件を他者が作る（例: 時間停止中に攻撃を仕込む）
//! 4. **相性システム**: 連携効率、禁忌組み合わせ
//!
//! ## 設計方針
//!
//! - **Requires/Provides**: 能力間の依存関係を明示的に定義
//! - **Compatibility Score**: 相性スコアで連携効率を数値化
//! - **Forbidden Combinations**: 禁忌組み合わせを定義可能
//! - **Event-Driven**: イベント駆動で疎結合な連携
//!
//! ## 実装例: MIDI + Claude Agent
//!
//! ```rust,ignore
//! // MIDICapabilityが入力イベントを提供
//! midi_capability.provides = vec![CapabilityTag::UserInput, CapabilityTag::HardwareTrigger];
//!
//! // ClaudeAgentCapabilityが入力を必要とする
//! agent_capability.requires = vec![CapabilityTag::UserInput];
//!
//! // 相性スコア: 90/100 (MIDI入力はAgent操作に最適)
//! let synergy = SynergyEngine::calculate(&midi_capability, &agent_capability);
//! assert_eq!(synergy.compatibility, 90);
//! ```

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// =============================================================================
// Capability Tags
// =============================================================================

/// 能力が提供/要求する機能のタグ
///
/// JoJoスタンドの「能力タイプ」に相当。各Capabilityが持つ特性を分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityTag {
    // === Input/Output ===
    /// ユーザー入力を提供（キーボード、MIDI、音声など）
    UserInput,
    /// ハードウェアトリガー（物理ボタン、MIDI、センサー）
    HardwareTrigger,
    /// 視覚的フィードバック（LED、画面表示）
    VisualFeedback,
    /// 音声フィードバック（TTS、サウンド）
    AudioFeedback,

    // === AI/Agent ===
    /// AIエージェント機能（Claude、ChatGPT等）
    AiAgent,
    /// 自然言語処理
    NaturalLanguage,
    /// コード生成・解析
    CodeGeneration,
    /// 思考・推論
    Reasoning,

    // === Development ===
    /// コード実行環境
    CodeExecution,
    /// ファイルシステムアクセス
    FileSystem,
    /// Git操作
    GitOperations,
    /// パッケージ管理
    PackageManagement,

    // === Communication ===
    /// WebSocket通信
    WebSocket,
    /// HTTP API
    HttpApi,
    /// プロセス間通信
    Ipc,

    // === State Management ===
    /// セッション管理
    SessionManagement,
    /// 永続化ストレージ
    PersistentStorage,
    /// メモリキャッシュ
    MemoryCache,

    // === UI/UX ===
    /// WebView/ブラウザUI
    WebViewUi,
    /// CLI出力
    CliOutput,
    /// 通知システム
    Notification,

    // === Context Awareness ===
    /// プロジェクト構造理解
    ProjectContext,
    /// コードベース解析
    CodebaseAnalysis,
    /// 開発履歴追跡
    DevelopmentHistory,
}

// =============================================================================
// Capability Metadata
// =============================================================================

/// Capability メタデータ
///
/// 各能力が持つ特性、依存関係、提供機能を定義。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityMetadata {
    /// 能力の一意識別子
    pub id: String,
    /// 能力の表示名
    pub name: String,
    /// 能力の説明
    pub description: String,
    /// この能力が提供する機能タグ
    pub provides: Vec<CapabilityTag>,
    /// この能力が依存する機能タグ
    pub requires: Vec<CapabilityTag>,
    /// この能力と相性が良いタグ（ボーナス）
    pub synergizes_with: Vec<CapabilityTag>,
    /// この能力と相性が悪いタグ（ペナルティ）
    pub conflicts_with: Vec<CapabilityTag>,
    /// 禁忌組み合わせ（絶対に同時使用不可）
    pub forbidden_with: Vec<String>,
}

impl CapabilityMetadata {
    /// 新しいCapabilityメタデータを作成
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            provides: Vec::new(),
            requires: Vec::new(),
            synergizes_with: Vec::new(),
            conflicts_with: Vec::new(),
            forbidden_with: Vec::new(),
        }
    }

    /// 説明を設定
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// 提供機能を設定
    pub fn provides(mut self, tags: Vec<CapabilityTag>) -> Self {
        self.provides = tags;
        self
    }

    /// 依存機能を設定
    pub fn requires(mut self, tags: Vec<CapabilityTag>) -> Self {
        self.requires = tags;
        self
    }

    /// 相性良好タグを設定
    pub fn synergizes_with(mut self, tags: Vec<CapabilityTag>) -> Self {
        self.synergizes_with = tags;
        self
    }

    /// 相性不良タグを設定
    pub fn conflicts_with(mut self, tags: Vec<CapabilityTag>) -> Self {
        self.conflicts_with = tags;
        self
    }

    /// 禁忌能力を設定
    pub fn forbidden_with(mut self, ids: Vec<String>) -> Self {
        self.forbidden_with = ids;
        self
    }
}

// =============================================================================
// Synergy Analysis
// =============================================================================

/// 能力間の連携分析結果
#[derive(Debug, Clone)]
pub struct SynergyAnalysis {
    /// 連携する能力のID
    pub capability_a: String,
    pub capability_b: String,
    /// 相性スコア (0-100)
    /// - 100: 完璧な連携
    /// - 75-99: 非常に相性が良い
    /// - 50-74: 相性が良い
    /// - 25-49: 中立
    /// - 1-24: 相性が悪い
    /// - 0: 連携不可
    pub compatibility: u8,
    /// 依存関係が満たされているか
    pub dependencies_met: bool,
    /// 禁忌組み合わせか
    pub is_forbidden: bool,
    /// 連携のタイプ
    pub synergy_type: SynergyType,
    /// 連携の詳細説明
    pub description: String,
}

/// 連携のタイプ
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SynergyType {
    /// 弱点補完型: 一方の弱点を他方が補う
    Complementary,
    /// 情報共有型: 偵察役 + 戦闘役の分業
    InformationSharing,
    /// 条件達成型: 前提条件を他者が作る
    Prerequisite,
    /// 増幅型: 両方の能力が相乗効果で強化される
    Amplification,
    /// 独立型: 連携効果なし（並列実行のみ）
    Independent,
    /// 競合型: 同時使用で効率が落ちる
    Conflicting,
    /// 禁忌型: 絶対に同時使用不可
    Forbidden,
}

// =============================================================================
// Synergy Engine
// =============================================================================

/// 能力連携エンジン
///
/// Capability間の相性分析、依存関係チェック、最適な組み合わせの提案を行う。
pub struct SynergyEngine {
    /// 登録されている能力のメタデータ
    capabilities: HashMap<String, CapabilityMetadata>,
    /// 能力間の連携スコアキャッシュ
    synergy_cache: HashMap<(String, String), SynergyAnalysis>,
}

impl SynergyEngine {
    /// 新しいSynergyEngineを作成
    pub fn new() -> Self {
        Self {
            capabilities: HashMap::new(),
            synergy_cache: HashMap::new(),
        }
    }

    /// 能力を登録
    pub fn register(&mut self, metadata: CapabilityMetadata) {
        self.capabilities.insert(metadata.id.clone(), metadata);
        // キャッシュをクリア（依存関係が変わる可能性があるため）
        self.synergy_cache.clear();
    }

    /// 2つの能力の連携を分析
    pub fn analyze(&mut self, id_a: &str, id_b: &str) -> Option<SynergyAnalysis> {
        // キャッシュをチェック
        let cache_key = (id_a.to_string(), id_b.to_string());
        if let Some(cached) = self.synergy_cache.get(&cache_key) {
            return Some(cached.clone());
        }

        let cap_a = self.capabilities.get(id_a)?;
        let cap_b = self.capabilities.get(id_b)?;

        let analysis = self.calculate_synergy(cap_a, cap_b);
        self.synergy_cache.insert(cache_key, analysis.clone());
        Some(analysis)
    }

    /// 能力が必要とする依存関係を満たす能力をリストアップ
    pub fn find_dependencies(&self, id: &str) -> Vec<String> {
        let cap = match self.capabilities.get(id) {
            Some(c) => c,
            None => return Vec::new(),
        };

        let required_tags: HashSet<_> = cap.requires.iter().copied().collect();
        let mut providers = Vec::new();

        for (provider_id, provider) in &self.capabilities {
            if provider_id == id {
                continue; // 自分自身は除外
            }

            let provided_tags: HashSet<_> = provider.provides.iter().copied().collect();
            if !required_tags.is_disjoint(&provided_tags) {
                providers.push(provider_id.clone());
            }
        }

        providers
    }

    /// 最適な能力組み合わせを提案
    pub fn suggest_combinations(
        &mut self,
        base_capability: &str,
        limit: usize,
    ) -> Vec<SynergyAnalysis> {
        let mut results = Vec::new();

        // キーを先に収集してからイテレート（借用問題を回避）
        let capability_ids: Vec<String> = self.capabilities.keys().cloned().collect();

        for id in capability_ids {
            if id == base_capability {
                continue;
            }

            if let Some(analysis) = self.analyze(base_capability, &id) {
                if analysis.compatibility > 50 && !analysis.is_forbidden {
                    results.push(analysis);
                }
            }
        }

        // スコアでソート
        results.sort_by(|a, b| b.compatibility.cmp(&a.compatibility));
        results.truncate(limit);
        results
    }

    /// 能力の連携スコアを計算
    fn calculate_synergy(
        &self,
        cap_a: &CapabilityMetadata,
        cap_b: &CapabilityMetadata,
    ) -> SynergyAnalysis {
        // 禁忌チェック
        let is_forbidden = cap_a.forbidden_with.contains(&cap_b.id)
            || cap_b.forbidden_with.contains(&cap_a.id);

        if is_forbidden {
            return SynergyAnalysis {
                capability_a: cap_a.id.clone(),
                capability_b: cap_b.id.clone(),
                compatibility: 0,
                dependencies_met: false,
                is_forbidden: true,
                synergy_type: SynergyType::Forbidden,
                description: "禁忌組み合わせ: 同時使用不可".to_string(),
            };
        }

        // 依存関係チェック
        let a_provides: HashSet<_> = cap_a.provides.iter().copied().collect();
        let b_provides: HashSet<_> = cap_b.provides.iter().copied().collect();
        let a_requires: HashSet<_> = cap_a.requires.iter().copied().collect();
        let b_requires: HashSet<_> = cap_b.requires.iter().copied().collect();

        let a_satisfied = a_requires.is_subset(&b_provides) || a_requires.is_empty();
        let b_satisfied = b_requires.is_subset(&a_provides) || b_requires.is_empty();
        let dependencies_met = a_satisfied && b_satisfied;

        // 相性スコア計算
        let mut score = 50; // ベーススコア

        // 依存関係が満たされている場合はボーナス
        if !a_requires.is_empty() && a_requires.is_subset(&b_provides) {
            score += 25; // Bが Aの依存を満たす
        }
        if !b_requires.is_empty() && b_requires.is_subset(&a_provides) {
            score += 25; // Aが Bの依存を満たす
        }

        // 相性良好タグのマッチ
        let a_synergizes: HashSet<_> = cap_a.synergizes_with.iter().copied().collect();
        let b_synergizes: HashSet<_> = cap_b.synergizes_with.iter().copied().collect();

        let synergy_matches = a_synergizes.intersection(&b_provides).count()
            + b_synergizes.intersection(&a_provides).count();

        score += (synergy_matches as u8).saturating_mul(10).min(30);

        // 競合タグのペナルティ
        let a_conflicts: HashSet<_> = cap_a.conflicts_with.iter().copied().collect();
        let b_conflicts: HashSet<_> = cap_b.conflicts_with.iter().copied().collect();

        let conflict_matches = a_conflicts.intersection(&b_provides).count()
            + b_conflicts.intersection(&a_provides).count();

        score = score.saturating_sub((conflict_matches as u8).saturating_mul(15));

        // 上限チェック
        score = score.min(100);

        // 実際に依存関係が満たされているか（空の要件は除外）
        let a_has_deps_from_b = !a_requires.is_empty() && a_requires.is_subset(&b_provides);
        let b_has_deps_from_a = !b_requires.is_empty() && b_requires.is_subset(&a_provides);
        let has_actual_dependency = a_has_deps_from_b || b_has_deps_from_a;

        // 連携タイプを判定
        let synergy_type = if !dependencies_met {
            SynergyType::Conflicting
        } else if synergy_matches > 0 {
            if has_actual_dependency {
                SynergyType::Prerequisite
            } else {
                SynergyType::Amplification
            }
        } else if has_actual_dependency {
            SynergyType::Complementary
        } else if !a_provides.is_disjoint(&b_provides) {
            SynergyType::InformationSharing
        } else {
            SynergyType::Independent
        };

        let description = self.generate_description(cap_a, cap_b, synergy_type, score);

        SynergyAnalysis {
            capability_a: cap_a.id.clone(),
            capability_b: cap_b.id.clone(),
            compatibility: score,
            dependencies_met,
            is_forbidden: false,
            synergy_type,
            description,
        }
    }

    /// 連携の説明文を生成
    fn generate_description(
        &self,
        cap_a: &CapabilityMetadata,
        cap_b: &CapabilityMetadata,
        synergy_type: SynergyType,
        score: u8,
    ) -> String {
        let quality = match score {
            90..=100 => "完璧な連携",
            75..=89 => "非常に相性が良い",
            50..=74 => "相性が良い",
            25..=49 => "中立的な関係",
            _ => "相性が悪い",
        };

        let type_desc = match synergy_type {
            SynergyType::Complementary => {
                format!("{}が{}の弱点を補完", cap_b.name, cap_a.name)
            }
            SynergyType::InformationSharing => {
                format!("{}と{}が情報を共有", cap_a.name, cap_b.name)
            }
            SynergyType::Prerequisite => {
                format!("{}が{}の前提条件を提供", cap_b.name, cap_a.name)
            }
            SynergyType::Amplification => {
                format!("{}と{}が相乗効果で強化", cap_a.name, cap_b.name)
            }
            SynergyType::Independent => "独立して並列実行可能".to_string(),
            SynergyType::Conflicting => "同時使用で効率が低下".to_string(),
            SynergyType::Forbidden => "禁忌組み合わせ".to_string(),
        };

        format!("{} - {}", quality, type_desc)
    }
}

impl Default for SynergyEngine {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Pre-defined Capability Metadata
// =============================================================================

/// MIDI入力能力のメタデータ
pub fn midi_capability() -> CapabilityMetadata {
    CapabilityMetadata::new("midi_input", "MIDI Input")
        .with_description("MIDI機器からの入力イベントを受信・処理")
        .provides(vec![
            CapabilityTag::UserInput,
            CapabilityTag::HardwareTrigger,
        ])
        .requires(vec![])
        .synergizes_with(vec![
            CapabilityTag::AiAgent,
            CapabilityTag::VisualFeedback,
            CapabilityTag::SessionManagement,
        ])
        .conflicts_with(vec![])
}

/// Claude Agent能力のメタデータ
pub fn claude_agent_capability() -> CapabilityMetadata {
    CapabilityMetadata::new("claude_agent", "Claude Agent")
        .with_description("Claude CLIを介したAIエージェント機能")
        .provides(vec![
            CapabilityTag::AiAgent,
            CapabilityTag::NaturalLanguage,
            CapabilityTag::CodeGeneration,
            CapabilityTag::Reasoning,
        ])
        .requires(vec![CapabilityTag::UserInput])
        .synergizes_with(vec![
            CapabilityTag::FileSystem,
            CapabilityTag::GitOperations,
            CapabilityTag::CodeExecution,
            CapabilityTag::ProjectContext,
        ])
        .conflicts_with(vec![])
}

/// WebView UI能力のメタデータ
pub fn webview_capability() -> CapabilityMetadata {
    CapabilityMetadata::new("webview_ui", "WebView UI")
        .with_description("WebViewベースのユーザーインターフェース")
        .provides(vec![
            CapabilityTag::WebViewUi,
            CapabilityTag::VisualFeedback,
            CapabilityTag::Notification,
        ])
        .requires(vec![
            CapabilityTag::WebSocket,
        ])
        .synergizes_with(vec![
            CapabilityTag::AiAgent,
            CapabilityTag::SessionManagement,
        ])
        .conflicts_with(vec![CapabilityTag::CliOutput])
}

/// WebSocket通信能力のメタデータ
pub fn websocket_capability() -> CapabilityMetadata {
    CapabilityMetadata::new("websocket_comm", "WebSocket Communication")
        .with_description("リアルタイムな双方向通信")
        .provides(vec![
            CapabilityTag::WebSocket,
            CapabilityTag::Ipc,
        ])
        .requires(vec![])
        .synergizes_with(vec![
            CapabilityTag::WebViewUi,
            CapabilityTag::AiAgent,
        ])
        .conflicts_with(vec![])
}

/// セッション管理能力のメタデータ
pub fn session_management_capability() -> CapabilityMetadata {
    CapabilityMetadata::new("session_mgmt", "Session Management")
        .with_description("複数のAgentセッションを管理")
        .provides(vec![
            CapabilityTag::SessionManagement,
            CapabilityTag::PersistentStorage,
            CapabilityTag::DevelopmentHistory,
        ])
        .requires(vec![
            CapabilityTag::AiAgent,
        ])
        .synergizes_with(vec![
            CapabilityTag::ProjectContext,
            CapabilityTag::WebViewUi,
        ])
        .conflicts_with(vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_metadata_builder() {
        let cap = CapabilityMetadata::new("test_cap", "Test Capability")
            .with_description("A test capability")
            .provides(vec![CapabilityTag::UserInput])
            .requires(vec![CapabilityTag::AiAgent]);

        assert_eq!(cap.id, "test_cap");
        assert_eq!(cap.name, "Test Capability");
        assert_eq!(cap.provides.len(), 1);
        assert_eq!(cap.requires.len(), 1);
    }

    #[test]
    fn test_synergy_analysis_midi_and_agent() {
        let mut engine = SynergyEngine::new();
        engine.register(midi_capability());
        engine.register(claude_agent_capability());

        let analysis = engine.analyze("midi_input", "claude_agent").unwrap();

        // MIDIがUserInputを提供し、AgentがUserInputを必要とする
        assert!(analysis.dependencies_met);
        assert_eq!(analysis.synergy_type, SynergyType::Prerequisite);
        assert!(analysis.compatibility >= 75); // 高い相性
    }

    #[test]
    fn test_synergy_analysis_webview_and_websocket() {
        let mut engine = SynergyEngine::new();
        engine.register(webview_capability());
        engine.register(websocket_capability());

        let analysis = engine.analyze("webview_ui", "websocket_comm").unwrap();

        // WebViewがWebSocketを必要とし、WebSocketがそれを提供
        assert!(analysis.dependencies_met);
        assert!(analysis.compatibility >= 75);
    }

    #[test]
    fn test_synergy_analysis_independent() {
        let mut engine = SynergyEngine::new();

        let cap_a = CapabilityMetadata::new("cap_a", "Capability A")
            .provides(vec![CapabilityTag::UserInput])
            .requires(vec![]);

        let cap_b = CapabilityMetadata::new("cap_b", "Capability B")
            .provides(vec![CapabilityTag::FileSystem])
            .requires(vec![]);

        engine.register(cap_a);
        engine.register(cap_b);

        let analysis = engine.analyze("cap_a", "cap_b").unwrap();

        // 依存関係なし、相性タグなし → Independent
        assert_eq!(analysis.synergy_type, SynergyType::Independent);
        assert!(analysis.compatibility >= 40);
        assert!(analysis.compatibility <= 60);
    }

    #[test]
    fn test_synergy_analysis_forbidden() {
        let mut engine = SynergyEngine::new();

        let cap_a = CapabilityMetadata::new("cap_a", "Capability A")
            .provides(vec![CapabilityTag::UserInput])
            .forbidden_with(vec!["cap_b".to_string()]);

        let cap_b = CapabilityMetadata::new("cap_b", "Capability B")
            .provides(vec![CapabilityTag::UserInput]);

        engine.register(cap_a);
        engine.register(cap_b);

        let analysis = engine.analyze("cap_a", "cap_b").unwrap();

        assert!(analysis.is_forbidden);
        assert_eq!(analysis.compatibility, 0);
        assert_eq!(analysis.synergy_type, SynergyType::Forbidden);
    }

    #[test]
    fn test_find_dependencies() {
        let mut engine = SynergyEngine::new();
        engine.register(midi_capability());
        engine.register(claude_agent_capability());
        engine.register(websocket_capability());

        let deps = engine.find_dependencies("claude_agent");

        // Claude Agentは UserInput を必要とし、MIDIがそれを提供
        assert!(deps.contains(&"midi_input".to_string()));
    }

    #[test]
    fn test_suggest_combinations() {
        let mut engine = SynergyEngine::new();
        engine.register(midi_capability());
        engine.register(claude_agent_capability());
        engine.register(webview_capability());
        engine.register(websocket_capability());
        engine.register(session_management_capability());

        let suggestions = engine.suggest_combinations("claude_agent", 3);

        // Claude Agentと相性の良い能力が提案される
        assert!(!suggestions.is_empty());
        for suggestion in suggestions {
            assert!(suggestion.compatibility > 50);
            assert!(!suggestion.is_forbidden);
        }
    }

    #[test]
    fn test_synergy_cache() {
        let mut engine = SynergyEngine::new();
        engine.register(midi_capability());
        engine.register(claude_agent_capability());

        // 初回分析
        let analysis1 = engine.analyze("midi_input", "claude_agent").unwrap();

        // 2回目はキャッシュから取得
        let analysis2 = engine.analyze("midi_input", "claude_agent").unwrap();

        assert_eq!(analysis1.compatibility, analysis2.compatibility);
    }
}
