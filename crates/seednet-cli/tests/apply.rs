//! Runs the seednet binary on a temporary sysroot.

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;

const CONFIG: &str = "version: 2\nethernets:\n  eth0:\n    dhcp4: true\n";
const OTHER_CONFIG: &str = "version: 2\nethernets:\n  eth1:\n    dhcp4: true\n";

struct Sysroot {
    dir: tempfile::TempDir,
}

impl Sysroot {
    fn new(cmdline: &str) -> Self {
        let sysroot = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        sysroot.write("proc/cmdline", &format!("{cmdline}\n"));
        fs::create_dir_all(sysroot.path("cidata")).unwrap();
        sysroot
    }

    fn path(&self, path: &str) -> PathBuf {
        self.dir.path().join(path)
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.path(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.path(path)).unwrap()
    }

    fn networks(&self) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(self.path("run/systemd/network"))
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn seednet(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_seednet"))
            .arg("--sysroot")
            .arg(self.dir.path())
            .args(args)
            .output()
            .unwrap()
    }

    fn apply(&self, stage: &str) -> Output {
        let cidata = self.path("cidata");
        self.seednet(&[
            "apply",
            "--stage",
            stage,
            "--cidata-dir",
            cidata.to_str().unwrap(),
        ])
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output) {
    assert!(output.status.success(), "{}", stderr(output));
}

/// Serves `body` over http to `requests` requests, and returns the URL.
fn serve(body: &'static str, requests: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/network-config", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming().take(requests) {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    url
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn initrd_uses_cidata_and_saves_it() {
    let sysroot = Sysroot::new("ro");
    sysroot.write("cidata/network-config", CONFIG);

    let output = sysroot.apply("initrd");
    assert_success(&output);
    assert_eq!(sysroot.networks(), ["10-seednet-eth0.network"]);
    assert_eq!(sysroot.read("run/seednet/network-config"), CONFIG);
    assert_eq!(sysroot.read("run/seednet/source"), "the CIDATA disk\n");
    assert_eq!(sysroot.read("run/seednet/backend"), "networkd\n");
    assert_eq!(mode(&sysroot.path("run/seednet/network-config")), 0o600);
    assert!(stderr(&output).contains("using the network-config from the CIDATA disk"));
}

#[test]
fn nothing_found_writes_nothing() {
    let sysroot = Sysroot::new("ro");

    let output = sysroot.apply("initrd");
    assert_success(&output);
    assert!(sysroot.networks().is_empty());
    assert!(!sysroot.path("run/seednet").exists());
}

#[test]
fn cmdline_config_wins_and_warns_about_what_it_overrides() {
    // base64 of OTHER_CONFIG
    let sysroot = Sysroot::new(
        "seednet.network-config=dmVyc2lvbjogMgpldGhlcm5ldHM6CiAgZXRoMToKICAgIGRoY3A0OiB0cnVlCg==",
    );
    sysroot.write("cidata/network-config", CONFIG);
    sysroot.write("etc/seednet/network-config", CONFIG);

    let output = sysroot.apply("rootfs");
    assert_success(&output);
    assert_eq!(sysroot.networks(), ["10-seednet-eth1.network"]);
    assert_eq!(sysroot.read("run/seednet/source"), "the kernel command line\n");
    assert!(stderr(&output).contains(
        "warning: seednet.network-config on the kernel command line overrides \
        /etc/seednet/network-config and the CIDATA disk"
    ));
}

#[test]
fn cmdline_config_alone_does_not_warn() {
    let sysroot = Sysroot::new(
        "seednet.network-config=dmVyc2lvbjogMgpldGhlcm5ldHM6CiAgZXRoMToKICAgIGRoY3A0OiB0cnVlCg==",
    );

    let output = sysroot.apply("initrd");
    assert_success(&output);
    assert!(!stderr(&output).contains("warning"));
}

#[test]
fn rootfs_prefers_etc_to_what_the_initrd_used() {
    let sysroot = Sysroot::new("ro");
    sysroot.write("run/seednet/network-config", OTHER_CONFIG);
    sysroot.write("run/seednet/source", "the CIDATA disk\n");
    sysroot.write("etc/seednet/network-config", CONFIG);

    assert_success(&sysroot.apply("rootfs"));
    assert_eq!(sysroot.networks(), ["10-seednet-eth0.network"]);
    assert_eq!(sysroot.read("run/seednet/network-config"), CONFIG);
    assert_eq!(sysroot.read("run/seednet/source"), "/etc/seednet/network-config\n");
}

#[test]
fn rootfs_reuses_what_the_initrd_used() {
    let sysroot = Sysroot::new("seednet.url=http://192.0.2.1/network-config");
    sysroot.write("run/seednet/network-config", OTHER_CONFIG);
    sysroot.write("run/seednet/source", "http://192.0.2.1/network-config\n");
    sysroot.write("cidata/network-config", CONFIG);

    // The rootfs does not fetch again: the URL is unreachable.
    assert_success(&sysroot.apply("rootfs"));
    assert_eq!(sysroot.networks(), ["10-seednet-eth1.network"]);
    assert_eq!(sysroot.read("run/seednet/source"), "http://192.0.2.1/network-config\n");
}

#[test]
fn network_root_keeps_the_network_of_the_initrd() {
    let sysroot = Sysroot::new("ro");
    sysroot.write("proc/self/mountinfo", "1 0 0:50 / / rw - nfs4 server:/export rw\n");
    sysroot.write("etc/seednet/network-config", CONFIG);

    // Without a config from the initrd, the rootfs leaves the network alone, CIDATA or not.
    sysroot.write("cidata/network-config", CONFIG);
    let output = sysroot.apply("rootfs");
    assert_success(&output);
    assert!(sysroot.networks().is_empty());
    assert!(stderr(&output).contains("ignores /etc/seednet/network-config"));

    sysroot.write("run/seednet/network-config", OTHER_CONFIG);
    assert_success(&sysroot.apply("rootfs"));
    assert_eq!(sysroot.networks(), ["10-seednet-eth1.network"]);
    assert!(sysroot.path("run/seednet/network-rootfs").exists());
}

/// The sha256 of OTHER_CONFIG.
const OTHER_SHA256: &str = "c751d7ae9e2b331c3e9054a150335f2088ef2d3d144cae5ae64dee984e4c531c";

#[test]
fn initrd_fetches_the_url_before_cidata() {
    let url = serve(OTHER_CONFIG, 1);
    let sysroot =
        Sysroot::new(&format!("seednet.url={url} seednet.checksum=sha256:{OTHER_SHA256}"));
    sysroot.write("cidata/network-config", CONFIG);

    let output = sysroot.apply("initrd");
    assert_success(&output);
    assert_eq!(sysroot.networks(), ["10-seednet-eth1.network"]);
    assert_eq!(sysroot.read("run/seednet/source"), format!("{url}\n"));
    assert!(!stderr(&output).contains("warning"));
}

#[test]
fn a_checksum_mismatch_fails() {
    let url = serve(CONFIG, 1);
    let sysroot =
        Sysroot::new(&format!("seednet.url={url} seednet.checksum=sha256:{OTHER_SHA256}"));

    let output = sysroot.apply("initrd");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("checksum mismatch"));
    assert!(sysroot.networks().is_empty());
}

#[test]
fn a_failed_fetch_fails() {
    // Nothing listens on the port of a listener that is gone.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let sysroot = Sysroot::new(&format!(
        "seednet.url=http://127.0.0.1:{port}/network-config seednet.url-timeout=1"
    ));
    sysroot.write("cidata/network-config", CONFIG);

    let output = sysroot.apply("initrd");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("failed to fetch"));
    assert!(sysroot.networks().is_empty());
}

