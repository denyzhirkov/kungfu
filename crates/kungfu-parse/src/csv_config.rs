//! CSV / TSV config symbols: header columns as `ConfigKey`s; `.tsv` (see `ConfigSymbols::file_path`) is tab-separated. Rows are data and are never indexed. Follows the `config_symbols` contract;
//! `json_config` is the reference.

use crate::config_symbols::{line_span, ConfigSymbols, ConfigValue};
use kungfu_types::symbol::SymbolKind;

/// A header longer than this is not a header (an unclosed quote would
/// otherwise make the first "record" the whole file).
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// A column label longer than this is data.
const MAX_LABEL_CHARS: usize = 64;

/// A whitespace-free run of letters and digits this long is a token, not a
/// column name.
const TOKEN_CHARS: usize = 20;

/// Candidate delimiters, in preference order on a tie.
const DELIMITERS: [char; 4] = [',', ';', '\t', '|'];

/// Prefixes of well-known credential formats.
const SECRET_PREFIXES: &[&str] = &["AKIA", "ghp_", "sk-", "xox"];

/// Push this file's config symbols into `out`, scanning `source` directly
/// (no tree-sitter grammar).
///
/// The delimiter is the most frequent of `,` `;` tab `|` in the first record
/// (always tab for `.tsv`); trailing empty fields are dropped. The first
/// record is taken as a header unless some field looks like data (see
/// [`looks_like_data`]); then nothing is emitted and the file is flagged
/// incomplete, so it is distinguishable from an empty one. Residual: a
/// headerless first row of plain short words (`admin,hunter2`) still passes as
/// a header, and a long single-word column name mixing letters and digits is
/// taken for a token.
pub fn extract(source: &str, out: &mut ConfigSymbols) {
    let source = crate::env_config::strip_bom(source);
    if crate::env_config::drops_text_at_lone_cr(source.lines().next().unwrap_or("")) {
        out.mark_incomplete();
    }
    let (record, too_long) = first_record(source);
    if too_long {
        out.mark_incomplete();
        return;
    }
    if record.trim().is_empty() {
        return;
    }
    let delimiter = if out.file_path().to_ascii_lowercase().ends_with(".tsv") {
        '\t'
    } else {
        detect_delimiter(record)
    };
    let mut fields = split_fields(record, delimiter);
    while fields.last().is_some_and(String::is_empty) {
        fields.pop();
    }
    if fields.is_empty() || fields.iter().any(|f| looks_like_data(f)) {
        out.mark_incomplete();
        return;
    }
    let first_line = crate::env_config::text_lines(source).next().unwrap_or("");
    for field in &fields {
        out.push(
            None,
            field,
            SymbolKind::ConfigKey,
            ConfigValue::None,
            line_span(1, first_line),
        );
    }
}

/// The first record (up to an unquoted line break), and whether it exceeded
/// [`MAX_HEADER_BYTES`].
fn first_record(source: &str) -> (&str, bool) {
    let mut in_quotes = false;
    for (at, c) in source.char_indices() {
        if at > MAX_HEADER_BYTES {
            return ("", true);
        }
        match c {
            '"' => in_quotes = !in_quotes,
            '\n' | '\r' if !in_quotes => return (source.get(..at).unwrap_or(""), false),
            _ => {}
        }
    }
    (source, false)
}

/// The candidate that occurs most outside quotes; `,` when none occurs.
fn detect_delimiter(record: &str) -> char {
    let mut counts = [0usize; DELIMITERS.len()];
    let mut in_quotes = false;
    for c in record.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if !in_quotes {
            if let Some(at) = DELIMITERS.iter().position(|&d| d == c) {
                counts[at] += 1;
            }
        }
    }
    let mut best = 0;
    for (at, &count) in counts.iter().enumerate() {
        if count > counts[best] {
            best = at;
        }
    }
    DELIMITERS[best]
}

