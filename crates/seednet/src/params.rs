//! The parameters of bonds and bridges, which version 1 and version 2 spell differently but mean
//! the same. Both are read with the version 2 names; the version 1 ones are mapped to them first.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use yaml_serde::Value;

use crate::ip::parse_addr;
use crate::network::{Bond, Bridge};

/// Checks a VLAN ID, which is 1 to 4094.
pub fn vlan_id(id: u32) -> Result<u16> {
    u16::try_from(id)
        .ok()
        .filter(|id| (1..4095).contains(id))
        .with_context(|| format!("VLAN ID out of range: {id}"))
}

/// A bridge with the settings of its ports, by port ID.
#[derive(Debug, Default)]
pub struct BridgeParams {
    pub bridge: Bridge,
    pub path_costs: BTreeMap<String, u32>,
    pub port_priorities: BTreeMap<String, u32>,
}

/// The version 2 name of a version 1 bond parameter.
pub fn v1_bond_key(key: &str) -> String {
    let name = match key.replace('_', "-").as_str() {
        "bond-mode" => "mode",
        "bond-lacp-rate" => "lacp-rate",
        "bond-miimon" => "mii-monitor-interval",
        "bond-updelay" => "up-delay",
        "bond-downdelay" => "down-delay",
        "bond-xmit-hash-policy" => "transmit-hash-policy",
        "bond-ad-select" => "ad-select",
        "bond-arp-interval" => "arp-interval",
        "bond-arp-ip-target" => "arp-ip-targets",
        "bond-fail-over-mac" => "fail-over-mac-policy",
        "bond-min-links" => "min-links",
        "bond-primary" => "primary",
        _ => key,
    };
    name.to_owned()
}

/// The version 2 name of a version 1 bridge parameter.
pub fn v1_bridge_key(key: &str) -> &str {
    match key {
        "bridge_stp" => "stp",
        "bridge_fd" => "forward-delay",
        "bridge_hello" => "hello-time",
        "bridge_maxage" => "max-age",
        "bridge_ageing" => "ageing-time",
        "bridge_bridgeprio" => "priority",
        "bridge_pathcost" => "path-cost",
        "bridge_portprio" => "port-priority",
        _ => key,
    }
}

/// The bond modes that both networkd and NetworkManager take, as netplan does but for those of
/// Open vSwitch.
const MODES: &[&str] = &[
    "balance-rr",
    "active-backup",
    "balance-xor",
    "broadcast",
    "802.3ad",
    "balance-tlb",
    "balance-alb",
];

/// The transmit hash policies that both networkd and NetworkManager take.
const TRANSMIT_HASH_POLICIES: &[&str] = &[
    "layer2",
    "layer3+4",
    "layer2+3",
    "encap2+3",
    "encap3+4",
    "vlan+srcmac",
];

/// Reads the bond parameters of `id`, and warns about the ones it does not take.
pub fn bond(id: &str, params: &[(&str, &Value)], warnings: &mut Vec<String>) -> Result<Bond> {
    let mut bond = Bond::default();
    for &(key, value) in params {
        let context = || format!("invalid bond parameter {key} of {id}");
        match key {
            "mode" => bond.mode = Some(one_of(value, MODES).with_context(context)?),
            "lacp-rate" => {
                bond.lacp_rate = Some(one_of(value, &["slow", "fast"]).with_context(context)?)
            }
            "mii-monitor-interval" => {
                bond.mii_monitor_interval = Some(millis(value).with_context(context)?);
            }
            "up-delay" => bond.up_delay = Some(millis(value).with_context(context)?),
            "down-delay" => bond.down_delay = Some(millis(value).with_context(context)?),
            "transmit-hash-policy" => {
                bond.transmit_hash_policy =
                    Some(one_of(value, TRANSMIT_HASH_POLICIES).with_context(context)?);
            }
            "ad-select" => {
                bond.ad_select =
                    Some(one_of(value, &["stable", "bandwidth", "count"]).with_context(context)?);
            }
            "arp-interval" => bond.arp_interval = Some(millis(value).with_context(context)?),
            "arp-ip-targets" => {
                for target in list(value).with_context(context)? {
                    bond.arp_ip_targets
                        .push(parse_addr(&target).with_context(context)?);
                }
            }
            "fail-over-mac-policy" => {
                bond.fail_over_mac_policy =
                    Some(one_of(value, &["none", "active", "follow"]).with_context(context)?);
            }
            "min-links" => bond.min_links = Some(number(value).with_context(context)?),
            "primary" => bond.primary = Some(string(value).with_context(context)?),
            _ => warnings.push(format!("skipping the unsupported bond parameter {key} of {id}")),
        }
    }
    Ok(bond)
}

