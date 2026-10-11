//! Maven coordinates: `group:artifact[:version][:classifier][@extension]`.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid Maven coordinate `{input}`: {reason}")]
pub struct CoordError {
    input: String,
    reason: &'static str,
}

impl CoordError {
    fn new(input: &str, reason: &'static str) -> Self {
        Self { input: input.to_owned(), reason }
    }
}

/// A module identity without a version: `group:artifact`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId {
    pub group: String,
    pub artifact: String,
}

impl ModuleId {
    pub fn new(group: impl Into<String>, artifact: impl Into<String>) -> Self {
        Self { group: group.into(), artifact: artifact.into() }
    }

    /// Repository-relative directory, e.g. `org/jetbrains/kotlin/kotlin-stdlib`.
    pub fn repo_dir(&self) -> String {
        format!("{}/{}", self.group.replace('.', "/"), self.artifact)
    }
}

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.group, self.artifact)
    }
}

impl FromStr for ModuleId {
    type Err = CoordError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split(':');
        let (Some(group), Some(artifact), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(CoordError::new(s, "expected `group:artifact`"));
        };
        validate_part(s, group)?;
        validate_part(s, artifact)?;
        Ok(Self::new(group, artifact))
    }
}

fn validate_part(input: &str, part: &str) -> Result<(), CoordError> {
    if part.is_empty() {
        return Err(CoordError::new(input, "empty coordinate component"));
    }
    if part.chars().any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '@' | ':')) {
        return Err(CoordError::new(input, "coordinate components must not contain whitespace, slashes, `@` or `:`"));
    }
    if part == "." || part == ".." {
        return Err(CoordError::new(input, "coordinate components must not be `.` or `..`"));
    }
    Ok(())
}

/// A fully specified artifact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Artifact {
    pub module: ModuleId,
    pub version: String,
    pub classifier: Option<String>,
    pub extension: String,
}

impl Artifact {
    pub fn new(module: ModuleId, version: impl Into<String>) -> Self {
        Self { module, version: version.into(), classifier: None, extension: "jar".to_owned() }
    }

    pub fn with_extension(mut self, extension: impl Into<String>) -> Self {
        self.extension = extension.into();
        self
    }

    pub fn with_classifier(mut self, classifier: Option<String>) -> Self {
        self.classifier = classifier;
        self
    }

    pub fn file_name(&self) -> String {
        match &self.classifier {
            Some(c) => format!("{}-{}-{}.{}", self.module.artifact, self.version, c, self.extension),
            None => format!("{}-{}.{}", self.module.artifact, self.version, self.extension),
        }
    }

    /// Repository-relative path of the artifact file.
    pub fn repo_path(&self) -> String {
        format!("{}/{}/{}", self.module.repo_dir(), self.version, self.file_name())
    }

    /// Path of a sibling file in the same version directory (e.g. the `.pom`).
    pub fn sibling_path(&self, file_name: &str) -> String {
        format!("{}/{}/{}", self.module.repo_dir(), self.version, file_name)
    }
}

impl fmt::Display for Artifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.module, self.version)?;
        if let Some(c) = &self.classifier {
            write!(f, ":{c}")?;
        }
        if self.extension != "jar" {
            write!(f, "@{}", self.extension)?;
        }
        Ok(())
    }
}

/// A user-supplied dependency spec: `group:artifact[:version[:classifier]][@ext]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordSpec {
    pub module: ModuleId,
    pub version: Option<String>,
    pub classifier: Option<String>,
    pub extension: Option<String>,
}

impl FromStr for CoordSpec {
    type Err = CoordError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (body, extension) = match s.split_once('@') {
            Some((body, ext)) if !ext.is_empty() => (body, Some(ext.to_owned())),
            Some(_) => return Err(CoordError::new(s, "empty extension after `@`")),
            None => (s, None),
        };
        let parts: Vec<&str> = body.split(':').collect();
        if !(2..=4).contains(&parts.len()) {
            return Err(CoordError::new(s, "expected `group:artifact[:version[:classifier]]`"));
        }
        for part in &parts {
            validate_part(s, part)?;
        }
        Ok(Self {
            module: ModuleId::new(parts[0], parts[1]),
            version: parts.get(2).map(|v| (*v).to_owned()),
            classifier: parts.get(3).map(|c| (*c).to_owned()),
            extension,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_module_id() {
        let id: ModuleId = "org.jetbrains.kotlin:kotlin-stdlib".parse().unwrap();
        assert_eq!(id.group, "org.jetbrains.kotlin");
        assert_eq!(id.artifact, "kotlin-stdlib");
        assert_eq!(id.repo_dir(), "org/jetbrains/kotlin/kotlin-stdlib");
        assert_eq!(id.to_string(), "org.jetbrains.kotlin:kotlin-stdlib");
    }

    #[test]
    fn rejects_bad_module_ids() {
        for bad in ["", "a", "a:", ":b", "a:b:c", "a b:c", "a/b:c", "..:x"] {
            assert!(bad.parse::<ModuleId>().is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn parses_full_spec() {
        let spec: CoordSpec = "com.x:y:1.0:natives@zip".parse().unwrap();
        assert_eq!(spec.module, ModuleId::new("com.x", "y"));
        assert_eq!(spec.version.as_deref(), Some("1.0"));
        assert_eq!(spec.classifier.as_deref(), Some("natives"));
        assert_eq!(spec.extension.as_deref(), Some("zip"));

        let bare: CoordSpec = "com.x:y".parse().unwrap();
        assert_eq!(bare.version, None);
        assert!("com.x:y:1:2:3".parse::<CoordSpec>().is_err());
        assert!("com.x:y@".parse::<CoordSpec>().is_err());
    }

    #[test]
    fn artifact_paths() {
        let a = Artifact::new(ModuleId::new("io.ktor", "ktor-io"), "3.0.0");
        assert_eq!(a.repo_path(), "io/ktor/ktor-io/3.0.0/ktor-io-3.0.0.jar");
        let k = a.clone().with_extension("klib").with_classifier(Some("cinterop".into()));
        assert_eq!(k.file_name(), "ktor-io-3.0.0-cinterop.klib");
        assert_eq!(k.to_string(), "io.ktor:ktor-io:3.0.0:cinterop@klib");
        assert_eq!(a.sibling_path("ktor-io-3.0.0.pom"), "io/ktor/ktor-io/3.0.0/ktor-io-3.0.0.pom");
    }
}
