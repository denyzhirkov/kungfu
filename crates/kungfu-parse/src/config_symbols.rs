//! Shared symbol builder for config files — every `*_config.rs` module emits
//! its symbols through [`ConfigSymbols::push`], so ids, naming, depth and the
//! per-file cap behave the same for every format.
//!
//! Contract for a format module:
//! - Walk the file in document order and call `push` for a node before any of
//!   its children (pre-order). Over the cap, level-1 nodes are kept first and
//!   level-2 nodes fill the rest in document order (see [`ConfigSymbols`]), so
//!   this order decides which attributes a trimmed file keeps.
//! - Level-1 nodes pass `parent: None`; a child passes the handle its parent's
//!   `push` returned. Past [`MAX_DEPTH`] `push` records nothing and returns
//!   `None` — stop descending there.
//! - `kind`: [`SymbolKind::ConfigBlock`] for a node with a body (object or
//!   mapping value, TOML table, HCL block, Dockerfile stage, Makefile target,
//!   XML element), [`SymbolKind::ConfigKey`] for everything else.
//! - Pass the bare key and its value separately, as a [`ConfigValue`]; never
//!   pre-format a signature. Source text goes in only as
//!   [`ConfigValue::Scalar`]; a non-scalar is described by its size
//!   ([`ConfigValue::Block`] → `{3 keys}`, [`ConfigValue::List`] →
//!   `[12 items]`), which carries no source text by construction.
//! - `push` renders `name = value` and is the one place secrets are redacted:
//!   a scalar becomes `<redacted>` when any segment of its dotted name names a
//!   secret (`password`, `token`, `secrets.github`, anything ending in `key`,
//!   …), or the value looks like one (`AKIA…`, `ghp_…`, `-----BEGIN`, a long
//!   base64/hex run, a URL with `user:password@`). `true`/`false`/`null` are
//!   shown as is. This is a best-effort denylist, not a guarantee: a secret
//!   used as a key, or one matching no pattern, still reaches the index.
//! - Values are never indexed as symbols: arrays/lists are one `ConfigKey`, not
//!   one symbol per element.
//! - When the module knows it could not read the whole file (an unterminated
//!   block, heredoc or `define` swallowed the rest, an oversized header, a
//!   guard hit), call [`ConfigSymbols::mark_incomplete`]: the file is then
//!   flagged "parsed with errors — symbols may be incomplete", as for a
//!   tree-sitter parse with syntax errors.
//! - Ids are `s:{file_id}:{start_line}:{name}` like code symbols; a repeat
//!   within the file (duplicate key, `"x.y"` beside `x: {y}` on one line) gets
//!   a `#2`, `#3`… suffix in document order, so ids stay unique and stable.

use crate::ConfigParseResult;
use kungfu_types::file::Language;
use kungfu_types::symbol::{Span, Symbol, SymbolKind};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// Levels of keys indexed: level-1 keys and their direct children.
pub const MAX_DEPTH: usize = 2;

/// Signatures longer than this many characters are cut and end with `…`.
pub const MAX_SIGNATURE_CHARS: usize = 80;

/// Keys longer than this many bytes are refused (see [`ConfigSymbols::push`]):
/// no real key is this long, and every child's name and id would repeat it.
pub const MAX_KEY_BYTES: usize = 256;

/// Shown instead of a scalar that looks like a secret.
pub const REDACTED: &str = "<redacted>";

/// Handle to a pushed node, passed as `parent` when pushing its children.
#[derive(Debug, Clone)]
pub struct ConfigParent {
    /// Dotted name; empty for a node that can never be stored.
    name: String,
    depth: usize,
    /// Index of the buffered level-1 candidate, `None` when count-only.
    slot: Option<usize>,
}

