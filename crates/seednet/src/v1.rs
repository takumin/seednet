//! Version 1 of network-config:
//! <https://docs.cloud-init.io/en/latest/reference/network-config-format-v1.html>
//!
//! It is read as cloud-init converts it to version 2 for netplan, with two exceptions: the name
//! servers of `type: nameserver` with `interface`, and the routes of `type: route`, which that
//! conversion drops, apply as the format says. So `optional`, which it drops too, is skipped, and a
//! subnet metric of 0 is none, as it takes a value that is not true for none.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use yaml_serde::Value;

use crate::ip::{Cidr, parse_addr};
use crate::lenient::{self, Rest};
use crate::network::{DhcpOverrides, Kind, Network, Route, attach_ports, push_unique};
use crate::params;

#[derive(Debug, Deserialize)]
struct Config {
    #[serde(default)]
    config: Vec<Value>,
    #[serde(flatten)]
    rest: Rest,
}

/// The keys that mean nothing here, so that they need no warning.
const IGNORED: &[&str] = &["version", "type"];

/// The subnet types that cloud-init converts to `dhcp6`, with router advertisements.
const IPV6_DYNAMIC: &[&str] = &[
    "ipv6_slaac",
    "ipv6_dhcpv6-stateless",
    "ipv6_dhcpv6-stateful",
];

/// Warns about the keys in `rest` but the ignored ones.
fn warn_unknown(mut rest: Rest, what: &str, warnings: &mut Vec<String>) {
    rest.retain(|key, _| !IGNORED.contains(&key.as_str()));
    lenient::warn_unknown(&rest, what, warnings);
}

#[derive(Debug, Deserialize)]
struct Physical {
    name: Option<String>,
    mac_address: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    mtu: Option<u32>,
    #[serde(default, rename = "accept-ra", deserialize_with = "lenient::opt_bool")]
    accept_ra: Option<bool>,
    #[serde(default)]
    subnets: Vec<Subnet>,
    #[serde(flatten)]
    rest: Rest,
}

/// A bond or a bridge.
#[derive(Debug, Deserialize)]
struct Controller {
    name: Option<String>,
    mac_address: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    mtu: Option<u32>,
    #[serde(default, rename = "accept-ra", deserialize_with = "lenient::opt_bool")]
    accept_ra: Option<bool>,
    #[serde(
        default,
        alias = "bridge_interfaces",
        deserialize_with = "lenient::list"
    )]
    bond_interfaces: Vec<String>,
    #[serde(default)]
    params: BTreeMap<String, Value>,
    #[serde(default)]
    subnets: Vec<Subnet>,
    #[serde(flatten)]
    rest: Rest,
}

#[derive(Debug, Deserialize)]
struct Vlan {
    name: Option<String>,
    mac_address: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    mtu: Option<u32>,
    #[serde(default, rename = "accept-ra", deserialize_with = "lenient::opt_bool")]
    accept_ra: Option<bool>,
    vlan_link: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    vlan_id: Option<u32>,
    #[serde(default)]
    subnets: Vec<Subnet>,
    #[serde(flatten)]
    rest: Rest,
}

#[derive(Debug, Deserialize)]
struct Subnet {
    #[serde(rename = "type")]
    kind: String,
    #[serde(
        default,
        alias = "ip_address",
        deserialize_with = "lenient::opt_string"
    )]
    address: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    netmask: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    prefix: Option<String>,
    gateway: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    metric: Option<u32>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    mtu: Option<u32>,
    #[serde(default, deserialize_with = "lenient::list")]
    dns_nameservers: Vec<String>,
    #[serde(default, deserialize_with = "lenient::list")]
    dns_search: Vec<String>,
    #[serde(default)]
    routes: Vec<SubnetRoute>,
    #[serde(flatten)]
    rest: Rest,
}

#[derive(Debug, Deserialize)]
struct SubnetRoute {
    #[serde(alias = "destination")]
    network: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    netmask: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_string")]
    prefix: Option<String>,
    gateway: Option<String>,
    #[serde(default, deserialize_with = "lenient::opt_u32")]
    metric: Option<u32>,
    #[serde(flatten)]
    rest: Rest,
}

#[derive(Debug, Deserialize)]
struct Nameserver {
    #[serde(default, deserialize_with = "lenient::list")]
    address: Vec<String>,
    #[serde(default, deserialize_with = "lenient::list")]
    search: Vec<String>,
    interface: Option<String>,
    #[serde(flatten)]
    rest: Rest,
}

