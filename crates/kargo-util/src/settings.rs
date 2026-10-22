//! Kargo's own configuration files, for the network a build runs in rather than the project:
//! `~/.kargo/config.toml` (the user's) and `.kargo/config.toml` (a project's, found by walking up
//! from the current directory). The project's file wins key by key.
//!
//! ```toml
//! [mirrors]
//! "*" = "https://artifactory.corp/maven-virtual"         # every remote repository
//! "https://maven.google.com" = "https://nexus.corp/google" # one repository, by URL or name
//!
//! [mirrors.credentials."https://artifactory.corp"]
//! username-env = "ART_USER"
//! password-env = "ART_TOKEN"
//!
//! [toolchain-mirrors]
//! "https://api.adoptium.net" = "https://artifactory.corp/adoptium-api"
//!
//! [http]
//! proxy = "http://proxy.corp:3128"
//! cainfo = "/etc/ssl/corp-ca.pem"
//!
//! [source]
//! vendored = true
//! directory = "vendor"
//!
//! [toolchain]
//! auto-install = false
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// The name of the configuration file, in the Kargo home and in a project's `.kargo` directory.
pub const FILE_NAME: &str = "config.toml";

/// The directory of a project's configuration file.
pub const PROJECT_DIR: &str = ".kargo";

/// The default directory `kargo vendor` writes and `--vendored` reads, relative to the project root.
pub const DEFAULT_VENDOR_DIR: &str = "vendor";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawSettings {
    #[serde(default)]
    mirrors: RawMirrors,
    #[serde(default)]
    toolchain_mirrors: BTreeMap<String, String>,
    #[serde(default)]
    http: RawHttp,
    #[serde(default)]
    source: RawSource,
    #[serde(default)]
    toolchain: RawToolchain,
}

