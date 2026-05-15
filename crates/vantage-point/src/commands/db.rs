//! `vp db` コマンド — embedded SurrealDB のメンテナンス
//!
//! embedded mode に移行したため、DB は TheWorld Process のライフサイクルと
//! 一緒に上がる (別 daemon は不要)。ここでは主に path 確認・初期化・スキーマ
//! 適用に絞った utility を提供する。
//!
//! VP-182: DB ディレクトリが World (`db/world/`) と per-SP (`db/sp_{slug}/`) に
//! 分離されたため、 本コマンドは **World DB** を対象とする。 per-SP DB の確認が
//! 必要になった場合は `--project <slug>` option を別途追加する (= 別 issue)。

use anyhow::Result;
use clap::Subcommand;

use crate::db;

/// SurrealDB サブコマンド
#[derive(Subcommand)]
pub enum DbCommands {
    /// DB data dir を確認し、初期スキーマを適用する (open → define_schema)
    Init,
    /// DB data dir のパスを表示
    Path,
    /// embedded DB のヘルスチェック (open → RETURN true)
    Status,
}

/// `vp db` を実行
pub fn execute(cmd: DbCommands) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let data_dir = db::db_data_dir_for_world();

    match cmd {
        DbCommands::Init => rt.block_on(async {
            let vpdb = db::VpDb::connect_embedded(&data_dir).await?;
            vpdb.define_schema().await?;
            println!("SurrealDB initialized at {}", data_dir.display());
            Ok(())
        }),
        DbCommands::Path => {
            println!("{}", data_dir.display());
            Ok(())
        }
        DbCommands::Status => rt.block_on(async {
            match db::VpDb::connect_embedded(&data_dir).await {
                Ok(vpdb) => {
                    let healthy = vpdb.health().await;
                    println!(
                        "SurrealDB embedded at {} — {}",
                        data_dir.display(),
                        if healthy { "OK" } else { "unhealthy" }
                    );
                    Ok(())
                }
                Err(e) => {
                    eprintln!("SurrealDB open failed ({}): {}", data_dir.display(), e);
                    std::process::exit(1);
                }
            }
        }),
    }
}
