#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod c_parser;
pub mod cpp_parser;
pub mod csharp_parser;
pub mod go_parser;
pub mod java_parser;
pub mod kotlin_parser;
pub mod python_parser;
pub mod rust_parser;
pub mod typescript_parser;

use anyhow::{bail, Result};
use kungfu_types::file::Language;
use std::borrow::Cow;

mod calls;
pub mod config_symbols;
mod csv_config;
mod dockerfile_config;
mod env_config;
mod hcl_config;
mod ini_config;
mod json_config;
mod makefile_config;
mod toml_config;
mod xml_config;
mod yaml_config;
use kungfu_types::symbol::Symbol;

/// Raw import extracted from source code.
#[derive(Debug, Clone)]
pub struct RawImport {
    /// The import path as written in source (e.g. "crate::scanner", "./bar", "fmt").
    pub path: String,
    /// Specific names imported (e.g. ["Result", "Context"]), empty for wildcard/module imports.
    pub names: Vec<String>,
    /// Line number of the import statement.
    pub line: usize,
}

/// A function/method call site extracted from source code.
#[derive(Debug, Clone)]
pub struct RawCall {
    /// Start line (1-based) of the enclosing function/method — matches the symbol id line.
    pub caller_line: usize,
    /// Simple (unqualified) name of the callee, e.g. `foo` for `a::b::foo()` or `x.foo()`.
    pub callee: String,
    /// True for receiver method calls (`x.foo()`), whose target type is unknown. These can
    /// only be resolved when the name is globally unique; free/path calls may resolve locally.
    pub is_method: bool,
}

/// Classification of a source code comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    Todo,
    Fixme,
    Note,
    Hack,
    Doc,
    Regular,
}

/// A comment extracted from source code.
#[derive(Debug, Clone)]
pub struct RawComment {
    pub text: String,
    pub line: usize,
    pub end_line: usize,
    pub kind: CommentKind,
    /// ID of the symbol this comment is attached to (directly precedes).
    pub attached_symbol_id: Option<String>,
}

pub struct ParseResult {
    pub symbols: Vec<Symbol>,
    pub imports: Vec<RawImport>,
    pub comments: Vec<RawComment>,
    /// Call sites within this file, for all supported code languages.
    pub calls: Vec<RawCall>,
    /// One-line file purpose from the module-level doc comment, if the file has one.
    pub module_doc: Option<String>,
}

/// Last-resort parse-time budget, per config file. The deterministic guards
/// below (YAML indentation, XML nesting depth) run first and catch the known
/// pathological inputs; this backstop only stops an unknown one from stalling
/// an index run. Generous on purpose: a legitimate 2 MiB file parses in well
/// under a second, and a file that times out loses all its symbols — a
/// result that depends on machine load, so it must stay out of reach of real
/// files even on a slow, busy machine.
const CONFIG_PARSE_TIMEOUT_MICROS: u64 = 5_000_000;

/// Longest leading run of indentation and block indicators (`- `, `? `,
/// `: `) kept on a YAML line; longer lines are blanked before parsing (see
/// [`blank_deep_yaml_lines`]). tree-sitter-yaml aborts the process
/// (`length <= 1024` assertion when serialising its scanner state) once its
/// indentation stack holds 254 entries. A stack entry needs a new column —
/// except that a block sequence may sit at its parent mapping's column
/// (`MAY_PUSH_SPC_SEQ_IND` in its scanner.c), so `k:` / `-` pairs one column
/// apart push two entries per column. Measured with that shape: a 126-column
/// run (253 entries) parses, 127 columns aborts. 60 columns allows at most
/// 2 × 61 + 1 (an indented block scalar) = 123 entries, under half of 254 —
/// and is still 30 levels of 2-space indentation.
const MAX_YAML_BLOCK_INDENT: usize = 60;

/// Deepest XML element nesting parsed. tree-sitter-xml keeps open tag names
/// in a scanner state capped at 1024 bytes; past ~508 levels (fewer with
/// longer names) it loses track, recovers from errors with no symbols, and
/// gets superlinear: measured (debug build) 400 levels 12 ms with correct
/// output, 800 levels 72 ms, 1600 levels 188 ms, 3200 levels 532 ms, 100k
/// levels minutes — all past ~508 with no symbols. 400 keeps the guard below
/// that cliff while real XML stays far below it.
const MAX_XML_DEPTH: usize = 400;

/// Whether XML element nesting goes deeper than `limit`, in one linear scan:
/// `<name …>` opens, `</…>` closes, `<…/>` is self-contained; comments,
/// CDATA, processing instructions and `<!…>` declarations are skipped.
fn xml_nesting_exceeds(source: &str, limit: usize) -> bool {
    /// Bytes up to and including the first `pat`, or all of `rest`.
    fn through(rest: &[u8], pat: &[u8]) -> usize {
        rest.windows(pat.len())
            .position(|w| w == pat)
            .map_or(rest.len(), |at| at + pat.len())
    }
    let bytes = source.as_bytes();
    let (mut i, mut depth) = (0, 0usize);
    while let Some(offset) = bytes[i..].iter().position(|&b| b == b'<') {
        i += offset;
        let rest = &bytes[i..];
        if rest.starts_with(b"<!--") {
            i += through(rest, b"-->");
        } else if rest.starts_with(b"<![CDATA[") {
            i += through(rest, b"]]>");
        } else if rest.starts_with(b"<?") {
            i += through(rest, b"?>");
        } else if rest.starts_with(b"<!") {
            // `<!DOCTYPE x [ <!ENTITY …> ]>`: the internal subset nests.
            let mut brackets = 0usize;
            let end = rest.iter().position(|&b| match b {
                b'[' => {
                    brackets += 1;
                    false
                }
                b']' => {
                    brackets = brackets.saturating_sub(1);
                    false
                }
                b'>' => brackets == 0,
                _ => false,
            });
            i += end.map_or(rest.len(), |at| at + 1);
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            i += through(rest, b">");
        } else if rest
            .get(1)
            .is_some_and(|&b| b.is_ascii_alphabetic() || b == b'_' || b == b':' || b >= 0x80)
        {
            // Find the tag's `>`, skipping quoted attribute values.
            let mut quote = None;
            let mut previous = 0u8;
            let mut end = rest.len();
            for (at, &b) in rest.iter().enumerate().skip(1) {
                match quote {
                    Some(q) if b == q => quote = None,
                    Some(_) => {}
                    None if b == b'"' || b == b'\'' => quote = Some(b),
                    None if b == b'>' => {
                        end = at + 1;
                        break;
                    }
                    None => {}
                }
                previous = b;
            }
            if previous != b'/' {
                depth += 1;
                if depth > limit {
                    return true;
                }
            }
            i += end;
        } else {
            i += 1;
        }
    }
    false
}

