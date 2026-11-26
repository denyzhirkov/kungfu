//! TOML config symbols: tables are `ConfigBlock`s, keys `ConfigKey`s, two levels deep (`dependencies.serde`); an array of tables (`[[bin]]`) is one symbol, not one per element. Follows the `config_symbols` contract;
//! `json_config` is the reference.
//!
//! A TOML document is a flat list of headers and pairs whose names say where
//! they belong, so it is read in two passes: first every declaration with its
//! full key path, then the symbols in document order. Dotted keys (`a.b = 1`)
//! and dotted headers (`[a.b]`) both just lengthen a path.

use crate::config_symbols::{node_span, ConfigParent, ConfigSymbols, ConfigValue, MAX_DEPTH};
use kungfu_types::symbol::{Span, SymbolKind};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

enum Kind<'t> {
    Table,
    /// The first two segments of a deeper header: a table nobody wrote a
    /// header for.
    Implicit,
    TableArray,
    /// The pair's value node.
    Pair(Node<'t>),
}

struct Decl<'t> {
    path: Vec<String>,
    kind: Kind<'t>,
    span: Span,
}

/// What the declarations say about a path, independent of where they sit.
#[derive(Default)]
struct Tally {
    /// Distinct next segments under a path: its direct keys.
    keys: HashMap<Vec<String>, HashSet<String>>,
    /// Occurrences of each `[[path]]` header.
    array_tables: HashMap<Vec<String>, usize>,
    /// Where the last declaration at or under a path ends (line, column), so a
    /// table without a header of its own can span its sub-tables.
    ends: HashMap<Vec<String>, (usize, usize)>,
}

impl Tally {
    fn note(&mut self, path: &[String]) {
        for depth in 1..path.len().min(MAX_DEPTH + 1) {
            self.keys
                .entry(path[..depth].to_vec())
                .or_default()
                .insert(path[depth].clone());
        }
    }

    fn cover(&mut self, path: &[String], span: &Span) {
        for depth in 1..=path.len().min(MAX_DEPTH) {
            let end = self.ends.entry(path[..depth].to_vec()).or_default();
            *end = (*end).max((span.end_line, span.end_col));
        }
    }

    fn key_count(&self, path: &[String]) -> usize {
        self.keys.get(path).map_or(0, HashSet::len)
    }
}

/// Index root pairs and `[table]` headers with their pairs. Of a header deeper
/// than [`MAX_DEPTH`] segments only its first two segments are symbols (as
/// implicit tables); the rest and its pairs are not. A table without a header
/// of its own spans from its first declaration through the last one under it.
pub fn extract(root: Node, source: &str, out: &mut ConfigSymbols) {
    let mut tally = Tally::default();
    let decls = collect(root, source, &mut tally);
    let mut handles: HashMap<Vec<String>, Option<ConfigParent>> = HashMap::new();
    for decl in &decls {
        for depth in 1..=decl.path.len().min(MAX_DEPTH) {
            let path = &decl.path[..depth];
            if handles.contains_key(path) {
                continue;
            }
            let parent = if depth == 1 {
                None
            } else {
                match handles.get(&decl.path[..1]) {
                    Some(Some(handle)) => Some(handle.clone()),
                    _ => break,
                }
            };
            // A path that is only a prefix of this declaration is a table
            // nobody wrote a header for.
            let is_implicit = depth < decl.path.len() || matches!(decl.kind, Kind::Implicit);
            let (kind, value) = if is_implicit {
                implicit(&tally, path)
            } else {
                match &decl.kind {
                    Kind::Table | Kind::Implicit => implicit(&tally, path),
                    Kind::TableArray => (
                        SymbolKind::ConfigKey,
                        ConfigValue::List {
                            count: tally.array_tables.get(path).copied().unwrap_or(1),
                            noun: "table",
                        },
                    ),
                    Kind::Pair(node) => pair_value(*node, source),
                }
            };
            let mut span = decl.span.clone();
            if is_implicit {
                if let Some(&(line, col)) = tally.ends.get(path) {
                    if (line, col) > (span.end_line, span.end_col) {
                        span.end_line = line;
                        span.end_col = col;
                    }
                }
            }
            let handle = out.push(parent.as_ref(), &decl.path[depth - 1], kind, value, span);
            handles.insert(path.to_vec(), handle);
        }
    }
}

fn implicit<'v>(tally: &Tally, path: &[String]) -> (SymbolKind, ConfigValue<'v>) {
    (
        SymbolKind::ConfigBlock,
        ConfigValue::Block {
            count: tally.key_count(path),
            noun: "key",
        },
    )
}

