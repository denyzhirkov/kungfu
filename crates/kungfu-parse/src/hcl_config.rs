//! HCL / Terraform config symbols: blocks (`resource "aws_s3_bucket" "logs"`)
//! as `ConfigBlock`s and attributes as `ConfigKey`s, two levels deep.
//! Line-based because the only published tree-sitter-hcl (1.1.0) is grammar
//! ABI 15 and the workspace tree-sitter 0.24 loads ABI 13-14 only. Follows the
//! `config_symbols` contract; `json_config` is the reference.
//!
//! Two passes: a lexer that turns the source into a flat token list (strings
//! with `${…}` interpolation, comments and heredocs are consumed whole, so a
//! brace inside them is never a token), then a one-level body parser that
//! never recurses — a nested body is skipped by counting braces and parsed
//! again on demand, so nesting depth cannot overflow the stack.
//!
//! Known limits: the scanner does not validate HCL. An unterminated `${`,
//! heredoc, comment or block swallows the rest of its body or of the file
//! (symbols already found stay, and the result is flagged incomplete); an
//! unmatched bracket cannot run past the end of its block; a block whose `{`
//! is not on the line of its type/labels is skipped as garbage. A scalar that
//! is a function call can still show literals from its first line.

use crate::config_symbols::{ConfigParent, ConfigSymbols, ConfigValue};
use kungfu_types::symbol::{Span, SymbolKind};
use std::cell::Cell;
use std::collections::HashMap;

/// Interpolations nest strings nest interpolations; past this depth a quote is
/// taken literally rather than recursing further.
const MAX_STRING_NESTING: usize = 32;

/// Push this file's config symbols into `out`, scanning `source` directly
/// (no tree-sitter grammar).
pub fn extract(source: &str, out: &mut ConfigSymbols) {
    let source = crate::env_config::strip_bom(source);
    let doc = lex(source);
    for item in parse_body(&doc, 0, doc.toks.len()) {
        match item {
            Item::Attr(attr) => {
                out.push(
                    None,
                    attr.name,
                    SymbolKind::ConfigKey,
                    attr_value(&attr, false),
                    attr_span(&attr),
                );
            }
            Item::Block(block) => {
                let body = parse_body(&doc, block.body_start, block.body_end);
                let has_blocks = body.iter().any(|i| matches!(i, Item::Block(_)));
                let shown = ConfigValue::Block {
                    count: body.len(),
                    noun: if has_blocks { "item" } else { "attribute" },
                };
                let handle = out.push(
                    None,
                    &address(&block),
                    SymbolKind::ConfigBlock,
                    shown,
                    block_span(&block, block.end_line, block.end_col),
                );
                if let Some(parent) = handle {
                    // A sensitive variable's default is a secret by declaration.
                    let hide_default = block.kind == "variable"
                        && body.iter().any(|i| {
                            matches!(i, Item::Attr(a) if a.name == "sensitive" && a.first == "true")
                        });
                    push_children(&doc, out, &parent, &body, hide_default);
                }
            }
        }
    }
    if doc.incomplete.get() {
        out.mark_incomplete();
    }
}

/// Terraform address of a top-level block: `resource "t" "n"` → `t.n`,
/// `data "t" "n"` → `data.t.n`, `variable "x"` → `var.x`, anything else
/// `type.label1.label2` (`locals`, `terraform` have no labels).
fn address(block: &Blk) -> String {
    let prefix = match block.kind {
        "resource" if !block.labels.is_empty() => None,
        "variable" => Some("var"),
        other => Some(other),
    };
    let mut parts: Vec<&str> = prefix.into_iter().collect();
    parts.extend(block.labels.iter().copied());
    parts.join(".")
}

/// Nested blocks of one type and labels (`ingress`, `dynamic.ingress`) are one
/// symbol anchored at the first and ending at the last.
struct Group<'i, 'a> {
    name: String,
    first: &'i Blk<'a>,
    last: &'i Blk<'a>,
    count: usize,
    emitted: bool,
}

fn push_children(
    doc: &Doc,
    out: &mut ConfigSymbols,
    parent: &ConfigParent,
    items: &[Item],
    hide_default: bool,
) {
    // One pass groups the blocks; the second emits in document order.
    let mut groups: Vec<Group> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    let mut group_of: Vec<Option<usize>> = Vec::with_capacity(items.len());
    for item in items {
        let Item::Block(block) = item else {
            group_of.push(None);
            continue;
        };
        let name = block_key(block);
        let at = *by_name.entry(name.clone()).or_insert_with(|| {
            groups.push(Group {
                name,
                first: block,
                last: block,
                count: 0,
                emitted: false,
            });
            groups.len() - 1
        });
        if let Some(group) = groups.get_mut(at) {
            group.count += 1;
            group.last = block;
        }
        group_of.push(Some(at));
    }
    for (item, group) in items.iter().zip(group_of) {
        match item {
            Item::Attr(attr) => {
                let hide = hide_default && attr.name == "default";
                out.push(
                    Some(parent),
                    attr.name,
                    SymbolKind::ConfigKey,
                    attr_value(attr, hide),
                    attr_span(attr),
                );
            }
            Item::Block(_) => {
                let Some(group) = group.and_then(|g| groups.get_mut(g)) else {
                    continue;
                };
                if std::mem::replace(&mut group.emitted, true) {
                    continue;
                }
                let shown = if group.count == 1 {
                    let body = parse_body(doc, group.first.body_start, group.first.body_end);
                    let has_blocks = body.iter().any(|i| matches!(i, Item::Block(_)));
                    ConfigValue::Block {
                        count: body.len(),
                        noun: if has_blocks { "item" } else { "attribute" },
                    }
                } else {
                    ConfigValue::List {
                        count: group.count,
                        noun: "block",
                    }
                };
                out.push(
                    Some(parent),
                    &group.name,
                    SymbolKind::ConfigBlock,
                    shown,
                    block_span(group.first, group.last.end_line, group.last.end_col),
                );
            }
        }
    }
}

