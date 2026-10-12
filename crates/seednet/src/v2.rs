//! Version 2 of network-config:
//! <https://docs.cloud-init.io/en/latest/reference/network-config-format-v2.html>

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use yaml_serde::Value;

use crate::ip::{Cidr, parse_addr};
use crate::lenient::{self, Rest};
use crate::network::{
    AddressOptions, DhcpOverrides, Kind, Network, Route, RoutingRule, attach_ports, push_unique,
};
use crate::params;

/// The device types that nothing here brings up.
const UNSUPPORTED: &[&str] = &["wifis", "tunnels", "vrfs", "modems"];

/// The keys that mean nothing here, so that they need no warning: seednet is the renderer.
const IGNORED: &[&str] = &["version", "renderer"];

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default)]
    ethernets: BTreeMap<String, Ethernet>,
    #[serde(default)]
    bonds: BTreeMap<String, Controller>,
    #[serde(default)]
    bridges: BTreeMap<String, Controller>,
    #[serde(default)]
    vlans: BTreeMap<String, Vlan>,
    #[serde(flatten)]
    rest: Rest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Ethernet {
    #[serde(rename = "match")]
    matches: Option<Match>,
    set_name: Option<String>,
    macaddress: Option<String>,
    #[serde(flatten)]
    common: Common,
}

/// A bond or a bridge.
#[derive(Debug, Deserialize)]
struct Controller {
    #[serde(default, deserialize_with = "lenient::list")]
    interfaces: Vec<String>,
    /// Even an empty one turns STP on for a bridge, as with netplan.
    parameters: Option<BTreeMap<String, Value>>,
    macaddress: Option<String>,
    #[serde(flatten)]
    common: Common,
}

#[derive(Debug, Deserialize)]
struct Vlan {
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    id: Option<u32>,
    link: Option<String>,
    macaddress: Option<String>,
    #[serde(flatten)]
    common: Common,
}

/// What every device type has.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Common {
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    dhcp4: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    dhcp6: Option<bool>,
    #[serde(default)]
    addresses: Vec<Value>,
    gateway4: Option<String>,
    gateway6: Option<String>,
    nameservers: Option<Nameservers>,
    #[serde(default)]
    routes: Vec<RouteConfig>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    mtu: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    accept_ra: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    optional: Option<bool>,
    link_local: Option<Vec<String>>,
    dhcp4_overrides: Option<Overrides>,
    dhcp6_overrides: Option<Overrides>,
    #[serde(default)]
    routing_policy: Vec<RuleConfig>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    ipv6_privacy: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    ipv6_mtu: Option<u32>,
    dhcp_identifier: Option<String>,
    /// What none of the above reads, which goes after them all.
    #[serde(flatten)]
    rest: Rest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Overrides {
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    use_dns: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    use_ntp: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    use_hostname: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    use_mtu: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    use_routes: Option<bool>,
    /// A boolean, or `route`.
    use_domains: Option<Value>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    send_hostname: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    hostname: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    route_metric: Option<u32>,
    #[serde(flatten)]
    rest: Rest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RuleConfig {
    from: Option<String>,
    to: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    table: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    priority: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    mark: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    type_of_service: Option<u32>,
    #[serde(flatten)]
    rest: Rest,
}

/// The options of an address.
#[derive(Debug, Deserialize)]
struct AddressConfig {
    #[serde(default, deserialize_with = "lenient::opt_string")]
    label: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    lifetime: Option<String>,
    #[serde(flatten)]
    rest: Rest,
}

#[derive(Debug, Deserialize)]
struct Match {
    macaddress: Option<String>,
    name: Option<String>,
    /// A glob, or a list of them.
    #[serde(default, deserialize_with = "lenient::list")]
    driver: Vec<String>,
    #[serde(flatten)]
    rest: Rest,
}

#[derive(Debug, Deserialize)]
struct Nameservers {
    #[serde(default, deserialize_with = "lenient::list")]
    addresses: Vec<String>,
    #[serde(default, deserialize_with = "lenient::list")]
    search: Vec<String>,
    #[serde(flatten)]
    rest: Rest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RouteConfig {
    to: Option<String>,
    via: Option<String>,
    from: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    metric: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_bool")]
    on_link: Option<bool>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    table: Option<u32>,
    scope: Option<String>,
    #[serde(rename = "type")]
    route_type: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    mtu: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    congestion_window: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    advertised_receive_window: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    advertised_mss: Option<u32>,
    #[serde(flatten)]
    rest: Rest,
}

/// The route types that netplan takes.
const ROUTE_TYPES: &[&str] = &[
    "unicast",
    "anycast",
    "blackhole",
    "broadcast",
    "local",
    "multicast",
    "nat",
    "prohibit",
    "throw",
    "unreachable",
    "xresolve",
];

pub fn convert(value: Value, warnings: &mut Vec<String>) -> Result<Vec<Network>> {
    for kind in UNSUPPORTED {
        if value.get(kind).is_some() {
            warnings.push(format!("skipping the unsupported type: {kind}"));
        }
    }

    let mut config: Config = yaml_serde::from_value(value).context("invalid version 2 config")?;
    config
        .rest
        .retain(|key, _| !UNSUPPORTED.contains(&key.as_str()) && !IGNORED.contains(&key.as_str()));
    lenient::warn_unknown(&config.rest, "the config", warnings);
    let mut networks = Vec::new();
    let mut ports = Vec::new();

    for (id, ethernet) in config.ethernets {
        networks.push(ethernet_network(id, ethernet, warnings)?);
    }
    for (id, bond) in config.bonds {
        let parameters = pairs(bond.parameters.as_ref());
        let kind = Kind::Bond(Box::new(params::bond(&id, &parameters, warnings)?));
        ports.push((id.clone(), bond.interfaces, None));
        networks.push(network(id, kind, bond.macaddress, bond.common, warnings)?);
    }
    for (id, bridge) in config.bridges {
        let parameters = pairs(bridge.parameters.as_ref());
        let mut params = params::bridge(&id, &parameters, warnings)?;
        if bridge.parameters.is_some() {
            params.bridge.stp.get_or_insert(true);
        }
        let kind = Kind::Bridge(params.bridge.clone());
        ports.push((id.clone(), bridge.interfaces, Some(params)));
        networks.push(network(id, kind, bridge.macaddress, bridge.common, warnings)?);
    }
    for (id, vlan) in config.vlans {
        let (Some(vlan_id), Some(link)) = (vlan.id, vlan.link) else {
            bail!("the VLAN {id} needs both id and link");
        };
        let kind = Kind::Vlan {
            id: params::vlan_id(vlan_id)?,
            link,
        };
        networks.push(network(id, kind, vlan.macaddress, vlan.common, warnings)?);
    }

    for (controller, interfaces, bridge) in ports {
        attach_ports(&mut networks, &controller, &interfaces, bridge.as_ref())?;
    }

    Ok(networks)
}

fn pairs(parameters: Option<&BTreeMap<String, Value>>) -> Vec<(&str, &Value)> {
    parameters
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.as_str(), v))
        .collect()
}

