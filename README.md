# seednet

Converts the `network-config` of the cloud-init [NoCloud] datasource to systemd-networkd or
NetworkManager files.

It is meant for an initrd that brings up the network before the rootfs is there, such as a dracut
initrd that fetches a live rootfs over HTTP on a host without DHCP, and for the rootfs after it, so
that both bring up the network the way the same config says.

[NoCloud]: https://docs.cloud-init.io/en/latest/reference/datasources/nocloud.html

## Usage

### `seednet apply`

Finds the `network-config` for this boot, converts it, and writes the files.

```console
$ seednet apply --stage initrd
seednet: using the network-config from the CIDATA disk
/run/systemd/network/10-seednet-id0.network
```

The config comes from the first of these that is there:

| Source | `--stage initrd` | `--stage rootfs` |
|---|---|---|
| `seednet.network-config=` on the kernel command line | yes | yes |
| `/etc/seednet/network-config` | | yes |
| The config the initrd used (`/run/seednet/network-config`) | | yes |
| `seednet.url=` on the kernel command line | yes | |
| The `network-config` on the CIDATA disk | yes | yes |

Without any, it writes nothing and leaves the network alone, as it does with a config that
configures no interface, such as an empty or disabled (`config: disabled`) one.

When the rootfs or `/usr` is on the network, the rootfs keeps the network of the initrd and ignores
`/etc/seednet/network-config`, since changing the network could cut the rootfs off. That is a
network file system (NFS, CIFS, CephFS, GlusterFS, SSHFS, Lustre or AFS), or a disk over the
network (NBD, iSCSI, NVMe over Fabrics, Ceph RBD, ATA over Ethernet or DRBD), followed through
partitions, device mapper, loop devices and overlays, as for a live image read over NFS. A ZFS pool
is not followed, nor a disk over FCoE.

It keeps the config it used in `/run/seednet/network-config`, readable by root only, where it came
from in `/run/seednet/source`, and the backend in `/run/seednet/backend`. In the rootfs, it creates
`/run/seednet/network-rootfs` when the rootfs is on the network. To keep the config when deploying to a disk, copy the former to
`/etc/seednet/network-config` on the disk: the rootfs then uses it on every boot, whatever the CIDATA
disk says. Seednet itself never writes to `/etc`.

- `--stage <initrd|rootfs>`: where it runs, which decides the sources above.
- `--cmdline <FILE>`: the kernel command line to read, instead of `/proc/cmdline` and, with
  `--stage initrd`, the `*.conf` files in `/etc/cmdline.d` before it, where
  `dracut --kernel-cmdline` puts the arguments it adds, as dracut reads them.
- `--cidata-dir <DIR>`: a directory with the CIDATA files, instead of finding and mounting the disk.
- `--ca-file <FILE>`: the CA certificates for an `https` URL, instead of the system bundle.
- `--takeover`: when a config is found, keeps the others from configuring the network themselves,
  for this boot: it removes what the netplan generator wrote, but for the connections that
  NetworkManager keeps in `/etc/netplan` on Ubuntu (see `--backend`), and in the rootfs masks the
  generator (`/run/systemd/system-generators/netplan`); it tells NetworkManager to make no
  connection of its own and, unless the rootfs is on the network, to bring up its connections on an
  interface the initrd configured (`/run/NetworkManager/conf.d/10-seednet.conf`); and, in the
  rootfs, it sets `network: {config: disabled}` in the runtime config of cloud-init
  (`/run/cloud-init/cloud.cfg`).
  It also removes the DHCP connection that an initrd with NetworkManager makes when nothing says
  how the network is. With `network-manager`, it also hides the config of Ubuntu that leaves all
  but Wi-Fi and modems unmanaged (`/usr/lib/NetworkManager/conf.d/10-globally-managed-devices.conf`),
  with an empty file of the same name in `/run`, as netplan does. Without a config, it leaves them
  alone.

#### Kernel command line

- `seednet.network-config=<BASE64>`: the config itself, in base64 (standard or URL-safe, padded or
  not) of the text or of its gzip. It wins over every other source, and warns when it overrides
  `/etc/seednet/network-config` or a CIDATA disk.