#[derive(Debug, Default, Deserialize)]
struct RawMirrors {
    #[serde(default)]
    credentials: BTreeMap<String, RawCredentials>,
    #[serde(flatten)]
    urls: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawCredentials {
    username_env: String,
    password_env: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawHttp {
    proxy: Option<String>,
    cainfo: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawSource {
    vendored: Option<bool>,
    directory: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawToolchain {
    auto_install: Option<bool>,
}

/// Environment variables that hold the credentials for every URL starting with a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialsEnv {
    pub prefix: String,
    pub username_env: String,
    pub password_env: String,
}

/// The merged settings of the configuration files.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// Mirror URL by what it replaces: `*`, a repository URL or a repository name.
    mirrors: BTreeMap<String, String>,
    credentials: Vec<CredentialsEnv>,
    /// Replacement by URL prefix, for toolchain downloads.
    toolchain_mirrors: BTreeMap<String, String>,
    proxy: Option<String>,
    cainfo: Option<PathBuf>,
    vendored: Option<bool>,
    vendor_dir: Option<PathBuf>,
    auto_install: Option<bool>,
    /// The files the settings were read from, in the order they apply.
    files: Vec<PathBuf>,
}

/// Strips the trailing slashes a URL may be written with, so `https://a/` and `https://a` match.
fn normalize(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

fn check_url(what: &str, url: &str) -> Result<()> {
    if !(url.starts_with("https://") || url.starts_with("http://") || url.starts_with("file:")) {
        bail!("{what} must be an http(s):// or file: URL, got `{url}`");
    }
    Ok(())
}

impl Settings {
    /// Reads `<home>/config.toml`, then the nearest `.kargo/config.toml` at or above `start`.
    pub fn load(home: &Path, start: Option<&Path>) -> Result<Self> {
        let mut settings = Settings::default();
        let user = home.join(FILE_NAME);
        if user.is_file() {
            settings.merge_file(&user, home)?;
        }
        let same_file = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        if let Some(start) = start {
            for dir in start.ancestors() {
                let file = dir.join(PROJECT_DIR).join(FILE_NAME);
                if file.is_file() {
                    if !same_file(&file, &user) {
                        settings.merge_file(&file, dir)?;
                    }
                    break;
                }
            }
        }
        Ok(settings)
    }

    fn merge_file(&mut self, file: &Path, base: &Path) -> Result<()> {
        let text = std::fs::read_to_string(file).with_context(|| format!("failed to read {}", file.display()))?;
        self.merge_str(&text, base).with_context(|| format!("invalid configuration in {}", file.display()))?;
        self.files.push(file.to_path_buf());
        Ok(())
    }

    /// Applies the settings in `text` over these. Relative paths in it are relative to `base`.
    pub fn merge_str(&mut self, text: &str, base: &Path) -> Result<()> {
        let raw: RawSettings = toml::from_str(text)?;
        for (from, to) in raw.mirrors.urls {
            check_url(&format!("the mirror of `{from}`"), to.trim())?;
            let from = if from == "*" { from } else { normalize(&from) };
            self.mirrors.insert(from, normalize(&to));
        }
        for (prefix, creds) in raw.mirrors.credentials {
            let prefix = normalize(&prefix);
            self.credentials.retain(|c| c.prefix != prefix);
            self.credentials.push(CredentialsEnv { prefix, username_env: creds.username_env, password_env: creds.password_env });
        }
        for (from, to) in raw.toolchain_mirrors {
            check_url(&format!("[toolchain-mirrors] key `{from}`"), from.trim())?;
            check_url(&format!("the toolchain mirror of `{from}`"), to.trim())?;
            self.toolchain_mirrors.insert(normalize(&from), normalize(&to));
        }
        if let Some(proxy) = raw.http.proxy {
            let proxy = proxy.trim().to_owned();
            if !proxy.is_empty() && !proxy.contains("://") {
                bail!("[http] `proxy` must be a URL such as \"http://proxy.corp:3128\", got `{proxy}`");
            }
            self.proxy = Some(proxy);
        }
        if let Some(cainfo) = raw.http.cainfo {
            self.cainfo = Some(base.join(cainfo));
        }
        if let Some(vendored) = raw.source.vendored {
            self.vendored = Some(vendored);
        }
        if let Some(dir) = raw.source.directory {
            self.vendor_dir = Some(dir);
        }
        if let Some(auto_install) = raw.toolchain.auto_install {
            self.auto_install = Some(auto_install);
        }
        Ok(())
    }

    /// The configuration files that were read, user's first.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// The mirror of the remote repository `name` at `url`: one for its URL or name, else `*`.
    pub fn repository_mirror(&self, name: &str, url: &str) -> Option<&str> {
        self.mirrors.get(&normalize(url)).or_else(|| self.mirrors.get(name)).or_else(|| self.mirrors.get("*")).map(String::as_str)
    }

    /// Whether any repository mirror is configured.
    pub fn has_mirrors(&self) -> bool {
        !self.mirrors.is_empty()
    }

    /// The credentials for `url` from `[mirrors.credentials]`: the longest matching prefix, read from
    /// its environment variables. Unset variables yield none (the server then answers 401).
    pub fn credentials(&self, url: &str) -> Option<(String, String)> {
        let entry = self.credentials_env(url)?;
        Some((std::env::var(&entry.username_env).ok()?, std::env::var(&entry.password_env).ok()?))
    }

    /// The environment variables of the credentials for `url`, for programs that read them themselves (Gradle).
    pub fn credentials_env(&self, url: &str) -> Option<&CredentialsEnv> {
        self.credentials
            .iter()
            .filter(|c| url == c.prefix || url.strip_prefix(&c.prefix).is_some_and(|rest| rest.starts_with('/')))
            .max_by_key(|c| c.prefix.len())
    }

    /// `url` with the longest matching prefix of `[toolchain-mirrors]` replaced.
    pub fn toolchain_url(&self, url: &str) -> String {
        let found = self
            .toolchain_mirrors
            .iter()
            .filter(|(from, _)| url == from.as_str() || url.strip_prefix(from.as_str()).is_some_and(|rest| rest.starts_with('/')))
            .max_by_key(|(from, _)| from.len());
        match found {
            Some((from, to)) => format!("{to}{}", &url[from.len()..]),
            None => url.to_owned(),
        }
    }

    /// `[http] proxy`: `Some("")` turns the proxy of the environment off.
    pub fn proxy(&self) -> Option<&str> {
        self.proxy.as_deref()
    }

    /// `[http] cainfo`: a PEM bundle of the certificate authorities to trust instead of the built-in ones.
    pub fn cainfo(&self) -> Option<&Path> {
        self.cainfo.as_deref()
    }

    /// `[source] vendored`.
    pub fn vendored(&self) -> bool {
        self.vendored.unwrap_or(false)
    }

    /// `[source] directory`, relative to the project root.
    pub fn vendor_dir(&self) -> &Path {
        self.vendor_dir.as_deref().unwrap_or(Path::new(DEFAULT_VENDOR_DIR))
    }

    /// `[toolchain] auto-install`: whether a command may download a toolchain it needs (default true).
    pub fn auto_install(&self) -> bool {
        self.auto_install.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        s.merge_str(text, Path::new("/base")).unwrap();
        s
    }

    #[test]
    fn picks_the_most_specific_mirror() {
        let s = parse(
            "[mirrors]\n\"*\" = \"https://art/all/\"\n\"https://repo.maven.apache.org/maven2/\" = \"https://nexus/central\"\ngoogle = \"https://nexus/google\"\n",
        );
        assert_eq!(s.repository_mirror("maven-central", "https://repo.maven.apache.org/maven2"), Some("https://nexus/central"));
        assert_eq!(s.repository_mirror("google", "https://dl.google.com/dl/android/maven2"), Some("https://nexus/google"));
        assert_eq!(s.repository_mirror("jitpack", "https://jitpack.io"), Some("https://art/all"));
        assert_eq!(parse("").repository_mirror("x", "https://x"), None);
    }

    #[test]
    fn rewrites_toolchain_urls_by_prefix() {
        let s = parse(
            "[toolchain-mirrors]\n\"https://github.com\" = \"https://art/github\"\n\"https://github.com/adoptium\" = \"https://art/adoptium/\"\n",
        );
        assert_eq!(s.toolchain_url("https://github.com/adoptium/temurin21/x.zip"), "https://art/adoptium/temurin21/x.zip");
        assert_eq!(s.toolchain_url("https://github.com/fwcd/kls/server.zip"), "https://art/github/fwcd/kls/server.zip");
        // A prefix only matches whole path segments.
        assert_eq!(s.toolchain_url("https://github.community/x"), "https://github.community/x");
        assert_eq!(s.toolchain_url("https://services.gradle.org/x"), "https://services.gradle.org/x");
    }

    #[test]
    fn reads_credentials_from_the_environment() {
        let s = parse(
            "[mirrors.credentials.\"https://art.corp\"]\nusername-env = \"KARGO_TEST_SETTINGS_USER\"\npassword-env = \"KARGO_TEST_SETTINGS_PASS\"\n",
        );
        assert_eq!(s.credentials("https://art.corp/maven"), None, "unset variables give no credentials");
        // SAFETY: no other test reads these variables.
        unsafe {
            std::env::set_var("KARGO_TEST_SETTINGS_USER", "me");
            std::env::set_var("KARGO_TEST_SETTINGS_PASS", "secret");
        }
        assert_eq!(s.credentials("https://art.corp/maven"), Some(("me".to_owned(), "secret".to_owned())));
        assert_eq!(s.credentials("https://art.corporate/maven"), None);
    }

    #[test]
    fn later_files_win_and_bad_values_are_errors() {
        let mut s = parse("[mirrors]\n\"*\" = \"https://a\"\n[source]\nvendored = true\n[http]\ncainfo = \"ca.pem\"\n");
        s.merge_str("[mirrors]\n\"*\" = \"https://b\"\n", Path::new("/other")).unwrap();
        assert_eq!(s.repository_mirror("x", "https://x"), Some("https://b"));
        assert!(s.vendored());
        assert_eq!(s.vendor_dir(), Path::new("vendor"));
        assert_eq!(s.cainfo(), Some(Path::new("/base").join("ca.pem").as_path()));

        let mut s = Settings::default();
        assert!(s.merge_str("[mirrors]\n\"*\" = \"artifactory\"\n", Path::new("/")).is_err());
        assert!(s.merge_str("[http]\nproxy = \"proxy:3128\"\n", Path::new("/")).is_err());
        assert!(s.merge_str("[unknown]\n", Path::new("/")).is_err());
        assert!(s.merge_str("[toolchain]\njdk = \"21\"\n", Path::new("/")).is_err());
    }

    #[test]
    fn toolchain_auto_install_defaults_to_on() {
        let mut s = Settings::default();
        assert!(s.auto_install());
        s.merge_str("[toolchain]\nauto-install = false\n", Path::new("/")).unwrap();
        assert!(!s.auto_install());
        s.merge_str("[http]\nproxy = \"\"\n", Path::new("/")).unwrap();
        assert!(!s.auto_install(), "a later file without the key keeps it");
        s.merge_str("[toolchain]\nauto-install = true\n", Path::new("/")).unwrap();
        assert!(s.auto_install());
    }

    #[test]
    fn loads_the_user_and_the_nearest_project_file() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(FILE_NAME), "[mirrors]\n\"*\" = \"https://user\"\ngoogle = \"https://user-google\"\n").unwrap();
        std::fs::create_dir_all(project.path().join(PROJECT_DIR)).unwrap();
        std::fs::write(project.path().join(PROJECT_DIR).join(FILE_NAME), "[mirrors]\n\"*\" = \"https://project\"\n").unwrap();
        let nested = project.path().join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();
        let s = Settings::load(home.path(), Some(&nested)).unwrap();
        assert_eq!(s.files().len(), 2);
        assert_eq!(s.repository_mirror("central", "https://c"), Some("https://project"));
        assert_eq!(s.repository_mirror("google", "https://g"), Some("https://user-google"));
    }
}