/// Reads the bridge parameters of `id`, and warns about the ones it does not take.
pub fn bridge(
    id: &str,
    params: &[(&str, &Value)],
    warnings: &mut Vec<String>,
) -> Result<BridgeParams> {
    let mut out = BridgeParams::default();
    let bridge = &mut out.bridge;
    for &(key, value) in params {
        let context = || format!("invalid bridge parameter {key} of {id}");
        match key {
            "stp" => bridge.stp = Some(boolean(value).with_context(context)?),
            "forward-delay" => bridge.forward_delay = Some(seconds(value).with_context(context)?),
            "hello-time" => bridge.hello_time = Some(seconds(value).with_context(context)?),
            "max-age" => bridge.max_age = Some(seconds(value).with_context(context)?),
            "ageing-time" | "aging-time" => {
                bridge.ageing_time = Some(seconds(value).with_context(context)?);
            }
            "priority" => bridge.priority = Some(number(value).with_context(context)?),
            "path-cost" => out.path_costs = port_map(value).with_context(context)?,
            "port-priority" => out.port_priorities = port_map(value).with_context(context)?,
            _ => warnings.push(format!("skipping the unsupported bridge parameter {key} of {id}")),
        }
    }
    Ok(out)
}

/// A setting per port: a mapping in version 2, and a list of `port value` in version 1.
fn port_map(value: &Value) -> Result<BTreeMap<String, u32>> {
    match value {
        Value::Mapping(mapping) => mapping
            .iter()
            .map(|(port, value)| Ok((string(port)?, number(value)?)))
            .collect(),
        _ => list(value)?
            .chunks(2)
            .map(|pair| match pair {
                [port, value] => Ok((port.clone(), number(&Value::String(value.clone()))?)),
                _ => bail!("not a list of port and value: {value:?}"),
            })
            .collect(),
    }
}

fn string(value: &Value) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => bail!("not a string: {value:?}"),
    }
}

/// A string that must be one of `allowed`.
fn one_of(value: &Value, allowed: &[&str]) -> Result<String> {
    let s = string(value)?;
    if !allowed.contains(&s.as_str()) {
        bail!("{s} is not one of {}", allowed.join(", "));
    }
    Ok(s)
}

fn number(value: &Value) -> Result<u32> {
    let s = string(value)?;
    s.trim()
        .parse()
        .with_context(|| format!("not a number: {s}"))
}

/// A YAML 1.1 boolean, as PyYAML and ifupdown take it.
fn boolean(value: &Value) -> Result<bool> {
    match string(value)?.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "y" | "1" => Ok(true),
        "false" | "no" | "off" | "n" | "0" => Ok(false),
        other => bail!("not a boolean: {other}"),
    }
}

/// A time in milliseconds: a bare number, or one with an `ms` or `s` suffix.
fn millis(value: &Value) -> Result<u32> {
    let s = string(value)?;
    let s = s.trim();
    let (n, scale) = match s.strip_suffix("ms") {
        Some(n) => (n, 1),
        None => match s.strip_suffix('s') {
            Some(n) => (n, 1000),
            None => (s, 1),
        },
    };
    let n: u32 = n
        .trim()
        .parse()
        .with_context(|| format!("not a time: {s}"))?;
    n.checked_mul(scale)
        .with_context(|| format!("time out of range: {s}"))
}

/// A time in seconds: a bare number, or one with an `s` suffix.
fn seconds(value: &Value) -> Result<u32> {
    let s = string(value)?;
    let s = s.trim();
    let n = if s.ends_with("ms") {
        s
    } else {
        s.strip_suffix('s').unwrap_or(s)
    };
    n.trim()
        .parse()
        .with_context(|| format!("not a number of seconds: {s}"))
}

/// A list, or a string of items split on whitespace or commas. The items of a list are split
/// too, as version 1 writes `[eth0 50, eth1 60]` for a setting per port.
fn list(value: &Value) -> Result<Vec<String>> {
    let items = match value {
        Value::Sequence(items) => items.iter().map(string).collect::<Result<Vec<_>>>()?,
        _ => vec![string(value)?],
    };
    Ok(items
        .iter()
        .flat_map(|item| item.split(|c: char| c.is_whitespace() || c == ','))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(s: &str) -> Value {
        yaml_serde::from_str(s).unwrap()
    }

    #[test]
    fn reads_times() {
        assert_eq!(millis(&value("100")).unwrap(), 100);
        assert_eq!(millis(&value("100ms")).unwrap(), 100);
        assert_eq!(millis(&value("2s")).unwrap(), 2000);
        assert_eq!(seconds(&value("4")).unwrap(), 4);
        assert_eq!(seconds(&value("4s")).unwrap(), 4);
        assert!(seconds(&value("400ms")).is_err());
        assert!(millis(&value("soon")).is_err());
    }

    #[test]
    fn reads_port_settings_of_both_versions() {
        let v2 = port_map(&value("{eth0: 50, eth1: 60}")).unwrap();
        let v1 = port_map(&value("[eth0 50, eth1 60]")).unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1["eth1"], 60);
        assert!(port_map(&value("[eth0]")).is_err());
    }

    #[test]
    fn maps_version_1_names() {
        assert_eq!(v1_bond_key("bond-miimon"), "mii-monitor-interval");
        assert_eq!(v1_bond_key("bond_xmit_hash_policy"), "transmit-hash-policy");
        assert_eq!(v1_bridge_key("bridge_fd"), "forward-delay");
    }
}
