//! Whether the rootfs is reached over the network, in which case the rootfs keeps the
//! network of the initrd: changing it could cut the rootfs off.
//!
//! The rootfs and /usr are followed down to what holds them: through an overlay to its layers,
//! through a loop device to the file behind it, and through a mapped device or a partition to the
//! disks under it. A live rootfs that was fetched into memory is not on the network, so it may
//! change the network.

use std::fs;
use std::path::{Path, PathBuf};

/// File systems that are themselves on the network.
const NETWORK_FS_TYPES: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "ceph",
    "fuse.ceph-fuse",
    "fuse.glusterfs",
    "fuse.sshfs",
    "lustre",
    "afs",
];

/// The NVMe transports that go over the network, as NVMe over Fabrics has them.
const NVME_NETWORK_TRANSPORTS: &[&str] = &["tcp", "rdma", "fc"];

/// How deep to follow what holds a mount, which only guards against a loop.
const MAX_DEPTH: usize = 16;

/// Whether the rootfs or /usr is on the network, judged from the mounts of `sysroot`.
pub fn is_network_rootfs(sysroot: &Path) -> bool {
    let Ok(mountinfo) = fs::read_to_string(sysroot.join("proc/self/mountinfo")) else {
        return false;
    };
    let mounts = Mounts::parse(&mountinfo);
    let checker = Checker {
        sysroot,
        mounts: &mounts,
    };
    ["/", "/usr"].iter().any(|path| {
        mounts
            .holding(path)
            .is_some_and(|mount| checker.mount_is_network(mount, 0))
    })
}

#[derive(Debug, PartialEq, Eq)]
struct Mount<'a> {
    /// `major:minor`, which is `0:N` for a file system without a block device of its own.
    device: &'a str,
    mount_point: String,
    fs_type: &'a str,
    source: &'a str,
    super_options: &'a str,
}

struct Mounts<'a>(Vec<Mount<'a>>);

impl<'a> Mounts<'a> {
    fn parse(mountinfo: &'a str) -> Self {
        Self(
            mountinfo
                .lines()
                .filter_map(|line| {
                    let (fields, rest) = line.split_once(" - ")?;
                    let mut fields = fields.split(' ');
                    let device = fields.nth(2)?;
                    let mount_point = unescape(fields.nth(1)?);
                    let mut rest = rest.split(' ');
                    Some(Mount {
                        device,
                        mount_point,
                        fs_type: rest.next()?,
                        source: rest.next()?,
                        super_options: rest.next().unwrap_or_default(),
                    })
                })
                .collect(),
        )
    }

    /// The mount that holds `path`: the one on the longest leading part of it, the last one if
    /// stacked.
    fn holding(&self, path: &str) -> Option<&Mount<'a>> {
        self.0
            .iter()
            .enumerate()
            .filter(|(_, mount)| is_under(path, &mount.mount_point))
            .max_by_key(|(i, mount)| (mount.mount_point.len(), *i))
            .map(|(_, mount)| mount)
    }
}

struct Checker<'a> {
    sysroot: &'a Path,
    mounts: &'a Mounts<'a>,
}

