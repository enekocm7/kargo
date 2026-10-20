//! Parsing of Maven POM files and construction of the "effective" model
//! (parent inheritance, property interpolation, dependency management).

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use super::coord::ModuleId;

/// Gradle writes this marker into POMs that have richer `.module` metadata.
pub const GRADLE_METADATA_MARKER: &str = "do_not_remove: published-with-gradle-metadata";

/// The POM property that names the main class of a published application. `kargo publish` writes it
/// for a package with a main class, and `kargo install group:artifact` runs that class.
pub const MAIN_CLASS_PROPERTY: &str = "kargo.main-class";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParentRef {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PomDependency {
    pub group: String,
    pub artifact: String,
    pub version: Option<String>,
    pub scope: Option<String>,
    pub classifier: Option<String>,
    pub kind: Option<String>,
    pub optional: bool,
    pub exclusions: Vec<(String, String)>,
}

impl PomDependency {
    pub fn module(&self) -> ModuleId {
        ModuleId::new(&self.group, &self.artifact)
    }

    /// Key used to match dependency management entries.
    fn management_key(&self) -> (String, String, Option<String>, String) {
        (self.group.clone(), self.artifact.clone(), self.classifier.clone(), self.kind.clone().unwrap_or_else(|| "jar".to_owned()))
    }

    pub fn is_bom_import(&self) -> bool {
        self.scope.as_deref() == Some("import") && self.kind.as_deref() == Some("pom")
    }
}

/// A raw POM as written on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pom {
    pub group: Option<String>,
    pub artifact: String,
    pub version: Option<String>,
    pub packaging: Option<String>,
    pub parent: Option<ParentRef>,
    pub properties: BTreeMap<String, String>,
    pub dependency_management: Vec<PomDependency>,
    pub dependencies: Vec<PomDependency>,
    pub relocation: Option<(Option<String>, Option<String>, Option<String>)>,
    pub has_gradle_metadata: bool,
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|n| n.is_element() && n.tag_name().name() == name)
}

fn text(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    child(node, name).and_then(|n| n.text()).map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

fn parse_dependency(node: roxmltree::Node<'_, '_>) -> Option<PomDependency> {
    let exclusions = child(node, "exclusions")
        .map(|ex| {
            ex.children()
                .filter(|n| n.is_element() && n.tag_name().name() == "exclusion")
                .filter_map(|n| Some((text(n, "groupId")?, text(n, "artifactId")?)))
                .collect()
        })
        .unwrap_or_default();
    Some(PomDependency {
        group: text(node, "groupId")?,
        artifact: text(node, "artifactId")?,
        version: text(node, "version"),
        scope: text(node, "scope"),
        classifier: text(node, "classifier"),
        kind: text(node, "type"),
        optional: text(node, "optional").is_some_and(|o| o.eq_ignore_ascii_case("true")),
        exclusions,
    })
}

fn parse_dependency_list(node: Option<roxmltree::Node<'_, '_>>) -> Vec<PomDependency> {
    node.map(|deps| {
        deps.children().filter(|n| n.is_element() && n.tag_name().name() == "dependency").filter_map(parse_dependency).collect()
    })
    .unwrap_or_default()
}

impl Pom {
    pub fn parse(xml: &str) -> Result<Self> {
        // Some old POMs carry a DTD or odd entities; roxmltree rejects DTDs by default.
        let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
        let doc = roxmltree::Document::parse_with_options(xml, options).context("malformed POM XML")?;
        let project = doc.root_element();
        if project.tag_name().name() != "project" {
            bail!("POM root element is `{}`, expected `project`", project.tag_name().name());
        }

        let parent = child(project, "parent")
            .and_then(|p| Some(ParentRef { group: text(p, "groupId")?, artifact: text(p, "artifactId")?, version: text(p, "version")? }));

        let properties = child(project, "properties")
            .map(|props| {
                props
                    .children()
                    .filter(|n| n.is_element())
                    .map(|n| (n.tag_name().name().to_owned(), n.text().unwrap_or("").trim().to_owned()))
                    .collect()
            })
            .unwrap_or_default();

        let dependency_management = parse_dependency_list(child(project, "dependencyManagement").and_then(|dm| child(dm, "dependencies")));

        let relocation = child(project, "distributionManagement")
            .and_then(|dm| child(dm, "relocation"))
            .map(|r| (text(r, "groupId"), text(r, "artifactId"), text(r, "version")));

        Ok(Self {
            group: text(project, "groupId"),
            artifact: text(project, "artifactId").context("POM is missing <artifactId>")?,
            version: text(project, "version"),
            packaging: text(project, "packaging"),
            parent,
            properties,
            dependency_management,
            dependencies: parse_dependency_list(child(project, "dependencies")),
            relocation,
            has_gradle_metadata: xml.contains(GRADLE_METADATA_MARKER),
        })
    }

    pub fn effective_group(&self) -> Option<&str> {
        self.group.as_deref().or(self.parent.as_ref().map(|p| p.group.as_str()))
    }

    pub fn effective_version(&self) -> Option<&str> {
        self.version.as_deref().or(self.parent.as_ref().map(|p| p.version.as_str()))
    }
}

/// The fully inherited, interpolated model of a POM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectivePom {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub packaging: String,
    pub dependencies: Vec<PomDependency>,
    pub dependency_management: Vec<PomDependency>,
    pub relocation: Option<(Option<String>, Option<String>, Option<String>)>,
    pub has_gradle_metadata: bool,
}