pub fn convert(value: Value, warnings: &mut Vec<String>) -> Result<Vec<Network>> {
    let config: Config = yaml_serde::from_value(value).context("invalid version 1 config")?;
    warn_unknown(config.rest, "the config", warnings);

    let mut networks = Vec::new();
    let mut ports = Vec::new();
    let mut nameservers = Vec::new();
    let mut routes = Vec::new();

    for item in config.config {
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        match kind.as_str() {
            "physical" => {
                let physical = yaml_serde::from_value(item).context("invalid physical config")?;
                networks.push(physical_network(physical, warnings)?);
            }
            "bond" | "bridge" => {
                let controller: Controller = yaml_serde::from_value(item)
                    .with_context(|| format!("invalid {kind} config"))?;
                let Some(id) = controller.name.clone() else {
                    bail!("a {kind} config has no name");
                };
                warn_unknown(controller.rest, &id, warnings);
                let (network, bridge) =
                    controller_network(&kind, id.clone(), controller.params, warnings)?;
                let network = with_subnets(
                    Network {
                        set_mac_address: controller.mac_address,
                        mtu: controller.mtu,
                        accept_ra: controller.accept_ra,
                        ..network
                    },
                    controller.subnets,
                    warnings,
                )?;
                ports.push((id, controller.bond_interfaces, bridge));
                networks.push(network);
            }
            "vlan" => {
                let mut vlan: Vlan = yaml_serde::from_value(item).context("invalid vlan config")?;
                let rest = std::mem::take(&mut vlan.rest);
                let (Some(id), Some(link), Some(vlan_id)) =
                    (vlan.name, vlan.vlan_link, vlan.vlan_id)
                else {
                    bail!("a vlan config needs name, vlan_link and vlan_id");
                };
                warn_unknown(rest, &id, warnings);
                let network = Network {
                    id,
                    kind: Kind::Vlan {
                        id: params::vlan_id(vlan_id)?,
                        link,
                    },
                    set_mac_address: vlan.mac_address,
                    mtu: vlan.mtu,
                    accept_ra: vlan.accept_ra,
                    ..Default::default()
                };
                networks.push(with_subnets(network, vlan.subnets, warnings)?);
            }
            "nameserver" => {
                let mut nameserver: Nameserver =
                    yaml_serde::from_value(item).context("invalid nameserver config")?;
                warn_unknown(std::mem::take(&mut nameserver.rest), "a nameserver", warnings);
                nameservers.push(nameserver);
            }
            // cloud-init brings up no loopback for netplan.
            "loopback" => {}
            "route" => {
                let mut route: SubnetRoute =
                    yaml_serde::from_value(item).context("invalid route config")?;
                warn_unknown(std::mem::take(&mut route.rest), "a route", warnings);
                routes.push(route);
            }
            _ => warnings.push(format!("skipping the unsupported type: {kind}")),
        }
    }

    for (controller, interfaces, bridge) in ports {
        attach_ports(&mut networks, &controller, &interfaces, bridge.as_ref())?;
    }

    // The name servers of every interface go first, to those without any of their own, then
    // those of one interface are added to it.
    let (global, scoped): (Vec<_>, Vec<_>) = nameservers
        .into_iter()
        .partition(|nameserver| nameserver.interface.is_none());
    add_global_nameservers(&mut networks, global)?;
    for nameserver in scoped {
        add_nameserver(&mut networks, nameserver)?;
    }

    for route in routes {
        add_global_route(&mut networks, route, warnings)?;
    }

    Ok(networks)
}

fn physical_network(physical: Physical, warnings: &mut Vec<String>) -> Result<Network> {
    let Some(id) = physical
        .name
        .clone()
        .or_else(|| physical.mac_address.clone())
    else {
        bail!("a physical config has neither name nor mac_address");
    };
    warn_unknown(physical.rest, &id, warnings);

    // With a MAC address, the name is the one to rename the interface to, as cloud-init does;
    // without one, it is the name the interface already has.
    let (mac_address, name, set_name) = match physical.mac_address {
        Some(mac) => (Some(mac), None, physical.name),
        None => (None, physical.name, None),
    };

    let network = Network {
        id,
        mac_address,
        name,
        set_name,
        mtu: physical.mtu,
        accept_ra: physical.accept_ra,
        ..Default::default()
    };

    with_subnets(network, physical.subnets, warnings)
}

