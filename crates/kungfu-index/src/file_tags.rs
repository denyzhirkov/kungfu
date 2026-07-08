//! Heuristic file tags derived at index time from path conventions, import
//! targets, and symbol composition.
//!
//! Precision over recall: every rule fires only on an unambiguous signal
//! (exact path segment, exact import segment), because a confidently wrong
//! tag is worse than none. Tags are structurally heuristic — the authored
//! `FileEntry.purpose` is the trusted layer and always wins on conflict.

use kungfu_parse::RawImport;
use kungfu_types::symbol::{Symbol, SymbolKind};

/// Import segments that identify a database / datastore driver or ORM.
const DATABASE_IMPORTS: &[&str] = &[
    // Rust
    "sqlx",
    "diesel",
    "sea_orm",
    "rusqlite",
    // Python
    "sqlalchemy",
    "psycopg2",
    "psycopg",
    "pymongo",
    "sqlite3",
    // JS/TS
    "mongoose",
    "typeorm",
    "prisma",
    "sequelize",
    "knex",
    "pg",
    "mysql",
    "mysql2",
    "mongodb",
    // Go: database/sql; Java: java.sql / jdbc
    "sql",
    "jdbc",
    "hibernate",
    "gorm",
    // .NET
    "entityframeworkcore",
    // Cross-ecosystem datastores
    "redis",
];

/// Import segments that identify an HTTP server / routing framework.
const HTTP_IMPORTS: &[&str] = &[
    // Rust
    "axum",
    "actix_web",
    "warp",
    "rocket",
    "hyper",
    // JS/TS
    "express",
    "fastify",
    "koa",
    "hapi",
    // Python
    "flask",
    "fastapi",
    "bottle",
    // Go (framework-unique segments only; "echo"/"chi"/"fiber" are too generic)
    "gin",
    // JVM
    "servlet",
    "ktor",
    // .NET
    "aspnetcore",
];

/// Import segments that identify a CLI argument-parsing framework.
const CLI_IMPORTS: &[&str] = &["clap", "structopt", "argparse", "click", "cobra", "yargs"];

/// Import segments that identify authentication / crypto-for-auth libraries.
const AUTH_IMPORTS: &[&str] = &[
    "jsonwebtoken",
    "jwt",
    "passport",
    "oauth2",
    "bcrypt",
    "argon2",
];

/// Path segments (directories or file stems) that mark auth-related code.
const AUTH_PATH_SEGMENTS: &[&str] = &["auth", "authentication", "authorization"];

/// File stems that mark configuration modules.
const CONFIG_STEMS: &[&str] = &["config", "configuration", "settings", "conf"];

/// Derive heuristic tags for one file. `imports`/`symbols` are empty for
/// files that were not parsed (non-code, name-only); path rules still apply.
pub fn derive_tags(rel_path: &str, imports: &[RawImport], symbols: &[Symbol]) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    let add = |tag: &str, tags: &mut Vec<String>| {
        if !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_string());
        }
    };

    let lower = rel_path.to_lowercase();
    let segments: Vec<&str> = lower.split('/').collect();
    let file_name = segments.last().copied().unwrap_or("");
    let stem = file_name.split('.').next().unwrap_or("");
    let dirs = &segments[..segments.len().saturating_sub(1)];

    // -- path rules ---------------------------------------------------------
    if is_test_path(dirs, stem, file_name) {
        add("tests", &mut tags);
    }
    if is_entrypoint_file(file_name) {
        add("entrypoint", &mut tags);
    }
    if CONFIG_STEMS.contains(&stem) {
        add("config", &mut tags);
    }
    if dirs.iter().any(|d| AUTH_PATH_SEGMENTS.contains(d)) || AUTH_PATH_SEGMENTS.contains(&stem) {
        add("auth", &mut tags);
    }

    // -- import rules (absolute imports only — a relative `./database` is the
    //    project's own module, not evidence of a driver dependency) ----------
    for import in imports {
        if import.path.starts_with('.') {
            continue;
        }
        let lower_path = import.path.to_lowercase();
        for seg in import_segments(&lower_path) {
            if DATABASE_IMPORTS.contains(&seg) {
                add("database", &mut tags);
            }
            if HTTP_IMPORTS.contains(&seg) {
                add("http", &mut tags);
            }
            if CLI_IMPORTS.contains(&seg) {
                add("cli", &mut tags);
            }
            if AUTH_IMPORTS.contains(&seg) {
                add("auth", &mut tags);
            }
        }
    }

    // -- symbol composition: a module of pure type definitions ---------------
    if symbols.len() >= 3 && symbols.iter().all(|s| is_type_definition(s.kind)) {
        add("types", &mut tags);
    }

    tags
}

