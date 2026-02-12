//! `vp update` コマンドの実行ロジック

use anyhow::Result;

use crate::capability::update_capability::UpdateCapability;

/// `vp update` を実行
pub fn execute(check: bool) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let mut cap = UpdateCapability::new();

        println!("更新をチェック中...");
        let result = match cap.check_update().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("更新チェックに失敗: {}", e);
                std::process::exit(1);
            }
        };

        println!("  現在: v{}", result.current_version);
        println!("  最新: v{}", result.latest_version);

        if !result.update_available {
            println!("最新バージョンです。");
            return Ok(());
        }

        println!("  → 更新が利用可能です！");

        if check {
            // --check: チェックのみで終了
            if let Some(ref release) = result.release {
                if let Some(ref notes) = release.body {
                    println!("\nリリースノート:\n{}", notes);
                }
            }
            return Ok(());
        }

        // 更新を適用
        let release = result.release.as_ref().unwrap();
        println!("\n更新を適用中...");

        match cap.apply_update(release).await {
            Ok(apply) => {
                println!("更新完了: v{} → v{}", apply.previous_version, apply.new_version);
                println!("  バイナリ: {}", apply.binary_path);
                if let Some(ref backup) = apply.backup_path {
                    println!("  バックアップ: {}", backup);
                }
                if apply.restart_required {
                    println!("\n新しいバージョンを使用するには再起動してください。");
                }
            }
            Err(e) => {
                eprintln!("更新に失敗: {}", e);
                std::process::exit(1);
            }
        }

        Ok(())
    })
}
