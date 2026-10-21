//! Folding ranges and selection ranges, from the lexer's bracket pairs and the parser's spans.

use std::ops::Range;

use serde_json::{Value, json};

use super::lexer::Tok;
use super::resolve::chain_start;
use super::syntax::File;
use crate::text;

fn line_of(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|&s| s <= offset) - 1
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    starts
}

/// What folds: bodies, argument lists, multi-line strings, comments and the imports.
pub fn folding_ranges(file: &File, src: &str) -> Value {
    let starts = line_starts(src);
    let lx = &file.lexed;
    // (start line, end line, kind). A closing bracket on a line of its own stays visible.
    let mut found: Vec<(usize, usize, Option<&str>)> = Vec::new();
    let mut add = |from: usize, to: usize, close: Option<usize>, kind: Option<&'static str>| {
        let (start, mut end) = (line_of(&starts, from), line_of(&starts, to));
        if let Some(c) = close
            && end > start
            && src[starts[end]..c].trim().is_empty()
        {
            end -= 1;
        }
        if end > start {
            found.push((start, end, kind));
        }
    };
    let mut strings = Vec::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        match t.kind {
            Tok::LBrace | Tok::LParen | Tok::LBracket => {
                if let Some(c) = lx.matching[i].filter(|&c| c > i) {
                    add(t.start, lx.tokens[c].start, Some(lx.tokens[c].start), None);
                }
            }
            Tok::StrOpen => strings.push(t.start),
            Tok::StrClose => {
                if let Some(start) = strings.pop()
                    && src[start..].starts_with("\"\"\"")
                {
                    add(start, t.end, None, None);
                }
            }
            _ => {}
        }
    }
    if let (Some(first), Some(last)) = (file.imports.first(), file.imports.last()) {
        add(first.span.start, last.span.end, None, Some("imports"));
    }
    // Block comments and KDoc, and runs of line comments.
    let mut run: Option<(usize, usize)> = None;
    let mut runs = Vec::new();
    for c in &lx.comments {
        if src[c.clone()].starts_with("/*") {
            add(c.start, c.end, None, Some("comment"));
            continue;
        }
        let line = line_of(&starts, c.start);
        let alone = src[starts[line]..c.start].trim().is_empty();
        run = match run {
            Some((first, last)) if alone && line == last + 1 => Some((first, line)),
            other => {
                runs.extend(other);
                alone.then_some((line, line))
            }
        };
    }
    runs.extend(run);
    found.extend(runs.into_iter().map(|(first, last)| (first, last, Some("comment"))));
    found.retain(|&(s, e, _)| e > s);
    // One fold per starting line: the widest.
    found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    found.dedup_by_key(|f| f.0);
    json!(
        found
            .iter()
            .map(|&(s, e, kind)| match kind {
                Some(kind) => json!({ "startLine": s, "endLine": e, "kind": kind }),
                None => json!({ "startLine": s, "endLine": e }),
            })
            .collect::<Vec<_>>()
    )
}

/// The last tokens the postfix chain after token `last` can end at: `.name`, calls, indexing,
/// trailing lambdas and `!!`, one entry per step.
fn chain_ends(file: &File, last: usize) -> Vec<usize> {
    let lx = &file.lexed;
    let mut ends = vec![last];
    let mut i = last;
    while let Some(t) = lx.tokens.get(i + 1) {
        let next = i + 1;
        match t.kind {
            Tok::Dot | Tok::SafeDot if lx.kind(next + 1) == Some(Tok::Ident) => i = next + 1,
            Tok::LParen | Tok::LBracket | Tok::LBrace if !t.nl => match lx.matching[next].filter(|&c| c > next) {
                Some(c) => i = c,
                None => break,
            },
            Tok::BangBang => i = next,
            _ => break,
        }
        ends.push(i);
    }
    ends
}

/// Whether token `i` continues the statement of the previous line.
fn continues(file: &File, src: &str, i: usize) -> bool {
    let lx = &file.lexed;
    let prev = i.checked_sub(1).map(|p| lx.tokens[p].kind);
    matches!(lx.tokens[i].kind, Tok::Dot | Tok::SafeDot | Tok::Comma | Tok::Arrow | Tok::Eq | Tok::Colon | Tok::Question)
        || matches!(prev, Some(Tok::Dot | Tok::SafeDot | Tok::Comma | Tok::Arrow | Tok::Op | Tok::Eq | Tok::Colon | Tok::LParen))
        || ["else", "catch", "finally", "by", "where"].iter().any(|w| lx.is(src, i, w))
}

