//! npm's semantic versions and version ranges (`^1.3.0`, `~1.2`, `>=1 <2`, `1.x`, `1.2.3 - 2.0.0`, `||`), for
//! `kargo add --npm`, `kargo outdated` and `kargo upgrade`: ordering the versions a registry publishes, and
//! telling which of them a range in `kargo.toml` still accepts.
//!
//! Maven's version ordering (`kargo-maven`) is not npm's: `1.0.0-beta.10` sorts after `1.0.0-beta.2` here, and a
//! pre-release is below its release.

use std::cmp::Ordering;
use std::fmt;

/// One dot-separated piece of a pre-release: numbers sort below text, and numbers by value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Ident {
    Num(u64),
    Text(String),
}

/// A version such as `1.4.2` or `2.0.0-beta.1` (build metadata after `+` is dropped, it never counts).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Semver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pre: Vec<IdentKey>,
}

/// [`Ident`] made hashable for [`Semver`] (it is only ever compared through [`Ident`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IdentKey(String);

impl IdentKey {
    fn ident(&self) -> Ident {
        match self.0.parse::<u64>() {
            Ok(n) if !self.0.starts_with('0') || self.0 == "0" => Ident::Num(n),
            _ => Ident::Text(self.0.clone()),
        }
    }
}

impl Semver {
    /// Reads a full `major.minor.patch[-pre][+build]` version, with an optional leading `v` or `=`.
    pub fn parse(text: &str) -> Option<Semver> {
        let text = text.trim().trim_start_matches(['v', '=']).trim();
        let text = text.split('+').next()?;
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut parts = core.split('.');
        let (major, minor, patch) = (number(parts.next()?)?, number(parts.next()?)?, number(parts.next()?)?);
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => {
                let idents: Vec<&str> = pre.split('.').collect();
                if idents.iter().any(|i| i.is_empty() || !i.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')) {
                    return None;
                }
                idents.into_iter().map(|i| IdentKey(i.to_owned())).collect()
            }
        };
        Some(Semver { major, minor, patch, pre })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// The version with the pre-release dropped: the release it leads up to.
    fn core(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }

    fn release(major: u64, minor: u64, patch: u64) -> Semver {
        Semver { major, minor, patch, pre: Vec::new() }
    }
}

fn number(text: &str) -> Option<u64> {
    (!text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) && (text == "0" || !text.starts_with('0'))).then(|| text.parse().ok())?
}

impl Ord for Semver {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core().cmp(&other.core()).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                let (a, b): (Vec<Ident>, Vec<Ident>) =
                    (self.pre.iter().map(IdentKey::ident).collect(), other.pre.iter().map(IdentKey::ident).collect());
                a.cmp(&b)
            }
        })
    }
}

impl PartialOrd for Semver {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Semver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.iter().map(|i| i.0.as_str()).collect::<Vec<_>>().join("."))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Comparator {
    op: Op,
    version: Semver,
}

impl Comparator {
    fn matches(&self, v: &Semver) -> bool {
        let order = v.cmp(&self.version);
        match self.op {
            Op::Lt => order.is_lt(),
            Op::Le => order.is_le(),
            Op::Gt => order.is_gt(),
            Op::Ge => order.is_ge(),
            Op::Eq => order.is_eq(),
        }
    }
}

/// A version range: alternatives (`||`) of comparators that must all hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Range {
    sets: Vec<Vec<Comparator>>,
}

/// A version with the parts a range may leave out (`1`, `1.2`, `1.x`).
#[derive(Debug, Clone)]
struct Partial {
    major: Option<u64>,
    minor: Option<u64>,
    patch: Option<u64>,
    version: Option<Semver>,
}

fn partial(text: &str) -> Option<Partial> {
    let text = text.trim().trim_start_matches(['v', '=']);
    if let Some(version) = Semver::parse(text) {
        return Some(Partial {
            major: Some(version.major),
            minor: Some(version.minor),
            patch: Some(version.patch),
            version: Some(version),
        });
    }
    let wildcard = |p: &str| matches!(p, "x" | "X" | "*");
    let mut parts = text.split('.');
    let mut next = || -> Option<Option<u64>> {
        match parts.next() {
            None => Some(None),
            Some(p) if wildcard(p) => Some(None),
            Some(p) => number(p).map(Some),
        }
    };
    let (major, minor, patch) = (next()?, next()?, next()?);
    if parts.next().is_some() || (major.is_none() && (minor.is_some() || patch.is_some())) || (minor.is_none() && patch.is_some()) {
        return None;
    }
    Some(Partial { major, minor, patch, version: None })
}

fn bound(op: Op, major: u64, minor: u64, patch: u64) -> Comparator {
    Comparator { op, version: Semver::release(major, minor, patch) }
}