/// `source` with the content of every line whose [`yaml_block_indent`]
/// exceeds [`MAX_YAML_BLOCK_INDENT`] removed — the line terminator stays, so
/// every kept line keeps its row and its byte columns, which is all a symbol
/// span records. Lines end at `\n`, `\r` or `\r\n`, as tree-sitter-yaml sees
/// them. Borrowed when no line is that deep. Also returns how many lines were
/// blanked. Only levels 1–2 are indexed, so the dropped lines (deeper than
/// 30 levels of 2-space indentation) never reach the output.
fn blank_deep_yaml_lines(source: &str) -> (Cow<'_, str>, usize) {
    let bytes = source.as_bytes();
    let mut copy: Option<String> = None;
    let mut blanked = 0;
    let mut start = 0;
    loop {
        let end = bytes[start..]
            .iter()
            .position(|&b| b == b'\n' || b == b'\r')
            .map_or(bytes.len(), |at| start + at);
        let line = &source[start..end];
        if yaml_block_indent(line) > MAX_YAML_BLOCK_INDENT {
            copy.get_or_insert_with(|| source[..start].to_string());
            blanked += 1;
        } else if let Some(copy) = copy.as_mut() {
            copy.push_str(line);
        }
        if end == bytes.len() {
            break;
        }
        let next = if bytes[end] == b'\r' && bytes.get(end + 1) == Some(&b'\n') {
            end + 2
        } else {
            end + 1
        };
        if let Some(copy) = copy.as_mut() {
            copy.push_str(&source[end..next]);
        }
        start = next;
    }
    (copy.map_or(Cow::Borrowed(source), Cow::Owned), blanked)
}

/// Columns of indentation and block indicators before a YAML line's
/// content. An indicator counts only when followed by whitespace or the end
/// of the line, as in YAML itself, so `------` in a scalar does not.
fn yaml_block_indent(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' => i += 1,
            b'-' | b'?' | b':'
                if bytes
                    .get(i + 1)
                    .is_none_or(|b| matches!(b, b' ' | b'\t' | b'\r')) =>
            {
                i += 1
            }
            _ => break,
        }
    }
    i
}

/// Symbols extracted from a config file by [`Parser::parse_config`].
pub struct ConfigParseResult {
    /// At most `limit` config symbols in document order (a parent precedes
    /// its children). Over the limit, level-1 nodes are kept first, then
    /// level-2 nodes fill what is left.
    pub symbols: Vec<Symbol>,
    /// How many config symbols the file has before the cap — equal to
    /// `symbols.len()` when nothing was trimmed.
    pub total: usize,
    /// The grammar hit syntax errors and recovered (JSON5 in a `.json`, a
    /// missing comma), so `symbols` may be incomplete.
    pub has_errors: bool,
}

/// Classify a comment's text into a CommentKind.
fn classify_comment(text: &str) -> CommentKind {
    let trimmed = text.trim_start_matches('/').trim_start_matches('*').trim();
    let upper = trimmed.to_uppercase();
    if upper.starts_with("TODO") {
        CommentKind::Todo
    } else if upper.starts_with("FIXME") {
        CommentKind::Fixme
    } else if upper.starts_with("NOTE") {
        CommentKind::Note
    } else if upper.starts_with("HACK") || upper.starts_with("XXX") {
        CommentKind::Hack
    } else {
        CommentKind::Regular
    }
}

/// Check if a tree-sitter node is a doc comment based on language conventions.
fn is_doc_comment(text: &str, language: Language) -> bool {
    match language {
        Language::Rust => {
            text.starts_with("///") || text.starts_with("//!") || text.starts_with("/**")
        }
        Language::Java | Language::CSharp | Language::Kotlin | Language::Cpp => {
            text.starts_with("/**")
        }
        Language::TypeScript | Language::JavaScript => text.starts_with("/**"),
        Language::Python => text.starts_with("\"\"\"") || text.starts_with("'''"),
        _ => false,
    }
}

/// Comment node types per language grammar.
fn comment_node_types(language: Language) -> &'static [&'static str] {
    match language {
        Language::Rust => &["line_comment", "block_comment"],
        Language::TypeScript | Language::JavaScript => &["comment"],
        Language::Python => &["comment"],
        Language::Go => &["comment"],
        Language::Java => &["line_comment", "block_comment"],
        Language::CSharp => &["comment"],
        Language::Kotlin => &["line_comment", "multiline_comment"],
        Language::C | Language::Cpp => &["comment"],
        _ => &[],
    }
}

/// Extract all comments from a tree-sitter AST.
fn extract_comments(
    root: tree_sitter::Node,
    source: &str,
    language: Language,
    symbols: &[Symbol],
) -> Vec<RawComment> {
    let node_types = comment_node_types(language);
    if node_types.is_empty() {
        return Vec::new();
    }

    let mut comments = Vec::new();
    let mut cursor = root.walk();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        if node_types.contains(&node.kind()) {
            let text = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            if text.trim().is_empty() {
                continue;
            }

            let line = node.start_position().row + 1;
            let end_line = node.end_position().row + 1;

            let kind = if is_doc_comment(&text, language) {
                CommentKind::Doc
            } else {
                classify_comment(&text)
            };

            // Only keep actionable comments (skip Regular)
            if kind == CommentKind::Regular {
                continue;
            }

            // Find symbol attached to this comment (next sibling or first symbol starting right after)
            let attached_symbol_id = symbols
                .iter()
                .find(|s| s.span.start_line == end_line + 1 || s.span.start_line == end_line)
                .map(|s| s.id.clone());

            comments.push(RawComment {
                text: clean_comment_text(&text),
                line,
                end_line,
                kind,
                attached_symbol_id,
            });
        }

        // Push children in reverse order for DFS
        cursor.reset(node);
        if cursor.goto_first_child() {
            let mut children = Vec::new();
            loop {
                children.push(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }
    }

    comments
}

/// Strip comment delimiters and normalize whitespace.
fn clean_comment_text(text: &str) -> String {
    let text = text.trim();
    // Handle multi-line block comments
    if text.starts_with("/**") || text.starts_with("/*") {
        let inner = text
            .trim_start_matches("/**")
            .trim_start_matches("/*")
            .trim_end_matches("*/")
            .trim();
        return inner
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
    }
    // Handle line comments
    if text.starts_with("///") || text.starts_with("//!") {
        return text
            .trim_start_matches('/')
            .trim_start_matches('!')
            .trim()
            .to_string();
    }
    if text.starts_with("//") {
        return text.trim_start_matches('/').trim().to_string();
    }
    // Python docstrings
    if text.starts_with("\"\"\"") || text.starts_with("'''") {
        return text
            .trim_start_matches('"')
            .trim_start_matches('\'')
            .trim_end_matches('"')
            .trim_end_matches('\'')
            .trim()
            .to_string();
    }
    text.to_string()
}

/// Fill `doc_summary` on symbols from their attached Doc comments.
fn fill_doc_summaries(symbols: &mut [Symbol], comments: &[RawComment]) {
    for comment in comments {
        if comment.kind != CommentKind::Doc {
            continue;
        }
        if let Some(ref sym_id) = comment.attached_symbol_id {
            if let Some(sym) = symbols.iter_mut().find(|s| s.id == *sym_id) {
                if sym.doc_summary.is_none() {
                    sym.doc_summary = Some(first_sentence(&comment.text, 120));
                }
            }
        }
    }
}

/// Extract the module-level doc comment from the head of a source file.
///
/// Works on raw lines rather than the AST: Python docstrings and Go package
/// comments are not comment nodes tree-sitter's comment walk would surface,
/// and Rust `//!` is indistinguishable from `///` after `clean_comment_text`.
/// Returns the first sentence, or None when the file has no module doc.
fn extract_module_doc(source: &str, language: Language) -> Option<String> {
    let doc = match language {
        Language::Rust => leading_line_doc(source, "//!"),
        Language::Python => python_module_docstring(source),
        Language::Go => go_package_doc(source),
        Language::TypeScript
        | Language::JavaScript
        | Language::Java
        | Language::CSharp
        | Language::Kotlin
        | Language::C
        | Language::Cpp => detached_header_block_doc(source),
        _ => None,
    }?;
    let doc = doc.trim();
    if doc.is_empty() {
        return None;
    }
    let sentence = first_sentence(doc.trim_start_matches(['#', ' ']), 160);
    // A header that opens with a doc tag / compiler pragma (`@flow`,
    // `@jsxImportSource`, `@module`) is tooling metadata, not a description.
    if sentence.starts_with('@') || looks_like_license_header(&sentence) {
        return None;
    }
    Some(sentence)
}

/// License/copyright file headers are boilerplate, not a file purpose — storing
/// them as one would be a confidently wrong label. Better no purpose than noise.
fn looks_like_license_header(sentence: &str) -> bool {
    let lower = sentence.to_lowercase();
    [
        "copyright",
        "spdx-license",
        "all rights reserved",
        "public domain",
        "licensed under",
        "licensed to",
        "mit licensed",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Consecutive `prefix` lines at the top of the file (blank lines before them allowed).
fn leading_line_doc(source: &str, prefix: &str) -> Option<String> {
    let mut collected: Vec<&str> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            collected.push(rest.trim());
        } else if trimmed.is_empty() && collected.is_empty() {
            continue;
        } else {
            break;
        }
    }
    join_doc_lines(&collected)
}

/// Module docstring: first statement after optional shebang/encoding comments.
fn python_module_docstring(source: &str) -> Option<String> {
    let mut lines = source.lines().peekable();
    // Skip shebang, encoding/comment lines, and blanks before the first statement.
    while let Some(line) = lines.peek() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            lines.next();
        } else {
            break;
        }
    }
    let first = lines.next()?.trim_start();
    let delim = if first.starts_with("\"\"\"") {
        "\"\"\""
    } else if first.starts_with("'''") {
        "'''"
    } else {
        return None;
    };
    let after_open = &first[delim.len()..];
    if let Some(end) = after_open.find(delim) {
        return Some(after_open[..end].trim().to_string());
    }
    let mut collected: Vec<&str> = vec![after_open.trim()];
    for line in lines {
        if let Some(end) = line.find(delim) {
            collected.push(line[..end].trim());
            return join_doc_lines(&collected);
        }
        collected.push(line.trim());
    }
    None // unterminated docstring
}

