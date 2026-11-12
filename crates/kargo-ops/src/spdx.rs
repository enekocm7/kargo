//! SPDX license ids for `kargo licenses`: the free-text names and URLs that POMs use for their licenses
//! mapped to ids, and the `license` expressions of npm packages (`(MIT OR Apache-2.0)`) read into the
//! alternatives they offer.

/// What a license is called when it cannot be told: a name that is not in the table of [`from_pom`],
/// an expression that cannot be read, or no license at all.
pub const UNKNOWN: &str = "unknown";

/// The ids [`canonical`] spells, and the ones a `spdx.org/licenses/<id>` URL names.
const IDS: &[&str] = &[
    "0BSD",
    "AGPL-3.0-only",
    "AGPL-3.0-or-later",
    "Apache-1.1",
    "Apache-2.0",
    "Artistic-2.0",
    "BlueOak-1.0.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSD-4-Clause",
    "BSL-1.0",
    "CC-BY-4.0",
    "CC0-1.0",
    "CDDL-1.0",
    "CDDL-1.1",
    "EPL-1.0",
    "EPL-2.0",
    "GPL-2.0-only",
    "GPL-2.0-or-later",
    "GPL-3.0-only",
    "GPL-3.0-or-later",
    "ISC",
    "JSON",
    "LGPL-2.1-only",
    "LGPL-2.1-or-later",
    "LGPL-3.0-only",
    "LGPL-3.0-or-later",
    "MIT",
    "MIT-0",
    "MPL-1.1",
    "MPL-2.0",
    "Python-2.0",
    "Unlicense",
    "WTFPL",
    "Zlib",
];

/// The GPL of version 2 with the exception that lets Java applications link to it.
const GPL_CLASSPATH: &str = "GPL-2.0-only WITH Classpath-exception-2.0";

/// URLs of licenses, by a part of the URL without its scheme; the first that matches wins, so the
/// longer names (`lgpl`, `agpl`) come before the ones they contain.
const URLS: &[(&str, &str)] = &[
    ("apache.org/licenses/license-2.0", "Apache-2.0"),
    ("opensource.org/licenses/apache2.0", "Apache-2.0"),
    ("opensource.org/license/apache-2.0", "Apache-2.0"),
    ("opensource.org/licenses/mit", "MIT"),
    ("opensource.org/license/mit", "MIT"),
    ("mit-license.org", "MIT"),
    ("opensource.org/licenses/bsd-3-clause", "BSD-3-Clause"),
    ("opensource.org/license/bsd-3-clause", "BSD-3-Clause"),
    ("opensource.org/licenses/bsd-2-clause", "BSD-2-Clause"),
    ("opensource.org/license/bsd-2-clause", "BSD-2-Clause"),
    ("opensource.org/licenses/isc", "ISC"),
    ("eclipse.org/legal/epl-2.0", "EPL-2.0"),
    ("eclipse.org/legal/epl-v20", "EPL-2.0"),
    ("eclipse.org/legal/epl-v10", "EPL-1.0"),
    ("eclipse.org/legal/epl-1.0", "EPL-1.0"),
    ("eclipse.org/org/documents/epl-v10", "EPL-1.0"),
    ("eclipse.org/org/documents/edl-v10", "BSD-3-Clause"),
    ("mozilla.org/mpl/2.0", "MPL-2.0"),
    ("mozilla.org/en-us/mpl/2.0", "MPL-2.0"),
    ("mozilla.org/mpl/1.1", "MPL-1.1"),
    ("gnu.org/licenses/agpl-3.0", "AGPL-3.0-only"),
    ("gnu.org/licenses/lgpl-2.1", "LGPL-2.1-only"),
    ("gnu.org/licenses/lgpl-3.0", "LGPL-3.0-only"),
    ("gnu.org/licenses/old-licenses/lgpl-2.1", "LGPL-2.1-only"),
    ("gnu.org/licenses/gpl-2.0", "GPL-2.0-only"),
    ("gnu.org/licenses/gpl-3.0", "GPL-3.0-only"),
    ("gnu.org/licenses/old-licenses/gpl-2.0", "GPL-2.0-only"),
    ("creativecommons.org/publicdomain/zero/1.0", "CC0-1.0"),
    ("unlicense.org", "Unlicense"),
];

