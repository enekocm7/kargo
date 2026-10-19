//! Finds C libraries with their `pkg-config` files (`<name>.pc`), for the `pkg-config` key of
//! `[target.native.cinterop.<name>]`.
//!
//! Kargo reads the `.pc` files itself, so no `pkg-config` program is needed (it often is missing on
//! Windows and macOS). It looks in `PKG_CONFIG_PATH`, then where an installed `pkg-config` looks,
//! then in the usual places of Homebrew, MacPorts, vcpkg, MSYS2 and Linux distributions. Unlike
//! `pkg-config`, it keeps `-I/usr/include` and `-L/usr/lib/...`: the Kotlin/Native toolchain brings
//! its own sysroot, which doesn't search the system's directories.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The compiler and linker flags of a package and of the packages it requires.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flags {
    pub cflags: Vec<String>,
    pub libs: Vec<String>,
}

/// Where `.pc` files are looked for, in order.
pub fn search_path() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = std::env::var_os("PKG_CONFIG_PATH") {
        dirs.extend(std::env::split_paths(&path).filter(|p| !p.as_os_str().is_empty()));
    }
    // An installed pkg-config knows the distribution's own directories.
    if let Some(pkg_config) = kargo_util::process::find_on_path("pkg-config").or_else(|| kargo_util::process::find_on_path("pkgconf"))
        && let Ok(out) = std::process::Command::new(pkg_config).args(["--variable", "pc_path", "pkg-config"]).output()
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        dirs.extend(std::env::split_paths(text.trim()).filter(|p| !p.as_os_str().is_empty()));
    }
    dirs.extend(well_known_dirs());
    let mut seen = std::collections::BTreeSet::new();
    dirs.retain(|d| d.is_dir() && seen.insert(d.clone()));
    dirs
}

/// The `pkgconfig` directories of the usual package managers on this host.
fn well_known_dirs() -> Vec<PathBuf> {
    let mut prefixes: Vec<PathBuf> = Vec::new();
    let vcpkg_triplets: &[&str] = if cfg!(windows) {
        if let Some(prefix) = std::env::var_os("MSYSTEM_PREFIX") {
            prefixes.push(PathBuf::from(prefix));
        }
        for root in ["C:/msys64", "C:/tools/msys64"] {
            for env in ["ucrt64", "mingw64", "clang64"] {
                prefixes.push(Path::new(root).join(env));
            }
        }
        &["x64-mingw-dynamic", "x64-mingw-static"]
    } else if cfg!(target_os = "macos") {
        prefixes.extend(["/opt/homebrew", "/usr/local", "/opt/local"].map(PathBuf::from));
        if cfg!(target_arch = "aarch64") { &["arm64-osx"] } else { &["x64-osx"] }
    } else {
        prefixes.extend(["/usr/local", "/usr", "/home/linuxbrew/.linuxbrew"].map(PathBuf::from));
        if cfg!(target_arch = "aarch64") { &["arm64-linux"] } else { &["x64-linux"] }
    };
    if let Some(root) = std::env::var_os("VCPKG_ROOT") {
        for triplet in vcpkg_triplets {
            prefixes.push(Path::new(&root).join("installed").join(triplet));
        }
    }
    let mut dirs = Vec::new();
    for prefix in prefixes {
        dirs.push(prefix.join("lib").join("pkgconfig"));
        dirs.push(prefix.join("share").join("pkgconfig"));
        if cfg!(target_os = "linux") {
            let multiarch = if cfg!(target_arch = "aarch64") { "aarch64-linux-gnu" } else { "x86_64-linux-gnu" };
            dirs.push(prefix.join("lib").join(multiarch).join("pkgconfig"));
            dirs.push(prefix.join("lib64").join("pkgconfig"));
        }
    }
    dirs
}

/// The flags of `name` and the packages it requires. `link_static` adds what a static link needs:
/// `Libs.private` and the libraries of `Requires.private`.
pub fn probe(name: &str, dirs: &[PathBuf], link_static: bool) -> Result<Flags> {
    let mut flags = Flags::default();
    let mut visited = Vec::new();
    collect(name, dirs, link_static, true, &mut visited, &mut flags)?;
    dedup(&mut flags.cflags);
    dedup(&mut flags.libs);
    Ok(flags)
}

/// Keeps the first of repeated flags, except `-l` and lone words, whose order matters to linkers.
fn dedup(flags: &mut Vec<String>) {
    let mut seen = std::collections::BTreeSet::new();
    flags.retain(|f| !(f.starts_with("-I") || f.starts_with("-L") || f.starts_with("-D")) || seen.insert(f.clone()));
}

