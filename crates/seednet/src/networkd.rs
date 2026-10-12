//! The systemd-networkd `.network` and `.netdev` files, and the udev `.link` files that rename
//! interfaces.

use std::fmt::Write as _;

use crate::network::{DhcpOverrides, HEADER, Kind, MAC_POLICIES, Network, Route, RoutingRule};
use crate::render::{Dir, File};

/// The `.netdev` and `.network` files of every network.
///
/// The files are named by the ID alone, as netplan names its own, so that a file keeps its name
/// when the config changes and a drop-in or an override in /etc keeps applying to it. Where the
/// matches of two files overlap, the first by ID wins, as with netplan.
pub fn render(networks: &[Network], prefix: &str, warnings: &mut Vec<String>) -> Vec<File> {
    let mut files = Vec::new();
    for network in networks {
        if let Some(policy) = network
            .set_mac_address
            .as_ref()
            .filter(|mac| MAC_POLICIES.contains(&mac.as_str()))
        {
            warnings.push(format!(
                "skipping the macaddress {policy} of {}, which networkd does not take",
                network.id
            ));
        }
        let base = format!("{prefix}-{}", network.file_id());
        if let Some(netdev) = netdev(network) {
            files.push(file(format!("{base}.netdev"), netdev));
        }
        files.push(file(format!("{base}.network"), network_file(network, networks)));
    }
    files
}

/// The `.link` files of the interfaces to rename.
pub fn links(networks: &[Network], prefix: &str) -> Vec<File> {
    networks
        .iter()
        .filter_map(|network| {
            let name = network.set_name.as_ref()?;
            let mut out = String::from(HEADER);
            out.push_str("\n[Match]\n");
            write_match(&mut out, network, "OriginalName");
            let _ = writeln!(out, "\n[Link]\nName={name}");
            Some(file(format!("{prefix}-{}.link", network.file_id()), out))
        })
        .collect()
}

fn file(name: String, content: String) -> File {
    File {
        dir: Dir::Networkd,
        name,
        content,
    }
}

/// The match keys of a physical interface. The permanent MAC address stays when a bond gives its
/// ports its own address, and the name to rename to is not matched, so the file applies before the
/// rename as well as after it.
fn write_match(out: &mut String, network: &Network, name_key: &str) {
    if let Some(mac) = &network.mac_address {
        let _ = writeln!(out, "PermanentMACAddress={}", mac.to_ascii_lowercase());
    }
    if let Some(name) = &network.name {
        let _ = write!(out, "{name_key}={name}");
        // The .network of an interface found by its name applies after the rename too.
        if name_key == "Name"
            && let Some(set_name) = &network.set_name
        {
            let _ = write!(out, " {set_name}");
        }
        out.push('\n');
    }
    if !network.drivers.is_empty() {
        let _ = writeln!(out, "Driver={}", network.drivers.join(" "));
    }
}

fn netdev(network: &Network) -> Option<String> {
    let kind = match &network.kind {
        Kind::Ethernet => return None,
        Kind::Bond(_) => "bond",
        Kind::Bridge(_) => "bridge",
        Kind::Vlan { .. } => "vlan",
    };

    let mut out = String::from(HEADER);
    let _ = writeln!(out, "\n[NetDev]\nName={}\nKind={kind}", network.id);
    if let Some(mac) = mac_address(network) {
        let _ = writeln!(out, "MACAddress={mac}");
    }

    match &network.kind {
        Kind::Ethernet => {}
        Kind::Bond(bond) => {
            let mut section = String::new();
            let mut set = |key: &str, value: Option<String>| {
                if let Some(value) = value {
                    let _ = writeln!(section, "{key}={value}");
                }
            };
            set("Mode", bond.mode.clone());
            set("LACPTransmitRate", bond.lacp_rate.clone());
            set("MIIMonitorSec", bond.mii_monitor_interval.map(|ms| format!("{ms}ms")));
            set("UpDelaySec", bond.up_delay.map(|ms| format!("{ms}ms")));
            set("DownDelaySec", bond.down_delay.map(|ms| format!("{ms}ms")));
            set("TransmitHashPolicy", bond.transmit_hash_policy.clone());
            set("AdSelect", bond.ad_select.clone());
            set("ARPIntervalSec", bond.arp_interval.map(|ms| format!("{ms}ms")));
            let targets: Vec<_> = bond
                .arp_ip_targets
                .iter()
                .map(ToString::to_string)
                .collect();
            set("ARPIPTargets", (!targets.is_empty()).then(|| targets.join(" ")));
            set("FailOverMACPolicy", bond.fail_over_mac_policy.clone());
            set("MinLinks", bond.min_links.map(|n| n.to_string()));
            if !section.is_empty() {
                let _ = write!(out, "\n[Bond]\n{section}");
            }
        }
        Kind::Bridge(bridge) => {
            let mut section = String::new();
            let mut set = |key: &str, value: Option<String>| {
                if let Some(value) = value {
                    let _ = writeln!(section, "{key}={value}");
                }
            };
            set("STP", bridge.stp.map(|stp| yes_no(stp).to_owned()));
            set("ForwardDelaySec", bridge.forward_delay.map(|s| s.to_string()));
            set("HelloTimeSec", bridge.hello_time.map(|s| s.to_string()));
            set("MaxAgeSec", bridge.max_age.map(|s| s.to_string()));
            set("AgeingTimeSec", bridge.ageing_time.map(|s| s.to_string()));
            set("Priority", bridge.priority.map(|p| p.to_string()));
            if !section.is_empty() {
                let _ = write!(out, "\n[Bridge]\n{section}");
            }
        }
        Kind::Vlan { id, .. } => {
            let _ = writeln!(out, "\n[VLAN]\nId={id}");
        }
    }

    Some(out)
}