- `seednet.url=<URL>`: an `http` or `https` URL to fetch the config from, over a network the initrd
  brought up for it. A fetch that does not succeed fails, and so fails the boot. An `https` URL
  follows no redirect to `http`. An empty one undoes one before it.
- `seednet.checksum=<ALGORITHM>:<HEX>`: what the fetched config must hash to, with `md5`, `sha1`,
  `sha256` or `sha512`. Prefer `sha256` or `sha512`.
- `seednet.url-timeout=<SECONDS>`: how long to keep trying to fetch the config while the network
  comes up (default: 60). A failure that trying again would not fix, such as a bad certificate or a
  `404`, fails at once.
- `rd.seednet.timeout=<SECONDS>`: how long to wait for the CIDATA disk (default: 5), in the rootfs
  too.

Only the last of a parameter counts, so only it must be valid, and `_` is the same as `-` in a name,
as systemd takes its own. The rootfs does not read the parameters of the fetch, but for the URL.

The CIDATA disk is the one labelled `cidata` or `CIDATA`, as `vfat` or `iso9660`, as cloud-init
looks for it. It is mounted read-only while it is read, or read where it is mounted already. A disk
with the label that mounts as neither is skipped with a warning, as cloud-init skips it.

An `https` URL is verified against the system CA bundle only (`/etc/ssl/certs/ca-certificates.crt`,
`/etc/pki/tls/certs/ca-bundle.crt`, `/etc/ssl/ca-bundle.pem` or `/etc/ssl/cert.pem`), so the initrd
needs one, and a clock close enough to the time for the certificate to be valid.

### `seednet convert`

Converts one `network-config` and writes the files, without looking for sources.

```console
$ seednet convert /path/to/network-config
/run/systemd/network/10-seednet-id0.network
```

- `INPUT`: the `network-config` to convert, or `-` for standard input.

### Both

- `--sysroot <DIR>`: the directory that every path is under (default: `/`), for tests and image builds.
- `--backend <auto|networkd|network-manager>`: the service that brings up the network (default:
  `networkd`). `auto` is `network-manager` in an initrd that has NetworkManager, as dracut puts it
  there (`nm-initrd.service` or `NetworkManager-initrd.service`), and in a rootfs where
  `NetworkManager.service` is enabled, and `networkd` otherwise. `networkd` writes `.network` and `.netdev` files to `/run/systemd/network`, and
  `network-manager` (or `nm`) writes keyfiles, readable by root only, to
  `/run/NetworkManager/system-connections`. It writes no keyfile for a connection whose UUID is in
  `/etc/NetworkManager/system-connections`, where NetworkManager moves a connection of seednet that
  was changed, as in its GUI or with `nmcli`, so that the change stays. On Ubuntu, NetworkManager
  keeps it in `/etc/netplan/90-NM-<UUID>.yaml` instead, which netplan renders to `/run`, and it
  writes none for that UUID either. The files are named by the
  ID in the config, such as `10-seednet-br0.network`, so that a drop-in or a copy that
  `networkctl edit` makes in `/etc/systemd/network` keeps applying to the same interface when the
  config changes. A copy hides the file of seednet, which warns of it. Either writes the `.link`
  files that rename interfaces to `/run/systemd/network`, where udev reads them.
- `--prefix <PREFIX>`: the prefix of the file names (default: `10-seednet`), which decides where they
  sort among the other files.

They print the path of each file written, and replace the files that seednet wrote before with that
prefix, for either backend, so that a rootfs using NetworkManager drops the networkd files of an
initrd. An empty or disabled (`config: disabled`) config writes nothing. An invalid config fails
before anything is written.

## Boot integration

`contrib/` has what runs seednet at boot, with seednet installed as `/usr/bin/seednet`.

### Initrd: `contrib/dracut/95seednet`

A dracut module for an initrd with systemd-networkd or NetworkManager, included only when asked
for. It takes the one that dracut picks for the network: the one that the initrd is given, or else
NetworkManager when the system has it, as Fedora and Enterprise Linux (RHEL, AlmaLinux, Rocky
Linux) do, and systemd-networkd otherwise. The initrd may have either, whichever the rootfs has:

```console
$ cp -r contrib/dracut/95seednet /usr/lib/dracut/modules.d/
$ dracut --add seednet ...
$ dracut --add "seednet systemd-networkd" --omit "network-manager network-legacy" ...
```

