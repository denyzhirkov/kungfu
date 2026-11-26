//! Makefile config symbols: targets as `ConfigBlock`s and variables as `ConfigKey`s. Follows the `config_symbols` contract;
//! `json_config` is the reference.
//!
//! Rules: pattern rules (`%`), special dot-targets (`.PHONY`), targets that are
//! variable references and target-specific variables are not symbols;
//! `define … endef` bodies, conditionals and `include`s are skipped.

use crate::config_symbols::{ConfigSymbols, ConfigValue};
use kungfu_types::symbol::{Span, SymbolKind};

/// Words that may precede a variable assignment (`export CC := gcc`).
const ASSIGNMENT_PREFIXES: &[&str] = &["export", "override", "private"];

/// Directives that are neither rules nor assignments.
const DIRECTIVES: &[&str] = &[
    "ifeq", "ifneq", "ifdef", "ifndef", "else", "endif", "include", "-include", "sinclude",
    "vpath", "undefine", "unexport", "endef",
];

enum Statement<'a> {
    Assignment { name: &'a str, value: &'a str },
    Rule { targets: &'a str, rest: &'a str },
}

/// Push this file's config symbols into `out`, scanning `source` directly
/// (no tree-sitter grammar).
pub fn extract(source: &str, out: &mut ConfigSymbols) {
    let source = crate::env_config::strip_bom(source);
    if crate::env_config::drops_text_at_lone_cr(source) {
        out.mark_incomplete();
    }
    let lines: Vec<&str> = crate::env_config::text_lines(source).collect();
    // A rule's recipe is open until a non-recipe statement: only then is a
    // tab-indented line a recipe line rather than an indented statement.
    let mut rule_open = false;
    // `.RECIPEPREFIX = >` changes what marks a recipe line.
    let mut recipe_prefix = '\t';
    let mut i = 0;
    while let Some(&line) = lines.get(i) {
        let start = i;
        i += 1;
        if rule_open && line.starts_with(recipe_prefix) {
            continue;
        }
        let first = strip_comment(line).trim();
        if first.is_empty() {
            continue;
        }
        let first = strip_prefixes(first);
        let word = first.split_whitespace().next().unwrap_or("");
        if word == "define" {
            let (next, closed) = skip_define(&lines, i);
            i = next;
            if !closed {
                out.mark_incomplete();
            }
            continue;
        }
        if DIRECTIVES.contains(&word) || first.is_empty() {
            continue;
        }

        // Fold continuation lines: the statement ends on the first line
        // without a trailing backslash.
        let mut text = strip_continuation(first).to_string();
        let mut continued = has_continuation(first);
        let mut last = start;
        while continued {
            let Some(&next) = lines.get(i) else {
                break;
            };
            last = i;
            i += 1;
            let next = strip_comment(next).trim();
            text.push(' ');
            text.push_str(strip_continuation(next));
            continued = has_continuation(next);
        }

        match classify(&text) {
            Some(Statement::Assignment { name, .. }) => {
                rule_open = false;
                if !is_variable(name) {
                    continue;
                }
                // Only the first physical line: later lines of a folded value
                // are often shell commands carrying credentials.
                let value = match classify(strip_continuation(first)) {
                    Some(Statement::Assignment { value, .. }) => value,
                    _ => "",
                };
                if name == ".RECIPEPREFIX" {
                    recipe_prefix = value.chars().next().unwrap_or('\t');
                }
                let shown = if value.is_empty() {
                    ConfigValue::None
                } else {
                    ConfigValue::Scalar(value)
                };
                out.push(
                    None,
                    name,
                    SymbolKind::ConfigKey,
                    shown,
                    Span {
                        start_line: start + 1,
                        end_line: last + 1,
                        start_col: 0,
                        end_col: lines.get(last).map_or(0, |l| l.len()),
                    },
                );
            }
            Some(Statement::Rule { targets, rest }) => {
                rule_open = true;
                let (end, next) = recipe_end(&lines, last, i, recipe_prefix);
                i = next;
                // `target: VAR = value` sets a variable for one target.
                if matches!(classify(rest), Some(Statement::Assignment { .. })) {
                    continue;
                }
                let prerequisites = prerequisites(rest);
                let shown = if prerequisites.is_empty() {
                    ConfigValue::None
                } else {
                    ConfigValue::Scalar(prerequisites)
                };
                let span = Span {
                    start_line: start + 1,
                    end_line: end + 1,
                    start_col: 0,
                    end_col: lines.get(end).map_or(0, |l| l.len()),
                };
                for target in targets.split_whitespace().filter(|t| is_target(t)) {
                    out.push(
                        None,
                        target,
                        SymbolKind::ConfigBlock,
                        shown_ref(&shown),
                        span.clone(),
                    );
                }
            }
            None => {}
        }
    }
}

