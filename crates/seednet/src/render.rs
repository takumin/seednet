//! The files for the network service of the system.

use crate::network::Network;
use crate::{networkd, nm};

/// The service that brings up the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Networkd,
    NetworkManager,
}

/// Where a file goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// `/run/systemd/network`, which udev also reads `.link` files from.
    Networkd,
    /// `/run/NetworkManager/system-connections`, whose files only root may read.
    NetworkManager,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub dir: Dir,
    pub name: String,
    pub content: String,
}

/// Renders the networks for `backend`, with what the backend cannot express in `warnings`.
/// Renaming an interface is for udev, so the `.link` files come with either.
pub fn render(
    networks: &[Network],
    backend: Backend,
    prefix: &str,
    warnings: &mut Vec<String>,
) -> Vec<File> {
    let mut files = networkd::links(networks, prefix, backend == Backend::Networkd);
    match backend {
        Backend::Networkd => files.extend(networkd::render(networks, prefix, warnings)),
        Backend::NetworkManager => files.extend(nm::render(networks, prefix, warnings)),
    }
    files
}
