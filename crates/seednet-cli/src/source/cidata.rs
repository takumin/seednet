//! The network-config on the CIDATA disk of the NoCloud datasource.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rustix::mount::{MountFlags, UnmountFlags};

/// The labels cloud-init looks for, in its order.
const LABELS: &[&str] = &["cidata", "CIDATA"];

/// The file systems cloud-init reads the disk as.
const FS_TYPES: &[&str] = &["vfat", "iso9660"];

/// Where the disk is mounted while it is read.
const MOUNT_POINT: &str = "run/seednet/cidata";

/// How long to wait for the disk unless `rd.seednet.timeout=` says otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Whether the disk is there now, without waiting for it.
pub fn present(sysroot: &Path) -> bool {
    device(sysroot).is_some()
}

/// Waits up to `timeout` for the disk, and reads its network-config. Neither the disk nor the file
/// being there is an error.
pub fn read(sysroot: &Path, timeout: Duration) -> Result<Option<String>> {
    let deadline = Instant::now() + timeout;
    let device = loop {
        if let Some(device) = device(sysroot) {
            break device;
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(POLL_INTERVAL);
    };

    let mount_point = sysroot.join(MOUNT_POINT);
    fs::create_dir_all(&mount_point)
        .with_context(|| format!("failed to create {}", mount_point.display()))?;
    mount(&device, &mount_point)?;

    let text = read_dir(&mount_point);
    rustix::mount::unmount(&mount_point, UnmountFlags::empty())
        .with_context(|| format!("failed to unmount {}", mount_point.display()))?;
    // Only an empty directory goes, so this never takes the files of a mount that is still there.
    let _ = fs::remove_dir(&mount_point);
    text
}

/// Reads the network-config of a directory with the CIDATA files.
pub fn read_dir(dir: &Path) -> Result<Option<String>> {
    let path = dir.join("network-config");
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn device(sysroot: &Path) -> Option<PathBuf> {
    LABELS
        .iter()
        .map(|label| sysroot.join("dev/disk/by-label").join(label))
        .find(|path| path.exists())
}

fn mount(device: &Path, mount_point: &Path) -> Result<()> {
    let flags = MountFlags::RDONLY | MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC;
    let mut errors = Vec::new();
    for fs_type in FS_TYPES {
        match rustix::mount::mount(device, mount_point, *fs_type, flags, None) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!("{fs_type}: {error}")),
        }
    }
    anyhow::bail!(
        "failed to mount {} on {} ({})",
        device.display(),
        mount_point.display(),
        errors.join(", ")
    )
}
