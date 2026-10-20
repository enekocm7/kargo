//! A small reader for Gradle build scripts, Kotlin DSL and Groovy alike.
//!
//! Gradle scripts are programs, so this reads their *shape*: blocks, calls and the strings and
//! names in their arguments. That is enough for `plugins {}`, `dependencies {}`, `kotlin {}` and the
//! other declarative parts, and it ignores what it does not understand.

use std::collections::HashMap;

/// A token of a script.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    /// A string literal's contents, with escapes resolved and `$` templates untouched.
    Str(String),
    Num(String),
    Punct(char),
    Newline,
}

/// Splits a script into tokens, leaving comments out.
pub fn tokenize(src: &str) -> Vec<Tok> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' => {
                out.push(Tok::Newline);
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i += 2;
            }
            '"' | '\'' => {
                let quote = c;
                let triple = chars.get(i + 1) == Some(&quote) && chars.get(i + 2) == Some(&quote);
                i += if triple { 3 } else { 1 };
                let mut text = String::new();
                while i < chars.len() {
                    if triple {
                        if chars[i] == quote && chars.get(i + 1) == Some(&quote) && chars.get(i + 2) == Some(&quote) {
                            i += 3;
                            break;
                        }
                    } else if chars[i] == quote {
                        i += 1;
                        break;
                    } else if chars[i] == '\n' {
                        // An unterminated string: stop at the end of the line.
                        break;
                    }
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                        text.push(match chars[i] {
                            'n' => '\n',
                            't' => '\t',
                            other => other,
                        });
                    } else if chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
                        // Keep a `${...}` template whole, quotes inside it included.
                        let mut depth = 0;
                        while i < chars.len() {
                            text.push(chars[i]);
                            match chars[i] {
                                '{' => depth += 1,
                                '}' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                }
                                _ => {}
                            }
                            i += 1;
                        }
                    } else {
                        text.push(chars[i]);
                    }
                    i += 1;
                }
                out.push(Tok::Str(text));
            }
            '`' => {
                let start = i + 1;
                i = start;
                while i < chars.len() && chars[i] != '`' {
                    i += 1;
                }
                out.push(Tok::Ident(chars[start..i.min(chars.len())].iter().collect()));
                i += 1;
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
                {
                    i += 1;
                }
                out.push(Tok::Num(chars[start..i].iter().collect()));
            }
            c if c.is_alphabetic() || c == '_' || c == '$' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                    i += 1;
                }
                out.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            c => {
                out.push(Tok::Punct(c));
                i += 1;
            }
        }
    }
    out
}

/// One statement: the tokens before its block, and the block.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stmt {
    pub tokens: Vec<Tok>,
    pub block: Option<Vec<Stmt>>,
}

/// Parses a script into its statements.
pub fn parse(src: &str) -> Vec<Stmt> {
    let tokens = tokenize(src);
    let mut pos = 0;
    parse_block(&tokens, &mut pos)
}

/// Tokens that, at the end of a line, say the statement goes on.
const CONTINUES: &[char] = &[',', '.', '=', '+', '-', '*', '/', '&', '|', '?', ':', '(', '['];