fn shown_ref<'a>(value: &ConfigValue<'a>) -> ConfigValue<'a> {
    match value {
        ConfigValue::Scalar(text) => ConfigValue::Scalar(text),
        _ => ConfigValue::None,
    }
}

/// Drop a leading `export` / `override` / `private`.
fn strip_prefixes(mut text: &str) -> &str {
    while let Some((word, rest)) = text.split_once(char::is_whitespace) {
        if !ASSIGNMENT_PREFIXES.contains(&word) {
            break;
        }
        text = rest.trim_start();
    }
    if ASSIGNMENT_PREFIXES.contains(&text) {
        ""
    } else {
        text
    }
}

/// Index of the line after the `endef` matching a `define` whose body starts
/// at `from` (the end of the file when unterminated), and whether it closed.
fn skip_define(lines: &[&str], from: usize) -> (usize, bool) {
    let mut depth = 1usize;
    let mut i = from;
    while let Some(line) = lines.get(i) {
        i += 1;
        let first = strip_prefixes(line.trim()).split_whitespace().next();
        match first {
            Some("define") => depth += 1,
            Some("endef") => {
                depth -= 1;
                if depth == 0 {
                    return (i, true);
                }
            }
            _ => {}
        }
    }
    (i, false)
}

/// The last line of the recipe following a rule that ends on line `rule_end`,
/// and where scanning resumes. Blank and comment lines may sit between recipe
/// lines.
fn recipe_end(lines: &[&str], rule_end: usize, from: usize, prefix: char) -> (usize, usize) {
    let mut end = rule_end;
    let mut j = from;
    while let Some(line) = lines.get(j) {
        if line.starts_with(prefix) {
            end = j;
            let mut continued = has_continuation(line.trim_end());
            j += 1;
            while continued {
                let Some(next) = lines.get(j) else {
                    break;
                };
                end = j;
                continued = has_continuation(next.trim_end());
                j += 1;
            }
        } else if line.trim().is_empty() || line.trim_start().starts_with('#') {
            j += 1;
        } else {
            break;
        }
    }
    (end, j)
}

/// Classify by the first `=` or `:` outside `$(…)` / `${…}`.
fn classify(text: &str) -> Option<Statement<'_>> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for (at, &c) in bytes.iter().enumerate() {
        match c {
            b'(' | b'{' => depth += 1,
            b')' | b'}' => depth = depth.saturating_sub(1),
            b'=' if depth == 0 => {
                // `:=` never gets here (the `:` returns first); `?=`, `+=`, `!=`.
                let name_end = if at > 0 && matches!(bytes.get(at - 1), Some(b'?' | b'+' | b'!')) {
                    at - 1
                } else {
                    at
                };
                return Some(Statement::Assignment {
                    name: text.get(..name_end)?.trim(),
                    value: text.get(at + 1..)?.trim(),
                });
            }
            b':' if depth == 0 => {
                let mut end = at;
                while bytes.get(end) == Some(&b':') {
                    end += 1;
                }
                if bytes.get(end) == Some(&b'=') {
                    return Some(Statement::Assignment {
                        name: text.get(..at)?.trim(),
                        value: text.get(end + 1..)?.trim(),
                    });
                }
                return Some(Statement::Rule {
                    targets: text.get(..at)?.trim(),
                    rest: text.get(end..)?.trim(),
                });
            }
            _ => {}
        }
    }
    None
}

/// Prerequisites of a rule: after a static pattern rule's second colon, before
/// an inline `;` recipe.
fn prerequisites(rest: &str) -> &str {
    let rest = match classify(rest) {
        Some(Statement::Rule { rest, .. }) => rest,
        _ => rest,
    };
    rest.split(';').next().unwrap_or("").trim()
}

fn is_variable(name: &str) -> bool {
    !name.is_empty() && !name.contains(char::is_whitespace) && !name.contains('$')
}

fn is_target(target: &str) -> bool {
    let special = target.len() > 1
        && target.starts_with('.')
        && target[1..]
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_');
    !special && target != "&" && !target.contains(['%', '$'])
}

/// Cut at the first `#` that is not escaped.
fn strip_comment(line: &str) -> &str {
    let mut prev = ' ';
    for (at, c) in line.char_indices() {
        if c == '#' && prev != '\\' {
            return line.get(..at).unwrap_or(line);
        }
        prev = c;
    }
    line
}

fn has_continuation(line: &str) -> bool {
    line.ends_with('\\')
}