/// Go doc convention: `//` comment block directly above the `package` clause.
fn go_package_doc(source: &str) -> Option<String> {
    let mut block: Vec<&str> = Vec::new();
    for line in source.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("//") {
            block.push(rest.trim());
        } else if t.starts_with("package ") {
            return join_doc_lines(&block);
        } else if t.is_empty() {
            // Blank line detaches the block from the package clause.
            block.clear();
        } else {
            return None;
        }
    }
    None
}

/// `/** … */` (or `/*! … */`) block at the top of the file that is NOT attached
/// to the following declaration: it counts as a file header only when followed
/// by a blank line or an import/package/preprocessor line. A block directly
/// above a declaration is that declaration's doc, not the file's.
fn detached_header_block_doc(source: &str) -> Option<String> {
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.peek() {
        if line.trim().is_empty() {
            lines.next();
        } else {
            break;
        }
    }
    let first = lines.peek()?.trim_start();
    if !(first.starts_with("/**") || first.starts_with("/*!")) {
        return None;
    }

    let mut block: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        block.push(line);
        if line.contains("*/") {
            closed = true;
            break;
        }
    }
    if !closed {
        return None;
    }

    let detached = match lines.next() {
        None => true,
        Some(next) => {
            let t = next.trim();
            t.is_empty()
                || t.starts_with("import ")
                || t.starts_with("package ")
                || t.starts_with("using ")
                || t.starts_with("#include")
                || t.starts_with("#pragma")
                || t.starts_with("'use ")
                || t.starts_with("\"use ")
        }
    };
    if !detached {
        return None;
    }

    let cleaned = clean_comment_text(&block.join("\n"));
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn join_doc_lines(lines: &[&str]) -> Option<String> {
    let joined = lines
        .iter()
        // Drop blank lines and punctuation-only rulers (`~~~~`, `====`, `****`)
        // used as section underlines in RST/plain-text docs.
        .filter(|l| !l.is_empty() && l.chars().any(|c| c.is_alphanumeric()))
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// Extract the first sentence from text, truncated to max_len.
fn first_sentence(text: &str, max_len: usize) -> String {
    // Take first line or up to first period
    let first = text.lines().next().unwrap_or(text).trim();
    let sentence = if let Some(dot) = first.find(". ") {
        &first[..=dot]
    } else {
        first
    };
    if sentence.len() <= max_len {
        sentence.to_string()
    } else {
        // Truncate on a UTF-8 char boundary at or below max_len so multi-byte
        // characters (e.g. Cyrillic) don't trigger a slice panic.
        let mut end = max_len;
        while end > 0 && !sentence.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &sentence[..end])
    }
}

