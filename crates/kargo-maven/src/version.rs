//! Maven version ordering (a port of Maven's `ComparableVersion`) and version ranges.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// A version string with Maven's ordering semantics.
///
/// `1.0-alpha1 < 1.0-beta < 1.0-rc1 < 1.0-SNAPSHOT < 1.0 == 1.0.0 < 1.0-sp1 < 1.0.1`
#[derive(Debug, Clone)]
pub struct Version {
    raw: String,
    items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Int(u128),
    Str(String),
    List(Vec<Item>),
}

const QUALIFIERS: [&str; 7] = ["alpha", "beta", "milestone", "rc", "snapshot", "", "sp"];
const RELEASE_INDEX: &str = "5";

fn comparable_qualifier(q: &str) -> String {
    match QUALIFIERS.iter().position(|x| *x == q) {
        Some(i) => i.to_string(),
        None => format!("{}-{q}", QUALIFIERS.len()),
    }
}

fn make_string_item(value: &str, followed_by_digit: bool) -> Item {
    let value = if followed_by_digit && value.len() == 1 {
        match value {
            "a" => "alpha",
            "b" => "beta",
            "m" => "milestone",
            other => other,
        }
    } else {
        value
    };
    let value = match value {
        "ga" | "final" | "release" => "",
        "cr" => "rc",
        other => other,
    };
    Item::Str(value.to_owned())
}

fn parse_item(is_digit: bool, s: &str) -> Item {
    if is_digit {
        // Extremely long numbers saturate rather than fail; ordering stays sane.
        Item::Int(s.trim_start_matches('0').parse().unwrap_or(if s.chars().all(|c| c == '0') { 0 } else { u128::MAX }))
    } else {
        make_string_item(s, false)
    }
}

impl Item {
    fn is_null(&self) -> bool {
        match self {
            Item::Int(v) => *v == 0,
            Item::Str(s) => comparable_qualifier(s) == RELEASE_INDEX,
            Item::List(l) => l.is_empty(),
        }
    }

    fn cmp_opt(&self, other: Option<&Item>) -> Ordering {
        match (self, other) {
            (Item::Int(v), None) => {
                if *v == 0 {
                    Ordering::Equal
                } else {
                    Ordering::Greater
                }
            }
            (Item::Str(s), None) => comparable_qualifier(s).cmp(&RELEASE_INDEX.to_owned()),
            (Item::List(l), None) => match l.first() {
                None => Ordering::Equal,
                Some(first) => first.cmp_opt(None),
            },
            (Item::Int(a), Some(Item::Int(b))) => a.cmp(b),
            (Item::Int(_), Some(_)) => Ordering::Greater,
            (Item::Str(_), Some(Item::Int(_))) => Ordering::Less,
            (Item::Str(a), Some(Item::Str(b))) => comparable_qualifier(a).cmp(&comparable_qualifier(b)),
            (Item::Str(_), Some(Item::List(_))) => Ordering::Less,
            (Item::List(_), Some(Item::Int(_))) => Ordering::Less,
            (Item::List(_), Some(Item::Str(_))) => Ordering::Greater,
            (Item::List(a), Some(Item::List(b))) => cmp_lists(a, b),
        }
    }
}

fn cmp_lists(a: &[Item], b: &[Item]) -> Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let result = match (a.get(i), b.get(i)) {
            (None, None) => Ordering::Equal,
            (None, Some(r)) => r.cmp_opt(None).reverse(),
            (Some(l), r) => l.cmp_opt(r),
        };
        if result != Ordering::Equal {
            return result;
        }
    }
    Ordering::Equal
}

fn normalize(list: &mut Vec<Item>) {
    let mut i = list.len();
    while i > 0 {
        i -= 1;
        if list[i].is_null() {
            list.remove(i);
        } else if !matches!(list[i], Item::List(_)) {
            break;
        }
    }
}

