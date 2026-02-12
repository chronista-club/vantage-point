//! `vp park` コマンドの実行ロジック

use anyhow::Result;

use crate::ParkCommands;

/// `vp park` を実行
pub fn execute(cmd: ParkCommands) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        match cmd {
            ParkCommands::Up { project_dir, port } => {
                let project_path = project_dir
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let project_id = project_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let port = port.unwrap_or(33100); // デフォルトポート

                println!("🌸 Paisley Park 起動中... 「情報を収集します」");
                println!("   Project: {} ({})", project_id, project_path.display());

                let config = crate::park::ParkConfig {
                    project_id,
                    project_path,
                    world_url: format!("http://localhost:{}", crate::world::WORLD_PORT),
                    ..Default::default()
                };

                let park = crate::park::PaisleyPark::new(config);
                park.start(port).await?;

                println!("✓ The World に登録完了");

                // 終了シグナルを待つ
                tokio::signal::ctrl_c().await?;
                println!("\n🌸 シャットダウン中...");
                park.stop().await?;
                Ok(())
            }
            ParkCommands::Down { project_id: _ } => {
                println!("🌸 Paisley Park 停止");
                // TODO: The World 経由で特定の Park を停止
                Ok(())
            }
            ParkCommands::List => {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(2))
                    .build()?;
                let url = format!("http://localhost:{}/api/parks", crate::world::WORLD_PORT);
                match client.get(&url).send().await {
                    Ok(response) if response.status().is_success() => {
                        let body = response.text().await.unwrap_or_default();
                        println!("登録済み Paisley Park:");
                        println!("{}", body);
                    }
                    Ok(_) => {
                        println!("✗ The World からエラー応答");
                    }
                    Err(e) => {
                        if e.is_connect() {
                            println!("✗ The World が稼働していません");
                            println!("  先に `vp world up` で起動してください");
                        } else {
                            println!("✗ 接続エラー: {}", e);
                        }
                    }
                }
                Ok(())
            }
        }
    })
}