/// Source of POMs during effective-model construction.
pub trait PomSource {
    fn pom(&self, module: &ModuleId, version: &str) -> Result<Pom>;
}

const MAX_PARENT_DEPTH: usize = 32;
const MAX_IMPORT_DEPTH: usize = 16;

/// The POM of `module:version` followed by its parents, nearest first.
pub fn pom_chain(source: &dyn PomSource, module: &ModuleId, version: &str) -> Result<Vec<Pom>> {
    let leaf = source.pom(module, version).with_context(|| format!("failed to load POM for {module}:{version}"))?;
    let mut chain = vec![leaf];
    while let Some(parent) = chain.last().and_then(|p| p.parent.clone()) {
        if chain.len() > MAX_PARENT_DEPTH {
            bail!("parent POM chain too deep (cycle?) for {module}:{version}");
        }
        let parent_id = ModuleId::new(&parent.group, &parent.artifact);
        let parent_pom = source
            .pom(&parent_id, &parent.version)
            .with_context(|| format!("failed to load parent POM {parent_id}:{} of {module}:{version}", parent.version))?;
        chain.push(parent_pom);
    }
    Ok(chain)
}

/// The group, version and properties of the first POM of `chain`: the properties of every POM in it
/// (children override parents) plus the `project.*` ones Maven defines.
fn chain_properties(chain: &[Pom], module: &ModuleId, version: &str) -> (String, String, BTreeMap<String, String>) {
    let leaf = &chain[0];
    let group = leaf.effective_group().unwrap_or(&module.group).to_owned();
    let pom_version = leaf.effective_version().unwrap_or(version).to_owned();

    // Properties: ancestors first so children override.
    let mut props: BTreeMap<String, String> = BTreeMap::new();
    for pom in chain.iter().rev() {
        props.extend(pom.properties.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    let builtins = [
        ("project.groupId", group.clone()),
        ("pom.groupId", group.clone()),
        ("groupId", group.clone()),
        ("project.artifactId", leaf.artifact.clone()),
        ("pom.artifactId", leaf.artifact.clone()),
        ("artifactId", leaf.artifact.clone()),
        ("project.version", pom_version.clone()),
        ("pom.version", pom_version.clone()),
        ("version", pom_version.clone()),
    ];
    for (k, v) in builtins {
        props.insert(k.to_owned(), v);
    }
    if let Some(parent) = &leaf.parent {
        props.insert("project.parent.version".into(), parent.version.clone());
        props.insert("project.parent.groupId".into(), parent.group.clone());
        props.insert("parent.version".into(), parent.version.clone());
    }
    (group, pom_version, props)
}

/// The properties `${...}` placeholders in the POM of `module:version` resolve to: its own and its
/// parents' (children win), plus `project.groupId`, `project.version` and the other built-in ones.
pub fn effective_properties(source: &dyn PomSource, module: &ModuleId, version: &str) -> Result<BTreeMap<String, String>> {
    let chain = pom_chain(source, module, version)?;
    Ok(chain_properties(&chain, module, version).2)
}

/// Builds the effective POM for `module:version`.
pub fn effective_pom(source: &dyn PomSource, module: &ModuleId, version: &str) -> Result<EffectivePom> {
    effective_pom_inner(source, module, version, 0, true)
}

/// Like [`effective_pom`], but without reading the BOMs the POM imports: the versions they manage stay
/// out of the result. For callers that only want the inherited dependencies and the POM's own management.
pub fn effective_pom_without_imports(source: &dyn PomSource, module: &ModuleId, version: &str) -> Result<EffectivePom> {
    effective_pom_inner(source, module, version, 0, false)
}

fn effective_pom_inner(
    source: &dyn PomSource,
    module: &ModuleId,
    version: &str,
    import_depth: usize,
    follow_imports: bool,
) -> Result<EffectivePom> {
    if import_depth > MAX_IMPORT_DEPTH {
        bail!("BOM import chain too deep while processing {module}:{version}");
    }
    let chain = pom_chain(source, module, version)?;
    let leaf = &chain[0];
    let (group, pom_version, props) = chain_properties(&chain, module, version);

    let interp = |s: &str| interpolate(s, &props);
    let interp_dep = |d: &PomDependency| PomDependency {
        group: interp(&d.group),
        artifact: interp(&d.artifact),
        version: d.version.as_deref().map(interp),
        scope: d.scope.as_deref().map(interp),
        classifier: d.classifier.as_deref().map(interp),
        kind: d.kind.as_deref().map(interp),
        optional: d.optional,
        exclusions: d.exclusions.iter().map(|(g, a)| (interp(g), interp(a))).collect(),
    };

    // Dependency management: nearest declaration wins.
    let mut management: Vec<PomDependency> = Vec::new();
    let mut imports: Vec<PomDependency> = Vec::new();
    for pom in &chain {
        for dep in pom.dependency_management.iter().map(interp_dep) {
            if dep.is_bom_import() {
                imports.push(dep);
            } else if !management.iter().any(|m| m.management_key() == dep.management_key()) {
                management.push(dep);
            }
        }
    }
    for import in imports.into_iter().filter(|_| follow_imports) {
        let Some(import_version) = import.version.clone() else { continue };
        let bom = effective_pom_inner(source, &import.module(), &import_version, import_depth + 1, follow_imports)?;
        for dep in bom.dependency_management {
            if !management.iter().any(|m| m.management_key() == dep.management_key()) {
                management.push(dep);
            }
        }
    }

    // Dependencies: children override parents with the same key.
    let mut dependencies: Vec<PomDependency> = Vec::new();
    for pom in &chain {
        for dep in pom.dependencies.iter().map(interp_dep) {
            if !dependencies.iter().any(|d| d.management_key() == dep.management_key()) {
                dependencies.push(dep);
            }
        }
    }
    for dep in &mut dependencies {
        if let Some(managed) = management.iter().find(|m| m.management_key() == dep.management_key()) {
            if dep.version.is_none() {
                dep.version = managed.version.clone();
            }
            if dep.scope.is_none() {
                dep.scope = managed.scope.clone();
            }
            if dep.exclusions.is_empty() {
                dep.exclusions = managed.exclusions.clone();
            }
        }
    }

    Ok(EffectivePom {
        group,
        artifact: leaf.artifact.clone(),
        version: pom_version,
        packaging: leaf.packaging.as_deref().map(interp).unwrap_or_else(|| "jar".to_owned()),
        dependencies,
        dependency_management: management,
        relocation: leaf.relocation.clone(),
        has_gradle_metadata: leaf.has_gradle_metadata,
    })
}

/// Replaces `${name}` placeholders, resolving nested references. Unknown
/// properties are left untouched, as Maven does.
pub fn interpolate(input: &str, props: &BTreeMap<String, String>) -> String {
    let mut current = input.to_owned();
    for _ in 0..16 {
        if !current.contains("${") {
            break;
        }
        let mut out = String::with_capacity(current.len());
        let mut rest = current.as_str();
        let mut changed = false;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            match after.find('}') {
                Some(end) => {
                    let key = &after[..end];
                    match props.get(key) {
                        Some(value) => {
                            out.push_str(value);
                            changed = true;
                        }
                        None => {
                            out.push_str("${");
                            out.push_str(key);
                            out.push('}');
                        }
                    }
                    rest = &after[end + 1..];
                }
                None => {
                    out.push_str(&rest[start..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        current = out;
        if !changed {
            break;
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct MapSource(HashMap<String, &'static str>);

    impl PomSource for MapSource {
        fn pom(&self, module: &ModuleId, version: &str) -> Result<Pom> {
            let key = format!("{module}:{version}");
            let xml = self.0.get(&key).with_context(|| format!("missing {key}"))?;
            Pom::parse(xml)
        }
    }

    const PARENT: &str = r#"<?xml version="1.0"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <groupId>com.acme</groupId>
  <artifactId>parent</artifactId>
  <version>1.0</version>
  <packaging>pom</packaging>
  <properties>
    <lib.version>2.0</lib.version>
    <shared>parent-value</shared>
  </properties>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>com.acme</groupId>
        <artifactId>lib</artifactId>
        <version>${lib.version}</version>
      </dependency>
      <dependency>
        <groupId>com.acme</groupId>
        <artifactId>bom</artifactId>
        <version>3.0</version>
        <type>pom</type>
        <scope>import</scope>
      </dependency>
    </dependencies>
  </dependencyManagement>
  <dependencies>
    <dependency>
      <groupId>com.acme</groupId>
      <artifactId>inherited</artifactId>
      <version>1.1</version>
    </dependency>
  </dependencies>
</project>"#;

    const BOM: &str = r#"<project>
  <groupId>com.acme</groupId>
  <artifactId>bom</artifactId>
  <version>3.0</version>
  <dependencyManagement><dependencies>
    <dependency><groupId>com.other</groupId><artifactId>from-bom</artifactId><version>9.9</version></dependency>
  </dependencies></dependencyManagement>
</project>"#;

    const CHILD: &str = r#"<project>
  <!-- do_not_remove: published-with-gradle-metadata -->
  <parent>
    <groupId>com.acme</groupId>
    <artifactId>parent</artifactId>
    <version>1.0</version>
  </parent>
  <artifactId>child</artifactId>
  <properties><shared>child-value</shared></properties>
  <dependencies>
    <dependency>
      <groupId>com.acme</groupId>
      <artifactId>lib</artifactId>
    </dependency>
    <dependency>
      <groupId>com.other</groupId>
      <artifactId>from-bom</artifactId>
      <optional>true</optional>
      <exclusions>
        <exclusion><groupId>x</groupId><artifactId>y</artifactId></exclusion>
      </exclusions>
    </dependency>
    <dependency>
      <groupId>${project.groupId}</groupId>
      <artifactId>sibling</artifactId>
      <version>${project.version}</version>
      <scope>test</scope>
    </dependency>
    <dependency>
      <groupId>com.acme</groupId>
      <artifactId>uses-prop</artifactId>
      <version>${shared}</version>
    </dependency>
  </dependencies>
</project>"#;

    fn source() -> MapSource {
        MapSource(HashMap::from([
            ("com.acme:parent:1.0".to_owned(), PARENT),
            ("com.acme:bom:3.0".to_owned(), BOM),
            ("com.acme:child:1.0".to_owned(), CHILD),
        ]))
    }

    #[test]
    fn parses_raw_pom() {
        let pom = Pom::parse(CHILD).unwrap();
        assert!(pom.has_gradle_metadata);
        assert_eq!(pom.artifact, "child");
        assert_eq!(pom.group, None);
        assert_eq!(pom.effective_group(), Some("com.acme"));
        assert_eq!(pom.effective_version(), Some("1.0"));
        assert_eq!(pom.dependencies.len(), 4);
        assert!(pom.dependencies[1].optional);
        assert_eq!(pom.dependencies[1].exclusions, vec![("x".to_owned(), "y".to_owned())]);
    }

    #[test]
    fn builds_effective_pom() {
        let eff = effective_pom(&source(), &ModuleId::new("com.acme", "child"), "1.0").unwrap();
        assert_eq!(eff.group, "com.acme");
        assert_eq!(eff.version, "1.0");
        assert_eq!(eff.packaging, "jar");
        assert!(eff.has_gradle_metadata);

        let find = |a: &str| eff.dependencies.iter().find(|d| d.artifact == a).unwrap();
        assert_eq!(find("lib").version.as_deref(), Some("2.0"), "managed version via parent property");
        assert_eq!(find("from-bom").version.as_deref(), Some("9.9"), "managed version via imported BOM");
        assert_eq!(find("sibling").group, "com.acme");
        assert_eq!(find("sibling").version.as_deref(), Some("1.0"));
        assert_eq!(find("sibling").scope.as_deref(), Some("test"));
        assert_eq!(find("uses-prop").version.as_deref(), Some("child-value"), "child property overrides parent");
        assert_eq!(find("inherited").version.as_deref(), Some("1.1"), "dependencies inherited from parent");
    }

    #[test]
    fn effective_pom_without_imports_leaves_the_boms_unread() {
        // `from-bom` is managed only by the imported BOM, and the BOM is not even available.
        let mut sources = source();
        sources.0.remove("com.acme:bom:3.0");
        let eff = effective_pom_without_imports(&sources, &ModuleId::new("com.acme", "child"), "1.0").unwrap();
        let find = |a: &str| eff.dependencies.iter().find(|d| d.artifact == a).unwrap();
        assert_eq!(find("lib").version.as_deref(), Some("2.0"));
        assert_eq!(find("from-bom").version, None);
        assert!(effective_pom(&sources, &ModuleId::new("com.acme", "child"), "1.0").is_err());
    }

    #[test]
    fn interpolation() {
        let props = BTreeMap::from([
            ("a".to_owned(), "${b}".to_owned()),
            ("b".to_owned(), "1.0".to_owned()),
            ("loop".to_owned(), "${loop}".to_owned()),
        ]);
        assert_eq!(interpolate("v${a}-x", &props), "v1.0-x");
        assert_eq!(interpolate("${missing}", &props), "${missing}");
        assert_eq!(interpolate("${unterminated", &props), "${unterminated");
        assert_eq!(interpolate("${loop}", &props), "${loop}");
    }

    #[test]
    fn missing_parent_is_an_error() {
        let src = MapSource(HashMap::from([("com.acme:child:1.0".to_owned(), CHILD)]));
        let err = effective_pom(&src, &ModuleId::new("com.acme", "child"), "1.0").unwrap_err();
        assert!(format!("{err:#}").contains("parent POM"), "{err:#}");
    }

    #[test]
    fn rejects_non_pom_xml() {
        assert!(Pom::parse("<settings/>").is_err());
        assert!(Pom::parse("not xml").is_err());
        assert!(Pom::parse("<project><groupId>x</groupId></project>").is_err());
    }

    #[test]
    fn parses_relocation() {
        let pom = Pom::parse(
            "<project><artifactId>old</artifactId><distributionManagement><relocation><groupId>new.group</groupId></relocation></distributionManagement></project>",
        )
        .unwrap();
        assert_eq!(pom.relocation, Some((Some("new.group".into()), None, None)));
    }
}