/// Builds the nested item tree. Lists are kept as a path of indices into the
/// root so that we never hold more than one mutable borrow at a time.
fn parse_items(version: &str) -> Vec<Item> {
    let version = version.to_lowercase();
    let mut root: Vec<Item> = Vec::new();
    // Path of indices from the root list to the list currently being filled.
    let mut path: Vec<usize> = Vec::new();

    fn current<'a>(root: &'a mut Vec<Item>, path: &[usize]) -> &'a mut Vec<Item> {
        let mut list = root;
        for &idx in path {
            list = match &mut list[idx] {
                Item::List(inner) => inner,
                _ => unreachable!("path always points at list items"),
            };
        }
        list
    }

    fn push_list(root: &mut Vec<Item>, path: &mut Vec<usize>) {
        let list = current(root, path);
        list.push(Item::List(Vec::new()));
        let idx = list.len() - 1;
        path.push(idx);
    }

    let chars: Vec<char> = version.chars().collect();
    let mut is_digit = false;
    let mut start = 0;
    let substr = |from: usize, to: usize| chars[from..to].iter().collect::<String>();

    for (i, &c) in chars.iter().enumerate() {
        if c == '.' {
            let item = if i == start { Item::Int(0) } else { parse_item(is_digit, &substr(start, i)) };
            current(&mut root, &path).push(item);
            start = i + 1;
        } else if c == '-' {
            let item = if i == start { Item::Int(0) } else { parse_item(is_digit, &substr(start, i)) };
            current(&mut root, &path).push(item);
            start = i + 1;
            push_list(&mut root, &mut path);
        } else if c.is_ascii_digit() {
            if !is_digit && i > start {
                current(&mut root, &path).push(make_string_item(&substr(start, i), true));
                start = i;
                push_list(&mut root, &mut path);
            }
            is_digit = true;
        } else {
            if is_digit && i > start {
                current(&mut root, &path).push(parse_item(true, &substr(start, i)));
                start = i;
                push_list(&mut root, &mut path);
            }
            is_digit = false;
        }
    }
    if chars.len() > start {
        current(&mut root, &path).push(parse_item(is_digit, &substr(start, chars.len())));
    }

    // Normalize innermost lists first, then the root.
    while !path.is_empty() {
        normalize(current(&mut root, &path));
        path.pop();
    }
    normalize(&mut root);
    root
}

impl Version {
    pub fn parse(raw: &str) -> Self {
        Self { raw: raw.to_owned(), items: parse_items(raw) }
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// True for SNAPSHOTs and alpha/beta/milestone/rc/dev/eap qualifiers.
    pub fn is_prerelease(&self) -> bool {
        let lower = self.raw.to_lowercase();
        if lower.ends_with("-snapshot") {
            return true;
        }
        lower.split(|c: char| c == '-' || c == '.' || c.is_ascii_digit()).filter(|s| !s.is_empty()).any(|tok| {
            matches!(tok, "alpha" | "a" | "beta" | "b" | "milestone" | "m" | "rc" | "cr" | "dev" | "eap" | "snapshot" | "pre" | "preview")
        })
    }

    /// The words in the version that are not release or pre-release qualifiers, joined by `-`:
    /// `jre` for `33.0.0-jre`, `x-compat` for `0.8.0-0.6.x-compat`, and empty for `1.2.0`,
    /// `2.0.0-RC1` or `6.4.0.Final`. Libraries publish such variants next to their plain versions.
    pub fn variant(&self) -> String {
        const QUALIFIER_WORDS: [&str; 18] = [
            "alpha",
            "a",
            "beta",
            "b",
            "milestone",
            "m",
            "rc",
            "cr",
            "dev",
            "eap",
            "snapshot",
            "pre",
            "preview",
            "final",
            "release",
            "ga",
            "sp",
            "v",
        ];
        let lower = self.raw.to_lowercase();
        let words: Vec<&str> =
            lower.split(|c: char| !c.is_ascii_alphabetic()).filter(|w| !w.is_empty() && !QUALIFIER_WORDS.contains(w)).collect();
        words.join("-")
    }

    /// The leading numeric components: `[1, 2]` for `1.2-RC1`.
    fn numeric_prefix(&self) -> Vec<u64> {
        let mut out = Vec::new();
        for part in self.raw.split('.') {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            let Ok(n) = digits.parse() else { break };
            out.push(n);
            if digits.len() != part.len() {
                break;
            }
        }
        out
    }

    /// Whether `other` is a compatible upgrade under semantic versioning: the same major
    /// version, or for `0.x` the same minor version (like Cargo's default requirements).
    pub fn is_compatible_with(&self, other: &Version) -> bool {
        let (a, b) = (self.numeric_prefix(), other.numeric_prefix());
        match (a.first(), b.first()) {
            (Some(0), Some(0)) => a.get(1).copied().unwrap_or(0) == b.get(1).copied().unwrap_or(0),
            (Some(x), Some(y)) => x == y,
            _ => self.raw == other.raw,
        }
    }
}

/// Picks the newest of `versions`, skipping pre-releases unless `allow_prerelease`, and preferring
/// versions whose [`Version::variant`] is `variant` (so `0.8.0` wins over `0.8.0-0.6.x-compat`
/// when `variant` is empty). Falls back to the newest version of any variant.
pub fn select_latest<'a>(versions: &'a [Version], variant: &str, allow_prerelease: bool) -> Option<&'a Version> {
    let candidates = || versions.iter().filter(|v| allow_prerelease || !v.is_prerelease());
    candidates().filter(|v| v.variant() == variant).max().or_else(|| candidates().max())
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_lists(&self.items, &other.items)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid version range `{0}`")]
pub struct RangeError(String);

