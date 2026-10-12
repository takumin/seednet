//! What seednet knows of NetworkManager: where it keeps the connections it was told to change,
//! whether it brings up the network, and its config that seednet adds to or hides.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use seednet::network::HEADER;

use crate::output;

/// The profiles that NetworkManager keeps, as when one of seednet was changed in its GUI or with
/// nmcli and so moved from /run.
const ETC_DIR: &str = "etc/NetworkManager/system-connections";

/// Where the NetworkManager of Ubuntu keeps a profile instead, as `90-NM-<UUID>.yaml`, which
/// netplan renders to a keyfile in /run.
const NETPLAN_ETC_DIR: &str = "etc/netplan";

/// The units that run NetworkManager in an initrd: the one of dracut, and the one of
/// NetworkManager 1.54 and later.
const INITRD_UNITS: &[&str] = &[
    "usr/lib/systemd/system/nm-initrd.service",
    "usr/lib/systemd/system/NetworkManager-initrd.service",
];

/// Where systemd looks for the units that are enabled: by the admin, and by the vendor.
const UNIT_DIRS: &[&str] = &[
    "etc/systemd/system",
    "usr/lib/systemd/system",
    "lib/systemd/system",
];

/// The DHCP connection of nm-initrd-generator, for when nothing says how the network is.
const INITRD_DEFAULT_CONNECTION: &str =
    "run/NetworkManager/system-connections/default_connection.nmconnection";

const CONF: &str = "run/NetworkManager/conf.d/10-seednet.conf";

/// The config of NetworkManager on Ubuntu that leaves all but Wi-Fi and modems unmanaged, and the
/// file that hides it, as netplan writes it to have NetworkManager manage the rest.
const MANAGED_DEVICES: &str = "usr/lib/NetworkManager/conf.d/10-globally-managed-devices.conf";
const MANAGED_DEVICES_MASK: &str = "run/NetworkManager/conf.d/10-globally-managed-devices.conf";

/// Whether the initrd has NetworkManager, as dracut puts it there.
pub fn in_initrd(sysroot: &Path) -> bool {
    INITRD_UNITS.iter().any(|unit| sysroot.join(unit).exists())
}

/// Whether NetworkManager brings up the network of the rootfs: whether a target wants it, by the
/// admin or by the vendor, and it is not masked.
pub fn enabled(sysroot: &Path) -> bool {
    let unit = "NetworkManager.service";
    let masked = fs::read_link(sysroot.join(UNIT_DIRS[0]).join(unit))
        .is_ok_and(|target| target == Path::new("/dev/null"));
    !masked
        && UNIT_DIRS.iter().any(|dir| {
            fs::read_dir(sysroot.join(dir)).is_ok_and(|entries| {
                entries.flatten().any(|entry| {
                    entry.file_name().to_string_lossy().ends_with(".wants")
                        && entry.path().join(unit).exists()
                })
            })
        })
}

/// The UUIDs of the profiles that NetworkManager keeps in /etc, with the file of each: in
/// /etc/NetworkManager, or on Ubuntu in /etc/netplan.
pub fn etc_uuids(sysroot: &Path) -> HashMap<String, PathBuf> {
    let mut uuids = netplan_uuids(sysroot);
    let Ok(entries) = fs::read_dir(sysroot.join(ETC_DIR)) else {
        return uuids;
    };
    uuids.extend(
        entries
            .flatten()
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|entry| {
                let path = entry.path();
                let uuid = keyfile_uuid(&fs::read_to_string(&path).ok()?)?;
                Some((uuid, path))
            }),
    );
    uuids
}

/// The UUIDs of the profiles in /etc/netplan, by the names of their files.
fn netplan_uuids(sysroot: &Path) -> HashMap<String, PathBuf> {
    let Ok(entries) = fs::read_dir(sysroot.join(NETPLAN_ETC_DIR)) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let uuid = name
                .to_str()?
                .strip_prefix("90-NM-")?
                .strip_suffix(".yaml")?;
            Some((uuid.to_ascii_lowercase(), entry.path()))
        })
        .collect()
}

/// The UUID of a keyfile, in lower case, as in its `[connection]` group.
pub fn keyfile_uuid(text: &str) -> Option<String> {
    let mut in_connection = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_connection = line == "[connection]";
        } else if in_connection
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "uuid"
        {
            return Some(value.trim().to_ascii_lowercase());
        }
    }
    None
}

