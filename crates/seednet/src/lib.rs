//! Converts the network-config of the cloud-init NoCloud datasource to systemd-networkd or
//! NetworkManager files, so that an initrd and the rootfs after it bring up the network the way the
//! config says.
//!
//! [`convert`] parses the config into [`Network`]s, and [`render()`] turns those into files.
//! Neither does any I/O.

pub mod ip;
mod lenient;
pub mod network;
mod networkd;
pub mod nm;
mod params;
pub mod render;
mod v1;
mod v2;

use anyhow::{Context, Result, bail};
use yaml_serde::Value;

pub use network::Network;
pub use render::{Backend, Dir, File, render};

/// The result of converting a network-config.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Conversion {
    pub networks: Vec<Network>,
    /// What the config has but the conversion skipped, for the caller to report.
    pub warnings: Vec<String>,
}

/// Parses a network-config and converts it. An empty or disabled config converts to no networks.
pub fn convert(text: &str) -> Result<Conversion> {
    let document: Value = yaml_serde::from_str(text).context("invalid YAML")?;

    // The config may be nested under `network:`, as in the user data, or be the whole document.
    let config = match document {
        Value::Null => return Ok(Conversion::default()),
        Value::Mapping(ref mapping) => match mapping.get("network") {
            Some(network) => network.clone(),
            None => document,
        },
        _ => bail!("the network-config is not a mapping"),
    };

    if config.is_null() || config.get("config").and_then(Value::as_str) == Some("disabled") {
        return Ok(Conversion::default());
    }

    let version = match config.get("version") {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    };

    let mut warnings = Vec::new();
    let networks = match version {
        Some(1) => v1::convert(config, &mut warnings)?,
        Some(2) => v2::convert(config, &mut warnings)?,
        _ => bail!("unsupported network-config version: {:?}", config.get("version")),
    };
    network::check_references(&networks)?;
    for network in &networks {
        if let Some(name) = network.ifname() {
            network::check_ifname(name)
                .with_context(|| format!("invalid interface name of {}", network.id))?;
        }
    }
    if let Some(duplicate) = networks
        .iter()
        .enumerate()
        .find_map(|(i, n)| networks[..i].iter().any(|m| m.id == n.id).then_some(&n.id))
    {
        bail!("{duplicate} is in the config twice");
    }
    // The files are named by the ID, with only the characters that are safe in a file name.
    if let Some((first, second)) = networks.iter().enumerate().find_map(|(i, n)| {
        let file_id = n.file_id();
        networks[..i]
            .iter()
            .find(|m| m.file_id() == file_id)
            .map(|m| (&m.id, &n.id))
    }) {
        bail!("{first} and {second} make the same file name");
    }

    Ok(Conversion { networks, warnings })
}
