//! Gold Experience - 創造と回復のスタンド
//!
//! JoJo Part 5 ジョルノ・ジョバーナのスタンドにちなんだ命名。
//! 「生命を与える」能力をコード生成・修復に応用。
//!
//! ## 能力
//! - **Scaffold** (生命を与える): プロジェクト/コードの雛形生成
//! - **Heal** (回復): 自動修復（lint fix, format, etc.）
//! - **Grow** (成長性A): 学習・パターン認識（将来実装）

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Gold Experience の設定
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeConfig {
    /// テンプレートディレクトリ
    pub template_dir: Option<PathBuf>,
    /// 自動修復を有効にするか
    pub auto_heal: bool,
    /// 成長モード（学習を有効にするか）
    pub growth_mode: bool,
}

/// Scaffold テンプレートの種類
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TemplateKind {
    /// Rust プロジェクト
    RustProject,
    /// Rust モジュール
    RustModule,
    /// TypeScript プロジェクト
    TypeScriptProject,
    /// React コンポーネント
    ReactComponent,
    /// カスタムテンプレート
    Custom(String),
}

impl std::fmt::Display for TemplateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateKind::RustProject => write!(f, "rust-project"),
            TemplateKind::RustModule => write!(f, "rust-module"),
            TemplateKind::TypeScriptProject => write!(f, "typescript-project"),
            TemplateKind::ReactComponent => write!(f, "react-component"),
            TemplateKind::Custom(name) => write!(f, "custom:{}", name),
        }
    }
}

/// Scaffold 結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldResult {
    /// 生成されたファイル一覧
    pub files: Vec<PathBuf>,
    /// テンプレート種類
    pub template: TemplateKind,
    /// 生成先ディレクトリ
    pub output_dir: PathBuf,
}

/// Heal（修復）アクションの種類
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HealAction {
    /// フォーマット（rustfmt, prettier など）
    Format,
    /// Lint 修正（clippy --fix, eslint --fix など）
    LintFix,
    /// 依存関係の修復
    FixDependencies,
    /// インポートの整理
    OrganizeImports,
    /// 全ての修復を実行
    All,
}

impl std::fmt::Display for HealAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealAction::Format => write!(f, "format"),
            HealAction::LintFix => write!(f, "lint-fix"),
            HealAction::FixDependencies => write!(f, "fix-deps"),
            HealAction::OrganizeImports => write!(f, "organize-imports"),
            HealAction::All => write!(f, "all"),
        }
    }
}

/// Heal 結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealResult {
    /// 実行したアクション
    pub action: HealAction,
    /// 修正されたファイル数
    pub files_fixed: usize,
    /// 修正内容の概要
    pub summary: String,
    /// 成功したか
    pub success: bool,
}

/// プロジェクト検出結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// プロジェクトの種類
    pub kind: ProjectKind,
    /// ルートディレクトリ
    pub root: PathBuf,
    /// パッケージマネージャ
    pub package_manager: Option<String>,
}

/// プロジェクトの種類
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ProjectKind {
    Rust,
    Node,
    Python,
    Go,
    Unknown,
}

impl std::fmt::Display for ProjectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectKind::Rust => write!(f, "rust"),
            ProjectKind::Node => write!(f, "node"),
            ProjectKind::Python => write!(f, "python"),
            ProjectKind::Go => write!(f, "go"),
            ProjectKind::Unknown => write!(f, "unknown"),
        }
    }
}

/// Gold Experience 本体
pub struct GoldExperience {
    config: GeConfig,
    /// 登録済みテンプレート
    templates: Arc<RwLock<HashMap<String, Template>>>,
    /// 成長データ（学習結果）
    growth_data: Arc<RwLock<GrowthData>>,
}

/// テンプレート定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    /// テンプレート名
    pub name: String,
    /// 説明
    pub description: String,
    /// ファイル一覧（相対パス -> 内容）
    pub files: HashMap<String, String>,
    /// 変数（プレースホルダー）
    pub variables: Vec<String>,
}

/// 成長データ（学習結果）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GrowthData {
    /// よく使うパターン
    pub patterns: Vec<String>,
    /// 修復履歴
    pub heal_history: Vec<HealRecord>,
    /// スキャフォールド履歴
    pub scaffold_history: Vec<ScaffoldRecord>,
}