/// A bond or a bridge, with the settings of the ports of a bridge.
fn controller_network(
    kind: &str,
    id: String,
    parameters: BTreeMap<String, Value>,
    warnings: &mut Vec<String>,
) -> Result<(Network, Option<params::BridgeParams>)> {
    if kind == "bond" {
        let keys: Vec<_> = parameters
            .keys()
            .map(|key| params::v1_bond_key(key))
            .collect();
        let pairs: Vec<_> = keys
            .iter()
            .map(String::as_str)
            .zip(parameters.values())
            .collect();
        let bond = params::bond(&id, &pairs, warnings)?;
        let network = Network {
            id,
            kind: Kind::Bond(bond),
            ..Default::default()
        };
        Ok((network, None))
    } else {
        let pairs: Vec<_> = parameters
            .iter()
            .map(|(key, value)| (params::v1_bridge_key(key), value))
            .collect();
        let mut bridge = params::bridge(&id, &pairs, warnings)?;
        // cloud-init gives netplan the parameters of a bridge that has any, which turn STP on.
        if !parameters.is_empty() {
            bridge.bridge.stp.get_or_insert(true);
        }
        let network = Network {
            id,
            kind: Kind::Bridge(bridge.bridge.clone()),
            ..Default::default()
        };
        Ok((network, Some(bridge)))
    }
}

fn with_subnets(
    mut network: Network,
    subnets: Vec<Subnet>,
    warnings: &mut Vec<String>,
) -> Result<Network> {
    let mut metrics = Metrics::default();
    for subnet in subnets {
        add_subnet(&mut network, subnet, &mut metrics, warnings)?;
    }

    // The MTU of an IPv4 subnet wins over the one of the interface, as cloud-init has it.
    if let Some(mtu) = metrics.mtu {
        if network.mtu.is_some_and(|device| device != mtu) {
            warnings.push(format!(
                "skipping the mtu of {}, since its IPv4 subnet has another",
                network.id
            ));
        }
        network.mtu = Some(mtu);
    }

    // netplan takes no different overrides for DHCPv4 and DHCPv6.
    if network.dhcp4 && network.dhcp6 && metrics.dhcp4 != metrics.dhcp6 {
        bail!("the DHCPv4 and DHCPv6 subnets of {} have different metrics", network.id);
    }
    network.dhcp_overrides = DhcpOverrides {
        route_metric: metrics.dhcp4.or(metrics.dhcp6),
        ..Default::default()
    };

    Ok(network)
}

/// What the subnets of an interface say for all of it.
#[derive(Default)]
struct Metrics {
    dhcp4: Option<u32>,
    dhcp6: Option<u32>,
    /// The MTU of an IPv4 subnet.
    mtu: Option<u32>,
}

