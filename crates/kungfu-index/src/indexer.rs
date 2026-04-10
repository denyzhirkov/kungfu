use anyhow::Result;
use chrono::Utc;
use kungfu_config::KungfuConfig;
use kungfu_parse::{CommentKind, Parser, RawComment, RawImport};
use kungfu_storage::JsonStore;
use kungfu_types::file::{FileEntry, Language};
use kungfu_types::memory::{MemoryEntry, MemoryKind};
use kungfu_types::relation::{Relation, RelationKind};
use kungfu_types::symbol::Symbol;
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info, warn};

use crate::scanner;

pub struct Indexer<'a> {
    root: std::path::PathBuf,
    config: KungfuConfig,
    store: &'a JsonStore,
    parser: Parser,
}

pub struct IndexStats {
    pub total_files: usize,
    pub new_files: usize,
    pub changed_files: usize,
    pub removed_files: usize,
    pub symbols_extracted: usize,
}

impl<'a> Indexer<'a> {
    pub fn new(root: &Path, config: KungfuConfig, store: &'a JsonStore) -> Self {
        Self {
            root: root.to_path_buf(),
            config,
            store,
            parser: Parser::new(),
        }
    }

    pub fn index_full(&mut self) -> Result<IndexStats> {
        info!("starting full index of {}", self.root.display());

        let paths = scanner::scan_files(&self.root, &self.config)?;
        let mut files = Vec::new();
        let mut fingerprints = HashMap::new();
        let mut all_symbols = Vec::new();
        let mut all_imports: Vec<(String, Vec<RawImport>)> = Vec::new();
        let mut all_comments: Vec<(String, Vec<RawComment>)> = Vec::new();

        for path in &paths {
            match self.index_file(path) {
                Ok((entry, symbols, imports, comments)) => {
                    fingerprints.insert(entry.path.clone(), entry.hash.clone());
                    if !imports.is_empty() {
                        all_imports.push((entry.path.clone(), imports));
                    }
                    if !comments.is_empty() {
                        all_comments.push((entry.path.clone(), comments));
                    }
                    all_symbols.extend(symbols);
                    files.push(entry);
                }
                Err(e) => {
                    warn!("failed to index {}: {}", path.display(), e);
                }
            }
        }

        let relations = Self::build_relations(&files, &all_imports);
        let mut memories = Self::build_memories(&all_comments);
        let doc_memories = self.scan_docs();
        memories.extend(doc_memories);

        let stats = IndexStats {
            total_files: files.len(),
            new_files: files.len(),
            changed_files: 0,
            removed_files: 0,
            symbols_extracted: all_symbols.len(),
        };

        self.store.save_files(&files)?;
        self.store.save_symbols(&all_symbols)?;
        self.store.save_relations(&relations)?;
        self.store.save_fingerprints(&fingerprints)?;
        self.store.save_memories(&memories)?;

        info!(
            "indexed {} files, {} symbols, {} relations, {} memories",
            stats.total_files, stats.symbols_extracted, relations.len(), memories.len()
        );
        Ok(stats)
    }

