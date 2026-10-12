//! SNAPSHOT builds: the version-level `maven-metadata.xml` that maps `1.0-SNAPSHOT` to the
//! timestamped files a repository actually holds (`lib-1.0-20240101.120000-1.jar`).

/// What `<version>/maven-metadata.xml` says about the newest build of a SNAPSHOT.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotMetadata {
    /// `<snapshot>`'s `<timestamp>-<buildNumber>`, e.g. `20240101.120000-1`.
    build: Option<String>,
    /// `<localCopy>`: the files carry the plain `-SNAPSHOT` name (a `mvn install` into a local repository).
    local_copy: bool,
    /// One entry per `<snapshotVersion>`: the build of each file kind.
    entries: Vec<SnapshotEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotEntry {
    classifier: Option<String>,
    extension: String,
    /// The file version, e.g. `1.0-20240101.120000-1`.
    value: String,
    /// `yyyyMMddHHmmss`; fixed width, so strings order chronologically.
    updated: String,
}

impl SnapshotMetadata {
    /// Parses a version-level `maven-metadata.xml`; `None` if it isn't XML.
    pub fn parse(xml: &str) -> Option<Self> {
        let doc = roxmltree::Document::parse(xml).ok()?;
        let text = |parent: roxmltree::Node, name: &str| -> Option<String> {
            let value = parent.children().find(|c| c.has_tag_name(name))?.text()?.trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        let mut meta = Self::default();
        for snapshot in doc.descendants().filter(|n| n.has_tag_name("snapshot") && n.parent().is_some_and(|p| p.has_tag_name("versioning")))
        {
            if let (Some(timestamp), Some(number)) = (text(snapshot, "timestamp"), text(snapshot, "buildNumber")) {
                meta.build = Some(format!("{timestamp}-{number}"));
            }
            meta.local_copy = text(snapshot, "localCopy").is_some_and(|v| v.eq_ignore_ascii_case("true"));
        }
        for node in doc.descendants().filter(|n| n.has_tag_name("snapshotVersion")) {
            let (Some(extension), Some(value)) = (text(node, "extension"), text(node, "value")) else { continue };
            meta.entries.push(SnapshotEntry {
                classifier: text(node, "classifier"),
                extension,
                value,
                updated: text(node, "updated").unwrap_or_default(),
            });
        }
        Some(meta)
    }

    /// The file version of the newest build of the `classifier`/`extension` file of SNAPSHOT `version`,
    /// such as `1.0-20240101.120000-1` for `1.0-SNAPSHOT`. `None` if the repository holds the file
    /// under the plain `-SNAPSHOT` name, or doesn't have such a file.
    ///
    /// The `<snapshotVersions>` entry with the latest `<updated>` wins; metadata without
    /// `<snapshotVersions>` (older repositories) falls back to `<snapshot>`'s timestamp and build number.
    pub fn file_version(&self, version: &str, classifier: Option<&str>, extension: &str) -> Option<String> {
        if self.local_copy {
            return None;
        }
        if self.entries.is_empty() {
            let base = version.strip_suffix("-SNAPSHOT")?;
            return self.build.as_ref().map(|build| format!("{base}-{build}"));
        }
        self.entries
            .iter()
            .filter(|e| e.extension == extension && e.classifier.as_deref() == classifier.filter(|c| !c.is_empty()))
            .max_by(|a, b| a.updated.cmp(&b.updated))
            .map(|e| e.value.clone())
    }
}

/// The `yyyyMMdd.HHmmss-N` build stamp in a file name such as `lib-1.0-20240101.120000-1.jar`, given
/// the artifact id and the SNAPSHOT version it belongs to (`1.0-SNAPSHOT`).
pub fn build_stamp<'a>(file_name: &'a str, artifact: &str, version: &str) -> Option<&'a str> {
    let base = version.strip_suffix("-SNAPSHOT")?;
    let rest = file_name.strip_prefix(artifact)?.strip_prefix('-')?.strip_prefix(base)?.strip_prefix('-')?;
    let bytes = rest.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes.get(range).is_some_and(|b| !b.is_empty() && b.iter().all(u8::is_ascii_digit));
    if !(digits(0..8) && bytes.get(8) == Some(&b'.') && digits(9..15) && bytes.get(15) == Some(&b'-')) {
        return None;
    }
    let number = bytes[16..].iter().take_while(|b| b.is_ascii_digit()).count();
    (number > 0).then(|| &rest[..16 + number])
}