pub struct Parser {
    ts_parser: tree_sitter::Parser,
    /// [`CONFIG_PARSE_TIMEOUT_MICROS`]; a field so tests can reach the
    /// timeout path without waiting seconds.
    config_timeout_micros: u64,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            ts_parser: tree_sitter::Parser::new(),
            config_timeout_micros: CONFIG_PARSE_TIMEOUT_MICROS,
        }
    }

    pub fn extract_symbols(
        &mut self,
        source: &str,
        language: Language,
        file_id: &str,
        file_path: &str,
    ) -> Result<Vec<Symbol>> {
        Ok(self.parse(source, language, file_id, file_path)?.symbols)
    }

    pub fn parse(
        &mut self,
        source: &str,
        language: Language,
        file_id: &str,
        file_path: &str,
    ) -> Result<ParseResult> {
        let ts_language = match language {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            _ => bail!("no parser for language: {}", language),
        };

        self.ts_parser.set_language(&ts_language)?;

        let tree = self
            .ts_parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("failed to parse {}", file_path))?;

        let root = tree.root_node();

        let (mut symbols, imports) = match language {
            Language::Rust => (
                rust_parser::extract(root, source, file_id, file_path),
                rust_parser::extract_imports(root, source),
            ),
            Language::TypeScript | Language::JavaScript => (
                typescript_parser::extract(root, source, file_id, file_path),
                typescript_parser::extract_imports(root, source),
            ),
            Language::Python => (
                python_parser::extract(root, source, file_id, file_path),
                python_parser::extract_imports(root, source),
            ),
            Language::Go => (
                go_parser::extract(root, source, file_id, file_path),
                go_parser::extract_imports(root, source),
            ),
            Language::Java => (
                java_parser::extract(root, source, file_id, file_path),
                java_parser::extract_imports(root, source),
            ),
            Language::CSharp => (
                csharp_parser::extract(root, source, file_id, file_path),
                csharp_parser::extract_imports(root, source),
            ),
            Language::Kotlin => (
                kotlin_parser::extract(root, source, file_id, file_path),
                kotlin_parser::extract_imports(root, source),
            ),
            Language::C => (
                c_parser::extract(root, source, file_id, file_path),
                c_parser::extract_imports(root, source),
            ),
            Language::Cpp => (
                cpp_parser::extract(root, source, file_id, file_path),
                cpp_parser::extract_imports(root, source),
            ),
            _ => (Vec::new(), Vec::new()),
        };

        let comments = extract_comments(root, source, language, &symbols);

        // Fill doc_summary on symbols from attached Doc comments
        fill_doc_summaries(&mut symbols, &comments);

        let calls = match language {
            Language::Rust => rust_parser::extract_calls(root, source),
            Language::TypeScript | Language::JavaScript => {
                typescript_parser::extract_calls(root, source)
            }
            Language::Python => python_parser::extract_calls(root, source),
            Language::Go => go_parser::extract_calls(root, source),
            Language::Java => java_parser::extract_calls(root, source),
            Language::CSharp => csharp_parser::extract_calls(root, source),
            Language::Kotlin => kotlin_parser::extract_calls(root, source),
            Language::C => c_parser::extract_calls(root, source),
            Language::Cpp => cpp_parser::extract_calls(root, source),
            _ => Vec::new(),
        };

        Ok(ParseResult {
            symbols,
            imports,
            comments,
            calls,
            module_doc: extract_module_doc(source, language),
        })
    }

    /// Extract config symbols (keys two levels deep, see [`config_symbols`])
    /// from a file whose `language.is_config()`, keeping the first `limit` and
    /// counting the rest. Config files yield no imports, comments or calls.
    pub fn parse_config(
        &mut self,
        source: &str,
        language: Language,
        file_id: &str,
        file_path: &str,
        limit: usize,
    ) -> Result<ConfigParseResult> {
        let mut out = config_symbols::ConfigSymbols::new(language, file_id, file_path, limit);
        let grammar: tree_sitter::Language = match language {
            Language::Json => tree_sitter_json::LANGUAGE.into(),
            Language::Yaml => tree_sitter_yaml::LANGUAGE.into(),
            Language::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
            Language::Xml => tree_sitter_xml::LANGUAGE_XML.into(),
            Language::Hcl => {
                hcl_config::extract(source, &mut out);
                return Ok(out.finish(false));
            }
            Language::Csv => {
                csv_config::extract(source, &mut out);
                return Ok(out.finish(false));
            }
            Language::Env => {
                env_config::extract(source, &mut out);
                return Ok(out.finish(false));
            }
            Language::Ini => {
                ini_config::extract(source, &mut out);
                return Ok(out.finish(false));
            }
            Language::Dockerfile => {
                dockerfile_config::extract(source, &mut out);
                return Ok(out.finish(false));
            }
            Language::Makefile => {
                makefile_config::extract(source, &mut out);
                return Ok(out.finish(false));
            }
            _ => bail!("not a config language: {}", language),
        };

        // Parse and extract from the same text: for YAML, lines nested deep
        // enough to crash tree-sitter-yaml are blanked first.
        let source: Cow<'_, str> = if language == Language::Yaml {
            let (text, blanked) = blank_deep_yaml_lines(source);
            if blanked > 0 {
                tracing::debug!(
                    "{file_path}: blanked {blanked} YAML lines nested deeper than \
                     {MAX_YAML_BLOCK_INDENT} columns before parsing (only levels 1-2 are indexed)"
                );
            }
            text
        } else {
            Cow::Borrowed(source)
        };
        let source = source.as_ref();
        if language == Language::Xml && xml_nesting_exceeds(source, MAX_XML_DEPTH) {
            tracing::debug!(
                "{file_path}: skipped — XML elements nested deeper than \
                 {MAX_XML_DEPTH} levels, which tree-sitter-xml cannot parse"
            );
            return Ok(out.finish(true));
        }

        self.ts_parser.set_language(&grammar)?;
        self.ts_parser
            .set_timeout_micros(self.config_timeout_micros);
        let tree = self.ts_parser.parse(source, None);
        // The timeout is for config files only; code parsing stays unbounded.
        self.ts_parser.set_timeout_micros(0);
        let Some(tree) = tree else {
            // A timed-out parser would resume this document on the next call.
            self.ts_parser.reset();
            tracing::debug!(
                "{file_path}: skipped — config parse timed out after {} ms (backstop)",
                self.config_timeout_micros / 1000
            );
            return Ok(out.finish(true));
        };
        let root = tree.root_node();
        if root.has_error() {
            tracing::debug!("{file_path}: config has syntax errors — symbols may be incomplete");
        }
        match language {
            Language::Json => json_config::extract(root, source, &mut out),
            Language::Yaml => yaml_config::extract(root, source, &mut out),
            Language::Toml => toml_config::extract(root, source, &mut out),
            Language::Xml => xml_config::extract(root, source, &mut out),
            _ => {}
        }
        Ok(out.finish(root.has_error()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::symbol::SymbolKind;

    fn nested_yaml(levels: usize) -> String {
        (0..levels)
            .map(|i| format!("{}k{i}:\n", "  ".repeat(i)))
            .collect::<String>()
            + &"  ".repeat(levels)
            + "leaf: 1\n"
    }

    fn names(r: &ConfigParseResult) -> Vec<&str> {
        r.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn yaml_nested_past_the_scanner_limit_keeps_its_shallow_symbols() {
        // 1000 levels would abort tree-sitter-yaml; deep lines are blanked.
        let mut parser = Parser::new();
        let r = parser
            .parse_config(&nested_yaml(1000), Language::Yaml, "f:1", "deep.yaml", 50)
            .unwrap();
        assert_eq!(names(&r), ["k0", "k0.k1"]);
        assert!(!r.has_errors);
        // Same shape on one line: `- - - …` is one blanked line.
        let inline = "- ".repeat(1000) + "x\n";
        let r = parser
            .parse_config(&inline, Language::Yaml, "f:1", "deep.yaml", 50)
            .unwrap();
        assert!(r.symbols.is_empty());
    }

    #[test]
    fn moderately_nested_yaml_still_parses() {
        let mut parser = Parser::new();
        let r = parser
            .parse_config(&nested_yaml(40), Language::Yaml, "f:1", "ok.yaml", 50)
            .unwrap();
        assert!(!r.has_errors);
        assert_eq!(names(&r), ["k0", "k0.k1"]);
        // Long dashes inside a scalar are not block indicators.
        let src = format!("a: |\n  {}\nb: 1\n", "-".repeat(500));
        let r = parser
            .parse_config(&src, Language::Yaml, "f:1", "ok.yaml", 50)
            .unwrap();
        assert!(!r.has_errors);
    }

    #[test]
    fn crd_with_a_deep_schema_keeps_top_symbols_and_true_lines() {
        let mut src = String::from(
            "apiVersion: apiextensions.k8s.io/v1\n\
             kind: CustomResourceDefinition\n\
             metadata:\n  name: widgets.example.com\n\
             spec:\n  group: example.com\n  versions:\n\
             \x20   openAPIV3Schema:\n",
        );
        // 40 levels of 2-space indentation under the schema (columns 6..84).
        for i in 0..40 {
            src.push_str(&format!("{}p{i}:\n", "  ".repeat(i + 3)));
        }
        src.push_str(&format!("{}type: string\n", "  ".repeat(43)));
        let scope_line = src.lines().count() + 1;
        src.push_str("  scope: Namespaced\nstatus:\n  acceptedNames: {}\n");

        let (blanked_text, blanked) = blank_deep_yaml_lines(&src);
        assert!(blanked > 0);
        assert_eq!(blanked_text.lines().count(), src.lines().count());

        let r = Parser::new()
            .parse_config(&src, Language::Yaml, "f:1", "crd.yaml", 50)
            .unwrap();
        assert!(!r.has_errors);
        let names = names(&r);
        for expected in [
            "apiVersion",
            "kind",
            "metadata.name",
            "spec.group",
            "spec.scope",
            "status",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        let line = |name: &str| {
            r.symbols
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .span
                .start_line
        };
        assert_eq!(line("kind"), 2);
        assert_eq!(line("spec.scope"), scope_line);
        assert_eq!(line("status"), scope_line + 1);
    }

    #[test]
    fn yaml_block_indent_counts_only_indicators_followed_by_space() {
        assert_eq!(yaml_block_indent("  - ? key: v"), 6);
        assert_eq!(yaml_block_indent("    -\r"), 5);
        assert_eq!(yaml_block_indent("  ------"), 2);
        assert_eq!(yaml_block_indent("  -x"), 2);
        assert_eq!(yaml_block_indent(""), 0);
    }

    #[test]
    fn yaml_with_lone_cr_line_breaks_is_guarded() {
        // tree-sitter-yaml treats `\r` as a newline; 300 levels would abort.
        let src: String = (0..300)
            .map(|i| format!("{}k{i}:\r", " ".repeat(i)))
            .collect();
        let (text, blanked) = blank_deep_yaml_lines(&src);
        assert_eq!(blanked, 300 - (MAX_YAML_BLOCK_INDENT + 1));
        assert_eq!(text.matches('\r').count(), 300);
        let r = Parser::new()
            .parse_config(&src, Language::Yaml, "f:1", "cr.yaml", 50)
            .unwrap();
        assert_eq!(names(&r), ["k0", "k0.k1"]);
        // tree-sitter numbers rows by `\n` only, so a CR-only file is one
        // row to it, as it is to `str::lines`.
        assert_eq!(r.symbols[1].span.start_line, 1);
    }

    #[test]
    fn yaml_guard_limit_on_the_two_entries_per_column_shape() {
        // `k:` / `-` pairs one column apart: map and sequence per column.
        let shape = |levels: usize| -> String {
            (0..levels)
                .map(|i| format!("{0}k:\n{0}-\n", " ".repeat(i)))
                .collect::<String>()
                + &" ".repeat(levels)
                + "x\n"
        };
        let mut parser = Parser::new();
        let at_limit = parser
            .parse_config(
                &shape(MAX_YAML_BLOCK_INDENT),
                Language::Yaml,
                "f:1",
                "a.yaml",
                50,
            )
            .unwrap();
        assert!(!at_limit.has_errors);
        assert!(!at_limit.symbols.is_empty());
        // 127 levels aborts tree-sitter-yaml unguarded; blanked, it parses.
        let over = parser
            .parse_config(&shape(127), Language::Yaml, "f:1", "a.yaml", 50)
            .unwrap();
        assert!(!over.symbols.is_empty());
        assert_eq!(over.symbols[0].name, "k");
    }

    #[test]
    fn xml_nesting_scan() {
        let nested = |depth: usize| "<a>".repeat(depth) + &"</a>".repeat(depth);
        assert!(!xml_nesting_exceeds(&nested(MAX_XML_DEPTH), MAX_XML_DEPTH));
        assert!(xml_nesting_exceeds(
            &nested(MAX_XML_DEPTH + 1),
            MAX_XML_DEPTH
        ));
        // Siblings, self-closing tags, comments, CDATA, PIs and DOCTYPE
        // do not nest.
        let wide = "<?xml version=\"1.0\"?><!DOCTYPE r [<!ENTITY e \"x\">]><r>".to_string()
            + &"<a/><b x=\"1>2\"></b><!-- <c> --><![CDATA[<d>]]>".repeat(1000)
            + "</r>";
        assert!(!xml_nesting_exceeds(&wide, 2));
        assert!(xml_nesting_exceeds("<a><b><c/></b></a>", 1));
    }

    #[test]
    fn deep_xml_is_guarded_quickly_and_the_parser_stays_usable() {
        let depth = 100_000;
        let xml = "<a>".repeat(depth) + &"</a>".repeat(depth);
        let mut parser = Parser::new();
        let started = std::time::Instant::now();
        let r = parser
            .parse_config(&xml, Language::Xml, "f:1", "deep.xml", 50)
            .unwrap();
        assert!(r.has_errors && r.symbols.is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        // A normal XML file still parses.
        let ok = parser
            .parse_config("<root><child/></root>", Language::Xml, "f:3", "ok.xml", 50)
            .unwrap();
        assert!(!ok.has_errors);
        assert!(!ok.symbols.is_empty());
    }

    #[test]
    fn timeout_flags_the_file_and_does_not_leak_into_code_parsing() {
        let mut parser = Parser::new();
        parser.config_timeout_micros = 1_000;
        let big: String = (0..50_000).map(|i| format!("\"k{i}\": {i},")).collect();
        let json = format!("{{{big}\"end\": 0}}");
        let r = parser
            .parse_config(&json, Language::Json, "f:1", "big.json", 50)
            .unwrap();
        assert!(r.has_errors && r.symbols.is_empty());
        assert_eq!(parser.ts_parser.timeout_micros(), 0);
        // The halted parse is not resumed by the next call.
        let code = parser
            .parse("pub fn after() {}\n", Language::Rust, "f:2", "a.rs")
            .unwrap();
        assert!(code.symbols.iter().any(|s| s.name == "after"));
        let small = parser
            .parse_config("{\"a\": 1}", Language::Json, "f:3", "a.json", 50)
            .unwrap();
        assert!(!small.has_errors);
        assert_eq!(small.symbols.len(), 1);
    }

    #[test]
    fn every_config_language_dispatches() {
        // Each grammar must load into the workspace tree-sitter (ABI check
        // happens at set_language), and each format must be wired.
        let mut parser = Parser::new();
        for lang in [
            Language::Json,
            Language::Yaml,
            Language::Toml,
            Language::Xml,
            Language::Hcl,
            Language::Csv,
            Language::Env,
            Language::Ini,
            Language::Dockerfile,
            Language::Makefile,
        ] {
            assert!(lang.is_config());
            parser
                .parse_config("", lang, "f:1", "x", 50)
                .unwrap_or_else(|e| panic!("{lang}: {e}"));
        }
        assert!(parser
            .parse_config("", Language::Rust, "f:1", "x", 50)
            .is_err());
    }

    #[test]
    fn first_sentence_truncates_on_char_boundary() {
        // 60 Cyrillic chars = 120 bytes; max_len of 119 lands inside a 'т'.
        let text = "т".repeat(60);
        let summary = first_sentence(&text, 119);
        assert!(summary.ends_with("..."));
        // Must not panic and must be valid UTF-8 (guaranteed by String).
        assert!(summary.len() <= 119 + 3);
    }

    #[test]
    fn module_doc_rust_inner_line_comments() {
        let src = "//! Context retrieval engine. Second sentence.\n//! More detail.\n\nuse std::fmt;\n\npub fn f() {}\n";
        assert_eq!(
            extract_module_doc(src, Language::Rust),
            Some("Context retrieval engine.".to_string())
        );
        // `///` on the first item is symbol doc, not module doc.
        let src = "/// Doc for f.\npub fn f() {}\n";
        assert_eq!(extract_module_doc(src, Language::Rust), None);
    }

    #[test]
    fn module_doc_python_docstring() {
        let src = "#!/usr/bin/env python\n\"\"\"Routing helpers for the API.\n\nDetails.\n\"\"\"\nimport os\n";
        assert_eq!(
            extract_module_doc(src, Language::Python),
            Some("Routing helpers for the API.".to_string())
        );
        let src = "'''Single-line doc.'''\nimport os\n";
        assert_eq!(
            extract_module_doc(src, Language::Python),
            Some("Single-line doc.".to_string())
        );
        assert_eq!(extract_module_doc("import os\n", Language::Python), None);
    }

    #[test]
    fn module_doc_go_package_comment() {
        let src = "// Package router dispatches HTTP requests.\n// More detail.\npackage router\n";
        assert_eq!(
            extract_module_doc(src, Language::Go),
            Some("Package router dispatches HTTP requests.".to_string())
        );
        // Blank line detaches the comment from the package clause.
        let src = "// Copyright notice.\n\npackage router\n";
        assert_eq!(extract_module_doc(src, Language::Go), None);
    }

    #[test]
    fn module_doc_header_block_detached_only() {
        // Header block followed by a blank line — file doc.
        let src = "/**\n * Database connection pool setup.\n */\n\nexport class Pool {}\n";
        assert_eq!(
            extract_module_doc(src, Language::TypeScript),
            Some("Database connection pool setup.".to_string())
        );
        // Header block followed by imports — file doc.
        let src = "/** Auth middleware. */\nimport express from 'express';\n";
        assert_eq!(
            extract_module_doc(src, Language::TypeScript),
            Some("Auth middleware.".to_string())
        );
        // Block directly above a declaration belongs to the declaration.
        let src = "/** Doc for Pool. */\nexport class Pool {}\n";
        assert_eq!(extract_module_doc(src, Language::TypeScript), None);
    }

    #[test]
    fn module_doc_license_headers_rejected() {
        // react-style file header
        let src = "/**\n * Copyright (c) Meta Platforms, Inc. and affiliates.\n *\n * This source code is licensed under the MIT license.\n */\n\nexport function f() {}\n";
        assert_eq!(extract_module_doc(src, Language::JavaScript), None);
        // express-style /*! banner
        let src = "/*!\n * express\n * Copyright(c) 2009-2013 TJ Holowaychuk\n * MIT Licensed\n */\n\nmodule.exports = {};\n";
        assert_eq!(extract_module_doc(src, Language::JavaScript), None);
        // public-domain C header
        let src = "/**\n * hdr_histogram.h\n * Written by Michael Barker and released to the public domain.\n */\n\n#include <stdint.h>\n";
        assert_eq!(extract_module_doc(src, Language::C), None);
        // pragma-only header (react-style @flow)
        let src = "/**\n * @flow\n */\n\nexport function f() {}\n";
        assert_eq!(extract_module_doc(src, Language::JavaScript), None);
    }

    #[test]
    fn module_doc_rulers_and_heading_marks_stripped() {
        // RST-style underline ruler inside a docstring is dropped.
        let src = "\"\"\"Tagged JSON\n~~~~~~~~~~~\n\nA compact representation for serialization.\n\"\"\"\nimport os\n";
        assert_eq!(
            extract_module_doc(src, Language::Python),
            Some("Tagged JSON A compact representation for serialization.".to_string())
        );
        // Leading markdown heading marks are stripped.
        let src = "//! # Cargo test macro.\n\npub fn f() {}\n";
        assert_eq!(
            extract_module_doc(src, Language::Rust),
            Some("Cargo test macro.".to_string())
        );
    }

    #[test]
    fn module_doc_populated_by_parse() {
        let mut parser = Parser::new();
        let src = "//! Shared domain types.\n\npub struct Budget;\n";
        let result = parser.parse(src, Language::Rust, "f:1", "lib.rs").unwrap();
        assert_eq!(result.module_doc, Some("Shared domain types.".to_string()));
    }

    #[test]
    fn rust_symbols_and_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
use std::path::Path;

pub fn hello() {
    println!("hi");
}

struct Foo {
    x: i32,
}
"#,
                Language::Rust,
                "f:test",
                "test.rs",
            )
            .unwrap();
        assert!(result.symbols.iter().any(|s| s.name == "hello"));
        assert!(result.symbols.iter().any(|s| s.name == "Foo"));
        assert!(!result.imports.is_empty());
    }

    #[test]
    fn java_symbols_and_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import java.util.List;
import java.util.Map;

public class UserService {
    private final String name;

    public UserService(String name) {
        this.name = name;
    }

    public List<String> getItems() {
        return List.of();
    }

    public interface Callback {
        void onResult(String result);
    }

    public enum Status {
        ACTIVE, INACTIVE
    }
}
"#,
                Language::Java,
                "f:test",
                "Test.java",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "got: {:?}", names);
        assert!(names.contains(&"getItems"), "got: {:?}", names);
        assert!(names.contains(&"Callback"), "got: {:?}", names);
        assert!(names.contains(&"Status"), "got: {:?}", names);
        assert_eq!(result.imports.len(), 2);
    }

    #[test]
    fn csharp_symbols_and_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
using System;
using System.Collections.Generic;

namespace MyApp {
    public class UserService {
        private readonly string _name;

        public UserService(string name) {
            _name = name;
        }

        public List<string> GetItems() {
            return new List<string>();
        }

        public interface ICallback {
            void OnResult(string result);
        }
    }
}
"#,
                Language::CSharp,
                "f:test",
                "Test.cs",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "got: {:?}", names);
        assert!(names.contains(&"GetItems"), "got: {:?}", names);
        assert!(names.contains(&"ICallback"), "got: {:?}", names);
        assert_eq!(result.imports.len(), 2);
    }

    #[test]
    fn kotlin_symbols_and_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import java.util.List
