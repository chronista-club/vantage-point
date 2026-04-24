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
        /// Lane index (0 = Lead, 1+ = Worker)
        #[arg(long, default_value_t = 0)]
        lane: u16,
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
            role,
        } => {
            let (layout, slot) = resolve_slot(project.as_deref(), slot)?;
            show(&layout, slot, lane, role.as_deref());
            Ok(())
        }
        PortCommands::Url {
            project,
            slot,
            lane,
            role,
        } => {
            let (layout, slot) = resolve_slot(project.as_deref(), slot)?;
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
        let label = if lane == 0 { "Lead" } else { "Worker" };
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
            config.save().context("failed to save config.toml")?;
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
            config.save().context("failed to save config.toml")?;
            println!("unassigned slot from '{}'", project);
            Ok(())
        }
    }
}
