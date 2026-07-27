//! home-node (daemon) の identity 層 (federation L2、 ADR-020 D2)。
//!
//! `lane` モジュールが lane の位置独立 id ([`crate::lane::lane_id`]) を持つのと対称に、
//! machine 階層の位置独立 id ([`node_id`]) をここに置く。VP の I1 史
//! (namespace を git/path から、 LaneId を path/location から切った) を **daemon に一段上げた**
//! もの — machine = 「今の居場所」、 node_id = 「不変の番地」、 handle = 「人間可読の表示名」の
//! 三層分離 (ADR-020 D2)。

pub mod endpoint;
pub mod node_id;

pub use node_id::NodeId;
