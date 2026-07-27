//! home-node が federation で advertise する到達可能 endpoint の収集 (ADR-020 D3、 S2)。
//!
//! ## 方針 (council 2026-06-27 / 28)
//! - **tailnet 非依存**: tailscale enroll 済みの相手しか繋げない依存は環境を絞るので外す。
//! - **IPv6 GUA を first-class な direct path に**: GUA は NAT が無く end-to-end routable で、
//!   障壁は firewall ポリシーだけ (IPv4 の carrier NAT/CGNAT と違い host 側で許可可能)。
//! - **relay が universal floor**: GUA 無し / inbound 閉塞の相手は hub 経由 relay に落ちる
//!   (別レイヤー、 ここでは扱わない)。dialer は「advertise された direct 候補を試行 → 全滅で
//!   relay」。本モジュールは *direct 候補* を作る側。
//! - **IPv4 は必要が出てから** (今は IPv6 GUA のみ、 candidate 配列で前方互換に増やせる)。
//!
//! ## 取得手段 = connect-trick (dep 0)
//! 公開 IPv6 へ UDP `connect` すると、 **パケットは出ない**まま OS の routing table が
//! 「その宛先に使う source address」を選ぶ。その local_addr を読めば host の外向き GUA が 1 個
//! 得られる (= 最も reachable な候補)。multi-homing で複数 GUA を出したくなったら interface
//! 列挙 (`if-addrs` 等) に上げる余地を candidate 配列の形で残す。

use std::net::{IpAddr, Ipv6Addr, UdpSocket};

/// IPv6 が federation で advertise 可能 (= LAN を超えて direct 到達しうる) か判定する純関数。
///
/// **global unicast `2000::/3` のみ** 受理する。link-local (`fe80::/10`) と ULA (`fc00::/7`) は
/// LAN を超えないため除外、 loopback / unspecified / multicast も除外。
fn is_advertisable_ipv6(addr: &Ipv6Addr) -> bool {
    if addr.is_loopback() || addr.is_unspecified() || addr.is_multicast() {
        return false;
    }
    // global unicast 2000::/3 = 先頭 3bit が 001 (= 先頭 hextet が 0x2000..=0x3FFF)。
    // std の Ipv6Addr::is_global 等は unstable なので segments() で直接判定する。
    (addr.segments()[0] & 0xe000) == 0x2000
}

/// この host が advertise すべき IPv6 GUA endpoint (`[GUA]:port`) を 1 個返す。
///
/// connect-trick で OS の外向き source GUA を取得する。IPv6 経路が無い / 選ばれた address が
/// advertise 不可 (GUA でない) なら `None` (= IPv6 direct 候補なし、 relay floor に委ねる)。
/// `port` は daemon QUIC port (peer はここに QUIC で dial する)。
pub fn local_ipv6_endpoint(port: u16) -> Option<String> {
    // 公開 IPv6 (Google Public DNS) へ UDP connect。connect は routing 解決のみでパケットは出ない。
    let probe = UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).ok()?;
    let public_v6 = Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888);
    probe.connect((public_v6, 53)).ok()?;
    let IpAddr::V6(v6) = probe.local_addr().ok()?.ip() else {
        return None;
    };
    if !is_advertisable_ipv6(&v6) {
        return None;
    }
    Some(format!("[{v6}]:{port}"))
}

/// この host が advertise する direct endpoint 候補の配列を返す (現状 IPv6 GUA のみ)。
///
/// hub registry に載せる `endpoints` field の元。空配列 = direct 候補なし (relay floor 行き)。
/// IPv4 / LAN / 複数 GUA は必要が出た時点で additive に足す。
pub fn local_advertised_endpoints(port: u16) -> Vec<String> {
    local_ipv6_endpoint(port).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertisable_accepts_global_unicast() {
        // 2000::/3 の GUA は受理。
        let gua: Ipv6Addr = "2400:4150:0:1::abcd".parse().unwrap();
        assert!(is_advertisable_ipv6(&gua));
        let gua2: Ipv6Addr = "2001:db8::1".parse().unwrap();
        assert!(is_advertisable_ipv6(&gua2));
    }

    #[test]
    fn advertisable_rejects_non_global() {
        // LAN を超えない / 到達 advertise すべきでない address は全て拒否。
        let cases = [
            "::1",               // loopback
            "::",                // unspecified
            "fe80::1",           // link-local
            "fc00::1",           // ULA (fc00::/7)
            "fd12:3456:789a::1", // ULA (fd..)
            "ff02::1",           // multicast
        ];
        for s in cases {
            let addr: Ipv6Addr = s.parse().unwrap();
            assert!(!is_advertisable_ipv6(&addr), "{s} は advertise 不可のはず",);
        }
    }

    #[test]
    fn local_endpoint_is_well_formed_when_present() {
        // env 依存 (CI runner に IPv6 GUA が無ければ None)。Some の時のみ形式を検証する。
        if let Some(ep) = local_ipv6_endpoint(32000) {
            let parsed: std::net::SocketAddrV6 =
                ep.parse().expect("[ipv6]:port としてパースできる");
            assert_eq!(parsed.port(), 32000);
            assert!(is_advertisable_ipv6(parsed.ip()), "GUA のみ advertise する");
        }
    }
}
