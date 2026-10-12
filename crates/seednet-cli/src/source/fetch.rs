//! Fetching the NoCloud seed from `seednet.url=`, over the network the initrd brought up for it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use seednet::checksum::Checksum;
use seednet::cmdline::MAX_CONFIG_SIZE;
use seednet::template::{Names, Template, Var};
use ureq::tls::{PemItem, RootCerts, TlsConfig, TlsProvider};

/// The system CA bundles of the common distributions, in the order they are looked for.
const CA_BUNDLES: &[&str] = &[
    "etc/ssl/certs/ca-certificates.crt",
    "etc/pki/tls/certs/ca-bundle.crt",
    "etc/ssl/ca-bundle.pem",
    "etc/ssl/cert.pem",
];

/// How long one attempt may take.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait between attempts, while the network comes up.
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// The most that a file of the seed but the network-config may take, so that a bad one cannot fill
/// the memory of the initrd.
const MAX_SEED_FILE_SIZE: usize = 16 << 20;

/// The files of the seed, as cloud-init reads them from `ds=nocloud;s=`: whether the seed must
/// have it, and the most it may take.
pub const SEED_FILES: &[(&str, bool, usize)] = &[
    ("meta-data", true, MAX_SEED_FILE_SIZE),
    ("user-data", true, MAX_SEED_FILE_SIZE),
    ("vendor-data", false, MAX_SEED_FILE_SIZE),
    ("network-config", false, MAX_CONFIG_SIZE),
];

/// The seed fetched, with the URL it came from.
pub struct Seed {
    /// The URL of the seed, with the variables expanded and the secrets left out.
    pub url: String,
    /// The files that the seed has, by name.
    pub files: Vec<(&'static str, Vec<u8>)>,
}

impl Seed {
    /// The network-config, if the seed has one.
    pub fn network_config(&self) -> Result<Option<String>> {
        self.files
            .iter()
            .find(|(name, _)| *name == "network-config")
            .map(|(_, body)| {
                String::from_utf8(body.clone())
                    .with_context(|| format!("the network-config of {} is not UTF-8", self.url))
            })
            .transpose()
    }
}

/// Fetches the seed, trying again until `timeout` runs out while the failure may be the network
/// still coming up, or DHCP not having given the names of the template yet. Any other failure,
/// such as a bad certificate or a checksum mismatch, fails at once.
pub fn fetch(
    template: &Template,
    checksum: Option<&Checksum>,
    timeout: Duration,
    sysroot: &Path,
    ca_file: Option<&Path>,
    names: impl Fn() -> Names,
) -> Result<Seed> {
    // A timeout too long for the clock is no deadline at all.
    let deadline = Instant::now().checked_add(timeout);
    let time_left = || deadline.is_none_or(|deadline| Instant::now() + RETRY_INTERVAL < deadline);

    let mut waiting = false;
    let base = loop {
        match template.expand(&names()) {
            Ok(url) => break url,
            Err(missing) if time_left() => {
                if !waiting {
                    crate::info(&format!("waiting for DHCP to give {}", describe(&missing)));
                    waiting = true;
                }
                thread::sleep(RETRY_INTERVAL);
            }
            Err(missing) => bail!(
                "DHCP did not give {} of seednet.url within {}s",
                describe(&missing),
                timeout.as_secs()
            ),
        }
    };

    let agent = agent(&base, sysroot, ca_file)?;
    // The URL may carry a secret, which the console and the journal must not.
    let shown = redact(&base);
    let mut files = Vec::new();
    for &(name, required, limit) in SEED_FILES {
        // As cloud-init reads them, the names follow the URL, which ends in a slash.
        let url = format!("{base}{name}");
        if let Some(body) = get_retrying(&agent, &url, required, limit, deadline, timeout)? {
            files.push((name, body));
        }
    }
    let seed = Seed { url: shown, files };

    if let Some(checksum) = checksum {
        let config = seed
            .files
            .iter()
            .find(|(name, _)| *name == "network-config")
            .with_context(|| {
                format!("seednet.checksum is given, but {} has no network-config", seed.url)
            })?;
        checksum.verify(&config.1).with_context(|| {
            format!("the network-config of {} is not the expected one", seed.url)
        })?;
    }

    Ok(seed)
}

/// The variables that have no value, for a message.
fn describe(missing: &[Var]) -> String {
    missing
        .iter()
        .map(|var| {
            let what = match var {
                Var::Hostname => "the host name",
                Var::Domainname => "the domain",
                Var::Fqdn => "the host name and the domain",
            };
            format!("{what} for {var}")
        })
        .collect::<Vec<_>>()
        .join(", and ")
}

/// Fetches one file, trying again while the failure may be the network still coming up. A file
/// that the seed need not have is none when the server says so.
fn get_retrying(
    agent: &ureq::Agent,
    url: &str,
    required: bool,
    limit: usize,
    deadline: Option<Instant>,
    timeout: Duration,
) -> Result<Option<Vec<u8>>> {
    let shown = redact(url);
    let mut last_message = String::new();
    loop {
        // An attempt takes no longer than the time left, so the fetch ends by the deadline.
        let attempt_timeout = deadline.map_or(ATTEMPT_TIMEOUT, |deadline| {
            ATTEMPT_TIMEOUT
                .min(deadline.saturating_duration_since(Instant::now()))
                .max(RETRY_INTERVAL)
        });
        let time_left = deadline.is_none_or(|deadline| Instant::now() + RETRY_INTERVAL < deadline);
        match get(agent, url, attempt_timeout, limit) {
            Ok(body) => return Ok(Some(body)),
            Err(ureq::Error::StatusCode(404)) if !required => return Ok(None),
            Err(error) if is_transient(&error) && time_left => {
                // Say it once, not every second, unless the failure changes.
                let message = error.to_string();
                if message != last_message {
                    crate::info(&format!("failed to fetch {shown}, trying again: {message}"));
                    last_message = message;
                }
                thread::sleep(RETRY_INTERVAL);
            }
            Err(error) if is_transient(&error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("failed to fetch {shown} within {}s", timeout.as_secs())));
            }
            Err(error) => {
                return Err(anyhow::Error::new(error).context(format!("failed to fetch {shown}")));
            }
        }
    }
}