/// `ingress`, `dynamic.ingress`, `provisioner.local-exec`.
fn block_key(block: &Blk) -> String {
    let mut key = block.kind.to_string();
    for label in &block.labels {
        key.push('.');
        key.push_str(label);
    }
    key
}

fn attr_value<'a>(attr: &Attr<'a>, hide: bool) -> ConfigValue<'a> {
    if hide {
        return ConfigValue::None;
    }
    match attr.shape {
        Shape::Object(count) => ConfigValue::Block { count, noun: "key" },
        Shape::List(count) => ConfigValue::List {
            count,
            noun: "item",
        },
        Shape::Plain if attr.first.is_empty() => ConfigValue::None,
        Shape::Plain => ConfigValue::Scalar(attr.first),
    }
}

fn attr_span(attr: &Attr) -> Span {
    Span {
        start_line: attr.line,
        end_line: attr.end_line,
        start_col: attr.col,
        end_col: attr.end_col,
    }
}

fn block_span(block: &Blk, end_line: usize, end_col: usize) -> Span {
    Span {
        start_line: block.line,
        end_line,
        start_col: block.col,
        end_col,
    }
}

// ---------------------------------------------------------------- lexer

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Ident,
    /// Quoted string, quotes included.
    Str,
    /// `<<EOF` opener only; the token's end position is the terminator line.
    Heredoc,
    /// One of `{ } [ ] ( ) , . ? : =`.
    Punct,
    Newline,
    Other,
}

struct Tok<'a> {
    kind: Kind,
    text: &'a str,
    /// Byte range of `text` in the source.
    start: usize,
    end: usize,
    /// 1-based line and 0-based byte column of the first byte.
    line: usize,
    col: usize,
    end_line: usize,
    end_col: usize,
}

impl Tok<'_> {
    fn is(&self, c: u8) -> bool {
        self.kind == Kind::Punct && self.text.as_bytes() == [c]
    }
}

struct Doc<'a> {
    src: &'a str,
    toks: Vec<Tok<'a>>,
    /// Something was swallowed or given up on; symbols may be missing.
    incomplete: Cell<bool>,
}

fn lex(src: &str) -> Doc<'_> {
    let mut lexer = Lexer {
        src,
        b: src.as_bytes(),
        pos: 0,
        line: 1,
        line_start: 0,
        toks: Vec::new(),
        incomplete: false,
    };
    lexer.run();
    Doc {
        src,
        toks: lexer.toks,
        incomplete: Cell::new(lexer.incomplete),
    }
}

struct Lexer<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    line: usize,
    line_start: usize,
    toks: Vec<Tok<'a>>,
    incomplete: bool,
}

impl<'a> Lexer<'a> {
    /// Every iteration consumes at least one byte.
    fn run(&mut self) {
        while let Some(&c) = self.b.get(self.pos) {
            let start = self.pos;
            let line = self.line;
            let col = start - self.line_start;
            match c {
                b'\n' => {
                    self.pos += 1;
                    self.emit(Kind::Newline, start, line, col);
                    self.line += 1;
                    self.line_start = self.pos;
                }
                // A lone `\r` ends a statement like a line break, but is not a
                // line for numbering: `str::lines()` does not split on it.
                b'\r' if self.b.get(start + 1) != Some(&b'\n') => {
                    self.pos += 1;
                    self.emit(Kind::Newline, start, line, col);
                }
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'#' => self.skip_line(),
                b'/' => match self.b.get(start + 1) {
                    Some(b'/') => self.skip_line(),
                    Some(b'*') => self.skip_block_comment(),
                    _ => {
                        self.pos += 1;
                        self.emit(Kind::Other, start, line, col);
                    }
                },
                b'"' => {
                    self.skip_string(0);
                    self.emit(Kind::Str, start, line, col);
                }
                b'<' => {
                    if !self.heredoc(start, line, col) {
                        self.pos += 1;
                        self.emit(Kind::Other, start, line, col);
                    }
                }
                b'{' | b'}' | b'[' | b']' | b'(' | b')' | b',' | b'.' | b'?' | b':' => {
                    self.pos += 1;
                    self.emit(Kind::Punct, start, line, col);
                }
                b'=' => {
                    // `==` and `=>` are operators, not an attribute's `=`.
                    let operator = matches!(self.b.get(start + 1), Some(b'=' | b'>'));
                    self.pos += if operator { 2 } else { 1 };
                    let kind = if operator { Kind::Other } else { Kind::Punct };
                    self.emit(kind, start, line, col);
                }
                c if c.is_ascii_alphabetic() || c == b'_' => {
                    while self
                        .b
                        .get(self.pos)
                        .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
                    {
                        self.pos += 1;
                    }
                    self.emit(Kind::Ident, start, line, col);
                }
                c if c.is_ascii_digit() => {
                    while self
                        .b
                        .get(self.pos)
                        .is_some_and(|c| c.is_ascii_alphanumeric())
                    {
                        self.pos += 1;
                    }
                    self.emit(Kind::Other, start, line, col);
                }
                _ => {
                    // One char, so a multibyte char is never split.
                    self.pos += 1;
                    while self.b.get(self.pos).is_some_and(|c| c & 0xC0 == 0x80) {
                        self.pos += 1;
                    }
                    self.emit(Kind::Other, start, line, col);
                }
            }
        }
    }

