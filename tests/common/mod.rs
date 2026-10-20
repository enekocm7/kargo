//! Helpers shared by the integration tests.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;

/// A scratch project directory with an isolated Kargo home.
pub struct Project {
    pub dir: tempfile::TempDir,
    pub home: tempfile::TempDir,
}

impl Project {
    pub fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap(), home: tempfile::tempdir().unwrap() }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn file(&self, rel: &str, contents: &str) -> &Self {
        let p = self.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
        self
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    /// `kargo` running in the project with an isolated home directory.
    pub fn kargo(&self) -> Command {
        let mut cmd = Command::cargo_bin("kargo").unwrap();
        cmd.current_dir(self.path()).env("KARGO_HOME", self.home.path()).env_remove("KARGO_TARGET_DIR").env("NO_COLOR", "1");
        cmd
    }
}

/// An on-disk Maven repository.
pub struct LocalRepo {
    pub dir: tempfile::TempDir,
}

impl LocalRepo {
    pub fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap() }
    }

    pub fn url(&self) -> String {
        format!("file:///{}", self.dir.path().display().to_string().replace('\\', "/").trim_start_matches('/'))
    }

    pub fn write(&self, rel: &str, contents: &str) {
        let p = self.dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }

    /// Publishes `group:artifact:version` with `(group:artifact, version)` dependencies.
    pub fn publish(&self, coord: &str, version: &str, deps: &[(&str, &str)]) -> &Self {
        let (group, artifact) = coord.split_once(':').unwrap();
        let dir = format!("{}/{artifact}", group.replace('.', "/"));
        self.write(&format!("{dir}/{version}/{artifact}-{version}.pom"), &pom(group, artifact, version, deps));
        self.write(&format!("{dir}/{version}/{artifact}-{version}.jar"), &format!("jar {coord} {version}"));
        self.list_version(&dir, version)
    }

    /// Publishes the `classifier` jar of a module already published with [`LocalRepo::publish`].
    pub fn publish_classifier(&self, coord: &str, version: &str, classifier: &str) -> &Self {
        let (group, artifact) = coord.split_once(':').unwrap();
        let rel = format!("{}/{artifact}/{version}/{artifact}-{version}-{classifier}.jar", group.replace('.', "/"));
        self.write(&rel, &format!("jar {coord} {version} {classifier}"));
        self
    }

    /// Publishes a SNAPSHOT the way `mvn deploy` stores it: timestamped files in the `1.0-SNAPSHOT`
    /// directory (`stamp` is `20240101.120000-1`) and the `maven-metadata.xml` that points at them.
    /// Publishing again moves the metadata to the new build and leaves the old files in place.
    pub fn publish_snapshot(&self, coord: &str, version: &str, stamp: &str, deps: &[(&str, &str)]) -> &Self {
        let (group, artifact) = coord.split_once(':').unwrap();
        let dir = format!("{}/{artifact}", group.replace('.', "/"));
        let file_version = format!("{}-{stamp}", version.strip_suffix("-SNAPSHOT").unwrap());
        let (timestamp, number) = stamp.split_once('-').unwrap();
        let updated = timestamp.replace('.', "");
        self.write(&format!("{dir}/{version}/{artifact}-{file_version}.pom"), &pom(group, artifact, version, deps));
        self.write(&format!("{dir}/{version}/{artifact}-{file_version}.jar"), &format!("jar {coord} {file_version}"));
        let entry = |extension: &str| {
            format!(
                "<snapshotVersion><extension>{extension}</extension><value>{file_version}</value><updated>{updated}</updated></snapshotVersion>"
            )
        };
        self.write(
            &format!("{dir}/{version}/maven-metadata.xml"),
            &format!(
                "<metadata><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version><versioning>\
                 <snapshot><timestamp>{timestamp}</timestamp><buildNumber>{number}</buildNumber></snapshot><lastUpdated>{updated}</lastUpdated>\
                 <snapshotVersions>{}{}</snapshotVersions></versioning></metadata>",
                entry("jar"),
                entry("pom")
            ),
        );
        self.list_version(&dir, version)
    }

    /// Adds `version` to the module's version list.
    fn list_version(&self, dir: &str, version: &str) -> &Self {
        let meta_path = self.dir.path().join(format!("{dir}/maven-metadata.xml"));
        let mut versions: Vec<String> = std::fs::read_to_string(&meta_path)
            .map(|s| s.split("<version>").skip(1).map(|p| p.split('<').next().unwrap().to_owned()).collect())
            .unwrap_or_default();
        if !versions.iter().any(|v| v == version) {
            versions.push(version.to_owned());
        }
        let body: String = versions.iter().map(|v| format!("<version>{v}</version>")).collect();
        self.write(
            &format!("{dir}/maven-metadata.xml"),
            &format!("<metadata><versioning><versions>{body}</versions></versioning></metadata>"),
        );
        self
    }
}

fn pom(group: &str, artifact: &str, version: &str, deps: &[(&str, &str)]) -> String {
    let deps_xml: String = deps
        .iter()
        .map(|(c, v)| {
            let (g, a) = c.split_once(':').unwrap();
            format!("<dependency><groupId>{g}</groupId><artifactId>{a}</artifactId><version>{v}</version></dependency>")
        })
        .collect();
    format!(
        "<project><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version><dependencies>{deps_xml}</dependencies></project>"
    )
}

pub fn manifest(name: &str, extra: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n{extra}")
}

/// Whether end-to-end tests that need a JDK and network should run.
pub fn e2e_enabled() -> bool {
    std::env::var_os("KARGO_E2E").is_some()
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    std::env::split_paths(&path).find_map(|d| exts.iter().map(|e| d.join(format!("{name}{e}"))).find(|p| p.is_file()))
}