/// A node's value as `push` should show it in the signature.
pub enum ConfigValue<'v> {
    /// Nothing to show (a section header, a CSV column): the signature is the
    /// dotted name alone.
    None,
    /// A scalar as written in the source, quotes included. Bounded, and
    /// redacted when any segment of the dotted name, or the value, looks like
    /// a secret.
    Scalar(&'v str),
    /// A body (object, table, block) described by its size: rendered
    /// `{3 keys}` for `Block { count: 3, noun: "key" }`.
    Block { count: usize, noun: &'static str },
    /// A list described by its size: rendered `[2 items]` for
    /// `List { count: 2, noun: "item" }`.
    List { count: usize, noun: &'static str },
}

/// A node that may be stored, kept until `finish` knows the selection.
struct Candidate {
    /// Document order among all pushed nodes.
    seq: usize,
    /// `s:{file_id}:{line}:{name}` before the uniqueness suffix.
    base_id: String,
    name: String,
    kind: SymbolKind,
    signature: String,
    span: Span,
    /// Level-1 parent's index in `level1` (level-2 candidates only).
    parent: Option<usize>,
}

/// Collects config symbols for one file. Created by `Parser::parse_config`.
///
/// Cap policy, when a file has more nodes than `limit`: level-1 nodes first
/// — all of them in document order, up to `limit` — then the remaining
/// budget goes to level-2 nodes in document order (all of which have a
/// stored parent). So every top-level block of a long file stays findable,
/// at the expense of attributes. The result is in document order. Under the
/// limit everything is stored, exactly as pushed.
pub struct ConfigSymbols<'a> {
    language: Language,
    file_id: &'a str,
    file_path: &'a str,
    limit: usize,
    /// At most `limit` level-1 candidates: all are stored.
    level1: Vec<Candidate>,
    /// At most `limit` level-2 candidates, each under a buffered level-1
    /// node. `finish` stores the first `limit - level1.len()` of them, so a
    /// level-2 node past the first `limit` can never be selected.
    level2: Vec<Candidate>,
    total: usize,
    incomplete: bool,
}

impl<'a> ConfigSymbols<'a> {
    pub(crate) fn new(
        language: Language,
        file_id: &'a str,
        file_path: &'a str,
        limit: usize,
    ) -> Self {
        Self {
            language,
            file_id,
            file_path,
            limit,
            level1: Vec::new(),
            level2: Vec::new(),
            total: 0,
            incomplete: false,
        }
    }

    /// The module could not read the whole file, so the symbols may be
    /// incomplete; the result is flagged like a parse with syntax errors.
    pub fn mark_incomplete(&mut self) {
        self.incomplete = true;
    }

    /// Project-relative path of the file — e.g. to tell `.tsv` from `.csv`.
    pub fn file_path(&self) -> &str {
        self.file_path
    }

    /// Record one config node.
    ///
    /// The symbol is named by the dotted key path (`scripts.build`), gets a
    /// file-unique id (see the module doc) and the parent's id as
    /// `parent_symbol_id`. Its signature is `name = value` (or `name` for
    /// [`ConfigValue::None`]), secrets redacted, one line, at most
    /// [`MAX_SIGNATURE_CHARS`]. Every node up to [`MAX_DEPTH`] counts toward
    /// the file's total; which are stored follows the cap policy on
    /// [`ConfigSymbols`].
    ///
    /// Memory does not grow with the file: at most `limit` level-1 and
    /// `limit` level-2 candidates are buffered, each bounded by
    /// [`MAX_KEY_BYTES`]; any other node is only counted.
    ///
    /// Returns the handle for children, or `None` when the node is not indexed:
    /// the parent is already at [`MAX_DEPTH`], `key` is empty, or `key` is
    /// longer than [`MAX_KEY_BYTES`]. A too-long key is refused outright —
    /// neither stored nor counted — and marks the file incomplete.
    pub fn push(
        &mut self,
        parent: Option<&ConfigParent>,
        key: &str,
        kind: SymbolKind,
        value: ConfigValue,
        span: Span,
    ) -> Option<ConfigParent> {
        let depth = parent.map_or(1, |p| p.depth + 1);
        if depth > MAX_DEPTH || key.is_empty() {
            return None;
        }
        if key.len() > MAX_KEY_BYTES {
            self.incomplete = true;
            return None;
        }
        let seq = self.total;
        self.total += 1;
        let count_only = ConfigParent {
            name: String::new(),
            depth,
            slot: None,
        };
        let parent_slot = match parent {
            None if self.level1.len() < self.limit => None,
            Some(p) if p.slot.is_some() && self.level2.len() < self.limit => p.slot,
            _ => return Some(count_only),
        };
        let name = match parent {
            Some(p) => format!("{}.{}", p.name, key),
            None => key.to_string(),
        };
        let signature = match value {
            ConfigValue::None => name.clone(),
            ConfigValue::Scalar(text) if is_secret(&name, text) => {
                format!("{name} = {REDACTED}")
            }
            ConfigValue::Scalar(text) => format!("{name} = {}", short_value(text)),
            ConfigValue::Block { count, noun } => {
                format!("{name} = {{{}}}", counted(count, noun))
            }
            ConfigValue::List { count, noun } => format!("{name} = [{}]", counted(count, noun)),
        };
        let candidate = Candidate {
            seq,
            base_id: format!("s:{}:{}:{}", self.file_id, span.start_line, name),
            name: name.clone(),
            kind,
            signature: truncate_signature(&signature),
            span,
            parent: parent_slot,
        };
        if parent.is_none() {
            self.level1.push(candidate);
            Some(ConfigParent {
                name,
                depth,
                slot: Some(self.level1.len() - 1),
            })
        } else {
            self.level2.push(candidate);
            Some(count_only)
        }
    }

    /// `tree_has_error`: the grammar recovered from syntax errors (always
    /// `false` for line-based formats). Either that or
    /// [`mark_incomplete`](Self::mark_incomplete) flags the result.
    ///
    /// Ids are made unique here, over the stored nodes only and in document
    /// order, so a dropped node never takes a suffix: the same content always
    /// yields the same ids.
    pub(crate) fn finish(self, tree_has_error: bool) -> ConfigParseResult {
        let budget = self.limit.saturating_sub(self.level1.len());
        let level1_count = self.level1.len();
        let mut stored: Vec<Candidate> = self.level1;
        stored.extend(self.level2.into_iter().take(budget));
        // Level-1 candidates keep their indices 0..level1_count, so a level-2
        // node's `parent` slot indexes `stored` directly. Ids and output
        // follow document order (`seq`).
        let mut ids = UniqueIds::default();
        let mut order: Vec<usize> = (0..stored.len()).collect();
        order.sort_by_key(|&i| stored[i].seq);
        let mut assigned: Vec<String> = vec![String::new(); stored.len()];
        for &i in &order {
            assigned[i] = ids.unique(&stored[i].base_id);
        }
        let mut symbols = Vec::with_capacity(stored.len());
        for &i in &order {
            let c = &stored[i];
            debug_assert!(c.parent.is_none_or(|p| p < level1_count));
            symbols.push(Symbol {
                id: assigned[i].clone(),
                file_id: self.file_id.to_string(),
                name: c.name.clone(),
                kind: c.kind,
                language: self.language.to_string(),
                path: self.file_path.to_string(),
                signature: Some(c.signature.clone()),
                span: c.span.clone(),
                parent_symbol_id: c.parent.map(|p| assigned[p].clone()),
                exported: false,
                visibility: None,
                doc_summary: None,
            });
        }
        ConfigParseResult {
            symbols,
            total: self.total,
            has_errors: tree_has_error || self.incomplete,
        }
    }
}

/// File-unique ids: `base`, or the lowest free `base#n` (n ≥ 2).
#[derive(Default)]
struct UniqueIds {
    used: HashSet<String>,
    /// Per repeated base, the lowest suffix not yet tried. Every suffix below
    /// it is taken, so resuming there yields the same ids as scanning from
    /// `#2`, in amortised O(1).
    next_suffix: HashMap<String, usize>,
}

impl UniqueIds {
    fn unique(&mut self, base: &str) -> String {
        if self.used.insert(base.to_string()) {
            return base.to_string();
        }
        let n = self.next_suffix.entry(base.to_string()).or_insert(2);
        loop {
            let id = format!("{base}#{n}");
            *n += 1;
            if self.used.insert(id.clone()) {
                return id;
            }
        }
    }
}

/// Key fragments that name a secret, matched against the key lowercased with
/// separators removed (`api_key`, `apiKey`, `API-KEY` → `apikey`).
const SECRET_KEY_PARTS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "apikey",
    "privatekey",
    "accesskey",
    "credential",
    "creds",
    "dsn",
    "connectionstring",
    "webhook",
];

