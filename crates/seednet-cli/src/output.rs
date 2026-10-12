//! The files seednet writes: the network files, and the config it used with where it came from.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
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
    pub fn write_networks(&self, conversion: &Conversion) -> Result<()> {
        // Render everything before touching anything, so that a bad config leaves the old files.
        let mut warnings = conversion.warnings.clone();
        let files =
            seednet::render(&conversion.networks, self.backend, &self.prefix, &mut warnings);
        for warning in &warnings {
            crate::warn(warning);
        }

        // The files of the other backend go too, as when the rootfs uses another backend than the
        // initrd.
        self.remove_old_files(&self.networkd_dir)?;
        self.remove_old_files(&self.nm_dir)?;

        let etc_uuids = networkmanager::etc_uuids(&self.sysroot);
        for file in files {
            match file.dir {
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
                    fs::create_dir_all(&self.networkd_dir).with_context(|| {
                        format!("failed to create {}", self.networkd_dir.display())
                    })?;
                    let path = self.networkd_dir.join(&file.name);
                    fs::write(&path, &file.content)
                        .with_context(|| format!("failed to write {}", path.display()))?;
                    println!("{}", path.display());
                }
                // NetworkManager ignores a keyfile that others may read.
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
                    let path = self.nm_dir.join(&file.name);
                    write_private(&path, file.content.as_bytes())?;
                    println!("{}", path.display());
                }
            }
        }

        Ok(())
    }

    /// Removes the files with this prefix that seednet wrote, leaving any other file alone.
    fn remove_old_files(&self, dir: &Path) -> Result<()> {
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
            let ours = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(&prefix) && EXTENSIONS.iter().any(|ext| name.ends_with(ext))
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
    let dir = path.parent().context("a file without a directory")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;

    // A hidden name, which NetworkManager does not take for a keyfile while it is written.
    let name = path.file_name().context("a file without a name")?;
    let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("failed to create {}", tmp.display()))?;
    file.write_all(content)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("failed to rename to {}", path.display()))
}