fn ethernet_network(id: String, ethernet: Ethernet, warnings: &mut Vec<String>) -> Result<Network> {
    // Without a match the ID is the interface name.
    let (mac_address, name, drivers) = match ethernet.matches {
        Some(m) => {
            if m.macaddress.is_none() && m.name.is_none() && m.driver.is_empty() {
                bail!("the match of {id} has none of macaddress, name and driver");
            }
            lenient::warn_unknown(&m.rest, &format!("the match of {id}"), warnings);
            (m.macaddress, m.name, m.driver)
        }
        None => {
            if ethernet.set_name.is_some() {
                bail!("{id} has set-name without match");
            }
            (None, Some(id.clone()), Vec::new())
        }
    };

    let mut network = network(id, Kind::Ethernet, ethernet.macaddress, ethernet.common, warnings)?;
    network.mac_address = mac_address;
    network.name = name;
    network.drivers = drivers;
    network.set_name = ethernet.set_name;
    Ok(network)
}

fn network(
    id: String,
    kind: Kind,
    mac_address: Option<String>,
    mut common: Common,
    warnings: &mut Vec<String>,
) -> Result<Network> {
    common
        .rest
        .retain(|key, _| !IGNORED.contains(&key.as_str()));
    lenient::warn_unknown(&common.rest, &id, warnings);

    let link_local = common.link_local.map(|families| {
        let ipv4 = families.iter().any(|f| f == "ipv4");
        let ipv6 = families.iter().any(|f| f == "ipv6");
        match (ipv4, ipv6) {
            (true, true) => "yes",
            (true, false) => "ipv4",
            (false, true) => "ipv6",
            (false, false) => "no",
        }
        .to_owned()
    });

    let mut network = Network {
        set_mac_address: mac_address,
        mtu: common.mtu,
        optional: common.optional.unwrap_or(false),
        dhcp4: common.dhcp4.unwrap_or(false),
        dhcp6: common.dhcp6.unwrap_or(false),
        accept_ra: common.accept_ra,
        link_local,
        ipv6_privacy: common.ipv6_privacy,
        ipv6_mtu: common.ipv6_mtu,
        dhcp_identifier: common.dhcp_identifier,
        id,
        kind,
        ..Default::default()
    };
    if let Some(identifier) = &network.dhcp_identifier
        && !matches!(identifier.as_str(), "mac" | "duid")
    {
        bail!("invalid dhcp-identifier of {}: {identifier}, not mac or duid", network.id);
    }
    let dhcp4_overrides = overrides(&network.id, "dhcp4", common.dhcp4_overrides, warnings)?;
    let dhcp6_overrides = overrides(&network.id, "dhcp6", common.dhcp6_overrides, warnings)?;
    network.dhcp_overrides =
        combine_overrides(&network, dhcp4_overrides, dhcp6_overrides, warnings)?;

    for address in &common.addresses {
        let (address, options) = match address {
            Value::String(address) => (address.as_str(), None),
            // An address with options: {"192.0.2.10/24": {lifetime: 0, label: ...}}
            Value::Mapping(mapping) if mapping.len() == 1 => {
                let Some((Value::String(key), options)) = mapping.iter().next() else {
                    bail!("invalid address of {}: {address:?}", network.id);
                };
                (key.as_str(), Some(options))
            }
            _ => bail!("invalid address of {}: {address:?}", network.id),
        };
        let address = Cidr::parse_with_prefix(address)
            .with_context(|| format!("invalid address of {}", network.id))?;
        push_unique(&mut network.addresses, address);
        if let Some(options) = options.filter(|options| !options.is_null()) {
            let what = format!("the address {address} of {}", network.id);
            let options: AddressConfig = yaml_serde::from_value(options.clone())
                .with_context(|| format!("invalid options of {what}"))?;
            lenient::warn_unknown(&options.rest, &what, warnings);
            let mut label = options.label;
            if address.addr.is_ipv6() && label.take().is_some() {
                warnings.push(format!("skipping the label of {what}, which only IPv4 has"));
            }
            if let Some(label) = &label
                && (!label.is_ascii() || !(1..=15).contains(&label.len()))
            {
                bail!("invalid label of {what}: {label}, not 1 to 15 ASCII characters");
            }
            if let Some(lifetime) = &options.lifetime
                && !matches!(lifetime.as_str(), "forever" | "0")
            {
                bail!("invalid lifetime of {what}: {lifetime}, not forever or 0");
            }
            if label.is_some() || options.lifetime.is_some() {
                network.address_options.push(AddressOptions {
                    address,
                    label,
                    preferred_lifetime: options.lifetime,
                });
            }
        }
    }

    for gateway in [&common.gateway4, &common.gateway6].into_iter().flatten() {
        push_unique(&mut network.gateways, parse_addr(gateway)?);
    }

    if let Some(nameservers) = common.nameservers {
        let what = format!("the nameservers of {}", network.id);
        lenient::warn_unknown(&nameservers.rest, &what, warnings);
        for address in &nameservers.addresses {
            push_unique(&mut network.nameservers, parse_addr(address)?);
        }
        for search in nameservers.search {
            push_unique(&mut network.search, search);
        }
    }

    for route in common.routes {
        let what = format!("a route of {}", network.id);
        lenient::warn_unknown(&route.rest, &what, warnings);
        push_unique(&mut network.routes, route_of(route, &what)?);
    }

    for rule in common.routing_policy {
        let what = format!("a routing policy rule of {}", network.id);
        lenient::warn_unknown(&rule.rest, &what, warnings);
        let from = rule
            .from
            .as_deref()
            .map(Cidr::parse_host_or_network)
            .transpose()?;
        let to = rule
            .to
            .as_deref()
            .map(Cidr::parse_host_or_network)
            .transpose()?;
        match (from, to) {
            (None, None) => bail!("{what} has neither from nor to"),
            (Some(from), Some(to)) if from.addr.is_ipv6() != to.addr.is_ipv6() => {
                bail!("{what} has from and to of different families")
            }
            _ => {}
        }
        let type_of_service = rule
            .type_of_service
            .map(u8::try_from)
            .transpose()
            .with_context(|| format!("type-of-service out of range in {what}"))?;
        let rule = RoutingRule {
            from,
            to,
            table: rule.table,
            priority: rule.priority,
            mark: rule.mark,
            type_of_service,
        };
        push_unique(&mut network.routing_policy, rule);
    }

    Ok(network)
}

