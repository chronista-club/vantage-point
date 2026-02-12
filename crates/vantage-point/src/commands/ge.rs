//! `vp ge` コマンドの実行ロジック（Gold Experience）

use anyhow::Result;

use crate::GeCommands;
use crate::cli::to_pascal_case;

/// `vp ge` を実行
pub fn execute(cmd: GeCommands) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let ge = crate::world::GoldExperience::default();

        match cmd {
            GeCommands::Scaffold {
                template,
                output,
                name,
                description,
            } => {
                println!("🌟 Gold Experience 発動... 「生命を与える」");

                let project_name = name.unwrap_or_else(|| {
                    output
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "new-project".to_string())
                });

                let mut variables = std::collections::HashMap::new();
                variables.insert("name".to_string(), project_name.clone());
                variables.insert("Name".to_string(), to_pascal_case(&project_name));
                variables.insert(
                    "description".to_string(),
                    description.unwrap_or_else(|| format!("{} project", project_name)),
                );

                match ge.scaffold(&template, &output, variables).await {
                    Ok(result) => {
                        println!("✓ スキャフォールド完了");
                        println!("  テンプレート: {}", template);
                        println!("  出力先: {}", result.output_dir.display());
                        println!("  生成ファイル:");
                        for file in result.files {
                            println!("    - {}", file.display());
                        }
                        Ok(())
                    }
                    Err(e) => {
                        println!("✗ スキャフォールドエラー: {}", e);
                        Ok(())
                    }
                }
            }
            GeCommands::Heal { dir, action } => {
                println!("💚 Gold Experience 発動... 「回復」");

                let heal_action = match action.as_str() {
                    "format" => crate::world::HealAction::Format,
                    "lint-fix" | "lint" => crate::world::HealAction::LintFix,
                    "deps" | "fix-deps" => crate::world::HealAction::FixDependencies,
                    "imports" | "organize-imports" => crate::world::HealAction::OrganizeImports,
                    _ => crate::world::HealAction::All,
                };

                match ge.heal(&dir, heal_action).await {
                    Ok(result) => {
                        let status = if result.success { "✓" } else { "⚠" };
                        println!("{} 修復完了: {}", status, result.action);
                        println!("  {}", result.summary);
                        Ok(())
                    }
                    Err(e) => {
                        println!("✗ 修復エラー: {}", e);
                        Ok(())
                    }
                }
            }
            GeCommands::Templates => {
                println!("🌟 Gold Experience テンプレート一覧:");
                let templates = ge.list_templates().await;
                if templates.is_empty() {
                    println!("  (テンプレートなし)");
                } else {
                    for template in templates {
                        println!("  - {}", template);
                    }
                }
                Ok(())
            }
            GeCommands::Detect { dir } => {
                let project = crate::world::GoldExperience::detect_project(&dir);
                println!("🔍 プロジェクト検出結果:");
                println!("  種類: {}", project.kind);
                println!("  ルート: {}", project.root.display());
                if let Some(pm) = project.package_manager {
                    println!("  パッケージマネージャ: {}", pm);
                }
                Ok(())
            }
            GeCommands::Stats => {
                let stats = ge.growth_stats().await;
                println!("🌱 Gold Experience 成長統計:");
                println!("  スキャフォールド実行: {} 回", stats.total_scaffolds);
                println!("  修復実行: {} 回", stats.total_heals);
                println!("  修復成功率: {:.1}%", stats.heal_success_rate);
                println!("  学習パターン: {} 件", stats.patterns_learned);
                Ok(())
            }
        }
    })
}
