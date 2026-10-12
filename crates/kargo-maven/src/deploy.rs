//! Publishing to Maven repositories: copying a module's files into a repository (a local
//! directory, or an HTTP `PUT` per file like `mvn deploy`) and keeping the module's
//! `maven-metadata.xml` version list current.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use super::coord::{ModuleId, is_snapshot};
use super::repo::{MavenClient, Repository};
use super::version::Version;
use kargo_util::hash;

/// One file of a published module, by name, inside its version directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployFile<'a> {
    pub name: String,
    pub path: &'a Path,
}

/// Where a module goes.
pub struct Deployer<'a> {
    client: &'a MavenClient,
    repo: &'a Repository,
    /// A repository that only this machine reads (`~/.m2/repository`), which keeps its version
    /// list in `maven-metadata-local.xml` like `mvn install`.
    local: bool,
}

impl<'a> Deployer<'a> {
    pub fn new(client: &'a MavenClient, repo: &'a Repository) -> Self {
        Self { client, repo, local: false }
    }

    /// A deployer for the local Maven repository, written like `mvn install` writes it.
    pub fn maven_local(client: &'a MavenClient, repo: &'a Repository) -> Self {
        Self { client, repo, local: true }
    }

    /// Uploads `files` (already in the version directory's layout, with their checksums and
    /// signatures) to `module`'s `version` directory, then adds `version` to the module's
    /// version list. A SNAPSHOT keeps its plain `-SNAPSHOT` file names.
    pub fn deploy(&self, module: &ModuleId, version: &str, files: &[DeployFile]) -> Result<()> {
        let dir = format!("{}/{version}", module.repo_dir());
        for file in files {
            let data = std::fs::read(file.path).with_context(|| format!("failed to read {}", file.path.display()))?;
            self.client.shell().verbose("Uploading", format!("{dir}/{}", file.name));
            self.client.upload(self.repo, &format!("{dir}/{}", file.name), &data)?;
        }
        self.update_metadata(module, version)
    }

    fn update_metadata(&self, module: &ModuleId, version: &str) -> Result<()> {
        let name = if self.local { "maven-metadata-local.xml" } else { "maven-metadata.xml" };
        let rel = format!("{}/{name}", module.repo_dir());
        let existing = self.client.read_text_from(self.repo, &rel)?;
        let xml = merge_metadata(existing.as_deref(), module, version, &now_stamp());
        self.client.upload(self.repo, &rel, xml.as_bytes())?;
        if !self.local {
            for (ext, sum) in hash::checksums_bytes(xml.as_bytes()).files() {
                self.client.upload(self.repo, &format!("{rel}.{ext}"), sum.as_bytes())?;
            }
        }
        Ok(())
    }
}

/// The current UTC time as Maven metadata spells it: `yyyyMMddHHmmss`.
pub fn now_stamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format_stamp(secs)
}

/// Seconds since the Unix epoch as `yyyyMMddHHmmss` (UTC).
fn format_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}{:02}{:02}{:02}", rem / 3_600, rem % 3_600 / 60, rem % 60)
}

/// The module-level `maven-metadata.xml` after publishing `version`: every version already
/// listed plus this one, with `<latest>` the newest and `<release>` the newest non-SNAPSHOT.
pub fn merge_metadata(existing: Option<&str>, module: &ModuleId, version: &str, updated: &str) -> String {
    let mut versions: Vec<String> = existing.map(listed_versions).unwrap_or_default();
    if !versions.iter().any(|v| v == version) {
        versions.push(version.to_owned());
    }
    versions.sort_by_key(|v| Version::parse(v));
    let latest = versions.last().cloned().unwrap_or_else(|| version.to_owned());
    let release = versions.iter().rev().find(|v| !is_snapshot(v)).cloned();
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<metadata>\n");
    xml.push_str(&format!("  <groupId>{}</groupId>\n  <artifactId>{}</artifactId>\n", escape(&module.group), escape(&module.artifact)));
    xml.push_str("  <versioning>\n");
    xml.push_str(&format!("    <latest>{}</latest>\n", escape(&latest)));
    if let Some(release) = release {
        xml.push_str(&format!("    <release>{}</release>\n", escape(&release)));
    }
    xml.push_str("    <versions>\n");
    for v in &versions {
        xml.push_str(&format!("      <version>{}</version>\n", escape(v)));
    }
    xml.push_str("    </versions>\n");
    xml.push_str(&format!("    <lastUpdated>{updated}</lastUpdated>\n"));
    xml.push_str("  </versioning>\n</metadata>\n");
    xml
}

/// The `<version>` entries of a `maven-metadata.xml`; none if it isn't valid XML.
fn listed_versions(xml: &str) -> Vec<String> {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return Vec::new() };
    doc.descendants()
        .filter(|n| n.has_tag_name("version") && n.parent().is_some_and(|p| p.has_tag_name("versions")))
        .filter_map(|n| n.text().map(|t| t.trim().to_owned()))
        .filter(|v| !v.is_empty())
        .collect()
}