/// 修復履歴レコード
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealRecord {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub action: HealAction,
    pub project_kind: ProjectKind,
    pub success: bool,
}

/// スキャフォールド履歴レコード
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldRecord {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub template: String,
    pub output_dir: PathBuf,
}

impl Default for GoldExperience {
    fn default() -> Self {
        Self::new(GeConfig::default())
    }
}

impl GoldExperience {
    /// 新しい Gold Experience を作成
    pub fn new(config: GeConfig) -> Self {
        let mut templates = HashMap::new();

        // 組み込みテンプレートを登録
        templates.insert("rust-module".to_string(), Self::builtin_rust_module());
        templates.insert("rust-project".to_string(), Self::builtin_rust_project());

        Self {
            config,
            templates: Arc::new(RwLock::new(templates)),
            growth_data: Arc::new(RwLock::new(GrowthData::default())),
        }
    }

    /// 組み込み: Rust モジュールテンプレート
    fn builtin_rust_module() -> Template {
        let mut files = HashMap::new();
        files.insert(
            "{{name}}.rs".to_string(),
            r#"//! {{name}} モジュール
//!
//! {{description}}

use anyhow::Result;

/// {{name}} の設定
#[derive(Debug, Clone, Default)]
pub struct {{Name}}Config {
    // TODO: 設定項目を追加
}

/// {{name}} 本体
pub struct {{Name}} {
    config: {{Name}}Config,
}

impl {{Name}} {
    /// 新しい {{Name}} を作成
    pub fn new(config: {{Name}}Config) -> Self {
        Self { config }
    }

    /// 初期化
    pub async fn init(&self) -> Result<()> {
        // TODO: 実装
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let config = {{Name}}Config::default();
        let _instance = {{Name}}::new(config);
    }
}
"#
            .to_string(),
        );

        Template {
            name: "rust-module".to_string(),
            description: "Rust モジュールの雛形".to_string(),
            files,
            variables: vec![
                "name".to_string(),
                "Name".to_string(),
                "description".to_string(),
            ],
        }
    }

    /// 組み込み: Rust プロジェクトテンプレート
    fn builtin_rust_project() -> Template {
        let mut files = HashMap::new();

        files.insert(
            "Cargo.toml".to_string(),
            r#"[package]
name = "{{name}}"
version = "0.1.0"
edition = "2021"
description = "{{description}}"

[dependencies]
anyhow = "1"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
"#
            .to_string(),
        );

        files.insert(
            "src/main.rs".to_string(),
            r#"//! {{name}}
//!
//! {{description}}

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Hello from {{name}}!");
    Ok(())
}
"#
            .to_string(),
        );

        files.insert(
            ".gitignore".to_string(),
            r#"/target
Cargo.lock
"#
            .to_string(),
        );