import kotlin.collections.Map

class UserService(private val name: String) {
    fun getItems(): List<String> {
        return listOf()
    }

    interface Callback {
        fun onResult(result: String)
    }

    enum class Status {
        ACTIVE, INACTIVE
    }
}

fun topLevel(): String = "hello"
"#,
                Language::Kotlin,
                "f:test",
                "Test.kt",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "got: {:?}", names);
        assert!(names.contains(&"getItems"), "got: {:?}", names);
        assert!(names.contains(&"topLevel"), "got: {:?}", names);
        assert_eq!(result.imports.len(), 2);
    }

    // ── Java advanced ──

    #[test]
    fn java_annotations_and_generics() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import java.util.List;
import static java.util.Collections.emptyList;

@Service
public class OrderService<T extends Comparable<T>> {
    @Autowired
    private final OrderRepository repo;

    @Override
    public List<T> findAll() {
        return emptyList();
    }

    public static <U> U identity(U value) {
        return value;
    }
}
"#,
                Language::Java,
                "f:test",
                "OrderService.java",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"OrderService"),
            "missing OrderService: {:?}",
            names
        );
        assert!(names.contains(&"findAll"), "missing findAll: {:?}", names);
        assert!(names.contains(&"identity"), "missing identity: {:?}", names);
        assert!(names.contains(&"repo"), "missing field repo: {:?}", names);
        // static import
        assert!(result
            .imports
            .iter()
            .any(|i| i.path.contains("Collections")));
    }

    #[test]
    fn java_abstract_and_inner_classes() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
