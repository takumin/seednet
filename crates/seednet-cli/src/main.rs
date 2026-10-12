mod cloud_init;
mod netplan;
mod networkmanager;
mod output;
mod source;
mod takeover;

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use seednet::cmdline::Params;

/// Converts a NoCloud network-config to systemd-networkd or NetworkManager files.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// The directory that every path is relative to, `/` but in tests and image builds.
    #[arg(long, global = true, default_value = "/")]
    sysroot: PathBuf,

    /// The prefix of the file names, which decides where they sort among the other files.
    #[arg(long, global = true, default_value = output::DEFAULT_PREFIX)]
    prefix: String,

    /// The service that brings up the network, which decides the files to write. `auto` is
    /// NetworkManager in a rootfs where it is enabled, and networkd otherwise.
    #[arg(long, global = true, value_enum, default_value = "networkd")]
    backend: BackendArg,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Converts one network-config and writes the files.
    ///
    /// Prints the path of each file written. An empty or disabled config writes nothing.
    Convert {
        /// The network-config to convert, or `-` for standard input.
        input: PathBuf,
    },

    /// Finds the network-config for this boot, converts it and writes the files.
    ///
    /// The config comes from the first of: `seednet.network-config=` on the kernel command line,
    /// /etc/seednet/network-config (rootfs only), the config the initrd used (rootfs only), the
    /// NoCloud seed at `seednet.url=` (initrd only), and the CIDATA disk. Without any, nothing is
    /// written. The rootfs hands the seed that the initrd fetched to cloud-init.
    Apply {
        /// Where this runs, which decides the sources to look at.
        #[arg(long, value_enum)]
        stage: Stage,

        /// The kernel command line to read, relative to the sysroot, instead of proc/cmdline and,
        /// in an initrd, the files of dracut in etc/cmdline.d.
        #[arg(long)]
        cmdline: Option<PathBuf>,

        /// A directory with the CIDATA files, instead of finding and mounting the disk.
        #[arg(long)]
        cidata_dir: Option<PathBuf>,

        /// The CA certificates for an https URL, instead of the system bundle.
        #[arg(long)]
        ca_file: Option<PathBuf>,

        /// Keeps cloud-init, netplan and NetworkManager from configuring the network themselves,
        /// for this boot, when a config is found.
        #[arg(long)]
        takeover: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BackendArg {
    /// NetworkManager in an initrd that has it and in a rootfs where NetworkManager.service is
    /// enabled, networkd otherwise.
    Auto,
    /// systemd-networkd: .network and .netdev files in /run/systemd/network.
    Networkd,
    /// NetworkManager: keyfiles in /run/NetworkManager/system-connections.
    #[value(alias = "nm")]
    NetworkManager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Stage {
    Initrd,
    Rootfs,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let stage = match args.command {
        Command::Apply { stage, .. } => Some(stage),
        Command::Convert { .. } => None,
    };
    let backend = match args.backend {
        BackendArg::Networkd => seednet::Backend::Networkd,
        BackendArg::NetworkManager => seednet::Backend::NetworkManager,
        BackendArg::Auto => detect_backend(&args.sysroot, stage),
    };
    let output = output::Output::new(&args.sysroot, &args.prefix, backend);

    match args.command {
        Command::Convert { input } => {
            let text = if input.as_os_str() == "-" {
                let mut text = String::new();
                io::stdin()
                    .read_to_string(&mut text)
                    .context("failed to read standard input")?;
                text
            } else {
                fs::read_to_string(&input)
                    .with_context(|| format!("failed to read {}", input.display()))?
            };
            let conversion = seednet::convert(&text)
                .with_context(|| format!("failed to convert {}", input.display()))?;
            output.write_networks(&conversion)?;
        }
        Command::Apply {
            stage,
            cmdline,
            cidata_dir,
            ca_file,
            takeover,
        } => {
            let options = source::Options {
                sysroot: args.sysroot.clone(),
                stage,
                cmdline: cmdline.map(|cmdline| args.sysroot.join(cmdline)),
                cidata_dir,
                ca_file,
                backend,
            };
            if apply(&options, &output)? && takeover {
                takeover::take_over(&args.sysroot, stage, backend)?;
                info("took over the network from cloud-init, netplan and NetworkManager");
            }
        }
    }

    Ok(())
}

/// Finds the config, converts it and writes the files, and tells whether it found one. Finding none
/// leaves the network alone.
fn apply(options: &source::Options, output: &output::Output) -> Result<bool> {
    let cmdline = source::read_cmdline(options)?;
    // The rootfs fetches nothing, so it need not read how to.
    let params = Params::parse(&cmdline, options.stage == Stage::Initrd)
        .context("invalid kernel command line")?;
    if options.stage == Stage::Rootfs {
        output.mark_network_rootfs(source::netrootfs::is_network_rootfs(&options.sysroot))?;
        output.install_seed()?;
    }

    let Some((text, source)) = source::find(options, &params, output)? else {
        info("no network-config for this boot, so the network is left alone");
        return Ok(false);
    };
    info(&format!("using the network-config from {source}"));

    let conversion = seednet::convert(&text)
        .with_context(|| format!("failed to convert the network-config from {source}"))?;
    // An empty or disabled config says nothing of the network, as cloud-init takes it, and taking
    // the network over for it would leave none.
    if conversion.networks.is_empty() {
        for warning in &conversion.warnings {
            warn(warning);
        }
        info(&format!(
            "the network-config from {source} configures no interface, so the network is left alone"
        ));
        return Ok(false);
    }
    output.write_networks(&conversion)?;
    output.save(&text, &source)?;
    Ok(true)
}

/// The backend of `auto`: NetworkManager in an initrd that has it, as dracut puts it there, and in
/// a rootfs where it is enabled, and networkd otherwise.
fn detect_backend(sysroot: &Path, stage: Option<Stage>) -> seednet::Backend {
    let nm = if stage == Some(Stage::Initrd) {
        networkmanager::in_initrd(sysroot)
    } else {
        networkmanager::enabled(sysroot)
    };
    if nm {
        seednet::Backend::NetworkManager
    } else {
        seednet::Backend::Networkd
    }
}

/// Reports what seednet does, for the journal of the boot.
pub fn info(message: &str) {
    eprintln!("seednet: {message}");
}

/// Reports what may not be what the admin meant.
pub fn warn(message: &str) {
    eprintln!("seednet: warning: {message}");
}
