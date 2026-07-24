//! VP Port Layout — World daemon の listen port を保持する。
//!
//! doc 44 P1 (fold-in): 旧構成は slot × lane × role で SP / lane / role の固定 port を
//! 算術で払い出していた（`vp port` が表示）。fold-in で project が portless（port=0）に
//! なり、その算術（`sp_port` / `lane_base` / `port` / `url` 等）は誰も使わなくなったため
//! 撤去した。残るのは `world_port`（`config.ports.world_port` で override 可）だけ。
//! layout 定数フィールドは config schema（`[ports]`）の互換のため struct に温存する。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Port layout 定義
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortLayout {
    /// World daemon が listen する port
    pub world_port: u16,
    /// Project slot 群の base port (slot 0 の始点)
    pub project_slot_base: u16,
    /// 1 project slot の占有 port 数
    pub project_slot_size: u16,
    /// project slot の最大数
    pub max_projects: u16,
    /// project slot 内で Lane 領域が始まる offset (SP/Unison 用を除いた位置)
    pub lane_base_offset: u16,
    /// 1 Lane の占有 port 数
    pub lane_size: u16,
    /// Lane 内での role → offset table (sort 安定のため BTreeMap)
    pub roles: BTreeMap<String, u16>,
}

impl Default for PortLayout {
    fn default() -> Self {
        let mut roles = BTreeMap::new();
        roles.insert("agent".into(), 0);
        roles.insert("dev_server".into(), 1);
        roles.insert("db_admin".into(), 2);
        roles.insert("board".into(), 3);
        roles.insert("preview".into(), 4);
        Self {
            // VP_PROFILE 分離 (#643): brew=32000 / dev=32100。 SSOT は vp_paths::default_world_port()。
            // ここを 32000 固定にすると Config::port_layout() 経由の解決 (world_wire 等) だけが
            // profile を無視して brew namespace に越境する (dev/brew 混在の再発)。
            world_port: vp_paths::default_world_port(),
            project_slot_base: 33000,
            project_slot_size: 100,
            max_projects: 20,
            lane_base_offset: 10,
            lane_size: 10,
            roles,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_world_port() {
        // profile 依存 (brew=32000 / dev=32100) なので SSOT と一致することを検証する
        // (32000 固定 assert だと VP_PROFILE=dev 環境の cargo test で偽陽性に落ちる)。
        assert_eq!(
            PortLayout::default().world_port,
            vp_paths::default_world_port()
        );
    }
}