#[test]
fn a_client_error_fails_at_once() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/network-config", listener.local_addr().unwrap());
    thread::spawn(move || {
        let mut stream = listener.incoming().next().unwrap().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap() > 2 {
            line.clear();
        }
        write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").unwrap();
    });
    let sysroot = Sysroot::new(&format!("seednet.url={url} seednet.url-timeout=60"));

    let started = std::time::Instant::now();
    let output = sysroot.apply("initrd");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("404"));
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn an_https_url_needs_a_ca_bundle() {
    let sysroot = Sysroot::new("seednet.url=https://192.0.2.1/network-config");

    let output = sysroot.apply("initrd");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("needs a CA bundle"));
}

#[test]
fn replaces_only_its_own_files() {
    let sysroot = Sysroot::new("ro");
    sysroot.write(
        "run/systemd/network/10-seednet-05-old.network",
        "# Generated by seednet from the NoCloud network-config.\n",
    );
    sysroot.write("run/systemd/network/10-seednet-mine.network", "[Match]\n");
    sysroot.write("run/systemd/network/20-other.network", "[Match]\n");
    sysroot.write("cidata/network-config", CONFIG);

    assert_success(&sysroot.apply("initrd"));
    assert_eq!(
        sysroot.networks(),
        [
            "10-seednet-eth0.network",
            "10-seednet-mine.network",
            "20-other.network"
        ]
    );
}

