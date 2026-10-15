//! Tokens of a TypeScript declaration file (`.d.ts`). Comments are dropped, except that the
//! last `/** ... */` before a token is kept as its documentation.

/// A token's kind and text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    /// A template literal type (`` `prefix-${string}` ``), kept as a whole.
    Template,
    Punct(&'static str),
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    /// Whether a line break comes before the token (members may end at one).
    pub newline: bool,
    /// The JSDoc comment right before the token.
    pub doc: Option<String>,
}

/// Punctuation, longest first.
const PUNCTS: [&str; 33] = [
    "...", "=>", "?.", "??", "&&", "||", "==", "!=", "<=", ">=", "{", "}", "(", ")", "[", "]", "<", ">", ",", ";", ":", "=", ".", "?", "|",
    "&", "!", "@", "#", "+", "-", "*", "/",
];

pub fn lex(src: &str) -> Vec<Token> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut newline = false;
    let mut doc: Option<String> = None;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            newline = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() || c == '\u{feff}' {
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let start = i + 2;
            let mut end = start;
            while end + 1 < chars.len() && !(chars[end] == '*' && chars[end + 1] == '/') {
                end += 1;
            }
            let body: String = chars[start..end.min(chars.len())].iter().collect();
            if let Some(text) = body.strip_prefix('*') {
                doc = Some(clean_doc(text));
            }
            i = (end + 2).min(chars.len());
            continue;
        }
        let tok = if c == '"' || c == '\'' {
            let (s, next) = string(&chars, i);
            i = next;
            Tok::Str(s)
        } else if c == '`' {
            i = template(&chars, i);
            Tok::Template
        } else if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_') {
                i += 1;
            }
            Tok::Num(chars[start..i].iter().collect())
        } else if is_ident_start(c) {
            let start = i;
            while i < chars.len() && is_ident_part(chars[i]) {
                i += 1;
            }
            Tok::Ident(chars[start..i].iter().collect())
        } else if let Some(p) = PUNCTS.iter().find(|p| p.chars().enumerate().all(|(k, pc)| chars.get(i + k) == Some(&pc))) {
            i += p.chars().count();
            Tok::Punct(p)
        } else {
            // Anything else (`%`, `^`, `~`) never matters in a declaration.
            i += 1;
            continue;
        };
        out.push(Token { tok, newline, doc: doc.take() });
        newline = false;
    }
    out.push(Token { tok: Tok::Eof, newline: true, doc: None });
    out
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}

fn is_ident_part(c: char) -> bool {
    is_ident_start(c) || c.is_alphanumeric()
}

/// A quoted string at `i`, unescaped, and the index after it.
fn string(chars: &[char], mut i: usize) -> (String, usize) {
    let quote = chars[i];
    i += 1;
    let mut s = String::new();
    while i < chars.len() && chars[i] != quote && chars[i] != '\n' {
        if chars[i] == '\\' && i + 1 < chars.len() {
            i += 1;
            s.push(match chars[i] {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
        } else {
            s.push(chars[i]);
        }
        i += 1;
    }
    (s, i + 1)
}

/// The index after the template literal at `i`, skipping `${ ... }` parts (which may nest).
fn template(chars: &[char], mut i: usize) -> usize {
    i += 1;
    while i < chars.len() && chars[i] != '`' {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
            let mut depth = 0;
            while i < chars.len() {
                match chars[i] {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    '`' => i = template(chars, i) - 1,
                    _ => {}
                }
                i += 1;
            }
        }
        i += 1;
    }
    i + 1
}

/// The text of a JSDoc comment without its leading `*`s.
fn clean_doc(body: &str) -> String {
    let lines: Vec<&str> = body
        .lines()
        .map(|l| {
            let l = l.trim();
            let l = l.strip_prefix('*').unwrap_or(l);
            l.strip_prefix(' ').unwrap_or(l).trim_end()
        })
        .collect();
    let start = lines.iter().position(|l| !l.is_empty()).unwrap_or(lines.len());
    let end = lines.iter().rposition(|l| !l.is_empty()).map_or(start, |e| e + 1);
    lines[start..end].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        lex(src).into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn tokens_and_docs() {
        let tokens = lex("/** Says hi.\n * Twice. */\nexport function hi(a?: string): void; // done");
        assert_eq!(tokens[0].tok, Tok::Ident("export".into()));
        assert_eq!(tokens[0].doc.as_deref(), Some("Says hi.\nTwice."));
        assert!(tokens[1].doc.is_none());
        assert_eq!(tokens[5].tok, Tok::Punct("?"));
        assert_eq!(tokens.last().unwrap().tok, Tok::Eof);
    }

    #[test]
    fn strings_templates_and_spread() {
        assert_eq!(
            kinds(r#"'a\'b' `x${`y${1}`}z` ...rest =>"#),
            [Tok::Str("a'b".into()), Tok::Template, Tok::Punct("..."), Tok::Ident("rest".into()), Tok::Punct("=>"), Tok::Eof]
        );
    }

    #[test]
    fn closing_generics_are_separate() {
        assert_eq!(kinds("A<B<C>>").iter().filter(|t| **t == Tok::Punct(">")).count(), 2);
    }
}