/// The ranges around `offset`, innermost first: the token, its expression, the statements and
/// brackets around it, its declarations, and the file.
fn nested(file: &File, src: &str, offset: usize) -> Vec<Range<usize>> {
    let lx = &file.lexed;
    let mut all: Vec<Range<usize>> = std::iter::once(0..src.len()).collect();
    let tok = lx.tokens.partition_point(|t| t.end < offset);
    if let Some(t) = lx.tokens.get(tok).filter(|t| t.start <= offset) {
        all.push(t.span());
        if matches!(t.kind, Tok::Ident | Tok::Int | Tok::Float | Tok::Char) {
            // The name, with the receivers before it and the calls after it.
            let mut first = tok;
            while first > 0 && matches!(lx.kind(first - 1), Some(Tok::Dot | Tok::SafeDot)) {
                first = chain_start(src, lx, first - 1);
                all.push(lx.tokens[first].start..t.end);
            }
            for end in chain_ends(file, tok) {
                all.push(lx.tokens[first].start..lx.tokens[end].end);
            }
        }
    }
    // Brackets around the offset, and the statements inside the innermost braces.
    let mut stack: Vec<usize> = Vec::new();
    for (i, t) in lx.tokens.iter().enumerate() {
        if t.start >= offset {
            break;
        }
        match t.kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace | Tok::TemplateOpen => stack.push(i),
            Tok::RParen | Tok::RBracket | Tok::RBrace => {
                stack.pop();
            }
            _ => {}
        }
    }
    let mut container: Option<usize> = None;
    for &open in stack.iter().rev() {
        let Some(close) = lx.matching[open].filter(|&c| c > open) else { continue };
        let (o, c) = (&lx.tokens[open], &lx.tokens[close]);
        if c.end < offset {
            continue;
        }
        all.push(o.start..c.end);
        all.push(o.end..c.start);
        if container.is_none() && matches!(o.kind, Tok::LBrace | Tok::TemplateOpen) {
            container = Some(open);
        }
    }
    let (from, to) = match container {
        Some(open) => (open + 1, lx.matching[open].unwrap_or(lx.tokens.len())),
        None => (0, lx.tokens.len()),
    };
    // Statements: split at `;` and at line breaks that do not continue an expression.
    let (mut begin, mut depth) = (from, 0usize);
    let mut statement = |begin: usize, end: usize| {
        if end > begin && lx.tokens[begin].start <= offset && offset <= lx.tokens[end - 1].end {
            all.push(lx.tokens[begin].start..lx.tokens[end - 1].end);
        }
    };
    for i in from..to {
        if depth == 0 && i > begin && ((lx.tokens[i].nl && !continues(file, src, i)) || lx.kind(i - 1) == Some(Tok::Semi)) {
            statement(begin, i);
            begin = i;
        }
        match lx.tokens[i].kind {
            Tok::LParen | Tok::LBracket | Tok::LBrace => depth += 1,
            Tok::RParen | Tok::RBracket | Tok::RBrace => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    statement(begin, to);
    for d in &file.decls {
        let span = d.span.start.min(d.name_span.start)..d.span.end.max(d.name_span.end);
        all.push(span);
    }
    all.extend(lx.comments.iter().cloned());
    all.retain(|r| r.start <= offset && offset <= r.end && r.start < r.end);
    // Smallest first; each must contain the one before it.
    all.sort_by_key(|r| (r.end - r.start, r.start));
    all.dedup();
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in all {
        if out.last().is_none_or(|l| r.start <= l.start && l.end <= r.end && r != *l) {
            out.push(r);
        }
    }
    out
}

/// The selection range at each of `positions`.
pub fn selection_ranges(file: &File, src: &str, positions: &[Value]) -> Value {
    json!(
        positions
            .iter()
            .map(|p| {
                let offset = text::offset(src, p);
                let chain = nested(file, src, offset);
                let mut node = Value::Null;
                for r in chain.iter().rev() {
                    node = if node.is_null() {
                        json!({ "range": text::range(src, r.clone()) })
                    } else {
                        json!({ "range": text::range(src, r.clone()), "parent": node })
                    };
                }
                if node.is_null() { json!({ "range": text::range(src, offset..offset) }) } else { node }
            })
            .collect::<Vec<_>>()
    )
}