On Fedora, `dracut-network` brings NetworkManager, so an initrd with systemd-networkd needs the
second form.

It brings up the network of the initrd when the network is needed: with `rd.neednet=1`, with a rootfs
that implies it, such as `root=live:https://...`, or with `seednet.url=`.

- Without `seednet.url=`, it runs `seednet apply --stage initrd --takeover` before systemd-networkd
  starts, and renames the interfaces.
- With `seednet.url=`, it fetches the config over the DHCP network of dracut, then renames the
  interfaces and restarts systemd-networkd or NetworkManager with the config, and waits for the
  network, before the initqueue reaches for the rootfs.

`seednet.url=` may be on the kernel command line or in the initrd, as `dracut --kernel-cmdline`
puts it there.

Either way, a config replaces the catch-all DHCP of dracut, and a failure ends in the emergency
shell. The module brings the bonding, bridge, 8021q, vfat and iso9660 modules, and the CA bundle
of the system for an `https` URL. It also brings systemd-resolved when the system has it, which a
host name in `seednet.url=` or `root=live:...` needs with systemd-networkd: without it, the initrd
cannot resolve host names, and dracut says so. NetworkManager writes `/etc/resolv.conf` itself.

### Rootfs: `contrib/systemd`

`seednet.service` runs `seednet-rootfs`, installed as `/usr/libexec/seednet-rootfs`, before
systemd-networkd, NetworkManager and cloud-init:

```console
$ install -m 0755 contrib/systemd/seednet-rootfs /usr/libexec/seednet-rootfs
$ install -m 0644 contrib/systemd/seednet.service /usr/lib/systemd/system/
$ systemctl enable seednet.service
```

It runs `seednet --backend auto apply --stage rootfs --takeover`, so the rootfs keeps the config of the
initrd, or uses `/etc/seednet/network-config` or the CIDATA disk, and renames the interfaces. When
the rootfs uses NetworkManager after an initrd without it, it deletes the bonds, bridges and VLANs
of the initrd, which NetworkManager cannot change while they have ports, unless the rootfs is on the
network. When it uses systemd-networkd after an initrd with NetworkManager, it removes the IPv6
link-local addresses of the bonds and bridges, which would stay along with the ones of the MAC
addresses that systemd-networkd gives them.

With SELinux, as on Fedora and Enterprise Linux, the files that seednet writes in `/run` take the
labels of the policy as systemd and the directories give them, and need no `restorecon`.

## Supported config

Both [version 1] and [version 2] are supported, nested under `network:` or as the whole document:
physical interfaces, bonds, bridges and VLANs. Other types, such as Wi-Fi and tunnels, the keys
that are not below, and the routes of a `type` other than `unicast` are skipped with a warning.
`renderer` is ignored, since seednet is the renderer.

A key means what it does in netplan with systemd-networkd, which is how cloud-init brings up the
network on Ubuntu. Each backend is given what it can do of the config, and skips the rest with a
warning: systemd-networkd takes all of the table, and NetworkManager, which a desktop needs little
more of than the addresses, takes what its column says.

What network-config has no key for, seednet does not either. Such settings of systemd-networkd go
in a drop-in in `/etc`, such as `/etc/systemd/network/10-seednet-eth0.network.d/50-local.conf`,
which keeps applying since the files of seednet are named by the ID in the config.

