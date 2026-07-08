//! Project glossary, computed on demand — no store, no schema. Two sources,
//! merged with explicit provenance: terms curated via annotate_file (`agent`)
//! and distinctive vocabulary mined from identifiers (`doc` when a same-named
//! type definition supplies a doc summary, `usage` when only statistics exist).
//! Precision over recall: a generic-word stoplist keeps the list short and real.

use kungfu_types::symbol::{Symbol, SymbolKind};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Programming vocabulary that is common to every codebase and therefore never
/// project glossary material. Lowercase; matched against split identifier words.
const GENERIC_WORDS: &[&str] = &[
    // actions
    "get",
    "set",
    "add",
    "remove",
    "delete",
    "create",
    "update",
    "build",
    "make",
    "init",
    "new",
    "handle",
    "handler",
    "process",
    "run",
    "start",
    "stop",
    "apply",
    "extract",
    "read",
    "write",
    "load",
    "save",
    "open",
    "close",
    "parse",
    "format",
    "print",
    "push",
    "pop",
    "next",
    "prev",
    "first",
    "last",
    "begin",
    "end",
    "find",
    "search",
    "check",
    "verify",
    "validate",
    "ensure",
    "assert",
    "detect",
    "resolve",
    "match",
    "matches",
    "convert",
    "wrap",
    "unwrap",
    "clone",
    "copy",
    "merge",
    "split",
    "join",
    "filter",
    "collect",
    "iter",
    "map",
    "fold",
    "reduce",
    "sort",
    "insert",
    "clear",
    "send",
    "recv",
    "receive",
    "emit",
    "notify",
    "call",
    "invoke",
    "exec",
    "execute",
    // things
    "name",
    "names",
    "value",
    "values",
    "data",
    "item",
    "items",
    "list",
    "array",
    "string",
    "str",
    "text",
    "num",
    "number",
    "count",
    "len",
    "size",
    "index",
    "idx",
    "key",
    "keys",
    "type",
    "types",
    "kind",
    "kinds",
    "file",
    "files",
    "path",
    "paths",
    "dir",
    "line",
    "lines",
    "col",
    "row",
    "entry",
    "entries",
    "node",
    "nodes",
    "elem",
    "element",
    "child",
    "children",
    "parent",
    "root",
    "leaf",
    "tree",
    "graph",
    "edge",
    "result",
    "results",
    "res",
    "response",
    "request",
    "req",
    "output",
    "input",
    "arg",
    "args",
    "param",
    "params",
    "opt",
    "opts",
    "option",
    "options",
    "flag",
    "config",
    "conf",
    "setting",
    "settings",
    "context",
    "ctx",
    "state",
    "status",
    "meta",
    "info",
    "util",
    "utils",
    "helper",
    "helpers",
    "common",
    "core",
    "base",
    "main",
    "mod",
    "module",
    "lib",
    "error",
    "err",
    "warning",
    "warn",
    "log",
    "debug",
    "trace",
    "event",
    "events",
    "message",
    "messages",
    "msg",
    "buffer",
    "buf",
    "cache",
    "pool",
    "queue",
    "stack",
    "task",
    "tasks",
    "job",
    "jobs",
    "worker",
    "client",
    "server",
    "service",
    "manager",
    "factory",
    "provider",
    "builder",
    "wrapper",
    "adapter",
    "proxy",
    "impl",
    "internal",
    "external",
    "custom",
    "id",
    "ids",
    "uid",
    "uuid",
    "ref",
    "refs",
    "ptr",
    "self",
    "this",
    "other",
    "test",
    "tests",
    "mock",
    "fake",
    "stub",
    "fixture",
    "spec",
    "example",
    "examples",
    "version",
    "ver",
    "level",
    "mode",
    "flags",
    "mask",
    "offset",
    "range",
    "span",
    "max",
    "min",
    "sum",
    "avg",
    "total",
    "temp",
    "tmp",
    "src",
    "dst",
    "dest",
    "source",
    "target",
    "from",
    "into",
    "with",
    "for",
    "and",
    "not",
    "none",
    "null",
    "true",
    "false",
    "default",
    "empty",
    "all",
    "any",
    "each",
    "per",
    "pre",
    "post",
    "async",
    "sync",
    "await",
    "lock",
    "mutex",
    "atomic",
    "shared",
    "global",
    "local",
    "static",
    "const",
    "mut",
    "raw",
    "inner",
    "outer",
    "current",
    "old",
    "prev",
    "user",
    "users",
    "time",
    "date",
    "timestamp",
    "duration",
    "timeout",
    "retry",
    "field",
    "fields",
    "column",
    "table",
    "record",
    "row",
    "rows",
    "object",
    "objects",
    "doc",
    "docs",
    "change",
    "changes",
    "changed",
    "stat",
    "stats",
    // language builtins & keywords that leak out of identifier splitting
    "int",
    "ints",
    "uint",
    "byte",
    "bytes",
    "bool",
    "float",
    "double",
    "long",
    "short",
    "char",
    "void",
    "func",
    "var",
    "let",
    "slice",
    "vec",
    "box",
    "panic",
    "append",
    // placeholder names and auxiliary verbs (test-speak)
    "foo",
    "bar",
    "baz",
    "qux",
    "non",
    "valid",
    "invalid",
    "did",
    "does",
    "have",
    "had",
    "was",
    "will",
    "should",
    "can",
    "use",
    "used",
    "uses",
    "benchmark",
    "bench",
    "dummy",
    "sample",
    "issue",
    "issues",
    "has",
    "fmt",
    "repr",
    "function",
    "functions",
    "private",
    "public",
    "protected",
    "done",
    "ready",
];

