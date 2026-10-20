//! Packaging JVM classes into jars (thin jars with a `Class-Path`, or fat jars).

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use zip::write::SimpleFileOptions;

#[derive(Debug, Clone, Default)]
pub struct JarManifest {
    pub main_class: Option<String>,
    /// Relative paths for the `Class-Path` attribute.
    pub class_path: Vec<String>,
    pub implementation_title: Option<String>,
    pub implementation_version: Option<String>,
}

impl JarManifest {
    pub fn render(&self) -> String {
        let mut lines = vec!["Manifest-Version: 1.0".to_owned(), format!("Created-By: Kargo {}", env!("CARGO_PKG_VERSION"))];
        if let Some(m) = &self.main_class {
            lines.push(format!("Main-Class: {m}"));
        }
        if let Some(t) = &self.implementation_title {
            lines.push(format!("Implementation-Title: {t}"));
        }
        if let Some(v) = &self.implementation_version {
            lines.push(format!("Implementation-Version: {v}"));
        }
        if !self.class_path.is_empty() {
            lines.push(format!("Class-Path: {}", self.class_path.join(" ")));
        }
        // Manifest lines are limited to 72 bytes; continuation lines start with a space.
        let mut out = String::new();
        for line in lines {
            let bytes = line.as_bytes();
            let mut start = 0;
            let mut first = true;
            while start < bytes.len() {
                let limit = if first { 72 } else { 71 };
                let mut end = (start + limit).min(bytes.len());
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                if !first {
                    out.push(' ');
                }
                out.push_str(&line[start..end]);
                out.push_str("\r\n");
                start = end;
                first = false;
            }
        }
        out.push_str("\r\n");
        out
    }
}

fn options() -> SimpleFileOptions {
    // A fixed timestamp keeps jars reproducible.
    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).last_modified_time(zip::DateTime::default())
}

/// Files whose contents must be concatenated when merging jars.
fn is_mergeable(name: &str) -> bool {
    name.starts_with("META-INF/services/")
        || name == "META-INF/spring.factories"
        || name.starts_with("META-INF/spring/") && name.ends_with(".imports")
        || name == "META-INF/spring.handlers"
        || name == "META-INF/spring.schemas"
}

/// Signature files of signed dependencies become invalid in a fat jar.
fn is_signature(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.starts_with("META-INF/")
        && !upper[9..].contains('/')
        && (upper.ends_with(".SF") || upper.ends_with(".DSA") || upper.ends_with(".RSA") || upper.ends_with(".EC"))
}

/// Whether the jar at `jar` exists and its `META-INF/MANIFEST.MF` is exactly what `manifest` renders.
pub fn manifest_matches(jar: &Path, manifest: &JarManifest) -> bool {
    let read = || -> Option<String> {
        let mut archive = zip::ZipArchive::new(File::open(jar).ok()?).ok()?;
        let mut entry = archive.by_name("META-INF/MANIFEST.MF").ok()?;
        let mut text = String::new();
        entry.read_to_string(&mut text).ok()?;
        Some(text)
    };
    read().is_some_and(|text| text == manifest.render())
}