/// Prefixes of well-known credential formats.
const SECRET_VALUE_PREFIXES: &[&str] = &["AKIA", "ghp_", "github_pat_", "sk-", "xox", "-----BEGIN"];

fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// `name` is the full dotted name: a secret parent (`secrets.github`,
/// `password.value`) makes every child secret.
fn is_secret(name: &str, value: &str) -> bool {
    let bare = unquote(value);
    if matches!(bare, "true" | "false" | "null" | "~") {
        return false;
    }
    name.split('.').any(is_secret_key) || looks_like_secret_value(bare)
}

fn is_secret_key(key: &str) -> bool {
    let key: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    // `auth`, `authToken`, `authorization` — but not package.json's `author`.
    let auth = key.contains("authoriz") || key.replace("author", "").contains("auth");
    // `encryptionKey`, `signingKey`, `masterKey`, `admin_pass` — over-redacts
    // `sortKey`, `bypass`.
    auth || key.ends_with("key")
        || key.ends_with("pass")
        || SECRET_KEY_PARTS.iter().any(|part| key.contains(part))
}

fn unquote(value: &str) -> &str {
    value.trim().trim_matches(|c| c == '"' || c == '\'')
}

/// `scheme://user:password@host` anywhere in the value.
/// The userinfo is everything before the last `@` ahead of the path, so an
/// unencoded `#` or `?` inside the password does not hide it.
fn has_url_password(value: &str) -> bool {
    value.match_indices("://").any(|(at, sep)| {
        let rest = &value[at + sep.len()..];
        let authority = rest
            .split(|c: char| matches!(c, '/' | '"' | '\'') || c.is_whitespace())
            .next()
            .unwrap_or("");
        authority.rsplit_once('@').is_some_and(|(userinfo, _)| {
            userinfo
                .split_once(':')
                .is_some_and(|(_, pass)| !pass.is_empty())
        })
    })
}

/// Credential parameter names inside a value: `?password=s3cret`,
/// `{ password = "x" }`, `Pwd=x;`, `token: x`, `--password x`.
const SECRET_PARAMS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "apikey",
    "api-key",
    "api_key",
    "access_key",
    "auth",
    "authorization",
];

fn has_secret_param(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    SECRET_PARAMS.iter().any(|param| {
        lower.match_indices(param).any(|(at, _)| {
            let rest = &lower[at + param.len()..];
            param_value_follows(rest)
                || (is_flag_tail(&lower, at, rest) && flag_value_follows(rest))
        })
    }) || has_auth_scheme(value, &lower)
}

/// After a parameter name: `=` or `:` with optional spaces around it, then a
/// non-empty value, possibly quoted.
fn param_value_follows(rest: &str) -> bool {
    let trimmed = rest.trim_start_matches([' ', '\t']);
    let Some(after) = trimmed.strip_prefix(['=', ':']) else {
        return false;
    };
    non_empty_value(after.trim_start_matches([' ', '\t']))
}

/// The name at `at` is the last `.`/`-` component of a `-`-led flag:
/// `--password`, `--db-password`, `--spring.datasource.password`. Short
/// flags (`-p`) never match: no secret name is one letter.
fn is_flag_tail(lower: &str, at: usize, rest: &str) -> bool {
    let flag_char = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_');
    let bytes = lower.as_bytes();
    if rest.bytes().next().is_some_and(flag_char) || at == 0 {
        return false;
    }
    if !matches!(bytes[at - 1], b'-' | b'.') {
        return false;
    }
    let start = bytes[..at]
        .iter()
        .rposition(|&b| !flag_char(b))
        .map_or(0, |i| i + 1);
    bytes[start] == b'-'
}

