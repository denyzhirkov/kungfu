//! YAML config symbols: mapping keys two levels deep (`services.web`); sequences are one `ConfigKey`. Follows the `config_symbols` contract;
//! `json_config` is the reference.

use crate::config_symbols::{node_span, ConfigParent, ConfigSymbols, ConfigValue};
use kungfu_types::symbol::SymbolKind;
use tree_sitter::Node;

/// What a `block_node` / `flow_node` holds once anchors, tags and comments are
/// set aside.
enum Shape<'t> {
    Mapping(Node<'t>),
    Sequence(Node<'t>),
    BlockScalar,
    /// A plain or quoted scalar, or an alias.
    Scalar(Node<'t>),
}

/// Index the keys of every document's root mapping. A root sequence or scalar
/// has no keys and yields nothing — values are never indexed.
pub fn extract(root: Node, source: &str, out: &mut ConfigSymbols) {
    let mut cursor = root.walk();
    for document in root.named_children(&mut cursor) {
        if document.kind() != "document" {
            continue;
        }
        let mut inner = document.walk();
        for node in document.named_children(&mut inner) {
            if let Some(Shape::Mapping(mapping)) = shape(node) {
                push_pairs(mapping, source, None, out);
            }
        }
    }
}

fn push_pairs(mapping: Node, source: &str, parent: Option<&ConfigParent>, out: &mut ConfigSymbols) {
    let mut cursor = mapping.walk();
    for pair in mapping.named_children(&mut cursor) {
        if !is_pair(pair) {
            continue;
        }
        let Some(key) = pair
            .child_by_field_name("key")
            .and_then(|k| key_text(k, source))
        else {
            continue;
        };
        // `<<: *base` merges another mapping; it is not a key of its own.
        if key == "<<" {
            continue;
        }
        let value = pair.child_by_field_name("value").and_then(shape);
        let (kind, shown) = match &value {
            Some(Shape::Mapping(m)) => (
                SymbolKind::ConfigBlock,
                ConfigValue::Block {
                    count: count_pairs(*m),
                    noun: "key",
                },
            ),
            Some(Shape::Sequence(s)) => (
                SymbolKind::ConfigKey,
                ConfigValue::List {
                    count: count_items(*s),
                    noun: "item",
                },
            ),
            Some(Shape::Scalar(s)) => {
                (SymbolKind::ConfigKey, ConfigValue::Scalar(text(*s, source)))
            }
            // A `|` / `>` scalar is prose or a script: none of it is shown.
            Some(Shape::BlockScalar) | None => (SymbolKind::ConfigKey, ConfigValue::None),
        };
        let pushed = out.push(parent, key, kind, shown, node_span(&pair));
        if let (Some(handle), Some(Shape::Mapping(m))) = (pushed, value) {
            push_pairs(m, source, Some(&handle), out);
        }
    }
}

fn is_pair(node: Node) -> bool {
    matches!(node.kind(), "block_mapping_pair" | "flow_pair")
}

fn shape(node: Node) -> Option<Shape> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        return Some(match child.kind() {
            "anchor" | "tag" | "comment" => continue,
            "block_mapping" | "flow_mapping" => Shape::Mapping(child),
            "block_sequence" | "flow_sequence" => Shape::Sequence(child),
            "block_scalar" => Shape::BlockScalar,
            _ => Shape::Scalar(child),
        });
    }
    None
}

/// The key as a name: quotes removed, escapes left as written. Collections
/// and aliases are not names.
fn key_text<'s>(key: Node, source: &'s str) -> Option<&'s str> {
    let Some(Shape::Scalar(scalar)) = shape(key) else {
        return None;
    };
    let raw = text(scalar, source);
    let name = match scalar.kind() {
        "double_quote_scalar" => raw.strip_prefix('"').and_then(|k| k.strip_suffix('"')),
        "single_quote_scalar" => raw.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')),
        "alias" => return None,
        _ => Some(raw),
    };
    name.or(Some(raw))
}

fn text<'s>(node: Node, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or("")
}

fn count_pairs(mapping: Node) -> usize {
    let mut cursor = mapping.walk();
    let count = mapping
        .named_children(&mut cursor)
        .filter(|c| is_pair(*c))
        .count();
    count
}

fn count_items(sequence: Node) -> usize {
    let mut cursor = sequence.walk();
    let count = sequence
        .named_children(&mut cursor)
        .filter(|c| c.kind() != "comment")
        .count();
    count
}