fn collect(name: &str, dirs: &[PathBuf], link_static: bool, libs: bool, visited: &mut Vec<String>, out: &mut Flags) -> Result<()> {
    if visited.iter().any(|v| v == name) {
        return Ok(());
    }
    visited.push(name.to_owned());
    let Some(file) = dirs.iter().map(|d| d.join(format!("{name}.pc"))).find(|f| f.is_file()) else {
        bail!(NotFound(name.to_owned()));
    };
    let pc = PcFile::read(&file)?;
    out.cflags.extend(split_flags(&pc.field("Cflags")));
    if libs {
        out.libs.extend(split_flags(&pc.field("Libs")));
        if link_static {
            out.libs.extend(split_flags(&pc.field("Libs.private")));
        }
    }
    for required in package_names(&pc.field("Requires")) {
        collect(&required, dirs, link_static, libs, visited, out).with_context(|| format!("required by {}", file.display()))?;
    }
    // Their headers are often included by the package's, so their compiler flags count either way.
    for required in package_names(&pc.field("Requires.private")) {
        collect(&required, dirs, link_static, libs && link_static, visited, out)
            .with_context(|| format!("required by {}", file.display()))?;
    }
    Ok(())
}

/// A package that no `.pc` file describes.
#[derive(Debug)]
pub struct NotFound(pub String);

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no pkg-config file `{}.pc` was found", self.0)
    }
}

impl std::error::Error for NotFound {}

/// How to install a missing library on this host.
pub fn install_hint(name: &str) -> String {
    if cfg!(windows) {
        format!(
            "install it with MSYS2 (`pacman -S mingw-w64-ucrt-x86_64-{name}`) or vcpkg (`vcpkg install {name}:x64-mingw-dynamic` with VCPKG_ROOT set)"
        )
    } else if cfg!(target_os = "macos") {
        format!("install it with Homebrew (`brew install {name}`)")
    } else {
        format!("install its development package (such as `apt install lib{name}-dev` or `dnf install {name}-devel`)")
    }
}

/// A parsed `.pc` file: its variables and fields.
#[derive(Debug, Default)]
struct PcFile {
    vars: BTreeMap<String, String>,
    fields: BTreeMap<String, String>,
}

impl PcFile {
    fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
        let dir = path.parent().unwrap_or(Path::new("."));
        Ok(Self::parse(&text, dir))
    }

    fn parse(text: &str, dir: &Path) -> Self {
        let mut pc = PcFile::default();
        let pcfiledir = kargo_util::process::slash(dir);
        pc.vars.insert("pcfiledir".to_owned(), pcfiledir);
        // Like pkgconf on Windows: a package moved with its prefix (MSYS2's `prefix=/ucrt64`) gets the
        // directory it is in, `<prefix>/lib/pkgconfig`.
        let relocated = cfg!(windows)
            .then(|| dir.parent().filter(|_| dir.ends_with("pkgconfig")).and_then(Path::parent))
            .flatten()
            .map(kargo_util::process::slash);
        let mut lines = text.lines().peekable();
        while let Some(line) = lines.next() {
            let mut line = line.to_owned();
            while line.ends_with('\\') && lines.peek().is_some() {
                line.pop();
                line.push_str(lines.next().unwrap_or_default());
            }
            let line = strip_comment(&line);
            let line = line.trim();
            let Some(at) = line.find([':', '=']) else { continue };
            let (key, value) = (line[..at].trim(), line[at + 1..].trim());
            if key.is_empty() || key.contains(char::is_whitespace) {
                continue;
            }
            if line.as_bytes()[at] == b'=' {
                let value = match (&relocated, key) {
                    (Some(prefix), "prefix") => prefix.clone(),
                    _ => pc.expand(value),
                };
                pc.vars.insert(key.to_owned(), value);
            } else {
                let value = pc.expand(value);
                // `CFlags` is a common misspelling that pkg-config accepts.
                let key = if key == "CFlags" { "Cflags" } else { key };
                pc.fields.insert(key.to_owned(), value);
            }
        }
        pc
    }

    fn field(&self, key: &str) -> String {
        self.fields.get(key).cloned().unwrap_or_default()
    }

    /// Replaces `${var}` with the variable's value; `$$` is a `$`.
    fn expand(&self, value: &str) -> String {
        let mut out = String::new();
        let mut rest = value;
        while let Some(at) = rest.find('$') {
            out.push_str(&rest[..at]);
            let after = &rest[at + 1..];
            if let Some(stripped) = after.strip_prefix('$') {
                out.push('$');
                rest = stripped;
            } else if let Some(inner) = after.strip_prefix('{')
                && let Some(end) = inner.find('}')
            {
                out.push_str(self.vars.get(&inner[..end]).map(String::as_str).unwrap_or_default());
                rest = &inner[end + 1..];
            } else {
                out.push('$');
                rest = after;
            }
        }
        out.push_str(rest);
        out
    }
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(at) => &line[..at],
        None => line,
    }
}

