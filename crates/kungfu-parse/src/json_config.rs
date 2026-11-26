//! JSON config symbols: object keys two levels deep. The reference
//! implementation of the `config_symbols` contract.

use crate::config_symbols::{node_span, ConfigParent, ConfigSymbols, ConfigValue};
use kungfu_types::symbol::SymbolKind;
use tree_sitter::Node;

/// Index the keys of a top-level object. A top-level array (or scalar) has no
/// keys and yields nothing — values are never indexed.
pub fn extract(root: Node, source: &str, out: &mut ConfigSymbols) {
    let mut cursor = root.walk();
    for value in root.named_children(&mut cursor) {
        if value.kind() == "object" {
            push_pairs(value, source, None, out);
        }
    }
}

fn push_pairs(object: Node, source: &str, parent: Option<&ConfigParent>, out: &mut ConfigSymbols) {
    let mut cursor = object.walk();
    for pair in object.named_children(&mut cursor) {
        if pair.kind() != "pair" {
            continue;
        }
        let (Some(key), Some(value)) = (
            pair.child_by_field_name("key"),
            pair.child_by_field_name("value"),
        ) else {
            continue;
        };
        let key = key_text(key, source);
        let (kind, shown) = match value.kind() {
            "object" => (
                SymbolKind::ConfigBlock,
                ConfigValue::Block {
                    count: count_named(value, "pair"),
                    noun: "key",
                },
            ),
            "array" => (
                SymbolKind::ConfigKey,
                ConfigValue::List {
                    count: count_items(value),
                    noun: "item",
                },
            ),
            _ => (
                SymbolKind::ConfigKey,
                ConfigValue::Scalar(text(value, source)),
            ),
        };
        let pushed = out.push(parent, key, kind, shown, node_span(&pair));
        if let (Some(handle), "object") = (pushed, value.kind()) {
            push_pairs(value, source, Some(&handle), out);
        }
    }
}

/// Key without its quotes; escapes are left as written.
fn key_text<'s>(key: Node, source: &'s str) -> &'s str {
    let raw = text(key, source);
    raw.strip_prefix('"')
        .and_then(|k| k.strip_suffix('"'))
        .unwrap_or(raw)
}

fn text<'s>(node: Node, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or("")
}

fn count_named(node: Node, kind: &str) -> usize {
    let mut cursor = node.walk();
    let count = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == kind)
        .count();
    count
}