/// The trimmed fields of a record, quotes resolved.
fn split_fields(record: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = record.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            c if c == delimiter && !in_quotes => {
                fields.push(field.trim().to_string());
                field.clear();
            }
            c => field.push(c),
        }
    }
    fields.push(field.trim().to_string());
    fields
}

/// A field that cannot be a column name: empty, a number (or a date or time),
/// an e-mail address, a URL, a control character, longer than
/// [`MAX_LABEL_CHARS`], or shaped like a token or a known credential.
fn looks_like_data(field: &str) -> bool {
    let has_digit = field.chars().any(|c| c.is_ascii_digit());
    let numeric = has_digit
        && (field.parse::<f64>().is_ok()
            || field.chars().all(|c| {
                c.is_ascii_digit()
                    || matches!(c, '.' | ',' | '-' | '+' | '/' | ':' | ';' | '|' | ' ')
            }));
    let token = field.chars().count() >= TOKEN_CHARS
        && !field.contains(char::is_whitespace)
        && has_digit
        && field.chars().any(char::is_alphabetic);
    field.is_empty()
        || field.chars().count() > MAX_LABEL_CHARS
        || field.contains(['@', '\n'])
        || field.contains("://")
        || field.chars().any(char::is_control)
        || numeric
        || token
        || SECRET_PREFIXES.iter().any(|p| field.starts_with(p))
}

#[cfg(test)]
mod tests {
    use crate::{ConfigParseResult, Parser};
    use kungfu_types::file::Language;
    use kungfu_types::symbol::SymbolKind;

    const CSV: &str = include_str!("../tests/fixtures/config/people.csv");
    const TSV: &str = include_str!("../tests/fixtures/config/people.tsv");

    fn parse(src: &str, path: &str, limit: usize) -> ConfigParseResult {
        Parser::new()
            .parse_config(src, Language::Csv, "f:1", path, limit)
            .unwrap()
    }