/// Writes a jar from `classes_dir`, optionally merging dependency jars (`fat`).
pub fn write_jar(dest: &Path, classes_dir: &Path, manifest: &JarManifest, merge: &[PathBuf]) -> Result<()> {
    kargo_util::fs::write_atomic(dest, |file| {
        let mut zip = zip::ZipWriter::new(file);
        let to_io = |e: zip::result::ZipError| std::io::Error::other(e);
        zip.start_file("META-INF/MANIFEST.MF", options()).map_err(to_io)?;
        zip.write_all(manifest.render().as_bytes())?;

        let mut seen: HashSet<String> = HashSet::from(["META-INF/MANIFEST.MF".to_owned()]);
        let mut merged: BTreeMap<String, Vec<u8>> = BTreeMap::new();

        if classes_dir.is_dir() {
            let mut entries: Vec<_> = walkdir::WalkDir::new(classes_dir).into_iter().filter_map(|e| e.ok()).collect();
            entries.sort_by(|a, b| a.path().cmp(b.path()));
            for entry in entries {
                let rel = entry.path().strip_prefix(classes_dir).map_err(std::io::Error::other)?;
                let name = rel.to_string_lossy().replace('\\', "/");
                if name.is_empty() || name.ends_with(".fingerprint") {
                    continue;
                }
                if entry.file_type().is_dir() {
                    let dir = format!("{name}/");
                    if seen.insert(dir.clone()) {
                        zip.add_directory(dir, options()).map_err(to_io)?;
                    }
                } else if is_mergeable(&name) {
                    let data = std::fs::read(entry.path())?;
                    let buf = merged.entry(name).or_default();
                    buf.extend_from_slice(&data);
                    if !data.ends_with(b"\n") {
                        buf.push(b'\n');
                    }
                } else if seen.insert(name.clone()) {
                    zip.start_file(name, options()).map_err(to_io)?;
                    std::io::copy(&mut File::open(entry.path())?, &mut zip)?;
                }
            }
        }

        for jar in merge {
            let mut archive = zip::ZipArchive::new(File::open(jar)?).map_err(to_io)?;
            for i in 0..archive.len() {
                let raw = archive.by_index_raw(i).map_err(to_io)?;
                let name = raw.name().map_err(to_io)?.into_owned();
                if name == "META-INF/MANIFEST.MF"
                    || is_signature(&name)
                    || name.starts_with("META-INF/versions/") && name.ends_with("module-info.class")
                    || name == "module-info.class"
                {
                    continue;
                }
                if raw.is_dir() {
                    if seen.insert(name.clone()) {
                        zip.add_directory(name, options()).map_err(to_io)?;
                    }
                    continue;
                }
                if is_mergeable(&name) {
                    drop(raw);
                    let mut data = Vec::new();
                    archive.by_index(i).map_err(to_io)?.read_to_end(&mut data)?;
                    let buf = merged.entry(name).or_default();
                    buf.extend_from_slice(&data);
                    if !data.ends_with(b"\n") {
                        buf.push(b'\n');
                    }
                    continue;
                }
                if seen.insert(name) {
                    // Copy the compressed bytes as they are instead of inflating and deflating again.
                    zip.raw_copy_file_touch(raw, zip::DateTime::default(), None).map_err(to_io)?;
                }
            }
        }

        for (name, data) in merged {
            zip.start_file(name, options()).map_err(to_io)?;
            zip.write_all(&data)?;
        }
        zip.finish().map_err(to_io)?;
        Ok(())
    })
    .with_context(|| format!("failed to write {}", dest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_entry(jar: &Path, name: &str) -> Option<String> {
        let mut archive = zip::ZipArchive::new(File::open(jar).unwrap()).unwrap();
        let mut entry = archive.by_name(name).ok()?;
        let mut s = String::new();
        entry.read_to_string(&mut s).unwrap();
        Some(s)
    }

    fn make_jar(path: &Path, files: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, content) in files {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn manifest_wraps_long_lines() {
        let m = JarManifest {
            main_class: Some("com.example.MainKt".into()),
            class_path: (0..10).map(|i| format!("lib/dependency-number-{i}.jar")).collect(),
            ..Default::default()
        };
        let text = m.render();
        assert!(text.contains("Main-Class: com.example.MainKt\r\n"));
        for line in text.split("\r\n") {
            assert!(line.len() <= 72, "line too long: {line}");
        }
        let unwrapped = text.replace("\r\n ", "");
        assert!(unwrapped.contains("lib/dependency-number-9.jar"));
    }

    #[test]
    fn thin_and_fat_jars() {
        let dir = tempfile::tempdir().unwrap();
        let classes = dir.path().join("classes");
        std::fs::create_dir_all(classes.join("com/x")).unwrap();
        std::fs::create_dir_all(classes.join("META-INF/services")).unwrap();
        std::fs::write(classes.join("com/x/MainKt.class"), "class").unwrap();
        std::fs::write(classes.join("META-INF/services/a.Spi"), "impl.One").unwrap();

        let dep = dir.path().join("dep.jar");
        make_jar(
            &dep,
            &[
                ("META-INF/MANIFEST.MF", "Manifest-Version: 1.0\n"),
                ("META-INF/SIGNER.SF", "sig"),
                ("META-INF/services/a.Spi", "impl.Two\n"),
                ("com/x/MainKt.class", "dup should lose"),
                ("dep/Lib.class", "lib"),
            ],
        );

        let thin = dir.path().join("thin.jar");
        let manifest = JarManifest { main_class: Some("com.x.MainKt".into()), ..Default::default() };
        write_jar(&thin, &classes, &manifest, &[]).unwrap();
        assert_eq!(read_entry(&thin, "com/x/MainKt.class").unwrap(), "class");
        assert!(read_entry(&thin, "META-INF/MANIFEST.MF").unwrap().contains("Main-Class: com.x.MainKt"));

        let fat = dir.path().join("fat.jar");
        write_jar(&fat, &classes, &manifest, &[dep]).unwrap();
        assert_eq!(read_entry(&fat, "com/x/MainKt.class").unwrap(), "class", "project classes win");
        assert_eq!(read_entry(&fat, "dep/Lib.class").unwrap(), "lib");
        assert!(read_entry(&fat, "META-INF/SIGNER.SF").is_none(), "signatures are stripped");
        assert_eq!(read_entry(&fat, "META-INF/services/a.Spi").unwrap(), "impl.One\nimpl.Two\n");
        assert!(read_entry(&fat, "META-INF/MANIFEST.MF").unwrap().contains("Main-Class"));
    }

    #[test]
    fn manifest_comparison() {
        let dir = tempfile::tempdir().unwrap();
        let classes = dir.path().join("classes");
        std::fs::create_dir_all(&classes).unwrap();
        let jar = dir.path().join("app.jar");
        let manifest =
            JarManifest { main_class: Some("MainKt".into()), class_path: vec!["lib/kotlin-stdlib.jar".into()], ..Default::default() };
        assert!(!manifest_matches(&jar, &manifest), "a missing jar never matches");
        write_jar(&jar, &classes, &manifest, &[]).unwrap();
        assert!(manifest_matches(&jar, &manifest));

        let other_main = JarManifest { main_class: Some("OtherKt".into()), ..manifest.clone() };
        assert!(!manifest_matches(&jar, &other_main));
        let more_deps = JarManifest { class_path: vec!["lib/kotlin-stdlib.jar".into(), "lib/lib-1.0.jar".into()], ..manifest };
        assert!(!manifest_matches(&jar, &more_deps));
    }

    #[test]
    fn signature_detection() {
        assert!(is_signature("META-INF/BC.SF"));
        assert!(is_signature("META-INF/bc.rsa"));
        assert!(!is_signature("META-INF/services/x.SF"));
        assert!(is_mergeable("META-INF/spring/org.springframework.boot.autoconfigure.AutoConfiguration.imports"));
        assert!(!is_mergeable("META-INF/LICENSE"));
    }
}
