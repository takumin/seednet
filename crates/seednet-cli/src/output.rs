//! The files seednet writes: the network files, and the config it used with where it came from.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use seednet::network::HEADER;
use seednet::{Backend, Conversion, Dir};

use crate::networkmanager;

pub const DEFAULT_PREFIX: &str = "10-seednet";

/// The kinds of file seednet writes, by which it finds the ones to replace.
const EXTENSIONS: &[&str] = &[".network", ".netdev", ".link", ".nmconnection"];

/// Where the admin overrides a file of systemd-networkd or udev, as `networkctl edit` does, by
/// its name.
const NETWORKD_ETC_DIR: &str = "etc/systemd/network";

/// The config that seednet used, which the rootfs reads back and a deploy copies to
/// /etc/seednet/network-config.
pub const SAVED_CONFIG: &str = "run/seednet/network-config";

/// Where the saved config came from, for the admin to see.
pub const SAVED_SOURCE: &str = "run/seednet/source";

/// The backend the files are for, for what hands the network over from the initrd to the rootfs.
pub const BACKEND: &str = "run/seednet/backend";

/// There when the rootfs is on the network, so that nothing takes its network down.
pub const NETWORK_ROOTFS: &str = "run/seednet/network-rootfs";

pub struct Output {
    sysroot: PathBuf,
    networkd_dir: PathBuf,
    networkd_etc_dir: PathBuf,
    nm_dir: PathBuf,
    prefix: String,
    backend: Backend,
}

impl Output {
    pub fn new(sysroot: &Path, prefix: &str, backend: Backend) -> Self {
        Self {
            sysroot: sysroot.to_owned(),
            networkd_dir: sysroot.join("run/systemd/network"),
            networkd_etc_dir: sysroot.join(NETWORKD_ETC_DIR),
            nm_dir: sysroot.join("run/NetworkManager/system-connections"),
            prefix: prefix.to_owned(),
            backend,
        }
    }

    /// Replaces the files that seednet wrote before with the ones of `conversion`, for either
    /// backend, and prints the path of each.
    ///
    /// Every file is written under a hidden name first, and renamed into place once all are, so
    /// that udev and the network service never read a file half written, and a failure leaves the
    /// old files.
    pub fn write_networks(&self, conversion: &Conversion) -> Result<()> {
        // Render everything before touching anything, so that a bad config leaves the old files.
        let mut warnings = conversion.warnings.clone();
        let files =
            seednet::render(&conversion.networks, self.backend, &self.prefix, &mut warnings);
        for warning in &warnings {
            crate::warn(warning);
        }

        let etc_uuids = networkmanager::etc_uuids(&self.sysroot);
        let mut staged = Vec::new();
        for file in files {
            let (path, mode) = match file.dir {
                Dir::Networkd => {
                    // A file in /etc hides the one in /run with its name, so the file is written
                    // all the same, for when the one in /etc goes.
                    if self.networkd_etc_dir.join(&file.name).exists() {
                        crate::warn(&format!(
                            "/{NETWORKD_ETC_DIR}/{} overrides the file of seednet, so it is used \
                            instead",
                            file.name
                        ));
                    }
                    // systemd-networkd reads the files as a user of its own.
                    (self.networkd_dir.join(&file.name), 0o644)
                }
                Dir::NetworkManager => {
                    // A profile in /run hides the one in /etc with its UUID, which would undo every
                    // change made to the profile since seednet first wrote it.
                    if let Some(etc) =
                        networkmanager::keyfile_uuid(&file.content).and_then(|u| etc_uuids.get(&u))
                    {
                        crate::warn(&format!(
                            "{} has the UUID of {}, so NetworkManager keeps that instead",
                            Path::new("/")
                                .join(etc.strip_prefix(&self.sysroot)?)
                                .display(),
                            file.name
                        ));
                        continue;
                    }
                    // NetworkManager ignores a keyfile that others may read.
                    (self.nm_dir.join(&file.name), 0o600)
                }
            };
            match write_hidden(&path, file.content.as_bytes(), mode) {
                Ok(tmp) => staged.push((tmp, path)),
                Err(error) => {
                    for (tmp, _) in &staged {
                        let _ = fs::remove_file(tmp);
                    }
                    return Err(error);
                }
            }
        }

        for (tmp, path) in &staged {
            fs::rename(tmp, path)
                .with_context(|| format!("failed to rename to {}", path.display()))?;
            println!("{}", path.display());
        }

        // The files of the other backend go too, as when the rootfs uses another backend than the
        // initrd.
        let written: Vec<_> = staged.into_iter().map(|(_, path)| path).collect();
        self.remove_old_files(&self.networkd_dir, &written)?;
        self.remove_old_files(&self.nm_dir, &written)?;

        Ok(())
    }

    /// Removes the files with this prefix that seednet wrote, but those just written, leaving any
    /// other file alone.
    fn remove_old_files(&self, dir: &Path, written: &[PathBuf]) -> Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", dir.display()));
            }
        };

        let prefix = format!("{}-", self.prefix);
        for entry in entries {
            let path = entry?.path();
            let ours = !written.contains(&path)
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(&prefix)
                            && EXTENSIONS.iter().any(|ext| name.ends_with(ext))
                    })
                && fs::read_to_string(&path).is_ok_and(|content| content.starts_with(HEADER));
            if ours {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
            }
        }

        Ok(())
    }

    /// Keeps the config and where it came from. The config may hold secrets, so only root reads it.
    pub fn save(&self, text: &str, source: &str) -> Result<()> {
        write_private(&self.sysroot.join(SAVED_CONFIG), text.as_bytes())?;
        write_private(&self.sysroot.join(SAVED_SOURCE), format!("{source}\n").as_bytes())?;
        let backend = match self.backend {
            Backend::Networkd => "networkd",
            Backend::NetworkManager => "network-manager",
        };
        write_private(&self.sysroot.join(BACKEND), format!("{backend}\n").as_bytes())
    }

    /// Notes whether the rootfs is on the network.
    pub fn mark_network_rootfs(&self, network_rootfs: bool) -> Result<()> {
        let path = self.sysroot.join(NETWORK_ROOTFS);
        if network_rootfs {
            write_private(&path, b"")
        } else {
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => {
                    Err(error).with_context(|| format!("failed to remove {}", path.display()))
                }
            }
        }
    }
}

/// Writes a file readable by its owner only, replacing it in one step.
fn write_private(path: &Path, content: &[u8]) -> Result<()> {
    let tmp = write_hidden(path, content, 0o600)?;
    fs::rename(&tmp, path).with_context(|| format!("failed to rename to {}", path.display()))
}

/// Writes a file under a hidden name next to `path`, which NetworkManager does not take for a
/// keyfile, with `mode` whatever the umask, and returns that name to rename it from.
fn write_hidden(path: &Path, content: &[u8], mode: u32) -> Result<PathBuf> {
    let dir = path.parent().context("a file without a directory")?;
    if !dir.is_dir() {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to set the mode of {}", dir.display()))?;
    }

    let name = path.file_name().context("a file without a name")?;
    let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&tmp)
        .with_context(|| format!("failed to create {}", tmp.display()))?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to set the mode of {}", tmp.display()))?;
    file.write_all(content)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    Ok(tmp)
}
