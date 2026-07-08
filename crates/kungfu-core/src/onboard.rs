use crate::helpers::detect_primary_language;
use crate::types::OnboardInfo;
use crate::KungfuService;
use anyhow::Result;
use kungfu_types::file::FileEntry;
use kungfu_types::symbol::Symbol;
use std::collections::HashMap;
use std::path::Path;

impl KungfuService {
    /// Generate a project onboarding summary: architecture, patterns, entrypoints, naming.
    pub fn onboard(&self) -> Result<OnboardInfo> {
        self.ensure_fresh_index()?;
        let store = self.store();
        let files = store.load_files()?;
        let symbols = store.load_symbols()?;
        let relations = store.relations_arc()?;

        // Languages
        let mut lang_counts: HashMap<String, usize> = HashMap::new();
        for f in &files {
            if let Some(ref lang) = f.language {
                *lang_counts.entry(lang.clone()).or_default() += 1;
            }
        }
        let mut languages: Vec<(String, usize)> = lang_counts.into_iter().collect();
        languages.sort_by(|a, b| b.1.cmp(&a.1));
        let primary_language = detect_primary_language(&files);

        // Top directories
        let mut dir_counts: HashMap<String, usize> = HashMap::new();
        for f in &files {
            if let Some(dir) = Path::new(&f.path).parent() {
                let dir_str = dir.to_string_lossy().to_string();
                if !dir_str.is_empty() {
                    let top = dir_str.split('/').next().unwrap_or(&dir_str).to_string();
                    *dir_counts.entry(top).or_default() += 1;
                }
            }
        }
        let mut top_dirs: Vec<(String, usize)> = dir_counts.into_iter().collect();
        top_dirs.sort_by(|a, b| b.1.cmp(&a.1));
        top_dirs.truncate(15);

        // Entrypoints
        let entrypoints: Vec<String> = files
            .iter()
            .filter(|f| {
                let p = &f.path;
                p.ends_with("main.rs")
                    || p.ends_with("lib.rs")
                    || p.ends_with("index.ts")
                    || p.ends_with("index.js")
                    || p.ends_with("main.py")
                    || p.ends_with("main.go")
                    || p.ends_with("app.ts")
                    || p.ends_with("app.js")
                    || p == "package.json"
                    || p == "Cargo.toml"
                    || p == "go.mod"
                    || p == "pyproject.toml"
            })
            .map(|f| match f.purpose {
                Some(ref purpose) => format!("{} — {}", f.path, purpose),
                None => f.path.clone(),
            })
            .collect();

        // Architecture detection
        let architecture = detect_architecture(&files, &symbols);

        // Key symbols (most connected)
        let mut symbol_connections: HashMap<String, usize> = HashMap::new();
        for r in relations.iter() {
            *symbol_connections.entry(r.source_id.clone()).or_default() += 1;
            *symbol_connections.entry(r.target_id.clone()).or_default() += 1;
        }
        let symbol_map: HashMap<&str, &Symbol> =
            symbols.iter().map(|s| (s.id.as_str(), s)).collect();
        let mut connected: Vec<(&str, usize)> = symbol_connections
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        connected.sort_by(|a, b| b.1.cmp(&a.1));
        let key_symbols: Vec<String> = connected
            .iter()
            .take(10)
            .filter_map(|(id, _)| {
                symbol_map.get(id).map(|s| {
                    if let Some(ref sig) = s.signature {
                        format!("[{}] {}", s.path, sig)
                    } else {
                        format!("[{}] {}", s.path, s.name)
                    }
                })
            })
            .collect();

        // Naming style detection
        let naming_style = detect_naming_style(&symbols);

        // Test pattern detection
        let test_pattern = detect_test_pattern(&files);

        // Glossary: agent-curated terms from annotations + vocabulary mined
        // from identifiers. Computed on demand — nothing persisted.
        let agent_terms: std::collections::BTreeMap<String, String> = self
            .store()
            .annotations()
            .load()
            .unwrap_or_default()
            .into_values()
            .flat_map(|a| a.terms)
            .collect();
        let project_name = self.project.meta.name.clone();
        let glossary =
            crate::glossary::build_glossary(&symbols, &agent_terms, &[project_name.as_str()], 15);

        Ok(OnboardInfo {
            project_name,
            languages,
            primary_language,
            architecture,
            top_dirs,
            entrypoints,
            key_symbols,
            naming_style,
            test_pattern,
            glossary,
            total_files: files.len(),
            total_symbols: symbols.len(),
        })
    }
}