    /// Push a token ending at the current position.
    fn emit(&mut self, kind: Kind, start: usize, line: usize, col: usize) {
        self.toks.push(Tok {
            kind,
            text: self.src.get(start..self.pos).unwrap_or(""),
            start,
            end: self.pos,
            line,
            col,
            end_line: self.line,
            end_col: self.pos.saturating_sub(self.line_start),
        });
    }

    fn skip_line(&mut self) {
        while self.b.get(self.pos).is_some_and(|&c| c != b'\n') {
            self.pos += 1;
        }
    }

    fn skip_block_comment(&mut self) {
        self.pos += 2;
        while let Some(&c) = self.b.get(self.pos) {
            if c == b'*' && self.b.get(self.pos + 1) == Some(&b'/') {
                self.pos += 2;
                return;
            }
            self.pos += 1;
            if c == b'\n' {
                self.line += 1;
                self.line_start = self.pos;
            }
        }
        self.incomplete = true;
    }

    /// At an opening quote. Consumes through the closing quote; an
    /// unterminated string ends at the line break (HCL strings are one line).
    fn skip_string(&mut self, nesting: usize) {
        self.pos += 1;
        while let Some(&c) = self.b.get(self.pos) {
            match c {
                b'"' => {
                    self.pos += 1;
                    return;
                }
                b'\n' => {
                    self.incomplete = true;
                    return;
                }
                b'\\' => {
                    self.pos += 1;
                    if self.b.get(self.pos).is_some_and(|&n| n != b'\n') {
                        self.pos += 1;
                    }
                }
                b'$' | b'%' => match (self.b.get(self.pos + 1), self.b.get(self.pos + 2)) {
                    // `$${` and `%%{` are literal text.
                    (Some(&n), Some(b'{')) if n == c => self.pos += 3,
                    (Some(b'{'), _) => {
                        self.pos += 2;
                        self.skip_interpolation(nesting);
                    }
                    _ => self.pos += 1,
                },
                _ => self.pos += 1,
            }
        }
        self.incomplete = true;
    }

    /// Just past `${`: consumes through the matching `}`. Newlines are allowed
    /// here, unlike in the string around it.
    fn skip_interpolation(&mut self, nesting: usize) {
        let mut depth = 1usize;
        while let Some(&c) = self.b.get(self.pos) {
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += 1;
                        return;
                    }
                }
                b'"' if nesting < MAX_STRING_NESTING => {
                    self.skip_string(nesting + 1);
                    continue;
                }
                b'\n' => {
                    self.line += 1;
                    self.line_start = self.pos + 1;
                }
                _ => {}
            }
            self.pos += 1;
        }
        self.incomplete = true;
    }

    /// At `<`: if it opens a heredoc (`<<ID` / `<<-ID`), consume through the
    /// terminator line and emit one token. An unterminated heredoc runs to EOF.
    fn heredoc(&mut self, start: usize, line: usize, col: usize) -> bool {
        if self.b.get(start + 1) != Some(&b'<') {
            return false;
        }
        let mut at = start + 2;
        if self.b.get(at) == Some(&b'-') {
            at += 1;
        }
        let id_start = at;
        if !self
            .b
            .get(at)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
        {
            return false;
        }
        while self
            .b
            .get(at)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            at += 1;
        }
        let Some(id) = self.src.get(id_start..at) else {
            return false;
        };
        let opener_end = at;

        let mut cur = self.next_newline(at);
        let mut cur_line = self.line;
        let mut cur_start = self.line_start;
        let mut closed = false;
        while cur < self.b.len() {
            let ls = cur + 1;
            if ls >= self.b.len() {
                break;
            }
            let le = self.next_newline(ls);
            cur_line += 1;
            cur_start = ls;
            cur = le;
            if self.src.get(ls..le).is_some_and(|l| l.trim() == id) {
                closed = true;
                break;
            }
        }
        self.incomplete |= !closed;
        self.pos = cur;
        self.toks.push(Tok {
            kind: Kind::Heredoc,
            text: self.src.get(start..opener_end).unwrap_or(""),
            start,
            end: opener_end,
            line,
            col,
            end_line: cur_line,
            end_col: cur - cur_start,
        });
        self.line = cur_line;
        self.line_start = cur_start;
        true
    }

    /// Index of the first `\n` at or after `from`, or the source length.
    fn next_newline(&self, from: usize) -> usize {
        self.b
            .get(from..)
            .and_then(|rest| rest.iter().position(|&c| c == b'\n'))
            .map_or(self.b.len(), |i| from + i)
    }
}