#[test]
fn networkd_tells_of_an_override_in_etc() {
    let sysroot = Sysroot::new("ro");
    sysroot.write("cidata/network-config", CONFIG);
    // networkctl edit copied the file of eth0 to /etc, which then hides the one in /run.
    sysroot.write("etc/systemd/network/10-seednet-eth0.network", "[Match]\nName=eth0\n");

    let output = sysroot.apply("rootfs");
    assert_success(&output);
    assert_eq!(sysroot.networks(), ["10-seednet-eth0.network"]);
    assert!(
        stderr(&output)
            .contains("/etc/systemd/network/10-seednet-eth0.network overrides the file of seednet")
    );
    assert_eq!(
        sysroot.read("etc/systemd/network/10-seednet-eth0.network"),
        "[Match]\nName=eth0\n"
    );
}

#[test]
fn network_manager_keyfiles_are_private_and_replace_networkd_files() {
    let sysroot = Sysroot::new("ro");
    sysroot.write("cidata/network-config", CONFIG);
    assert_success(&sysroot.apply("initrd"));
    assert_eq!(sysroot.networks(), ["10-seednet-eth0.network"]);

    // The rootfs uses NetworkManager: the files of the initrd go, and the keyfile is root's only.
    let cidata = sysroot.path("cidata");
    let output = sysroot.seednet(&[
        "--backend",
        "nm",
        "apply",
        "--stage",
        "rootfs",
        "--cidata-dir",
        cidata.to_str().unwrap(),
    ]);
    assert_success(&output);
    assert!(sysroot.networks().is_empty());
    let keyfile =
        sysroot.path("run/NetworkManager/system-connections/10-seednet-eth0.nmconnection");
    assert_eq!(mode(&keyfile), 0o600);
    assert!(fs::read_to_string(keyfile).unwrap().contains("method=auto"));
}