fn detect_architecture(files: &[FileEntry], _symbols: &[Symbol]) -> String {
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

    // Check for common patterns
    let has_src = paths.iter().any(|p| p.starts_with("src/"));
    let has_crates = paths.iter().any(|p| p.starts_with("crates/"));
    let has_packages = paths.iter().any(|p| p.starts_with("packages/"));
    let has_cmd = paths.iter().any(|p| p.starts_with("cmd/"));
    let has_internal = paths.iter().any(|p| p.starts_with("internal/"));
    let has_controllers = paths
        .iter()
        .any(|p| p.contains("controller") || p.contains("handler"));
    let has_services = paths.iter().any(|p| p.contains("service"));
    let has_models = paths
        .iter()
        .any(|p| p.contains("model") || p.contains("entity"));
    let has_routes = paths
        .iter()
        .any(|p| p.contains("route") || p.contains("router"));
    let has_components = paths.iter().any(|p| p.contains("component"));

    if has_crates || has_packages {
        "Workspace / Monorepo — multiple crates/packages".to_string()
    } else if has_cmd && has_internal {
        "Go-style — cmd/ for binaries, internal/ for packages".to_string()
    } else if has_controllers && has_services && has_models {
        "MVC / Layered — controllers, services, models".to_string()
    } else if has_routes && has_services {
        "Service-oriented — routes + services".to_string()
    } else if has_components {
        "Component-based — UI components".to_string()
    } else if has_src {
        "Standard — src/ based".to_string()
    } else {
        "Flat / Custom".to_string()
    }
}

fn detect_naming_style(symbols: &[Symbol]) -> String {
    let mut snake = 0;
    let mut camel = 0;
    let mut pascal = 0;

    for s in symbols {
        let name = &s.name;
        if name.contains('_') && name == &name.to_lowercase() {
            snake += 1;
        } else if name
            .chars()
            .next()
            .map(|c| c.is_lowercase())
            .unwrap_or(false)
            && name.contains(char::is_uppercase)
        {
            camel += 1;
        } else if name
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
        {
            pascal += 1;
        }
    }

    let total = snake + camel + pascal;
    if total == 0 {
        return "unknown".to_string();
    }

    let mut styles = vec![];
    if snake > total / 4 {
        styles.push(format!("snake_case ({}%)", snake * 100 / total));
    }
    if camel > total / 4 {
        styles.push(format!("camelCase ({}%)", camel * 100 / total));
    }
    if pascal > total / 4 {
        styles.push(format!("PascalCase ({}%)", pascal * 100 / total));
    }

    if styles.is_empty() {
        "mixed".to_string()
    } else {
        styles.join(", ")
    }
}