fn pair_value<'s>(value: Node, source: &'s str) -> (SymbolKind, ConfigValue<'s>) {
    match value.kind() {
        "inline_table" => (
            SymbolKind::ConfigBlock,
            ConfigValue::Block {
                count: count_children(value, |c| c.kind() == "pair"),
                noun: "key",
            },
        ),
        "array" => (
            SymbolKind::ConfigKey,
            ConfigValue::List {
                count: count_children(value, |c| c.kind() != "comment"),
                noun: "item",
            },
        ),
        _ => (
            SymbolKind::ConfigKey,
            ConfigValue::Scalar(text(value, source)),
        ),
    }
}

fn collect<'t>(root: Node<'t>, source: &str, tally: &mut Tally) -> Vec<Decl<'t>> {
    let mut decls = Vec::new();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        match node.kind() {
            "pair" => {
                if let Some(decl) = pair_decl(node, &[], source) {
                    tally.note(&decl.path);
                    tally.cover(&decl.path, &decl.span);
                    decls.push(decl);
                }
            }
            "table" | "table_array_element" => {
                let Some(header) = node.named_child(0).map(|k| key_path(k, source)) else {
                    continue;
                };
                if header.is_empty() {
                    continue;
                }
                let array = node.kind() == "table_array_element";
                tally.note(&header);
                if array {
                    *tally.array_tables.entry(header.clone()).or_default() += 1;
                }
                let deep = header.len() > MAX_DEPTH;
                let mut span = node_span(&node);
                let mut pairs = Vec::new();
                let mut last = None;
                let mut inner = node.walk();
                for child in node.named_children(&mut inner) {
                    if child.kind() == "comment" {
                        continue;
                    }
                    last = Some(child);
                    // An array of tables is one symbol: its elements' pairs are not.
                    if child.kind() == "pair" && !array && !deep {
                        pairs.extend(pair_decl(child, &header, source));
                    }
                }
                // The node runs on over trailing blank lines; the table ends
                // with its last pair.
                if let Some(last) = last {
                    let end = node_span(&last);
                    span.end_line = end.end_line;
                    span.end_col = end.end_col;
                }
                let (path, kind) = if deep {
                    (header[..MAX_DEPTH].to_vec(), Kind::Implicit)
                } else if array {
                    (header, Kind::TableArray)
                } else {
                    (header, Kind::Table)
                };
                tally.cover(&path, &span);
                decls.push(Decl { path, kind, span });
                for pair in pairs {
                    tally.note(&pair.path);
                    tally.cover(&pair.path, &pair.span);
                    decls.push(pair);
                }
            }
            _ => {}
        }
    }
    decls
}

fn pair_decl<'t>(pair: Node<'t>, table: &[String], source: &str) -> Option<Decl<'t>> {
    if pair.named_child_count() < 2 {
        return None;
    }
    let mut path = table.to_vec();
    path.extend(key_path(pair.named_child(0)?, source));
    let value = pair.named_child(pair.named_child_count() - 1)?;
    (path.len() > table.len()).then(|| Decl {
        path,
        kind: Kind::Pair(value),
        span: node_span(&pair),
    })
}

/// The first `MAX_DEPTH + 1` segments of a bare, quoted or dotted key (one
/// more than is ever shown, so a header's depth is still known); quotes
/// removed, escapes left as written.
fn key_path(key: Node, source: &str) -> Vec<String> {
    // `dotted_key` nests to the left: `a.b.c` is `((a . b) . c)`, so the
    // first segments are at the bottom. Walk down iteratively — input-controlled
    // depth must not become stack depth — then back up the saved chain.
    let mut chain = vec![key];
    while let Some(inner) = chain
        .last()
        .filter(|k| k.kind() == "dotted_key")
        .and_then(|k| k.named_child(0))
        .filter(|k| k.kind() == "dotted_key")
    {
        chain.push(inner);
    }
    let mut segments = Vec::new();
    for node in chain.iter().rev() {
        if node.kind() != "dotted_key" {
            segment(*node, source, &mut segments);
            continue;
        }
        let mut cursor = node.walk();
        for part in node.named_children(&mut cursor) {
            if part.kind() != "dotted_key" {
                segment(part, source, &mut segments);
            }
        }
        if segments.len() > MAX_DEPTH {
            break;
        }
    }
    segments.truncate(MAX_DEPTH + 1);
    segments
}

