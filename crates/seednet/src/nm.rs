//! The NetworkManager keyfiles.

use std::fmt::Write as _;
use std::net::IpAddr;

use sha1::Digest as _;

use crate::network::{DhcpOverrides, HEADER, Kind, Network, Route};
use crate::render::{Dir, File};

/// One keyfile per network.
pub fn render(networks: &[Network], prefix: &str, warnings: &mut Vec<String>) -> Vec<File> {
    networks
        .iter()
        .map(|network| File {
            dir: Dir::NetworkManager,
            name: format!("{prefix}-{}.nmconnection", network.file_id()),
            content: keyfile(network, networks, warnings),
        })
        .collect()
}

/// A UUID that stays the same for the same ID, so that a connection keeps its UUID across boots
/// and another connection can name it as its controller or parent.
pub fn uuid(id: &str) -> String {
    let hash = sha1::Sha1::digest(format!("seednet/{id}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    // A name-based UUID: version 5, RFC 4122 variant.
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn keyfile(network: &Network, networks: &[Network], warnings: &mut Vec<String>) -> String {
    let find = |id: &str| networks.iter().find(|other| other.id == id);
    let mut out = String::from(HEADER);

    let kind = match network.kind {
        Kind::Ethernet => "ethernet",
        Kind::Bond(_) => "bond",
        Kind::Bridge(_) => "bridge",
        Kind::Vlan { .. } => "vlan",
    };
    let _ = writeln!(
        out,
        "\n[connection]\nid=seednet-{}\nuuid={}\ntype={kind}",
        network.id,
        uuid(&network.id)
    );
    // A physical interface goes by its MAC address when it has one, so that it is found before
    // and after a rename. A name with wildcards goes in [match].
    let ifname = match network.kind {
        Kind::Ethernet if network.mac_address.is_some() => None,
        _ => network.ifname().filter(|_| network.set_name.is_none()),
    };
    if let Some(ifname) = ifname {
        let _ = writeln!(out, "interface-name={ifname}");
    }
    let controller = network.controller.as_deref().and_then(find);
    if let Some(controller) = controller {
        let port_type = match controller.kind {
            Kind::Bridge(_) => "bridge",
            _ => "bond",
        };
        let _ = writeln!(out, "master={}\nslave-type={port_type}", uuid(&controller.id));
    }

    let mut ethernet = String::new();
    if !network.is_virtual()
        && let Some(mac) = &network.mac_address
    {
        let _ = writeln!(ethernet, "mac-address={}", mac.to_ascii_uppercase());
    }
    if let Some(mac) = &network.set_mac_address {
        let _ = writeln!(ethernet, "cloned-mac-address={}", mac.to_ascii_uppercase());
    }
    if let Some(mtu) = network.mtu {
        let _ = writeln!(ethernet, "mtu={mtu}");
    }
    if !ethernet.is_empty() {
        let _ = write!(out, "\n[ethernet]\n{ethernet}");
    }

    let glob = network
        .name
        .as_ref()
        .filter(|_| !network.is_virtual() && ifname.is_none());
    if glob.is_some() || network.driver.is_some() {
        out.push_str("\n[match]\n");
        if let Some(name) = glob {
            // Found by its name, the interface is found after the rename too.
            let _ = write!(out, "interface-name={name};");
            if let Some(set_name) = &network.set_name {
                let _ = write!(out, "{set_name};");
            }
            out.push('\n');
        }
        if let Some(driver) = &network.driver {
            let _ = writeln!(out, "driver={driver}");
        }
    }

    match &network.kind {
        Kind::Ethernet => {}
        Kind::Bond(bond) => {
            out.push_str("\n[bond]\n");
            let mut set = |key: &str, value: Option<String>| {
                if let Some(value) = value {
                    let _ = writeln!(out, "{key}={value}");
                }
            };
            set("mode", Some(bond.mode.clone().unwrap_or_else(|| "balance-rr".to_owned())));
            set("lacp_rate", bond.lacp_rate.clone());
            set("miimon", bond.mii_monitor_interval.map(|ms| ms.to_string()));
            set("updelay", bond.up_delay.map(|ms| ms.to_string()));
            set("downdelay", bond.down_delay.map(|ms| ms.to_string()));
            set("xmit_hash_policy", bond.transmit_hash_policy.clone());
            set("ad_select", bond.ad_select.clone());
            set("arp_interval", bond.arp_interval.map(|ms| ms.to_string()));
            let targets: Vec<_> = bond
                .arp_ip_targets
                .iter()
                .map(ToString::to_string)
                .collect();
            set("arp_ip_target", (!targets.is_empty()).then(|| targets.join(",")));
            set("fail_over_mac", bond.fail_over_mac_policy.clone());
            set("min_links", bond.min_links.map(|n| n.to_string()));
            if let Some(primary) = &bond.primary {
                match find(primary).and_then(Network::ifname) {
                    Some(ifname) => set("primary", Some(ifname.to_owned())),
                    None => warnings.push(format!(
                        "skipping the primary {primary} of {}, since NetworkManager needs its \
                        interface name",
                        network.id
                    )),
                }
            }
        }
        Kind::Bridge(bridge) => {
            out.push_str("\n[bridge]\n");
            let mut set = |key: &str, value: Option<String>| {
                if let Some(value) = value {
                    let _ = writeln!(out, "{key}={value}");
                }
            };
            set("stp", bridge.stp.map(|stp| stp.to_string()));
            set("forward-delay", bridge.forward_delay.map(|s| s.to_string()));
            set("hello-time", bridge.hello_time.map(|s| s.to_string()));
            set("max-age", bridge.max_age.map(|s| s.to_string()));
            set("ageing-time", bridge.ageing_time.map(|s| s.to_string()));
            set("priority", bridge.priority.map(|p| p.to_string()));
        }
        Kind::Vlan { id, link } => {
            let _ = writeln!(out, "\n[vlan]\nid={id}\nparent={}", uuid(link));
        }
    }

    if network.port_cost.is_some() || network.port_priority.is_some() {
        out.push_str("\n[bridge-port]\n");
        if let Some(cost) = network.port_cost {
            let _ = writeln!(out, "path-cost={cost}");
        }
        if let Some(priority) = network.port_priority {
            let _ = writeln!(out, "priority={priority}");
        }
    }

    // A port has no addresses of its own.
    if controller.is_none() {
        write_ip(&mut out, network, false);
        write_ip(&mut out, network, true);
    }
    warn_unsupported(network, controller.is_none(), warnings);

    out
}

/// The [ipv4] or [ipv6] section.
fn write_ip(out: &mut String, network: &Network, ipv6: bool) {
    let family = |ip: &IpAddr| ip.is_ipv6() == ipv6;
    let addresses: Vec<_> = network
        .addresses
        .iter()
        .filter(|address| family(&address.addr))
        .collect();
    let link_local = network
        .link_local
        .as_deref()
        .is_none_or(|families| families == "yes" || families == if ipv6 { "ipv6" } else { "ipv4" });

    let method = if ipv6 {
        let ra = network.accept_ra != Some(false);
        if network.dhcp6 && ra || network.accept_ra == Some(true) {
            "auto"
        } else if network.dhcp6 {
            "dhcp"
        } else if !addresses.is_empty() {
            "manual"
        } else if link_local {
            "link-local"
        } else {
            "disabled"
        }
    } else if network.dhcp4 {
        "auto"
    } else if !addresses.is_empty() {
        "manual"
    } else if network.link_local.is_some() && link_local {
        "link-local"
    } else {
        "disabled"
    };

    let _ = writeln!(out, "\n[{}]\nmethod={method}", if ipv6 { "ipv6" } else { "ipv4" });
    // An IPv4 link-local address along with the others.
    if !ipv6
        && matches!(method, "auto" | "manual")
        && matches!(network.link_local.as_deref(), Some("yes" | "ipv4"))
    {
        out.push_str("link-local=3\n");
    }
    for (i, address) in addresses.iter().enumerate() {
        let _ = writeln!(out, "address{}={address}", i + 1);
    }

    let mut gateways = network.gateways.iter().filter(|gateway| family(gateway));
    if let Some(gateway) = gateways.next() {
        let _ = writeln!(out, "gateway={gateway}");
    }
    // NetworkManager takes one gateway; the others become default routes.
    let extra_defaults: Vec<_> = gateways
        .map(|gateway| Route::default_via(*gateway))
        .collect();

    let nameservers: Vec<_> = network
        .nameservers
        .iter()
        .filter(|ns| family(ns))
        .map(ToString::to_string)
        .collect();
    if !nameservers.is_empty() {
        let _ = writeln!(out, "dns={};", nameservers.join(";"));
    }
    // The search domains go with IPv4 unless only IPv6 has name servers or addresses.
    let search_here = if ipv6 {
        !network.nameservers.iter().any(|ns| ns.is_ipv4()) && method != "disabled"
    } else {
        network.nameservers.iter().any(IpAddr::is_ipv4) || method != "disabled"
    };
    if search_here && !network.search.is_empty() {
        let _ = writeln!(out, "dns-search={};", network.search.join(";"));
    }

    let routes = network
        .routes
        .iter()
        .chain(&extra_defaults)
        .filter(|route| route.destination.addr.is_ipv6() == ipv6);
    for (i, route) in routes.enumerate() {
        let n = i + 1;
        let mut line = route.destination.to_string();
        if let Some(gateway) = route.gateway {
            let _ = write!(line, ",{gateway}");
        }
        if let Some(metric) = route.metric {
            if route.gateway.is_none() {
                line.push(',');
            }
            let _ = write!(line, ",{metric}");
        }
        let _ = writeln!(out, "route{n}={line}");

        let mut options = Vec::new();
        if let Some(table) = route.table {
            options.push(format!("table={table}"));
        }
        if route.on_link {
            options.push("onlink=true".to_owned());
        }
        if !options.is_empty() {
            let _ = writeln!(out, "route{n}_options={}", options.join(","));
        }
    }

    let rules = network
        .routing_policy
        .iter()
        .filter(|rule| rule.is_ipv6() == ipv6);
    // NetworkManager needs the priority to know a rule.
    let rules = rules.filter_map(|rule| Some((rule.priority?, rule)));
    for (i, (priority, rule)) in rules.enumerate() {
        let mut line = format!("priority {priority}");
        if let Some(from) = &rule.from {
            let _ = write!(line, " from {from}");
        }
        if let Some(to) = &rule.to {
            let _ = write!(line, " to {to}");
        }
        if let Some(tos) = rule.type_of_service {
            let _ = write!(line, " tos 0x{tos:02x}");
        }
        if let Some(mark) = rule.mark {
            let _ = write!(line, " fwmark {mark}");
        }
        if let Some(table) = rule.table {
            let _ = write!(line, " table {table}");
        }
        let _ = writeln!(out, "routing-rule{}={line}", i + 1);
    }

    if ipv6 {
        if let Some(privacy) = network.ipv6_privacy {
            // 2 prefers the temporary addresses, as networkd's yes does.
            let _ = writeln!(out, "ip6-privacy={}", if privacy { 2 } else { 0 });
        }
        if let Some(mtu) = network.ipv6_mtu {
            let _ = writeln!(out, "mtu={mtu}");
        }
    }

    if !ipv6 && network.dhcp4 {
        let identifier = network.dhcp_identifier.as_deref().unwrap_or("mac");
        let _ = writeln!(out, "dhcp-client-id={identifier}");
    }
    if network.dhcp4 || network.dhcp6 {
        write_overrides(out, network, ipv6, method);
    }
}

/// The DHCP overrides, meaning what they do in netplan with networkd (see [`DhcpOverrides`]).
fn write_overrides(out: &mut String, network: &Network, ipv6: bool, method: &str) {
    let overrides = &network.dhcp_overrides;
    // The family whose DHCP the overrides are for here: DHCPv4, else DHCPv6.
    let dhcp = if ipv6 { network.dhcp6 } else { network.dhcp4 };

    // The metric of the routes of DHCPv4, and of the router advertisements as with networkd.
    if if ipv6 { method == "auto" } else { dhcp } {
        let metric = overrides
            .route_metric
            .unwrap_or(DhcpOverrides::DEFAULT_ROUTE_METRIC);
        let _ = writeln!(out, "route-metric={metric}");
    }
    if !dhcp {
        return;
    }
    // With DHCPv6, ignore-auto-dns would drop the name servers of the router advertisements too.
    if !ipv6 && overrides.use_dns == Some(false) {
        out.push_str("ignore-auto-dns=true\n");
    }
    if !ipv6 && overrides.use_routes == Some(false) {
        out.push_str("ignore-auto-routes=true\nnever-default=true\n");
    }
    if let Some(send) = overrides.send_hostname {
        let _ = writeln!(out, "dhcp-send-hostname={send}");
    }
    if !ipv6 && let Some(hostname) = &overrides.hostname {
        let _ = writeln!(out, "dhcp-hostname={hostname}");
    }
}

/// Warns of what the config says that NetworkManager has no setting for, where it does not do the
/// same on its own.
fn warn_unsupported(network: &Network, has_addresses: bool, warnings: &mut Vec<String>) {
    let id = &network.id;
    let mut skip = |what: String| {
        warnings.push(format!("skipping {what} of {id}, which NetworkManager does not take"));
    };
    if network.optional {
        skip("optional".to_owned());
    }
    if !has_addresses {
        return;
    }
    for options in &network.address_options {
        skip(format!("the options of the address {}", options.address));
    }
    if network
        .routing_policy
        .iter()
        .any(|rule| rule.priority.is_none())
    {
        skip("the routing policy rules without priority".to_owned());
    }
    // With IPv6 on, NetworkManager always gives the interface its IPv6 link-local address.
    let ipv6_on = network.dhcp6
        || network.accept_ra == Some(true)
        || network
            .addresses
            .iter()
            .any(|address| address.addr.is_ipv6());
    if ipv6_on && matches!(network.link_local.as_deref(), Some("no" | "ipv4")) {
        skip("link-local without ipv6".to_owned());
    }
    let overrides = &network.dhcp_overrides;
    let unsupported = [
        ("use-ntp", overrides.use_ntp == Some(true)),
        ("use-hostname", overrides.use_hostname == Some(false)),
        ("use-mtu", overrides.use_mtu == Some(false)),
        ("use-domains", overrides.use_domains.as_deref().is_some_and(|d| d != "yes")),
        ("use-dns with dhcp6", network.dhcp6 && overrides.use_dns == Some(false)),
    ];
    for (key, set) in unsupported {
        if set {
            skip(format!("{key} of the DHCP overrides"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_stable_and_name_based() {
        let a = uuid("bond0");
        assert_eq!(a, uuid("bond0"));
        assert_ne!(a, uuid("bond1"));
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "5");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
    }
}
