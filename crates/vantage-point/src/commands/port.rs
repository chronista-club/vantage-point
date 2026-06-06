//! `vp port` subcommand — deterministic port layout の表示 / 計算
//!
//! VP Port Management (memory: mem_1CaKCbNE24KTQDuf9x4Eim)
//! - Phase 0.5: port_layout pure function + CLI scaffold (read-only)
//! - Phase 1: config 連携、project slug → slot 解決、slot assign/unassign 永続化
//!
//! ## slot 解決
//!
//! `--project <name>` (slug) → config の `projects[].slot` を参照。
//! `--slot <N>` は直接指定 (config 無視、計算のみ)。

use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::PathBuf;

use crate::config::Config;
use crate::port_layout::PortLayout;

#[derive(Subcommand, Debug)]
pub enum PortCommands {
    /// 特定 project/lane/role の port を表示
    Show {
        /// Project slug (config の name、slot 解決に使う)
        #[arg(long, conflicts_with = "slot")]
        project: Option<String>,
        /// Project slot を直接指定 (config を無視した raw 計算)
        #[arg(long)]
        slot: Option<u16>,
        /// Lane index (0 = Lead, 1+ = Wing)
        #[arg(long, default_value_t = 0)]
        lane: u16,
        /// Wing name (= lane index を name 解決、 --lane を上書き)
        #[arg(long)]
        wing: Option<String>,
        /// Role 名 (agent / dev_server / db_admin / canvas / preview)
        #[arg(long)]
        role: Option<String>,
    },
    /// URL (http://localhost:{port}) を表示
    Url {
        #[arg(long, conflicts_with = "slot")]
        project: Option<String>,
        #[arg(long)]
        slot: Option<u16>,
        #[arg(long, default_value_t = 0)]
        lane: u16,
        /// Wing name (= lane index を name 解決、 --lane を上書き)
        #[arg(long)]
        wing: Option<String>,
        role: String,
    },
    /// Role offset table
    Roles,
    /// 1 Project slot の全割当一覧
    Layout {
        #[arg(long, conflicts_with = "slot")]
        project: Option<String>,
        #[arg(long)]
        slot: Option<u16>,
    },
    /// Slot 割当管理 (config.toml 永続)
    #[command(subcommand)]
    Slot(SlotCommands),
}

#[derive(Subcommand, Debug)]
pub enum SlotCommands {
    /// project ↔ slot mapping 一覧
    List,
    /// project に slot を assign (未割当なら自動、指定 slot 衝突は error)
    Assign {
        /// Project slug
        project: String,
        /// 指定 slot (省略時は次の空き slot を自動割当)
        #[arg(long)]
        slot: Option<u16>,
    },
    /// project から slot を unassign
    Unassign { project: String },
}

pub fn execute(cmd: PortCommands) -> Result<()> {
    match cmd {
        PortCommands::Roles => {
            let layout = load_layout()?;
            print_roles(&layout);
            Ok(())
        }
        PortCommands::Show {
            project,
            slot,
            lane,
            wing,
            role,
        } => {
            let (layout, slot) = resolve_slot(project.as_deref(), slot)?;
            let lane = resolve_lane(project.as_deref(), wing.as_deref(), lane)?;
            show(&layout, slot, lane, role.as_deref());
            Ok(())
        }
        PortCommands::Url {
            project,
            slot,
            lane,
            wing,
            role,
        } => {
            let (layout, slot) = resolve_slot(project.as_deref(), slot)?;
            let lane = resolve_lane(project.as_deref(), wing.as_deref(), lane)?;
            url_cmd(&layout, slot, lane, &role);
            Ok(())
        }
        PortCommands::Layout { project, slot } => {
            let (layout, slot) = resolve_slot(project.as_deref(), slot)?;
            print_layout(&layout, slot);
            Ok(())
        }
        PortCommands::Slot(sc) => execute_slot(sc),
    }
}

/// Config から PortLayout を取得 (override 適用済み)
fn load_layout() -> Result<PortLayout> {
    let config = Config::load().unwrap_or_default();
    Ok(config.port_layout())
}

/// `--wing <name>` を lane_index に解決する (= 「目的ベース」 port allocation の核)。
///
/// 優先順位:
/// - `wing` 不在 → 引数 `lane` をそのまま (= 既存挙動、 default 0 = lead)
/// - `wing` 指定 → repo root を解決 (project 指定があれば projects.kdl、 無ければ cwd)
///   → `lane::commands::resolve_lane_index_by_wing_name` で alphabetical 順 + 1
///
/// 注: 解決は **alphabetical sort + 1** なので、 wing 追加削除で順序が変わる →
/// port が変動する可能性。 「name 経由 access」 を default にする運用が前提。
fn resolve_lane(project: Option<&str>, wing: Option<&str>, lane: u16) -> Result<u16> {
    let Some(wing_name) = wing else {
        return Ok(lane);
    };
    let repo_root = resolve_repo_root(project)
        .with_context(|| format!("--wing {wing_name} の repo root 解決に失敗"))?;
    crate::lane::commands::resolve_lane_index_by_wing_name(&repo_root, wing_name)
        .with_context(|| {
            format!(
                "wing '{wing_name}' が repo {} の `.vp/lanes/` に存在しません。`vp lane ls` で確認してください。",
                repo_root.display()
            )
        })
}

/// repo root を解決する (= `--wing` 経路で wing list を引くため)。
///
/// 優先順位:
/// - `project` 指定 → projects.kdl から path lookup → 該当 path
/// - 不在 → cwd の `git rev-parse --show-toplevel`
fn resolve_repo_root(project: Option<&str>) -> Result<PathBuf> {
    if let Some(name) = project {
        let config = Config::load().unwrap_or_default();
        if let Some(p) = config.projects.iter().find(|p| p.name == name) {
            return Ok(PathBuf::from(&p.path));
        }
        anyhow::bail!("project '{name}' が config に未登録です。`vp config` で確認してください。");
    }
    crate::lane::config::find_repo_root().map_err(Into::into)
}

