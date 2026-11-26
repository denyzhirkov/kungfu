//! INI / .properties / .cfg / .conf config symbols: sections as `ConfigBlock`s and their keys as `ConfigKey`s (`section.key`). Follows the `config_symbols` contract;
//! `json_config` is the reference.
//!
//! `.properties` has no sections: `[x]` is not a header there, a dotted key
//! (`spring.datasource.url`) is one level-1 key as written, and a trailing `\`
//! continues the value onto the next line (only the first line is kept).

use crate::config_symbols::{line_span, ConfigSymbols, ConfigValue};
use kungfu_types::symbol::{Span, SymbolKind};

struct Entry<'a> {
    key: &'a str,
    value: &'a str,
    span: Span,
}

struct Section<'a> {
    /// `None` for the pairs before the first header.
    header: Option<(&'a str, Span)>,
    entries: Vec<Entry<'a>>,
}

/// Push this file's config symbols into `out`, scanning `source` directly
/// (no tree-sitter grammar).
pub fn extract(source: &str, out: &mut ConfigSymbols) {
    let source = crate::env_config::strip_bom(source);
    if crate::env_config::drops_text_at_lone_cr(source) {
        out.mark_incomplete();
    }
    let properties = out
        .file_path()
        .to_ascii_lowercase()
        .ends_with(".properties");
    for section in sections(source, properties) {
        match section.header {
            None => {
                for entry in &section.entries {
                    out.push(
                        None,
                        entry.key,
                        SymbolKind::ConfigKey,
                        scalar(entry.value),
                        entry.span.clone(),
                    );
                }
            }
            Some((name, span)) => {
                let shown = ConfigValue::Block {
                    count: section.entries.len(),
                    noun: "key",
                };
                let Some(parent) = out.push(None, name, SymbolKind::ConfigBlock, shown, span)
                else {
                    continue;
                };
                for entry in &section.entries {
                    out.push(
                        Some(&parent),
                        entry.key,
                        SymbolKind::ConfigKey,
                        scalar(entry.value),
                        entry.span.clone(),
                    );
                }
            }
        }
    }
}

fn scalar(value: &str) -> ConfigValue<'_> {
    if value.is_empty() {
        ConfigValue::None
    } else {
        ConfigValue::Scalar(value)
    }
}

fn sections(source: &str, properties: bool) -> Vec<Section<'_>> {
    let mut sections = vec![Section {
        header: None,
        entries: Vec::new(),
    }];
    // Indentation of the last key line: a deeper-indented line after it is a
    // continuation of its value (`install_requires =` / `    click>=8`).
    let mut key_indent: Option<usize> = None;
    let mut lines = crate::env_config::text_lines(source).enumerate();
    while let Some((idx, line)) = lines.next() {
        let line_no = idx + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with([';', '#']) {
            continue;
        }
        if properties && trimmed.starts_with('!') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if !properties {
            if let Some(name) = section_name(trimmed) {
                sections.push(Section {
                    header: Some((name, line_span(line_no, line))),
                    entries: Vec::new(),
                });
                key_indent = None;
                continue;
            }
            if key_indent.is_some_and(|k| indent > k) {
                extend_last(&mut sections, line_no, line.len());
                continue;
            }
        }
        let Some((key, value)) = split_pair(trimmed, properties) else {
            continue;
        };
        let mut span = line_span(line_no, line);
        if properties {
            // Consume continuation lines so they are not read as keys.
            let mut continued = ends_with_continuation(trimmed);
            while continued {
                let Some((_, next)) = lines.next() else {
                    break;
                };
                span.end_line += 1;
                span.end_col = next.len();
                continued = ends_with_continuation(next.trim());
            }
        }
        let value = if properties {
            value.trim_end_matches('\\').trim_end()
        } else {
            value
        };
        key_indent = Some(indent);
        if let Some(current) = sections.last_mut() {
            if let Some((_, header_span)) = current.header.as_mut() {
                header_span.end_line = span.end_line;
                header_span.end_col = span.end_col;
            }
            current.entries.push(Entry { key, value, span });
        }
    }
    // A keyless `[section]` is still reported; only the implicit leading group
    // is dropped when empty.
    sections.retain(|s| s.header.is_some() || !s.entries.is_empty());
    sections
}

/// A continuation line belongs to the last key and to its section.
fn extend_last(sections: &mut [Section], end_line: usize, end_col: usize) {
    let Some(current) = sections.last_mut() else {
        return;
    };
    if let Some((_, header)) = current.header.as_mut() {
        header.end_line = end_line;
        header.end_col = end_col;
    }
    if let Some(entry) = current.entries.last_mut() {
        entry.span.end_line = end_line;
        entry.span.end_col = end_col;
    }
}

