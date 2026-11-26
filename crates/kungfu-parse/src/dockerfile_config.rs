//! Dockerfile config symbols: stages (`FROM … AS name`) as `ConfigBlock`s and their instructions as `ConfigKey`s. Follows the `config_symbols` contract;
//! `json_config` is the reference.
//!
//! `ARG` and `ENV` contribute variable names only (never values: `ENV` often
//! carries secrets). `RUN`/`COPY`/`ADD`/`LABEL` bodies are ignored.

use crate::config_symbols::{ConfigParent, ConfigSymbols, ConfigValue};
use kungfu_types::symbol::{Span, SymbolKind};

/// Instructions shown with the rest of their first line.
const SHOWN: &[&str] = &["EXPOSE", "ENTRYPOINT", "CMD", "WORKDIR", "USER"];

struct Instruction<'a> {
    keyword: &'a str,
    /// The rest of the first physical line, without a trailing `\`.
    first_line: &'a str,
    /// All continuation lines joined with single spaces.
    joined: String,
    span: Span,
}

/// Push this file's config symbols into `out`, scanning `source` directly
/// (no tree-sitter grammar).
pub fn extract(source: &str, out: &mut ConfigSymbols) {
    let source = crate::env_config::strip_bom(source);
    if crate::env_config::drops_text_at_lone_cr(source) {
        out.mark_incomplete();
    }
    let (instructions, incomplete) = instructions(source);
    if incomplete {
        out.mark_incomplete();
    }
    let lines: Vec<&str> = crate::env_config::text_lines(source).collect();
    let total_lines = lines.len();
    let mut stage: Option<ConfigParent> = None;
    let mut stages = 0usize;
    for (at, ins) in instructions.iter().enumerate() {
        let keyword = ins.keyword.to_ascii_uppercase();
        if keyword == "FROM" {
            let tokens = tokens(&ins.joined);
            let mut args = tokens.iter().copied().filter(|t| !t.starts_with("--"));
            let image = args.next().unwrap_or("");
            let alias = match (args.next(), args.next()) {
                (Some(as_kw), Some(name)) if as_kw.eq_ignore_ascii_case("as") => Some(name),
                _ => None,
            };
            let key = alias.map_or_else(|| format!("stage{stages}"), str::to_string);
            stages += 1;
            let end_line = instructions[at + 1..]
                .iter()
                .find(|next| next.keyword.eq_ignore_ascii_case("FROM"))
                .map_or(total_lines, |next| next.span.start_line - 1)
                .max(ins.span.end_line);
            let span = Span {
                end_line,
                end_col: lines.get(end_line - 1).map_or(0, |l| l.len()),
                ..ins.span.clone()
            };
            let shown = if image.is_empty() {
                ConfigValue::None
            } else {
                ConfigValue::Scalar(image)
            };
            stage = out.push(None, &key, SymbolKind::ConfigBlock, shown, span);
            continue;
        }
        if stages > 0 && stage.is_none() {
            continue;
        }
        if keyword == "ARG" || keyword == "ENV" {
            for name in variable_names(&ins.joined, keyword == "ENV") {
                out.push(
                    stage.as_ref(),
                    name,
                    SymbolKind::ConfigKey,
                    ConfigValue::None,
                    ins.span.clone(),
                );
            }
        } else if stages > 0 && SHOWN.contains(&keyword.as_str()) {
            let shown = if ins.first_line.is_empty() {
                ConfigValue::None
            } else {
                ConfigValue::Scalar(ins.first_line)
            };
            out.push(
                stage.as_ref(),
                &keyword,
                SymbolKind::ConfigKey,
                shown,
                ins.span.clone(),
            );
        }
    }
}

/// Logical instructions: continuation lines folded in, comments and blank
/// lines dropped, here-document bodies of `RUN`/`COPY`/`ADD` consumed. The flag
/// is set when a here-document never closes (it swallowed the rest of the file).
fn instructions(source: &str) -> (Vec<Instruction<'_>>, bool) {
    let mut out = Vec::new();
    let mut incomplete = false;
    let mut lines = crate::env_config::text_lines(source).enumerate();
    while let Some((idx, line)) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (keyword, rest) = trimmed
            .split_once(char::is_whitespace)
            .unwrap_or((trimmed, ""));
        let first_line = strip_continuation(rest.trim());
        let mut joined = first_line.to_string();
        let mut span = Span {
            start_line: idx + 1,
            end_line: idx + 1,
            start_col: 0,
            end_col: line.len(),
        };
        let mut continued = has_continuation(trimmed);
        while continued {
            let Some((next_idx, next)) = lines.next() else {
                break;
            };
            let next = next.trim();
            // Comments and blank lines inside a continuation are skipped.
            if next.is_empty() || next.starts_with('#') {
                continue;
            }
            joined.push(' ');
            joined.push_str(strip_continuation(next));
            span.end_line = next_idx + 1;
            span.end_col = next.len();
            continued = has_continuation(next);
        }
        if ["RUN", "COPY", "ADD"]
            .iter()
            .any(|k| keyword.eq_ignore_ascii_case(k))
        {
            for terminator in heredoc_terminators(&joined) {
                let mut closed = false;
                for (body_idx, body) in lines.by_ref() {
                    if body.trim() == terminator {
                        span.end_line = body_idx + 1;
                        span.end_col = body.len();
                        closed = true;
                        break;
                    }
                }
                incomplete |= !closed;
            }
        }
        out.push(Instruction {
            keyword,
            first_line,
            joined,
            span,
        });
    }
    (out, incomplete)
}