/// The id of a license from the `<name>` and `<url>` of a POM, for the common ones. The name decides;
/// the URL is asked when the name says nothing.
pub fn from_pom(name: Option<&str>, url: Option<&str>) -> Option<&'static str> {
    name.and_then(from_name).or_else(|| url.and_then(from_url))
}

fn from_url(url: &str) -> Option<&'static str> {
    let url = url.trim().to_ascii_lowercase();
    let url = url.split_once("://").map_or(url.as_str(), |(_, rest)| rest);
    let url = url.strip_prefix("www.").unwrap_or(url);
    if let Some((_, page)) = url.split_once("spdx.org/licenses/") {
        // SPDX names its pages by id: `Apache-2.0.html`.
        let id = page.trim_end_matches('/');
        let id = [".html", ".txt", ".json"].iter().find_map(|suffix| id.strip_suffix(suffix)).unwrap_or(id);
        return IDS.iter().copied().find(|known| known.eq_ignore_ascii_case(id));
    }
    URLS.iter().find(|(needle, _)| url.contains(needle)).map(|&(_, id)| id)
}

fn from_name(name: &str) -> Option<&'static str> {
    let name = name.trim();
    if let Some(id) = IDS.iter().copied().find(|id| id.eq_ignore_ascii_case(name)) {
        return Some(id);
    }
    let lower = name.to_ascii_lowercase();
    // Words, keeping the dots of versions: `LGPL v2.1+` is `lgpl`, `v2.1`.
    let spaced: String = lower.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { ' ' }).collect();
    let words: Vec<&str> = spaced.split_whitespace().map(|w| w.trim_matches('.')).filter(|w| !w.is_empty()).collect();
    let has = |word: &str| words.contains(&word);
    let version = words.iter().find_map(|w| {
        let digits = w.trim_start_matches(|c: char| c.is_ascii_alphabetic());
        (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.')).then_some(digits)
    });
    let is_version = |wanted: &[&str]| version.is_some_and(|v| wanted.contains(&v));
    let gpl = words.iter().any(|w| w.starts_with("gpl") || w.starts_with("lgpl") || w.starts_with("agpl"));

    let cddl = has("cddl") || (has("common") && has("development") && has("distribution"));
    if cddl {
        return match () {
            _ if gpl => None,
            _ if is_version(&["1.1"]) => Some("CDDL-1.1"),
            _ if is_version(&["1", "1.0"]) => Some("CDDL-1.0"),
            _ => None,
        };
    }
    if (has("eclipse") && has("distribution")) || has("edl") {
        return Some("BSD-3-Clause");
    }
    if (has("eclipse") && has("public")) || has("epl") {
        return if is_version(&["2", "2.0"]) {
            Some("EPL-2.0")
        } else if is_version(&["1", "1.0"]) {
            Some("EPL-1.0")
        } else {
            None
        };
    }
    if (has("mozilla") && has("public")) || has("mpl") {
        return if is_version(&["2", "2.0"]) {
            Some("MPL-2.0")
        } else if is_version(&["1.1"]) {
            Some("MPL-1.1")
        } else {
            None
        };
    }
    if has("apache") || has("asl") {
        return is_version(&["2", "2.0"]).then_some("Apache-2.0");
    }
    if gpl || (has("general") && has("public") && has("license")) {
        let family = if words.iter().any(|w| w.starts_with("agpl")) || has("affero") {
            "AGPL"
        } else if words.iter().any(|w| w.starts_with("lgpl")) || has("lesser") {
            "LGPL"
        } else {
            "GPL"
        };
        let later = has("later") || name.ends_with('+');
        return match (family, version?, later) {
            ("AGPL", "3" | "3.0", false) => Some("AGPL-3.0-only"),
            ("AGPL", "3" | "3.0", true) => Some("AGPL-3.0-or-later"),
            ("LGPL", "2.1", false) => Some("LGPL-2.1-only"),
            ("LGPL", "2.1", true) => Some("LGPL-2.1-or-later"),
            ("LGPL", "3" | "3.0", false) => Some("LGPL-3.0-only"),
            ("LGPL", "3" | "3.0", true) => Some("LGPL-3.0-or-later"),
            ("GPL", "2" | "2.0", false) if has("classpath") => Some(GPL_CLASSPATH),
            ("GPL", "2" | "2.0", false) => Some("GPL-2.0-only"),
            ("GPL", "2" | "2.0", true) => Some("GPL-2.0-or-later"),
            ("GPL", "3" | "3.0", false) => Some("GPL-3.0-only"),
            ("GPL", "3" | "3.0", true) => Some("GPL-3.0-or-later"),
            _ => None,
        };
    }
    if has("bsd") {
        return if has("3") || has("new") || has("revised") || has("modified") {
            Some("BSD-3-Clause")
        } else if has("2") || has("simplified") || has("freebsd") {
            Some("BSD-2-Clause")
        } else {
            None
        };
    }
    if has("mit") || (has("bouncy") && has("castle")) {
        return Some("MIT");
    }
    if has("cc0") || (has("creative") && has("commons") && has("zero")) {
        return Some("CC0-1.0");
    }
    if has("unlicense") {
        return Some("Unlicense");
    }
    if has("isc") {
        return Some("ISC");
    }
    if has("zlib") {
        return Some("Zlib");
    }
    if has("boost") {
        return Some("BSL-1.0");
    }
    has("wtfpl").then_some("WTFPL")
}