/// The comparators for one `[operator]version` token.
fn comparators(token: &str) -> Option<Vec<Comparator>> {
    let (op, rest) = ["~>", "^", "~", ">=", "<=", ">", "<", "="]
        .into_iter()
        .find_map(|op| token.strip_prefix(op).map(|rest| (op, rest)))
        .unwrap_or(("", token));
    let p = partial(rest)?;
    let any = || vec![bound(Op::Ge, 0, 0, 0)];
    let Some(major) = p.major else {
        return Some(match op {
            "<" => vec![bound(Op::Lt, 0, 0, 0)],
            ">" => vec![bound(Op::Lt, 0, 0, 0)],
            _ => any(),
        });
    };
    let (minor_x, patch_x) = (p.minor.is_none(), p.patch.is_none());
    let (minor, patch) = (p.minor.unwrap_or(0), p.patch.unwrap_or(0));
    let range = |lo: Comparator, hi: Comparator| Some(vec![lo, hi]);
    match op {
        "" | "=" => match p.version {
            Some(version) => Some(vec![Comparator { op: Op::Eq, version }]),
            None if minor_x => range(bound(Op::Ge, major, 0, 0), bound(Op::Lt, major + 1, 0, 0)),
            None => range(bound(Op::Ge, major, minor, 0), bound(Op::Lt, major, minor + 1, 0)),
        },
        "^" => {
            let lo = match p.version {
                Some(version) => Comparator { op: Op::Ge, version },
                None => bound(Op::Ge, major, minor, patch),
            };
            let hi = if major > 0 || (minor_x && major == 0) {
                bound(Op::Lt, major + 1, 0, 0)
            } else if minor > 0 || patch_x {
                bound(Op::Lt, 0, minor + 1, 0)
            } else {
                bound(Op::Lt, 0, 0, patch + 1)
            };
            range(lo, hi)
        }
        "~" | "~>" => {
            let lo = match p.version {
                Some(version) => Comparator { op: Op::Ge, version },
                None => bound(Op::Ge, major, minor, patch),
            };
            let hi = if minor_x { bound(Op::Lt, major + 1, 0, 0) } else { bound(Op::Lt, major, minor + 1, 0) };
            range(lo, hi)
        }
        ">" => Some(match p.version {
            Some(version) => vec![Comparator { op: Op::Gt, version }],
            None if minor_x => vec![bound(Op::Ge, major + 1, 0, 0)],
            None => vec![bound(Op::Ge, major, minor + 1, 0)],
        }),
        ">=" => Some(vec![match p.version {
            Some(version) => Comparator { op: Op::Ge, version },
            None => bound(Op::Ge, major, minor, patch),
        }]),
        "<" => Some(vec![match p.version {
            Some(version) => Comparator { op: Op::Lt, version },
            None => bound(Op::Lt, major, minor, patch),
        }]),
        "<=" => Some(vec![match p.version {
            Some(version) => Comparator { op: Op::Le, version },
            None if minor_x => bound(Op::Lt, major + 1, 0, 0),
            None => bound(Op::Lt, major, minor + 1, 0),
        }]),
        _ => None,
    }
}

impl Range {
    /// Reads an npm range. `None` for anything that is not one, such as a dist-tag (`next`), a URL or `file:`.
    pub fn parse(text: &str) -> Option<Range> {
        let mut sets = Vec::new();
        for alternative in text.split("||") {
            let alternative = alternative.trim();
            let mut set = Vec::new();
            if let Some((low, high)) = alternative.split_once(" - ") {
                let (low, high) = (partial(low)?, partial(high)?);
                set.push(match low.version {
                    Some(version) => Comparator { op: Op::Ge, version },
                    None => bound(Op::Ge, low.major.unwrap_or(0), low.minor.unwrap_or(0), 0),
                });
                match (high.version, high.major, high.minor) {
                    (Some(version), ..) => set.push(Comparator { op: Op::Le, version }),
                    (None, Some(major), Some(minor)) => set.push(bound(Op::Lt, major, minor + 1, 0)),
                    (None, Some(major), None) => set.push(bound(Op::Lt, major + 1, 0, 0)),
                    (None, None, _) => {}
                }
            } else {
                // `>= 1.2.3` is one comparator even though it has a space.
                let mut tokens: Vec<String> = Vec::new();
                for token in alternative.split_whitespace() {
                    match tokens.last_mut() {
                        Some(last) if matches!(last.as_str(), "^" | "~" | "~>" | ">=" | "<=" | ">" | "<" | "=") => last.push_str(token),
                        _ => tokens.push(token.to_owned()),
                    }
                }
                if tokens.is_empty() {
                    set.push(bound(Op::Ge, 0, 0, 0));
                }
                for token in tokens {
                    set.extend(comparators(&token)?);
                }
            }
            sets.push(set);
        }
        (!sets.is_empty()).then_some(Range { sets })
    }