    pub fn index_incremental(&mut self) -> Result<IndexStats> {
        let old_fingerprints = self.store.load_fingerprints()?;
        let old_files = self.store.load_files()?;
        let old_symbols = self.store.load_symbols()?;

        let paths = scanner::scan_files(&self.root, &self.config)?;

        let mut new_fingerprints = HashMap::new();
        let mut new_files = Vec::new();
        let mut new_symbols = Vec::new();
        let mut all_imports: Vec<(String, Vec<RawImport>)> = Vec::new();
        let mut all_comments: Vec<(String, Vec<RawComment>)> = Vec::new();

        let mut stats = IndexStats {
            total_files: 0,
            new_files: 0,
            changed_files: 0,
            removed_files: 0,
            symbols_extracted: 0,
        };

        // Build set of current paths
        let current_paths: std::collections::HashSet<String> = paths
            .iter()
            .filter_map(|p| p.strip_prefix(&self.root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        for path in &paths {
            let rel_path = path
                .strip_prefix(&self.root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();

            let content = match std::fs::read(path) {
                Ok(c) => c,
                Err(e) => {
                    warn!("cannot read {}: {}", path.display(), e);
                    continue;
                }
            };
            let hash = blake3::hash(&content).to_hex().to_string();

            if let Some(old_hash) = old_fingerprints.get(&rel_path) {
                if *old_hash == hash {
                    // Unchanged — keep old data
                    if let Some(old_file) = old_files.iter().find(|f| f.path == rel_path) {
                        new_files.push(old_file.clone());
                        let file_symbols: Vec<_> = old_symbols
                            .iter()
                            .filter(|s| s.file_id == old_file.id)
                            .cloned()
                            .collect();
                        new_symbols.extend(file_symbols);
                    }
                    new_fingerprints.insert(rel_path, hash);
                    continue;
                }
                stats.changed_files += 1;
            } else {
                stats.new_files += 1;
            }

            match self.index_file_with_content(path, content) {
                Ok((entry, symbols, imports, comments)) => {
                    new_fingerprints.insert(entry.path.clone(), entry.hash.clone());
                    if !imports.is_empty() {
                        all_imports.push((entry.path.clone(), imports));
                    }
                    if !comments.is_empty() {
                        all_comments.push((entry.path.clone(), comments));
                    }
                    new_symbols.extend(symbols);
                    new_files.push(entry);
                }
                Err(e) => {
                    warn!("failed to index {}: {}", path.display(), e);
                }
            }
        }

        // Count removed
        for old_path in old_fingerprints.keys() {
            if !current_paths.contains(old_path) {
                stats.removed_files += 1;
            }
        }

        stats.total_files = new_files.len();
        stats.symbols_extracted = new_symbols.len();

        let relations = Self::build_relations(&new_files, &all_imports);
        let memories = Self::build_memories(&all_comments);

        self.store.save_files(&new_files)?;
        self.store.save_symbols(&new_symbols)?;
        self.store.save_relations(&relations)?;
        self.store.save_fingerprints(&new_fingerprints)?;
        self.store.save_memories(&memories)?;

        info!(
            "incremental index: {} total, {} new, {} changed, {} removed, {} symbols, {} relations",
            stats.total_files,
            stats.new_files,
            stats.changed_files,
            stats.removed_files,
            stats.symbols_extracted,
            relations.len()
        );
        Ok(stats)
    }

    /// Index only the specified files (by relative path), keeping everything else unchanged.
    pub fn index_only(&mut self, changed_paths: &[String]) -> Result<IndexStats> {
        let old_fingerprints = self.store.load_fingerprints()?;
        let old_files = self.store.load_files()?;
        let old_symbols = self.store.load_symbols()?;
        let old_relations = self.store.load_relations()?;

        let changed_set: std::collections::HashSet<&str> =
            changed_paths.iter().map(|s| s.as_str()).collect();

        let mut new_fingerprints = old_fingerprints.clone();
        let mut new_files: Vec<FileEntry> = Vec::new();
        let mut new_symbols: Vec<Symbol> = Vec::new();
        let mut all_imports: Vec<(String, Vec<RawImport>)> = Vec::new();

        let mut stats = IndexStats {
            total_files: 0,
            new_files: 0,
            changed_files: 0,
            removed_files: 0,
            symbols_extracted: 0,
        };

        // Keep unchanged files
        for f in &old_files {
            if !changed_set.contains(f.path.as_str()) {
                new_files.push(f.clone());
                let file_syms: Vec<_> = old_symbols
                    .iter()
                    .filter(|s| s.file_id == f.id)
                    .cloned()
                    .collect();
                new_symbols.extend(file_syms);
            }
        }

        let mut all_comments: Vec<(String, Vec<RawComment>)> = Vec::new();

        // Re-index changed files
        for rel_path in changed_paths {
            let abs_path = self.root.join(rel_path);
            if !abs_path.exists() {
                new_fingerprints.remove(rel_path);
                stats.removed_files += 1;
                continue;
            }

            if old_fingerprints.contains_key(rel_path) {
                stats.changed_files += 1;
            } else {
                stats.new_files += 1;
            }

            match self.index_file(&abs_path) {
                Ok((entry, symbols, imports, comments)) => {
                    new_fingerprints.insert(entry.path.clone(), entry.hash.clone());
                    stats.symbols_extracted += symbols.len();
                    if !imports.is_empty() {
                        all_imports.push((entry.path.clone(), imports));
                    }
                    if !comments.is_empty() {
                        all_comments.push((entry.path.clone(), comments));
                    }
                    new_symbols.extend(symbols);
                    new_files.push(entry);
                }
                Err(e) => {
                    warn!("failed to index {}: {}", abs_path.display(), e);
                }
            }
        }

        stats.total_files = new_files.len();

        // Merge: keep old relations for unchanged files, add new ones for changed files
        let changed_file_ids: std::collections::HashSet<&str> = new_files
            .iter()
            .filter(|f| changed_set.contains(f.path.as_str()))
            .map(|f| f.id.as_str())
            .collect();
        let mut relations: Vec<Relation> = old_relations
            .into_iter()
            .filter(|r| !changed_file_ids.contains(r.source_id.as_str()))
            .collect();
        let new_relations = Self::build_relations(&new_files, &all_imports);
        relations.extend(new_relations);

        // Merge memories: keep old for unchanged, add new for changed
        let old_memories = self.store.load_memories().unwrap_or_default();
        let mut memories: Vec<MemoryEntry> = old_memories
            .into_iter()
            .filter(|m| !changed_set.contains(m.path.as_str()))
            .collect();
        memories.extend(Self::build_memories(&all_comments));

        self.store.save_files(&new_files)?;
        self.store.save_symbols(&new_symbols)?;
        self.store.save_relations(&relations)?;
        self.store.save_fingerprints(&new_fingerprints)?;
        self.store.save_memories(&memories)?;

        info!(
            "changed-only index: {} changed, {} new, {} removed",
            stats.changed_files, stats.new_files, stats.removed_files
        );
        Ok(stats)
    }

    fn index_file(&mut self, path: &Path) -> Result<(FileEntry, Vec<Symbol>, Vec<RawImport>, Vec<RawComment>)> {
        let content = std::fs::read(path)?;
        self.index_file_with_content(path, content)
    }

    fn index_file_with_content(&mut self, path: &Path, content: Vec<u8>) -> Result<(FileEntry, Vec<Symbol>, Vec<RawImport>, Vec<RawComment>)> {
        let hash = blake3::hash(&content).to_hex().to_string();

        let rel_path = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();

        let language = Language::from_extension(&ext);
        let size = content.len() as u64;
        let file_id = format!("f:{}", &hash[..12]);

        let entry = FileEntry {
            id: file_id.clone(),
            path: rel_path.clone(),
            extension: if ext.is_empty() { None } else { Some(ext) },
            language: Some(language.to_string()),
            size,
            hash,
            indexed_at: Utc::now(),
            tags: Vec::new(),
        };

        let (symbols, imports, comments) = if language.is_code() {
            let content_str = String::from_utf8_lossy(&content);
            match self.parser.parse(&content_str, language, &file_id, &rel_path) {
                Ok(result) => {
                    debug!(
                        "extracted {} symbols, {} imports, {} comments from {}",
                        result.symbols.len(),
                        result.imports.len(),
                        result.comments.len(),
                        rel_path
                    );
                    (result.symbols, result.imports, result.comments)
                }
                Err(e) => {
                    debug!("parsing failed for {}: {}", rel_path, e);
                    (Vec::new(), Vec::new(), Vec::new())
                }
            }
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };

        Ok((entry, symbols, imports, comments))
    }

    /// Resolve collected imports into Relations.
    fn build_relations(
        files: &[FileEntry],
        file_imports: &[(String, Vec<RawImport>)],
    ) -> Vec<Relation> {
        let mut relations = Vec::new();

        // Build lookup maps
        let path_to_id: HashMap<&str, &str> = files
            .iter()
            .map(|f| (f.path.as_str(), f.id.as_str()))
            .collect();

        // Stem lookup: "foo" → ["src/foo.rs", "src/foo/mod.rs", ...]
        let mut stem_to_paths: HashMap<String, Vec<&str>> = HashMap::new();
        // Suffix lookup: "foo/bar.rs" → ["src/foo/bar.rs", "lib/foo/bar.rs", ...]
        let mut suffix_to_paths: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut dir_suffix_to_paths: HashMap<&str, Vec<&str>> = HashMap::new();
        for f in files {
            let p = Path::new(&f.path);
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                stem_to_paths
                    .entry(stem.to_string())
                    .or_default()
                    .push(&f.path);
            }
            // Index all suffixes starting after each '/'
            let path_str = f.path.as_str();
            let mut pos = 0;
            while let Some(slash) = path_str[pos..].find('/') {
                let suffix = &path_str[pos + slash + 1..];
                suffix_to_paths.entry(suffix).or_default().push(path_str);
                pos += slash + 1;
            }
            // Also index the full path itself
            suffix_to_paths.entry(path_str).or_default().push(path_str);

            // Index directory suffixes for JVM/C# package resolution
            // e.g. path "ktor-server/common/src/io/ktor/server/routing/RoutingNode.kt"
            // → dir suffix "io/ktor/server/routing" maps to the full path
            if let Some(parent) = Path::new(path_str).parent().and_then(|p| p.to_str()) {
                let mut dpos = 0;
                while let Some(slash) = parent[dpos..].find('/') {
                    let dir_suffix = &parent[dpos + slash + 1..];
                    if !dir_suffix.is_empty() {
                        dir_suffix_to_paths.entry(dir_suffix).or_default().push(path_str);
                    }
                    dpos += slash + 1;
                }
            }
        }

        for (source_path, imports) in file_imports {
            let source_id = match path_to_id.get(source_path.as_str()) {
                Some(id) => *id,
                None => continue,
            };
            let source_dir = Path::new(source_path)
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy();

            for imp in imports {
                let resolved = resolve_import(&imp.path, &source_dir, &path_to_id, &stem_to_paths, &suffix_to_paths, &dir_suffix_to_paths);
                for target_path in resolved {
                    if let Some(&target_id) = path_to_id.get(target_path) {
                        if target_id != source_id {
                            relations.push(Relation {
                                source_id: source_id.to_string(),
                                target_id: target_id.to_string(),
                                kind: RelationKind::Imports,
                                weight: 1.0,
                            });
                        }
                    }
                }
            }
        }

        // Add test/config relations
        Self::build_test_relations(&files, &mut relations);
        Self::build_config_relations(&files, &mut relations);

        // Deduplicate
        relations.sort_by(|a, b| {
            (&a.source_id, &a.target_id, &a.kind)
                .cmp(&(&b.source_id, &b.target_id, &b.kind))
        });
        relations.dedup_by(|a, b| {
            a.source_id == b.source_id
                && a.target_id == b.target_id
                && a.kind == b.kind
        });

        relations
    }

    /// Scan markdown documentation files and parse them into memories.
    fn scan_docs(&self) -> Vec<MemoryEntry> {
        let mut memories = Vec::new();
        let doc_dirs = ["docs", "doc", "adr", "decisions"];

        // Scan known doc directories
        for dir_name in &doc_dirs {
            let dir_path = self.root.join(dir_name);
            if dir_path.is_dir() {
                self.scan_md_dir(&dir_path, &mut memories);
            }
        }

        // Also scan root-level .md files (README, ARCHITECTURE, etc.)
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        if ext.eq_ignore_ascii_case("md") {
                            self.parse_md_file(&path, &mut memories);
                        }
                    }
                }
            }
        }

        debug!("scanned {} doc memories", memories.len());
        memories
    }

    fn scan_md_dir(&self, dir: &Path, memories: &mut Vec<MemoryEntry>) {
        let walker = walkdir::WalkDir::new(dir).max_depth(3);
        for entry in walker.into_iter().flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if ext.eq_ignore_ascii_case("md") {
                        self.parse_md_file(path, memories);
                    }
                }
            }
        }
    }

    fn parse_md_file(&self, path: &Path, memories: &mut Vec<MemoryEntry>) {
        let rel_path = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        match std::fs::read_to_string(path) {
            Ok(content) => {
                let doc_mems = kungfu_memory::doc_parser::parse_doc(&rel_path, &content);
                debug!("parsed {} memories from {}", doc_mems.len(), rel_path);
                memories.extend(doc_mems);
            }
            Err(e) => {
                debug!("failed to read doc {}: {}", rel_path, e);
            }
        }
    }

    /// Convert extracted comments into MemoryEntry items.
    fn build_memories(all_comments: &[(String, Vec<RawComment>)]) -> Vec<MemoryEntry> {
        let mut memories = Vec::new();
        for (path, comments) in all_comments {
            for comment in comments {
                let kind = match comment.kind {
                    CommentKind::Todo => MemoryKind::Todo,
                    CommentKind::Fixme => MemoryKind::Fixme,
                    CommentKind::Note => MemoryKind::Note,
                    CommentKind::Hack => MemoryKind::Note,
                    CommentKind::Doc => MemoryKind::Rationale,
                    CommentKind::Regular => continue,
                };

                let weight = match comment.kind {
                    CommentKind::Fixme => 0.9,
                    CommentKind::Todo => 0.8,
                    CommentKind::Note => 0.7,
                    CommentKind::Doc => 0.6,
                    CommentKind::Hack => 0.7,
                    CommentKind::Regular => 0.3,
                };

                let anchors = extract_anchors(&comment.text);

                let id = format!(
                    "mem:{}:{}",
                    path.replace('/', ":"),
                    comment.line
                );

                memories.push(MemoryEntry {
                    id,
                    path: path.clone(),
                    kind,
                    text: comment.text.clone(),
                    anchors,
                    weight,
                    symbol_id: comment.attached_symbol_id.clone(),
                    line_range: Some((comment.line, comment.end_line)),
                });
            }
        }
        memories
    }

    /// Detect test files and create TestFor relations to their source files.
    /// Only links when test and source are in nearby directories to avoid explosion.
    fn build_test_relations(files: &[FileEntry], relations: &mut Vec<Relation>) {
        // Build stem→file lookup (only non-test source files)
        let mut source_by_stem: HashMap<String, Vec<&FileEntry>> = HashMap::new();
        for f in files {
            if !is_test_file(&f.path) {
                let stem = extract_stem(&f.path);
                if !stem.is_empty() {
                    source_by_stem.entry(stem).or_default().push(f);
                }
            }
        }

        for f in files {
            if !is_test_file(&f.path) {
                continue;
            }
            let stem = extract_test_base_stem(&f.path);
            if stem.is_empty() {
                continue;
            }

            if let Some(sources) = source_by_stem.get(&stem) {
                let test_dir = Path::new(&f.path)
                    .parent()
                    .unwrap_or(Path::new(""))
                    .to_string_lossy();

                // Score candidates by directory proximity, only keep close ones
                let mut scored: Vec<(&FileEntry, u8)> = Vec::new();
                for source in sources {
                    let src_dir = Path::new(&source.path)
                        .parent()
                        .unwrap_or(Path::new(""))
                        .to_string_lossy();

                    // Same directory (e.g. foo.rs + foo_test.rs)
                    if src_dir == test_dir {
                        scored.push((source, 0));
                    // Sibling: tests/test_foo.py ↔ src/foo.py (share parent)
                    } else if dirs_share_parent(&test_dir, &src_dir) {
                        scored.push((source, 1));
                    // Test dir is child of source dir (e.g. src/foo.rs + src/__tests__/foo.test.ts)
                    } else if test_dir.starts_with(&format!("{}/", src_dir)) {
                        scored.push((source, 2));
                    // Source dir is child of test dir parent
                    } else if let Some(test_parent) = Path::new(test_dir.as_ref()).parent() {
                        let tp = test_parent.to_string_lossy();
                        if !tp.is_empty() && src_dir.starts_with(&format!("{}/", tp)) {
                            scored.push((source, 3));
                        }
                    }
                }

                // If too many matches even after proximity filter, skip (ambiguous)
                if scored.len() > 5 {
                    continue;
                }

                for (source, score) in &scored {
                    let weight = match score {
                        0 => 1.0,
                        1 => 0.9,
                        2 => 0.8,
                        _ => 0.6,
                    };
                    relations.push(Relation {
                        source_id: f.id.clone(),
                        target_id: source.id.clone(),
                        kind: RelationKind::TestFor,
                        weight,
                    });
                }
            }
        }
    }

    /// Detect config files and create ConfigFor relations to nearby source files.
    /// Only links to files in the same directory (not recursive) to avoid explosion
    /// on root-level configs like package.json or Cargo.toml.
    fn build_config_relations(files: &[FileEntry], relations: &mut Vec<Relation>) {
        let config_files: Vec<&FileEntry> = files
            .iter()
            .filter(|f| is_config_file(&f.path))
            .collect();

        if config_files.is_empty() {
            return;
        }

        for config in &config_files {
            let config_dir = Path::new(&config.path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();

            // Skip root-level configs — they relate to everything, which means nothing
            if config_dir.is_empty() {
                continue;
            }

            for f in files {
                if f.id == config.id || is_config_file(&f.path) {
                    continue;
                }
                let f_dir = Path::new(&f.path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                // Same directory only — no recursive descent
                if f_dir == config_dir {
                    let ext = Path::new(&f.path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");
                    if Language::from_extension(ext).is_code() {
                        relations.push(Relation {
                            source_id: config.id.clone(),
                            target_id: f.id.clone(),
                            kind: RelationKind::ConfigFor,
                            weight: 0.5,
                        });
                    }
                }
            }
        }
    }
}

/// Try to resolve an import path to actual file paths in the index.
fn resolve_import<'a>(
    import_path: &str,
    source_dir: &str,
    path_to_id: &HashMap<&'a str, &str>,
    stem_to_paths: &HashMap<String, Vec<&'a str>>,
    suffix_to_paths: &HashMap<&'a str, Vec<&'a str>>,
    dir_suffix_to_paths: &HashMap<&'a str, Vec<&'a str>>,
) -> Vec<&'a str> {
    let mut results = Vec::new();

    // 1. Relative imports: ./foo, ../foo
    if import_path.starts_with('.') {
        let base = if source_dir.is_empty() {
            import_path.to_string()
        } else {
            format!("{}/{}", source_dir, import_path)
        };
        // Normalize path (remove ./ and resolve ../)
        let normalized = normalize_path(&base);

        // Try common extensions — use direct HashMap lookup instead of linear scan
        let candidates = [
            normalized.clone(),
            format!("{}.ts", normalized),
            format!("{}.tsx", normalized),
            format!("{}.js", normalized),
            format!("{}.jsx", normalized),
            format!("{}.py", normalized),
            format!("{}/index.ts", normalized),
            format!("{}/index.js", normalized),
            format!("{}/__init__.py", normalized),
        ];
        for candidate in &candidates {
            if let Some((&path, _)) = path_to_id.get_key_value(candidate.as_str()) {
                results.push(path);
            }
        }
        return results;
    }

    // 2. Rust crate-internal: crate::foo::bar, super::foo, self::foo
    if import_path.starts_with("crate")
        || import_path.starts_with("super")
        || import_path.starts_with("self")
    {
        let stripped = import_path
            .trim_start_matches("crate::")
            .trim_start_matches("super::")
            .trim_start_matches("self::");

        // Convert module path to file path: foo::bar → foo/bar
        let module_path = stripped.replace("::", "/");

        // For crate:: imports, try resolving relative to the crate's src/ directory
        // Find the crate root by looking for the closest Cargo.toml-adjacent src/
        let crate_prefix = if import_path.starts_with("crate") {
            // Walk up from source_dir to find src/ boundary
            let mut prefix = source_dir.to_string();
            loop {
                if prefix.ends_with("/src") || prefix == "src" {
                    break;
                }
                if let Some((parent, _)) = prefix.rsplit_once('/') {
                    prefix = parent.to_string();
                } else {
                    prefix = String::new();
                    break;
                }
            }
            if prefix.is_empty() { None } else { Some(format!("{}/", prefix)) }
        } else {
            None
        };

        // For super::, resolve relative to parent directory
        let super_prefix = if import_path.starts_with("super") {
            Path::new(source_dir).parent()
                .map(|p| format!("{}/", p.to_string_lossy()))
        } else {
            None
        };

        // For self::, resolve relative to current directory
        let self_prefix = if import_path.starts_with("self") {
            Some(format!("{}/", source_dir))
        } else {
            None
        };

        // Try with specific crate/super/self prefix first (high confidence)
        for prefix in [crate_prefix, super_prefix, self_prefix].iter().flatten() {
            let candidates = [
                format!("{}{}.rs", prefix, module_path),
                format!("{}{}/mod.rs", prefix, module_path),
                format!("{}{}/lib.rs", prefix, module_path),
            ];
            for candidate in &candidates {
                if let Some((&path, _)) = path_to_id.get_key_value(candidate.as_str()) {
                    results.push(path);
                }
            }
        }

        // Fallback: suffix-based lookup (lower confidence)
        if results.is_empty() {
            let candidates = [
                format!("{}.rs", module_path),
                format!("{}/mod.rs", module_path),
                format!("{}/lib.rs", module_path),
            ];
            for candidate in &candidates {
                if let Some(paths) = suffix_to_paths.get(candidate.as_str()) {
                    // Only take results that are close to the source file
                    for &path in paths.iter() {
                        // Prefer same crate (shares a common prefix)
                        let common = source_dir.chars().zip(path.chars())
                            .take_while(|(a, b)| a == b).count();
                        if common > 5 || paths.len() == 1 {
                            results.push(path);
                        }
                    }
                }
            }
        }

        return results;
    }

    // 3. Dotted imports: Python, Java, Kotlin, C#
    //    foo.bar.baz → foo/bar/baz.py | .java | .kt | .cs
    if import_path.contains('.') && !import_path.contains('/') {
        let file_path = import_path.replace('.', "/");
        let candidates = [
            format!("{}.py", file_path),
            format!("{}/__init__.py", file_path),
            format!("{}.java", file_path),
            format!("{}.kt", file_path),
            format!("{}.cs", file_path),
        ];
        // Use suffix index for O(1) lookup
        for candidate in &candidates {
            if let Some(paths) = suffix_to_paths.get(candidate.as_str()) {
                results.extend(paths.iter());
            }
        }
        if !results.is_empty() {
            return results;
        }

        // 3b. JVM/C# package-directory resolution
        // import io.ktor.server.routing.Routing → package dir "io/ktor/server/routing"
        // Class name may not match filename, so find all files in the package directory
        let segments: Vec<&str> = import_path.split('.').collect();
        if segments.len() >= 2 {
            // Last segment is class/function name, rest is package
            let pkg_dir = segments[..segments.len() - 1].join("/");
            if let Some(paths) = dir_suffix_to_paths.get(pkg_dir.as_str()) {
                // Only take code files (not resources)
                for &path in paths.iter().take(5) {
                    results.push(path);
                }
            }
            if !results.is_empty() {
                return results;
            }

            // Also try wildcard: import io.ktor.server.routing.* (path already stripped of .*)
            // In this case file_path IS the directory
            if let Some(paths) = dir_suffix_to_paths.get(file_path.as_str()) {
                for &path in paths.iter().take(5) {
                    results.push(path);
                }
            }
            if !results.is_empty() {
                return results;
            }
        }
    }

    // 4. Fallback: try matching the last segment as a file stem
    // Only use stem fallback if the name is specific enough (>= 4 chars, not a common word)
    let last = import_path
        .rsplit(|c| c == '/' || c == ':' || c == '.')
        .next()
        .unwrap_or(import_path);

    if !last.is_empty() && last.len() >= 4 {
        if let Some(paths) = stem_to_paths.get(last) {
            if paths.len() == 1 {
                // Unique stem match — high confidence
                results.extend(paths.iter());
            } else {
                // Multiple matches — prefer ones close to source
                for &path in paths.iter().take(3) {
                    let common = source_dir.chars().zip(path.chars())
                        .take_while(|(a, b)| a == b).count();
                    if common > 5 {
                        results.push(path);
                    }
                }
                // If no proximity match, take first one only
                if results.is_empty() {
                    results.extend(paths.iter().take(1));
                }
            }
        }
    }

    results
}

