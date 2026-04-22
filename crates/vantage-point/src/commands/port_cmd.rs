//! `vp port` subcommand — deterministic port layout の表示 / 計算
//!
//! VP Port Management リデザイン (memory: mem_1CaKCbNE24KTQDuf9x4Eim) の
//! operation surface。現 Phase 0.5 は read-only (layout は Default 値)。
//! Phase 1 で config.toml からの PortLayout 読込 + project→slot 永続化、
//! Phase 2 で `slot assign/unassign` を config 書き込みに実装予定。

use anyhow::Result;
use clap::Subcommand;

use crate::port_layout::PortLayout;

#[derive(Subcommand, Debug)]
pub enum PortCommands {
    /// 特定 project/lane/role の port を表示
    Show {
        /// Project slot を直接指定 (Phase 0.5 暫定、将来 project 名 → slot 解決)
        #[arg(long)]
        slot: u16,
        /// Lane index (0 = Lead, 1+ = Worker)
        #[arg(long, default_value_t = 0)]
        lane: u16,
        /// Role 名 (agent / dev_server / db_admin / canvas / preview)
        #[arg(long)]
        role: Option<String>,
    },
    /// URL (http://localhost:{port}) を表示
    Url {
        #[arg(long)]
        slot: u16,
        #[arg(long, default_value_t = 0)]
        lane: u16,
        role: String,
    },
    /// Role offset table を表示
    Roles,
    /// 1 Project slot の全割当一覧
    Layout {
        /// 表示する slot (省略時は 0)
        #[arg(long, default_value_t = 0)]
        slot: u16,
    },
}

pub fn execute(cmd: PortCommands) -> Result<()> {
    let layout = PortLayout::default();
    match cmd {
        PortCommands::Roles => print_roles(&layout),
        PortCommands::Show { slot, lane, role } => show(&layout, slot, lane, role.as_deref()),
        PortCommands::Url { slot, lane, role } => url_cmd(&layout, slot, lane, &role),
        PortCommands::Layout { slot } => print_layout(&layout, slot),
    }
    Ok(())
}

fn print_roles(layout: &PortLayout) {
    println!("Role offset table (lane_size = {}):", layout.lane_size);
    for (name, offset) in layout.valid_roles() {
        println!("  +{:>2}  {}", offset, name);
    }
}

fn show(layout: &PortLayout, slot: u16, lane: u16, role: Option<&str>) {
    match role {
        None => {
            // lane base のみ表示
            match layout.lane_base(slot, lane) {
                Some(p) => println!("{}", p),
                None => eprintln!("out of range (slot={}, lane={})", slot, lane),
            }
        }
        Some(r) => match layout.port(slot, lane, r) {
            Some(p) => println!("{}", p),
            None => eprintln!(
                "no port for (slot={}, lane={}, role={})",
                slot, lane, r
            ),
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
        eprintln!("slot {} is out of range (max_projects = {})", slot, layout.max_projects);
        return;
    };
    println!("Project slot {} — base {}", slot, base);
    println!("  SP HTTP       : {}", layout.sp_port(slot).unwrap());
    println!("  SP Unison     : {}", layout.unison_port(slot).unwrap());
    println!();
    let max_lane = layout.max_lanes_per_project();
    for lane in 0..max_lane {
        let Some(lb) = layout.lane_base(slot, lane) else { continue };
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