fn parse_block(tokens: &[Tok], pos: &mut usize) -> Vec<Stmt> {
    let mut stmts = Vec::new();
    loop {
        while matches!(tokens.get(*pos), Some(Tok::Newline | Tok::Punct(';'))) {
            *pos += 1;
        }
        match tokens.get(*pos) {
            None | Some(Tok::Punct('}')) => return stmts,
            _ => {}
        }
        let mut stmt = Stmt::default();
        let mut nest = 0usize;
        while let Some(token) = tokens.get(*pos) {
            match token {
                Tok::Punct('{') if nest == 0 => {
                    *pos += 1;
                    stmt.block = Some(parse_block(tokens, pos));
                    if matches!(tokens.get(*pos), Some(Tok::Punct('}'))) {
                        *pos += 1;
                    }
                    break;
                }
                Tok::Punct('}') if nest == 0 => break,
                Tok::Punct(';') if nest == 0 => break,
                Tok::Newline if nest == 0 => {
                    let goes_on = matches!(stmt.tokens.last(), Some(Tok::Punct(c)) if CONTINUES.contains(c))
                        || matches!(next_significant(tokens, *pos), Some(Tok::Punct('.' | '?' | '&' | '|')));
                    if !goes_on {
                        break;
                    }
                    *pos += 1;
                }
                Tok::Newline => *pos += 1,
                Tok::Punct(c @ ('(' | '[' | '{')) => {
                    nest += 1;
                    stmt.tokens.push(Tok::Punct(*c));
                    *pos += 1;
                }
                Tok::Punct(c @ (')' | ']' | '}')) => {
                    nest = nest.saturating_sub(1);
                    stmt.tokens.push(Tok::Punct(*c));
                    *pos += 1;
                }
                other => {
                    stmt.tokens.push(other.clone());
                    *pos += 1;
                }
            }
        }
        if !stmt.tokens.is_empty() || stmt.block.is_some() {
            stmts.push(stmt);
        }
    }
}

fn next_significant(tokens: &[Tok], from: usize) -> Option<&Tok> {
    tokens[from..].iter().find(|t| !matches!(t, Tok::Newline))
}

/// An argument of a call: `value`, or `name = value` / `name: value`.
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Vec<Tok>,
}

impl Arg {
    /// The string literal this argument is, when it is one.
    pub fn string(&self) -> Option<&str> {
        match self.value.as_slice() {
            [Tok::Str(s)] => Some(s),
            _ => None,
        }
    }
}

/// Splits `tokens` at the commas outside any brackets.
fn split_args(tokens: &[Tok]) -> Vec<Arg> {
    let mut args = Vec::new();
    let mut current: Vec<Tok> = Vec::new();
    let mut nest = 0usize;
    let mut finish = |current: &mut Vec<Tok>| {
        if current.is_empty() {
            return;
        }
        let tokens = std::mem::take(current);
        let named = match tokens.as_slice() {
            [Tok::Ident(name), Tok::Punct('=' | ':'), rest @ ..] if !rest.is_empty() && !matches!(rest.first(), Some(Tok::Punct('='))) => {
                Some((name.clone(), rest.to_vec()))
            }
            _ => None,
        };
        args.push(match named {
            Some((name, value)) => Arg { name: Some(name), value },
            None => Arg { name: None, value: tokens },
        });
    };
    for token in tokens {
        match token {
            Tok::Punct('(' | '[' | '{') => {
                nest += 1;
                current.push(token.clone());
            }
            Tok::Punct(')' | ']' | '}') => {
                nest = nest.saturating_sub(1);
                current.push(token.clone());
            }
            Tok::Punct(',') if nest == 0 => finish(&mut current),
            Tok::Newline => {}
            other => current.push(other.clone()),
        }
    }
    finish(&mut current);
    args
}

impl Stmt {
    /// The dotted name the statement starts with: `implementation`, `kotlin.sourceSets.getByName`.
    pub fn path(&self) -> Vec<String> {
        let mut path = Vec::new();
        let mut tokens = self.tokens.iter().peekable();
        while let Some(Tok::Ident(name)) = tokens.peek() {
            path.push(name.clone());
            tokens.next();
            match tokens.peek() {
                Some(Tok::Punct('.')) => {
                    tokens.next();
                }
                Some(Tok::Punct('?')) => break,
                _ => break,
            }
        }
        path
    }

    /// The last segment of [`Stmt::path`]: the name of the call.
    pub fn name(&self) -> Option<String> {
        self.path().pop()
    }

    /// Whether the statement's path is exactly `names`.
    pub fn is(&self, names: &[&str]) -> bool {
        let path = self.path();
        path.len() == names.len() && path.iter().zip(names).all(|(a, b)| a == b)
    }

    /// The arguments of the call the statement is: `name(a, b)` or the Groovy `name a, b`.
    pub fn args(&self) -> Vec<Arg> {
        let skip = self.path_len();
        let rest = &self.tokens[skip..];
        match rest.first() {
            Some(Tok::Punct('(')) => {
                let end = matching(rest, 0).unwrap_or(rest.len());
                split_args(&rest[1..end])
            }
            Some(Tok::Str(_) | Tok::Ident(_) | Tok::Num(_)) => split_args(rest),
            _ => Vec::new(),
        }
    }

