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

    fn write(&self, rel: &str, contents: &str) {
        let p = self.dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }

    /// Publishes `group:artifact:version` with `(group:artifact, version)` dependencies.
    pub fn publish(&self, coord: &str, version: &str, deps: &[(&str, &str)]) -> &Self {
        let (group, artifact) = coord.split_once(':').unwrap();
        let dir = format!("{}/{artifact}", group.replace('.', "/"));
        let deps_xml: String = deps
            .iter()
            .map(|(c, v)| {
                let (g, a) = c.split_once(':').unwrap();
                format!("<dependency><groupId>{g}</groupId><artifactId>{a}</artifactId><version>{v}</version></dependency>")
            })
            .collect();
        self.write(
            &format!("{dir}/{version}/{artifact}-{version}.pom"),
            &format!(
                "<project><groupId>{group}</groupId><artifactId>{artifact}</artifactId><version>{version}</version><dependencies>{deps_xml}</dependencies></project>"
            ),
        );
        self.write(&format!("{dir}/{version}/{artifact}-{version}.jar"), &format!("jar {coord} {version}"));
        let meta_path = self.dir.path().join(format!("{dir}/maven-metadata.xml"));
        let mut versions: Vec<String> = std::fs::read_to_string(&meta_path)
            .map(|s| s.split("<version>").skip(1).map(|p| p.split('<').next().unwrap().to_owned()).collect())
            .unwrap_or_default();
        versions.push(version.to_owned());
        let body: String = versions.iter().map(|v| format!("<version>{v}</version>")).collect();
        self.write(
            &format!("{dir}/maven-metadata.xml"),
            &format!("<metadata><versioning><versions>{body}</versions></versioning></metadata>"),
        );
        self
    }
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