    fn names(r: &ConfigParseResult) -> Vec<&str> {
        r.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn quoted_header_with_bom_and_crlf() {
        let r = parse(CSV, "people.csv", 50);
        assert_eq!(
            names(&r),
            ["id", "full name", "price (USD)", "Qty.", "created_at"]
        );
        assert!(r.symbols.iter().all(|s| s.kind == SymbolKind::ConfigKey
            && s.parent_symbol_id.is_none()
            && s.span.start_line == 1
            && s.span.end_line == 1));
        assert_eq!(r.symbols[2].signature.as_deref(), Some("price (USD)"));
        // Data rows never reach the index.
        assert!(!names(&r).contains(&"Ada"));
        assert_eq!(r.total, 5);
        assert!(!r.has_errors);
    }

    #[test]
    fn tsv_splits_on_tabs_only() {
        let r = parse(TSV, "data/people.tsv", 50);
        assert_eq!(names(&r), ["id", "name", "email"]);
        // `.tsv` forces tab even when commas are more frequent.
        let r = parse("a b\tc\td\n1\t2\t3\n", "x.TSV", 50);
        assert_eq!(names(&r), ["a b", "c", "d"]);
    }

    #[test]
    fn delimiter_is_detected_from_the_first_record() {
        let r = parse("Name;Vorname;E-Mail\nx;y;z\n", "de.csv", 50);
        assert_eq!(names(&r), ["Name", "Vorname", "E-Mail"]);
        let r = parse("id|name|city\n1|a|b\n", "p.csv", 50);
        assert_eq!(names(&r), ["id", "name", "city"]);
        let r = parse("id\tname\n1\ta\n", "p.csv", 50);
        assert_eq!(names(&r), ["id", "name"]);
        // A delimiter inside quotes does not count.
        let r = parse("\"a;b;c\",d\n", "p.csv", 50);
        assert_eq!(names(&r), ["a;b;c", "d"], "comma wins outside quotes");
    }

    #[test]
    fn ordinary_headers_are_accepted() {
        for header in [
            "Total ($)",
            "Amount (€)",
            "2024 Revenue",
            "R&D",
            "Time: start",
            "Customer's Name",
            "Active?",
            "#",
            "Temp [C]",
            "Name*",
            "\"Last, First\"",
            "\"say \"\"hi\"\"\"",
        ] {
            let src = format!("{header},id\n");
            let r = parse(&src, "n.csv", 50);
            assert_eq!(r.total, 2, "{header}");
            assert!(!r.has_errors, "{header}");
        }
        let r = parse("id,\"Last, First\",Age\n", "n.csv", 50);
        assert_eq!(names(&r), ["id", "Last, First", "Age"]);
        // Trailing empty fields are dropped; a middle one is not a header.
        assert_eq!(
            names(&parse("id,name,\n1,2,\n", "n.csv", 50)),
            ["id", "name"]
        );
        assert_eq!(names(&parse("id,name,,\n", "n.csv", 50)), ["id", "name"]);
        assert!(parse("id,,name\n", "n.csv", 50).symbols.is_empty());
    }

    #[test]
    fn data_rows_are_rejected_and_flagged() {
        for src in [
            "alice,alice@example.com,hunter2\n",
            "AKIAIOSFODNN7EXAMPLE,wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n",
            "ghp_abc,x\n",
            "sk-proj-abc,x\n",
            "xoxb-1,x\n",
            "id,https://example.com/x\n",
            "abcdefghij1234567890x,id\n",
            "1;2,5;3\n",
            "1,2,3\n4,5,6\n",
            "1.5,-2,3e4\n",
            "id,2024\n1,2\n",
            "2024-01-01,x\n",
            "id,12:30\n",
            ",,\n",
            "id,,name\n",
        ] {
            let r = parse(src, "n.csv", 50);
            assert!(r.symbols.is_empty(), "{src:?}");
            assert!(r.has_errors, "{src:?}");
        }
        let long = format!("id,{}\n", "x".repeat(65));
        assert!(parse(&long, "n.csv", 50).symbols.is_empty());
        let ok = format!("id,{}\n", "x".repeat(64));
        assert_eq!(parse(&ok, "n.csv", 50).total, 2);
        // Empty file / blank first line: nothing, and nothing to flag.
        for src in ["", "\n1,2\n"] {
            let r = parse(src, "n.csv", 50);
            assert!(r.symbols.is_empty() && !r.has_errors, "{src:?}");
        }
        // Residual: a row of plain short words passes.
        assert_eq!(parse("admin,hunter2\n", "n.csv", 50).total, 2);
        assert_eq!(parse("admin,secret\n", "n.csv", 50).total, 2);
    }

    #[test]
    fn unicode_labels_single_column_and_lone_cr() {
        assert_eq!(
            names(&parse("Straße,名前\n", "n.csv", 50)),
            ["Straße", "名前"]
        );
        assert_eq!(names(&parse("name\nx\n", "n.csv", 50)), ["name"]);
        let r = parse("a,b\rsecret,x\n1,2\n", "n.csv", 50);
        assert_eq!(names(&r), ["a", "b"]);
        assert!(r.has_errors);
        assert!(!parse("a,b\r\n1,2\r\n", "n.csv", 50).has_errors);
    }

    #[test]
    fn oversized_or_unclosed_header_is_flagged() {
        let huge = format!("\"{}", "x".repeat(200_000));
        let r = parse(&huge, "n.csv", 50);
        assert!(r.symbols.is_empty());
        assert!(r.has_errors);
        let r = parse("\"never closed,a\nrow\n", "n.csv", 50);
        assert_eq!(r.total, r.symbols.len());
        assert!(!parse("a,b\n1,2\n", "n.csv", 50).has_errors);
    }

    #[test]
    fn limit_keeps_total() {
        let r = parse("a,b,c\n1,2,3\n", "n.csv", 2);
        assert_eq!((r.symbols.len(), r.total), (2, 3));
    }
}
