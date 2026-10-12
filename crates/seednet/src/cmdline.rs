//! The seednet parameters on the kernel command line.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use crate::checksum::Checksum;

/// The most a network-config may take, decoded or fetched, so that a bad one cannot fill the
/// memory of the initrd.
pub const MAX_CONFIG_SIZE: usize = 1 << 20;

/// The seednet parameters, each from its last occurrence on the command line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Params {
    /// `seednet.network-config=`: the config itself, decoded from base64 and gunzipped if need be.
    pub network_config: Option<String>,
    /// `seednet.url=`: where to fetch the config from.
    pub url: Option<String>,
    /// `seednet.checksum=`: what the fetched config must hash to.
    pub checksum: Option<Checksum>,
    /// `seednet.url-timeout=`: how long to keep trying to fetch the config, in seconds.
    pub url_timeout: Option<Duration>,
    /// `rd.seednet.timeout=`: how long to wait for the CIDATA disk, in seconds.
    pub cidata_timeout: Option<Duration>,
}

impl Params {
    /// Parses the command line, as in `/proc/cmdline`. Only the last occurrence of a parameter
    /// counts, so only it must be valid. Without `fetch`, as in the rootfs, which fetches nothing,
    /// the parameters of the fetch are not read beyond the URL.
    pub fn parse(cmdline: &str, fetch: bool) -> Result<Self> {
        let mut last = std::collections::HashMap::new();
        for arg in split(cmdline) {
            if let Some((key, value)) = arg.split_once('=') {
                // As systemd takes its own, `-` and `_` are the same in a name.
                last.insert(key.replace('_', "-"), value.to_owned());
            }
        }
        let get = |key: &str| last.get(key).map(String::as_str);

        let mut params = Self {
            network_config: get("seednet.network-config")
                .map(|value| decode(value).context("invalid seednet.network-config"))
                .transpose()?,
            // An empty one undoes one before it, as for a boot that should not fetch.
            url: get("seednet.url")
                .filter(|url| !url.is_empty())
                .map(str::to_owned),
            cidata_timeout: get("rd.seednet.timeout")
                .map(|value| seconds(value).context("invalid rd.seednet.timeout"))
                .transpose()?,
            ..Self::default()
        };
        if fetch {
            params.checksum = get("seednet.checksum")
                .map(|value| value.parse().context("invalid seednet.checksum"))
                .transpose()?;
            params.url_timeout = get("seednet.url-timeout")
                .map(|value| seconds(value).context("invalid seednet.url-timeout"))
                .transpose()?;
            if params.checksum.is_some() && params.url.is_none() {
                bail!("seednet.checksum is given without seednet.url");
            }
        }

        Ok(params)
    }
}

/// Splits the command line into arguments the way the kernel does: on whitespace outside double
/// quotes, which are dropped, and up to `--`, after which the arguments are for init.
fn split(cmdline: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut arg = String::new();
    let mut quoted = false;
    let mut in_arg = false;

    for c in cmdline.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                in_arg = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_arg {
                    if arg == "--" {
                        return args;
                    }
                    args.push(std::mem::take(&mut arg));
                    in_arg = false;
                }
            }
            c => {
                arg.push(c);
                in_arg = true;
            }
        }
    }
    if in_arg && arg != "--" {
        args.push(arg);
    }

    args
}

/// Decodes `seednet.network-config=`: base64, padded or not, of the config or of its gzip.
pub fn decode(value: &str) -> Result<String> {
    const ENGINE: GeneralPurpose = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );

    // The URL-safe alphabet too, which has no characters that a boot loader may take apart.
    let value: String = value
        .trim()
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    let bytes = ENGINE.decode(value).context("invalid base64")?;
    let bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
        gunzip(&bytes)?
    } else {
        bytes
    };
    if bytes.len() > MAX_CONFIG_SIZE {
        bail!("the config is larger than {MAX_CONFIG_SIZE} bytes");
    }

    String::from_utf8(bytes).context("the config is not UTF-8")
}

fn gunzip(bytes: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read as _;

    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(bytes)
        .take(MAX_CONFIG_SIZE as u64 + 1)
        .read_to_end(&mut out)
        .context("invalid gzip")?;
    Ok(out)
}

fn seconds(value: &str) -> Result<Duration> {
    Ok(Duration::from_secs(
        value
            .parse()
            .with_context(|| format!("not a number of seconds: {value}"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_the_kernel() {
        assert_eq!(
            split(r#"  root=/dev/vda ro "a b=c d" e="f g"   x -- seednet.url=y"#),
            ["root=/dev/vda", "ro", "a b=c d", "e=f g", "x"]
        );
        assert_eq!(split("a\n"), ["a"]);
        assert!(split("").is_empty());
    }

    #[test]
    fn parses_the_seednet_params() {
        let empty_sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let params = Params::parse(
            &format!(
                "ro seednet.url=http://a/1 seednet.url=http://a/2 \
            seednet.checksum=sha256:{empty_sha256} seednet.url-timeout=30 rd.seednet.timeout=3 \
            seednet.network-config=dmVyc2lvbjogMgo\n",
            ),
            true,
        )
        .unwrap();

        assert_eq!(params.url.as_deref(), Some("http://a/2"));
        assert!(params.checksum.is_some());
        assert_eq!(params.url_timeout, Some(Duration::from_secs(30)));
        assert_eq!(params.cidata_timeout, Some(Duration::from_secs(3)));
        assert_eq!(params.network_config.as_deref(), Some("version: 2\n"));

        let params = Params::parse("seednet.url=http://a/1 seednet.url=", true).unwrap();
        assert_eq!(params.url, None);
    }

    #[test]
    fn rejects_bad_params() {
        assert!(Params::parse("seednet.network-config=!!!", true).is_err());
        assert!(Params::parse("seednet.url-timeout=soon", true).is_err());
        assert!(Params::parse("seednet.checksum=sha256:00", true).is_err());
        let md5 = "seednet.checksum=md5:d41d8cd98f00b204e9800998ecf8427e";
        assert!(Params::parse(md5, true).is_err());

        // What is overridden, or what the rootfs does not fetch with, need not be valid.
        assert!(Params::parse("seednet.url-timeout=soon seednet.url-timeout=5", true).is_ok());
        assert!(Params::parse("seednet.url-timeout=soon", false).is_ok());
        assert!(Params::parse(md5, false).is_ok());
    }

    #[test]
    fn takes_names_with_underscores_too() {
        let params = Params::parse("seednet.network_config=dmVyc2lvbjogMgo", true).unwrap();
        assert_eq!(params.network_config.as_deref(), Some("version: 2\n"));
    }

    #[test]
    fn decodes_gzip() {
        use std::io::Write as _;

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(b"version: 1\n").unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(gz.finish().unwrap());

        assert_eq!(decode(&encoded).unwrap(), "version: 1\n");

        // Of several members, as from gzip of files one after another.
        let member = |text: &[u8]| {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gz.write_all(text).unwrap();
            gz.finish().unwrap()
        };
        let members = [member(b"version: 2\n"), member(b"ethernets: {}\n")].concat();
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(members);
        assert_eq!(decode(&encoded).unwrap(), "version: 2\nethernets: {}\n");
    }

    #[test]
    fn rejects_a_gzip_bomb() {
        use std::io::Write as _;

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(&vec![b'#'; MAX_CONFIG_SIZE + 1]).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(gz.finish().unwrap());

        assert!(decode(&encoded).is_err());
    }
}
