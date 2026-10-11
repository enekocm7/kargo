//! Extracting toolchain archives and installing them atomically.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Archive formats used by toolchain distributions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    TarGz,
}

impl ArchiveKind {
    /// Infers the format from a file name.
    pub fn from_name(name: &str) -> Result<Self> {
        if name.ends_with(".zip") {
            Ok(ArchiveKind::Zip)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Ok(ArchiveKind::TarGz)
        } else {
            bail!("unsupported archive format: {name}")
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ArchiveKind::Zip => "zip",
            ArchiveKind::TarGz => "tar.gz",
        }
    }
}

/// Extracts `archive` and moves its top-level directory to `dest`.
///
/// Extraction happens in a private staging directory next to `dest`, so a
/// crash never leaves a half-installed toolchain, and concurrent installs of
/// the same toolchain are safe: the first one to finish wins.
pub fn install(archive: &Path, kind: ArchiveKind, dest: &Path) -> Result<()> {
    let parent = dest.parent().context("install destination has no parent")?;
    fs::create_dir_all(parent)?;
    let name = dest.file_name().context("install destination has no name")?.to_string_lossy();
    let staging = parent.join(format!(".staging-{name}-{}", std::process::id()));
    kargo_util::fs::remove_dir_all_if_exists(&staging)?;
    fs::create_dir_all(&staging)?;

    let result = (|| -> Result<()> {
        match kind {
            ArchiveKind::Zip => extract_zip(archive, &staging)?,
            ArchiveKind::TarGz => extract_tar_gz(archive, &staging)?,
        }
        let root = single_child_dir(&staging)?.unwrap_or_else(|| staging.clone());
        if dest.exists() {
            // Another process installed it meanwhile.
            return Ok(());
        }
        fs::rename(&root, dest).with_context(|| format!("failed to install into {}", dest.display()))
    })();
    let _ = kargo_util::fs::remove_dir_all_if_exists(&staging);
    result
}

pub fn single_child_dir(dir: &Path) -> Result<Option<PathBuf>> {
    let entries: Vec<_> = fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    match entries.as_slice() {
        [only] if only.file_type()?.is_dir() => Ok(Some(only.path())),
        _ => Ok(None),
    }
}

/// Extracts a zip archive, rejecting entries that would escape `dest`.
pub fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let file = fs::File::open(archive).with_context(|| format!("failed to open {}", archive.display()))?;
    let mut zip = zip::ZipArchive::new(file).with_context(|| format!("{} is not a valid zip archive", archive.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            bail!("archive {} contains an unsafe path `{}`", archive.display(), entry.name().unwrap_or_default());
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut target = fs::File::create(&out).with_context(|| format!("failed to create {}", out.display()))?;
        std::io::copy(&mut entry, &mut target)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out, fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

/// Extracts a `.tar.gz` archive; `tar` refuses entries escaping `dest`.
pub fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    let file = fs::File::open(archive).with_context(|| format!("failed to open {}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.set_preserve_permissions(true);
    tar.unpack(dest).with_context(|| format!("failed to extract {}", archive.display()))
}

/// Total size in bytes of a directory tree.
pub fn dir_size(dir: &Path) -> u64 {
    fn walk(dir: &Path) -> u64 {
        let Ok(entries) = fs::read_dir(dir) else { return 0 };
        entries
            .flatten()
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => walk(&e.path()),
                Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
                Err(_) => 0,
            })
            .sum()
    }
    walk(dir)
}

/// Formats a byte count for humans (`1.2 GiB`).
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn make_zip(path: &Path, entries: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for (name, contents) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(contents.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn archive_kinds() {
        assert_eq!(ArchiveKind::from_name("a.zip").unwrap(), ArchiveKind::Zip);
        assert_eq!(ArchiveKind::from_name("a.tar.gz").unwrap(), ArchiveKind::TarGz);
        assert!(ArchiveKind::from_name("a.msi").is_err());
        assert_eq!(ArchiveKind::TarGz.extension(), "tar.gz");
    }

    #[test]
    fn zip_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        make_zip(&archive, &[("root/bin/tool", "hi")]);
        let out = dir.path().join("out");
        extract_zip(&archive, &out).unwrap();
        assert_eq!(fs::read_to_string(out.join("root/bin/tool")).unwrap(), "hi");
        assert_eq!(single_child_dir(&out).unwrap(), Some(out.join("root")));
    }

    #[test]
    fn zip_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.zip");
        make_zip(&archive, &[("../evil.txt", "x")]);
        assert!(extract_zip(&archive, &dir.path().join("out")).is_err());
        assert!(!dir.path().join("evil.txt").exists());
    }

    #[test]
    fn tar_gz_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        {
            let enc = flate2::write::GzEncoder::new(fs::File::create(&archive).unwrap(), flate2::Compression::fast());
            let mut builder = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(2);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, "dist/bin/konanc", &b"ok"[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let out = dir.path().join("out");
        extract_tar_gz(&archive, &out).unwrap();
        assert_eq!(fs::read_to_string(out.join("dist/bin/konanc")).unwrap(), "ok");
    }

    #[test]
    fn install_moves_top_level_dir_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("t.zip");
        make_zip(&archive, &[("jdk-1/bin/java", "j"), ("jdk-1/release", "r")]);
        let dest = dir.path().join("tools/jdk-1");
        install(&archive, ArchiveKind::Zip, &dest).unwrap();
        assert_eq!(fs::read_to_string(dest.join("bin/java")).unwrap(), "j");
        // Reinstalling over an existing install keeps it and leaves no staging dirs.
        install(&archive, ArchiveKind::Zip, &dest).unwrap();
        let names: Vec<_> = fs::read_dir(dir.path().join("tools")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["jdk-1"]);
        assert_eq!(dir_size(&dest), 2);
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