    /// Whether `version` satisfies the range. A pre-release does only when a comparator of the same set names
    /// a pre-release of the same `major.minor.patch`, as in npm.
    pub fn matches(&self, version: &Semver) -> bool {
        self.sets.iter().any(|set| {
            set.iter().all(|c| c.matches(version))
                && (!version.is_prerelease() || set.iter().any(|c| c.version.is_prerelease() && c.version.core() == version.core()))
        })
    }

    /// The newest of `versions` that satisfies the range.
    pub fn newest<'v>(&self, versions: impl IntoIterator<Item = &'v Semver>) -> Option<&'v Semver> {
        versions.into_iter().filter(|v| self.matches(v)).max()
    }
}

/// A spec of `kargo.toml` with one version to raise: `1.4.2`, `^1.4.2`, `~1.4.2` or `=1.4.2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pinned {
    /// What stands before the version: empty, `^`, `~` or `=`.
    pub prefix: &'static str,
    pub version: Semver,
}

impl Pinned {
    /// `None` for every other kind of range (`>=1 <2`, `1.x`, `*`, a tag, a URL), which Kargo does not rewrite.
    pub fn parse(spec: &str) -> Option<Pinned> {
        let spec = spec.trim();
        let (prefix, rest) = ["^", "~", "="].into_iter().find_map(|p| spec.strip_prefix(p).map(|rest| (p, rest))).unwrap_or(("", spec));
        // A leading `v` would be lost when the version is rewritten.
        let rest = rest.trim();
        (!rest.starts_with('v')).then_some(Pinned { prefix, version: Semver::parse(rest)? })
    }

    /// The same kind of spec at `version`.
    pub fn with_version(&self, version: &Semver) -> String {
        format!("{}{version}", self.prefix)
    }

    /// The range of versions that keep working with this one: what the spec accepts, or for an exact version
    /// the same major (the same minor below 1.0), like a caret.
    pub fn compatible(&self) -> Range {
        let caret = format!("^{}", self.version);
        match self.prefix {
            "~" => Range::parse(&format!("~{}", self.version)),
            _ => Range::parse(&caret),
        }
        .expect("a version makes a range")
    }
}