fn ends_with_continuation(line: &str) -> bool {
    let slashes = line.chars().rev().take_while(|&c| c == '\\').count();
    slashes % 2 == 1
}

/// `[name]`, optionally followed by a `;` or `#` comment; the brackets and
/// surrounding blanks are removed.
fn section_name(trimmed: &str) -> Option<&str> {
    let (name, after) = trimmed.strip_prefix('[')?.split_once(']')?;
    let after = after.trim_start();
    let name = name.trim();
    (!name.is_empty() && (after.is_empty() || after.starts_with([';', '#']))).then_some(name)
}

/// `key = value`, `key: value`, and in .properties `key value`.
fn split_pair(trimmed: &str, properties: bool) -> Option<(&str, &str)> {
    let sep = if properties {
        trimmed.find(|c: char| c == '=' || c == ':' || c.is_whitespace())
    } else {
        trimmed.find(['=', ':'])
    };
    let Some(at) = sep else {
        return properties.then_some((trimmed, ""));
    };
    let key = trimmed.get(..at)?.trim();
    let mut rest = trimmed.get(at..)?.trim_start();
    if properties {
        // One `=` or `:` may follow the blanks that ended the key.
        if let Some(after) = rest.strip_prefix(['=', ':']) {
            rest = after;
        }
    } else {
        rest = rest.get(1..)?;
    }
    (!key.is_empty()).then_some((key, rest.trim()))
}

#[cfg(test)]
mod tests {
    use crate::{ConfigParseResult, Parser};
    use kungfu_types::file::Language;
    use kungfu_types::symbol::SymbolKind;

    const INI: &str = include_str!("../tests/fixtures/config/app.ini");
    const PROPS: &str = include_str!("../tests/fixtures/config/application.properties");

    fn parse(src: &str, path: &str, limit: usize) -> ConfigParseResult {
        Parser::new()
            .parse_config(src, Language::Ini, "f:1", path, limit)
            .unwrap()
    }

    fn sigs(r: &ConfigParseResult) -> Vec<(&str, &str)> {
        r.symbols
            .iter()
            .map(|s| (s.name.as_str(), s.signature.as_deref().unwrap()))
            .collect()
    }

    #[test]
    fn ini_sections_keys_parents_and_spans() {
        let r = parse(INI, "app.ini", 50);
        assert_eq!(
            sigs(&r),
            [
                ("debug", "debug = true"),
                ("name", "name = demo"),
                ("database", "database = {4 keys}"),
                ("database.host", "database.host = localhost"),
                ("database.port", "database.port = 5432"),
                ("database.password", "database.password = <redacted>"),
                ("database.url", "database.url = postgres://app@localhost/db"),
                ("logging", "logging = {1 key}"),
                ("logging.level", "logging.level = INFO"),
                ("empty", "empty = {0 keys}"),
            ]
        );
        assert_eq!(r.symbols[0].kind, SymbolKind::ConfigKey);
        assert!(r.symbols[0].parent_symbol_id.is_none());
        let db = &r.symbols[2];
        assert_eq!(db.kind, SymbolKind::ConfigBlock);
        assert_eq!((db.span.start_line, db.span.end_line), (5, 10));
        let host = &r.symbols[3];
        assert_eq!(host.kind, SymbolKind::ConfigKey);
        assert_eq!(host.parent_symbol_id.as_deref(), Some(db.id.as_str()));
        assert_eq!(host.span.start_line, 6);
        let empty = r.symbols.last().unwrap();
        assert_eq!((empty.span.start_line, empty.span.end_line), (15, 15));
        assert_eq!(r.total, 10);
    }

    #[test]
    fn properties_keep_dotted_keys_whole() {
        let r = parse(PROPS, "src/main/resources/application.properties", 50);
        assert_eq!(
            sigs(&r),
            [
                (
                    "spring.application.name",
                    "spring.application.name = orders"
                ),
                (
                    "spring.datasource.url",
                    "spring.datasource.url = jdbc:postgresql://db:5432/orders"
                ),
                (
                    "spring.datasource.password",
                    "spring.datasource.password = <redacted>"
                ),
                ("server.port", "server.port = 8080"),
                ("long.value", "long.value = first"),
                ("empty.key", "empty.key"),
            ]
        );
        assert!(r
            .symbols
            .iter()
            .all(|s| s.parent_symbol_id.is_none() && s.kind == SymbolKind::ConfigKey));
        // The continuation lines belong to `long.value`, not new keys.
        let long = &r.symbols[4];
        assert_eq!((long.span.start_line, long.span.end_line), (7, 9));
        assert_eq!(r.symbols[5].span.start_line, 10);
    }