fn strip_continuation(line: &str) -> &str {
    line.strip_suffix('\\').unwrap_or(line).trim_end()
}

#[cfg(test)]
mod tests {
    use crate::{ConfigParseResult, Parser};
    use kungfu_types::file::Language;
    use kungfu_types::symbol::SymbolKind;

    const FIXTURE: &str = include_str!("../tests/fixtures/config/Makefile");

    fn parse(src: &str, limit: usize) -> ConfigParseResult {
        Parser::new()
            .parse_config(src, Language::Makefile, "f:1", "Makefile", limit)
            .unwrap()
    }

    fn sigs(r: &ConfigParseResult) -> Vec<(&str, &str)> {
        r.symbols
            .iter()
            .map(|s| (s.name.as_str(), s.signature.as_deref().unwrap()))
            .collect()
    }

    fn lines(r: &ConfigParseResult, name: &str) -> (usize, usize) {
        let s = r.symbols.iter().find(|s| s.name == name).unwrap();
        (s.span.start_line, s.span.end_line)
    }

    #[test]
    fn fixture_variables_targets_and_skips() {
        let r = parse(FIXTURE, 50);
        assert_eq!(
            sigs(&r),
            [
                ("CC", "CC = gcc"),
                ("CFLAGS", "CFLAGS = -O2 -Wall"),
                ("VERSION", "VERSION = 1.2.3"),
                ("OBJS", "OBJS = main.o util.o"),
                ("all", "all = build test"),
                ("build", "build = $(OBJS)"),
                ("test", "test = $(OBJS)"),
                ("clean", "clean"),
                ("EXE", "EXE = .exe"),
                ("deploy", "deploy = build"),
            ]
        );
        let kind = |n: &str| r.symbols.iter().find(|s| s.name == n).unwrap().kind;
        assert_eq!(kind("CC"), SymbolKind::ConfigKey);
        assert_eq!(kind("all"), SymbolKind::ConfigBlock);
        assert!(r.symbols.iter().all(|s| s.parent_symbol_id.is_none()));
        assert_eq!(lines(&r, "CC"), (2, 2));
        assert_eq!(lines(&r, "all"), (9, 9));
        assert_eq!(lines(&r, "build"), (11, 12));
        assert_eq!(lines(&r, "test"), (11, 12));
        assert_eq!(lines(&r, "clean"), (17, 18));
        assert_eq!(lines(&r, "EXE"), (23, 23));
        assert_eq!(lines(&r, "deploy"), (31, 33));
        assert_eq!(r.total, 10);
    }

    #[test]
    fn assignment_operators_and_prefixes() {
        let r = parse(
            "A = 1\nB := 2\nC ::= 3\nD ?= 4\nE += 5\nexport F := 6\noverride G = 7\nH != date\nAPI_TOKEN = abc\nEMPTY =\n",
            50,
        );
        assert_eq!(
            sigs(&r),
            [
                ("A", "A = 1"),
                ("B", "B = 2"),
                ("C", "C = 3"),
                ("D", "D = 4"),
                ("E", "E = 5"),
                ("F", "F = 6"),
                ("G", "G = 7"),
                ("H", "H = date"),
                ("API_TOKEN", "API_TOKEN = <redacted>"),
                ("EMPTY", "EMPTY"),
            ]
        );
    }

    #[test]
    fn recipe_lines_and_target_variables_are_not_statements() {
        let r = parse(
            "all: a\n\tX = 1\n\ty: z\n\ndebug: CFLAGS += -g\nreal: x ; echo inline\n",
            50,
        );
        assert_eq!(sigs(&r), [("all", "all = a"), ("real", "real = x")]);
        assert_eq!(lines(&r, "all"), (1, 3));
    }

    #[test]
    fn special_pattern_and_variable_targets_are_skipped() {
        let r = parse(
            ".PHONY: a\n.DEFAULT: x\n.SUFFIXES:\n%.o: %.c\n\tcc\n$(BIN): x\n${A} b: y\n.venv: x\n./out/x: y\nobj1.o obj2.o: %.o: %.c\n",
            50,
        );
        assert_eq!(
            sigs(&r),
            [
                ("b", "b = y"),
                (".venv", ".venv = x"),
                ("./out/x", "./out/x = y"),
                ("obj1.o", "obj1.o = %.c"),
                ("obj2.o", "obj2.o = %.c"),
            ]
        );
    }