/// An SPDX id spelled the way [`IDS`] does (`apache-2.0` is `Apache-2.0`), with the deprecated `GPL-3.0`
/// and `GPL-3.0+` as the `-only` and `-or-later` ids they stand for. Anything else stays as written.
pub fn canonical(id: &str) -> String {
    let id = id.trim();
    let (base, later) = id.strip_suffix('+').map_or((id, false), |base| (base, true));
    let is_gnu = ["GPL-2.0", "GPL-3.0", "LGPL-2.1", "LGPL-3.0", "AGPL-3.0"].iter().any(|g| g.eq_ignore_ascii_case(base));
    let spelled = if is_gnu { format!("{base}-{}", if later { "or-later" } else { "only" }) } else { id.to_owned() };
    IDS.iter().find(|known| known.eq_ignore_ascii_case(&spelled)).map_or_else(|| id.to_owned(), |known| (*known).to_owned())
}

/// The alternatives an SPDX expression offers, each the licenses that apply together: `MIT OR Apache-2.0`
/// is `[[MIT], [Apache-2.0]]` and `(MIT OR GPL-2.0) AND BSD-3-Clause` is `[[MIT, BSD-3-Clause],
/// [GPL-2.0-only, BSD-3-Clause]]`. `None` for text that is not an expression, such as `SEE LICENSE IN
/// LICENSE.txt`.
pub fn parse_expression(text: &str) -> Option<Vec<Vec<String>>> {
    let spaced = text.replace('(', " ( ").replace(')', " ) ");
    let mut parser = Parser { tokens: spaced.split_whitespace().collect(), at: 0 };
    let alternatives = parser.or()?;
    (parser.at == parser.tokens.len()).then_some(alternatives)
}

struct Parser<'a> {
    tokens: Vec<&'a str>,
    at: usize,
}