fn network_file(network: &Network, networks: &[Network]) -> String {
    let mut out = String::from(HEADER);

    out.push_str("\n[Match]\n");
    if network.is_virtual() {
        let _ = writeln!(out, "Name={}", network.id);
    } else {
        write_match(&mut out, network, "Name");
    }

    let controller = network
        .controller
        .as_ref()
        .and_then(|id| networks.iter().find(|other| other.id == *id));
    // An interface that gets no address, such as one that only carries VLANs, never ends up
    // configured without a router advertisement, so it must not hold up the network being online.
    let optional = network.optional || (controller.is_none() && !network.is_configured());

    // A created interface gets its MAC address in its .netdev.
    let mac_address = mac_address(network).filter(|_| !network.is_virtual());
    if mac_address.is_some() || network.mtu.is_some() || optional {
        out.push_str("\n[Link]\n");
        if let Some(mac) = mac_address {
            let _ = writeln!(out, "MACAddress={mac}");
        }
        if let Some(mtu) = network.mtu {
            let _ = writeln!(out, "MTUBytes={mtu}");
        }
        if optional {
            out.push_str("RequiredForOnline=no\n");
        }
    }

    out.push_str("\n[Network]\n");
    match controller {
        // A port has no addresses of its own.
        Some(controller) => {
            let key = match controller.kind {
                Kind::Bridge(_) => "Bridge",
                _ => "Bond",
            };
            let _ = writeln!(out, "{key}={}", controller.id);
            out.push_str("LinkLocalAddressing=no\n");
            if let Kind::Bond(bond) = &controller.kind
                && bond.primary.as_ref() == Some(&network.id)
            {
                out.push_str("PrimarySlave=yes\n");
            }
        }
        None => write_addresses(&mut out, network),
    }
    for vlan in networks {
        if matches!(&vlan.kind, Kind::Vlan { link, .. } if *link == network.id) {
            let _ = writeln!(out, "VLAN={}", vlan.id);
        }
    }
    // A created interface may be up before its ports have a carrier, as netplan has it, even as a
    // port itself.
    if network.is_virtual() {
        out.push_str("ConfigureWithoutCarrier=yes\n");
    }

    if controller.is_none() {
        for options in &network.address_options {
            let _ = writeln!(out, "\n[Address]\nAddress={}", options.address);
            if let Some(label) = &options.label {
                let _ = writeln!(out, "Label={label}");
            }
            if let Some(lifetime) = &options.preferred_lifetime {
                let _ = writeln!(out, "PreferredLifetime={lifetime}");
            }
        }
        write_dhcp(&mut out, network);
    }

    if network.port_cost.is_some() || network.port_priority.is_some() {
        out.push_str("\n[Bridge]\n");
        if let Some(cost) = network.port_cost {
            let _ = writeln!(out, "Cost={cost}");
        }
        if let Some(priority) = network.port_priority {
            let _ = writeln!(out, "Priority={priority}");
        }
    }

    if controller.is_none() {
        for route in &network.routes {
            write_route(&mut out, route);
        }
        for rule in &network.routing_policy {
            write_rule(&mut out, rule);
        }
    }

    out
}

