//! The names that DHCP gave the host, for the variables of `seednet.url=`.
//!
//! The kernel host name is no source: the initrd takes it from the /etc/hostname that dracut
//! copies from the system it was built on.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use seednet::Backend;
use seednet::template::Names;

/// Where systemd-networkd keeps the DHCPv4 leases, by the index of the interface.
const NETWORKD_LEASES: &str = "run/systemd/netif/leases";

/// Whether it was said that nmcli does not tell the leases, which is said once, not on every try.
static NMCLI_WARNED: AtomicBool = AtomicBool::new(false);

/// The names of the lease of the interface with the lowest index that has a host name, or none.
pub fn dhcp(sysroot: &Path, backend: Backend) -> Names {
    let mut leases = match backend {
        Backend::Networkd => networkd_leases(sysroot),
        // nmcli asks the NetworkManager that runs, which has nothing to do with another sysroot.
        Backend::NetworkManager if sysroot == Path::new("/") => nm_leases(sysroot),
        Backend::NetworkManager => Vec::new(),
    };
    leases.sort_by_key(|(index, _)| *index);
    leases
        .into_iter()
        .map(|(_, names)| names)
        .find(|names| names.hostname.is_some())
        .unwrap_or_default()
}

/// The leases of systemd-networkd, which has `HOSTNAME=` and `DOMAINNAME=` from DHCP in them.
fn networkd_leases(sysroot: &Path) -> Vec<(u32, Names)> {
    let Ok(entries) = fs::read_dir(sysroot.join(NETWORKD_LEASES)) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let index = entry.file_name().to_str()?.parse().ok()?;
            let lease = fs::read_to_string(entry.path()).ok()?;
            let value = |key: &str| {
                lease
                    .lines()
                    .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            };
            Some((index, Names::from_dhcp(value("HOSTNAME")?, value("DOMAINNAME"))))
        })
        .collect()
}

/// The leases of NetworkManager, which keeps them to itself but for nmcli: the initrd of dracut has
/// it.
fn nm_leases(sysroot: &Path) -> Vec<(u32, Names)> {
    let output = match run_nmcli("nmcli") {
        Ok(output) => output,
        // Without it, the fetch would wait for the names to its timeout without saying why.
        Err(error) => {
            if !NMCLI_WARNED.swap(true, Ordering::Relaxed) {
                crate::warn(&format!(
                    "cannot read the names that DHCP gave from NetworkManager, trying again: \
                    {error}"
                ));
            }
            return Vec::new();
        }
    };
    parse_nmcli(&output)
        .into_iter()
        .filter_map(|(device, names)| {
            let index =
                fs::read_to_string(sysroot.join("sys/class/net").join(&device).join("ifindex"))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()?;
            Some((index, names))
        })
        .collect()
}

/// What `nmcli -t -f GENERAL.DEVICE,DHCP4 device show` prints, or why it does not.
fn run_nmcli(program: &str) -> Result<String, String> {
    let output = Command::new(program)
        .args(["-t", "-f", "GENERAL.DEVICE,DHCP4", "device", "show"])
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program}: {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The names in what `nmcli -t -f GENERAL.DEVICE,DHCP4 device show` prints, by the device.
fn parse_nmcli(output: &str) -> Vec<(String, Names)> {
    let mut devices: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for line in output.lines() {
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        // The terse output escapes the colons and backslashes of the values.
        let value = value.replace("\\:", ":").replace("\\\\", "\\");
        if field == "GENERAL.DEVICE" {
            devices.push((value, None, None));
        } else if field.starts_with("DHCP4.OPTION")
            && let Some((_, hostname, domainname)) = devices.last_mut()
            && let Some((name, value)) = value.split_once(" = ")
        {
            match name {
                "host_name" => *hostname = Some(value.to_owned()),
                "domain_name" => *domainname = Some(value.to_owned()),
                _ => {}
            }
        }
    }
    devices
        .into_iter()
        .filter_map(|(device, hostname, domainname)| {
            Some((device, Names::from_dhcp(&hostname?, domainname.as_deref())))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_lease_of_the_lowest_index_with_a_host_name() {
        let dir = tempfile::tempdir().unwrap();
        let leases = dir.path().join(NETWORKD_LEASES);
        fs::create_dir_all(&leases).unwrap();
        fs::write(leases.join("2"), "ADDRESS=10.0.2.15\n").unwrap();
        fs::write(leases.join("10"), "HOSTNAME=other\n").unwrap();
        fs::write(leases.join("3"), "ADDRESS=10.0.2.15\nHOSTNAME=host\nDOMAINNAME=example.com\n")
            .unwrap();

        assert_eq!(
            dhcp(dir.path(), Backend::Networkd),
            Names {
                hostname: Some("host".to_owned()),
                domainname: Some("example.com".to_owned()),
            }
        );
        // A sysroot of NetworkManager has no NetworkManager to ask.
        assert_eq!(dhcp(dir.path(), Backend::NetworkManager), Names::default());
    }

    #[test]
    fn no_lease_has_no_names() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dhcp(dir.path(), Backend::Networkd), Names::default());
    }

    #[test]
    fn tells_why_nmcli_tells_nothing() {
        let error = run_nmcli("seednet-test-no-such-nmcli").unwrap_err();
        assert!(error.starts_with("seednet-test-no-such-nmcli: "), "{error}");
        let error = run_nmcli("false").unwrap_err();
        assert!(error.starts_with("false: exit status: 1"), "{error}");
    }

    #[test]
    fn parses_what_nmcli_prints() {
        let output = "GENERAL.DEVICE:lo\n\
            \n\
            GENERAL.DEVICE:ens3\n\
            DHCP4.OPTION[1]:broadcast_address = 10.0.2.255\n\
            DHCP4.OPTION[2]:domain_name = example.com\n\
            DHCP4.OPTION[3]:host_name = host\n\
            DHCP4.OPTION[4]:requested_host_name = 1\n\
            \n\
            GENERAL.DEVICE:ens4\n\
            DHCP4.OPTION[1]:host_name = a\\:b\n";

        assert_eq!(
            parse_nmcli(output),
            [
                (
                    "ens3".to_owned(),
                    Names {
                        hostname: Some("host".to_owned()),
                        domainname: Some("example.com".to_owned()),
                    }
                ),
                ("ens4".to_owned(), Names::from_dhcp("a:b", None)),
            ]
        );
    }
}