fn has_continuation(line: &str) -> bool {
    line.ends_with('\\')
}

fn strip_continuation(line: &str) -> &str {
    line.strip_suffix('\\').unwrap_or(line).trim_end()
}

/// Delimiters of `<<EOF`, `<<-EOF`, `<<'EOF'` here-documents on an instruction.
fn heredoc_terminators(joined: &str) -> Vec<&str> {
    tokens(joined)
        .into_iter()
        .filter_map(|t| t.strip_prefix("<<"))
        .map(|t| t.trim_start_matches('-').trim_matches(['"', '\'']))
        .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .collect()
}

/// `ARG NAME[=default]` and both `ENV` forms: `ENV NAME value…` and
/// `ENV A=1 B=2`.
fn variable_names(joined: &str, is_env: bool) -> Vec<&str> {
    let tokens = tokens(joined);
    let pairs = tokens.first().is_some_and(|t| t.contains('='));
    let wanted = if is_env && !pairs { 1 } else { tokens.len() };
    tokens
        .into_iter()
        .take(wanted)
        .filter(|t| !is_env || !pairs || t.contains('='))
        .map(|t| t.split_once('=').map_or(t, |(name, _)| name))
        .filter(|name| !name.is_empty() && !name.starts_with(['"', '\'']))
        .collect()
}