/// Keeps NetworkManager from making a connection of its own: for an interface that seednet did not
/// configure, or from what the initrd configured.
pub fn take_over(sysroot: &Path, backend: seednet::Backend) -> Result<()> {
    remove_file(&sysroot.join(INITRD_DEFAULT_CONNECTION))?;
    write_conf(sysroot)?;
    // Only for NetworkManager to bring up the network: with networkd, it would manage the
    // interfaces that networkd brings up.
    if backend == seednet::Backend::NetworkManager {
        manage_all_devices(sysroot)?;
    }
    Ok(())
}

fn write_conf(sysroot: &Path) -> Result<()> {
    let mut conf = format!(
        "{}
# No connection for an interface that seednet did not configure.
[main]
no-auto-default=*
",
        HEADER.trim_end()
    );
    // Over a rootfs on the network, the configuration of the initrd stays as it is, for
    // NetworkManager to take it over without taking the interface down.
    if !sysroot.join(output::NETWORK_ROOTFS).exists() {
        conf.push_str(
            "
# Bring up the connections of seednet on an interface that the initrd already configured, rather
# than take its configuration for an external connection and leave it as it is.
[device-seednet]
keep-configuration=no
",
        );
    }
    write(&sysroot.join(CONF), &conf)
}

/// Hides the config of Ubuntu that leaves the interfaces of seednet unmanaged, as netplan does with
/// the same name in /run, which one in /etc still overrides.
fn manage_all_devices(sysroot: &Path) -> Result<()> {
    if !sysroot.join(MANAGED_DEVICES).exists() {
        return Ok(());
    }
    write(&sysroot.join(MANAGED_DEVICES_MASK), HEADER)
}

fn write(path: &Path, content: &str) -> Result<()> {
    let dir = path.parent().context("a file without a directory")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_whether_network_manager_is_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let sysroot = dir.path();
        assert!(!enabled(sysroot));

        let vendor = sysroot.join("usr/lib/systemd/system/graphical.target.wants");
        fs::create_dir_all(&vendor).unwrap();
        fs::write(vendor.join("NetworkManager.service"), "").unwrap();
        assert!(enabled(sysroot));

        fs::create_dir_all(sysroot.join("etc/systemd/system")).unwrap();
        std::os::unix::fs::symlink(
            "/dev/null",
            sysroot.join("etc/systemd/system/NetworkManager.service"),
        )
        .unwrap();
        assert!(!enabled(sysroot));
    }

    #[test]
    fn keeps_the_configuration_of_the_initrd_under_a_network_rootfs() {
        let dir = tempfile::tempdir().unwrap();
        let sysroot = dir.path();
        write_conf(sysroot).unwrap();
        assert!(
            fs::read_to_string(sysroot.join(CONF))
                .unwrap()
                .contains("keep-configuration=no")
        );

        write(&sysroot.join(output::NETWORK_ROOTFS), "").unwrap();
        write_conf(sysroot).unwrap();
        let conf = fs::read_to_string(sysroot.join(CONF)).unwrap();
        assert!(conf.contains("no-auto-default=*"));
        assert!(!conf.contains("keep-configuration"));
    }

    #[test]
    fn keeps_the_uuids_of_both_places_in_etc() {
        let dir = tempfile::tempdir().unwrap();
        let sysroot = dir.path();
        fs::create_dir_all(sysroot.join(ETC_DIR)).unwrap();
        fs::write(
            sysroot.join(ETC_DIR).join("Wired.nmconnection"),
            "[connection]\nid=Wired\nuuid = AAAA\n",
        )
        .unwrap();
        fs::write(sysroot.join(ETC_DIR).join(".hidden"), "[connection]\nuuid=cccc\n").unwrap();
        fs::create_dir_all(sysroot.join(NETPLAN_ETC_DIR)).unwrap();
        fs::write(sysroot.join(NETPLAN_ETC_DIR).join("90-NM-BBBB.yaml"), "").unwrap();
        fs::write(sysroot.join(NETPLAN_ETC_DIR).join("50-cloud-init.yaml"), "").unwrap();

        let mut uuids: Vec<_> = etc_uuids(sysroot).into_keys().collect();
        uuids.sort();
        assert_eq!(uuids, ["aaaa", "bbbb"]);
    }

    #[test]
    fn the_uuid_is_the_one_of_the_connection_group() {
        assert_eq!(
            keyfile_uuid("[ethernet]\nuuid=no\n[connection]\nid=x\nuuid=AbC\n").as_deref(),
            Some("abc")
        );
        assert_eq!(keyfile_uuid("[connection]\nid=x\n"), None);
    }
}