/// (project slug or --slot) から slot index 決定 — layout も返す
fn resolve_slot(project: Option<&str>, slot: Option<u16>) -> Result<(PortLayout, u16)> {
    let config = Config::load().unwrap_or_default();
    let layout = config.port_layout();

    if let Some(s) = slot {
        return Ok((layout, s));
    }
    if let Some(name) = project {
        let s = config.resolve_slot_by_name(name).with_context(|| {
            format!(
                "project '{}' has no slot assigned — run 'vp port slot assign {}'",
                name, name
            )
        })?;
        return Ok((layout, s));
    }
    anyhow::bail!("specify either --project <name> or --slot <N>");
}

fn print_roles(layout: &PortLayout) {
    println!("Role offset table (lane_size = {}):", layout.lane_size);
    for (name, offset) in layout.valid_roles() {
        println!("  +{:>2}  {}", offset, name);
    }
}

fn show(layout: &PortLayout, slot: u16, lane: u16, role: Option<&str>) {
    match role {
        None => match layout.lane_base(slot, lane) {
            Some(p) => println!("{}", p),
            None => eprintln!("out of range (slot={}, lane={})", slot, lane),
        },
        Some(r) => match layout.port(slot, lane, r) {
            Some(p) => println!("{}", p),
            None => eprintln!("no port for (slot={}, lane={}, role={})", slot, lane, r),
        },
    }
}

fn url_cmd(layout: &PortLayout, slot: u16, lane: u16, role: &str) {
    match layout.url(slot, lane, role) {
        Some(u) => println!("{}", u),
        None => eprintln!("no URL for (slot={}, lane={}, role={})", slot, lane, role),
    }
}

fn print_layout(layout: &PortLayout, slot: u16) {
    let Some(base) = layout.project_base(slot) else {
        eprintln!(
            "slot {} is out of range (max_projects = {})",
            slot, layout.max_projects
        );
        return;
    };
    println!("Project slot {} — base {}", slot, base);
    println!("  SP HTTP       : {}", layout.sp_port(slot).unwrap());
    println!("  SP Unison     : {}", layout.unison_port(slot).unwrap());
    println!();
    for lane in 0..layout.max_lanes_per_project() {
        let Some(lb) = layout.lane_base(slot, lane) else {
            continue;
        };
        let label = if lane == 0 { "Lead" } else { "Wing" };
        println!("  Lane {} ({}) — base {}", lane, label, lb);
        for (role, offset) in layout.valid_roles() {
            if let Some(p) = layout.port(slot, lane, &role) {
                println!("    +{:>2} {:<12} : {}", offset, role, p);
            }
        }
        println!();
    }
}

fn execute_slot(cmd: SlotCommands) -> Result<()> {
    match cmd {
        SlotCommands::List => {
            let config = Config::load().unwrap_or_default();
            let layout = config.port_layout();
            println!("Slot assignments (max_projects = {}):", layout.max_projects);
            let mut assigned: Vec<_> = config
                .projects
                .iter()
                .filter(|p| p.slot.is_some())
                .collect();
            assigned.sort_by_key(|p| p.slot);
            if assigned.is_empty() {
                println!("  (none)");
            } else {
                for p in &assigned {
                    let base = layout.project_base(p.slot.unwrap()).unwrap_or(0);
                    println!(
                        "  slot {:>2} → {:<30} (base {})",
                        p.slot.unwrap(),
                        p.name,
                        base
                    );
                }
            }
            let unassigned: Vec<_> = config
                .projects
                .iter()
                .filter(|p| p.slot.is_none())
                .collect();
            if !unassigned.is_empty() {
                println!("\nUnassigned:");
                for p in unassigned {
                    println!("  {}", p.name);
                }
            }
            Ok(())
        }
        SlotCommands::Assign { project, slot } => {
            let mut config = Config::load().unwrap_or_default();
            let assigned = config.ensure_slot(&project, slot)?;
            // PR-D: slot を daemon (db/world 真実源) に永続化。 daemon 不在は kdl フォールバック。
            let key = config
                .projects
                .iter()
                .find(|p| p.name == project)
                .map(|p| Config::normalize_path(std::path::Path::new(&p.path)));
            let persisted = key
                .as_deref()
                .map(|k| crate::world_client::notify_world_set_slot(k, assigned))
                .unwrap_or(false);
            if !persisted {
                config
                    .persist_projects_kdl()
                    .context("failed to save projects.kdl")?;
            }
            let layout = config.port_layout();
            let base = layout.project_base(assigned).unwrap();
            println!(
                "assigned slot {} to '{}' (base port {})",
                assigned, project, base
            );
            Ok(())
        }
        SlotCommands::Unassign { project } => {
            let mut config = Config::load().unwrap_or_default();
            config.unassign_slot(&project)?;
            // PR-D: daemon (db/world 真実源) で slot 解除。 daemon 不在は kdl フォールバック。
            let key = config
                .projects
                .iter()
                .find(|p| p.name == project)
                .map(|p| Config::normalize_path(std::path::Path::new(&p.path)));
            let persisted = key
                .as_deref()
                .map(crate::world_client::notify_world_unassign_slot)
                .unwrap_or(false);
            if !persisted {
                config
                    .persist_projects_kdl()
                    .context("failed to save projects.kdl")?;
            }
            println!("unassigned slot from '{}'", project);
            Ok(())
        }
    }
}
