//! Keeping the other network renderers away, so that the network is the way seednet wrote it:
//! cloud-init leaves the network alone, the netplan generator does not run, and NetworkManager
//! makes no connection of its own, neither for an interface that seednet did not configure nor
//! from what the initrd configured.
//!
//! Everything goes in /run, for this boot only, and only when seednet found a config: a boot
//! without one leaves the others as they are.

use std::path::Path;

use anyhow::Result;

use crate::{Stage, cloud_init, netplan, networkmanager};

pub fn take_over(sysroot: &Path, stage: Stage, backend: seednet::Backend) -> Result<()> {
    netplan::take_over(sysroot, stage)?;
    networkmanager::take_over(sysroot, backend)?;
    // ds-identify rewrites the runtime config of cloud-init when the rootfs starts.
    if stage == Stage::Rootfs {
        cloud_init::take_over(sysroot)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn takes_over_in_run() {
        let dir = tempfile::tempdir().unwrap();
        let sysroot = dir.path();
        let connections = sysroot.join("run/NetworkManager/system-connections");
        fs::create_dir_all(&connections).unwrap();
        fs::write(connections.join("default_connection.nmconnection"), "").unwrap();
        fs::write(connections.join("mine.nmconnection"), "").unwrap();

        take_over(sysroot, Stage::Initrd, seednet::Backend::Networkd).unwrap();
        assert!(!connections.join("default_connection.nmconnection").exists());
        assert!(connections.join("mine.nmconnection").exists());
        assert!(
            fs::read_to_string(sysroot.join("run/NetworkManager/conf.d/10-seednet.conf"))
                .unwrap()
                .contains("no-auto-default=*")
        );
        // The initrd leaves cloud-init to the rootfs.
        assert!(!sysroot.join("run/cloud-init/cloud.cfg").exists());

        take_over(sysroot, Stage::Rootfs, seednet::Backend::Networkd).unwrap();
        assert!(sysroot.join("run/cloud-init/cloud.cfg").exists());
    }
}