#[test]
fn network_manager_keeps_its_own_profile_of_a_uuid() {
    let sysroot = Sysroot::new("ro");
    sysroot.write(
        "cidata/network-config",
        "version: 2\nethernets:\n  eth0:\n    dhcp4: true\n  eth1:\n    dhcp4: true\n",
    );
    let cidata = sysroot.path("cidata");
    let args = [
        "--backend",
        "nm",
        "apply",
        "--stage",
        "rootfs",
        "--cidata-dir",
        cidata.to_str().unwrap(),
    ];
    assert_success(&sysroot.seednet(&args));
    let eth0 = "run/NetworkManager/system-connections/10-seednet-eth0.nmconnection";
    let eth1 = "run/NetworkManager/system-connections/10-seednet-eth1.nmconnection";
    let uuid = sysroot
        .read(eth0)
        .lines()
        .find_map(|line| line.strip_prefix("uuid="))
        .unwrap()
        .to_owned();

    // NetworkManager moved the profile of eth0 to /etc when it was changed, as it writes it.
    sysroot.write(
        "etc/NetworkManager/system-connections/Wired.nmconnection",
        &format!("[connection]\nid=Wired\nuuid = {}\ntype=ethernet\n", uuid.to_uppercase()),
    );
    let output = sysroot.seednet(&args);
    assert_success(&output);
    assert!(!sysroot.path(eth0).exists());
    assert!(sysroot.path(eth1).exists());
    assert!(stderr(&output).contains(
        "/etc/NetworkManager/system-connections/Wired.nmconnection has the UUID of \
        10-seednet-eth0.nmconnection"
    ));

    // The NetworkManager of Ubuntu keeps it in /etc/netplan instead, by its UUID.
    let eth1_uuid = sysroot
        .read(eth1)
        .lines()
        .find_map(|line| line.strip_prefix("uuid="))
        .unwrap()
        .to_owned();
    sysroot.write(&format!("etc/netplan/90-NM-{eth1_uuid}.yaml"), "network: {version: 2}\n");
    let output = sysroot.seednet(&args);
    assert_success(&output);
    assert!(!sysroot.path(eth1).exists());
    assert!(stderr(&output).contains(&format!(
        "/etc/netplan/90-NM-{eth1_uuid}.yaml has the UUID of 10-seednet-eth1.nmconnection"
    )));
}

#[test]
fn a_config_of_no_interface_leaves_the_network_alone() {
    for config in ["", "network: {config: disabled}\n", "version: 2\n"] {
        let sysroot = Sysroot::new("ro");
        sysroot.write("cidata/network-config", config);
        let cidata = sysroot.path("cidata");

        let output = sysroot.seednet(&[
            "apply",
            "--stage",
            "initrd",
            "--takeover",
            "--cidata-dir",
            cidata.to_str().unwrap(),
        ]);
        assert_success(&output);
        assert!(stderr(&output).contains("configures no interface"), "{config:?}");
        assert!(!stderr(&output).contains("took over"), "{config:?}");
        assert!(!sysroot.path("run/seednet").exists(), "{config:?}");
        assert!(!sysroot.path("run/NetworkManager").exists(), "{config:?}");
    }
}

#[test]
fn takeover_only_with_a_config() {
    let sysroot = Sysroot::new("ro");
    let cidata = sysroot.path("cidata");
    let args = [
        "--backend",
        "auto",
        "apply",
        "--stage",
        "rootfs",
        "--takeover",
        "--cidata-dir",
        cidata.to_str().unwrap(),
    ];

    // Nothing found: cloud-init and the others keep the network.
    assert_success(&sysroot.seednet(&args));
    assert!(
        !sysroot
            .path("run/systemd/system-generators/netplan")
            .exists()
    );
    assert!(!sysroot.path("run/cloud-init/cloud.cfg").exists());

    // Found, in a rootfs with NetworkManager enabled, which leaves ethernet unmanaged as Ubuntu
    // has it.
    sysroot.write("cidata/network-config", CONFIG);
    sysroot.write("etc/systemd/system/multi-user.target.wants/NetworkManager.service", "");
    sysroot.write(
        "usr/lib/NetworkManager/conf.d/10-globally-managed-devices.conf",
        "[keyfile]\nunmanaged-devices=*,except:type:wifi\n",
    );
    let output = sysroot.seednet(&args);
    assert_success(&output);
    assert!(stderr(&output).contains("took over the network"));
    assert!(
        sysroot
            .path("run/NetworkManager/system-connections/10-seednet-eth0.nmconnection")
            .exists()
    );
    assert!(
        sysroot
            .path("run/systemd/system-generators/netplan")
            .is_symlink()
    );
    assert!(
        sysroot
            .read("run/cloud-init/cloud.cfg")
            .contains("network: {config: disabled}")
    );
    assert!(
        !sysroot
            .read("run/NetworkManager/conf.d/10-globally-managed-devices.conf")
            .contains("unmanaged-devices")
    );
}