/// A flag's value: after a run of whitespace, quotes and commas (exec form
/// `["--token","abc"]` as well as `--token abc`), a value that is not the
/// next flag.
fn flag_value_follows(rest: &str) -> bool {
    let after = rest.trim_start_matches([' ', '\t', '"', '\'', ',']);
    after.len() < rest.len() && !after.starts_with('-') && non_empty_value(after)
}

/// `Bearer <token>` / `Basic <token>`: the scheme as a word, whitespace, then
/// a run of at least 8 token characters that does not read like a word — it
/// has a digit, a symbol, or a capital after its first letter — so prose such
/// as "Basic settings" stays readable.
fn has_auth_scheme(value: &str, lower: &str) -> bool {
    ["bearer", "basic"].iter().any(|scheme| {
        lower.match_indices(scheme).any(|(at, _)| {
            if lower[..at]
                .bytes()
                .next_back()
                .is_some_and(|b| b.is_ascii_alphanumeric())
            {
                return false;
            }
            let rest = &value[at + scheme.len()..];
            let token_start = rest.trim_start_matches([' ', '\t']);
            if token_start.len() == rest.len() {
                return false;
            }
            let token: &str = token_start
                .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ','))
                .next()
                .unwrap_or("");
            let token_chars = |c: char| c.is_ascii_alphanumeric() || "-._~+/=".contains(c);
            token.len() >= 8
                && token.chars().all(token_chars)
                && (token
                    .chars()
                    .any(|c| c.is_ascii_digit() || !c.is_ascii_alphanumeric())
                    || token.chars().skip(1).any(|c| c.is_ascii_uppercase()))
        })
    })
}

/// A value starts here: not empty, not just a closing quote, not a delimiter.
fn non_empty_value(after: &str) -> bool {
    let mut chars = after.chars();
    let first = match chars.next() {
        Some(quote @ ('"' | '\'')) => match chars.next() {
            Some(c) if c != quote => c,
            _ => return false,
        },
        Some(c) => c,
        None => return false,
    };
    !matches!(first, '&' | ';' | ',' | '}' | ']' | ')') && !first.is_whitespace()
}

fn looks_like_secret_value(value: &str) -> bool {
    if SECRET_VALUE_PREFIXES.iter().any(|p| value.starts_with(p))
        || has_url_password(value)
        || has_secret_param(value)
    {
        return true;
    }
    // A long unbroken base64/hex-ish run with letters and digits: an API key,
    // a hash, a signed token. Words, versions, paths and URLs break the run.
    value.chars().count() >= 24
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-'))
        && value.chars().any(|c| c.is_ascii_digit())
        && value.chars().any(|c| c.is_ascii_alphabetic())
}

/// A value's source text bounded before formatting: at most twice
/// [`MAX_SIGNATURE_CHARS`] characters, with `…` appended when cut, so a huge
/// value is never copied whole.
fn short_value(text: &str) -> Cow<'_, str> {
    match text.char_indices().nth(2 * MAX_SIGNATURE_CHARS) {
        Some((cut, _)) => Cow::Owned(format!("{}…", &text[..cut])),
        None => Cow::Borrowed(text),
    }
}

/// Collapse whitespace to single spaces and cut to [`MAX_SIGNATURE_CHARS`]
/// characters (on a char boundary), marking a cut with `…`.
fn truncate_signature(text: &str) -> String {
    let mut out = String::new();
    let mut chars = 0;
    for word in text.split_whitespace() {
        let sep = usize::from(chars > 0);
        let len = word.chars().count();
        if chars + sep + len > MAX_SIGNATURE_CHARS {
            if sep == 1 && chars + 1 < MAX_SIGNATURE_CHARS {
                out.push(' ');
                chars += 1;
            }
            out.extend(word.chars().take(MAX_SIGNATURE_CHARS.saturating_sub(chars)));
            out.push('…');
            return out;
        }
        if sep == 1 {
            out.push(' ');
        }
        out.push_str(word);
        chars += sep + len;
    }
    out
}

/// Span of a tree-sitter node: 1-based lines, 0-based columns.
pub fn node_span(node: &tree_sitter::Node) -> Span {
    Span {
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_col: node.start_position().column,
        end_col: node.end_position().column,
    }
}

