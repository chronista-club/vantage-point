//! `vp world` コマンドの実行ロジック

use anyhow::Result;

use crate::{GuardianCommands, WorldCommands};

/// `vp world` を実行
pub fn execute(cmd: WorldCommands) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        match cmd {
            WorldCommands::Up {
                debug,
                static_dir,
                midi,
                requiem,
            } => {
                if requiem {
                    println!("🌍 The World 起動中... 「真実にはたどり着けない」(Requiem Mode)");
                } else {
                    println!("🌍 The World 起動中... 「時よ止まれ」");
                }
                if let Some(ref pattern) = midi {
                    println!(
                        "🎹 MIDI 有効: {}",
                        if pattern.is_empty() {
                            "(全ポート)"
                        } else {
                            pattern
                        }
                    );
                }
                let config = crate::world::WorldConfig {
                    debug,
                    static_dir,
                    midi_port_pattern: midi,
                    requiem_mode: requiem,
                    ..Default::default()
                };
                crate::world::run(config).await
            }
            WorldCommands::Down => {
                println!("🌍 The World 停止中... 「そして時は動き出す」");
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(2))
                    .build()?;
                let url = format!("http://localhost:{}/api/shutdown", crate::world::WORLD_PORT);
                match client.post(&url).send().await {
                    Ok(_) => {
                        println!("✓ The World 停止完了");
                        Ok(())
                    }
                    Err(e) => {
                        if e.is_connect() {
                            println!("✗ The World は稼働していません");
                        } else {
                            println!("✗ 停止エラー: {}", e);
                        }
                        Ok(())
                    }
                }
            }
            WorldCommands::Status => {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(2))
                    .build()?;
                let url = format!("http://localhost:{}/health", crate::world::WORLD_PORT);
                match client.get(&url).send().await {
                    Ok(response) if response.status().is_success() => {
                        let body = response.text().await.unwrap_or_default();
                        println!("✓ The World 稼働中 (port {})", crate::world::WORLD_PORT);
                        println!("{}", body);
                    }
                    Ok(_) => {
                        println!("✗ The World がエラー応答");
                    }
                    Err(e) => {
                        if e.is_connect() {
                            println!("✗ The World は稼働していません");
                        } else {
                            println!("✗ 接続エラー: {}", e);
                        }
                    }
                }
                Ok(())
            }
            WorldCommands::Open => {
                println!("🌍 ViewPoint を開いています...");
                // The World が稼働しているか確認
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(2))
                    .build()?;
                let url = format!("http://localhost:{}/health", crate::world::WORLD_PORT);
                match client.get(&url).send().await {
                    Ok(response) if response.status().is_success() => {
                        // WebView を起動
                        drop(client); // クライアントを解放
                        crate::terminal_window::run_terminal(crate::world::WORLD_PORT)?;
                    }
                    Ok(_) => {
                        println!("✗ The World がエラー応答");
                    }
                    Err(e) => {
                        if e.is_connect() {
                            println!("✗ The World が稼働していません");
                            println!("  先に `vp world up -s ./web` で起動してください");
                        } else {
                            println!("✗ 接続エラー: {}", e);
                        }
                    }
                }
                Ok(())
            }
            WorldCommands::Mcp => {
                // MCP Server として起動（stdio JSON-RPC）
                tracing::info!("The World MCP Server 起動");
                crate::world::mcp::run_mcp_server().await
            }
            WorldCommands::Snapshot { name, description } => {
                println!("📸 スナップショット作成中... 「時を止める」");

                // 現在のディレクトリをスナップショット対象とする
                let target_dir = std::env::current_dir().unwrap_or_default();

                // ローカルで GER を使用してスナップショット作成
                let ger = crate::world::GoldExperienceRequiem::default();
                match ger
                    .create_snapshot(&name, description.as_deref(), &target_dir)
                    .await
                {
                    Ok(snapshot) => {
                        println!("✓ スナップショット '{}' を作成しました", snapshot.name);
                        println!("  場所: {}", snapshot.path.display());
                        println!(
                            "  作成: {}",
                            snapshot.created_at.format("%Y-%m-%d %H:%M:%S")
                        );
                        Ok(())
                    }
                    Err(e) => {
                        println!("✗ スナップショット作成エラー: {}", e);
                        Ok(())
                    }
                }
            }
            WorldCommands::Restore { name } => {
                println!("⏪ スナップショット復元中... 「ゼロに戻す」");

                let ger = crate::world::GoldExperienceRequiem::default();
                // まず既存のスナップショットを読み込む
                if let Err(e) = ger.load_snapshots().await {
                    println!("✗ スナップショット読み込みエラー: {}", e);
                    return Ok(());
                }

                match ger.restore_snapshot(&name).await {
                    Ok(()) => {
                        println!("✓ スナップショット '{}' から復元しました", name);
                        Ok(())
                    }
                    Err(e) => {
                        println!("✗ 復元エラー: {}", e);
                        Ok(())
                    }
                }
            }
            WorldCommands::Snapshots => {
                println!("📸 スナップショット一覧:");

                let ger = crate::world::GoldExperienceRequiem::default();
                if let Err(e) = ger.load_snapshots().await {
                    println!("✗ スナップショット読み込みエラー: {}", e);
                    return Ok(());
                }

                let snapshots = ger.list_snapshots().await;
                if snapshots.is_empty() {
                    println!("  (スナップショットなし)");
                } else {
                    println!("  NAME                CREATED              DESCRIPTION");
                    println!("  ────                ───────              ───────────");
                    for snap in snapshots {
                        let desc = snap.description.as_deref().unwrap_or("-");
                        let desc_display = if desc.len() > 30 {
                            format!("{}...", &desc[..27])
                        } else {
                            desc.to_string()
                        };
                        println!(
                            "  {:18}  {}  {}",
                            snap.name,
                            snap.created_at.format("%Y-%m-%d %H:%M"),
                            desc_display
                        );
                    }
                }
                Ok(())
            }
            WorldCommands::Guardian(guardian_cmd) => {
                let ger = crate::world::GoldExperienceRequiem::default();

                match guardian_cmd {
                    GuardianCommands::Status => {
                        let status = ger.guardian_status().await;
                        println!("🛡️ Guardian ステータス:");
                        println!(
                            "  状態: {}",
                            if status.enabled {
                                "有効 ✓"
                            } else {
                                "無効"
                            }
                        );
                        println!("  ルール数: {}", status.rule_count);
                        println!("  ブロック回数: {}", status.block_count);
                        if let Some(last) = status.last_check {
                            println!("  最終チェック: {}", last.format("%Y-%m-%d %H:%M:%S"));
                        }
                        Ok(())
                    }
                    GuardianCommands::Enable => {
                        ger.enable_guardian().await;
                        println!("✓ Guardian を有効化しました「自動防御発動」");
                        Ok(())
                    }
                    GuardianCommands::Disable => {
                        ger.disable_guardian().await;
                        println!("✓ Guardian を無効化しました");
                        Ok(())
                    }
                    GuardianCommands::Rules => {
                        let rules = ger.list_rules().await;
                        println!("🛡️ Guardian ルール一覧:");
                        if rules.is_empty() {
                            println!("  (ルールなし)");
                        } else {
                            println!("  NAME                PATTERN              ACTION");
                            println!("  ────                ───────              ──────");
                            for rule in rules {
                                let action = format!("{:?}", rule.action).to_lowercase();
                                let status = if rule.enabled { "" } else { " (無効)" };
                                println!(
                                    "  {:18}  {:20}  {}{}",
                                    rule.name, rule.pattern, action, status
                                );
                            }
                        }
                        Ok(())
                    }
                    GuardianCommands::AddRule { name, pattern } => {
                        match ger
                            .add_rule(&name, &pattern, crate::world::GuardianAction::Block)
                            .await
                        {
                            Ok(()) => {
                                println!(
                                    "✓ ルール '{}' を追加しました (pattern: {})",
                                    name, pattern
                                );
                                Ok(())
                            }
                            Err(e) => {
                                println!("✗ ルール追加エラー: {}", e);
                                Ok(())
                            }
                        }
                    }
                }
            }
        }
    })
}