#[cfg(test)]
mod tests {
    use super::*;

    const METADATA: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata modelVersion="1.1.0">
  <groupId>com.acme</groupId>
  <artifactId>lib</artifactId>
  <version>1.0-SNAPSHOT</version>
  <versioning>
    <snapshot><timestamp>20240102.080000</timestamp><buildNumber>3</buildNumber></snapshot>
    <lastUpdated>20240102080000</lastUpdated>
    <snapshotVersions>
      <snapshotVersion><extension>jar</extension><value>1.0-20240101.120000-1</value><updated>20240101120000</updated></snapshotVersion>
      <snapshotVersion><extension>jar</extension><value>1.0-20240102.080000-3</value><updated>20240102080000</updated></snapshotVersion>
      <snapshotVersion><extension>jar</extension><value>1.0-20240101.130000-2</value><updated>20240101130000</updated></snapshotVersion>
      <snapshotVersion><classifier>sources</classifier><extension>jar</extension><value>1.0-20240101.120000-1</value><updated>20240101120000</updated></snapshotVersion>
      <snapshotVersion><extension>pom</extension><value>1.0-20240102.080000-3</value><updated>20240102080000</updated></snapshotVersion>
    </snapshotVersions>
  </versioning>
</metadata>"#;

    #[test]
    fn picks_the_latest_build_per_classifier_and_extension() {
        let meta = SnapshotMetadata::parse(METADATA).unwrap();
        assert_eq!(meta.file_version("1.0-SNAPSHOT", None, "jar").as_deref(), Some("1.0-20240102.080000-3"));
        assert_eq!(meta.file_version("1.0-SNAPSHOT", None, "pom").as_deref(), Some("1.0-20240102.080000-3"));
        assert_eq!(meta.file_version("1.0-SNAPSHOT", Some("sources"), "jar").as_deref(), Some("1.0-20240101.120000-1"));
        assert_eq!(meta.file_version("1.0-SNAPSHOT", Some(""), "jar").as_deref(), Some("1.0-20240102.080000-3"));
        assert_eq!(meta.file_version("1.0-SNAPSHOT", None, "module"), None, "no such file is listed");
        assert_eq!(meta.file_version("1.0-SNAPSHOT", Some("natives"), "jar"), None);
    }

    #[test]
    fn falls_back_to_the_snapshot_timestamp() {
        let xml = "<metadata><versioning><snapshot><timestamp>20240101.120000</timestamp><buildNumber>7</buildNumber></snapshot></versioning></metadata>";
        let meta = SnapshotMetadata::parse(xml).unwrap();
        assert_eq!(meta.file_version("2.1-SNAPSHOT", None, "jar").as_deref(), Some("2.1-20240101.120000-7"));
        assert_eq!(meta.file_version("2.1-SNAPSHOT", Some("tests"), "zip").as_deref(), Some("2.1-20240101.120000-7"));
    }

    #[test]
    fn local_copies_and_empty_metadata_keep_the_plain_name() {
        let local = "<metadata><versioning><snapshot><localCopy>true</localCopy></snapshot>\
                     <snapshotVersions><snapshotVersion><extension>jar</extension><value>1.0-SNAPSHOT</value></snapshotVersion></snapshotVersions>\
                     </versioning></metadata>";
        assert_eq!(SnapshotMetadata::parse(local).unwrap().file_version("1.0-SNAPSHOT", None, "jar"), None);
        assert_eq!(SnapshotMetadata::parse("<metadata/>").unwrap().file_version("1.0-SNAPSHOT", None, "jar"), None);
        assert!(SnapshotMetadata::parse("garbage").is_none());
    }

    #[test]
    fn finds_the_build_stamp_in_a_file_name() {
        let stamp = |name| build_stamp(name, "lib", "1.0-SNAPSHOT");
        assert_eq!(stamp("lib-1.0-20240101.120000-1.jar"), Some("20240101.120000-1"));
        assert_eq!(stamp("lib-1.0-20240101.120000-12-sources.jar"), Some("20240101.120000-12"));
        assert_eq!(stamp("lib-1.0-SNAPSHOT.jar"), None);
        assert_eq!(stamp("other-1.0-20240101.120000-1.jar"), None);
        assert_eq!(stamp("lib-1.0-2024.jar"), None);
        assert_eq!(build_stamp("lib-1.0-20240101.120000-1.jar", "lib", "1.0"), None);
    }
}