/// The newest of `versions` for `current` and for anyone: `(compatible, latest)`, each only when newer than
/// `current`. Pre-releases count when `prerelease` is set or `current` is one. `tag` is the version the
/// registry calls `latest`, which wins over a higher version published on another line when it is stable.
pub fn newer(current: &Pinned, versions: &[Semver], tag: Option<&Semver>, prerelease: bool) -> (Option<Semver>, Option<Semver>) {
    let prerelease = prerelease || current.version.is_prerelease();
    let eligible: Vec<&Semver> = versions.iter().filter(|v| prerelease || !v.is_prerelease()).collect();
    let compatible = current.compatible().newest(eligible.iter().copied()).filter(|v| **v > current.version).cloned();
    let latest = match tag.filter(|t| prerelease || !t.is_prerelease()) {
        Some(tag) if !prerelease => Some(tag),
        _ => eligible.iter().copied().max(),
    }
    .filter(|v| **v > current.version)
    .cloned();
    (compatible, latest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Semver {
        Semver::parse(text).unwrap_or_else(|| panic!("{text} is a version"))
    }

    fn matching(range: &str, versions: &[&str]) -> Vec<String> {
        let range = Range::parse(range).unwrap_or_else(|| panic!("{range} is a range"));
        versions.iter().filter(|text| range.matches(&v(text))).map(|t| t.to_string()).collect()
    }

    const ALL: [&str; 9] = ["0.2.9", "0.3.0", "0.3.4", "1.0.0", "1.2.3", "1.9.0", "2.0.0", "2.0.1-beta.1", "2.1.0"];

    #[test]
    fn orders_like_npm() {
        assert!(v("1.0.0-beta.2") < v("1.0.0-beta.10"));
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert!(v("1.0.0-1") < v("1.0.0-alpha"), "numbers sort below text");
        assert_eq!(v("1.2.3+build.5"), v("1.2.3"));
        assert_eq!(v("v1.2.3").to_string(), "1.2.3");
        assert_eq!(v("2.0.0-beta.1").to_string(), "2.0.0-beta.1");
        for bad in ["1.2", "1.2.3.4", "01.2.3", "a.b.c", "1.2.3-", "latest", ""] {
            assert!(Semver::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn caret_and_tilde() {
        assert_eq!(matching("^1.2.3", &ALL), ["1.2.3", "1.9.0"]);
        assert_eq!(matching("^0.3.0", &ALL), ["0.3.0", "0.3.4"]);
        assert_eq!(matching("^0.0.3", &["0.0.3", "0.0.4"]), ["0.0.3"]);
        assert_eq!(matching("^1", &ALL), ["1.0.0", "1.2.3", "1.9.0"]);
        assert_eq!(matching("~1.2.3", &ALL), ["1.2.3"]);
        assert_eq!(matching("~1.2", &["1.2.0", "1.2.9", "1.3.0"]), ["1.2.0", "1.2.9"]);
        assert_eq!(matching("~1", &ALL), ["1.0.0", "1.2.3", "1.9.0"]);
    }

    #[test]
    fn comparison_wildcard_and_hyphen_ranges() {
        assert_eq!(matching(">=1.2.3 <2", &ALL), ["1.2.3", "1.9.0"]);
        assert_eq!(matching(">= 1.9.0", &["1.2.3", "1.9.0", "2.0.0"]), ["1.9.0", "2.0.0"]);
        assert_eq!(matching("1.x", &ALL), ["1.0.0", "1.2.3", "1.9.0"]);
        assert_eq!(matching("1.2.x", &ALL), ["1.2.3"]);
        assert_eq!(matching("*", &["0.1.0", "3.0.0"]), ["0.1.0", "3.0.0"]);
        assert_eq!(matching("", &["0.1.0"]), ["0.1.0"]);
        assert_eq!(matching("1.0.0 - 1.9", &ALL), ["1.0.0", "1.2.3", "1.9.0"]);
        assert_eq!(matching("<1.0.0 || >=2.1.0", &ALL), ["0.2.9", "0.3.0", "0.3.4", "2.1.0"]);
        assert_eq!(matching(">1.2", &ALL), ["1.9.0", "2.0.0", "2.1.0"]);
        assert_eq!(matching("<=1", &ALL), ["0.2.9", "0.3.0", "0.3.4", "1.0.0", "1.2.3", "1.9.0"]);
        assert_eq!(matching("1.2.3", &ALL), ["1.2.3"]);
        assert!(Range::parse("next").is_none());
        assert!(Range::parse("github:user/repo").is_none());
    }

    #[test]
    fn pre_releases_only_match_their_own_version() {
        assert_eq!(matching("^2.0.0", &ALL), ["2.0.0", "2.1.0"]);
        assert_eq!(matching("^2.0.1-beta.1", &ALL), ["2.0.1-beta.1", "2.1.0"]);
        assert_eq!(matching(">=2.0.0-0", &["2.0.0-beta.1", "2.0.0"]), ["2.0.0-beta.1", "2.0.0"]);
    }

    #[test]
    fn pinned_specs() {
        assert_eq!(Pinned::parse("^1.3.0").unwrap().with_version(&v("1.4.0")), "^1.4.0");
        assert_eq!(Pinned::parse("~1.3.0").unwrap().with_version(&v("1.3.9")), "~1.3.9");
        assert_eq!(Pinned::parse("1.3.0").unwrap().with_version(&v("2.0.0")), "2.0.0");
        for other in [">=1.3.0", "1.x", "*", "next", "^1.3", "1.0.0 || 2.0.0", "file:../x"] {
            assert!(Pinned::parse(other).is_none(), "{other}");
        }
    }

    #[test]
    fn finds_newer_versions() {
        let versions: Vec<Semver> = ["1.3.0", "1.4.2", "1.5.0-rc.1", "2.0.0", "2.1.0"].map(v).into();
        let tag = v("2.1.0");
        let (compatible, latest) = newer(&Pinned::parse("^1.3.0").unwrap(), &versions, Some(&tag), false);
        assert_eq!((compatible, latest), (Some(v("1.4.2")), Some(v("2.1.0"))));
        // An exact version is compatible within its major, like a caret.
        let (compatible, _) = newer(&Pinned::parse("1.3.0").unwrap(), &versions, Some(&tag), false);
        assert_eq!(compatible, Some(v("1.4.2")));
        // A tilde stays on its minor.
        let (compatible, latest) = newer(&Pinned::parse("~1.3.0").unwrap(), &versions, Some(&tag), false);
        assert_eq!((compatible, latest), (None, Some(v("2.1.0"))));
        // Pre-releases need the flag.
        let (compatible, _) = newer(&Pinned::parse("^1.4.2").unwrap(), &versions, Some(&tag), true);
        assert_eq!(compatible, None, "1.5.0-rc.1 is not in ^1.4.2");
        let (_, latest) = newer(&Pinned::parse("^2.1.0").unwrap(), &versions, Some(&tag), false);
        assert_eq!(latest, None);
        // The `latest` tag wins over a higher version of an older line only when it is stable.
        let tag = v("2.0.0");
        let (_, latest) = newer(&Pinned::parse("^1.3.0").unwrap(), &versions, Some(&tag), false);
        assert_eq!(latest, Some(v("2.0.0")));
    }
}
