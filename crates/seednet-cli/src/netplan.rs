//! Keeping netplan from rendering the network: its generator renders /etc/netplan, where cloud-init
//! writes the network-config too, as systemd starts and on each reload.

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::symlink;
use std::path::Path;

use anyhow::{Context, Result};

use crate::{Stage, networkmanager};

/// Masks the generator of the same name, which systemd runs at its start and again on each reload.
const MASK: &str = "run/systemd/system-generators/netplan";

/// What the generator writes, by directory and the start of the name.
const OUTPUTS: &[(&str, &str)] = &[
    ("run/systemd/network", "10-netplan-"),
    ("run/NetworkManager/system-connections", "netplan-"),
];

pub fn take_over(sysroot: &Path, stage: Stage) -> Result<()> {
    // The generator runs as the rootfs starts, before seednet, which removes what it wrote. It has
    // to, for the profiles that the NetworkManager of Ubuntu keeps in /etc/netplan, so only the
    // rootfs masks it, for when systemd reloads.
    if stage == Stage::Rootfs {
        mask(sysroot)?;
    }
    remove_outputs(sysroot)
}

fn mask(sysroot: &Path) -> Result<()> {
    let mask = sysroot.join(MASK);
    if fs::read_link(&mask).is_ok_and(|target| target == Path::new("/dev/null")) {
        return Ok(());
    }
    let dir = mask.parent().context("a mask without a directory")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    match fs::remove_file(&mask) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to remove {}", mask.display()));
        }
    }
    symlink("/dev/null", &mask).with_context(|| format!("failed to create {}", mask.display()))
}

/// Removes what the generator wrote in this boot before it was masked, but for the profiles that
/// NetworkManager keeps in /etc, which it renders for the NetworkManager of Ubuntu.
fn remove_outputs(sysroot: &Path) -> Result<()> {
    let kept = networkmanager::etc_uuids(sysroot);
    for (dir, prefix) in OUTPUTS {
        let dir = sysroot.join(dir);
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry?.path();
            let ours = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix));
            let kept = ours
                && fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| networkmanager::keyfile_uuid(&text))
                    .is_some_and(|uuid| kept.contains_key(&uuid));
            if ours && !kept {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_what_it_rendered_but_the_profiles_of_network_manager() {
        let dir = tempfile::tempdir().unwrap();
        let sysroot = dir.path();
        fs::create_dir_all(sysroot.join("run/systemd/network")).unwrap();
        fs::write(sysroot.join("run/systemd/network/10-netplan-eth0.network"), "").unwrap();
        fs::write(sysroot.join("run/systemd/network/20-mine.network"), "").unwrap();
        let connections = sysroot.join("run/NetworkManager/system-connections");
        fs::create_dir_all(&connections).unwrap();
        // A profile that the NetworkManager of Ubuntu keeps in /etc/netplan, and netplan rendered.
        let uuid = "d82e11ea-e007-5757-9a05-ab81b6026d2b";
        fs::create_dir_all(sysroot.join("etc/netplan")).unwrap();
        fs::write(sysroot.join(format!("etc/netplan/90-NM-{uuid}.yaml")), "").unwrap();
        fs::write(
            connections.join("netplan-br0.nmconnection"),
            format!("[connection]\nid=seednet-br0\nuuid={uuid}\n"),
        )
        .unwrap();
        fs::write(
            connections.join("netplan-eth0.nmconnection"),
            "[connection]\nid=eth0\nuuid=00000000-0000-0000-0000-000000000000\n",
        )
        .unwrap();

        // The initrd leaves the generator to run as the rootfs starts.
        take_over(sysroot, Stage::Initrd).unwrap();
        assert!(!sysroot.join(MASK).exists());
        assert!(
            !sysroot
                .join("run/systemd/network/10-netplan-eth0.network")
                .exists()
        );
        assert!(sysroot.join("run/systemd/network/20-mine.network").exists());
        assert!(connections.join("netplan-br0.nmconnection").exists());
        assert!(!connections.join("netplan-eth0.nmconnection").exists());

        take_over(sysroot, Stage::Rootfs).unwrap();
        assert_eq!(fs::read_link(sysroot.join(MASK)).unwrap(), Path::new("/dev/null"));
    }
}
