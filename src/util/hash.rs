//! Content hashing helpers.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha1::Sha1;
use sha2::{Digest, Sha256};

fn hash_reader<D: Digest>(mut reader: impl Read) -> io::Result<String> {
    let mut hasher = D::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn sha1_file(path: &Path) -> io::Result<String> {
    hash_reader::<Sha1>(File::open(path)?)
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    hash_reader::<Sha256>(File::open(path)?)
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Incremental fingerprint builder used for up-to-date checks.
#[derive(Default)]
pub struct Fingerprint {
    hasher: Sha256,
}

impl Fingerprint {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a length-prefixed string so that `["ab","c"]` and `["a","bc"]` differ.
    pub fn str(&mut self, s: &str) -> &mut Self {
        self.hasher.update((s.len() as u64).to_le_bytes());
        self.hasher.update(s.as_bytes());
        self
    }

    /// Adds a file's path, size and modification time (cheap, like Cargo's mtime checks).
    pub fn file_stamp(&mut self, path: &Path) -> &mut Self {
        self.str(&path.to_string_lossy());
        match std::fs::metadata(path) {
            Ok(meta) => {
                self.hasher.update(meta.len().to_le_bytes());
                let mtime =
                    meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
                self.hasher.update(mtime.to_le_bytes());
            }
            Err(_) => {
                self.hasher.update(b"missing");
            }
        }
        self
    }

    pub fn finish(&self) -> String {
        hex::encode(self.hasher.clone().finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digests() {
        assert_eq!(sha256_bytes(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(sha1_file(&p).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(sha256_file(&p).unwrap(), sha256_bytes(b"abc"));
    }

    #[test]
    fn fingerprint_is_length_prefixed() {
        let a = Fingerprint::new().str("ab").str("c").finish();
        let b = Fingerprint::new().str("a").str("bc").finish();
        assert_ne!(a, b);
        assert_eq!(a, Fingerprint::new().str("ab").str("c").finish());
    }

    #[test]
    fn fingerprint_tracks_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let missing = Fingerprint::new().file_stamp(&p).finish();
        std::fs::write(&p, b"1").unwrap();
        let one = Fingerprint::new().file_stamp(&p).finish();
        assert_ne!(missing, one);
        std::fs::write(&p, b"22").unwrap();
        assert_ne!(one, Fingerprint::new().file_stamp(&p).finish());
    }
}