fn detect_test_pattern(files: &[FileEntry]) -> String {
    let test_files: Vec<&str> = files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| p.contains("test") || p.contains("spec"))
        .collect();

    if test_files.is_empty() {
        return "no tests detected".to_string();
    }

    let co_located = test_files
        .iter()
        .any(|p| !p.starts_with("test") && !p.starts_with("tests") && !p.starts_with("__tests__"));
    let separate_dir = test_files
        .iter()
        .any(|p| p.starts_with("test") || p.starts_with("tests") || p.starts_with("__tests__"));
    let spec_style = test_files.iter().any(|p| p.contains(".spec."));
    let test_suffix = test_files
        .iter()
        .any(|p| p.contains(".test.") || p.contains("_test."));

    let mut patterns = vec![];
    if co_located {
        patterns.push("co-located");
    }
    if separate_dir {
        patterns.push("tests/ directory");
    }
    if spec_style {
        patterns.push("*.spec.* naming");
    }
    if test_suffix {
        patterns.push("*.test.* / *_test.* naming");
    }

    format!("{} test files — {}", test_files.len(), patterns.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn architecture_detection_workspace() {
        let files = vec![FileEntry {
            id: "1".into(),
            path: "crates/core/src/lib.rs".into(),
            extension: Some("rs".into()),
            language: Some("rust".into()),
            size: 100,
            hash: "h1".into(),
            indexed_at: Default::default(),
            tags: vec![],
            purpose: None,
            purpose_source: None,
        }];
        let result = detect_architecture(&files, &[]);
        assert!(result.contains("Workspace"), "got: {}", result);
    }

    #[test]
    fn architecture_detection_mvc() {
        let files = vec![
            FileEntry {
                id: "1".into(),
                path: "src/controller/auth.ts".into(),
                extension: Some("ts".into()),
                language: Some("typescript".into()),
                size: 100,
                hash: "h1".into(),
                indexed_at: Default::default(),
                tags: vec![],
                purpose: None,
                purpose_source: None,
            },
            FileEntry {
                id: "2".into(),
                path: "src/service/auth.ts".into(),
                extension: Some("ts".into()),
                language: Some("typescript".into()),
                size: 100,
                hash: "h2".into(),
                indexed_at: Default::default(),
                tags: vec![],
                purpose: None,
                purpose_source: None,
            },
            FileEntry {
                id: "3".into(),
                path: "src/model/user.ts".into(),
                extension: Some("ts".into()),
                language: Some("typescript".into()),
                size: 100,
                hash: "h3".into(),
                indexed_at: Default::default(),
                tags: vec![],
                purpose: None,
                purpose_source: None,
            },
        ];
        let result = detect_architecture(&files, &[]);
        assert!(
            result.contains("MVC") || result.contains("Layered"),
            "got: {}",
            result
        );
    }

    #[test]
    fn naming_style_snake() {
        use kungfu_types::symbol::{Span, SymbolKind};
        let symbols = vec![
            Symbol {
                id: "1".into(),
                file_id: "f1".into(),
                name: "my_function".into(),
                kind: SymbolKind::Function,
                language: "rust".into(),
                path: "a.rs".into(),
                signature: None,
                span: Span {
                    start_line: 1,
                    end_line: 5,
                    start_col: 0,
                    end_col: 0,
                },
                parent_symbol_id: None,
                exported: true,
                visibility: None,
                doc_summary: None,
            },
            Symbol {
                id: "2".into(),
                file_id: "f1".into(),
                name: "another_func".into(),
                kind: SymbolKind::Function,
                language: "rust".into(),
                path: "a.rs".into(),
                signature: None,
                span: Span {
                    start_line: 6,
                    end_line: 10,
                    start_col: 0,
                    end_col: 0,
                },
                parent_symbol_id: None,
                exported: true,
                visibility: None,
                doc_summary: None,
            },
        ];
        let result = detect_naming_style(&symbols);
        assert!(result.contains("snake_case"), "got: {}", result);
    }

    #[test]
    fn naming_style_camel() {
        use kungfu_types::symbol::{Span, SymbolKind};
        let symbols = vec![
            Symbol {
                id: "1".into(),
                file_id: "f1".into(),
                name: "myFunction".into(),
                kind: SymbolKind::Function,
                language: "ts".into(),
                path: "a.ts".into(),
                signature: None,
                span: Span {
                    start_line: 1,
                    end_line: 5,
                    start_col: 0,
                    end_col: 0,
                },
                parent_symbol_id: None,
                exported: true,
                visibility: None,
                doc_summary: None,
            },
            Symbol {
                id: "2".into(),
                file_id: "f1".into(),
                name: "anotherFunc".into(),
                kind: SymbolKind::Function,
                language: "ts".into(),
                path: "a.ts".into(),
                signature: None,
                span: Span {
                    start_line: 6,
                    end_line: 10,
                    start_col: 0,
                    end_col: 0,
                },
                parent_symbol_id: None,
                exported: true,
                visibility: None,
                doc_summary: None,
            },
        ];
        let result = detect_naming_style(&symbols);
        assert!(result.contains("camelCase"), "got: {}", result);
    }

    #[test]
    fn test_pattern_detection() {
        let files = vec![
            FileEntry {
                id: "1".into(),
                path: "src/auth.ts".into(),
                extension: Some("ts".into()),
                language: Some("typescript".into()),
                size: 100,
                hash: "h1".into(),
                indexed_at: Default::default(),
                tags: vec![],
                purpose: None,
                purpose_source: None,
            },
            FileEntry {
                id: "2".into(),
                path: "tests/auth.test.ts".into(),
                extension: Some("ts".into()),
                language: Some("typescript".into()),
                size: 100,
                hash: "h2".into(),
                indexed_at: Default::default(),
                tags: vec![],
                purpose: None,
                purpose_source: None,
            },
        ];
        let result = detect_test_pattern(&files);
        assert!(result.contains("test"), "got: {}", result);
    }
}