/// One bound of a version range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    version: Version,
    inclusive: bool,
}

/// A single interval such as `[1.0,2.0)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interval {
    lower: Option<Bound>,
    upper: Option<Bound>,
}

impl Interval {
    fn contains(&self, v: &Version) -> bool {
        let lower_ok = self.lower.as_ref().is_none_or(|b| match v.cmp(&b.version) {
            Ordering::Greater => true,
            Ordering::Equal => b.inclusive,
            Ordering::Less => false,
        });
        let upper_ok = self.upper.as_ref().is_none_or(|b| match v.cmp(&b.version) {
            Ordering::Less => true,
            Ordering::Equal => b.inclusive,
            Ordering::Greater => false,
        });
        lower_ok && upper_ok
    }
}

/// A version requirement: either a soft "recommended" version or a range set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionReq {
    /// A plain version such as `1.2.3` (Maven treats it as a soft requirement).
    Soft(Version),
    /// One or more intervals, e.g. `[1.0,2.0),[3.0,)`.
    Ranges(Vec<Interval>),
}

impl VersionReq {
    pub fn is_range(&self) -> bool {
        matches!(self, VersionReq::Ranges(_))
    }

    pub fn matches(&self, v: &Version) -> bool {
        match self {
            VersionReq::Soft(s) => s == v,
            VersionReq::Ranges(ranges) => ranges.iter().any(|r| r.contains(v)),
        }
    }

    /// For a single-version pin like `[1.0]` or a soft version, the pinned version.
    pub fn exact(&self) -> Option<&Version> {
        match self {
            VersionReq::Soft(v) => Some(v),
            VersionReq::Ranges(r) if r.len() == 1 => match (&r[0].lower, &r[0].upper) {
                (Some(l), Some(u)) if l.inclusive && u.inclusive && l.version == u.version => Some(&l.version),
                _ => None,
            },
            VersionReq::Ranges(_) => None,
        }
    }