    #[test]
    fn properties_do_not_have_sections() {
        let r = parse("[not.a.section]=1\na=2\n", "x.properties", 50);
        assert_eq!(
            sigs(&r),
            [("[not.a.section]", "[not.a.section] = 1"), ("a", "a = 2")]
        );
    }

    #[test]
    fn comments_blank_lines_and_junk_are_skipped() {
        let r = parse(
            "; c\n# c\n! bang\n\n[s]\n  k = v\nno separator here\n[]\n= nokey\n",
            "a.cfg",
            50,
        );
        assert_eq!(sigs(&r), [("s", "s = {1 key}"), ("s.k", "s.k = v")]);
        // `!` is a comment only in .properties.
        let r = parse("! x = 1\n", "a.properties", 50);
        assert!(r.symbols.is_empty());
        let r = parse("! x = 1\n", "a.ini", 50);
        assert_eq!(sigs(&r), [("! x", "! x = 1")]);
    }

    #[test]
    fn crlf_bom_and_empty() {
        let r = parse("\u{feff}[a]\r\nk=1\r\n", "a.ini", 50);
        assert_eq!(sigs(&r), [("a", "a = {1 key}"), ("a.k", "a.k = 1")]);
        assert!(parse("", "a.ini", 50).symbols.is_empty());
        assert!(parse("", "a.properties", 50).symbols.is_empty());
        let r = parse("a=1\\\r\n  b\r\nc=2\r\n", "a.properties", 50);
        assert_eq!(sigs(&r), [("a", "a = 1"), ("c", "c = 2")]);
    }

    #[test]
    fn trailing_continuation_at_eof_terminates() {
        let r = parse("a=1\\", "a.properties", 50);
        assert_eq!(sigs(&r), [("a", "a = 1")]);
    }

    #[test]
    fn limit_keeps_total() {
        let r = parse("[a]\nx=1\ny=2\n[b]\nz=3\n", "a.ini", 3);
        assert_eq!((r.symbols.len(), r.total), (3, 5));
    }

    #[test]
    fn indented_continuations_and_commented_headers() {
        let src = "[options]\ninstall_requires =\n    click>=8\n    django==4.2\npython_requires = >=3.9\n[metadata] ; prod\nname = x\n[tox] # c\nenvlist = py\n";
        let r = parse(src, "setup.cfg", 50);
        assert_eq!(
            sigs(&r),
            [
                ("options", "options = {2 keys}"),
                ("options.install_requires", "options.install_requires"),
                ("options.python_requires", "options.python_requires = >=3.9"),
                ("metadata", "metadata = {1 key}"),
                ("metadata.name", "metadata.name = x"),
                ("tox", "tox = {1 key}"),
                ("tox.envlist", "tox.envlist = py"),
            ]
        );
        let key = &r.symbols[1];
        assert_eq!((key.span.start_line, key.span.end_line), (2, 4));
        let sec = &r.symbols[0];
        assert_eq!((sec.span.start_line, sec.span.end_line), (1, 5));
        // Uniformly indented keys are not continuations of each other.
        let r = parse("[s]\n  a = 1\n  b = 2\n", "a.ini", 50);
        assert_eq!(r.total, 3);
        // `[x] junk` is not a header.
        assert!(parse("[x] junk\n", "a.ini", 50).symbols.is_empty());
    }

    #[test]
    fn lone_cr_is_a_line_break() {
        // Numbering is `str::lines()`: a lone `\r` is not a break, and text
        // after it is cut, never read as a key or value.
        let r = parse(
            "[a]\rk = 1\npassword = hunter2\nlast = 3 \rx\r\n",
            "a.ini",
            50,
        );
        let got: Vec<(&str, usize)> = r
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.span.start_line))
            .collect();
        assert_eq!(got, [("a", 1), ("a.password", 2), ("a.last", 3)]);
        assert_eq!(
            r.symbols[1].signature.as_deref(),
            Some("a.password = <redacted>")
        );
        assert_eq!(r.symbols[2].signature.as_deref(), Some("a.last = 3"));
        assert!(r.has_errors);
        assert!(!parse("[a]\r\nk = 1\r\n", "a.ini", 50).has_errors);
    }
}