| Version 1 | Version 2 | networkd | NetworkManager |
|---|---|---|---|
| `type: physical` | `ethernets` | `.network` | `type=ethernet` |
| `type: bond` (`bond_interfaces`) | `bonds` (`interfaces`) | `.netdev` `Kind=bond`, `Bond=` | `type=bond`, `master=` |
| `type: bridge` (`bridge_interfaces`) | `bridges` (`interfaces`) | `.netdev` `Kind=bridge`, `Bridge=` | `type=bridge`, `master=` |
| `type: vlan` (`vlan_link`, `vlan_id`) | `vlans` (`link`, `id`) | `.netdev` `Kind=vlan`, `VLAN=` | `type=vlan`, `parent=` |
| `mac_address`, else `name` | `match` (`macaddress`, `name`, `driver` or a list of them), else the ID | `[Match]` | `mac-address=`, `interface-name=`, `[match]` |
| `name` with `mac_address` | `set-name` | `.link` `Name=` | `.link` `Name=` |
| `mac_address` of a bond, bridge or VLAN | `macaddress` | `MACAddress=`, but `permanent`, `random`, `stable` and `preserve` skipped | `cloned-mac-address=` |
| `mtu`, static subnet `mtu` | `mtu` | `MTUBytes=` | `mtu=` |
| | `optional` | `RequiredForOnline=no` | skipped |
| subnet `dhcp`, `dhcp4`, `dhcp6`, `ipv6_slaac`, `ipv6_dhcpv6-stateless`, `ipv6_dhcpv6-stateful` | `dhcp4`, `dhcp6` | `DHCP=` | `method=auto` |
| `accept-ra` | `accept-ra` | `IPv6AcceptRA=` | `method=auto` |
| | `link-local` | `LinkLocalAddressing=` | `method=link-local`, or `link-local=` of IPv4 with other addresses |
| | `dhcp-identifier` (`mac` unless said) | `ClientIdentifier=` | `dhcp-client-id=` |
| subnet `static`, `static6` (`address`, `netmask`, `prefix`) | `addresses` | `Address=` | `address1=` |
| static subnet `gateway` | `gateway4`, `gateway6` | `Gateway=`, or `[Route]` in version 1 | `gateway=`, or `route1=` in version 1, for a gateway beyond the first, without an address of its family, or with `use-routes: false` |
| subnet `dns_nameservers`, `dns_search`, `type: nameserver` | `nameservers` | `DNS=`, `Domains=` | `dns=`, `dns-search=` |
| static subnet `routes`, `type: route` | `routes` (`to`, `via`, `metric`, `on-link`, `table`) | `[Route]` | `route1=` |
| | `addresses` options `label`, `lifetime` | `[Address]` `Label=`, `PreferredLifetime=` | skipped |
| | `routing-policy` | `[RoutingPolicyRule]` | `routing-rule1=`, with `priority` only |
| | `ipv6-privacy`, `ipv6-mtu` | `IPv6PrivacyExtensions=`, `IPv6MTUBytes=` | `ip6-privacy=`, `mtu=` |
| | `dhcp4-overrides`, `dhcp6-overrides` | `[DHCPv4]`, `[DHCPv6]`, `[IPv6AcceptRA]` | see below |

Version 1 is read as cloud-init converts it to version 2 for netplan:

- `ipv6_slaac` and `ipv6_dhcpv6-stateless` are `dhcp6`, which takes the router advertisements too.
- Only a static subnet takes `gateway`, `mtu`, `dns_nameservers`, `dns_search` and `routes`. Its
  `gateway` is a default route, on the link when the subnet does not have it, and its `metric` is
  the one of that route and of its `routes` that have none. The `mtu` of an IPv4 subnet wins over
  the one of the interface, and that of an IPv6 one is `ipv6-mtu`.
- The `metric` of a DHCP subnet is the `route-metric` of DHCP, the same for DHCPv4 and DHCPv6.
- The name servers of `type: nameserver` without `interface` go to the interfaces with static
  addresses and no name servers or search domains of their own.
- `type: loopback` is ignored, and so is `optional`, which that conversion drops, with a warning.
- A `metric` of 0 of a subnet is none, as cloud-init drops the values that are not true; that of a
  route stays.
- A route to `default` is to `0.0.0.0/0` or `::/0`, by the family of its gateway.

Unlike that conversion, which drops them, the name servers of `type: nameserver` with `interface`
are added to that interface, and a `type: route` goes to the interface whose subnet has its
gateway.

The bond parameters are `mode`, `lacp-rate`, `mii-monitor-interval`, `up-delay`, `down-delay`,
`transmit-hash-policy`, `ad-select`, `arp-interval`, `arp-ip-targets`, `fail-over-mac-policy`,
`min-links` and `primary`, and the bridge parameters `stp`, `forward-delay`, `hello-time`,
`max-age`, `ageing-time`, `priority`, `path-cost` and `port-priority`, with their version 1
spellings (`bond-miimon`, `bridge_fd` and so on). `mode`, `lacp-rate`, `transmit-hash-policy`,
`ad-select` and `fail-over-mac-policy` take only the names that both systemd-networkd and
NetworkManager take, such as `802.3ad`, and a name that the config gives an interface must be one
that the kernel takes: 1 to 15 bytes, without `/`, `:` or white space. A bridge with `parameters`,
even empty ones, has STP on unless they say `stp: false`, as netplan has it, and a VLAN ID is 0 to
4094.