// --------------------------------------------------------------- parser

enum Shape {
    Plain,
    Object(usize),
    List(usize),
}

struct Attr<'a> {
    name: &'a str,
    line: usize,
    col: usize,
    end_line: usize,
    end_col: usize,
    /// The expression's first line, empty when there is none.
    first: &'a str,
    shape: Shape,
}

struct Blk<'a> {
    kind: &'a str,
    labels: Vec<&'a str>,
    line: usize,
    col: usize,
    end_line: usize,
    end_col: usize,
    /// Index of the first token after the opening `{`.
    body_start: usize,
    /// Index of the closing `}` token, or the token count when unterminated.
    body_end: usize,
}

enum Item<'a> {
    Attr(Attr<'a>),
    Block(Blk<'a>),
}

/// The direct attributes and blocks of the tokens `start..end` (a block body,
/// or the whole file). Nothing is read past `end`, so a bracket left open in
/// one block cannot run into the next. Nested bodies are skipped, not
/// entered. Every iteration consumes a token.
fn parse_body<'a>(doc: &Doc<'a>, start: usize, end: usize) -> Vec<Item<'a>> {
    let toks = &doc.toks;
    let mut items = Vec::new();
    let mut i = start;
    while i < end {
        let Some(tok) = toks.get(i) else {
            break;
        };
        if tok.kind == Kind::Newline || tok.is(b'}') {
            i += 1;
        } else if tok.kind != Kind::Ident {
            i = expr_end(doc, i, end).max(i + 1);
        } else if toks.get(i + 1).is_some_and(|t| t.is(b'=')) {
            let expr_end = expr_end(doc, i + 2, end);
            items.push(Item::Attr(attribute(doc, tok, i + 2, expr_end)));
            i = expr_end;
        } else if let Some((block, next)) = block_at(doc, i, end) {
            i = next;
            items.push(Item::Block(block));
        } else {
            i = expr_end(doc, i, end).max(i + 1);
        }
    }
    items
}

/// A block whose type token is at `at`, and the index after its closing `}`.
fn block_at<'a>(doc: &Doc<'a>, at: usize, limit: usize) -> Option<(Blk<'a>, usize)> {
    let head = doc.toks.get(at)?;
    let mut labels = Vec::new();
    let mut j = at + 1;
    loop {
        let tok = doc.toks.get(j).filter(|_| j < limit)?;
        match tok.kind {
            Kind::Str => labels.push(tok.text.trim_matches('"')),
            Kind::Ident => labels.push(tok.text),
            Kind::Punct if tok.is(b'{') => break,
            _ => return None,
        }
        j += 1;
    }
    let body_start = j + 1;
    let (next, end_line, end_col, closed) = skip_braces(&doc.toks, body_start, limit);
    if !closed {
        doc.incomplete.set(true);
    }
    Some((
        Blk {
            kind: head.text,
            labels,
            line: head.line,
            col: head.col,
            end_line,
            end_col,
            body_start,
            body_end: if closed { next - 1 } else { next },
        },
        next,
    ))
}

/// From just after a `{`: the index after its matching `}` (or `limit` when
/// unterminated), the position where the body ends, and whether it closed.
fn skip_braces(toks: &[Tok], from: usize, limit: usize) -> (usize, usize, usize, bool) {
    let mut depth = 1usize;
    let mut i = from;
    while i < limit {
        let Some(tok) = toks.get(i) else {
            break;
        };
        i += 1;
        if tok.is(b'{') {
            depth += 1;
        } else if tok.is(b'}') {
            depth -= 1;
            if depth == 0 {
                return (i, tok.line, tok.end_col, true);
            }
        }
    }
    let last = toks.get(i.saturating_sub(1).min(toks.len().saturating_sub(1)));
    (
        i,
        last.map_or(1, |t| t.end_line),
        last.map_or(0, |t| t.end_col),
        false,
    )
}