fn is_test_file(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let lower = filename.to_lowercase();

    // foo_test.rs, foo_test.go
    lower.contains("_test.")
        // foo.test.ts, foo.test.js
        || lower.contains(".test.")
        // foo.spec.ts, foo.spec.js
        || lower.contains(".spec.")
        // files in tests/ or test/ or __tests__/ directories
        || path.contains("/tests/")
        || path.contains("/test/")
        || path.contains("/__tests__/")
        // test_foo.py
        || lower.starts_with("test_")
}

fn is_config_file(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let lower = filename.to_lowercase();

    matches!(
        lower.as_str(),
        "cargo.toml"
            | "package.json"
            | "tsconfig.json"
            | "pyproject.toml"
            | "setup.py"
            | "setup.cfg"
            | "go.mod"
            | "go.sum"
            | "build.gradle"
            | "build.gradle.kts"
            | "settings.gradle"
            | "settings.gradle.kts"
            | "pom.xml"
            | "gradle.properties"
            | "makefile"
            | "dockerfile"
            | "docker-compose.yml"
            | "docker-compose.yaml"
            | ".env"
            | ".env.example"
    ) || lower.ends_with(".config.js")
        || lower.ends_with(".config.ts")
        || lower.ends_with(".config.mjs")
        || lower.ends_with(".csproj")
        || lower.ends_with(".sln")
}

