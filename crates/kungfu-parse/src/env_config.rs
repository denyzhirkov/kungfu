//! `.env` config symbols: variable names as `ConfigKey`s. Values are usually
//! secrets: push `ConfigValue::None`, never the value. Follows the
//! `config_symbols` contract; `json_config` is the reference.

use crate::config_symbols::{line_span, ConfigSymbols, ConfigValue};
use kungfu_types::symbol::SymbolKind;
/// Source text without a leading BOM.
pub(crate) fn strip_bom(source: &str) -> &str {
    source.strip_prefix('\u{feff}').unwrap_or(source)
}

/// `str::lines()` — the numbering every consumer of symbol spans uses — with
/// each line also cut at a lone `\r`, so text after a stray carriage return
/// can never leak into a name or value.
pub(crate) fn text_lines(source: &str) -> impl Iterator<Item = &str> + Clone {
    source
        .lines()
        .map(|line| line.split('\r').next().unwrap_or(""))
}

/// Does cutting `text`'s lines at a lone `\r` (see [`text_lines`]) drop
/// anything but whitespace? Then the symbols are incomplete.
pub(crate) fn drops_text_at_lone_cr(text: &str) -> bool {
    text.lines().any(|line| {
        line.split_once('\r')
            .is_some_and(|(_, rest)| !rest.trim().is_empty())
    })
}

/// Push this file's config symbols into `out`, scanning `source` directly
/// (no tree-sitter grammar).
pub fn extract(source: &str, out: &mut ConfigSymbols) {
    let source = strip_bom(source);
    if drops_text_at_lone_cr(source) {
        out.mark_incomplete();
    }
    let mut lines = text_lines(source).enumerate();
    // Once a quote character has no closing quote anywhere after some line,
    // no later line can have one either: such a value is read as one line.
    let mut no_closing = [false; 2];
    while let Some((idx, line)) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let declaration = trimmed
            .strip_prefix("export")
            .filter(|rest| rest.starts_with([' ', '\t']))
            .unwrap_or(trimmed);
        let Some((key, value)) = declaration.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !is_variable_name(key) {
            continue;
        }
        out.push(
            None,
            key,
            SymbolKind::ConfigKey,
            ConfigValue::None,
            line_span(idx + 1, line),
        );
        // A quoted value may run over several lines; its inner lines are text,
        // not declarations.
        let value = value.trim_start();
        let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            continue;
        };
        let dead = &mut no_closing[usize::from(quote == '\'')];
        if closes(value.get(1..).unwrap_or(""), quote) || *dead {
            continue;
        }
        let mut rest = lines.clone();
        let mut skipped = 0;
        let closed = loop {
            let Some((_, next)) = rest.next() else {
                break false;
            };
            skipped += 1;
            if closes(next, quote) {
                break true;
            }
        };
        if closed {
            lines.nth(skipped - 1);
        } else {
            *dead = true;
        }
    }
}

/// Does `text` (the part of a line after an opening quote) contain the
/// closing quote? Double quotes honour `\"`.
fn closes(text: &str, quote: char) -> bool {
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quote == '"' {
            escaped = true;
        } else if c == quote {
            return true;
        }
    }
    false
}