/// The URL with the user and password, and the query, which may hold a token, left out.
pub fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, rest) = rest.split_at(end);
    let host = match authority.rsplit_once('@') {
        Some((_, host)) => format!("***@{host}"),
        None => authority.to_owned(),
    };
    let path = rest.split('#').next().unwrap_or_default();
    let path = match path.split_once('?') {
        Some((path, _)) => format!("{path}?***"),
        None => path.to_owned(),
    };
    format!("{scheme}://{host}{path}")
}

fn get(
    agent: &ureq::Agent,
    url: &str,
    timeout: Duration,
    limit: usize,
) -> Result<Vec<u8>, ureq::Error> {
    agent
        .get(url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .call()?
        .body_mut()
        .with_config()
        .limit(limit as u64)
        .read_to_vec()
}

/// Whether trying again may help: the network or the server is not up yet, or is busy.
fn is_transient(error: &ureq::Error) -> bool {
    use std::io::ErrorKind;

    match error {
        ureq::Error::StatusCode(status) => *status == 408 || *status == 429 || *status >= 500,
        // A host name that does not resolve yet, as when the name servers come with the network
        // that is still coming up: the standard library tells it only by its message.
        ureq::Error::Io(error)
            if error
                .to_string()
                .starts_with("failed to lookup address information") =>
        {
            true
        }
        ureq::Error::Io(error) => matches!(
            error.kind(),
            ErrorKind::ConnectionRefused
                | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::NotConnected
                | ErrorKind::TimedOut
                | ErrorKind::HostUnreachable
                | ErrorKind::NetworkUnreachable
                | ErrorKind::NetworkDown
                | ErrorKind::AddrNotAvailable
                | ErrorKind::UnexpectedEof
                | ErrorKind::Interrupted
        ),
        ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::BodyStalled => true,
        _ => false,
    }
}

fn agent(url: &str, sysroot: &Path, ca_file: Option<&Path>) -> Result<ureq::Agent> {
    let mut config = ureq::Agent::config_builder()
        // The config should come straight from where the admin pointed, not from a proxy that the
        // environment of the initrd happens to name.
        .proxy(None);

    let scheme = url
        .split_once("://")
        .map(|(scheme, _)| scheme.to_ascii_lowercase());
    if scheme.as_deref() == Some("https") {
        // A redirect to plain http would hand over a config that no certificate vouches for.
        config = config.https_only(true);
        let ca_file = match ca_file {
            Some(ca_file) => ca_file.to_owned(),
            None => system_ca_bundle(sysroot)?,
        };
        config = config.tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                .unversioned_rustls_crypto_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .root_certs(RootCerts::Specific(Arc::new(load_certs(&ca_file)?)))
                .build(),
        );
    } else if scheme.as_deref() != Some("http") {
        bail!("seednet.url is neither http:// nor https://: {}", redact(url));
    }

    Ok(config.build().new_agent())
}

fn system_ca_bundle(sysroot: &Path) -> Result<PathBuf> {
    CA_BUNDLES
        .iter()
        .map(|bundle| sysroot.join(bundle))
        .find(|path| path.is_file())
        .with_context(|| {
            format!(
                "an https seednet.url needs a CA bundle, but there is none of /{}",
                CA_BUNDLES.join(", /")
            )
        })
}

fn load_certs(path: &Path) -> Result<Vec<ureq::tls::Certificate<'static>>> {
    let pem = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let certs: Vec<_> = ureq::tls::parse_pem(&pem)
        .filter_map(|item| match item {
            Ok(PemItem::Certificate(cert)) => Some(Ok(cert)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<_, _>>()
        .with_context(|| format!("invalid PEM in {}", path.display()))?;
    if certs.is_empty() {
        bail!("no certificate in {}", path.display());
    }
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_does_not_resolve_yet_is_transient() {
        let error = std::io::Error::other("failed to lookup address information: Try again");
        assert!(is_transient(&ureq::Error::Io(error)));
        let error = std::io::Error::other("invalid certificate");
        assert!(!is_transient(&ureq::Error::Io(error)));
    }

    #[test]
    fn leaves_the_secrets_of_a_url_out() {
        assert_eq!(redact("http://a/b"), "http://a/b");
        assert_eq!(redact("https://u:p@a:8443/b?sig=x#f"), "https://***@a:8443/b?***");
        assert_eq!(redact("http://u@a?t=1"), "http://***@a?***");
    }

    #[test]
    fn an_https_url_follows_no_redirect_to_http() {
        let dir = tempfile::tempdir().unwrap();
        let ca_file = dir.path().join("ca.pem");
        fs::write(&ca_file, TEST_CA).unwrap();

        let https = agent("HTTPS://example.com/", dir.path(), Some(&ca_file)).unwrap();
        assert!(https.config().https_only());
        let http = agent("Http://example.com/", dir.path(), None).unwrap();
        assert!(!http.config().https_only());
        assert!(agent("ftp://example.com/", dir.path(), None).is_err());
    }

    /// A self-signed certificate, which is only parsed here.
    const TEST_CA: &str = include_str!("../../tests/fixtures/test-ca.pem");
}