        Template {
            name: "rust-project".to_string(),
            description: "Rust プロジェクトの雛形".to_string(),
            files,
            variables: vec!["name".to_string(), "description".to_string()],
        }
    }

    /// プロジェクトの種類を検出
    pub fn detect_project(dir: &Path) -> ProjectInfo {
        let kind = if dir.join("Cargo.toml").exists() {
            ProjectKind::Rust
        } else if dir.join("package.json").exists() {
            ProjectKind::Node
        } else if dir.join("pyproject.toml").exists() || dir.join("setup.py").exists() {
            ProjectKind::Python
        } else if dir.join("go.mod").exists() {
            ProjectKind::Go
        } else {
            ProjectKind::Unknown
        };

        let package_manager = match kind {
            ProjectKind::Rust => Some("cargo".to_string()),
            ProjectKind::Node => {
                if dir.join("pnpm-lock.yaml").exists() {
                    Some("pnpm".to_string())
                } else if dir.join("yarn.lock").exists() {
                    Some("yarn".to_string())
                } else {
                    Some("npm".to_string())
                }
            }
            ProjectKind::Python => {
                if dir.join("poetry.lock").exists() {
                    Some("poetry".to_string())
                } else if dir.join("Pipfile").exists() {
                    Some("pipenv".to_string())
                } else {
                    Some("pip".to_string())
                }
            }
            ProjectKind::Go => Some("go".to_string()),
            ProjectKind::Unknown => None,
        };

        ProjectInfo {
            kind,
            root: dir.to_path_buf(),
            package_manager,
        }
    }

    /// テンプレート一覧を取得
    pub async fn list_templates(&self) -> Vec<String> {
        let templates = self.templates.read().await;
        templates.keys().cloned().collect()
    }

    /// スキャフォールド実行「生命を与える」
    pub async fn scaffold(
        &self,
        template_name: &str,
        output_dir: &Path,
        variables: HashMap<String, String>,
    ) -> Result<ScaffoldResult> {
        let templates = self.templates.read().await;
        let template = templates
            .get(template_name)
            .ok_or_else(|| anyhow::anyhow!("Template '{}' not found", template_name))?;

        // 出力ディレクトリを作成
        std::fs::create_dir_all(output_dir)?;

        let mut generated_files = Vec::new();

        for (file_path_template, content_template) in &template.files {
            // ファイルパスの変数を置換
            let mut file_path = file_path_template.clone();
            let mut content = content_template.clone();

            for (key, value) in &variables {
                let placeholder = format!("{{{{{}}}}}", key);
                file_path = file_path.replace(&placeholder, value);
                content = content.replace(&placeholder, value);
            }

            // ファイルを書き込み
            let full_path = output_dir.join(&file_path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&full_path, &content)?;
            generated_files.push(full_path);
        }

        // 成長データに記録
        if self.config.growth_mode {
            let mut growth = self.growth_data.write().await;
            growth.scaffold_history.push(ScaffoldRecord {
                timestamp: chrono::Utc::now(),
                template: template_name.to_string(),
                output_dir: output_dir.to_path_buf(),
            });
        }

        Ok(ScaffoldResult {
            files: generated_files,
            template: TemplateKind::Custom(template_name.to_string()),
            output_dir: output_dir.to_path_buf(),
        })
    }

    /// 修復実行「回復」
    pub async fn heal(&self, dir: &Path, action: HealAction) -> Result<HealResult> {
        let project = Self::detect_project(dir);

        let result = match (&project.kind, &action) {
            (ProjectKind::Rust, HealAction::Format) => self.heal_rust_format(dir).await,
            (ProjectKind::Rust, HealAction::LintFix) => self.heal_rust_lint(dir).await,
            (ProjectKind::Rust, HealAction::All) => self.heal_rust_all(dir).await,
            (ProjectKind::Node, HealAction::Format) => self.heal_node_format(dir).await,
            (ProjectKind::Node, HealAction::LintFix) => self.heal_node_lint(dir).await,
            (ProjectKind::Node, HealAction::All) => self.heal_node_all(dir).await,
            (_, HealAction::All) => {
                // 不明なプロジェクトでも試みる
                Ok(HealResult {
                    action: action.clone(),
                    files_fixed: 0,
                    summary: "Unknown project type, no healing performed".to_string(),
                    success: true,
                })
            }
            _ => Ok(HealResult {
                action: action.clone(),
                files_fixed: 0,
                summary: format!("Action {:?} not supported for {:?}", action, project.kind),
                success: false,
            }),
        };

        // 成長データに記録
        if self.config.growth_mode
            && let Ok(ref heal_result) = result
        {
            let mut growth = self.growth_data.write().await;
            growth.heal_history.push(HealRecord {
                timestamp: chrono::Utc::now(),
                action,
                project_kind: project.kind,
                success: heal_result.success,
            });
        }

        result
    }

    /// Rust: フォーマット
    async fn heal_rust_format(&self, dir: &Path) -> Result<HealResult> {
        let output = tokio::process::Command::new("cargo")
            .arg("fmt")
            .current_dir(dir)
            .output()
            .await?;

        Ok(HealResult {
            action: HealAction::Format,
            files_fixed: 0, // cargo fmt doesn't report count
            summary: if output.status.success() {
                "Formatted with cargo fmt".to_string()
            } else {
                String::from_utf8_lossy(&output.stderr).to_string()
            },
            success: output.status.success(),
        })
    }

    /// Rust: Lint 修正
    async fn heal_rust_lint(&self, dir: &Path) -> Result<HealResult> {
        let output = tokio::process::Command::new("cargo")
            .args(["clippy", "--fix", "--allow-dirty", "--allow-staged"])
            .current_dir(dir)
            .output()
            .await?;

        Ok(HealResult {
            action: HealAction::LintFix,
            files_fixed: 0,
            summary: if output.status.success() {
                "Fixed with cargo clippy --fix".to_string()
            } else {
                String::from_utf8_lossy(&output.stderr).to_string()
            },
            success: output.status.success(),
        })
    }

    /// Rust: 全ての修復
    async fn heal_rust_all(&self, dir: &Path) -> Result<HealResult> {
        let fmt_result = self.heal_rust_format(dir).await?;
        let lint_result = self.heal_rust_lint(dir).await?;

        Ok(HealResult {
            action: HealAction::All,
            files_fixed: fmt_result.files_fixed + lint_result.files_fixed,
            summary: format!(
                "Format: {}, Lint: {}",
                if fmt_result.success { "OK" } else { "FAIL" },
                if lint_result.success { "OK" } else { "FAIL" }
            ),
            success: fmt_result.success && lint_result.success,
        })
    }

    /// Node: フォーマット
    async fn heal_node_format(&self, dir: &Path) -> Result<HealResult> {
        // prettier を試す
        let output = tokio::process::Command::new("npx")
            .args(["prettier", "--write", "."])
            .current_dir(dir)
            .output()
            .await?;

        Ok(HealResult {
            action: HealAction::Format,
            files_fixed: 0,
            summary: if output.status.success() {
                "Formatted with prettier".to_string()
            } else {
                String::from_utf8_lossy(&output.stderr).to_string()
            },
            success: output.status.success(),
        })
    }

    /// Node: Lint 修正
    async fn heal_node_lint(&self, dir: &Path) -> Result<HealResult> {
        let output = tokio::process::Command::new("npx")
            .args(["eslint", "--fix", "."])
            .current_dir(dir)
            .output()
            .await?;

        Ok(HealResult {
            action: HealAction::LintFix,
            files_fixed: 0,
            summary: if output.status.success() {
                "Fixed with eslint --fix".to_string()
            } else {
                String::from_utf8_lossy(&output.stderr).to_string()
            },
            success: output.status.success(),
        })
    }

    /// Node: 全ての修復
    async fn heal_node_all(&self, dir: &Path) -> Result<HealResult> {
        let fmt_result = self.heal_node_format(dir).await?;
        let lint_result = self.heal_node_lint(dir).await?;

        Ok(HealResult {
            action: HealAction::All,
            files_fixed: fmt_result.files_fixed + lint_result.files_fixed,
            summary: format!(
                "Format: {}, Lint: {}",
                if fmt_result.success { "OK" } else { "FAIL" },
                if lint_result.success { "OK" } else { "FAIL" }
            ),
            success: fmt_result.success && lint_result.success,
        })
    }

    /// カスタムテンプレートを登録
    pub async fn register_template(&self, template: Template) {
        let mut templates = self.templates.write().await;
        templates.insert(template.name.clone(), template);
    }

    /// 成長データを取得
    pub async fn growth_stats(&self) -> GrowthStats {
        let growth = self.growth_data.read().await;

        let heal_success_rate = if growth.heal_history.is_empty() {
            0.0
        } else {
            let success_count = growth.heal_history.iter().filter(|h| h.success).count();
            (success_count as f64 / growth.heal_history.len() as f64) * 100.0
        };

        GrowthStats {
            total_scaffolds: growth.scaffold_history.len(),
            total_heals: growth.heal_history.len(),
            heal_success_rate,
            patterns_learned: growth.patterns.len(),
        }
    }
}

/// 成長統計
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrowthStats {
    /// スキャフォールド実行回数
    pub total_scaffolds: usize,
    /// 修復実行回数
    pub total_heals: usize,
    /// 修復成功率 (%)
    pub heal_success_rate: f64,
    /// 学習したパターン数
    pub patterns_learned: usize,
}
