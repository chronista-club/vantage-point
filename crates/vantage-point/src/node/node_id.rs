//! home-node の位置独立な安定 id `nd_xxx` (federation L2、 ADR-020 D2)。
//!
//! home-node (= VP の daemon) の identity を machine / hostname / endpoint から
//! 切り離す。daemon が別マシンへ移ろうと hostname が変わろうと、 この id は変わらない。
//! federation の **routing key** であり、 hub が `node_id → endpoint` を解決する (machine は
//! 「今の居場所」、 node_id は「不変の番地」、 handle は表示名 = ADR-002/008 idiom)。
//!
//! ## [`crate::lane::lane_id::LaneId`] との違い
//! - LaneId は (repo, lane) ごとに **多数**、 address で引く。
//! - NodeId は home-node に **1 つ (singleton)**。daemon に 1 個なので address key を持たない。
//!   発行・永続は db/machine の単一 row ([`crate::db::VpDb::load_or_create_node_id`])。
//!
//! ## format = EntId 形式 (`nd_` + base58(uuid_v7))
//! chronista エコシステムの EntId (`mem_xxx` / `atl_xxx`) と **byte 単位で同一の符号**に揃える
//! ([`encode_base58_uuid`] は creo `packages/entid` の `encodeBase58` を忠実 port、 golden test
//! で固定)。本体は 128-bit UUID v7 を base58 し、 creo と同じく「先頭の 0 hex nibble の数だけ
//! `1` を前置」する。UUID v7 は hex が `0..` 始まり (先頭 48bit ミリ秒タイムスタンプ) のため
//! 結果は必ず `1..` で始まり、 時刻ソート可能 + 同時代の id は先頭数文字が揃う (`nd_1C..`)。
//! **format は opaque** — hub も呼び手も中身を decode しない (routing key として不透明に扱う)。
//! serde は `transparent` で素の文字列として wire に乗る (人にも読める)。

use uuid::Uuid;

/// node_id の prefix (EntId 形式の type tag)。
pub const NODE_ID_PREFIX: &str = "nd_";

/// base58 alphabet (Bitcoin、 視認性のため `0 O I l` を除外)。
/// creo `packages/entid/src/base58.ts` の `ALPHABET` と同一。
const BASE58_ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// UUID (128bit) を creo EntId と同一規則で base58 短縮符号化する。
///
/// creo `encodeBase58` の忠実 port: u128 を base58 した上で、 **32-char zero-pad hex の先頭の
/// `0` nibble の数だけ** `1` を前置する (creo は byte でなく hex 文字単位で leading-zero を保存)。
/// UUID v7 は hex が `0..` 始まりなので先頭 nibble が 1 個 0 で、 結果は EntId と同じ `1..` 始まり。
fn encode_base58_uuid(uuid: Uuid) -> String {
    let mut num = uuid.as_u128();
    // creo: num === 0 は単一 '1' を返す (leading-zero 前置はしない)。UUID v7 では起きない。
    if num == 0 {
        return (BASE58_ALPHABET[0] as char).to_string();
    }
    let mut body: Vec<u8> = Vec::with_capacity(22);
    while num > 0 {
        body.push(BASE58_ALPHABET[(num % 58) as usize]);
        num /= 58;
    }
    body.reverse();

    // creo と同じ leading-zero 保存: 32-char zero-pad hex の先頭 '0' の数だけ '1' を足す。
    let hex = format!("{:032x}", uuid.as_u128());
    let leading = hex.bytes().take_while(|&c| c == b'0').count();
    let mut out = vec![BASE58_ALPHABET[0]; leading];
    out.extend_from_slice(&body);
    // BASE58_ALPHABET は ASCII のみなので from_utf8 は必ず成功する。
    String::from_utf8(out).expect("base58 output is ASCII")
}

/// home-node の位置独立 安定 id。opaque newtype (中身に依存しないこと)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct NodeId(String);

impl NodeId {
    /// 新規 node_id を生成する (`nd_` + creo 互換 base58(uuid_v7))。
    pub fn generate() -> Self {
        Self(format!(
            "{NODE_ID_PREFIX}{}",
            encode_base58_uuid(Uuid::now_v7())
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 空 id 判定 (legacy wire payload を `#[serde(default)]` で受けた時の値)。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for NodeId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_matches_creo_entid() {
        // creo packages/entid (canonical) と **byte 一致** を pin する。golden は実 creo を
        // bun 実行した値: uuid 019bdc73-aace-766c-8064-f43e2a8cfcd5 → "1CXK2Q8WPAUUueD7jdRJCt"。
        // これが通る = VP の node_id 本体は creo EntId と同じ符号 (= `mem_`/`atl_` と同形)。
        let uuid = Uuid::parse_str("019bdc73-aace-766c-8064-f43e2a8cfcd5").unwrap();
        assert_eq!(encode_base58_uuid(uuid), "1CXK2Q8WPAUUueD7jdRJCt");
    }

    #[test]
    fn generate_has_nd_prefix() {
        let id = NodeId::generate();
        assert!(
            id.as_str().starts_with(NODE_ID_PREFIX),
            "nd_ prefix が無い: {}",
            id
        );
        assert!(!id.is_empty());
    }

    #[test]
    fn generate_is_entid_shaped() {
        // EntId 形式: prefix の後ろは base58 本体で、 現状の UUID v7 era では `1` 始まり。
        let id = NodeId::generate();
        let body = id.as_str().strip_prefix(NODE_ID_PREFIX).expect("prefix");
        assert!(
            body.starts_with('1'),
            "UUID v7 hex は `0..` 始まりなので本体は '1' 始まりのはず: {}",
            body
        );
        // base58 charset は 0/O/I/l を含まない (= 視認性の高い EntId と同じ alphabet)。
        assert!(
            !body.contains(['0', 'O', 'I', 'l']),
            "base58 本体に紛らわしい文字が混入: {}",
            body
        );
    }

    #[test]
    fn generate_is_unique_across_calls() {
        // singleton だが採番自体は毎回ユニーク (load_or_create が「1 回だけ生成」を担う)。
        let a = NodeId::generate();
        let b = NodeId::generate();
        assert_ne!(a, b, "generate は毎回新しい id を返す");
    }

    #[test]
    fn roundtrips_through_string_and_serde() {
        let id = NodeId::generate();
        // From<String> で復元できる (db / wire から読んだ値の再構築経路)。
        let restored = NodeId::from(id.as_str().to_string());
        assert_eq!(id, restored);
        // serde transparent: 素の文字列として乗る (引用符付き JSON string)。
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id.as_str()));
        let de: NodeId = serde_json::from_str(&json).unwrap();
        assert_eq!(de, id);
    }
}