/// A route, as netplan reads it.
fn route_of(route: RouteConfig, what: &str) -> Result<Route> {
    let gateway = route.via.as_deref().map(parse_addr).transpose()?;
    // `default` takes the family of the gateway, and a bare address is a route to that host.
    let destination = match route.to.as_deref().map(str::trim) {
        None => bail!("{what} has no to"),
        Some("default") => match gateway {
            Some(gateway) => Cidr::default_route(gateway.is_ipv6()),
            None => bail!("{what} to default has no via, which would tell its family"),
        },
        Some(to) => Cidr::parse_host_or_network(to)?,
    };
    let from = route.from.as_deref().map(parse_addr).transpose()?;
    for address in [gateway, from].into_iter().flatten() {
        if address.is_ipv6() != destination.addr.is_ipv6() {
            bail!("{what} has addresses of different families");
        }
    }

    let route_type = route.route_type.unwrap_or_else(|| "unicast".to_owned());
    if !ROUTE_TYPES.contains(&route_type.as_str()) {
        bail!("invalid type of {what}: {route_type}");
    }
    // The scope that netplan gives a route that says none.
    let scope = match route.scope {
        Some(scope) if matches!(scope.as_str(), "global" | "link" | "host") => scope,
        Some(scope) => bail!("invalid scope of {what}: {scope}, not global, link or host"),
        None if matches!(route_type.as_str(), "local" | "nat") => "host".to_owned(),
        None if route_type == "unicast" && gateway.is_none()
            || matches!(route_type.as_str(), "broadcast" | "multicast" | "anycast") =>
        {
            "link".to_owned()
        }
        None => "global".to_owned(),
    };
    if route_type == "unicast" && scope == "global" && gateway.is_none() {
        bail!("{what} is a global unicast route without via");
    }

    Ok(Route {
        destination,
        gateway,
        from,
        metric: route.metric,
        on_link: route.on_link.unwrap_or(false),
        table: route.table,
        scope,
        route_type,
        mtu: route.mtu,
        congestion_window: route.congestion_window,
        advertised_receive_window: route.advertised_receive_window,
        advertised_mss: route.advertised_mss,
    })
}