fn is_variable_name(key: &str) -> bool {
    key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[cfg(test)]
mod tests {
    use crate::{ConfigParseResult, Parser};
    use kungfu_types::file::Language;
    use kungfu_types::symbol::SymbolKind;

    const FIXTURE: &str = include_str!("../tests/fixtures/config/app.env");

    fn parse(src: &str, limit: usize) -> ConfigParseResult {
        Parser::new()
            .parse_config(src, Language::Env, "f:1", ".env", limit)
            .unwrap()
    }

    #[test]
    fn keys_only_never_values() {
        let r = parse(FIXTURE, 50);
        let got: Vec<(&str, &str, usize)> = r
            .symbols
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.signature.as_deref().unwrap(),
                    s.span.start_line,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("APP_ENV", "APP_ENV", 2),
                ("DATABASE_URL", "DATABASE_URL", 3),
                ("STRIPE_SECRET_KEY", "STRIPE_SECRET_KEY", 5),
                ("EMPTY", "EMPTY", 6),
                ("INDENTED", "INDENTED", 7),
            ]
        );
        assert!(r.symbols.iter().all(|s| s.kind == SymbolKind::ConfigKey
            && s.parent_symbol_id.is_none()
            && s.span.end_line == s.span.start_line));
        for secret in ["production", "S3cr3t", "sk-live"] {
            assert!(r
                .symbols
                .iter()
                .all(|s| !s.signature.as_deref().unwrap().contains(secret)));
        }
        assert_eq!(r.total, 5);
    }

    #[test]
    fn crlf_bom_and_export_forms() {
        let r = parse(
            "\u{feff}A=1\r\nexport B=2\r\nexported=3\r\nexport  = 4\r\n",
            50,
        );
        let names: Vec<&str> = r.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "exported"]);
        assert_eq!(r.symbols[1].span.start_line, 2);
    }

    #[test]
    fn multiline_quoted_values_are_not_declarations() {
        let src = "KEY=\"-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA0123456789abcdef=\nfoo=bar\n-----END RSA PRIVATE KEY-----\"\nAFTER=1\nSQ='a\nb=c'\nLAST=2\n";
        let r = parse(src, 50);
        let names: Vec<&str> = r.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["KEY", "AFTER", "SQ", "LAST"]);
        assert_eq!(r.symbols[1].span.start_line, 5);
        assert!(!r.has_errors);
        // Escaped quote does not close; closing quote followed by a comment.
        let r = parse("A=\"x \\\"\nB=1\"\nC=\"d\" # c\nD=1\n", 50);
        let names: Vec<&str> = r.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["A", "C", "D"]);
    }

    #[test]
    fn unclosed_quote_is_a_one_line_value() {
        let names_of = |src: &str| -> Vec<String> {
            parse(src, 50)
                .symbols
                .iter()
                .map(|s| s.name.clone())
                .collect()
        };
        assert_eq!(
            names_of("A=1\nB=\"never closed\nC=3\nD=4\n"),
            ["A", "B", "C", "D"]
        );
        // A trailing backslash before the quote is not an escape in practice.
        assert_eq!(
            names_of("LOG_DIR=\"C:\\logs\\\"\nNEXT=1\nMORE=2\n"),
            ["LOG_DIR", "NEXT", "MORE"]
        );
        assert!(!parse("A=\"x\nB=1\n", 50).has_errors);
        // Many unclosed quotes stay linear (each pair of lines is one value).
        let many = "K=\"x\n".repeat(50_000);
        assert_eq!(parse(&many, 10).total, 25_000);
    }

    #[test]
    fn lone_cr_is_a_line_break() {
        let src = "A=1\rLEAK=2\nB=2\r\nC=3\r";
        let r = parse(src, 50);
        let got: Vec<(&str, usize)> = r
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.span.start_line))
            .collect();
        assert_eq!(got, [("A", 1), ("B", 2), ("C", 3)]);
        assert_eq!(src.lines().count(), 3);
        assert!(r.has_errors, "text after the lone \\r was dropped");
        assert!(!parse("A=1\r\nB=2 \r", 50).has_errors);
        assert!(parse("A=1\rB=2\rC=3", 50).has_errors);
    }

    #[test]
    fn empty_and_comment_only() {
        assert!(parse("", 50).symbols.is_empty());
        assert!(parse("# x=1\n\n   \n", 50).symbols.is_empty());
    }

    #[test]
    fn limit_keeps_total() {
        let r = parse("A=1\nB=2\nC=3\n", 2);
        assert_eq!((r.symbols.len(), r.total), (2, 3));
    }
}
