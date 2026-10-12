//! Whether the rootfs is reached over the network, in which case the rootfs keeps the
//! network of the initrd: changing it could cut the rootfs off.
//!
//! A live rootfs that was fetched into memory is not on the network, so it may change the network.

use std::fs;
use std::path::Path;

/// File systems that are themselves on the network.
const NETWORK_FS_TYPES: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "ceph", "glusterfs"];

/// Whether the rootfs is mounted from the network, judged from the mounts of `sysroot`.
pub fn is_network_rootfs(sysroot: &Path) -> bool {
    let Ok(mountinfo) = fs::read_to_string(sysroot.join("proc/self/mountinfo")) else {
        return false;
    };
    let Some((device, fs_type)) = rootfs_mount(&mountinfo) else {
        return false;
    };

    NETWORK_FS_TYPES.contains(&fs_type)
        || is_network_block_device(&sysroot.join("sys/dev/block").join(device), 0)
}

/// The `major:minor` and the file system type of the mount on `/`, the last one if stacked.
fn rootfs_mount(mountinfo: &str) -> Option<(&str, &str)> {
    mountinfo.lines().rev().find_map(|line| {
        let (fields, rest) = line.split_once(" - ")?;
        let mut fields = fields.split(' ');
        let device = fields.nth(2)?;
        let mount_point = fields.nth(1)?;
        let fs_type = rest.split(' ').next()?;
        (mount_point == "/").then_some((device, fs_type))
    })
}

/// Whether a block device in /sys/dev/block is an NBD or iSCSI disk, or is built on one, such as an
/// LVM volume or a partition of one.
fn is_network_block_device(path: &Path, depth: usize) -> bool {
    // Device mapper stacks are shallow; the limit only guards against a loop.
    if depth > 8 {
        return false;
    }
    let Ok(path) = fs::canonicalize(path) else {
        return false;
    };

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    // An iSCSI disk sits under its session, as in .../host2/session1/target2:0:0/...
    if name.starts_with("nbd") || path.components().any(|c| is_iscsi_session(c.as_os_str())) {
        return true;
    }

    // A partition is under its disk, and a mapped device lists what it is built on in slaves/.
    let parent_is_network = path.parent().is_some_and(|parent| {
        parent.join("dev").exists() && is_network_block_device(parent, depth + 1)
    });
    parent_is_network
        || fs::read_dir(path.join("slaves")).is_ok_and(|slaves| {
            slaves
                .flatten()
                .any(|slave| is_network_block_device(&slave.path(), depth + 1))
        })
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
    use super::*;

    const MOUNTINFO: &str = "\
22 1 252:1 / / rw,relatime shared:1 - ext4 /dev/vda1 rw
23 22 0:21 / /proc rw shared:2 - proc proc rw
";

    #[test]
    fn finds_the_rootfs_mount() {
        assert_eq!(rootfs_mount(MOUNTINFO), Some(("252:1", "ext4")));
        let nfs = "1 0 0:50 / / rw - nfs4 server:/export rw\n";
        assert_eq!(rootfs_mount(nfs), Some(("0:50", "nfs4")));
        let overlay = format!("{MOUNTINFO}30 22 0:40 / / rw - overlay overlay rw\n");
        assert_eq!(rootfs_mount(&overlay), Some(("0:40", "overlay")));
    }

    #[test]
    fn tells_a_network_rootfs() {
        let dir = tempfile::tempdir().unwrap();
        let sysroot = dir.path();
        fs::create_dir_all(sysroot.join("proc/self")).unwrap();

        fs::write(sysroot.join("proc/self/mountinfo"), "1 0 0:50 / / rw - nfs4 s:/e rw\n").unwrap();
        assert!(is_network_rootfs(sysroot));

        // An NBD partition: /sys/dev/block/43:1 -> devices/virtual/block/nbd0/nbd0p1
        let disk = sysroot.join("sys/devices/virtual/block/nbd0");
        fs::create_dir_all(disk.join("nbd0p1")).unwrap();
        fs::write(disk.join("dev"), "43:0\n").unwrap();
        fs::write(disk.join("nbd0p1/dev"), "43:1\n").unwrap();
        fs::create_dir_all(sysroot.join("sys/dev/block")).unwrap();
        std::os::unix::fs::symlink(disk.join("nbd0p1"), sysroot.join("sys/dev/block/43:1"))
            .unwrap();
        fs::write(sysroot.join("proc/self/mountinfo"), "1 0 43:1 / / rw - ext4 /dev/nbd0p1 rw\n")
            .unwrap();
        assert!(is_network_rootfs(sysroot));

        // A local disk.
        let vda = sysroot.join("sys/devices/pci0000:00/0000:00:02.0/virtio1/block/vda");
        fs::create_dir_all(&vda).unwrap();
        fs::write(vda.join("dev"), "252:0\n").unwrap();
        std::os::unix::fs::symlink(&vda, sysroot.join("sys/dev/block/252:0")).unwrap();
        fs::write(sysroot.join("proc/self/mountinfo"), "1 0 252:0 / / rw - ext4 /dev/vda rw\n")
            .unwrap();
        assert!(!is_network_rootfs(sysroot));
    }
}
