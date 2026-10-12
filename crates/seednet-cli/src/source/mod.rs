//! Finding the network-config for this boot, by the order of its sources.

mod cidata;
mod fetch;
pub mod netrootfs;

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use seednet::cmdline::Params;

use crate::output::{SAVED_CONFIG, SAVED_SOURCE};
use crate::{Stage, info, warn};

/// The config that the admin put in the rootfs, which wins over everything but the command line.
const ETC_CONFIG: &str = "etc/seednet/network-config";

/// How long to keep trying to fetch `seednet.url=` unless `seednet.url-timeout=` says otherwise.
const DEFAULT_URL_TIMEOUT: Duration = Duration::from_secs(60);

/// Where dracut keeps the arguments that it adds to the kernel command line, as those that
/// `dracut --kernel-cmdline` puts in the initrd.
const CMDLINE_D: &str = "etc/cmdline.d";

pub struct Options {
    pub sysroot: PathBuf,
    pub stage: Stage,
    /// The kernel command line to read instead of the one of the system.
    pub cmdline: Option<PathBuf>,
    pub cidata_dir: Option<PathBuf>,
    pub ca_file: Option<PathBuf>,
}

/// The config for this boot and where it came from, or none.
pub fn find(options: &Options, params: &Params) -> Result<Option<(String, String)>> {
    let sysroot = &options.sysroot;
    let etc_config = sysroot.join(ETC_CONFIG);

    if let Some(text) = &params.network_config {
        let mut overridden = Vec::new();
        if options.stage == Stage::Rootfs && etc_config.is_file() {
            overridden.push(format!("/{ETC_CONFIG}"));
        }
        if cidata_present(options) {
            overridden.push("the CIDATA disk".to_owned());
        }
        if !overridden.is_empty() {
            warn(&format!(
                "seednet.network-config on the kernel command line overrides {}",
                overridden.join(" and ")
            ));
        }
        return Ok(Some((text.clone(), "the kernel command line".to_owned())));
    }

    match options.stage {
        Stage::Rootfs => {
            let network_rootfs = netrootfs::is_network_rootfs(sysroot);
            if network_rootfs {
                if etc_config.is_file() {
                    warn(&format!(
                        "the rootfs is on the network, so it keeps the network of the initrd and \
                        ignores /{ETC_CONFIG}"
                    ));
                }
            } else if let Some(text) = read_optional(&etc_config)? {
                return Ok(Some((text, format!("/{ETC_CONFIG}"))));
            }

            if let Some(text) = read_optional(&sysroot.join(SAVED_CONFIG))? {
                let source = read_optional(&sysroot.join(SAVED_SOURCE))?
                    .map(|source| source.trim().to_owned())
                    .unwrap_or_else(|| "the initrd".to_owned());
                info(&format!("keeping the network-config that the initrd used, from {source}"));
                return Ok(Some((text, source)));
            }
            if network_rootfs {
                return Ok(None);
            }
            if params.url.is_some() {
                warn("seednet.url is fetched in the initrd only, and the initrd fetched nothing");
            }
        }
        Stage::Initrd => {
            if let Some(url) = &params.url {
                let timeout = params.url_timeout.unwrap_or(DEFAULT_URL_TIMEOUT);
                let text = fetch::fetch(
                    url,
                    params.checksum.as_ref(),
                    timeout,
                    sysroot,
                    options.ca_file.as_deref(),
                )?;
                return Ok(Some((text, fetch::redact(url))));
            }
        }
    }

    let text = match &options.cidata_dir {
        Some(dir) => cidata::read_dir(dir)?,
        None => cidata::read(sysroot, params.cidata_timeout.unwrap_or(cidata::DEFAULT_TIMEOUT))?,
    };
    Ok(text.map(|text| (text, "the CIDATA disk".to_owned())))
}

/// The kernel command line: /proc/cmdline, after the files of /etc/cmdline.d in an initrd, as
/// dracut reads it, so that the kernel has the last word.
pub fn read_cmdline(options: &Options) -> Result<String> {
    let read = |path: &Path| {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
    };
    if let Some(cmdline) = &options.cmdline {
        return read(cmdline);
    }

    let mut cmdline = String::new();
    if options.stage == Stage::Initrd {
        let mut files: Vec<_> = match fs::read_dir(options.sysroot.join(CMDLINE_D)) {
            Ok(entries) => entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<_, _>>()
                .with_context(|| format!("failed to read /{CMDLINE_D}"))?,
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read /{CMDLINE_D}"));
            }
        };
        files.retain(|path| path.extension().is_some_and(|ext| ext == "conf"));
        files.sort();
        for file in files {
            cmdline.push_str(&read(&file)?);
            cmdline.push(' ');
        }
    }
    cmdline.push_str(&read(&options.sysroot.join("proc/cmdline"))?);
    Ok(cmdline)
}

fn cidata_present(options: &Options) -> bool {
    match &options.cidata_dir {
        Some(dir) => dir.join("network-config").is_file(),
        None => cidata::present(&options.sysroot),
    }
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}