fn extract_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn extract_test_base_stem(path: &str) -> String {
    let stem = extract_stem(path);
    // foo_test → foo
    if let Some(base) = stem.strip_suffix("_test") {
        return base.to_string();
    }
    // foo.test → foo, foo.spec → foo (stem already stripped extension once)
    if let Some(base) = stem.strip_suffix(".test") {
        return base.to_string();
    }
    if let Some(base) = stem.strip_suffix(".spec") {
        return base.to_string();
    }
    // test_foo → foo
    if let Some(base) = stem.strip_prefix("test_") {
        return base.to_string();
    }
    // If in tests/ dir, use stem as-is for matching
    if path.contains("/tests/") || path.contains("/test/") || path.contains("/__tests__/") {
        return stem;
    }
    String::new()
}

/// Check if two directory paths share the same parent (are siblings).
fn dirs_share_parent(a: &str, b: &str) -> bool {
    let pa = Path::new(a).parent();
    let pb = Path::new(b).parent();
    match (pa, pb) {
        (Some(pa), Some(pb)) => !pa.as_os_str().is_empty() && pa == pb,
        _ => false,
    }
}

/// Extract keyword anchors from comment text for matching.
fn extract_anchors(text: &str) -> Vec<String> {
    static STOP_WORDS: &[&str] = &[
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "shall",
        "should", "may", "might", "must", "can", "could", "this", "that",
        "these", "those", "it", "its", "of", "in", "to", "for", "with",
        "on", "at", "by", "from", "as", "into", "about", "not", "no",
        "but", "or", "and", "if", "then", "else", "when", "while",
        "todo", "fixme", "note", "hack", "xxx", "bug",
    ];

    let mut anchors: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .collect();
    anchors.sort();
    anchors.dedup();
    anchors
}

/// Normalize a file path: resolve `.` and `..` components.
fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}