fn segment(key: Node, source: &str, segments: &mut Vec<String>) {
    match key.kind() {
        "bare_key" => segments.push(text(key, source).to_string()),
        "quoted_key" => {
            let raw = text(key, source);
            let unquoted = [('"', '"'), ('\'', '\'')]
                .iter()
                .find_map(|(open, close)| raw.strip_prefix(*open)?.strip_suffix(*close))
                .unwrap_or(raw);
            segments.push(unquoted.to_string());
        }
        _ => {}
    }
}

fn text<'s>(node: Node, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or("")
}

fn count_children(node: Node, keep: impl Fn(&Node) -> bool) -> usize {
    let mut cursor = node.walk();
    let count = node.named_children(&mut cursor).filter(keep).count();
    count
}

#[cfg(test)]
mod tests {
    use crate::{ConfigParseResult, Parser};
    use kungfu_types::file::Language;
    use kungfu_types::symbol::{Symbol, SymbolKind};

    fn parse(src: &str, limit: usize) -> ConfigParseResult {
        Parser::new()
            .parse_config(src, Language::Toml, "f:1", "Cargo.toml", limit)
            .unwrap()
    }

    fn names(r: &ConfigParseResult) -> Vec<&str> {
        r.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    fn find<'r>(r: &'r ConfigParseResult, name: &str) -> &'r Symbol {
        r.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol {name} in {:?}", names(r)))
    }

    fn sig<'r>(r: &'r ConfigParseResult, name: &str) -> &'r str {
        find(r, name).signature.as_deref().unwrap()
    }

    fn lines(r: &ConfigParseResult, name: &str) -> (usize, usize) {
        let s = find(r, name);
        (s.span.start_line, s.span.end_line)
    }

    #[test]
    fn cargo_toml_shape() {
        let r = parse(include_str!("../tests/fixtures/config/Cargo.toml"), 100);
        assert_eq!(
            names(&r),
            [
                "workspace",
                "workspace.members",
                "workspace.resolver",
                "workspace.package",
                "workspace.dependencies",
                "package",
                "package.name",
                "package.version",
                "dependencies",
                "dependencies.serde",
                "dependencies.tokio",
                "dependencies.regex",
                "bin",
                "profile",
                "profile.release",
            ]
        );
        assert_eq!(r.total, r.symbols.len());
        assert!(!r.has_errors);
        // Direct keys of a table include its sub-tables.
        assert_eq!(sig(&r, "workspace"), "workspace = {4 keys}");
        assert_eq!(find(&r, "workspace").kind, SymbolKind::ConfigBlock);
        assert_eq!(
            sig(&r, "workspace.members"),
            "workspace.members = [2 items]"
        );
        assert_eq!(find(&r, "workspace.members").kind, SymbolKind::ConfigKey);
        assert_eq!(sig(&r, "workspace.resolver"), "workspace.resolver = \"2\"");
        assert_eq!(sig(&r, "workspace.package"), "workspace.package = {2 keys}");
        assert_eq!(
            sig(&r, "workspace.dependencies"),
            "workspace.dependencies = {2 keys}"
        );
        assert_eq!(
            find(&r, "workspace.package").parent_symbol_id.as_deref(),
            Some(find(&r, "workspace").id.as_str())
        );
        // `version.workspace = true` is the table `package.version`.
        assert_eq!(sig(&r, "package"), "package = {2 keys}");
        assert_eq!(sig(&r, "package.version"), "package.version = {1 key}");
        assert_eq!(find(&r, "package.version").kind, SymbolKind::ConfigBlock);
        assert_eq!(sig(&r, "dependencies"), "dependencies = {3 keys}");
        assert_eq!(sig(&r, "dependencies.serde"), "dependencies.serde = \"1\"");
        // Inline table: a block; `[dependencies.regex]`: a block of its own.
        assert_eq!(
            sig(&r, "dependencies.tokio"),
            "dependencies.tokio = {2 keys}"
        );
        assert_eq!(find(&r, "dependencies.tokio").kind, SymbolKind::ConfigBlock);
        assert_eq!(
            sig(&r, "dependencies.regex"),
            "dependencies.regex = {1 key}"
        );
        // Two `[[bin]]` are one symbol at the first one.
        assert_eq!(sig(&r, "bin"), "bin = [2 tables]");
        assert_eq!(find(&r, "bin").kind, SymbolKind::ConfigKey);
        assert_eq!(find(&r, "bin").id, "s:f:1:24:bin");
        // 1-based lines; a table ends with its last pair, not the blank line.
        assert_eq!(lines(&r, "workspace"), (1, 3));
        assert_eq!(lines(&r, "workspace.package"), (5, 7));
        assert_eq!(lines(&r, "workspace.dependencies"), (9, 11));
        assert_eq!(lines(&r, "workspace.members"), (2, 2));
        assert_eq!(lines(&r, "package.version"), (15, 15));
        assert_eq!(lines(&r, "dependencies.regex"), (21, 22));
        assert_eq!(lines(&r, "bin"), (24, 25));
        // `[profile.release.package.foo]` is deeper than two segments: only its
        // prefix is indexed, as implicit tables.
        assert_eq!(sig(&r, "profile.release"), "profile.release = {1 key}");
        assert_eq!(lines(&r, "profile"), (30, 31));
    }

    #[test]
    fn pyproject_shape() {
        let r = parse(include_str!("../tests/fixtures/config/pyproject.toml"), 100);
        assert_eq!(
            names(&r),
            [
                "build-system",
                "build-system.requires",
                "build-system.build-backend",
                "project",
                "project.name",
                "project.version",
                "project.dependencies",
                "project.optional-dependencies",
                "tool",
                "tool.black",
                "tool.pytest",
            ]
        );
        assert_eq!(sig(&r, "project"), "project = {4 keys}");
        assert_eq!(
            sig(&r, "project.dependencies"),
            "project.dependencies = [2 items]"
        );
        assert_eq!(
            sig(&r, "project.optional-dependencies"),
            "project.optional-dependencies = {1 key}"
        );
        // `tool` and `tool.pytest` have no header of their own: implicit,
        // pushed once before their children. `ini_options` is three segments
        // deep: not a symbol, but it is one of `tool.pytest`'s keys.
        assert_eq!(sig(&r, "tool.pytest"), "tool.pytest = {1 key}");
        assert_eq!(lines(&r, "tool.pytest"), (16, 17));
        assert_eq!(find(&r, "tool").kind, SymbolKind::ConfigBlock);
        assert_eq!(sig(&r, "tool"), "tool = {2 keys}");
        assert_eq!(
            find(&r, "tool.black").parent_symbol_id.as_deref(),
            Some(find(&r, "tool").id.as_str())
        );
        // An implicit table spans through the last declaration under it.
        assert_eq!(lines(&r, "tool"), (13, 17));
    }

    #[test]
    fn a_table_declared_after_its_children_is_not_duplicated() {
        let r = parse("[a.b]\nx = 1\n[a]\ny = 2\n", 50);
        assert_eq!(names(&r), ["a", "a.b", "a.y"]);
        assert_eq!(sig(&r, "a"), "a = {2 keys}");
        assert_eq!(
            find(&r, "a.y").parent_symbol_id.as_deref(),
            Some(find(&r, "a").id.as_str())
        );
    }

    #[test]
    fn dotted_pairs_are_equivalent_tables() {
        let r = parse("a.b = 1\na.c = 2\n[t]\nx.y = 1\nx.z = 2\nw = 3\n", 50);
        assert_eq!(names(&r), ["a", "a.b", "a.c", "t", "t.x", "t.w"]);
        assert_eq!(sig(&r, "a"), "a = {2 keys}");
        assert_eq!(sig(&r, "a.b"), "a.b = 1");
        assert_eq!(sig(&r, "t"), "t = {2 keys}");
        assert_eq!(sig(&r, "t.x"), "t.x = {2 keys}");
        // Deeper dotted keys stop at the second level.
        let r = parse("a.b.c.d = 1\n", 50);
        assert_eq!(names(&r), ["a", "a.b"]);
        assert_eq!(sig(&r, "a.b"), "a.b = {1 key}");
    }

    #[test]
    fn deep_headers_keep_their_two_level_prefix_but_not_the_rest() {
        let r = parse(
            "[a]\nk = 1\n[a.b.c]\nhidden = 1\n[[a.b.d]]\nhidden = 2\n[z]\n",
            50,
        );
        assert_eq!(names(&r), ["a", "a.k", "a.b", "z"]);
        assert_eq!(r.total, 4);
        assert_eq!(sig(&r, "a"), "a = {2 keys}");
        assert_eq!(sig(&r, "a.b"), "a.b = {2 keys}");
        assert_eq!(find(&r, "a.b").kind, SymbolKind::ConfigBlock);
    }

    #[test]
    fn a_deep_header_alone_still_yields_its_prefix() {
        let r = parse("[tool.ruff.lint]\nselect = [\"E\"]\n", 50);
        assert_eq!(names(&r), ["tool", "tool.ruff"]);
        assert_eq!(sig(&r, "tool"), "tool = {1 key}");
        let r = parse("[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n", 50);
        assert_eq!(names(&r), ["target", "target.cfg(unix)"]);
        // An array of tables that deep is an implicit table, not a list.
        let r = parse("[[a.b.c]]\nx = 1\n[[a.b.c]]\nx = 2\n", 50);
        assert_eq!(names(&r), ["a", "a.b"]);
        assert_eq!(sig(&r, "a.b"), "a.b = {1 key}");
        assert_eq!(lines(&r, "a"), (1, 4));
    }

    #[test]
    fn input_controlled_depth_does_not_overflow_the_stack() {
        let n = 100_000;
        let dotted = format!("{}= 1\n", "a.".repeat(n));
        let r = parse(&dotted, 50);
        assert_eq!(names(&r), ["a", "a.a"]);
        let header = format!("[{}a]\nx = 1\n", "a.".repeat(n));
        assert_eq!(names(&parse(&header, 50)), ["a", "a.a"]);
        let array = format!("[[{}a]]\n", "a.".repeat(n));
        assert_eq!(names(&parse(&array, 50)), ["a", "a.a"]);
        let inline = format!("k = {}1{}\n", "{a = ".repeat(n), "}".repeat(n));
        let r = parse(&inline, 50);
        assert_eq!(r.symbols[0].signature.as_deref(), Some("k = {1 key}"));
        let nested = format!("k = {}1{}\n", "[".repeat(n), "]".repeat(n));
        assert_eq!(names(&parse(&nested, 50)), ["k"]);
    }

    #[test]
    fn nested_array_of_tables_is_one_level_two_symbol() {
        let r = parse("[[a.b]]\nx = 1\n[[a.b]]\nx = 2\n[[a.b]]\nx = 3\n", 50);
        assert_eq!(names(&r), ["a", "a.b"]);
        assert_eq!(sig(&r, "a.b"), "a.b = [3 tables]");
        assert_eq!(sig(&r, "a"), "a = {1 key}");
    }

    #[test]
    fn quoted_keys_lose_their_quotes() {
        let r = parse("[\"my table\"]\n\"a key\" = 1\n'lit' = 2\n", 50);
        assert_eq!(names(&r), ["my table", "my table.a key", "my table.lit"]);
    }

    #[test]
    fn values_are_scalars_lists_or_blocks() {
        let r = parse(
            "s = 'x'\nn = 1\nb = true\nd = 1979-05-27\nl = [1, [2], {k = 1}]\nt = {a = 1, b = 2}\ne = []\n",
            50,
        );
        assert_eq!(sig(&r, "s"), "s = 'x'");
        assert_eq!(sig(&r, "d"), "d = 1979-05-27");
        assert_eq!(sig(&r, "l"), "l = [3 items]");
        assert_eq!(sig(&r, "t"), "t = {2 keys}");
        assert_eq!(sig(&r, "e"), "e = [0 items]");
    }

    #[test]
    fn secrets_are_redacted() {
        let r = parse(
            "[registry]\ntoken = \"abc123\"\nurl = \"https://example.com\"\nkey = \"AKIAIOSFODNN7EXAMPLE\"\n",
            50,
        );
        assert_eq!(sig(&r, "registry.token"), "registry.token = <redacted>");
        assert_eq!(
            sig(&r, "registry.url"),
            "registry.url = \"https://example.com\""
        );
        assert_eq!(sig(&r, "registry.key"), "registry.key = <redacted>");
    }

    #[test]
    fn malformed_toml_does_not_panic_and_yields_what_it_can() {
        let r = parse("[a]\nx = 1\ny = = 2\n[b\nz = 3\n[c]\nw = 4\n", 50);
        assert!(r.has_errors);
        let n = names(&r);
        assert!(n.contains(&"a") && n.contains(&"a.x"), "{n:?}");
        for src in [
            "[",
            "[[",
            "= 1",
            "a =",
            "[a]\n[a.]\n",
            "\"unterminated = 1\n",
        ] {
            parse(src, 50);
        }
        assert!(parse("", 50).symbols.is_empty());
    }

    #[test]
    fn duplicate_keys_are_one_symbol() {
        let r = parse("a = 1\na = 2\n", 50);
        assert_eq!(names(&r), ["a"]);
    }

    #[test]
    fn limit_trims_but_total_counts_everything() {
        let r = parse("[a]\nx = 1\ny = 2\n[b]\nz = 1\n", 2);
        // Over the cap, level-1 tables come first.
        assert_eq!(names(&r), ["a", "b"]);
        assert_eq!(r.total, 5);
    }
}