/// Whitespace-separated words, with quoted and backslash-escaped text kept
/// inside one word.
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            start.get_or_insert(at);
        } else if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            start.get_or_insert(at);
        } else if c.is_whitespace() {
            if let Some(from) = start.take() {
                out.extend(text.get(from..at));
            }
        } else {
            start.get_or_insert(at);
        }
    }
    if let Some(from) = start {
        out.extend(text.get(from..));
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::{ConfigParseResult, Parser};
    use kungfu_types::file::Language;
    use kungfu_types::symbol::SymbolKind;

    const FIXTURE: &str = include_str!("../tests/fixtures/config/Dockerfile");

    fn parse(src: &str, limit: usize) -> ConfigParseResult {
        Parser::new()
            .parse_config(src, Language::Dockerfile, "f:1", "Dockerfile", limit)
            .unwrap()
    }

    fn names(r: &ConfigParseResult) -> Vec<&str> {
        r.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    fn find<'r>(r: &'r ConfigParseResult, name: &str) -> &'r kungfu_types::symbol::Symbol {
        r.symbols.iter().find(|s| s.name == name).unwrap()
    }

    #[test]
    fn multi_stage_fixture() {
        let r = parse(FIXTURE, 50);
        assert_eq!(
            names(&r),
            [
                "NODE_VERSION",
                "build",
                "build.WORKDIR",
                "build.BUILD_ID",
                "build.NODE_ENV",
                "build.API_TOKEN",
                "build.LOG_LEVEL",
                "build.EXPOSE",
                "stage1",
                "stage1.LEGACY_STYLE",
                "stage1.USER",
                "stage1.CMD",
            ]
        );
        let sig = |n: &str| find(&r, n).signature.clone().unwrap();
        assert_eq!(sig("NODE_VERSION"), "NODE_VERSION");
        assert_eq!(find(&r, "NODE_VERSION").kind, SymbolKind::ConfigKey);
        assert!(find(&r, "NODE_VERSION").parent_symbol_id.is_none());
        assert_eq!(sig("build"), "build = node:${NODE_VERSION}-alpine");
        assert_eq!(sig("stage1"), "stage1 = nginx:1.25");
        assert_eq!(sig("build.WORKDIR"), "build.WORKDIR = /app");
        assert_eq!(sig("build.EXPOSE"), "build.EXPOSE = 3000");
        assert_eq!(sig("stage1.USER"), "stage1.USER = nginx");
        assert_eq!(sig("stage1.CMD"), "stage1.CMD = [\"nginx\",");
        // ARG / ENV: names only.
        assert_eq!(sig("build.API_TOKEN"), "build.API_TOKEN");
        assert_eq!(sig("build.NODE_ENV"), "build.NODE_ENV");
        assert!(r
            .symbols
            .iter()
            .all(|s| !s.signature.as_deref().unwrap().contains("abc")));
        let build = find(&r, "build");
        assert_eq!(build.kind, SymbolKind::ConfigBlock);
        assert_eq!((build.span.start_line, build.span.end_line), (4, 14));
        let stage1 = find(&r, "stage1");
        assert_eq!((stage1.span.start_line, stage1.span.end_line), (15, 20));
        let workdir = find(&r, "build.WORKDIR");
        assert_eq!(workdir.parent_symbol_id.as_deref(), Some(build.id.as_str()));
        assert_eq!(workdir.kind, SymbolKind::ConfigKey);
        let env = find(&r, "build.NODE_ENV");
        assert_eq!((env.span.start_line, env.span.end_line), (7, 8));
        let cmd = find(&r, "stage1.CMD");
        assert_eq!((cmd.span.start_line, cmd.span.end_line), (19, 20));
        assert_eq!(r.total, 12);
    }

    #[test]
    fn instructions_are_case_insensitive_and_runs_ignored() {
        let r = parse(
            "from alpine\nrun echo hi\nlabel a=b\nworkdir /x\nExpose 80 443\nadd a b\n",
            50,
        );
        assert_eq!(names(&r), ["stage0", "stage0.WORKDIR", "stage0.EXPOSE"]);
        assert_eq!(
            find(&r, "stage0.EXPOSE").signature.as_deref(),
            Some("stage0.EXPOSE = 80 443")
        );
    }

    #[test]
    fn unaliased_stages_are_numbered_by_position() {
        let r = parse("FROM a\nFROM b AS two\nFROM c\n", 50);
        assert_eq!(names(&r), ["stage0", "two", "stage2"]);
        assert_eq!(find(&r, "stage0").span.end_line, 1);
        assert_eq!(find(&r, "stage2").span.end_line, 3);
    }

    #[test]
    fn env_forms_and_quoting() {
        let r = parse(
            "FROM a\nENV LEGACY some value=with equals\nENV A=\"x y\" B='z w' C=3\nARG ONE\nARG TWO=2\n",
            50,
        );
        assert_eq!(
            names(&r),
            [
                "stage0",
                "stage0.LEGACY",
                "stage0.A",
                "stage0.B",
                "stage0.C",
                "stage0.ONE",
                "stage0.TWO"
            ]
        );
    }

    #[test]
    fn heredoc_bodies_do_not_leak_instructions() {
        let r = parse(
            "FROM a\nRUN <<EOF\nFROM fake\nEXPOSE 1\nEOF\nEXPOSE 2\n",
            50,
        );
        assert_eq!(names(&r), ["stage0", "stage0.EXPOSE"]);
        assert_eq!(find(&r, "stage0.EXPOSE").span.start_line, 6);
    }

    #[test]
    fn comments_inside_continuations_and_unterminated_input() {
        let r = parse(
            "FROM a\nCMD [\"a\", \\\n# note\n\n  \"b\"]\nEXPOSE 1\nENV X=1 \\",
            50,
        );
        assert_eq!(
            names(&r),
            ["stage0", "stage0.CMD", "stage0.EXPOSE", "stage0.X"]
        );
        assert_eq!(find(&r, "stage0.CMD").span.end_line, 5);
        let r = parse("RUN <<EOF\nnever closed\n", 50);
        assert!(r.symbols.is_empty());
        assert!(r.has_errors);
        assert!(!parse(FIXTURE, 50).has_errors);
    }

    #[test]
    fn crlf_and_empty() {
        let r = parse(&FIXTURE.replace('\n', "\r\n"), 50);
        let lf = parse(FIXTURE, 50);
        assert_eq!(names(&r), names(&lf));
        assert_eq!(
            find(&r, "build").span.end_line,
            find(&lf, "build").span.end_line
        );
        assert!(parse("", 50).symbols.is_empty());
        assert!(parse("\n# only a comment\n", 50).symbols.is_empty());
    }

    #[test]
    fn limit_keeps_total() {
        let r = parse(FIXTURE, 3);
        assert_eq!(r.symbols.len(), 3);
        assert_eq!(r.total, 12);
    }

    #[test]
    fn stage_end_column_and_lone_cr() {
        let r = parse("FROM a\nUSER bob\n\nFROM b\n", 50);
        let stage0 = find(&r, "stage0");
        assert_eq!((stage0.span.end_line, stage0.span.end_col), (3, 0));
        let r = parse("FROM a\nUSER bob\nFROM b\n", 50);
        assert_eq!(find(&r, "stage0").span.end_col, "USER bob".len());
        // Numbering is `str::lines()`; text after a lone `\r` is cut.
        let r = parse("FROM a\rEXPOSE 1\nUSER u\rleak\n", 50);
        assert_eq!(names(&r), ["stage0", "stage0.USER"]);
        assert_eq!(find(&r, "stage0.USER").span.start_line, 2);
        assert_eq!(
            find(&r, "stage0.USER").signature.as_deref(),
            Some("stage0.USER = u")
        );
        assert!(r.has_errors);
        assert!(!parse("FROM a\r\nUSER u\r\n", 50).has_errors);
    }
}