#[test]
fn takeover_with_networkd_leaves_the_unmanaged_devices_of_network_manager() {
    let sysroot = Sysroot::new("ro");
    sysroot.write("cidata/network-config", CONFIG);
    sysroot.write(
        "usr/lib/NetworkManager/conf.d/10-globally-managed-devices.conf",
        "[keyfile]\nunmanaged-devices=*,except:type:wifi\n",
    );
    let cidata = sysroot.path("cidata");
    assert_success(&sysroot.seednet(&[
        "--backend",
        "networkd",
        "apply",
        "--stage",
        "rootfs",
        "--takeover",
        "--cidata-dir",
        cidata.to_str().unwrap(),
    ]));
    assert!(
        !sysroot
            .path("run/NetworkManager/conf.d/10-globally-managed-devices.conf")
            .exists()
    );
}

#[test]
fn auto_takes_network_manager_in_an_initrd_that_has_it() {
    let sysroot = Sysroot::new("ro");
    sysroot.write("cidata/network-config", CONFIG);
    let cidata = sysroot.path("cidata");
    let args = [
        "--backend",
        "auto",
        "apply",
        "--stage",
        "initrd",
        "--cidata-dir",
        cidata.to_str().unwrap(),
    ];

    // An enabled NetworkManager.service is for the rootfs: the initrd has networkd.
    sysroot.write("etc/systemd/system/multi-user.target.wants/NetworkManager.service", "");
    assert_success(&sysroot.seednet(&args));
    assert_eq!(sysroot.read("run/seednet/backend"), "networkd\n");

    // The unit that dracut puts in an initrd with NetworkManager.
    sysroot.write("usr/lib/systemd/system/nm-initrd.service", "");
    assert_success(&sysroot.seednet(&args));
    assert_eq!(sysroot.read("run/seednet/backend"), "network-manager\n");
    assert!(
        sysroot
            .path("run/NetworkManager/system-connections/10-seednet-eth0.nmconnection")
            .exists()
    );
}

#[test]
fn convert_writes_one_config() {
    let sysroot = Sysroot::new("ro");
    sysroot.write("input", CONFIG);

    let output = sysroot.seednet(&["convert", sysroot.path("input").to_str().unwrap()]);
    assert_success(&output);
    assert_eq!(sysroot.networks(), ["10-seednet-eth0.network"]);
    assert!(String::from_utf8_lossy(&output.stdout).ends_with("10-seednet-eth0.network\n"));
}

#[test]
fn a_timeout_too_long_for_the_clock_waits_without_end() {
    let url = serve(CONFIG, 1);
    let sysroot = Sysroot::new(&format!(
        "seednet.url={url} seednet.url-timeout=18446744073709551615 \
        rd.seednet.timeout=18446744073709551615"
    ));

    assert_success(&sysroot.apply("initrd"));
    assert_eq!(sysroot.networks(), ["10-seednet-eth0.network"]);
}

#[test]
fn a_cidata_disk_that_does_not_mount_is_skipped() {
    let sysroot = Sysroot::new("ro");
    // Not a block device, so it mounts as neither vfat nor iso9660.
    sysroot.write("dev/disk/by-label/cidata", "");

    let output = sysroot.seednet(&["apply", "--stage", "initrd"]);
    assert_success(&output);
    assert!(stderr(&output).contains("skipping the CIDATA disk"));
    assert!(sysroot.networks().is_empty());
    assert!(!sysroot.path("run/seednet/cidata").exists());
}

#[test]
fn initrd_reads_the_cmdline_of_dracut_too() {
    let url = serve(OTHER_CONFIG, 1);
    let sysroot = Sysroot::new("ro seednet.url-timeout=5");
    sysroot.write("etc/cmdline.d/50-seednet.conf", &format!("seednet.url={url}\n"));
    sysroot.write("etc/cmdline.d/60-other.conf", "seednet.url-timeout=soon\n");
    sysroot.write("etc/cmdline.d/README", "seednet.url=http://192.0.2.1/\n");

    // The kernel has the last word, and the rootfs has no /etc/cmdline.d of dracut.
    assert_success(&sysroot.apply("initrd"));
    assert_eq!(sysroot.networks(), ["10-seednet-eth1.network"]);
    assert_eq!(sysroot.read("run/seednet/source"), format!("{url}\n"));
}