#[derive(Debug, Clone, serde::Serialize)]
pub struct GlossaryEntry {
    pub term: String,
    /// Human meaning: agent-curated or the doc summary of a same-named type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meaning: Option<String>,
    /// "agent" (annotate_file terms) | "doc" (same-named type's doc summary)
    /// | "usage" (identifier statistics only — no definition found).
    pub source: String,
    /// Definition site (`path:line`) when a same-named type exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defined_at: Option<String>,
    /// "N symbols / M files" identifier footprint; absent for annotation-only terms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
    /// Next action when no definition exists (retrieval honesty: an undefined
    /// term names its own gap).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

struct TermStat {
    symbol_count: usize,
    files: HashSet<String>,
}

/// Mine the glossary from symbols + agent-curated terms. `limit` caps the
/// mined portion; agent terms are always included (they were curated by hand).
pub fn build_glossary(
    symbols: &[Symbol],
    agent_terms: &BTreeMap<String, String>,
    exclude: &[&str],
    limit: usize,
) -> Vec<GlossaryEntry> {
    let generic: HashSet<&str> = GENERIC_WORDS.iter().copied().collect();
    let excluded: HashSet<String> = exclude.iter().map(|e| e.to_lowercase()).collect();

    // Test symbols poison the glossary twice over: fixture vocabulary inflates
    // the counts and fixture types hijack definition sites. Mine production
    // symbols only (same detector semantic_search uses for its demotion).
    let test_ids = crate::helpers::test_symbol_ids(symbols);
    let production: Vec<&Symbol> = symbols
        .iter()
        .filter(|s| !test_ids.contains(&s.id))
        .collect();

    // Identifier footprint per candidate word.
    let mut stats: HashMap<String, TermStat> = HashMap::new();
    for s in &production {
        let mut seen_in_symbol: HashSet<String> = HashSet::new();
        for word in kungfu_search::split_identifier(&s.name) {
            if word.len() < 3 || generic.contains(word.as_str()) || excluded.contains(&word) {
                continue;
            }
            if word.chars().any(|c| c.is_ascii_digit()) {
                continue;
            }
            if !seen_in_symbol.insert(word.clone()) {
                continue;
            }
            let stat = stats.entry(word).or_insert_with(|| TermStat {
                symbol_count: 0,
                files: HashSet::new(),
            });
            stat.symbol_count += 1;
            stat.files.insert(s.path.clone());
        }
    }

    // Fold plurals into their singular: "symbols" strengthens "symbol" instead
    // of appearing beside it; a plural whose singular is generic is itself
    // generic ("calls" → "call") and is dropped.
    let plural_keys: Vec<String> = stats
        .keys()
        .filter(|w| w.ends_with('s') && w.len() > 3)
        .cloned()
        .collect();
    for plural in plural_keys {
        let singular = &plural[..plural.len() - 1];
        if generic.contains(singular) {
            stats.remove(&plural);
        } else if stats.contains_key(singular) {
            if let Some(pstat) = stats.remove(&plural) {
                if let Some(sstat) = stats.get_mut(singular) {
                    sstat.symbol_count += pstat.symbol_count;
                    sstat.files.extend(pstat.files);
                }
            }
        }
    }

    // Definition sites: term → the best same-named type definition.
    let mut definitions: HashMap<String, &Symbol> = HashMap::new();
    for s in &production {
        if !matches!(
            s.kind,
            SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Class
                | SymbolKind::Interface
                | SymbolKind::Trait
                | SymbolKind::TypeAlias
        ) {
            continue;
        }
        let key = s.name.to_lowercase();
        // Prefer an exported definition with a doc summary.
        let better = |cand: &Symbol, cur: &Symbol| {
            (cand.doc_summary.is_some(), cand.exported) > (cur.doc_summary.is_some(), cur.exported)
        };
        match definitions.get(&key) {
            Some(cur) if !better(s, cur) => {}
            _ => {
                definitions.insert(key, s);
            }
        }
    }

    let usage_of =
        |stat: &TermStat| format!("{} symbols / {} files", stat.symbol_count, stat.files.len());

    let mut entries: Vec<GlossaryEntry> = Vec::new();
    let mut covered: HashSet<String> = HashSet::new();

    // Agent-curated terms first — unconditional, they were written on purpose.
    for (term, meaning) in agent_terms {
        let key = term.to_lowercase();
        covered.insert(key.clone());
        entries.push(GlossaryEntry {
            term: term.clone(),
            meaning: Some(meaning.clone()),
            source: "agent".to_string(),
            defined_at: definitions
                .get(&key)
                .map(|d| format!("{}:{}", d.path, d.span.start_line)),
            usage: stats.get(&key).map(usage_of),
            hint: None,
        });
    }

    // Mined terms: frequent enough to be project vocabulary, ranked by spread.
    let mut mined: Vec<(&String, &TermStat)> = stats
        .iter()
        .filter(|(word, stat)| {
            !covered.contains(*word) && stat.symbol_count >= 3 && stat.files.len() >= 2
        })
        .collect();
    mined.sort_by(|a, b| {
        let score = |s: &TermStat| s.symbol_count * s.files.len();
        score(b.1).cmp(&score(a.1)).then_with(|| a.0.cmp(b.0))
    });
    mined.truncate(limit);

    for (word, stat) in mined {
        let def = definitions.get(word);
        let meaning = def.and_then(|d| d.doc_summary.clone());
        let (source, hint) = if meaning.is_some() {
            ("doc", None)
        } else if def.is_some() {
            (
                "usage",
                Some("type exists but has no doc comment — add one or describe via annotate_file terms".to_string()),
            )
        } else {
            (
                "usage",
                Some("no definition found — describe via annotate_file terms".to_string()),
            )
        };
        entries.push(GlossaryEntry {
            term: word.clone(),
            meaning,
            source: source.to_string(),
            defined_at: def.map(|d| format!("{}:{}", d.path, d.span.start_line)),
            usage: Some(usage_of(stat)),
            hint,
        });
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::symbol::Span;

    fn sym(name: &str, kind: SymbolKind, path: &str, doc: Option<&str>) -> Symbol {
        Symbol {
            id: format!("s:{path}:{name}"),
            file_id: "f:1".into(),
            name: name.into(),
            kind,
            language: "rust".into(),
            path: path.into(),
            signature: None,
            span: Span {
                start_line: 1,
                end_line: 2,
                start_col: 0,
                end_col: 0,
            },
            parent_symbol_id: None,
            exported: true,
            visibility: None,
            doc_summary: doc.map(|d| d.to_string()),
        }
    }

    #[test]
    fn mines_distinctive_terms_with_doc_definitions() {
        let symbols = vec![
            sym(
                "Budget",
                SymbolKind::Struct,
                "src/budget.rs",
                Some("Retrieval size cap."),
            ),
            sym("resolve_budget", SymbolKind::Function, "src/lib.rs", None),
            sym("budget_levels", SymbolKind::Function, "src/budget.rs", None),
            sym("apply_budget", SymbolKind::Function, "src/rank.rs", None),
            // Generic-word symbols must not become glossary terms.
            sym("get_value", SymbolKind::Function, "src/a.rs", None),
            sym("set_value", SymbolKind::Function, "src/b.rs", None),
            sym("value_count", SymbolKind::Function, "src/c.rs", None),
        ];
        let glossary = build_glossary(&symbols, &BTreeMap::new(), &[], 10);
        let terms: Vec<&str> = glossary.iter().map(|e| e.term.as_str()).collect();
        assert!(terms.contains(&"budget"), "got: {terms:?}");
        assert!(!terms.contains(&"value"), "generic word leaked: {terms:?}");

        let budget = glossary.iter().find(|e| e.term == "budget").unwrap();
        assert_eq!(budget.source, "doc");
        assert_eq!(budget.meaning.as_deref(), Some("Retrieval size cap."));
        assert_eq!(budget.defined_at.as_deref(), Some("src/budget.rs:1"));
    }

    #[test]
    fn undefined_terms_declare_the_gap() {
        let symbols = vec![
            sym("packet_trim", SymbolKind::Function, "src/a.rs", None),
            sym("packet_fill", SymbolKind::Function, "src/b.rs", None),
            sym("send_packet", SymbolKind::Function, "src/c.rs", None),
        ];
        let glossary = build_glossary(&symbols, &BTreeMap::new(), &[], 10);
        let packet = glossary.iter().find(|e| e.term == "packet").unwrap();
        assert_eq!(packet.source, "usage");
        assert!(packet.meaning.is_none());
        assert!(packet.hint.as_deref().unwrap().contains("annotate_file"));
    }

    #[test]
    fn agent_terms_always_included_and_override_mined() {
        let symbols = vec![
            sym("mmr_pass", SymbolKind::Function, "src/a.rs", None),
            sym("mmr_score", SymbolKind::Function, "src/b.rs", None),
            sym("apply_mmr", SymbolKind::Function, "src/c.rs", None),
        ];
        let agent = BTreeMap::from([
            ("mmr".to_string(), "marginal relevance pass".to_string()),
            (
                "saga".to_string(),
                "long-running business transaction".to_string(),
            ),
        ]);
        let glossary = build_glossary(&symbols, &agent, &[], 10);
        let mmr = glossary.iter().find(|e| e.term == "mmr").unwrap();
        assert_eq!(mmr.source, "agent");
        assert_eq!(mmr.meaning.as_deref(), Some("marginal relevance pass"));
        assert!(mmr.usage.is_some(), "identifier footprint still reported");
        // Term with no identifier presence still appears (curated by hand).
        let saga = glossary.iter().find(|e| e.term == "saga").unwrap();
        assert_eq!(saga.source, "agent");
        assert!(saga.usage.is_none());
        // No duplicate "mmr" from mining.
        assert_eq!(glossary.iter().filter(|e| e.term == "mmr").count(), 1);
    }
}