/// Index of the token that ends an expression starting at `from`: the next
/// newline outside any bracket, an unmatched closer (the `}` of a one-line
/// block), or `limit`. Reaching `limit` inside a bracket means it was never
/// closed.
fn expr_end(doc: &Doc, from: usize, limit: usize) -> usize {
    let mut depth = 0usize;
    let mut i = from;
    while i < limit {
        let Some(tok) = doc.toks.get(i) else {
            break;
        };
        if tok.kind == Kind::Newline && depth == 0 {
            return i;
        }
        if tok.kind == Kind::Punct {
            match tok.text {
                "{" | "[" | "(" => depth += 1,
                "}" | "]" | ")" => {
                    if depth == 0 {
                        return i;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if depth > 0 {
        doc.incomplete.set(true);
    }
    i.min(limit)
}

/// `name = expr`: `name` is the name token and the expression is the tokens
/// `expr_start..end`.
fn attribute<'a>(doc: &Doc<'a>, name: &Tok<'a>, expr_start: usize, end: usize) -> Attr<'a> {
    let mut attr = Attr {
        name: name.text,
        line: name.line,
        col: name.col,
        end_line: name.end_line,
        end_col: name.end_col,
        first: "",
        shape: Shape::Plain,
    };
    let Some((first, _)) = doc
        .toks
        .get(expr_start..end)
        .and_then(|expr| expr.split_first())
    else {
        return attr;
    };
    let expr = doc.toks.get(expr_start..end).unwrap_or(&[]);
    if let Some(last) = expr.last() {
        attr.end_line = last.end_line;
        attr.end_col = last.end_col;
    }
    let first_line_end = expr
        .iter()
        .take_while(|t| t.kind != Kind::Newline)
        .last()
        .map_or(first.end, |t| t.end);
    attr.first = doc.src.get(first.start..first_line_end).unwrap_or("");
    let is_for = expr.get(1).is_some_and(|t| t.text == "for");
    // A bracketed value is shown by its size, even on one line: its literals
    // (`{ password = "x" }`) must not reach the signature.
    if !is_for {
        if first.is(b'{') {
            attr.shape = Shape::Object(count_entries(expr));
        } else if first.is(b'[') {
            attr.shape = Shape::List(count_entries(expr));
        }
    }
    attr
}

/// Entries of the bracketed expression whose opener is `expr[0]`: items
/// separated by commas or line breaks at its top level.
fn count_entries(expr: &[Tok]) -> usize {
    let mut depth = 1usize;
    let mut in_entry = false;
    let mut count = 0;
    for tok in expr.iter().skip(1) {
        if tok.kind == Kind::Punct {
            match tok.text {
                "{" | "[" | "(" => depth += 1,
                "}" | "]" | ")" => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                "," if depth == 1 => {
                    count += usize::from(in_entry);
                    in_entry = false;
                    continue;
                }
                _ => {}
            }
        }
        if tok.kind == Kind::Newline {
            if depth == 1 {
                count += usize::from(in_entry);
                in_entry = false;
            }
        } else {
            in_entry = true;
        }
    }
    count + usize::from(in_entry)
}

#[cfg(test)]
mod tests {
    use crate::{ConfigParseResult, Parser};
    use kungfu_types::file::Language;
    use kungfu_types::symbol::SymbolKind;

    const MAIN: &str = include_str!("../tests/fixtures/config/main.tf");
    const VARIABLES: &str = include_str!("../tests/fixtures/config/variables.tf");
    const TFVARS: &str = include_str!("../tests/fixtures/config/terraform.tfvars");

    fn parse_as(src: &str, path: &str, limit: usize) -> ConfigParseResult {
        Parser::new()
            .parse_config(src, Language::Hcl, "f:1", path, limit)
            .unwrap()
    }

    fn parse(src: &str) -> ConfigParseResult {
        parse_as(src, "main.tf", 500)
    }

    fn names(r: &ConfigParseResult) -> Vec<&str> {
        r.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    fn sig<'r>(r: &'r ConfigParseResult, name: &str) -> &'r str {
        r.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol {name} in {:?}", names(r)))
            .signature
            .as_deref()
            .unwrap()
    }

    fn lines(r: &ConfigParseResult, name: &str) -> (usize, usize) {
        let s = r.symbols.iter().find(|s| s.name == name).unwrap();
        (s.span.start_line, s.span.end_line)
    }

    #[test]
    fn main_tf_blocks_use_terraform_addresses() {
        let r = parse(MAIN);
        let top: Vec<&str> = r
            .symbols
            .iter()
            .filter(|s| s.parent_symbol_id.is_none())
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            top,
            [
                "terraform",
                "provider.aws",
                "locals",
                "aws_s3_bucket.logs",
                "aws_security_group.web",
                "data.aws_ami.ubuntu",
                "module.vpc",
                "output.bucket_arn",
            ]
        );
        assert!(r
            .symbols
            .iter()
            .filter(|s| s.parent_symbol_id.is_none())
            .all(|s| s.kind == SymbolKind::ConfigBlock));
        assert_eq!(r.total, r.symbols.len());
    }

    #[test]
    fn main_tf_signatures_and_spans() {
        let r = parse(MAIN);
        assert_eq!(
            sig(&r, "aws_s3_bucket.logs"),
            "aws_s3_bucket.logs = {4 items}"
        );
        assert_eq!(lines(&r, "aws_s3_bucket.logs"), (21, 38));
        assert_eq!(lines(&r, "provider.aws"), (8, 11));
        assert_eq!(lines(&r, "locals"), (13, 19));
        assert_eq!(lines(&r, "terraform"), (1, 6));
        assert_eq!(lines(&r, "aws_s3_bucket.logs.policy"), (28, 33));
        assert_eq!(lines(&r, "aws_s3_bucket.logs.lifecycle"), (35, 37));
        assert_eq!(sig(&r, "provider.aws"), "provider.aws = {2 attributes}");
    }

    #[test]
    fn main_tf_attributes_nested_blocks_and_heredoc() {
        let r = parse(MAIN);
        assert_eq!(
            sig(&r, "provider.aws.region"),
            "provider.aws.region = var.region"
        );
        assert_eq!(
            sig(&r, "aws_s3_bucket.logs.bucket"),
            "aws_s3_bucket.logs.bucket = \"acme-${var.env}-logs\""
        );
        // Heredoc attribute: the opener only, and the body braces do not
        // disturb the structure that follows.
        assert_eq!(
            sig(&r, "aws_s3_bucket.logs.policy"),
            "aws_s3_bucket.logs.policy = <<-POLICY"
        );
        // A brace in a string, a comment, and a heredoc line.
        assert_eq!(
            sig(&r, "aws_s3_bucket.logs.tags"),
            "aws_s3_bucket.logs.tags = {2 keys}"
        );
        let lifecycle = r
            .symbols
            .iter()
            .find(|s| s.name == "aws_s3_bucket.logs.lifecycle")
            .unwrap();
        assert_eq!(lifecycle.kind, SymbolKind::ConfigBlock);
        assert_eq!(
            lifecycle.signature.as_deref(),
            Some("aws_s3_bucket.logs.lifecycle = {1 attribute}")
        );
        let parent = r
            .symbols
            .iter()
            .find(|s| s.name == "aws_s3_bucket.logs")
            .unwrap();
        assert_eq!(
            lifecycle.parent_symbol_id.as_deref(),
            Some(parent.id.as_str())
        );
        assert_eq!(
            sig(&r, "module.vpc.cidr"),
            "module.vpc.cidr = \"10.0.0.0/16\""
        );
        assert_eq!(
            sig(&r, "output.bucket_arn.value"),
            "output.bucket_arn.value = aws_s3_bucket.logs.arn"
        );
    }

    #[test]
    fn repeated_nested_blocks_collapse_into_a_list() {
        let r = parse(MAIN);
        let ingress = r
            .symbols
            .iter()
            .find(|s| s.name == "aws_security_group.web.ingress")
            .unwrap();
        assert_eq!(ingress.kind, SymbolKind::ConfigBlock);
        assert_eq!(
            ingress.signature.as_deref(),
            Some("aws_security_group.web.ingress = [2 blocks]")
        );
        assert_eq!(
            names(&r)
                .iter()
                .filter(|n| **n == "aws_security_group.web.ingress")
                .count(),
            1
        );
        let (start, end) = (ingress.span.start_line, ingress.span.end_line);
        assert!(end > start + 5, "spans first through last ingress");
        assert_eq!(
            sig(&r, "aws_security_group.web"),
            "aws_security_group.web = {5 items}"
        );
    }

    #[test]
    fn variables_tf_names_use_var_prefix() {
        let r = parse_as(VARIABLES, "variables.tf", 500);
        let top: Vec<&str> = r
            .symbols
            .iter()
            .filter(|s| s.parent_symbol_id.is_none())
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            top,
            ["var.region", "var.env", "var.db_password", "var.tags"]
        );
        assert_eq!(
            sig(&r, "var.region.default"),
            "var.region.default = \"eu-west-1\""
        );
        assert_eq!(
            sig(&r, "var.db_password.sensitive"),
            "var.db_password.sensitive = true"
        );
        assert_eq!(
            sig(&r, "var.env.validation"),
            "var.env.validation = {2 attributes}"
        );
        // One-line block.
        assert_eq!(sig(&r, "var.tags"), "var.tags = {1 attribute}");
        assert_eq!(lines(&r, "var.tags"), (21, 21));
        assert_eq!(lines(&r, "var.env.validation"), (10, 13));
    }

    #[test]
    fn tfvars_attributes_are_level_one_keys_with_secrets_redacted() {
        let r = parse_as(TFVARS, "terraform.tfvars", 500);
        assert!(r.symbols.iter().all(|s| s.parent_symbol_id.is_none()));
        assert_eq!(sig(&r, "region"), "region = \"eu-west-1\"");
        assert_eq!(sig(&r, "db_password"), "db_password = <redacted>");
        assert_eq!(sig(&r, "subnets"), "subnets = [3 items]");
        assert_eq!(sig(&r, "tags"), "tags = {2 keys}");
        assert!(r.symbols.iter().all(|s| s.kind == SymbolKind::ConfigKey));
        assert!(!r
            .symbols
            .iter()
            .any(|s| s.signature.as_deref().unwrap().contains("hunter2")));
    }

    #[test]
    fn address_conventions() {
        let src = r#"
variable "region" {}
data "aws_ami" "x" {}
resource "aws_s3_bucket" "logs" {}
output "o" { value = 1 }
module "vpc" {}
provider "aws" {}
locals {}
terraform {}
import {
  to = aws_s3_bucket.logs
}
check "health" {}
resource {}
"#;
        let r = parse(src);
        assert_eq!(
            names(&r),
            [
                "var.region",
                "data.aws_ami.x",
                "aws_s3_bucket.logs",
                "output.o",
                "output.o.value",
                "module.vpc",
                "provider.aws",
                "locals",
                "terraform",
                "import",
                "import.to",
                "check.health",
                "resource",
            ]
        );
    }

    #[test]
    fn braces_in_strings_comments_and_heredocs_do_not_change_depth() {
        let src = r#"resource "t" "a" {
  s = "}}} {"
  # } comment {
  // } another {
  /* } block
     { comment */
  h = <<EOF
}
{ "x": { }
EOF
  t = "${lookup({a = "}"}, "a")} \" }"
  after = 1
}

resource "t" "b" {
  k = 2
}
"#;
        let r = parse(src);
        let child: Vec<&str> = names(&r)
            .into_iter()
            .filter(|n| n.starts_with("t.a."))
            .collect();
        assert_eq!(child, ["t.a.s", "t.a.h", "t.a.t", "t.a.after"]);
        assert_eq!(lines(&r, "t.a"), (1, 13));
        assert_eq!(lines(&r, "t.b"), (15, 17));
        assert_eq!(sig(&r, "t.a.s"), "t.a.s = \"}}} {\"");
        assert_eq!(sig(&r, "t.a.h"), "t.a.h = <<EOF");
        assert_eq!(lines(&r, "t.a.h"), (7, 10));
        assert_eq!(lines(&r, "t.a.after"), (12, 12));
    }

    #[test]
    fn one_line_block_and_trailing_content() {
        let r = parse("variable \"x\" { default = 1 }\nvariable \"y\" { type = string, }\n");
        assert_eq!(names(&r), ["var.x", "var.x.default", "var.y", "var.y.type"]);
        assert_eq!(lines(&r, "var.x"), (1, 1));
        assert_eq!(sig(&r, "var.x"), "var.x = {1 attribute}");
    }

    #[test]
    fn unterminated_input_neither_panics_nor_hangs() {
        for src in [
            "resource \"a\" \"b\" {\n  x = 1\n",
            "resource \"a\" \"b\" {\n  x = <<EOF\nnever closed {\n",
            "resource \"a\" \"b\" {\n  x = \"${foo\n  y = 2\n}\n",
            "resource \"a\" \"b\" {\n  x = [1,\n",
            "resource \"a\" \"b\" {\n  /* open comment\n",
            "resource \"a\" \"b\" \"unfinished\"",
            "}}}}\n)]\n",
            "x = \"unterminated\ny = 2\n",
            "<<",
            "<<-",
            "\"",
            "a = \"${\"",
            "é = ü ö",
        ] {
            let r = parse(src);
            assert_eq!(r.total, r.symbols.len(), "{src:?}");
        }
        let r = parse("resource \"a\" \"b\" {\n  x = 1\n");
        assert_eq!(names(&r), ["a.b", "a.b.x"]);
        assert_eq!(lines(&r, "a.b"), (1, 2));
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let src = format!("a {}b = 1{}", "x {\n".repeat(50_000), "}\n".repeat(50_000));
        let r = parse(&src);
        assert_eq!(names(&r), ["a.x", "a.x.x"]);
    }

    #[test]
    fn crlf_line_endings() {
        let src = MAIN.replace('\n', "\r\n");
        let lf = parse(MAIN);
        let crlf = parse(&src);
        assert_eq!(names(&crlf), names(&lf));
        let sigs = |r: &ConfigParseResult| {
            r.symbols
                .iter()
                .map(|s| (s.signature.clone(), s.span.start_line, s.span.end_line))
                .collect::<Vec<_>>()
        };
        assert_eq!(sigs(&crlf), sigs(&lf));
    }

    #[test]
    fn empty_and_comment_only_files_yield_nothing() {
        assert!(parse("").symbols.is_empty());
        assert!(parse("\n\n").symbols.is_empty());
        assert!(parse("# just a comment\n/* x */\n").symbols.is_empty());
    }

    #[test]
    fn total_counts_past_the_limit() {
        let r = parse_as(MAIN, "main.tf", 5);
        let full = parse(MAIN);
        assert_eq!(r.symbols.len(), 5);
        assert!(r.total > 5);
        assert_eq!(r.total, full.total);
        // Over the cap, level-1 blocks come first, in document order.
        let level1: Vec<&str> = full
            .symbols
            .iter()
            .filter(|s| s.parent_symbol_id.is_none())
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names(&r), level1[..5]);
        assert!(!r.has_errors);
    }

    #[test]
    fn multiline_list_and_object_counts() {
        let src = "r \"a\" {\n  l = [\n    \"x\",\n    \"y\", # c\n  ]\n  o = {\n    a = 1\n    b = { c = 2 }\n  }\n  f = [for x in y : x]\n  m = merge({\n    k = 1\n  }, {})\n}\n";
        let r = parse(src);
        assert_eq!(sig(&r, "r.a.l"), "r.a.l = [2 items]");
        assert_eq!(sig(&r, "r.a.o"), "r.a.o = {2 keys}");
        assert_eq!(sig(&r, "r.a.f"), "r.a.f = [for x in y : x]");
        assert_eq!(sig(&r, "r.a.m"), "r.a.m = merge({");
        assert_eq!(lines(&r, "r.a.l"), (2, 5));
    }

    #[test]
    fn many_distinct_nested_block_types_scale_linearly() {
        let mut src = String::from("r \"a\" {\n");
        for n in 0..30_000 {
            src.push_str(&format!("b{n} {{}}\n"));
        }
        src.push_str("}\n");
        let r = parse_as(&src, "main.tf", 10);
        assert_eq!(r.total, 30_001);
        assert_eq!(r.symbols.len(), 10);
    }

    #[test]
    fn unmatched_bracket_cannot_run_past_its_block() {
        let n = 30_000;
        let src = "r \"aN\" {\n x = [\n}\n".repeat(n);
        let r = parse_as(&src, "main.tf", 50);
        // One block and its `x` per repetition, no fake children; over the
        // cap the 50 kept symbols are all blocks.
        assert_eq!(r.total, 2 * n);
        assert!(names(&r).iter().all(|n| *n == "r.aN"));
        assert!(r.has_errors);
        let small = parse("r \"a\" {\n x = [\n}\nr \"b\" {\n y = 1\n}\n");
        assert_eq!(names(&small), ["r.a", "r.a.x", "r.b", "r.b.y"]);
        assert_eq!(lines(&small, "r.a"), (1, 3));
    }

    #[test]
    fn labeled_nested_blocks_keep_their_labels() {
        let src = "r \"a\" {\n  dynamic \"ingress\" {}\n  dynamic \"egress\" {}\n  dynamic \"egress\" {}\n  provisioner \"local-exec\" { command = \"x\" }\n  ingress {}\n}\n";
        let r = parse(src);
        assert_eq!(
            names(&r),
            [
                "r.a",
                "r.a.dynamic.ingress",
                "r.a.dynamic.egress",
                "r.a.provisioner.local-exec",
                "r.a.ingress"
            ]
        );
        assert_eq!(
            sig(&r, "r.a.dynamic.egress"),
            "r.a.dynamic.egress = [2 blocks]"
        );
        assert_eq!(
            sig(&r, "r.a.dynamic.ingress"),
            "r.a.dynamic.ingress = {0 attributes}"
        );
        assert_eq!(sig(&r, "r.a"), "r.a = {5 items}");
    }

    #[test]
    fn one_line_bracketed_values_never_reach_the_signature() {
        let r = parse_as(
            "db = { username = \"admin\", password = \"hunter2\" }\nlist = [\"a\", \"b\", \"c\"]\nf = [for x in y : x]\nplain = \"v\"\n",
            "x.tfvars",
            50,
        );
        assert_eq!(sig(&r, "db"), "db = {2 keys}");
        assert_eq!(sig(&r, "list"), "list = [3 items]");
        assert_eq!(sig(&r, "f"), "f = [for x in y : x]");
        assert_eq!(sig(&r, "plain"), "plain = \"v\"");
        assert!(!r
            .symbols
            .iter()
            .any(|s| s.signature.as_deref().unwrap().contains("hunter2")));
    }

    #[test]
    fn sensitive_variable_default_is_hidden() {
        let r = parse(
            "variable \"a\" {\n  default = \"plainvalue\"\n  sensitive = true\n}\nvariable \"b\" {\n  default = \"shown\"\n}\n",
        );
        assert_eq!(sig(&r, "var.a.default"), "var.a.default");
        assert_eq!(sig(&r, "var.b.default"), "var.b.default = \"shown\"");
    }

    #[test]
    fn partial_parses_are_flagged() {
        for src in [
            "r \"a\" {\n x = 1\n",
            "r \"a\" {\n x = <<EOF\nopen\n",
            "r \"a\" {\n x = \"${foo\n}\n",
            "r \"a\" {\n x = [1,\n}\n",
            "r \"a\" {\n x = (1,\n}\n",
            "r \"a\" {}\n/* open\n",
            "x = \"open\ny = 1\n",
        ] {
            assert!(parse(src).has_errors, "{src:?}");
        }
        assert!(!parse(MAIN).has_errors);
        assert!(!parse(VARIABLES).has_errors);
        assert!(!parse(TFVARS).has_errors);
    }

    #[test]
    fn bom_and_lone_cr() {
        // A lone `\r` ends a statement but is not a line for numbering.
        let src = "\u{feff}a = \"eu\"\rdb_password = \"hunter2\"\nb = 1\r\n";
        let r = parse(src);
        assert_eq!(names(&r), ["a", "db_password", "b"]);
        assert_eq!(sig(&r, "a"), "a = \"eu\"");
        assert_eq!(sig(&r, "db_password"), "db_password = <redacted>");
        assert_eq!(lines(&r, "a"), (1, 1));
        assert_eq!(lines(&r, "db_password"), (1, 1));
        assert_eq!(lines(&r, "b"), (2, 2));
        assert_eq!(src.lines().count(), 2);
    }
}