/// The overrides of both DHCPv4 and DHCPv6, as netplan takes them with networkd: those of the one
/// that is enabled, which must be the same when both are.
fn combine_overrides(
    network: &Network,
    dhcp4: DhcpOverrides,
    mut dhcp6: DhcpOverrides,
    warnings: &mut Vec<String>,
) -> Result<DhcpOverrides> {
    let id = &network.id;
    if !network.dhcp4 && !dhcp4.is_empty() {
        warnings.push(format!("skipping the dhcp4-overrides of {id}, without dhcp4"));
    }
    if !network.dhcp6 && !dhcp6.is_empty() {
        warnings.push(format!("skipping the dhcp6-overrides of {id}, without dhcp6"));
    }

    match (network.dhcp4, network.dhcp6) {
        (true, true) if dhcp4 != dhcp6 => {
            bail!(
                "the dhcp4-overrides and dhcp6-overrides of {id} differ, as netplan does not take"
            )
        }
        (true, _) => Ok(dhcp4),
        (false, true) => {
            // As in netplan, these are for DHCPv4 alone.
            let dhcp4_only = [
                ("use-mtu", dhcp6.use_mtu.take().is_some()),
                ("use-routes", dhcp6.use_routes.take().is_some()),
                ("use-hostname", dhcp6.use_hostname.take().is_some()),
                ("hostname", dhcp6.hostname.take().is_some()),
            ];
            for (key, set) in dhcp4_only {
                if set {
                    warnings.push(format!(
                        "skipping {key} of the dhcp6-overrides of {id}, which is for DHCPv4 alone"
                    ));
                }
            }
            Ok(dhcp6)
        }
        (false, false) => Ok(DhcpOverrides::default()),
    }
}

/// The overrides of `dhcp4` or `dhcp6`.
fn overrides(
    id: &str,
    family: &str,
    overrides: Option<Overrides>,
    warnings: &mut Vec<String>,
) -> Result<DhcpOverrides> {
    let Some(overrides) = overrides else {
        return Ok(DhcpOverrides::default());
    };
    lenient::warn_unknown(&overrides.rest, &format!("the {family}-overrides of {id}"), warnings);
    let use_domains = match &overrides.use_domains {
        None => None,
        Some(Value::Bool(value)) => Some(if *value { "yes" } else { "no" }),
        Some(Value::String(value)) => match value.to_ascii_lowercase().as_str() {
            "route" => Some("route"),
            "true" | "yes" | "on" | "y" => Some("yes"),
            "false" | "no" | "off" | "n" => Some("no"),
            _ => bail!("invalid use-domains of {id}: {value}"),
        },
        Some(value) => bail!("invalid use-domains of {id}: {value:?}"),
    };
    Ok(DhcpOverrides {
        use_dns: overrides.use_dns,
        use_ntp: overrides.use_ntp,
        use_hostname: overrides.use_hostname,
        use_mtu: overrides.use_mtu,
        use_routes: overrides.use_routes,
        use_domains: use_domains.map(str::to_owned),
        send_hostname: overrides.send_hostname,
        hostname: overrides.hostname,
        route_metric: overrides.route_metric,
    })
}