fn count_items(array: Node) -> usize {
    let mut cursor = array.walk();
    let count = array
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
            .parse_config(src, Language::Json, "f:1", "package.json", limit)
            .unwrap()
    }

    fn names(r: &ConfigParseResult) -> Vec<&str> {
        r.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn two_levels_with_dotted_names_and_parents() {
        let src = r#"{
  "name": "app",
  "scripts": {
    "build": "tsc",
    "nested": { "too": { "deep": 1 } }
  },
  "files": ["a", "b", "c"]
}"#;
        let r = parse(src, 50);
        assert_eq!(
            names(&r),
            [
                "name",
                "scripts",
                "scripts.build",
                "scripts.nested",
                "files"
            ]
        );
        assert_eq!(r.total, 5);
        let scripts = &r.symbols[1];
        assert_eq!(scripts.kind, SymbolKind::ConfigBlock);
        assert_eq!(scripts.signature.as_deref(), Some("scripts = {2 keys}"));
        let build = &r.symbols[2];
        assert_eq!(build.kind, SymbolKind::ConfigKey);
        assert_eq!(build.parent_symbol_id.as_deref(), Some(scripts.id.as_str()));
        assert_eq!(build.signature.as_deref(), Some(r#"scripts.build = "tsc""#));
        // A level-2 object is a block, but its keys are past the depth limit.
        assert_eq!(r.symbols[3].kind, SymbolKind::ConfigBlock);
    }

    #[test]
    fn arrays_are_one_key_not_exploded() {
        let r = parse(
            r#"{"files": [{"a": 1}, {"b": 2}], "list": [], "one": {"k": 1}}"#,
            50,
        );
        assert_eq!(names(&r), ["files", "list", "one", "one.k"]);
        assert_eq!(r.symbols[0].kind, SymbolKind::ConfigKey);
        assert_eq!(r.symbols[0].signature.as_deref(), Some("files = [2 items]"));
        assert_eq!(r.symbols[1].signature.as_deref(), Some("list = [0 items]"));
        assert_eq!(r.symbols[2].signature.as_deref(), Some("one = {1 key}"));
        // A top-level array has no keys at all.
        assert!(parse(r#"[{"a": 1}]"#, 50).symbols.is_empty());
    }

    #[test]
    fn long_values_are_truncated() {
        let src = format!(r#"{{"description": "{}"}}"#, "word ".repeat(100));
        let r = parse(&src, 50);
        let sig = r.symbols[0].signature.as_deref().unwrap();
        assert!(sig.starts_with("description = \"word word"));
        assert!(sig.ends_with('…'));
        assert!(sig.chars().count() <= crate::config_symbols::MAX_SIGNATURE_CHARS + 1);
    }

    #[test]
    fn spans_cover_the_pair_lines() {
        let src = "{\n  \"a\": 1,\n  \"b\": {\n    \"c\": true\n  }\n}\n";
        let r = parse(src, 50);
        let span = |i: usize| (r.symbols[i].span.start_line, r.symbols[i].span.end_line);
        assert_eq!(span(0), (2, 2));
        assert_eq!(span(1), (3, 5));
        assert_eq!(span(2), (4, 4));
        assert_eq!(r.symbols[2].id, "s:f:1:4:b.c");
    }

    #[test]
    fn secrets_are_redacted_in_signatures() {
        let r = parse(
            r#"{"aws": {"secretAccessKey": "wJalrXUtnFEMI/K7MDENG", "region": "eu-west-1"}}"#,
            50,
        );
        assert_eq!(
            r.symbols[1].signature.as_deref(),
            Some("aws.secretAccessKey = <redacted>")
        );
        assert_eq!(
            r.symbols[2].signature.as_deref(),
            Some(r#"aws.region = "eu-west-1""#)
        );
    }

    #[test]
    fn duplicate_and_dotted_keys_get_distinct_ids() {
        let r = parse(r#"{"a":1,"a":2,"x.y":1,"x":{"y":2}}"#, 50);
        let ids: std::collections::HashSet<&str> =
            r.symbols.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(r.symbols.len(), 5);
        assert_eq!(ids.len(), 5);
        assert_eq!(r.total, 5);
    }

    #[test]
    fn syntax_errors_are_flagged() {
        assert!(!parse(r#"{"a": 1}"#, 50).has_errors);
        assert!(parse(r#"{a: 1, "b": 2}"#, 50).has_errors);
        let r = parse("{\"a\": 1\n \"b\": 2, \"c\": 3}", 50);
        assert!(r.has_errors, "missing comma");
    }

    #[test]
    fn limit_trims_but_total_counts_everything() {
        let r = parse(r#"{"a": {"x": 1, "y": 2}, "b": 1, "c": 2}"#, 2);
        // Over the cap, level-1 keys come first.
        assert_eq!(names(&r), ["a", "b"]);
        assert_eq!(r.total, 5);
    }

    #[test]
    fn fixture_package_json_smoke() {
        let src = include_str!("../tests/fixtures/config/package.json");
        let r = parse(src, 50);
        let names = names(&r);
        for expected in [
            "name",
            "scripts",
            "scripts.build",
            "dependencies.react",
            "workspaces",
        ] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        // A key may itself contain a dot; third-level keys and array elements
        // are still not symbols.
        assert!(names.contains(&"exports.."));
        assert!(!names.iter().any(|n| n.ends_with(".import")));
        assert!(!names.contains(&"packages/*"));
        assert_eq!(r.total, r.symbols.len());
    }
}