    /// Picks the highest candidate satisfying the requirement.
    pub fn select<'a>(&self, candidates: impl IntoIterator<Item = &'a Version>) -> Option<&'a Version> {
        candidates.into_iter().filter(|v| self.matches(v)).max()
    }
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionReq::Soft(v) => write!(f, "{v}"),
            VersionReq::Ranges(ranges) => {
                for (i, r) in ranges.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    let open = if r.lower.as_ref().is_none_or(|b| !b.inclusive) { '(' } else { '[' };
                    let close = if r.upper.as_ref().is_none_or(|b| !b.inclusive) { ')' } else { ']' };
                    let lower = r.lower.as_ref().map(|b| b.version.as_str()).unwrap_or("");
                    let upper = r.upper.as_ref().map(|b| b.version.as_str()).unwrap_or("");
                    if r.lower.is_some() && r.lower == r.upper {
                        write!(f, "[{lower}]")?;
                    } else {
                        write!(f, "{open}{lower},{upper}{close}")?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl FromStr for VersionReq {
    type Err = RangeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(RangeError(s.to_owned()));
        }
        if !s.starts_with(['[', '(']) {
            return Ok(VersionReq::Soft(Version::parse(s)));
        }
        let err = || RangeError(s.to_owned());
        let mut intervals = Vec::new();
        let mut rest = s;
        while !rest.is_empty() {
            let open = rest.chars().next().ok_or_else(err)?;
            if !matches!(open, '[' | '(') {
                return Err(err());
            }
            let close_idx = rest.find([']', ')']).ok_or_else(err)?;
            let close = rest[close_idx..].chars().next().ok_or_else(err)?;
            let body = &rest[1..close_idx];
            let lower_inclusive = open == '[';
            let upper_inclusive = close == ']';
            let interval = if let Some((lo, hi)) = body.split_once(',') {
                let (lo, hi) = (lo.trim(), hi.trim());
                if hi.contains(',') {
                    return Err(err());
                }
                let lower = (!lo.is_empty()).then(|| Bound { version: Version::parse(lo), inclusive: lower_inclusive });
                let upper = (!hi.is_empty()).then(|| Bound { version: Version::parse(hi), inclusive: upper_inclusive });
                if let (Some(l), Some(u)) = (&lower, &upper)
                    && l.version > u.version
                {
                    return Err(err());
                }
                Interval { lower, upper }
            } else {
                // `[1.0]` pins exactly; `(1.0)` is meaningless.
                if !(lower_inclusive && upper_inclusive) || body.trim().is_empty() {
                    return Err(err());
                }
                let v = Version::parse(body.trim());
                Interval { lower: Some(Bound { version: v.clone(), inclusive: true }), upper: Some(Bound { version: v, inclusive: true }) }
            };
            intervals.push(interval);
            rest = rest[close_idx + 1..].trim_start();
            if let Some(stripped) = rest.strip_prefix(',') {
                rest = stripped.trim_start();
                if rest.is_empty() {
                    return Err(err());
                }
            } else if !rest.is_empty() {
                return Err(err());
            }
        }
        Ok(VersionReq::Ranges(intervals))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s)
    }

    /// Asserts that each version is strictly less than the next.
    fn assert_ascending(versions: &[&str]) {
        for pair in versions.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} should be < {}", pair[0], pair[1]);
            assert!(v(pair[1]) > v(pair[0]), "{} should be > {}", pair[1], pair[0]);
        }
    }

    #[test]
    fn variants() {
        for plain in ["1.2.0", "2.0.0-RC1", "2.1.0-dev-5441", "1.0.0-beta02", "6.4.0.Final", "5.3.0.RELEASE", "1.0-SNAPSHOT"] {
            assert_eq!(v(plain).variant(), "", "{plain}");
        }
        assert_eq!(v("33.0.0-jre").variant(), "jre");
        assert_eq!(v("0.8.0-0.6.x-compat").variant(), "x-compat");
        assert_eq!(v("1.6.4-native-mt").variant(), "native-mt");
    }

    #[test]
    fn compatibility() {
        assert!(v("1.2.0").is_compatible_with(&v("1.9.3")));
        assert!(!v("1.2.0").is_compatible_with(&v("2.0.0")));
        assert!(v("0.6.0").is_compatible_with(&v("0.6.2")));
        assert!(!v("0.6.0").is_compatible_with(&v("0.7.0")));
        assert!(v("33.0.0-jre").is_compatible_with(&v("33.4.0-jre")));
        assert!(v("6.4.0.Final").is_compatible_with(&v("6.6.1.Final")));
        assert!(!v("abc").is_compatible_with(&v("1.0")));
    }

    #[test]
    fn latest_prefers_plain_versions() {
        let all: Vec<Version> = ["0.6.2", "0.7.1", "0.8.0", "0.8.0-0.6.x-compat", "0.9.0-RC1"].map(v).to_vec();
        assert_eq!(select_latest(&all, "", false).unwrap().as_str(), "0.8.0");
        assert_eq!(select_latest(&all, "", true).unwrap().as_str(), "0.9.0-RC1");
        assert_eq!(select_latest(&all, "x-compat", false).unwrap().as_str(), "0.8.0-0.6.x-compat");
        // Only variants published: take the newest of them.
        let guava: Vec<Version> = ["32.1.0-jre", "33.0.0-android", "33.0.0-jre"].map(v).to_vec();
        assert_eq!(select_latest(&guava, "", false).unwrap().as_str(), "33.0.0-jre");
        assert_eq!(select_latest(&guava, "android", false).unwrap().as_str(), "33.0.0-android");
        assert!(select_latest(&[v("1.0-RC1")], "", false).is_none());
    }

    #[test]
    fn maven_ordering() {
        assert_ascending(&[
            "1",
            "1.1",
            "1.2-alpha1",
            "1.2-alpha2",
            "1.2-beta-1",
            "1.2-milestone-1",
            "1.2-rc1",
            "1.2-SNAPSHOT",
            "1.2",
            "1.2-sp",
            "1.2.1",
            "1.10",
            "2.0",
        ]);
        assert_ascending(&["2.4.0-Beta1", "2.4.0-Beta2", "2.4.0-RC", "2.4.0", "2.4.10", "2.4.20", "2.4.21"]);
        assert_ascending(&["1.0-alpha", "1.0-a1", "1.0-b1", "1.0-m1", "1.0-cr1", "1.0"]);
    }

    #[test]
    fn maven_equality() {
        for (a, b) in
            [("1", "1.0"), ("1.0", "1.0.0"), ("1-ga", "1"), ("1.0-final", "1"), ("1.0-release", "1"), ("1cr", "1rc"), ("1.0.0-0", "1")]
        {
            assert_eq!(v(a), v(b), "{a} == {b}");
        }
    }

    #[test]
    fn numeric_not_lexical() {
        assert!(v("1.9") < v("1.10"));
        assert!(v("1.0.9") < v("1.0.10"));
        assert!(v("20240101") > v("9"));
        assert!(v("1.00000000000000000000000000000000000000001") > v("1.0"));
    }

    #[test]
    fn unknown_qualifiers_sort_after_release() {
        assert!(v("1.0-foo") > v("1.0"));
        assert!(v("1.0-foo") < v("1.0.1"));
        assert!(v("1.0-bar") < v("1.0-foo"));
    }

    #[test]
    fn prerelease_detection() {
        for pre in ["1.0-SNAPSHOT", "2.5.0-Beta1", "1.12.0-RC", "1.13.0-alpha02", "4.2.0-M2", "1.0-dev-123", "2.0.0-eap"] {
            assert!(v(pre).is_prerelease(), "{pre}");
        }
        for rel in ["1.0", "2.4.21", "1.18.48", "3.6.0", "1.0-jre", "33.0.0-android"] {
            assert!(!v(rel).is_prerelease(), "{rel}");
        }
    }

    #[test]
    fn soft_requirement() {
        let req: VersionReq = "1.2.3".parse().unwrap();
        assert!(!req.is_range());
        assert!(req.matches(&v("1.2.3")));
        assert_eq!(req.exact(), Some(&v("1.2.3")));
    }

    #[test]
    fn ranges() {
        let req: VersionReq = "[1.0,2.0)".parse().unwrap();
        assert!(req.matches(&v("1.0")));
        assert!(req.matches(&v("1.9.9")));
        assert!(!req.matches(&v("2.0")));
        assert!(!req.matches(&v("0.9")));

        let open: VersionReq = "(,1.0]".parse().unwrap();
        assert!(open.matches(&v("0.1")));
        assert!(open.matches(&v("1.0")));
        assert!(!open.matches(&v("1.0.1")));

        let pinned: VersionReq = "[1.5]".parse().unwrap();
        assert_eq!(pinned.exact(), Some(&v("1.5")));

        assert_eq!(req.to_string(), "[1.0,2.0)");
        assert_eq!(open.to_string(), "(,1.0]");
        assert_eq!(pinned.to_string(), "[1.5]");

        let multi: VersionReq = "(,1.0],[1.2,)".parse().unwrap();
        assert_eq!(multi.to_string(), "(,1.0],[1.2,)");
        assert!(multi.matches(&v("1.0")));
        assert!(!multi.matches(&v("1.1")));
        assert!(multi.matches(&v("5")));
    }

    #[test]
    fn range_selection_picks_highest_match() {
        let req: VersionReq = "[1.0,2.0)".parse().unwrap();
        let candidates = [v("0.9"), v("1.0"), v("1.5"), v("2.0"), v("1.10")];
        assert_eq!(req.select(&candidates).map(Version::as_str), Some("1.10"));
    }

    #[test]
    fn invalid_ranges() {
        for bad in ["", "[", "[1.0", "(1.0)", "[2.0,1.0]", "[1.0,2.0],", "[1.0,2.0]x", "[1,2,3]", "[]"] {
            assert!(bad.parse::<VersionReq>().is_err(), "{bad:?} should fail");
        }
    }
}
