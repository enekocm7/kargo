//! Content hashing helpers.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

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

pub fn sha512_file(path: &Path) -> io::Result<String> {
    hash_reader::<Sha512>(File::open(path)?)
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The checksums a Maven repository keeps next to every file, in hexadecimal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checksums {
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
    pub sha512: String,
}

impl Checksums {
    /// Each checksum with the extension of the file it is published in (`md5`, `sha1`, ...).
    pub fn files(&self) -> [(&'static str, &str); 4] {
        [("md5", &self.md5), ("sha1", &self.sha1), ("sha256", &self.sha256), ("sha512", &self.sha512)]
    }
}

/// MD5, SHA-1, SHA-256 and SHA-512 of a file, computed in one pass.
pub fn checksums_file(path: &Path) -> io::Result<Checksums> {
    checksums_reader(File::open(path)?)
}

/// MD5, SHA-1, SHA-256 and SHA-512 of `bytes`.
pub fn checksums_bytes(bytes: &[u8]) -> Checksums {
    checksums_reader(bytes).expect("reading from memory cannot fail")
}

fn checksums_reader(mut reader: impl Read) -> io::Result<Checksums> {
    let (mut md5, mut sha1, mut sha256, mut sha512) = (Md5::new(), Sha1::new(), Sha256::new(), Sha512::new());
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        md5.update(&buf[..n]);
        sha1.update(&buf[..n]);
        sha256.update(&buf[..n]);
        sha512.update(&buf[..n]);
    }
    Ok(Checksums {
        md5: hex::encode(md5.finalize()),
        sha1: hex::encode(sha1.finalize()),
        sha256: hex::encode(sha256.finalize()),
        sha512: hex::encode(sha512.finalize()),
    })
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

    /// Adds length-prefixed raw bytes.
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.hasher.update((b.len() as u64).to_le_bytes());
        self.hasher.update(b);
        self
    }

    /// Adds a file's path, size and modification time (cheap, like Cargo's mtime checks).
    /// A directory (such as a classes directory on a classpath) adds every file in it, since
    /// its own modification time doesn't change when a file inside is rewritten.
    pub fn file_stamp(&mut self, path: &Path) -> &mut Self {
        self.str(&path.to_string_lossy());
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => {
                let mut files: Vec<_> = walkdir::WalkDir::new(path)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                    .map(|e| e.into_path())
                    .collect();
                files.sort();
                for file in files {
                    self.file_stamp(&file);
                }
            }
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
    fn directories_are_stamped_by_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a/X.class"), "1").unwrap();
        let stamp = || Fingerprint::new().file_stamp(dir.path()).finish();
        let before = stamp();
        assert_eq!(before, stamp());
        std::fs::write(dir.path().join("a/X.class"), "22").unwrap();
        assert_ne!(before, stamp(), "rewriting a file inside changes the stamp");
    }

    #[test]
    fn known_digests() {
        assert_eq!(sha256_bytes(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(sha1_file(&p).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(sha256_file(&p).unwrap(), sha256_bytes(b"abc"));
        let sums = checksums_file(&p).unwrap();
        assert_eq!(sums.md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(sums.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(sums.sha256, sha256_bytes(b"abc"));
        assert!(sums.sha512.starts_with("ddaf35a193617aba"));
        assert_eq!(checksums_bytes(b"abc"), sums);
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