fn write_addresses(out: &mut String, network: &Network) {
    let dhcp = match (network.dhcp4, network.dhcp6) {
        (true, true) => "yes",
        (true, false) => "ipv4",
        (false, true) => "ipv6",
        (false, false) => "no",
    };
    let _ = writeln!(out, "DHCP={dhcp}");
    if let Some(link_local) = &network.link_local {
        let _ = writeln!(out, "LinkLocalAddressing={link_local}");
    }
    if let Some(accept_ra) = network.accept_ra {
        let _ = writeln!(out, "IPv6AcceptRA={}", yes_no(accept_ra));
    }
    if let Some(privacy) = network.ipv6_privacy {
        let _ = writeln!(out, "IPv6PrivacyExtensions={}", yes_no(privacy));
    }
    if let Some(mtu) = network.ipv6_mtu {
        let _ = writeln!(out, "IPv6MTUBytes={mtu}");
    }
    // An address with options gets an [Address] section instead.
    for address in &network.addresses {
        if !network
            .address_options
            .iter()
            .any(|o| o.address == *address)
        {
            let _ = writeln!(out, "Address={address}");
        }
    }
    for gateway in &network.gateways {
        let _ = writeln!(out, "Gateway={gateway}");
    }
    for nameserver in &network.nameservers {
        let _ = writeln!(out, "DNS={nameserver}");
    }
    if !network.search.is_empty() {
        let _ = writeln!(out, "Domains={}", network.search.join(" "));
    }
}

/// The [DHCPv4], [DHCPv6] and [IPv6AcceptRA] sections, which say what the `[DHCP]` section that
/// netplan writes says: the overrides, with the route metric and the MTU that netplan sets when the
/// config does not.
fn write_dhcp(out: &mut String, network: &Network) {
    if !network.dhcp4 && !network.dhcp6 {
        return;
    }
    let overrides = &network.dhcp_overrides;
    let route_metric = overrides
        .route_metric
        .unwrap_or(DhcpOverrides::DEFAULT_ROUTE_METRIC);
    let set = |section: &mut String, key: &str, value: Option<&str>| {
        if let Some(value) = value {
            let _ = writeln!(section, "{key}={value}");
        }
    };
    let opt = |value: Option<bool>| value.map(yes_no);
    // What netplan's [DHCP] gives both DHCPv4 and DHCPv6.
    let both = |section: &mut String| {
        set(section, "UseDNS", opt(overrides.use_dns));
        set(section, "UseNTP", opt(overrides.use_ntp));
        set(section, "UseDomains", overrides.use_domains.as_deref());
        set(section, "SendHostname", opt(overrides.send_hostname));
    };

    if network.dhcp4 {
        // Unless the config says, the same client identifier as the default network of dracut, so
        // that a DHCP server that pins the address to the MAC hands out the same one.
        let identifier = network.dhcp_identifier.as_deref().unwrap_or("mac");
        let _ = writeln!(out, "\n[DHCPv4]\nClientIdentifier={identifier}");
        let _ = writeln!(out, "RouteMetric={route_metric}");
        // netplan takes the MTU of DHCP unless told not to, as dhclient does.
        let _ = writeln!(out, "UseMTU={}", yes_no(overrides.use_mtu.unwrap_or(true)));
        set(out, "UseRoutes", opt(overrides.use_routes));
        set(out, "UseHostname", opt(overrides.use_hostname));
        set(out, "Hostname", overrides.hostname.as_deref());
        both(out);
    }

    if network.dhcp6 {
        let mut section = String::new();
        both(&mut section);
        if !section.is_empty() {
            let _ = write!(out, "\n[DHCPv6]\n{section}");
        }
    }

    // The route metric of netplan's [DHCP] is the one of the router advertisements too.
    let _ = writeln!(out, "\n[IPv6AcceptRA]\nRouteMetric={route_metric}");
}

fn write_rule(out: &mut String, rule: &RoutingRule) {
    out.push_str("\n[RoutingPolicyRule]\n");
    if let Some(from) = &rule.from {
        let _ = writeln!(out, "From={from}");
    }
    if let Some(to) = &rule.to {
        let _ = writeln!(out, "To={to}");
    }
    if let Some(table) = rule.table {
        let _ = writeln!(out, "Table={table}");
    }
    if let Some(priority) = rule.priority {
        let _ = writeln!(out, "Priority={priority}");
    }
    if let Some(mark) = rule.mark {
        let _ = writeln!(out, "FirewallMark={mark}");
    }
    if let Some(tos) = rule.type_of_service {
        let _ = writeln!(out, "TypeOfService={tos}");
    }
}

fn write_route(out: &mut String, route: &Route) {
    out.push_str("\n[Route]\n");
    let _ = writeln!(out, "Destination={}", route.destination);
    if let Some(gateway) = &route.gateway {
        let _ = writeln!(out, "Gateway={gateway}");
    }
    if route.on_link {
        out.push_str("GatewayOnLink=yes\n");
    }
    if let Some(metric) = route.metric {
        let _ = writeln!(out, "Metric={metric}");
    }
    if let Some(table) = route.table {
        let _ = writeln!(out, "Table={table}");
    }
}

/// The MAC address to give the interface, but a policy of NetworkManager.
fn mac_address(network: &Network) -> Option<String> {
    network
        .set_mac_address
        .as_ref()
        .filter(|mac| !MAC_POLICIES.contains(&mac.as_str()))
        .map(|mac| mac.to_ascii_lowercase())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