impl Checker<'_> {
    fn mount_is_network(&self, mount: &Mount, depth: usize) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        if NETWORK_FS_TYPES.contains(&mount.fs_type) {
            return true;
        }
        if mount.fs_type == "overlay" {
            return overlay_layers(mount.super_options)
                .any(|layer| self.path_is_network(&layer, depth + 1));
        }
        self.block_device(mount)
            .is_some_and(|device| self.block_device_is_network(&device, depth + 1))
    }

    /// Whether the file at `path` is on the network.
    fn path_is_network(&self, path: &str, depth: usize) -> bool {
        self.mounts
            .holding(path)
            .is_some_and(|mount| self.mount_is_network(mount, depth))
    }

    /// The block device in sysfs of a mount: by its `major:minor`, or, for a file system that has
    /// none of its own, such as btrfs, by its source.
    fn block_device(&self, mount: &Mount) -> Option<PathBuf> {
        let by_number = self.sysroot.join("sys/dev/block").join(mount.device);
        if by_number.exists() {
            return Some(by_number);
        }
        let source = Path::new(mount.source.strip_prefix("/dev/")?);
        // A link such as /dev/mapper/root or /dev/disk/by-uuid/... names the device it points to.
        let resolved = fs::canonicalize(self.sysroot.join("dev").join(source)).ok();
        let name = resolved.as_deref().unwrap_or(source).file_name()?;
        let path = self.sysroot.join("sys/class/block").join(name);
        path.exists().then_some(path)
    }

    /// Whether a block device in sysfs is on the network, or is built on what is: a partition of
    /// such a disk, a mapped device on one, or a loop device of a file on the network.
    fn block_device_is_network(&self, path: &Path, depth: usize) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        let Ok(path) = fs::canonicalize(path) else {
            return false;
        };
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();

        // NBD, Ceph RBD, ATA over Ethernet and DRBD disks, and an iSCSI disk, which sits under its
        // session, as in .../host2/session1/target2:0:0/...
        if ["nbd", "rbd", "etherd!", "drbd"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
            || path.components().any(|c| is_iscsi_session(c.as_os_str()))
            || self.is_nvme_over_fabrics(&path)
        {
            return true;
        }

        if let Ok(backing_file) = fs::read_to_string(path.join("loop/backing_file"))
            && self.path_is_network(backing_file.trim_end(), depth + 1)
        {
            return true;
        }

        // A partition is under its disk, and a mapped device lists what it is built on in slaves/.
        let parent_is_network = path.parent().is_some_and(|parent| {
            parent.join("dev").exists() && self.block_device_is_network(parent, depth + 1)
        });
        parent_is_network
            || fs::read_dir(path.join("slaves")).is_ok_and(|slaves| {
                slaves
                    .flatten()
                    .any(|slave| self.block_device_is_network(&slave.path(), depth + 1))
            })
    }

    /// Whether a block device is an NVMe namespace over a network transport: its controllers say
    /// the transport, whether the namespace is under one or under a subsystem.
    fn is_nvme_over_fabrics(&self, path: &Path) -> bool {
        let transport_is_network = |controller: &Path| {
            fs::read_to_string(controller.join("transport"))
                .is_ok_and(|transport| NVME_NETWORK_TRANSPORTS.contains(&transport.trim()))
        };
        let sys = fs::canonicalize(self.sysroot.join("sys")).unwrap_or_default();
        path.ancestors()
            .take_while(|dir| dir.starts_with(&sys))
            .any(|dir| {
                transport_is_network(dir)
                    || fs::read_dir(dir).is_ok_and(|entries| {
                        dir.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("nvme-subsys"))
                            && entries.flatten().any(|entry| {
                                entry.file_name().to_string_lossy().starts_with("nvme")
                                    && transport_is_network(&entry.path())
                            })
                    })
            })
    }
}

/// The directories of the layers of an overlay, from its super options.
fn overlay_layers(options: &str) -> impl Iterator<Item = String> + '_ {
    options
        .split(',')
        .filter_map(|option| option.split_once('='))
        .filter(|(key, _)| matches!(*key, "lowerdir" | "upperdir" | "lowerdir+" | "datadir+"))
        .flat_map(|(_, dirs)| dirs.split(':'))
        .filter(|dir| !dir.is_empty())
        .map(unescape)
}