    #[test]
    fn define_conditionals_and_includes_produce_nothing() {
        let r = parse(
            "define A\nx: y\nZ = 1\n  define nested\nendef\nq: r\nendef\nifeq ($(A),b)\nelse ifeq (x:y,z)\nendif\ninclude a.mk\n-include b: c\nexport\nreal: x\n",
            50,
        );
        assert_eq!(sigs(&r), [("real", "real = x")]);
        // Unterminated define swallows the rest, without hanging.
        let r = parse("define A\nx: y\n", 50);
        assert!(r.symbols.is_empty());
        assert!(r.has_errors);
        assert!(!parse(FIXTURE, 50).has_errors);
    }

    #[test]
    fn continuations_and_comments() {
        let r = parse(
            "SRCS = a.c \\\n  b.c \\\n  c.c\nall: a \\\n b # trailing\n\techo \\\n\t  more\nnext: x # c\n",
            50,
        );
        assert_eq!(
            sigs(&r),
            [
                ("SRCS", "SRCS = a.c"),
                ("all", "all = a b"),
                ("next", "next = x"),
            ]
        );
        assert_eq!(lines(&r, "SRCS"), (1, 3));
        assert_eq!(lines(&r, "all"), (4, 7));
    }

    #[test]
    fn crlf_and_empty() {
        let r = parse(&FIXTURE.replace('\n', "\r\n"), 50);
        let lf = parse(FIXTURE, 50);
        assert_eq!(sigs(&r), sigs(&lf));
        assert!(parse("", 50).symbols.is_empty());
        assert!(parse("# c\n\n", 50).symbols.is_empty());
        assert!(parse("all: x \\", 50).symbols.len() == 1);
    }

    #[test]
    fn limit_keeps_total() {
        let r = parse(FIXTURE, 4);
        assert_eq!((r.symbols.len(), r.total), (4, 10));
    }

    #[test]
    fn grouped_targets_continued_target_lists_and_indented_statements() {
        let r = parse(
            "a b &: c\nx \\\n y: z\nB := 0\nifeq ($(A),1)\n\tINDENTED := 1\nendif\n\nrule: dep\n\tcmd\nifeq ($(A),1)\n\tSTILL_RECIPE = 1\nendif\nAFTER := 2\n\tTAB_AFTER_ASSIGN := 3\n",
            50,
        );
        assert_eq!(
            sigs(&r),
            [
                ("a", "a = c"),
                ("b", "b = c"),
                ("x", "x = z"),
                ("y", "y = z"),
                ("B", "B = 0"),
                ("INDENTED", "INDENTED = 1"),
                ("rule", "rule = dep"),
                ("AFTER", "AFTER = 2"),
                ("TAB_AFTER_ASSIGN", "TAB_AFTER_ASSIGN = 3"),
            ]
        );
        assert_eq!(lines(&r, "x"), (2, 3));
    }

    #[test]
    fn lone_cr_is_a_line_break() {
        // Numbering is `str::lines()`; text after a lone `\r` is cut.
        let r = parse("A = 1\rB = leak\nC = 2 \rx\nall: x\r\n\tcmd\n", 50);
        assert_eq!(
            sigs(&r),
            [("A", "A = 1"), ("C", "C = 2"), ("all", "all = x")]
        );
        assert_eq!(lines(&r, "C"), (2, 2));
        assert_eq!(lines(&r, "all"), (3, 4));
        assert!(r.has_errors);
        assert!(!parse("A = 1\r\nB = 2\r\n", 50).has_errors);
    }

    #[test]
    fn assignment_shows_only_its_first_physical_line() {
        let r = parse(
            "DEPLOY = curl -X POST \\\n  -H \"Authorization: Bearer abc123\" \\\n  https://x.example\nNEXT = 1\n",
            50,
        );
        assert_eq!(
            sigs(&r),
            [("DEPLOY", "DEPLOY = curl -X POST"), ("NEXT", "NEXT = 1")]
        );
        assert_eq!(lines(&r, "DEPLOY"), (1, 3));
    }

    #[test]
    fn recipe_prefix_is_honoured() {
        let src = ".RECIPEPREFIX = >\nall: x\n>echo usage: make x\n>y: z\nnext: q\n>cmd\n";
        let r = parse(src, 50);
        assert_eq!(
            sigs(&r),
            [
                (".RECIPEPREFIX", ".RECIPEPREFIX = >"),
                ("all", "all = x"),
                ("next", "next = q")
            ]
        );
        assert_eq!(lines(&r, "all"), (2, 4));
        assert_eq!(lines(&r, "next"), (5, 6));
        // Without the assignment `>`-lines are statements, tabs are recipes.
        let r = parse("all: x\n\tcmd: y\n", 50);
        assert_eq!(sigs(&r), [("all", "all = x")]);
    }
}
