//! The `algorithm:hex` checksum that a fetched network-config must match.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use sha2::Digest as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl Algorithm {
    fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Md5 => md5::Md5::digest(data).to_vec(),
            Self::Sha1 => sha1::Sha1::digest(data).to_vec(),
            Self::Sha256 => sha2::Sha256::digest(data).to_vec(),
            Self::Sha512 => sha2::Sha512::digest(data).to_vec(),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Md5 => "md5",
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    fn len(self) -> usize {
        match self {
            Self::Md5 => 16,
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha512 => 64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checksum {
    pub algorithm: Algorithm,
    pub digest: Vec<u8>,
}

impl Checksum {
    /// Fails unless `data` hashes to this checksum.
    pub fn verify(&self, data: &[u8]) -> Result<()> {
        let actual = self.algorithm.digest(data);
        if actual != self.digest {
            bail!(
                "checksum mismatch: expected {self}, got {}:{}",
                self.algorithm.name(),
                hex(&actual)
            );
        }
        Ok(())
    }
}

impl FromStr for Checksum {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (name, digest) = s
            .split_once(':')
            .with_context(|| format!("not algorithm:hex: {s}"))?;
        let algorithm = match name.to_ascii_lowercase().as_str() {
            "md5" => Algorithm::Md5,
            "sha1" => Algorithm::Sha1,
            "sha256" => Algorithm::Sha256,
            "sha512" => Algorithm::Sha512,
            _ => bail!("unsupported algorithm: {name}"),
        };

        let digest = unhex(digest).with_context(|| format!("invalid hex: {digest}"))?;
        if digest.len() != algorithm.len() {
            bail!("a {name} digest is {} bytes, not {}", algorithm.len(), digest.len());
        }

        Ok(Self { algorithm, digest })
    }
}

impl fmt::Display for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm.name(), hex(&self.digest))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_each_algorithm() {
        for (checksum, data) in [
            ("md5:900150983cd24fb0d6963f7d28e17f72", "abc"),
            ("sha1:a9993e364706816aba3e25717850c26c9cd0d89d", "abc"),
            ("SHA256:BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD", "abc"),
            (
                "sha512:ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
                2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
                "abc",
            ),
        ] {
            let checksum: Checksum = checksum.parse().unwrap();
            checksum.verify(data.as_bytes()).unwrap();
            assert!(checksum.verify(b"abd").is_err());
        }
    }

    #[test]
    fn rejects_bad_checksums() {
        assert!("sha256".parse::<Checksum>().is_err());
        assert!("crc32:00000000".parse::<Checksum>().is_err());
        assert!(
            "md5:900150983cd24fb0d6963f7d28e17f7"
                .parse::<Checksum>()
                .is_err()
        );
        assert!(
            "md5:900150983cd24fb0d6963f7d28e17fzz"
                .parse::<Checksum>()
                .is_err()
        );
        assert!(
            "md5:é00150983cd24fb0d6963f7d28e17f7"
                .parse::<Checksum>()
                .is_err()
        );
    }
}