/// Whether `path` is `dir` or under it.
fn is_under(path: &str, dir: &str) -> bool {
    dir == "/"
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Undoes the octal escapes of mountinfo, as `\040` for a space.
pub fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes.get(i + 1..i + 4).and_then(|digits| {
            std::str::from_utf8(digits)
                .ok()
                .and_then(|digits| u8::from_str_radix(digits, 8).ok())
        });
        match (bytes[i], octal) {
            (b'\\', Some(byte)) => {
                out.push(byte);
                i += 4;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether a sysfs path component is an iSCSI session, such as `session1`.
fn is_iscsi_session(component: &std::ffi::OsStr) -> bool {
    component
        .to_str()
        .and_then(|name| name.strip_prefix("session"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    const MOUNTINFO: &str = "\
22 1 252:1 / / rw,relatime shared:1 - ext4 /dev/vda1 rw
23 22 0:21 / /proc rw shared:2 - proc proc rw
";

    struct Sysroot(tempfile::TempDir);

    impl Sysroot {
        fn new() -> Self {
            let sysroot = Self(tempfile::tempdir().unwrap());
            fs::create_dir_all(sysroot.path("proc/self")).unwrap();
            fs::create_dir_all(sysroot.path("sys/dev/block")).unwrap();
            fs::create_dir_all(sysroot.path("sys/class/block")).unwrap();
            sysroot
        }

        fn path(&self, path: &str) -> PathBuf {
            self.0.path().join(path)
        }

        fn mountinfo(&self, mountinfo: &str) {
            fs::write(self.path("proc/self/mountinfo"), mountinfo).unwrap();
        }

        /// A block device in sysfs, linked from /sys/dev/block and /sys/class/block.
        fn device(&self, path: &str, number: &str) -> PathBuf {
            let device = self.path("sys/devices").join(path);
            fs::create_dir_all(&device).unwrap();
            fs::write(device.join("dev"), format!("{number}\n")).unwrap();
            symlink(&device, self.path("sys/dev/block").join(number)).unwrap();
            let name = device.file_name().unwrap();
            symlink(&device, self.path("sys/class/block").join(name)).unwrap();
            device
        }

        fn is_network_rootfs(&self) -> bool {
            is_network_rootfs(self.0.path())
        }
    }

    #[test]
    fn finds_the_mount_that_holds_a_path() {
        let overlay = format!("{MOUNTINFO}30 22 0:40 / / rw - overlay overlay rw,lowerdir=/a\n");
        let mounts = Mounts::parse(&overlay);
        assert_eq!(mounts.holding("/").unwrap().fs_type, "overlay");
        assert_eq!(mounts.holding("/proc/self").unwrap().fs_type, "proc");
        assert_eq!(mounts.holding("/procfs").unwrap().fs_type, "overlay");

        let escaped = Mounts::parse("1 0 0:5 / /a\\040b rw - tmpfs tmpfs rw\n");
        assert_eq!(escaped.holding("/a b/c").unwrap().mount_point, "/a b");
    }

    #[test]
    fn tells_a_network_file_system() {
        let sysroot = Sysroot::new();
        sysroot.mountinfo("1 0 0:50 / / rw - nfs4 s:/e rw\n");
        assert!(sysroot.is_network_rootfs());
        sysroot.mountinfo("1 0 0:50 / / rw - fuse.glusterfs s:/v rw\n");
        assert!(sysroot.is_network_rootfs());

        // /usr alone on the network.
        sysroot.mountinfo(&format!("{MOUNTINFO}24 22 0:51 / /usr rw - nfs s:/usr rw\n"));
        assert!(sysroot.is_network_rootfs());
    }

    #[test]
    fn tells_a_network_block_device() {
        let sysroot = Sysroot::new();

        // An NBD partition: /sys/dev/block/43:1 -> devices/virtual/block/nbd0/nbd0p1
        sysroot.device("virtual/block/nbd0", "43:0");
        sysroot.device("virtual/block/nbd0/nbd0p1", "43:1");
        sysroot.mountinfo("1 0 43:1 / / rw - ext4 /dev/nbd0p1 rw\n");
        assert!(sysroot.is_network_rootfs());

        // A local disk.
        sysroot.device("pci0000:00/0000:00:02.0/virtio1/block/vda", "252:0");
        sysroot.mountinfo("1 0 252:0 / / rw - ext4 /dev/vda rw\n");
        assert!(!sysroot.is_network_rootfs());

        // btrfs on NBD, whose mount has no device number of its own.
        sysroot.mountinfo("1 0 0:33 /@ / rw - btrfs /dev/nbd0p1 rw,subvol=/@\n");
        assert!(sysroot.is_network_rootfs());
        sysroot.mountinfo("1 0 0:33 /@ / rw - btrfs /dev/vda rw,subvol=/@\n");
        assert!(!sysroot.is_network_rootfs());
    }

    #[test]
    fn tells_nvme_over_fabrics() {
        let sysroot = Sysroot::new();
        let subsystem = "virtual/nvme-subsystem/nvme-subsys0";
        sysroot.device(&format!("{subsystem}/nvme0n1"), "259:0");
        let controller = sysroot.path("sys/devices").join(subsystem).join("nvme0");
        fs::create_dir_all(&controller).unwrap();
        fs::write(controller.join("transport"), "tcp\n").unwrap();
        sysroot.mountinfo("1 0 259:0 / / rw - xfs /dev/nvme0n1 rw\n");
        assert!(sysroot.is_network_rootfs());

        fs::write(controller.join("transport"), "pcie\n").unwrap();
        assert!(!sysroot.is_network_rootfs());
    }

    #[test]
    fn follows_an_overlay_and_a_loop_device() {
        let sysroot = Sysroot::new();
        // A live rootfs: an overlay on a squashfs of a loop device, whose file is on NFS.
        let loop0 = sysroot.device("virtual/block/loop0", "7:0");
        fs::create_dir_all(loop0.join("loop")).unwrap();
        fs::write(loop0.join("loop/backing_file"), "/run/initramfs/live/LiveOS/squashfs.img\n")
            .unwrap();
        let live = "\
1 0 0:40 / / rw - overlay LiveOS_rootfs rw,lowerdir=/run/rootfsbase,upperdir=/run/overlay/upper
2 1 0:24 / /run rw - tmpfs tmpfs rw
3 2 0:50 / /run/initramfs/live ro - nfs s:/live ro
4 2 7:0 / /run/rootfsbase ro - squashfs /dev/loop0 ro
";
        sysroot.mountinfo(live);
        assert!(sysroot.is_network_rootfs());

        // The same, fetched into memory.
        fs::write(loop0.join("loop/backing_file"), "/run/initramfs/squashfs.img\n").unwrap();
        assert!(!sysroot.is_network_rootfs());
    }
}