/// Test markers: test directories (this also covers the JVM `src/test/java`
/// layout), `test_*` / `*_test` stems, and `.test.` / `.spec.` infixes.
/// A bare `*test` suffix is deliberately NOT a marker — "contest.rs" is not a test.
fn is_test_path(dirs: &[&str], stem: &str, file_name: &str) -> bool {
    dirs.iter().any(|d| {
        matches!(
            *d,
            "tests" | "test" | "__tests__" | "spec" | "specs" | "testdata"
        )
    }) || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || file_name.contains(".test.")
        || file_name.contains(".spec.")
}

fn is_entrypoint_file(file_name: &str) -> bool {
    matches!(
        file_name,
        "main.rs"
            | "main.go"
            | "main.py"
            | "main.c"
            | "main.cc"
            | "main.cpp"
            | "main.ts"
            | "main.js"
            | "program.cs"
            | "__main__.py"
    )
}

fn is_type_definition(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Struct
            | SymbolKind::Enum
            | SymbolKind::EnumVariant
            | SymbolKind::Interface
            | SymbolKind::TypeAlias
            | SymbolKind::Trait
            | SymbolKind::Constant
            | SymbolKind::Field
    )
}

/// Segments of a (pre-lowercased) import path, split on the separators used
/// across the supported languages (`::`, `/`, `.`, `-`).
fn import_segments(path: &str) -> Vec<&str> {
    path.split(['/', '.', ':', '-'])
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imp(path: &str) -> RawImport {
        RawImport {
            path: path.to_string(),
            names: vec![],
            line: 1,
        }
    }

    #[test]
    fn path_rules() {
        assert_eq!(derive_tags("src/main.rs", &[], &[]), vec!["entrypoint"]);
        assert_eq!(derive_tags("tests/api_test.rs", &[], &[]), vec!["tests"]);
        assert_eq!(
            derive_tags("src/components/__tests__/Button.test.tsx", &[], &[]),
            vec!["tests"]
        );
        assert_eq!(derive_tags("src/config.ts", &[], &[]), vec!["config"]);
        assert_eq!(derive_tags("src/auth/session.rs", &[], &[]), vec!["auth"]);
        // "author" must not match "auth"
        assert!(derive_tags("src/authors.rs", &[], &[]).is_empty());
        assert!(derive_tags("src/handlers.rs", &[], &[]).is_empty());
    }

    #[test]
    fn import_rules() {
        assert_eq!(
            derive_tags("src/db.rs", &[imp("sqlx::postgres")], &[]),
            vec!["database"]
        );
        assert_eq!(
            derive_tags("src/store.go", &[imp("database/sql")], &[]),
            vec!["database"]
        );
        assert_eq!(
            derive_tags("src/Repo.java", &[imp("java.sql.Connection")], &[]),
            vec!["database"]
        );
        assert_eq!(
            derive_tags("src/server.ts", &[imp("express")], &[]),
            vec!["http"]
        );
        assert_eq!(
            derive_tags("src/app.py", &[imp("flask")], &[]),
            vec!["http"]
        );
        assert_eq!(
            derive_tags("src/cli_args.rs", &[imp("clap")], &[]),
            vec!["cli"]
        );
        assert_eq!(
            derive_tags("src/token.ts", &[imp("jsonwebtoken")], &[]),
            vec!["auth"]
        );
        // Relative imports carry no dependency evidence.
        assert!(derive_tags("src/handlers.ts", &[imp("./database")], &[]).is_empty());
        // Unrelated absolute import.
        assert!(derive_tags("src/util.rs", &[imp("serde::Deserialize")], &[]).is_empty());
    }

    #[test]
    fn combined_and_deduped() {
        let tags = derive_tags(
            "src/auth/login.ts",
            &[imp("express"), imp("jsonwebtoken"), imp("passport")],
            &[],
        );
        assert_eq!(tags, vec!["auth", "http"]);
    }

    #[test]
    fn types_rule() {
        use kungfu_types::symbol::Span;
        let sym = |name: &str, kind: SymbolKind| Symbol {
            id: format!("s:{name}"),
            file_id: "f:1".into(),
            name: name.into(),
            kind,
            language: "rust".into(),
            path: "src/types.rs".into(),
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
            doc_summary: None,
        };
        let all_types = vec![
            sym("Budget", SymbolKind::Struct),
            sym("Intent", SymbolKind::Enum),
            sym("PacketId", SymbolKind::TypeAlias),
        ];
        assert_eq!(derive_tags("src/types.rs", &[], &all_types), vec!["types"]);

        let with_fn = vec![
            sym("Budget", SymbolKind::Struct),
            sym("Intent", SymbolKind::Enum),
            sym("resolve", SymbolKind::Function),
        ];
        assert!(derive_tags("src/types.rs", &[], &with_fn).is_empty());
        // Fewer than 3 symbols is not enough evidence.
        assert!(derive_tags("src/types.rs", &[], &all_types[..2]).is_empty());
    }
}