/// Escapes text for an XML element or attribute.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kargo_util::config::Config;
    use kargo_util::shell::{Shell, Verbosity};

    #[test]
    fn formats_utc_stamps() {
        assert_eq!(format_stamp(0), "19700101000000");
        assert_eq!(format_stamp(1_704_067_200 + 3_661), "20240101010101");
        assert_eq!(format_stamp(951_782_400), "20000229000000");
    }

    #[test]
    fn merges_versions_into_metadata() {
        let module = ModuleId::new("com.acme", "lib");
        let first = merge_metadata(None, &module, "1.0.0", "20240101000000");
        assert!(first.contains("<latest>1.0.0</latest>") && first.contains("<release>1.0.0</release>"));
        let second = merge_metadata(Some(&first), &module, "1.10.0", "20240102000000");
        let third = merge_metadata(Some(&second), &module, "1.2.0-SNAPSHOT", "20240103000000");
        assert_eq!(listed_versions(&third), ["1.0.0", "1.2.0-SNAPSHOT", "1.10.0"]);
        assert!(third.contains("<latest>1.10.0</latest>"));
        assert!(third.contains("<release>1.10.0</release>"));
        assert!(third.contains("<lastUpdated>20240103000000</lastUpdated>"));
        // Publishing a version again keeps a single entry.
        assert_eq!(listed_versions(&merge_metadata(Some(&third), &module, "1.0.0", "x")).len(), 3);
        let snapshot_only = merge_metadata(None, &module, "2.0-SNAPSHOT", "x");
        assert!(!snapshot_only.contains("<release>"));
    }

    #[test]
    fn deploys_over_http_with_credentials() {
        use crate::repo::Credentials;
        use crate::testserver::TestServer;
        let existing = merge_metadata(None, &ModuleId::new("com.acme", "lib"), "0.9", "20240101000000");
        let server = TestServer::start(move |r| match r.method.as_str() {
            "GET" if r.path.ends_with("maven-metadata.xml") => (200, existing.clone()),
            "PUT" if r.header("authorization").is_some() => (201, String::new()),
            "PUT" => (401, String::new()),
            _ => (404, String::new()),
        });
        let dir = tempfile::tempdir().unwrap();
        let config = Config::new(dir.path().join("home"), Shell::new(Verbosity::Quiet));
        let mut repo = Repository::new("internal", format!("{}/releases", server.url));
        let client = MavenClient::new(&config, vec![]);
        let jar = dir.path().join("lib-1.0.jar");
        std::fs::write(&jar, "jar").unwrap();
        let module = ModuleId::new("com.acme", "lib");
        let files = [DeployFile { name: "lib-1.0.jar".into(), path: &jar }];

        let err = Deployer::new(&client, &repo).deploy(&module, "1.0", &files).unwrap_err().to_string();
        assert!(err.contains("username-env") && err.contains("[repositories.internal]"), "{err}");

        repo.credentials = Some(Credentials { username: "me".into(), password: "secret".into() });
        Deployer::new(&client, &repo).deploy(&module, "1.0", &files).unwrap();
        let puts: Vec<_> = server.requests().into_iter().filter(|r| r.method == "PUT" && r.header("authorization").is_some()).collect();
        let paths: Vec<&str> = puts.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/releases/com/acme/lib/1.0/lib-1.0.jar",
                "/releases/com/acme/lib/maven-metadata.xml",
                "/releases/com/acme/lib/maven-metadata.xml.md5",
                "/releases/com/acme/lib/maven-metadata.xml.sha1",
                "/releases/com/acme/lib/maven-metadata.xml.sha256",
                "/releases/com/acme/lib/maven-metadata.xml.sha512",
            ]
        );
        assert_eq!(puts[0].body, b"jar");
        assert_eq!(puts[0].header("authorization"), Some("Basic bWU6c2VjcmV0"));
        let metadata = String::from_utf8(puts[1].body.clone()).unwrap();
        assert_eq!(listed_versions(&metadata), ["0.9", "1.0"], "the published version joins the existing ones");
    }

    #[test]
    fn deploys_into_a_local_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("repo");
        let config = Config::new(dir.path().join("home"), Shell::new(Verbosity::Quiet));
        let repo = Repository::new("local", format!("file://{}", repo_dir.to_string_lossy().replace('\\', "/")));
        let client = MavenClient::new(&config, vec![repo.clone()]);
        let jar = dir.path().join("lib-1.0.jar");
        std::fs::write(&jar, "jar").unwrap();
        let module = ModuleId::new("com.acme", "lib");
        let files = [DeployFile { name: "lib-1.0.jar".into(), path: &jar }];
        Deployer::new(&client, &repo).deploy(&module, "1.0", &files).unwrap();
        let module_dir = repo_dir.join("com/acme/lib");
        assert_eq!(std::fs::read_to_string(module_dir.join("1.0/lib-1.0.jar")).unwrap(), "jar");
        let metadata = std::fs::read_to_string(module_dir.join("maven-metadata.xml")).unwrap();
        assert!(metadata.contains("<version>1.0</version>"));
        let sha1 = std::fs::read_to_string(module_dir.join("maven-metadata.xml.sha1")).unwrap();
        assert_eq!(sha1, hash::sha1_file(&module_dir.join("maven-metadata.xml")).unwrap());

        Deployer::maven_local(&client, &repo).deploy(&module, "1.1", &files).unwrap();
        let local = std::fs::read_to_string(module_dir.join("maven-metadata-local.xml")).unwrap();
        assert!(local.contains("<version>1.1</version>"));
        assert!(!module_dir.join("maven-metadata-local.xml.sha1").exists());
    }
}
