//! The Kotlin/Native distribution (`kotlin-native-prebuilt`), downloaded from
//! Maven Central and extracted under `~/.kargo/toolchains`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::maven::{Artifact, MavenClient, ModuleId};

/// The host as Kotlin/Native names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Host {
    /// Classifier of the prebuilt distribution, e.g. `linux-x86_64`.
    pub classifier: &'static str,
    /// Archive format of the distribution.
    pub extension: &'static str,
    /// Konan target producing binaries for this host.
    pub konan_target: &'static str,
}

impl Host {
    pub fn current() -> Result<Self> {
        Self::for_platform(std::env::consts::OS, std::env::consts::ARCH)
    }

    pub fn for_platform(os: &str, arch: &str) -> Result<Self> {
        Ok(match (os, arch) {
            ("windows", "x86_64") => Host { classifier: "windows-x86_64", extension: "zip", konan_target: "mingw_x64" },
            ("linux", "x86_64") => Host { classifier: "linux-x86_64", extension: "tar.gz", konan_target: "linux_x64" },
            ("macos", "aarch64") => Host { classifier: "macos-aarch64", extension: "tar.gz", konan_target: "macos_arm64" },
            ("macos", "x86_64") => Host { classifier: "macos-x86_64", extension: "tar.gz", konan_target: "macos_x64" },
            _ => bail!("Kotlin/Native does not support {os}/{arch} as a build host"),
        })
    }
}

/// Resolves `host` in a platform list to the concrete Konan target.
pub fn resolve_platform(name: &str) -> Result<String> {
    if name == "host" { Ok(Host::current()?.konan_target.to_owned()) } else { Ok(name.to_owned()) }
}

/// Extension of executables produced for a Konan target.
pub fn executable_extension(konan_target: &str) -> &'static str {
    if konan_target == "mingw_x64" { "exe" } else { "kexe" }
}

#[derive(Debug, Clone)]
pub struct NativeToolchain {
    pub home: PathBuf,
    pub version: String,
}

impl NativeToolchain {
    pub fn konanc(&self) -> PathBuf {
        let name = if cfg!(windows) { "konanc.bat" } else { "konanc" };
        self.home.join("bin").join(name)
    }

    /// Ensures the distribution for `version` is installed, downloading it if needed.
    pub fn ensure(config: &Config, client: &MavenClient, version: &str) -> Result<Self> {
        let host = Host::current()?;
        let home = config.toolchains_dir().join(format!("kotlin-native-{version}-{}", host.classifier));
        let toolchain = Self { home: home.clone(), version: version.to_owned() };
        if toolchain.konanc().is_file() {
            return Ok(toolchain);
        }

        let artifact = Artifact::new(ModuleId::new("org.jetbrains.kotlin", "kotlin-native-prebuilt"), version)
            .with_classifier(Some(host.classifier.to_owned()))
            .with_extension(host.extension);
        let archive = client
            .fetch_required(&artifact.repo_path())
            .with_context(|| format!("failed to download Kotlin/Native {version} for {}", host.classifier))?;

        config.shell().status("Installing", format!("Kotlin/Native {version} ({})", host.classifier));
        let staging = config.toolchains_dir().join(format!(".staging-{}-{}", version, std::process::id()));
        crate::util::fs::remove_dir_all_if_exists(&staging)?;
        fs::create_dir_all(&staging)?;
        let result = (|| -> Result<()> {
            if host.extension == "zip" {
                extract_zip(&archive, &staging)?;
            } else {
                extract_tar_gz(&archive, &staging)?;
            }
            let root = single_child_dir(&staging)?.unwrap_or_else(|| staging.clone());
            if let Some(parent) = home.parent() {
                fs::create_dir_all(parent)?;
            }
            crate::util::fs::remove_dir_all_if_exists(&home)?;
            fs::rename(&root, &home).with_context(|| format!("failed to install toolchain into {}", home.display()))?;
            Ok(())
        })();
        let _ = crate::util::fs::remove_dir_all_if_exists(&staging);
        result?;

        if !toolchain.konanc().is_file() {
            bail!("the Kotlin/Native distribution at {} has no konanc", home.display());
        }
        Ok(toolchain)
    }
}

fn single_child_dir(dir: &Path) -> Result<Option<PathBuf>> {
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn host_mapping() {
        assert_eq!(Host::for_platform("windows", "x86_64").unwrap().konan_target, "mingw_x64");
        assert_eq!(Host::for_platform("macos", "aarch64").unwrap().classifier, "macos-aarch64");
        assert_eq!(Host::for_platform("linux", "x86_64").unwrap().extension, "tar.gz");
        assert!(Host::for_platform("freebsd", "x86_64").is_err());
        assert!(Host::current().is_ok() || cfg!(not(any(target_os = "windows", target_os = "linux", target_os = "macos"))));
    }

    #[test]
    fn platform_names() {
        assert_eq!(resolve_platform("linux_x64").unwrap(), "linux_x64");
        assert_eq!(executable_extension("mingw_x64"), "exe");
        assert_eq!(executable_extension("linux_x64"), "kexe");
    }

    #[test]
    fn zip_extraction() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        {
            let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            zip.add_directory("root/bin/", opts).unwrap();
            zip.start_file("root/bin/tool", opts).unwrap();
            zip.write_all(b"hi").unwrap();
            zip.finish().unwrap();
        }
        let out = dir.path().join("out");
        extract_zip(&archive, &out).unwrap();
        assert_eq!(fs::read_to_string(out.join("root/bin/tool")).unwrap(), "hi");
        assert_eq!(single_child_dir(&out).unwrap(), Some(out.join("root")));
    }

    #[test]
    fn zip_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.zip");
        {
            let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            zip.start_file("../evil.txt", zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(b"x").unwrap();
            zip.finish().unwrap();
        }
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
}