The config fails, as netplan does, on an address without its prefix length, a route without `to`
or to `default` without `via`, whose family it would not know, and a MAC address that is none. A
route to an address without a prefix length is to that host alone. The values that go in the files
as they are, such as IDs, names, driver globs, search domains, the DHCP hostname and address labels,
may not have white space, control characters, `;`, `,`, `\` or quotes, which would break the files.

A port of a bond or bridge has no addresses of its own: its addressing, such as `dhcp4`,
`addresses` or `nameservers`, is skipped with a warning.

The DHCP overrides are `use-dns`, `use-ntp`, `use-hostname`, `use-domains`, `use-mtu`,
`use-routes`, `route-metric`, `send-hostname` and `hostname`. As netplan writes them to one
`[DHCP]` section:

- `dhcp4-overrides` and `dhcp6-overrides` must be the same when both `dhcp4` and `dhcp6` are on,
  and the config fails otherwise.
- `use-dns`, `use-ntp`, `use-domains` and `send-hostname` are for DHCPv4 and DHCPv6, and
  `use-mtu`, `use-routes`, `use-hostname` and `hostname` for DHCPv4 alone, so `dhcp6-overrides`
  without `dhcp4` skips the latter with a warning.
- `route-metric` is the metric of the routes of DHCPv4 and of the router advertisements, and is
  100 when the config says none. The MTU of DHCPv4 is used unless `use-mtu` is false.

NetworkManager takes `route-metric`, `use-routes` (`ignore-auto-routes=` and `never-default=`),
`send-hostname`, `hostname`, and `use-dns` (`ignore-auto-dns=`) for DHCPv4: for DHCPv6 it would
ignore the name servers of the router advertisements too. It skips the others with a warning, but
for what it does on its own, such as `use-mtu: true`. It also skips a `link-local` without `ipv6`
on an interface with IPv6, which always has its IPv6 link-local address with NetworkManager, the
name servers, gateways, routes and routing policy rules of a family that the interface has disabled,
which NetworkManager does not take, and the `arp-ip-targets` of a bond without `arp-interval`. The
search domains go with IPv4, unless only IPv6 has name servers or IPv4 is disabled.

A physical interface with a MAC address is matched on its permanent one, which stays when a bond
gives its ports its own address, and never on the name it is renamed to, so its files apply before
and after the rename. One found by its name is matched on that name and the one it is renamed to,
for the same. The rename is for udev, which reads the `.link` files when the interface
appears: after writing them, reload udev and trigger the network devices again
(`udevadm control --reload`, then `udevadm trigger --action=add --subsystem-match=net`) before
the interfaces are up, since a running interface cannot be renamed.

An interface that gets no address and is no port, such as one that only carries VLANs, does not
hold up `systemd-networkd-wait-online`. NetworkManager names a bond's `primary` port by its
interface name, so a port matched on its MAC address alone needs `set-name` for it.

[version 1]: https://docs.cloud-init.io/en/latest/reference/network-config-format-v1.html
[version 2]: https://docs.cloud-init.io/en/latest/reference/network-config-format-v2.html

## Install

The [releases] have static binaries for `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl`, and `seednet-contrib.tar.gz` with what `contrib/` has, each with its
SHA-256 in `SHA256SUMS`, a Sigstore bundle signed by the release workflow, and a build provenance
attestation:

```console
$ version=v0.1.0 target=x86_64-unknown-linux-musl
$ base=https://github.com/takumin/seednet/releases/download/$version
$ curl -fLO "$base/seednet-$target" -fLO "$base/seednet-$target.sigstore.json" \
    -fLO "$base/seednet-contrib.tar.gz" -fLO "$base/SHA256SUMS"
$ sha256sum --ignore-missing -c SHA256SUMS
$ cosign verify-blob --bundle "seednet-$target.sigstore.json" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    --certificate-identity "https://github.com/takumin/seednet/.github/workflows/wc-sign.yml@refs/tags/$version" \
    "seednet-$target"
