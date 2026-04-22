//! VP Port Layout — deterministic 透過的固定 port 計算
//!
//! VP の port 使用戦略を slot × lane × role 階層で固定化する。
//! 設計詳細は creo-memories の `mem_1CaKCbNE24KTQDuf9x4Eim` 参照。
//!
//! ## 公式
//!
//! ```text
//! port(project_slot, lane_index, role)
//!   = project_slot_base (33000)
//!   + project_slot × project_slot_size (100)
//!   + lane_base_offset (10) + lane_index × lane_size (10)
//!   + role_offset
//! ```
//!
//! ## 例: vantage-point (slot=0) × Worker laneIndex=1 × dev_server
//! = 33000 + 0*100 + 10 + 1*10 + 1 = **33021**
//!
//! 再起動しても slot が永続 assign されている限り同じ port。
//! bookmark / URL 貼付が "透過的固定" = 再起動をまたいで有効。

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
        roles.insert("canvas".into(), 3);
        roles.insert("preview".into(), 4);
        Self {
            world_port: 32000,
            project_slot_base: 33000,
            project_slot_size: 100,
            max_projects: 20,
            lane_base_offset: 10,
            lane_size: 10,
            roles,
        }
    }
}

impl PortLayout {
    /// Project slot の base port (= SP HTTP port)
    pub fn project_base(&self, slot: u16) -> Option<u16> {
        if slot >= self.max_projects {
            return None;
        }
        Some(self.project_slot_base + slot * self.project_slot_size)
    }

    /// SP HTTP port = project_base
    pub fn sp_port(&self, slot: u16) -> Option<u16> {
        self.project_base(slot)
    }

    /// SP Unison (QUIC) port = project_base + 1
    pub fn unison_port(&self, slot: u16) -> Option<u16> {
        self.project_base(slot).map(|p| p + 1)
    }

    /// Lane の base port
    pub fn lane_base(&self, slot: u16, lane_index: u16) -> Option<u16> {
        let base = self.project_base(slot)?;
        let lane_start = base + self.lane_base_offset;
        let slot_end_exclusive = base + self.project_slot_size;
        let p = lane_start + lane_index * self.lane_size;
        if p + self.lane_size > slot_end_exclusive {
            return None;
        }
        Some(p)
    }

    /// 指定 role の port
    pub fn port(&self, slot: u16, lane_index: u16, role: &str) -> Option<u16> {
        let base = self.lane_base(slot, lane_index)?;
        let offset = *self.roles.get(role)?;
        if offset >= self.lane_size {
            return None;
        }
        Some(base + offset)
    }

    /// URL を生成 (http://localhost:{port})
    pub fn url(&self, slot: u16, lane_index: u16, role: &str) -> Option<String> {
        self.port(slot, lane_index, role)
            .map(|p| format!("http://localhost:{}", p))
    }

    /// Project slot 内で作れる Lane の最大数
    pub fn max_lanes_per_project(&self) -> u16 {
        let usable = self.project_slot_size.saturating_sub(self.lane_base_offset);
        usable / self.lane_size
    }

    /// 有効な role (lane_size 内に収まるもの) を offset 順で返す
    pub fn valid_roles(&self) -> Vec<(String, u16)> {
        let mut v: Vec<_> = self
            .roles
            .iter()
            .filter(|(_, o)| **o < self.lane_size)
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        v.sort_by_key(|(_, o)| *o);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_world_port() {
        assert_eq!(PortLayout::default().world_port, 32000);
    }

    #[test]
    fn project_base_computation() {
        let l = PortLayout::default();
        assert_eq!(l.project_base(0), Some(33000));
        assert_eq!(l.project_base(1), Some(33100));
        assert_eq!(l.project_base(19), Some(34900));
        assert_eq!(l.project_base(20), None);
    }

    #[test]
    fn sp_and_unison_ports() {
        let l = PortLayout::default();
        assert_eq!(l.sp_port(0), Some(33000));
        assert_eq!(l.unison_port(0), Some(33001));
        assert_eq!(l.sp_port(1), Some(33100));
        assert_eq!(l.unison_port(1), Some(33101));
    }

    #[test]
    fn lane_base_within_slot() {
        let l = PortLayout::default();
        // Slot 0
        assert_eq!(l.lane_base(0, 0), Some(33010)); // Lead
        assert_eq!(l.lane_base(0, 1), Some(33020)); // Worker A
        assert_eq!(l.lane_base(0, 8), Some(33090)); // Worker H
        assert_eq!(l.lane_base(0, 9), None); // over slot boundary
        // Slot 1
        assert_eq!(l.lane_base(1, 0), Some(33110));
        assert_eq!(l.lane_base(1, 8), Some(33190));
    }

    #[test]
    fn lane_role_ports() {
        let l = PortLayout::default();
        // vantage-point slot=0, Worker (laneIndex=1) — design memo の具体例
        assert_eq!(l.port(0, 1, "agent"), Some(33020));
        assert_eq!(l.port(0, 1, "dev_server"), Some(33021));
        assert_eq!(l.port(0, 1, "db_admin"), Some(33022));
        assert_eq!(l.port(0, 1, "canvas"), Some(33023));
        assert_eq!(l.port(0, 1, "preview"), Some(33024));
        assert_eq!(l.port(0, 1, "unknown_role"), None);
    }

    #[test]
    fn url_generation() {
        let l = PortLayout::default();
        assert_eq!(
            l.url(0, 1, "dev_server"),
            Some("http://localhost:33021".into())
        );
    }

    #[test]
    fn max_lanes_per_project() {
        assert_eq!(PortLayout::default().max_lanes_per_project(), 9);
    }

    #[test]
    fn valid_roles_sorted() {
        let roles = PortLayout::default().valid_roles();
        assert_eq!(roles[0], ("agent".into(), 0));
        assert_eq!(roles[1], ("dev_server".into(), 1));
        assert_eq!(roles[4], ("preview".into(), 4));
    }
}