/// The package names of a `Requires` field, without their version constraints.
fn package_names(field: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut skip_version = false;
    for token in field.split([',', ' ', '\t']).filter(|t| !t.is_empty()) {
        if skip_version {
            skip_version = false;
            continue;
        }
        if matches!(token, "=" | "<" | ">" | "<=" | ">=" | "!=") {
            skip_version = true;
            continue;
        }
        names.push(token.to_owned());
    }
    names
}

/// Splits flags like a shell: on whitespace, with quotes and backslash escapes.
fn split_flags(text: &str) -> Vec<String> {
    let mut flags = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                current.extend(chars.next());
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    flags.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        flags.push(current);
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(format!("{name}.pc")), text).unwrap();
    }

    #[test]
    fn reads_flags_variables_and_requirements() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "sqlite3",
            "# sqlite\nprefix=/opt/sq\nlibdir=${prefix}/lib\nincludedir=${prefix}/include\n\nName: SQLite\nVersion: 3.45\n\
             Requires: zlib >= 1.2, base\nRequires.private: secret\nLibs: -L${libdir} -lsqlite3\nLibs.private: -lm -lpthread\n\
             Cflags: -I${includedir} -DSQLITE=\"a b\"\n",
        );
        write(dir.path(), "zlib", "prefix=/opt/z\nLibs: -L${prefix}/lib -lz\nCflags: -I${prefix}/include\n");
        write(dir.path(), "base", "Libs: -L/opt/z/lib -lbase\nCflags: -I/opt/z/include\n");
        write(dir.path(), "secret", "Libs: -lsecret\nCflags: -I/opt/secret\n");
        let dirs = vec![dir.path().to_path_buf()];

        let dynamic = probe("sqlite3", &dirs, false).unwrap();
        assert_eq!(
            dynamic.cflags,
            ["-I/opt/sq/include".to_owned(), "-DSQLITE=a b".into(), "-I/opt/z/include".into(), "-I/opt/secret".into()]
        );
        assert_eq!(dynamic.libs, ["-L/opt/sq/lib", "-lsqlite3", "-L/opt/z/lib", "-lz", "-lbase"]);

        let fully = probe("sqlite3", &dirs, true).unwrap();
        assert_eq!(fully.libs, ["-L/opt/sq/lib", "-lsqlite3", "-lm", "-lpthread", "-L/opt/z/lib", "-lz", "-lbase", "-lsecret"]);

        let err = probe("missing", &dirs, false).unwrap_err();
        assert!(err.downcast_ref::<NotFound>().is_some());
        write(dir.path(), "broken", "Requires: nowhere\n");
        let err = format!("{:#}", probe("broken", &dirs, false).unwrap_err());
        assert!(err.contains("no pkg-config file `nowhere.pc`") && err.contains("required by"), "{err}");
    }

    #[test]
    fn pcfiledir_and_relocation() {
        let dir = tempfile::tempdir().unwrap();
        let pkgconfig = dir.path().join("ucrt64").join("lib").join("pkgconfig");
        std::fs::create_dir_all(&pkgconfig).unwrap();
        write(&pkgconfig, "a", "prefix=/ucrt64\nLibs: -L${prefix}/lib -la\nCflags: -I${pcfiledir}/../../include\n");
        let flags = probe("a", std::slice::from_ref(&pkgconfig), false).unwrap();
        let root = kargo_util::process::slash(&dir.path().join("ucrt64"));
        if cfg!(windows) {
            assert_eq!(flags.libs, [format!("-L{root}/lib"), "-la".to_owned()]);
        } else {
            assert_eq!(flags.libs, ["-L/ucrt64/lib", "-la"]);
        }
        assert_eq!(flags.cflags, [format!("-I{}/../../include", kargo_util::process::slash(&pkgconfig))]);
    }

    #[test]
    fn parsing_details() {
        assert_eq!(split_flags(r#"-I/a\ b -D'X=1 2' "-lc""#), ["-I/a b", "-DX=1 2", "-lc"]);
        assert_eq!(package_names("glib-2.0 >= 2.50, gobject-2.0,zlib"), ["glib-2.0", "gobject-2.0", "zlib"]);
        let pc = PcFile::parse("a=1\nb=${a}$${a}\nCFlags: x \\\n y # comment\n", Path::new("/d"));
        assert_eq!(pc.vars["b"], "1${a}");
        assert_eq!(pc.field("Cflags"), "x  y");
        assert!(install_hint("sqlite3").contains("sqlite3"));
    }
}