impl Parser<'_> {
    fn peek_is(&self, word: &str) -> bool {
        self.tokens.get(self.at).is_some_and(|t| t.eq_ignore_ascii_case(word))
    }

    fn or(&mut self) -> Option<Vec<Vec<String>>> {
        let mut alternatives = self.and()?;
        while self.peek_is("or") {
            self.at += 1;
            alternatives.extend(self.and()?);
        }
        Some(alternatives)
    }

    fn and(&mut self) -> Option<Vec<Vec<String>>> {
        let mut alternatives = self.atom()?;
        while self.peek_is("and") {
            self.at += 1;
            let next = self.atom()?;
            alternatives =
                alternatives.iter().flat_map(|left| next.iter().map(move |right| left.iter().chain(right).cloned().collect())).collect();
        }
        Some(alternatives)
    }

    fn atom(&mut self) -> Option<Vec<Vec<String>>> {
        let token = *self.tokens.get(self.at)?;
        self.at += 1;
        if token == "(" {
            let inner = self.or()?;
            return self.peek_is(")").then(|| {
                self.at += 1;
                inner
            });
        }
        if [")", "and", "or", "with"].iter().any(|word| token.eq_ignore_ascii_case(word)) {
            return None;
        }
        let mut id = canonical(token);
        if self.peek_is("with") {
            let exception = *self.tokens.get(self.at + 1)?;
            id = format!("{id} WITH {exception}");
            self.at += 2;
        }
        Some(vec![vec![id]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_names_poms_use() {
        let names = [
            ("The Apache Software License, Version 2.0", "Apache-2.0"),
            ("Apache License, Version 2.0", "Apache-2.0"),
            ("Apache 2.0", "Apache-2.0"),
            ("Apache-2.0", "Apache-2.0"),
            ("apache license 2.0", "Apache-2.0"),
            ("ASL 2.0", "Apache-2.0"),
            ("MIT", "MIT"),
            ("MIT License", "MIT"),
            ("The MIT License (MIT)", "MIT"),
            ("Bouncy Castle Licence", "MIT"),
            ("BSD-3-Clause", "BSD-3-Clause"),
            ("The 3-Clause BSD License", "BSD-3-Clause"),
            ("New BSD License", "BSD-3-Clause"),
            ("Revised BSD", "BSD-3-Clause"),
            ("Eclipse Distribution License - v 1.0", "BSD-3-Clause"),
            ("BSD 2-Clause License", "BSD-2-Clause"),
            ("Simplified BSD License", "BSD-2-Clause"),
            ("Eclipse Public License - v 1.0", "EPL-1.0"),
            ("Eclipse Public License v2.0", "EPL-2.0"),
            ("EPL 2.0", "EPL-2.0"),
            ("Mozilla Public License, Version 2.0", "MPL-2.0"),
            ("MPL 1.1", "MPL-1.1"),
            ("GNU Lesser General Public License, Version 2.1", "LGPL-2.1-only"),
            ("LGPL 2.1 or later", "LGPL-2.1-or-later"),
            ("GNU Lesser General Public License v3.0", "LGPL-3.0-only"),
            ("LGPLv3+", "LGPL-3.0-or-later"),
            ("GNU General Public License, version 2", "GPL-2.0-only"),
            ("GNU General Public License, version 2 with the Classpath Exception", GPL_CLASSPATH),
            ("GPL-3.0", "GPL-3.0-only"),
            ("GNU Affero General Public License v3", "AGPL-3.0-only"),
            ("COMMON DEVELOPMENT AND DISTRIBUTION LICENSE (CDDL) Version 1.0", "CDDL-1.0"),
            ("CDDL 1.1", "CDDL-1.1"),
            ("CC0 1.0 Universal", "CC0-1.0"),
            ("Creative Commons Zero v1.0 Universal", "CC0-1.0"),
            ("The Unlicense", "Unlicense"),
            ("ISC License", "ISC"),
            ("zlib License", "Zlib"),
            ("Boost Software License 1.0", "BSL-1.0"),
        ];
        for (name, id) in names {
            assert_eq!(from_pom(Some(name), None), Some(id), "{name}");
        }
        for name in [
            "Public Domain",
            "BSD",
            "GNU General Public License",
            "GNU Lesser General Public License",
            "Eclipse Public License",
            "Apache License 1.1",
            "CDDL + GPLv2 with classpath exception",
            "Some Company Internal License",
            "",
        ] {
            assert_eq!(from_pom(Some(name), None), None, "{name:?}");
        }
    }

    #[test]
    fn maps_the_urls_poms_use_when_the_name_says_nothing() {
        let urls = [
            ("https://www.apache.org/licenses/LICENSE-2.0.txt", "Apache-2.0"),
            ("http://www.apache.org/licenses/LICENSE-2.0.html", "Apache-2.0"),
            ("https://opensource.org/licenses/MIT", "MIT"),
            ("https://opensource.org/license/mit/", "MIT"),
            ("https://www.eclipse.org/legal/epl-v10.html", "EPL-1.0"),
            ("https://www.eclipse.org/legal/epl-2.0/", "EPL-2.0"),
            ("http://www.eclipse.org/org/documents/edl-v10.php", "BSD-3-Clause"),
            ("https://www.mozilla.org/MPL/2.0/", "MPL-2.0"),
            ("https://www.gnu.org/licenses/lgpl-3.0.html", "LGPL-3.0-only"),
            ("https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html", "LGPL-2.1-only"),
            ("https://www.gnu.org/licenses/gpl-3.0.html", "GPL-3.0-only"),
            ("https://www.gnu.org/licenses/agpl-3.0.html", "AGPL-3.0-only"),
            ("https://creativecommons.org/publicdomain/zero/1.0/", "CC0-1.0"),
            ("https://spdx.org/licenses/BSD-3-Clause.html", "BSD-3-Clause"),
            ("https://spdx.org/licenses/Apache-2.0", "Apache-2.0"),
        ];
        for (url, id) in urls {
            assert_eq!(from_pom(None, Some(url)), Some(id), "{url}");
            assert_eq!(from_pom(Some("See the link"), Some(url)), Some(id), "{url}");
        }
        assert_eq!(from_pom(None, Some("https://example.com/license.txt")), None);
        assert_eq!(from_pom(Some("MIT"), Some("https://www.apache.org/licenses/LICENSE-2.0.txt")), Some("MIT"), "the name decides");
        assert_eq!(from_pom(None, None), None);
    }

    #[test]
    fn spells_ids_the_spdx_way() {
        assert_eq!(canonical("apache-2.0"), "Apache-2.0");
        assert_eq!(canonical(" mit "), "MIT");
        assert_eq!(canonical("GPL-3.0"), "GPL-3.0-only");
        assert_eq!(canonical("GPL-2.0+"), "GPL-2.0-or-later");
        assert_eq!(canonical("lgpl-2.1"), "LGPL-2.1-only");
        assert_eq!(canonical("GPL-3.0-or-later"), "GPL-3.0-or-later");
        assert_eq!(canonical("UNLICENSED"), "UNLICENSED");
        assert_eq!(canonical("LicenseRef-Custom"), "LicenseRef-Custom");
    }

    #[test]
    fn reads_expressions_into_alternatives() {
        let parse = |text: &str| parse_expression(text);
        assert_eq!(parse("MIT"), Some(vec![vec!["MIT".to_owned()]]));
        assert_eq!(parse("(MIT OR Apache-2.0)"), Some(vec![vec!["MIT".to_owned()], vec!["Apache-2.0".to_owned()]]));
        assert_eq!(parse("MIT AND Zlib"), Some(vec![vec!["MIT".to_owned(), "Zlib".to_owned()]]));
        assert_eq!(
            parse("(MIT OR GPL-2.0) AND BSD-3-Clause"),
            Some(vec![vec!["MIT".to_owned(), "BSD-3-Clause".to_owned()], vec!["GPL-2.0-only".to_owned(), "BSD-3-Clause".to_owned()]])
        );
        assert_eq!(parse("MIT OR (ISC AND Zlib)"), Some(vec![vec!["MIT".to_owned()], vec!["ISC".to_owned(), "Zlib".to_owned()]]));
        assert_eq!(parse("GPL-2.0 WITH Classpath-exception-2.0"), Some(vec![vec![GPL_CLASSPATH.to_owned()]]));
        assert_eq!(parse("GPL-2.0+"), Some(vec![vec!["GPL-2.0-or-later".to_owned()]]));
        assert_eq!(parse("mit or apache-2.0"), Some(vec![vec!["MIT".to_owned()], vec!["Apache-2.0".to_owned()]]));
        for text in ["", "SEE LICENSE IN LICENSE.txt", "MIT OR", "(MIT", "MIT)", "OR MIT", "MIT WITH", "AND"] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }
}
