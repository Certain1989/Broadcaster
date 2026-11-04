// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Address *classification*, not user consent. Each network consumer must
//! explicitly decide which classes it authorizes before a pinned socket dial.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Destination {
    Public,
    Lan,
    Loopback,
    LinkLocal,
    CarrierGrade,
    Denied,
}

pub(crate) fn classify(ip: IpAddr) -> Destination {
    match ip {
        IpAddr::V4(ip) => ipv4(ip),
        IpAddr::V6(ip) => ipv6(ip),
    }
}

fn ipv4(ip: Ipv4Addr) -> Destination {
    let [a, b, c, _] = ip.octets();
    if a == 127 {
        return Destination::Loopback;
    }
    if a == 169 && b == 254 {
        return Destination::LinkLocal;
    }
    if a == 100 && (64..=127).contains(&b) {
        return Destination::CarrierGrade;
    }
    if a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168) {
        return Destination::Lan;
    }
    if a == 0
        || a >= 224
        || (a == 192 && b == 0 && matches!(c, 0 | 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && matches!(b, 18 | 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
    {
        Destination::Denied
    } else {
        Destination::Public
    }
}

fn ipv6(ip: Ipv6Addr) -> Destination {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return ipv4(mapped);
    }
    if ip.is_loopback() {
        return Destination::Loopback;
    }
    if ip.is_unique_local() {
        return Destination::Lan;
    }
    if ip.is_unicast_link_local() {
        return Destination::LinkLocal;
    }
    let [first, second, ..] = ip.segments();
    if first & 0xe000 != 0x2000
        || (first == 0x2001 && (second <= 0x01ff || second == 0x0db8))
        || first == 0x2002
        || first == 0x3fff
    {
        Destination::Denied
    } else {
        Destination::Public
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_ranges_and_mapped_addresses_never_gain_public_trust() {
        for (ip, expected) in [
            ("10.0.0.1", Destination::Lan),
            ("fd00::1", Destination::Lan),
            ("::ffff:10.0.0.1", Destination::Lan),
            ("127.123.1.1", Destination::Loopback),
            ("::1", Destination::Loopback),
            ("169.254.169.254", Destination::LinkLocal),
            ("fe80::1", Destination::LinkLocal),
            ("100.64.0.1", Destination::CarrierGrade),
            ("2001:db8::1", Destination::Denied),
            ("3fff:1234::1", Destination::Denied),
            ("192.0.2.1", Destination::Denied),
            ("8.8.8.8", Destination::Public),
            ("2606:4700::1111", Destination::Public),
        ] {
            assert_eq!(classify(ip.parse().unwrap()), expected, "{ip}");
        }
    }
}