public abstract class BaseHandler {
    public abstract void handle(String input);

    protected void log(String msg) {
        System.out.println(msg);
    }

    public static class Config {
        private int timeout;
    }

    private enum Priority {
        LOW, MEDIUM, HIGH
    }
}
"#,
                Language::Java,
                "f:test",
                "BaseHandler.java",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"BaseHandler"), "got: {:?}", names);
        assert!(names.contains(&"handle"), "got: {:?}", names);
        assert!(names.contains(&"log"), "got: {:?}", names);
        assert!(
            names.contains(&"Config"),
            "missing inner class: {:?}",
            names
        );
        assert!(
            names.contains(&"Priority"),
            "missing inner enum: {:?}",
            names
        );

        // Check parent relationships
        let config = result.symbols.iter().find(|s| s.name == "Config").unwrap();
        assert!(
            config.parent_symbol_id.is_some(),
            "Config should have parent"
        );
    }

    #[test]
    fn java_interface_with_default_methods() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
public interface Repository<T, ID> {
    T findById(ID id);
    List<T> findAll();

    default void delete(ID id) {
        // default impl
    }
}
"#,
                Language::Java,
                "f:test",
                "Repository.java",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Repository"), "got: {:?}", names);
        assert!(names.contains(&"findById"), "got: {:?}", names);
        assert!(names.contains(&"findAll"), "got: {:?}", names);
        assert!(names.contains(&"delete"), "got: {:?}", names);

        let repo = result
            .symbols
            .iter()
            .find(|s| s.name == "Repository")
            .unwrap();
        assert_eq!(repo.kind, SymbolKind::Interface);
        assert!(repo.exported);
    }

    // ── C# advanced ──

    #[test]
    fn csharp_record_and_file_scoped_namespace() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