$ gh attestation verify "seednet-$target" --repo takumin/seednet
$ install -m 0755 "seednet-$target" /usr/bin/seednet
$ tar -xzf seednet-contrib.tar.gz
```

Then install what `contrib/` has as [Boot integration](#boot-integration) says.

[releases]: https://github.com/takumin/seednet/releases

## Build

```console
$ cargo build --release --target x86_64-unknown-linux-musl
```

### End-to-end tests

`task e2e` boots guests on cloud-hypervisor and checks their network, without root privileges:
cloud-init, seednet in a dracut initrd, and systemd-networkd or NetworkManager, with three NICs on
passt for a bond, a VLAN and a bridge on it (see `e2e/network-config`).
The guests run at once, `E2E_JOBS` of them (by default, as many as the host has two CPUs and 4 GiB
of available memory for), and the scenarios of each in turn; the output of each scenario goes to
`target/e2e/<guest>/<scenario>.log`. `task e2e -- debian-nm-networkd el10-nm-nm` boots some guests
only, and `task e2e:run DISTRO=debian INITRD=nm BACKEND=networkd SCENARIO=live` one scenario, with
`INITRD` and `BACKEND` the backends of the initrd and of the rootfs.

| Distro | Initrd, rootfs | Guest |
|---|---|---|
| `debian` | `networkd` or `nm`, each with `networkd` or `nm` | Debian trixie, with netplan when the rootfs has `networkd`. |
| `ubuntu` | `networkd`, `networkd`; `nm`, `nm` | Ubuntu 26.04, with netplan. |
| `fedora` | `networkd`, `networkd`; `nm`, `nm` | Fedora 44, with systemd-resolved. |
| `el9` | `nm`, `nm` | Rocky Linux 9. |
| `el10` | `nm`, `nm` | AlmaLinux 10. |

Where the initrd brings up the network, the check also makes sure that it did so with its backend.
The Fedora and Enterprise Linux guests run SELinux enforcing, and fail on any denial. Their images list the
labels of their files, which `e2e/disks.sh` puts on the disks, since `podman export` keeps none.

| Scenario | Boot |
|---|---|
| `disk` | The rootfs on a disk, and the rootfs reads the CIDATA disk. |
| `neednet` | As `disk`, with `rd.neednet=1`: the initrd brings up the network and the rootfs keeps it. The config is of version 1, on a CIDATA disk of iso9660 labelled `cidata`, as `cloud-localds` makes it. |
| `cmdline` | As `neednet`, without a CIDATA disk: the config is in `seednet.network-config`, gzipped and in URL-safe base64. |
| `url` | No CIDATA disk: the initrd fetches `seednet.url` over DHCP and checks `seednet.checksum`. |
| `live` | No DHCP and no rootfs disk: the initrd brings up the network from the CIDATA disk to fetch the rootfs, as `root=live:http://...` says. |
| `edit` | As `disk`, then `nmcli` changes a connection of seednet as the GUI does, or `networkctl edit` adds a drop-in to one file of seednet and copies another to `/etc`, and a second boot checks that the changes stayed. |
| `https` | As `url` and `live` at once, by a host name over HTTPS: the initrd fetches `seednet.url` with the name server of DHCP, then the live rootfs with the one of the config. The image trusts a CA of the tests, and the name server of the tests runs with passt in a network namespace of its own. |
| `badsum` | As `url`, with a `seednet.checksum` that the config does not have: the boot must stop in the emergency shell of the initrd, saying why. |

They need an x86_64 host with KVM, podman, passt with vhost-user, cloud-hypervisor, mkdosfs, mcopy,
xorrisofs, mksquashfs 4.6 or later, mkfs.ext4 and debugfs of e2fsprogs 1.43 or later (for `mkfs.ext4 -d`),
`ip` of iproute2, `unshare` of util-linux, `timeout` and `sha256sum` of coreutils, openssl and
python3, and work in `target/e2e`.

The workspace has two crates: `crates/seednet`, a library that parses and converts the config
without any I/O, and `crates/seednet-cli`, which builds the `seednet` command.

## License

[Apache License 2.0](LICENSE)