    /// The tokens after the dotted name (and after `=`, for an assignment).
    pub fn rest(&self) -> &[Tok] {
        &self.tokens[self.path_len()..]
    }

    fn path_len(&self) -> usize {
        let mut count = 0;
        let mut expect_ident = true;
        for token in &self.tokens {
            match (token, expect_ident) {
                (Tok::Ident(_), true) => {
                    count += 1;
                    expect_ident = false;
                }
                (Tok::Punct('.'), false) => {
                    count += 1;
                    expect_ident = true;
                }
                _ => break,
            }
        }
        if expect_ident && count > 0 {
            // A trailing dot is not part of the name.
            count -= 1;
        }
        count
    }

    /// For `name = value`, `name += value` or the Groovy `name value`, the value's tokens.
    pub fn assigned(&self) -> Option<&[Tok]> {
        let rest = self.rest();
        match rest {
            [Tok::Punct('='), value @ ..] if !value.is_empty() => Some(value),
            [Tok::Punct('+'), Tok::Punct('='), value @ ..] if !value.is_empty() => Some(value),
            // Groovy: `group 'com.example'`
            [Tok::Str(_) | Tok::Num(_), ..] => Some(rest),
            _ => None,
        }
    }
}

/// The index of the bracket that closes the one at `open`.
pub fn matching(tokens: &[Tok], open: usize) -> Option<usize> {
    let mut nest = 0usize;
    for (i, token) in tokens.iter().enumerate().skip(open) {
        match token {
            Tok::Punct('(' | '[' | '{') => nest += 1,
            Tok::Punct(')' | ']' | '}') => {
                nest = nest.checked_sub(1)?;
                if nest == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Calls `visit` for every statement of `stmts` and of the blocks below, with the names of the blocks around it.
pub fn walk<'a>(stmts: &'a [Stmt], visit: &mut impl FnMut(&'a Stmt, &[String])) {
    fn go<'a>(stmts: &'a [Stmt], ancestors: &mut Vec<String>, visit: &mut impl FnMut(&'a Stmt, &[String])) {
        for stmt in stmts {
            visit(stmt, ancestors);
            if let Some(block) = &stmt.block {
                ancestors.push(stmt.name().unwrap_or_default());
                go(block, ancestors, visit);
                ancestors.pop();
            }
        }
    }
    go(stmts, &mut Vec::new(), visit);
}

/// A string-valued expression: what `eval` makes of tokens, given what is known about the build.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Properties and `val`s: `gradle.properties`, `val x = "..."`, `extra["x"] = "..."`.
    pub vars: HashMap<String, String>,
    /// `libs.versions.<alias>`, by normalized alias, for every catalog.
    pub versions: HashMap<String, String>,
}

/// An alias as Gradle's accessors spell it: `ktor-server-core` and `ktor_server_core` are `ktor.server.core`.
pub fn normalize_alias(alias: &str) -> String {
    alias.replace(['-', '_'], ".")
}

impl Scope {
    /// The value of a string template: `"$a-${b}"`, with the names the scope knows. `None` when one does not resolve.
    pub fn template(&self, text: &str) -> Option<String> {
        let mut out = String::new();
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] != '$' || i + 1 >= chars.len() {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            if chars[i + 1] == '{' {
                let mut depth = 0;
                let mut j = i + 1;
                while j < chars.len() {
                    match chars[j] {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let inner: String = chars[i + 2..j.min(chars.len())].iter().collect();
                out.push_str(&self.eval(&tokenize(&inner))?);
                i = j + 1;
            } else if chars[i + 1].is_alphabetic() || chars[i + 1] == '_' {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                let name: String = chars[i + 1..j].iter().collect();
                out.push_str(self.vars.get(&name)?);
                i = j;
            } else {
                out.push('$');
                i += 1;
            }
        }
        Some(out)
    }

    /// The string an expression evaluates to, for the forms build scripts use for versions and names.
    pub fn eval(&self, tokens: &[Tok]) -> Option<String> {
        let tokens: Vec<&Tok> = tokens.iter().filter(|t| !matches!(t, Tok::Newline)).collect();
        match tokens.as_slice() {
            [] => return None,
            [Tok::Str(s)] => return self.template(s),
            [Tok::Num(n)] => return Some(n.clone()),
            _ => {}
        }
        // `a + b + "c"`
        if tokens.iter().any(|t| matches!(t, Tok::Punct('+'))) && !tokens.iter().any(|t| matches!(t, Tok::Punct('('))) {
            let mut out = String::new();
            for part in tokens.split(|t| matches!(t, Tok::Punct('+'))) {
                let owned: Vec<Tok> = part.iter().map(|t| (*t).clone()).collect();
                out.push_str(&self.eval(&owned)?);
            }
            return Some(out);
        }
        // `name`, `a.b.c`, with the calls that only unwrap a provider dropped.
        let mut path: Vec<String> = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            match tokens[i] {
                Tok::Ident(name) => {
                    path.push(name.clone());
                    i += 1;
                    if matches!(tokens.get(i), Some(Tok::Punct('('))) {
                        // `.get()` and friends take nothing; `property("x")` and `findProperty("x")` name a variable.
                        let close = matching_refs(&tokens, i)?;
                        let inner: Vec<Tok> = tokens[i + 1..close].iter().map(|t| (*t).clone()).collect();
                        if matches!(path.last().map(String::as_str), Some("property" | "findProperty" | "getProperty")) {
                            let key = self.eval(&inner)?;
                            return self.vars.get(&key).cloned();
                        }
                        if !inner.is_empty() {
                            return None;
                        }
                        if matches!(name.as_str(), "get" | "toString" | "orNull" | "getOrNull" | "asString") {
                            path.pop();
                        }
                        i = close + 1;
                    }
                }
                Tok::Punct('.') | Tok::Punct('?') => i += 1,
                // `extra["x"]`, `project.extra["x"]`, `properties["x"]`
                Tok::Punct('[') => {
                    let close = matching_refs(&tokens, i)?;
                    let inner: Vec<Tok> = tokens[i + 1..close].iter().map(|t| (*t).clone()).collect();
                    let key = self.eval(&inner)?;
                    if tokens.get(close + 1).is_some() {
                        return None;
                    }
                    return self.vars.get(&key).cloned();
                }
                _ => return None,
            }
        }
        self.path_value(&path)
    }

    fn path_value(&self, path: &[String]) -> Option<String> {
        match path {
            [] => None,
            [only] => self.vars.get(only).cloned(),
            // `libs.versions.ktor`, `versions.ktor` of a catalog with another name.
            [_, versions, rest @ ..] if versions == "versions" && !rest.is_empty() => {
                self.versions.get(&normalize_alias(&rest.join("."))).cloned()
            }
            // `project.version`, `rootProject.version`, `project.ext.x`, `extra.x`
            [first, rest @ ..] if matches!(first.as_str(), "project" | "rootProject" | "extra" | "ext" | "properties") => {
                let rest: Vec<String> = rest.iter().filter(|s| !matches!(s.as_str(), "extra" | "ext" | "properties")).cloned().collect();
                self.path_value(&rest)
            }
            _ => None,
        }
    }
}

fn matching_refs(tokens: &[&Tok], open: usize) -> Option<usize> {
    let owned: Vec<Tok> = tokens.iter().map(|t| (*t).clone()).collect();
    matching(&owned, open)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stmts(src: &str) -> Vec<Stmt> {
        parse(src)
    }

    #[test]
    fn reads_blocks_and_calls() {
        let src = r#"
plugins {
    kotlin("jvm") version "2.0.20"
    id("io.ktor.plugin") version "3.0.0" // the Ktor plugin
    `java-library`
}
dependencies {
    implementation("io.ktor:ktor-server-core-jvm") {
        exclude(group = "a", module = "b")
    }
    testImplementation(kotlin("test"))
}
group = "com.example"
"#;
        let all = stmts(src);
        assert_eq!(all.len(), 3);
        let plugins = all[0].block.as_ref().unwrap();
        assert_eq!(plugins.len(), 3);
        assert_eq!(plugins[0].name().as_deref(), Some("kotlin"));
        assert_eq!(plugins[0].args()[0].string(), Some("jvm"));
        assert_eq!(plugins[2].name().as_deref(), Some("java-library"));
        let deps = all[1].block.as_ref().unwrap();
        assert_eq!(deps[0].args()[0].string(), Some("io.ktor:ktor-server-core-jvm"));
        let exclude = &deps[0].block.as_ref().unwrap()[0];
        let args = exclude.args();
        assert_eq!((args[0].name.as_deref(), args[0].string()), (Some("group"), Some("a")));
        assert_eq!(all[2].assigned().and_then(|t| Scope::default().eval(t)).as_deref(), Some("com.example"));
    }

    #[test]
    fn reads_groovy() {
        let all = stmts(
            "apply plugin: 'kotlin'\ndependencies {\n  implementation 'g:a:1.0'\n  implementation group: 'g', name: 'b', version: '2'\n}\n",
        );
        assert_eq!(all[0].args()[0].name.as_deref(), Some("plugin"));
        assert_eq!(all[0].args()[0].string(), Some("kotlin"));
        let deps = all[1].block.as_ref().unwrap();
        assert_eq!(deps[0].args()[0].string(), Some("g:a:1.0"));
        let named = deps[1].args();
        assert_eq!(named.iter().filter_map(|a| a.name.as_deref()).collect::<Vec<_>>(), ["group", "name", "version"]);
    }

    #[test]
    fn statements_continue_over_lines() {
        let all = stmts("val x = listOf(\n  \"a\",\n  \"b\"\n)\nfoo = bar\n  .baz()\nnext()\n");
        assert_eq!(all.len(), 3);
        let all = stmts(
            "kotlin {\n  jvm()\n  sourceSets {\n    val commonMain by getting {\n      dependencies { implementation(\"x:y:1\") }\n    }\n  }\n}\n",
        );
        let kotlin = all[0].block.as_ref().unwrap();
        assert_eq!(kotlin.len(), 2);
        let main = &kotlin[1].block.as_ref().unwrap()[0];
        assert_eq!(main.block.as_ref().unwrap()[0].name().as_deref(), Some("dependencies"));
    }

    #[test]
    fn evaluates_templates_and_properties() {
        let mut scope = Scope::default();
        scope.vars.insert("ktor_version".into(), "3.0.0".into());
        scope.vars.insert("kotlin_version".into(), "2.0.20".into());
        scope.versions.insert("kotlin".into(), "2.0.21".into());
        scope.versions.insert("ktor.core".into(), "3.1.0".into());
        assert_eq!(scope.template("io.ktor:ktor-client:$ktor_version").as_deref(), Some("io.ktor:ktor-client:3.0.0"));
        assert_eq!(scope.template("a:b:${kotlin_version}").as_deref(), Some("a:b:2.0.20"));
        assert_eq!(scope.template("a:b:${libs.versions.kotlin.get()}").as_deref(), Some("a:b:2.0.21"));
        assert_eq!(scope.template("a:b:${libs.versions.ktor.core.get()}").as_deref(), Some("a:b:3.1.0"));
        assert_eq!(scope.template("a:b:${project.property(\"ktor_version\")}").as_deref(), Some("a:b:3.0.0"));
        assert_eq!(scope.template("a:b:$unknown"), None);
        assert_eq!(scope.eval(&tokenize("ktor_version")).as_deref(), Some("3.0.0"));
        assert_eq!(scope.eval(&tokenize("\"v\" + kotlin_version")).as_deref(), Some("v2.0.20"));
        assert_eq!(scope.eval(&tokenize("extra[\"ktor_version\"]")).as_deref(), Some("3.0.0"));
    }

    #[test]
    fn aliases_normalize() {
        assert_eq!(normalize_alias("ktor-server_core"), "ktor.server.core");
    }
}