using System;

namespace MyApp;

public record UserDto(string Name, int Age);

public class UserMapper {
    public UserDto ToDto(User user) {
        return new UserDto(user.Name, user.Age);
    }
}
"#,
                Language::CSharp,
                "f:test",
                "UserDto.cs",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserDto"), "missing record: {:?}", names);
        assert!(names.contains(&"UserMapper"), "missing class: {:?}", names);
        assert!(names.contains(&"ToDto"), "missing method: {:?}", names);
        assert_eq!(result.imports.len(), 1);
    }

    #[test]
    fn csharp_struct_enum_and_properties() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
using System.Collections.Generic;

namespace MyApp {
    public struct Point {
        public int X { get; set; }
        public int Y { get; set; }
    }

    public enum Color {
        Red,
        Green,
        Blue
    }

    public class Canvas {
        public List<Point> Points { get; } = new();
        public Color Background { get; set; }

        public void Draw() { }
    }
}
"#,
                Language::CSharp,
                "f:test",
                "Canvas.cs",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Point"), "missing struct: {:?}", names);
        assert!(names.contains(&"Color"), "missing enum: {:?}", names);
        assert!(names.contains(&"Canvas"), "missing class: {:?}", names);
        assert!(names.contains(&"Draw"), "missing method: {:?}", names);
        assert!(names.contains(&"X"), "missing property X: {:?}", names);

        let point = result.symbols.iter().find(|s| s.name == "Point").unwrap();
        assert_eq!(point.kind, SymbolKind::Struct);
    }

    #[test]
    fn csharp_async_and_generics() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
using System.Threading.Tasks;

namespace MyApp {
    public interface IRepository<T> where T : class {
        Task<T?> FindByIdAsync(int id);
        Task<IEnumerable<T>> GetAllAsync();
    }

    public class UserRepository : IRepository<User> {
        public async Task<User?> FindByIdAsync(int id) {
            return await _db.FindAsync(id);
        }

        public async Task<IEnumerable<User>> GetAllAsync() {
            return await _db.ToListAsync();
        }
    }
}
"#,
                Language::CSharp,
                "f:test",
                "UserRepository.cs",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"IRepository"),
            "missing interface: {:?}",
            names
        );
        assert!(
            names.contains(&"UserRepository"),
            "missing class: {:?}",
            names
        );
        assert!(
            names.contains(&"FindByIdAsync"),
            "missing async method: {:?}",
            names
        );

        let iface = result
            .symbols
            .iter()
            .find(|s| s.name == "IRepository")
            .unwrap();
        assert_eq!(iface.kind, SymbolKind::Interface);
    }

    // ── Kotlin advanced ──

    #[test]
    fn kotlin_data_class_and_companion() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import java.util.UUID

data class User(val name: String, val age: Int) {
    companion object {
        fun create(name: String): User = User(name, 0)
    }
}

sealed class Result<out T> {
    data class Success<T>(val data: T) : Result<T>()
    data class Error(val message: String) : Result<Nothing>()
}
"#,
                Language::Kotlin,
                "f:test",
                "User.kt",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"User"), "missing data class: {:?}", names);
        assert!(
            names.contains(&"Result"),
            "missing sealed class: {:?}",
            names
        );
        assert!(
            names.contains(&"Success"),
            "missing inner data class: {:?}",
            names
        );
        assert!(
            names.contains(&"Error"),
            "missing inner data class: {:?}",
            names
        );
        assert_eq!(result.imports.len(), 1);
    }

    #[test]
    fn kotlin_object_and_extension_functions() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import io.ktor.server.application.*

object AppConfig {
    val defaultPort: Int = 8080
    fun load(): AppConfig = this
}

fun String.toSlug(): String = this.lowercase().replace(" ", "-")

suspend fun fetchData(url: String): ByteArray {
    return byteArrayOf()
}

internal class HttpClient {
    suspend fun get(url: String): String = ""
}
"#,
                Language::Kotlin,
                "f:test",
                "Utils.kt",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AppConfig"), "missing object: {:?}", names);
        assert!(
            names.contains(&"load"),
            "missing object method: {:?}",
            names
        );
        assert!(
            names.contains(&"toSlug"),
            "missing extension fn: {:?}",
            names
        );
        assert!(
            names.contains(&"fetchData"),
            "missing suspend fn: {:?}",
            names
        );
        assert!(
            names.contains(&"HttpClient"),
            "missing internal class: {:?}",
            names
        );

        let client = result
            .symbols
            .iter()
            .find(|s| s.name == "HttpClient")
            .unwrap();
        assert!(!client.exported, "internal class should not be exported");
    }

    #[test]
    fn kotlin_interface_and_enum_class() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
interface Repository<T> {
    fun findById(id: Long): T?
    fun findAll(): List<T>
    fun save(entity: T): T
}

enum class HttpMethod {
    GET, POST, PUT, DELETE;

    fun isIdempotent(): Boolean = this != POST
}
"#,
                Language::Kotlin,
                "f:test",
                "Repository.kt",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"Repository"),
            "missing interface: {:?}",
            names
        );
        assert!(names.contains(&"findById"), "missing method: {:?}", names);
        assert!(
            names.contains(&"HttpMethod"),
            "missing enum class: {:?}",
            names
        );

        let repo = result
            .symbols
            .iter()
            .find(|s| s.name == "Repository")
            .unwrap();
        assert_eq!(repo.kind, SymbolKind::Interface);
        let http = result
            .symbols
            .iter()
            .find(|s| s.name == "HttpMethod")
            .unwrap();
        assert_eq!(http.kind, SymbolKind::Enum);
    }

    // ── Visibility tests ──

    #[test]
    fn java_visibility_detection() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