/// Span of a single source line (1-based) — for line-based formats.
pub fn line_span(line: usize, text: &str) -> Span {
    Span {
        start_line: line,
        end_line: line,
        start_col: 0,
        end_col: text.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(line: usize) -> Span {
        line_span(line, "x")
    }

    #[test]
    fn dotted_names_parent_ids_and_depth_limit() {
        let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", 10);
        let top = out
            .push(
                None,
                "scripts",
                SymbolKind::ConfigBlock,
                ConfigValue::None,
                span(2),
            )
            .unwrap();
        let child = out
            .push(
                Some(&top),
                "build",
                SymbolKind::ConfigKey,
                ConfigValue::None,
                span(3),
            )
            .unwrap();
        assert!(out
            .push(
                Some(&child),
                "deep",
                SymbolKind::ConfigKey,
                ConfigValue::None,
                span(4)
            )
            .is_none());
        let r = out.finish(false);
        assert_eq!(r.total, 2);
        assert_eq!(r.symbols[0].id, "s:f:1:2:scripts");
        assert_eq!(r.symbols[1].name, "scripts.build");
        assert_eq!(
            r.symbols[1].parent_symbol_id.as_deref(),
            Some("s:f:1:2:scripts")
        );
        assert_eq!(r.symbols[1].language, "json");
    }

    #[test]
    fn limit_keeps_first_n_and_counts_all() {
        let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", 2);
        for (i, key) in ["a", "b", "c"].iter().enumerate() {
            let p = out
                .push(
                    None,
                    key,
                    SymbolKind::ConfigBlock,
                    ConfigValue::None,
                    span(i + 1),
                )
                .unwrap();
            out.push(
                Some(&p),
                "x",
                SymbolKind::ConfigKey,
                ConfigValue::None,
                span(i + 1),
            );
        }
        let r = out.finish(false);
        assert_eq!(r.total, 6);
        let names: Vec<_> = r.symbols.iter().map(|s| s.name.as_str()).collect();
        // Level-1 first: `b` beats `a.x`.
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn signature_is_one_line_and_bounded() {
        assert_eq!(truncate_signature("a =\n  1"), "a = 1");
        let long = format!("key = \"{}\"", "é".repeat(200));
        let sig = truncate_signature(&long);
        assert!(sig.ends_with('…'));
        assert_eq!(sig.chars().count(), MAX_SIGNATURE_CHARS + 1);
        let short = "k = v";
        assert_eq!(truncate_signature(short), short);
        assert_eq!(short_value("abc"), "abc");
        let cut = short_value(&"x".repeat(10_000)).into_owned();
        assert_eq!(cut.chars().count(), 2 * MAX_SIGNATURE_CHARS + 1);
    }

    fn signature(key: &str, value: &str) -> String {
        let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", 10);
        out.push(
            None,
            key,
            SymbolKind::ConfigKey,
            ConfigValue::Scalar(value),
            span(1),
        );
        out.finish(false).symbols.remove(0).signature.unwrap()
    }

    #[test]
    fn secret_keys_are_redacted() {
        for key in [
            "password",
            "DB_PASSWORD",
            "secretAccessKey",
            "api_key",
            "x-api-key",
            "authToken",
            "Authorization",
            "auth",
            "DATABASE_DSN",
            "connection-string",
            "slackWebhook",
            "private_key",
        ] {
            assert_eq!(signature(key, "\"hunter2\""), format!("{key} = <redacted>"));
        }
    }

    #[test]
    fn secret_values_are_redacted() {
        for value in [
            "\"AKIAIOSFODNN7EXAMPLE\"",
            "\"ghp_abc\"",
            "\"sk-proj-abc\"",
            "\"xoxb-123\"",
            "\"-----BEGIN RSA PRIVATE KEY-----\"",
            "\"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\"",
            "\"3f786850e387550fdab836ed7e6dc881de23001b\"",
        ] {
            assert_eq!(signature("value", value), "value = <redacted>", "{value}");
        }
    }

    #[test]
    fn ordinary_values_are_kept() {
        assert_eq!(signature("version", "\"1.2.3\""), "version = \"1.2.3\"");
        assert_eq!(signature("author", "\"Jane Doe\""), "author = \"Jane Doe\"");
        assert_eq!(
            signature("main", "\"./dist/some/long/path/index.js\""),
            "main = \"./dist/some/long/path/index.js\""
        );
        assert_eq!(
            signature("homepage", "\"https://example.com/docs/getting-started\""),
            "homepage = \"https://example.com/docs/getting-started\""
        );
    }

    fn child_signature(parent: &str, key: &str, value: &str) -> String {
        let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", 10);
        let p = out
            .push(
                None,
                parent,
                SymbolKind::ConfigBlock,
                ConfigValue::None,
                span(1),
            )
            .unwrap();
        out.push(
            Some(&p),
            key,
            SymbolKind::ConfigKey,
            ConfigValue::Scalar(value),
            span(2),
        );
        out.finish(false).symbols.remove(1).signature.unwrap()
    }

    #[test]
    fn secret_parent_segments_redact_children() {
        assert_eq!(
            child_signature("secrets", "github", "\"abc123def\""),
            "secrets.github = <redacted>"
        );
        assert_eq!(
            child_signature("credentials", "prod", "\"hunter2\""),
            "credentials.prod = <redacted>"
        );
        assert_eq!(
            child_signature("password", "value", "\"x\""),
            "password.value = <redacted>"
        );
        assert_eq!(
            child_signature("scripts", "build", "\"tsc\""),
            "scripts.build = \"tsc\""
        );
    }

    #[test]
    fn url_passwords_are_redacted() {
        assert_eq!(
            signature("databaseUrl", "\"postgres://admin:S3cr3t@db:5432/app\""),
            "databaseUrl = <redacted>"
        );
        assert_eq!(
            signature("mirror", "\"see https://u:p@host/x\""),
            "mirror = <redacted>"
        );
        // No password in the userinfo, or none at all: kept.
        assert_eq!(
            signature("repo", "\"ssh://git@github.com/o/r\""),
            "repo = \"ssh://git@github.com/o/r\""
        );
        assert_eq!(
            signature("url", "\"http://localhost:8080/a@b\""),
            "url = \"http://localhost:8080/a@b\""
        );
    }

    #[test]
    fn query_credentials_and_odd_passwords_are_redacted() {
        assert_eq!(
            signature("url", "\"jdbc:postgresql://h/db?user=app&password=s3cret\""),
            "url = <redacted>"
        );
        assert_eq!(
            signature("conn", "\"Server=h;Pwd=x;\""),
            "conn = <redacted>"
        );
        assert_eq!(
            signature("hook", "\"https://h/x?TOKEN=abc\""),
            "hook = <redacted>"
        );
        // Empty parameter values carry nothing.
        assert_eq!(
            signature("url", "\"https://h/x?password=&a=1\""),
            "url = \"https://h/x?password=&a=1\""
        );
        // `#` / `?` inside an unencoded password.
        assert_eq!(
            signature("db", "\"postgres://u:p#a?ss@h:5432/app\""),
            "db = <redacted>"
        );
        assert_eq!(
            signature("url", "\"https://example.com/a?b=c#frag\""),
            "url = \"https://example.com/a?b=c#frag\""
        );
    }

    #[test]
    fn many_repeated_ids_stay_linear_and_match_the_scan() {
        // Every node stored, so every id is built: quadratic suffixing would crawl.
        let n = 50_000;
        let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", n + 1);
        // A natural `a#5` key takes that slot first; the repeats skip it.
        out.push(
            None,
            "a#5",
            SymbolKind::ConfigKey,
            ConfigValue::None,
            span(1),
        );
        for _ in 0..n {
            out.push(None, "a", SymbolKind::ConfigKey, ConfigValue::None, span(1));
        }
        let r = out.finish(false);
        assert_eq!(r.total, n + 1);
        let ids: Vec<&str> = r.symbols.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids[..5],
            [
                "s:f:1:1:a#5",
                "s:f:1:1:a",
                "s:f:1:1:a#2",
                "s:f:1:1:a#3",
                "s:f:1:1:a#4"
            ]
        );
        assert_eq!(ids[5], "s:f:1:1:a#6");
        assert_eq!(ids[n], format!("s:f:1:1:a#{}", n + 1));
    }

    #[test]
    fn keys_ending_in_key_are_redacted_but_literals_shown() {
        for key in [
            "encryptionKey",
            "signingKey",
            "service_role_key",
            "masterKey",
        ] {
            assert_eq!(signature(key, "\"abc\""), format!("{key} = <redacted>"));
        }
        assert_eq!(signature("apiKey", "null"), "apiKey = null");
        assert_eq!(signature("useSecret", "false"), "useSecret = false");
        assert_eq!(signature("keyboard", "\"us\""), "keyboard = \"us\"");
    }

    #[test]
    fn shapes_render_sizes_only() {
        let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", 10);
        let block = ConfigValue::Block {
            count: 1,
            noun: "key",
        };
        let list = ConfigValue::List {
            count: 3,
            noun: "item",
        };
        out.push(None, "a", SymbolKind::ConfigBlock, block, span(1));
        out.push(None, "b", SymbolKind::ConfigKey, list, span(2));
        let sigs: Vec<String> = out
            .finish(false)
            .symbols
            .into_iter()
            .filter_map(|s| s.signature)
            .collect();
        assert_eq!(sigs, ["a = {1 key}", "b = [3 items]"]);
    }

    #[test]
    fn redaction_additions() {
        for key in ["admin_pass", "DB_PASS", "creds", "aws_creds"] {
            assert_eq!(signature(key, "\"x\""), format!("{key} = <redacted>"));
        }
        for value in [
            "\"https://h/x?apikey=abc\"",
            "\"https://h/x?api-key=abc\"",
            "\"https://h/x?access_key=abc\"",
            "\"https://h/x?auth=abc\"",
            "{ user = \"a\", password = \"hunter2\" }",
            "\"run --password hunter2\"",
            "\"password: x\"",
        ] {
            assert_eq!(signature("v", value), "v = <redacted>", "{value}");
        }
    }

    #[test]
    fn authorization_headers_are_redacted() {
        for value in [
            "\"-H 'Authorization: Bearer abc123def'\"",
            "\"Authorization=Basic dXNlcjpwYXNz\"",
            "\"curl -H \\\"Authorization: token x\\\" https://h\"",
            "\"bearer eyJhbGciOiJIUzI1NiJ9.e30.sig\"",
            "\"Basic dXNlcjpwYXNzd29yZA==\"",
        ] {
            assert_eq!(signature("v", value), "v = <redacted>", "{value}");
        }
        assert_eq!(
            signature("v", "\"Basic settings for production\""),
            "v = \"Basic settings for production\""
        );
        assert_eq!(
            signature("v", "\"Authorization:\""),
            "v = \"Authorization:\""
        );
    }

    #[test]
    fn exec_form_flags_are_redacted() {
        for value in [
            r#"["app","--token","abc"]"#,
            r#"["/app","--spring.datasource.password","hunter2"]"#,
            r#"["/app", "--db-password", "x"]"#,
            "\"run --password hunter2\"",
        ] {
            assert_eq!(signature("CMD", value), "CMD = <redacted>", "{value}");
        }
    }

    #[test]
    fn exec_form_without_secrets_stays_readable() {
        for value in [
            r#"["nginx","-g","daemon off;"]"#,
            r#"["/app","--port","8080"]"#,
            r#"["/app","-p","hunter2"]"#,
            r#"["/app","--password-file","/run/secrets/db"]"#,
            r#"["/app","--token","--verbose"]"#,
        ] {
            assert_eq!(signature("CMD", value), format!("CMD = {value}"), "{value}");
        }
    }

    #[test]
    fn redaction_additions_keep_ordinary_values() {
        assert_eq!(
            signature("description", "\"Compass heading\""),
            "description = \"Compass heading\""
        );
        assert_eq!(signature("bypass", "true"), "bypass = true");
        assert_eq!(
            signature("path", "\"/usr/bin/passwd\""),
            "path = \"/usr/bin/passwd\""
        );
        assert_eq!(
            signature("url", "\"https://h/x?password=&a=1\""),
            "url = \"https://h/x?password=&a=1\""
        );
        assert_eq!(signature("v", "\"password = \"\""), "v = \"password = \"\"");
        // `token` must be followed by a separator: `max_tokens: 100` is kept.
        assert_eq!(
            signature("limits", "\"max_tokens: 100\""),
            "limits = \"max_tokens: 100\""
        );
        assert_eq!(
            signature("note", "\"reset your password here\""),
            "note = \"reset your password here\""
        );
    }

    #[test]
    fn control_whitespace_never_breaks_a_signature() {
        let sig = signature("v", "\"a\rb\nc\r\nd\u{2028}e\"");
        assert_eq!(sig, "v = \"a b c d e\"");
    }

    #[test]
    fn oversized_keys_are_refused_and_flag_the_file() {
        let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", 10);
        let huge = "a".repeat(1 << 20);
        assert!(out
            .push(
                None,
                &huge,
                SymbolKind::ConfigBlock,
                ConfigValue::None,
                span(1)
            )
            .is_none());
        let ok = "k".repeat(MAX_KEY_BYTES);
        assert!(out
            .push(None, &ok, SymbolKind::ConfigKey, ConfigValue::None, span(2))
            .is_some());
        let r = out.finish(false);
        assert_eq!(r.total, 1);
        assert_eq!(r.symbols.len(), 1);
        assert!(r.has_errors);
    }

    #[test]
    fn huge_parent_key_with_many_children_stays_small() {
        // `{"aaa…1 MB": {"k0": 0, …, "k99999": 0}}` through the real parser.
        let mut src = format!("{{\"{}\": {{", "a".repeat(1 << 20));
        for i in 0..100_000 {
            src.push_str(&format!("\"k{i}\": 0,"));
        }
        src.push_str("\"end\": 0}, \"after\": 1}");
        let r = crate::Parser::new()
            .parse_config(&src, Language::Json, "f:1", "a.json", 50)
            .unwrap();
        let names: Vec<&str> = r.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["after"]);
        assert_eq!(r.total, 1);
        assert!(r.has_errors);
    }

    #[test]
    fn nothing_is_kept_for_nodes_past_the_cap() {
        let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", 3);
        let parent = out
            .push(
                None,
                "p",
                SymbolKind::ConfigBlock,
                ConfigValue::None,
                span(1),
            )
            .unwrap();
        let key = "k".repeat(MAX_KEY_BYTES);
        for i in 0..100_000 {
            out.push(
                Some(&parent),
                &key,
                SymbolKind::ConfigKey,
                ConfigValue::None,
                span(i + 2),
            );
        }
        // Only `limit` level-2 candidates are ever buffered.
        assert_eq!(out.level1.len(), 1);
        assert_eq!(out.level2.len(), 3);
        let r = out.finish(false);
        assert_eq!(r.total, 100_001);
        assert_eq!(r.symbols[1].id, format!("s:f:1:2:p.{key}"));
        assert_eq!(r.symbols[2].parent_symbol_id.as_deref(), Some("s:f:1:1:p"));
        assert!(!r.has_errors);
    }

    /// `blocks` level-1 blocks with `attrs` attributes each, one per line.
    fn blocks(blocks: usize, attrs: usize, limit: usize) -> ConfigParseResult {
        let mut out = ConfigSymbols::new(Language::Hcl, "f:1", "main.tf", limit);
        let mut line = 0;
        for b in 0..blocks {
            line += 1;
            let p = out
                .push(
                    None,
                    &format!("b{b}"),
                    SymbolKind::ConfigBlock,
                    ConfigValue::None,
                    span(line),
                )
                .unwrap();
            for a in 0..attrs {
                line += 1;
                out.push(
                    Some(&p),
                    &format!("a{a}"),
                    SymbolKind::ConfigKey,
                    ConfigValue::None,
                    span(line),
                );
            }
        }
        out.finish(false)
    }

    #[test]
    fn over_the_cap_level_one_nodes_come_first() {
        let r = blocks(30, 5, 50);
        assert_eq!(r.total, 180);
        assert_eq!(r.symbols.len(), 50);
        let level1: Vec<&str> = r
            .symbols
            .iter()
            .filter(|s| s.parent_symbol_id.is_none())
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(level1.len(), 30);
        // The 20 attributes are the first 20 in document order: b0..b3 × 5.
        let attrs: Vec<&str> = r
            .symbols
            .iter()
            .filter(|s| s.parent_symbol_id.is_some())
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(attrs.len(), 20);
        assert_eq!(attrs[0], "b0.a0");
        assert_eq!(attrs[19], "b3.a4");
        // Document order, parent before child, parents stored.
        let ids: std::collections::HashSet<&str> =
            r.symbols.iter().map(|s| s.id.as_str()).collect();
        assert!(r
            .symbols
            .windows(2)
            .all(|w| w[0].span.start_line < w[1].span.start_line));
        assert!(r
            .symbols
            .iter()
            .filter_map(|s| s.parent_symbol_id.as_deref())
            .all(|p| ids.contains(p)));
    }

    #[test]
    fn more_level_one_nodes_than_the_cap_store_no_level_two() {
        let r = blocks(80, 2, 50);
        assert_eq!(r.total, 240);
        assert_eq!(r.symbols.len(), 50);
        assert!(r.symbols.iter().all(|s| s.parent_symbol_id.is_none()));
        assert_eq!(r.symbols[49].name, "b49");
    }

    #[test]
    fn under_the_cap_everything_is_stored_in_push_order() {
        let r = blocks(3, 2, 50);
        let names: Vec<&str> = r.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["b0", "b0.a0", "b0.a1", "b1", "b1.a0", "b1.a1", "b2", "b2.a0", "b2.a1"]
        );
        assert_eq!(r.symbols[1].parent_symbol_id.as_deref(), Some("s:f:1:1:b0"));
    }

    #[test]
    fn suffixes_ignore_dropped_nodes() {
        // Three `x` blocks on one line, each with an `x.y` child on the same
        // line, then a lone `x.y` key: names repeat across kept and dropped
        // nodes. Limit 4 keeps the four level-1 nodes; every `x.y` child is
        // dropped, so the level-1 `x.y` gets the bare id, not a suffix.
        let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", 4);
        for _ in 0..3 {
            let x = out
                .push(
                    None,
                    "x",
                    SymbolKind::ConfigBlock,
                    ConfigValue::None,
                    span(1),
                )
                .unwrap();
            out.push(
                Some(&x),
                "y",
                SymbolKind::ConfigKey,
                ConfigValue::None,
                span(1),
            );
        }
        out.push(
            None,
            "x.y",
            SymbolKind::ConfigKey,
            ConfigValue::None,
            span(1),
        );
        let r = out.finish(false);
        let ids: Vec<&str> = r.symbols.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            ["s:f:1:1:x", "s:f:1:1:x#2", "s:f:1:1:x#3", "s:f:1:1:x.y"]
        );
        assert_eq!(r.total, 7);
        // Same content, same ids.
        let again = {
            let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", 4);
            for _ in 0..3 {
                let x = out
                    .push(
                        None,
                        "x",
                        SymbolKind::ConfigBlock,
                        ConfigValue::None,
                        span(1),
                    )
                    .unwrap();
                out.push(
                    Some(&x),
                    "y",
                    SymbolKind::ConfigKey,
                    ConfigValue::None,
                    span(1),
                );
            }
            out.push(
                None,
                "x.y",
                SymbolKind::ConfigKey,
                ConfigValue::None,
                span(1),
            );
            out.finish(false)
        };
        let again: Vec<&str> = again.symbols.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, again);
    }

    #[test]
    fn mark_incomplete_flags_the_result() {
        let mut out = ConfigSymbols::new(Language::Ini, "f:1", "a.ini", 10);
        out.push(None, "a", SymbolKind::ConfigKey, ConfigValue::None, span(1));
        assert!(
            !ConfigSymbols::new(Language::Ini, "f:1", "a.ini", 10)
                .finish(false)
                .has_errors
        );
        out.mark_incomplete();
        let r = out.finish(false);
        assert!(r.has_errors);
        assert_eq!(r.symbols.len(), 1);
    }

    #[test]
    fn repeated_ids_get_a_suffix_and_all_count() {
        let mut out = ConfigSymbols::new(Language::Json, "f:1", "a.json", 10);
        out.push(None, "a", SymbolKind::ConfigKey, ConfigValue::None, span(1));
        out.push(None, "a", SymbolKind::ConfigKey, ConfigValue::None, span(1));
        out.push(
            None,
            "x.y",
            SymbolKind::ConfigKey,
            ConfigValue::None,
            span(1),
        );
        let x = out
            .push(
                None,
                "x",
                SymbolKind::ConfigBlock,
                ConfigValue::None,
                span(1),
            )
            .unwrap();
        out.push(
            Some(&x),
            "y",
            SymbolKind::ConfigKey,
            ConfigValue::None,
            span(1),
        );
        let r = out.finish(false);
        let ids: Vec<&str> = r.symbols.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "s:f:1:1:a",
                "s:f:1:1:a#2",
                "s:f:1:1:x.y",
                "s:f:1:1:x",
                "s:f:1:1:x.y#2"
            ]
        );
        assert_eq!(r.symbols[4].parent_symbol_id.as_deref(), Some("s:f:1:1:x"));
        assert_eq!(r.total, 5);
    }
}