#[cfg(test)]
mod tests {
    use crate::{ConfigParseResult, Parser};
    use kungfu_types::file::Language;
    use kungfu_types::symbol::SymbolKind;

    fn parse(src: &str, limit: usize) -> ConfigParseResult {
        Parser::new()
            .parse_config(src, Language::Yaml, "f:1", "a.yaml", limit)
            .unwrap()
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

    fn find<'r>(r: &'r ConfigParseResult, name: &str) -> &'r kungfu_types::symbol::Symbol {
        r.symbols.iter().find(|s| s.name == name).unwrap()
    }

    fn lines(r: &ConfigParseResult, name: &str) -> (usize, usize) {
        let s = find(r, name);
        (s.span.start_line, s.span.end_line)
    }

    #[test]
    fn docker_compose_shape() {
        let r = parse(include_str!("../tests/fixtures/config/compose.yaml"), 100);
        assert_eq!(
            names(&r),
            [
                "version",
                "x-common",
                "x-common.restart",
                "x-common.environment",
                "services",
                "services.web",
                "services.db",
                "volumes",
                "volumes.data",
                "volumes.cache",
                "networks",
                "quoted name",
            ]
        );
        assert_eq!(r.total, r.symbols.len());
        assert!(!r.has_errors);
        assert_eq!(sig(&r, "version"), "version = \"3.9\"");
        assert_eq!(sig(&r, "services"), "services = {2 keys}");
        assert_eq!(sig(&r, "services.web"), "services.web = {4 keys}");
        assert_eq!(find(&r, "services.web").kind, SymbolKind::ConfigBlock);
        assert_eq!(
            find(&r, "services.web").parent_symbol_id.as_deref(),
            Some(find(&r, "services").id.as_str())
        );
        // Block scalar: no dump of the script; anchor: the mapping is a block.
        assert_eq!(sig(&r, "services.db"), "services.db = {2 keys}");
        assert_eq!(sig(&r, "x-common"), "x-common = {2 keys}");
        assert_eq!(
            sig(&r, "x-common.environment"),
            "x-common.environment = [1 item]"
        );
        assert_eq!(find(&r, "x-common.environment").kind, SymbolKind::ConfigKey);
        // Flow mappings, alias values and quoted keys/values.
        assert_eq!(sig(&r, "volumes.data"), "volumes.data = {0 keys}");
        assert_eq!(sig(&r, "volumes.cache"), "volumes.cache = {2 keys}");
        assert_eq!(sig(&r, "networks"), "networks = *common");
        assert_eq!(sig(&r, "quoted name"), "quoted name = 'single'");
        // 1-based lines; a block spans its whole body.
        assert_eq!(lines(&r, "version"), (2, 2));
        assert_eq!(lines(&r, "services"), (7, 20));
        assert_eq!(lines(&r, "services.web"), (8, 15));
        assert_eq!(find(&r, "services.web").id, "s:f:1:8:services.web");
    }

    #[test]
    fn merge_keys_and_sequences_are_not_symbols() {
        let r = parse(
            "services:\n  web:\n    <<: *common\n    ports: [1, 2]\n",
            50,
        );
        assert_eq!(names(&r), ["services", "services.web"]);
        let r = parse(
            "base: &b\n  <<: *x\n  a: 1\nlist:\n  - &i one\n  - *i\n",
            50,
        );
        assert_eq!(names(&r), ["base", "base.a", "list"]);
        assert_eq!(sig(&r, "list"), "list = [2 items]");
    }

    #[test]
    fn github_workflow_shape() {
        let r = parse(include_str!("../tests/fixtures/config/workflow.yml"), 100);
        assert_eq!(
            names(&r),
            [
                "name",
                "on",
                "on.push",
                "on.pull_request",
                "env",
                "env.RUST_LOG",
                "jobs",
                "jobs.build",
                "jobs.lint",
            ]
        );
        assert_eq!(sig(&r, "name"), "name = CI");
        assert_eq!(sig(&r, "on"), "on = {2 keys}");
        assert_eq!(sig(&r, "on.push"), "on.push = {1 key}");
        // An empty value is a key with nothing to show.
        assert_eq!(sig(&r, "on.pull_request"), "on.pull_request");
        assert_eq!(sig(&r, "jobs.build"), "jobs.build = {2 keys}");
        assert_eq!(lines(&r, "jobs.build"), (9, 13));
        assert_eq!(lines(&r, "jobs"), (8, 18));
    }

    #[test]
    fn every_document_of_a_multi_document_file() {
        let r = parse(include_str!("../tests/fixtures/config/k8s.yaml"), 100);
        assert_eq!(
            names(&r),
            [
                "apiVersion",
                "kind",
                "metadata",
                "metadata.name",
                "metadata.labels",
                "spec",
                "spec.replicas",
                "spec.template",
                "apiVersion",
                "kind",
                "metadata",
                "metadata.name",
                "spec",
                "spec.ports",
            ]
        );
        assert_eq!(sig(&r, "spec.template"), "spec.template = {1 key}");
        let ids: std::collections::HashSet<&str> =
            r.symbols.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids.len(), r.symbols.len());
        let second_kind = r
            .symbols
            .iter()
            .filter(|s| s.name == "kind")
            .nth(1)
            .unwrap();
        assert_eq!(second_kind.signature.as_deref(), Some("kind = Service"));
        assert_eq!(second_kind.span.start_line, 15);
    }

    #[test]
    fn root_sequence_and_scalar_yield_nothing() {
        assert!(parse("- a: 1\n- b: 2\n", 50).symbols.is_empty());
        assert!(parse("just text\n", 50).symbols.is_empty());
        assert!(parse("", 50).symbols.is_empty());
    }

    #[test]
    fn flow_mapping_root() {
        let r = parse("{a: 1, b: {c: 2}}\n", 50);
        assert_eq!(names(&r), ["a", "b", "b.c"]);
    }

    #[test]
    fn secrets_are_redacted() {
        let r = parse(
            "env:\n  DB_PASSWORD: hunter2\n  REGION: eu\n  note: wJalrXUtnFEMI/K7MDENG/bPxRfiCY\n",
            50,
        );
        assert_eq!(sig(&r, "env.DB_PASSWORD"), "env.DB_PASSWORD = <redacted>");
        assert_eq!(sig(&r, "env.REGION"), "env.REGION = eu");
        assert_eq!(sig(&r, "env.note"), "env.note = <redacted>");
        let compose = parse(include_str!("../tests/fixtures/config/compose.yaml"), 100);
        assert!(!compose.symbols.iter().any(|s| s
            .signature
            .as_deref()
            .is_some_and(|sig| sig.contains("hunter2"))));
    }

    #[test]
    fn multiline_scalars_stay_on_one_line() {
        let r = parse(
            "a: |\n  line one\n  line two\nb: >-\n  folded\nc: \"x\n  y\"\n",
            50,
        );
        assert_eq!(sig(&r, "a"), "a");
        assert_eq!(sig(&r, "b"), "b");
        assert_eq!(sig(&r, "c"), "c = \"x y\"");
        assert_eq!(lines(&r, "a"), (1, 3));
    }

    #[test]
    fn malformed_yaml_does_not_panic_and_yields_what_it_can() {
        let r = parse("a: 1\nb: \"unterminated\nc: 2\n", 50);
        assert!(r.has_errors);
        assert_eq!(names(&r), ["a", "b", "c"]);
        // Some breakage makes the grammar drop the whole document: no symbols,
        // flagged, no panic.
        for src in [
            "a: 1\nb: 2\nc: [1, 2\n",
            "a: 1\nb: 2\n\tc: 3\n",
            "a: 1\n- x\nb: 2\n",
        ] {
            assert!(parse(src, 50).has_errors, "{src:?}");
        }
        parse("\u{0}: [\n  - {\n", 50);
    }

    #[test]
    fn duplicate_keys_get_distinct_ids() {
        let r = parse("a: 1\na: 2\n", 50);
        assert_eq!(r.symbols.len(), 2);
        assert_ne!(r.symbols[0].id, r.symbols[1].id);
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let n = 100_000;
        let flow = format!("k: {}1{}\n", "[".repeat(n), "]".repeat(n));
        assert_eq!(names(&parse(&flow, 50)), ["k"]);
        let map = format!("k: {}1{}\n", "{a: ".repeat(n), "}".repeat(n));
        assert_eq!(names(&parse(&map, 50)), ["k", "k.a"]);
    }

    #[test]
    fn limit_trims_but_total_counts_everything() {
        let r = parse("a:\n  x: 1\n  y: 2\nb: 1\nc: 2\n", 2);
        // Over the cap, level-1 keys come first.
        assert_eq!(names(&r), ["a", "b"]);
        assert_eq!(r.total, 5);
    }
}