public class Api {
    public void publicMethod() {}
    private void privateMethod() {}
    protected void protectedMethod() {}
    void packageMethod() {}
}
"#,
                Language::Java,
                "f:test",
                "Api.java",
            )
            .unwrap();
        let pub_m = result
            .symbols
            .iter()
            .find(|s| s.name == "publicMethod")
            .unwrap();
        let priv_m = result
            .symbols
            .iter()
            .find(|s| s.name == "privateMethod")
            .unwrap();
        let prot_m = result
            .symbols
            .iter()
            .find(|s| s.name == "protectedMethod")
            .unwrap();
        let pkg_m = result
            .symbols
            .iter()
            .find(|s| s.name == "packageMethod")
            .unwrap();
        assert!(pub_m.exported);
        assert!(!priv_m.exported);
        assert!(!prot_m.exported);
        assert!(!pkg_m.exported); // package-private not exported
    }

    #[test]
    fn csharp_visibility_detection() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
namespace Test {
    public class Api {
        public void PublicMethod() {}
        private void PrivateMethod() {}
        internal void InternalMethod() {}
    }
}
"#,
                Language::CSharp,
                "f:test",
                "Api.cs",
            )
            .unwrap();
        let pub_m = result
            .symbols
            .iter()
            .find(|s| s.name == "PublicMethod")
            .unwrap();
        let priv_m = result
            .symbols
            .iter()
            .find(|s| s.name == "PrivateMethod")
            .unwrap();
        let int_m = result
            .symbols
            .iter()
            .find(|s| s.name == "InternalMethod")
            .unwrap();
        assert!(pub_m.exported);
        assert!(!priv_m.exported);
        assert!(!int_m.exported);
    }

    #[test]
    fn kotlin_visibility_detection() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
class Api {
    fun defaultMethod() {}
    private fun privateMethod() {}
    internal fun internalMethod() {}
    protected fun protectedMethod() {}
}
"#,
                Language::Kotlin,
                "f:test",
                "Api.kt",
            )
            .unwrap();
        let def_m = result
            .symbols
            .iter()
            .find(|s| s.name == "defaultMethod")
            .unwrap();
        let priv_m = result
            .symbols
            .iter()
            .find(|s| s.name == "privateMethod")
            .unwrap();
        let int_m = result
            .symbols
            .iter()
            .find(|s| s.name == "internalMethod")
            .unwrap();
        assert!(def_m.exported, "Kotlin default is public");
        assert!(!priv_m.exported);
        assert!(!int_m.exported);
    }

    // ── Import extraction edge cases ──

    #[test]
    fn java_static_and_wildcard_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import java.util.*;
import static org.junit.Assert.assertEquals;
import com.example.model.User;

public class Test {}
"#,
                Language::Java,
                "f:test",
                "Test.java",
            )
            .unwrap();
        assert_eq!(result.imports.len(), 3);
        let wildcard = result
            .imports
            .iter()
            .find(|i| i.path.contains("java.util"))
            .unwrap();
        assert!(
            wildcard.names.is_empty(),
            "wildcard import should have no names"
        );
        let static_imp = result
            .imports
            .iter()
            .find(|i| i.path.contains("assertEquals"))
            .unwrap();
        assert!(static_imp.names.contains(&"assertEquals".to_string()));
    }

    #[test]
    fn csharp_using_variations() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
using System;
using System.Collections.Generic;
using Alias = System.Text.StringBuilder;
using static System.Math;

namespace Test {
    public class Foo {}
}
"#,
                Language::CSharp,
                "f:test",
                "Foo.cs",
            )
            .unwrap();
        // Alias should be skipped (contains '=')
        assert_eq!(
            result.imports.len(),
            3,
            "got: {:?}",
            result.imports.iter().map(|i| &i.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn kotlin_star_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import io.ktor.server.application.*
import io.ktor.server.routing.Routing
import kotlinx.coroutines.flow.Flow

class App
"#,
                Language::Kotlin,
                "f:test",
                "App.kt",
            )
            .unwrap();
        assert_eq!(
            result.imports.len(),
            3,
            "got: {:?}",
            result.imports.iter().map(|i| &i.path).collect::<Vec<_>>()
        );
    }

    /// Shared assertion: parse `source`, expect callee names with given is_method flags.
    fn assert_calls(source: &str, language: Language, ext: &str, expected: &[(&str, bool)]) {
        let mut parser = Parser::new();
        let result = parser
            .parse(source, language, "f:test", &format!("test.{ext}"))
            .unwrap();
        for (name, is_method) in expected {
            assert!(
                result
                    .calls
                    .iter()
                    .any(|c| c.callee == *name && c.is_method == *is_method),
                "expected call {name} (is_method={is_method}) in {language:?}, got: {:?}",
                result
                    .calls
                    .iter()
                    .map(|c| (c.callee.as_str(), c.is_method))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn typescript_calls() {
        assert_calls(
            "function run() {\n  helper();\n  obj.method();\n}\nconst arrow = () => { helper(); };\n",
            Language::TypeScript,
            "ts",
            &[("helper", false), ("method", true)],
        );
    }

    #[test]
    fn javascript_calls() {
        assert_calls(
            "function run() {\n  helper();\n  thing.doIt();\n}\n",
            Language::JavaScript,
            "js",
            &[("helper", false), ("doIt", true)],
        );
    }

    #[test]
    fn python_calls() {
        assert_calls(
            "def run():\n    helper()\n    obj.method()\n",
            Language::Python,
            "py",
            &[("helper", false), ("method", true)],
        );
    }

    #[test]
    fn go_calls() {
        assert_calls(
            "package main\nfunc run() {\n\thelper()\n\tpkg.Func()\n}\nfunc helper() {}\n",
            Language::Go,
            "go",
            &[("helper", false), ("Func", true)],
        );
    }

    #[test]
    fn java_calls() {
        assert_calls(
            "class A {\n  void run() {\n    helper();\n    obj.method();\n  }\n}\n",
            Language::Java,
            "java",
            &[("helper", false), ("method", true)],
        );
    }

    #[test]
    fn csharp_calls() {
        assert_calls(
            "class A {\n  void Run() {\n    Helper();\n    obj.Method();\n  }\n}\n",
            Language::CSharp,
            "cs",
            &[("Helper", false), ("Method", true)],
        );
    }

    #[test]
    fn kotlin_calls() {
        assert_calls(
            "fun run() {\n    helper()\n    obj.method()\n}\n",
            Language::Kotlin,
            "kt",
            &[("helper", false), ("method", true)],
        );
    }

    #[test]
    fn c_calls() {
        assert_calls(
            "void run(void) {\n    helper();\n    s->op();\n}\n",
            Language::C,
            "c",
            &[("helper", false), ("op", true)],
        );
    }

    #[test]
    fn cpp_calls() {
        assert_calls(
            "void run() {\n    helper();\n    obj.method();\n    ns::scoped();\n}\n",
            Language::Cpp,
            "cpp",
            &[("helper", false), ("method", true), ("scoped", false)],
        );
    }

    #[test]
    fn calls_outside_functions_are_skipped_across_languages() {
        // Top-level calls have no caller symbol to attribute to.
        let mut parser = Parser::new();
        let result = parser
            .parse("top_level()\n", Language::Python, "f:test", "test.py")
            .unwrap();
        assert!(result.calls.is_empty(), "got: {:?}", result.calls);
    }
}
