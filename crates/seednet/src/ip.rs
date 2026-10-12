//! Addresses with a prefix length, and the netmask forms that cloud-init accepts for one.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use anyhow::{Context, Result, bail};

/// An address with its prefix length, written as `address/prefix` in a `.network` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl Cidr {
    /// Parses `address/prefix`, or a bare address with the prefix from `netmask`, or else the
    /// prefix cloud-init assumes for a static subnet without one (24 for IPv4, 64 for IPv6).
    pub fn parse(address: &str, netmask: Option<&str>) -> Result<Self> {
        let (addr, prefix) = match address.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (address, None),
        };

        let addr: IpAddr = addr
            .trim()
            .parse()
            .with_context(|| format!("invalid address: {address}"))?;

        let prefix = match (prefix, netmask) {
            (Some(prefix), _) | (None, Some(prefix)) => parse_prefix(prefix, addr.is_ipv6())?,
            (None, None) if addr.is_ipv6() => 64,
            (None, None) => 24,
        };

        Ok(Self { addr, prefix })
    }

    /// The destination of a default route: `0.0.0.0/0` or `::/0`.
    pub fn default_route(ipv6: bool) -> Self {
        let addr = if ipv6 {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        } else {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        };
        Self { addr, prefix: 0 }
    }

    /// Parses an address that must have its prefix length, as netplan takes it.
    pub fn parse_with_prefix(address: &str) -> Result<Self> {
        if !address.contains('/') {
            bail!("address {address} is missing /prefixlength");
        }
        Self::parse(address, None)
    }

    /// Parses an address with a prefix length, or a bare one that stands for the host alone.
    pub fn parse_host_or_network(address: &str) -> Result<Self> {
        if address.contains('/') {
            return Self::parse(address, None);
        }
        let addr = parse_addr(address)?;
        Ok(Self {
            addr,
            prefix: if addr.is_ipv6() { 128 } else { 32 },
        })
    }

    /// Whether `ip` lies in the network of this address.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// Parses a prefix length, or a netmask written as an address (`255.255.255.0`).
pub fn parse_prefix(netmask: &str, ipv6: bool) -> Result<u8> {
    let netmask = netmask.trim();
    let max = if ipv6 { 128 } else { 32 };

    if let Ok(prefix) = netmask.parse::<u8>() {
        if prefix > max {
            bail!("prefix length out of range: {netmask}");
        }
        return Ok(prefix);
    }

    let bits = if ipv6 {
        netmask.parse::<Ipv6Addr>().map(u128::from)
    } else {
        netmask
            .parse::<Ipv4Addr>()
            .map(|mask| u128::from(u32::from(mask)) << 96)
    }
    .with_context(|| format!("invalid netmask: {netmask}"))?;

    let prefix = bits.leading_ones();
    if bits.checked_shl(prefix).unwrap_or(0) != 0 {
        bail!("netmask is not contiguous: {netmask}");
    }

    Ok(prefix as u8)
}

/// Parses an address that must not carry a prefix, such as a gateway or a name server.
pub fn parse_addr(addr: &str) -> Result<IpAddr> {
    addr.trim()
        .parse()
        .with_context(|| format!("invalid address: {addr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prefix_forms() {
        assert_eq!(Cidr::parse("192.0.2.10/24", None).unwrap().to_string(), "192.0.2.10/24");
        assert_eq!(
            Cidr::parse("192.0.2.10", Some("255.255.255.128"))
                .unwrap()
                .to_string(),
            "192.0.2.10/25"
        );
        assert_eq!(Cidr::parse("192.0.2.10", Some("28")).unwrap().to_string(), "192.0.2.10/28");
        assert_eq!(Cidr::parse("192.0.2.10", None).unwrap().to_string(), "192.0.2.10/24");
        assert_eq!(Cidr::parse("2001:db8::10", None).unwrap().to_string(), "2001:db8::10/64");
        assert_eq!(
            Cidr::parse("2001:db8::10", Some("ffff:ffff:ffff:ffff:ffff:ffff::"))
                .unwrap()
                .prefix,
            96
        );
    }

    #[test]
    fn rejects_bad_netmasks() {
        assert!(parse_prefix("255.0.255.0", false).is_err());
        assert!(parse_prefix("33", false).is_err());
        assert!(parse_prefix("ffff::", false).is_err());
        assert!(Cidr::parse("192.0.2.300/24", None).is_err());
    }

    #[test]
    fn contains_checks_the_network() {
        let net = Cidr::parse("192.0.2.10/24", None).unwrap();
        assert!(net.contains("192.0.2.1".parse().unwrap()));
        assert!(!net.contains("192.0.3.1".parse().unwrap()));
        assert!(!net.contains("2001:db8::1".parse().unwrap()));

        let all = Cidr::parse("0.0.0.0/0", None).unwrap();
        assert!(all.contains("203.0.113.1".parse().unwrap()));
    }
}