fn add_subnet(
    network: &mut Network,
    mut subnet: Subnet,
    metrics: &mut Metrics,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let what = format!("a {} subnet of {}", subnet.kind, network.id);
    warn_unknown(subnet.rest, &what, warnings);
    // cloud-init drops what is not true, as a metric of 0.
    subnet.metric = subnet.metric.filter(|&metric| metric != 0);
    let kind = subnet.kind.as_str();
    match kind {
        "dhcp" | "dhcp4" => {
            network.dhcp4 = true;
            metrics.dhcp4 = subnet.metric.or(metrics.dhcp4);
        }
        "dhcp6" => {
            network.dhcp6 = true;
            metrics.dhcp6 = subnet.metric.or(metrics.dhcp6);
        }
        kind if IPV6_DYNAMIC.contains(&kind) => {
            network.dhcp6 = true;
            metrics.dhcp6 = subnet.metric.or(metrics.dhcp6);
        }
        "static" | "static6" => {
            let Some(address) = &subnet.address else {
                bail!("a static subnet of {} has no address", network.id);
            };
            let netmask = subnet.prefix.as_deref().or(subnet.netmask.as_deref());
            let address = Cidr::parse(address, netmask)?;
            push_unique(&mut network.addresses, address);
            if let Some(gateway) = &subnet.gateway {
                let gateway = parse_addr(gateway)?;
                // A gateway out of the subnet is reached on the link.
                let route = Route {
                    on_link: !address.contains(gateway),
                    metric: subnet.metric,
                    ..Route::default_via(gateway)
                };
                push_unique(&mut network.routes, route);
            }
            if let Some(mtu) = subnet.mtu {
                if address.addr.is_ipv6() {
                    network.ipv6_mtu = Some(mtu);
                } else {
                    metrics.mtu = Some(mtu);
                }
            }
            for nameserver in &subnet.dns_nameservers {
                push_unique(&mut network.nameservers, parse_addr(nameserver)?);
            }
            for search in subnet.dns_search {
                push_unique(&mut network.search, search);
            }
            for mut route in subnet.routes {
                let what = format!("a route of {}", network.id);
                warn_unknown(std::mem::take(&mut route.rest), &what, warnings);
                let mut route = route_of(&route)?;
                route.metric = route.metric.or(subnet.metric);
                push_unique(&mut network.routes, route);
            }
            return Ok(());
        }
        "manual" => {}
        kind => bail!("unsupported subnet type of {}: {kind}", network.id),
    }

    // cloud-init reads these of a static subnet alone.
    let static_only = [
        ("address", subnet.address.is_some()),
        ("gateway", subnet.gateway.is_some()),
        ("mtu", subnet.mtu.is_some()),
        ("dns_nameservers", !subnet.dns_nameservers.is_empty()),
        ("dns_search", !subnet.dns_search.is_empty()),
        ("routes", !subnet.routes.is_empty()),
    ];
    for (key, set) in static_only {
        if set {
            warnings.push(format!("skipping {key} of {what}, which only a static one takes"));
        }
    }
    if kind == "manual" && subnet.metric.is_some() {
        warnings.push(format!("skipping metric of {what}, which only a static one takes"));
    }

    Ok(())
}

fn route_of(route: &SubnetRoute) -> Result<Route> {
    let gateway = route.gateway.as_deref().map(parse_addr).transpose()?;
    // `default`, as cloud-init normalizes it, takes the family of the gateway.
    let destination = match route.network.as_deref().map(str::trim) {
        Some("default") | None => match gateway {
            Some(gateway) => Cidr::default_route(gateway.is_ipv6()),
            None => bail!("a default route has no gateway, which would tell its family"),
        },
        Some(network) => {
            let netmask = route.prefix.as_deref().or(route.netmask.as_deref());
            Cidr::parse(network, netmask)?
        }
    };

    Ok(Route {
        metric: route.metric,
        ..Route::to(destination, gateway)
    })
}

/// The name servers of every interface, which cloud-init gives each interface with static
/// addresses and no name servers or search domains of its own.
fn add_global_nameservers(networks: &mut [Network], nameservers: Vec<Nameserver>) -> Result<()> {
    let mut addresses = Vec::new();
    let mut search = Vec::new();
    for nameserver in nameservers {
        for address in &nameserver.address {
            push_unique(&mut addresses, parse_addr(address)?);
        }
        for domain in nameserver.search {
            push_unique(&mut search, domain);
        }
    }

    for network in networks.iter_mut() {
        if network.addresses.is_empty()
            || !network.nameservers.is_empty()
            || !network.search.is_empty()
        {
            continue;
        }
        network.nameservers.clone_from(&addresses);
        network.search.clone_from(&search);
    }

    Ok(())
}

/// The name servers of one interface, added to its own.
fn add_nameserver(networks: &mut [Network], nameserver: Nameserver) -> Result<()> {
    let addresses = nameserver
        .address
        .iter()
        .map(|address| parse_addr(address))
        .collect::<Result<Vec<_>>>()?;

    for network in networks.iter_mut() {
        if nameserver.interface.as_deref() != Some(network.id.as_str()) {
            continue;
        }
        for address in &addresses {
            push_unique(&mut network.nameservers, *address);
        }
        for search in &nameserver.search {
            push_unique(&mut network.search, search.clone());
        }
    }

    Ok(())
}

/// A route of no interface goes to the interface whose network has its gateway.
fn add_global_route(
    networks: &mut [Network],
    route: SubnetRoute,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let route = route_of(&route)?;

    let Some(gateway) = route.gateway else {
        warnings.push("skipping a route without a gateway".to_owned());
        return Ok(());
    };

    let network = networks.iter_mut().find(|network| {
        network
            .addresses
            .iter()
            .any(|address| address.contains(gateway))
    });

    match network {
        Some(network) => push_unique(&mut network.routes, route),
        None => warnings.push(format!("skipping a route via {gateway}, on no interface")),
    }

    Ok(())
}
