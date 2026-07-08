use anyhow::Result;
use chrono::Utc;
use kungfu_config::KungfuConfig;
use kungfu_parse::{CommentKind, Parser, RawCall, RawComment, RawImport};
use kungfu_storage::JsonStore;
use kungfu_types::file::{FileEntry, Language};
use kungfu_types::memory::{MemoryEntry, MemoryKind};
use kungfu_types::relation::{CallGraphMeta, Relation, RelationKind};
use kungfu_types::symbol::Symbol;
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info, warn};

use crate::scanner;

type ParsedFile = (
    FileEntry,
    Vec<Symbol>,
    Vec<RawImport>,
    Vec<RawComment>,
    Vec<RawCall>,
);

/// Outcome of deciding whether a file's content should be read for indexing.
enum ReadOutcome {
    /// Content fits the cap and is text — index it fully.
    Full(Vec<u8>),
    /// Oversized or binary — record the file by name only, no content read/parse.
    Skipped { size: u64, reason: &'static str },
}

/// Heuristic: a NUL byte in the first 8 KiB means binary content.
fn is_binary(content: &[u8]) -> bool {
    content.iter().take(8192).any(|&b| b == 0)
}

/// Known binary/asset extensions that are never worth reading. Matched before the
/// size cap, so e.g. a 15 MB photo is recorded by name without ever being opened.
/// This is a fast-path; the size cap and NUL sniff remain the catch-all.
fn is_binary_extension(path: &Path) -> bool {
    const BINARY_EXTS: &[&str] = &[
        // images
        "png",
        "jpg",
        "jpeg",
        "gif",
        "webp",
        "bmp",
        "ico",
        "tiff",
        "tif",
        "heic",
        "avif",
        // video / audio
        "mp4",
        "mov",
        "avi",
        "mkv",
        "webm",
        "mp3",
        "wav",
        "flac",
        "ogg",
        "m4a",
        "aac",
        // archives / compressed
        "zip",
        "tar",
        "gz",
        "tgz",
        "bz2",
        "xz",
        "zst",
        "7z",
        "rar",
        // documents / fonts
        "pdf",
        "woff",
        "woff2",
        "ttf",
        "otf",
        "eot",
        // executables / binary blobs
        "bin",
        "dat",
        "wasm",
        "exe",
        "dll",
        "so",
        "dylib",
        "o",
        "a",
        "class",
        "jar",
        // databases
        "sqlite",
        "db",
        "mdb",
        // ml / numeric blobs
        "onnx",
        "pt",
        "pth",
        "safetensors",
        "gguf",
        "npy",
        "npz",
        "parquet",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| BINARY_EXTS.contains(&e.as_str()))
}

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
    /// Call edges dropped by the frequency cutoff (callee invoked from more than
    /// `call_graph.max_caller_files` distinct files — utility-noise).
    pub call_edges_filtered: usize,
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
        let mut all_calls: Vec<(String, Vec<RawCall>)> = Vec::new();

        for path in &paths {
            match self.index_file(path) {
                Ok((entry, symbols, imports, comments, calls)) => {
                    fingerprints.insert(entry.path.clone(), entry.hash.clone());
                    if !imports.is_empty() {
                        all_imports.push((entry.path.clone(), imports));
                    }
                    if !comments.is_empty() {
                        all_comments.push((entry.path.clone(), comments));
                    }
                    if !calls.is_empty() {
                        all_calls.push((entry.path.clone(), calls));
                    }
                    all_symbols.extend(symbols);
                    files.push(entry);
                }
                Err(e) => {
                    warn!("failed to index {}: {}", path.display(), e);
                }
            }
        }

        let mut relations = Self::build_relations(&files, &all_imports);
        relations.extend(Self::build_call_relations(
            &self.config.call_graph,
            &files,
            &all_symbols,
            &all_calls,
        ));
        let (call_edges_filtered, dropped_callees) =
            Self::filter_call_graph_noise(&self.config.call_graph, &mut relations, &all_symbols);
        let mut memories = Self::build_memories(&all_comments);
        let doc_memories = self.scan_docs();
        memories.extend(doc_memories);

        let stats = IndexStats {
            total_files: files.len(),
            new_files: files.len(),
            changed_files: 0,
            removed_files: 0,
            symbols_extracted: all_symbols.len(),
            call_edges_filtered,
        };

        self.store.save_files(&files)?;
        self.store.save_symbols(&all_symbols)?;
        self.store.save_relations(&relations)?;
        self.persist_call_graph_meta(dropped_callees, false)?;
        self.store.save_fingerprints(&fingerprints)?;
        self.store.save_memories(&memories)?;
        self.store.save_schema_version()?;

        info!(
            "indexed {} files, {} symbols, {} relations, {} memories",
            stats.total_files,
            stats.symbols_extracted,
            relations.len(),
            memories.len()
        );
        Ok(stats)
    }

    pub fn index_incremental(&mut self) -> Result<IndexStats> {
        let old_fingerprints = self.store.load_fingerprints()?;
        let old_files = self.store.load_files()?;
        let old_symbols = self.store.load_symbols()?;
        let old_relations = self.store.load_relations()?;
        // Files re-parsed this run; their old relations/memories are dropped and rebuilt.
        // Unchanged files keep theirs — otherwise a no-op reindex would wipe the whole
        // graph and all comment/doc memories, since both are only collected for
        // re-parsed files.
        let mut changed_file_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut changed_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

        let paths = scanner::scan_files(&self.root, &self.config)?;

        let mut new_fingerprints = HashMap::new();
        let mut new_files = Vec::new();
        let mut new_symbols = Vec::new();
        let mut all_imports: Vec<(String, Vec<RawImport>)> = Vec::new();
        let mut all_comments: Vec<(String, Vec<RawComment>)> = Vec::new();
        let mut all_calls: Vec<(String, Vec<RawCall>)> = Vec::new();

        let mut stats = IndexStats {
            total_files: 0,
            new_files: 0,
            changed_files: 0,
            removed_files: 0,
            symbols_extracted: 0,
            call_edges_filtered: 0,
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

            let read = match self.read_for_index(path) {
                Ok(r) => r,
                Err(e) => {
                    warn!("cannot read {}: {}", path.display(), e);
                    continue;
                }
            };
            let hash = match &read {
                ReadOutcome::Full(content) => blake3::hash(content).to_hex().to_string(),
                ReadOutcome::Skipped { size, .. } => format!("skip:{size}"),
            };

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

            match read {
                ReadOutcome::Full(content) => match self.index_file_with_content(path, content) {
                    Ok((entry, symbols, imports, comments, calls)) => {
                        new_fingerprints.insert(entry.path.clone(), entry.hash.clone());
                        if !imports.is_empty() {
                            all_imports.push((entry.path.clone(), imports));
                        }
                        if !comments.is_empty() {
                            all_comments.push((entry.path.clone(), comments));
                        }
                        if !calls.is_empty() {
                            all_calls.push((entry.path.clone(), calls));
                        }
                        changed_file_ids.insert(entry.id.clone());
                        changed_paths.insert(entry.path.clone());
                        new_symbols.extend(symbols);
                        new_files.push(entry);
                    }
                    Err(e) => {
                        warn!("failed to index {}: {}", path.display(), e);
                    }
                },
                ReadOutcome::Skipped { size, reason } => {
                    debug!("indexing {} by name only ({})", path.display(), reason);
                    let entry = self.name_only_entry(path, size);
                    new_fingerprints.insert(entry.path.clone(), entry.hash.clone());
                    changed_file_ids.insert(entry.id.clone());
                    changed_paths.insert(entry.path.clone());
                    new_files.push(entry);
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

        // Merge: keep relations whose source is an unchanged file, rebuild the rest.
        // Source ids are file-level (imports/test/config) or symbol-level (`s:{file_id}:…`, calls).
        let changed_sym_prefixes: Vec<String> = changed_file_ids
            .iter()
            .map(|fid| format!("s:{fid}:"))
            .collect();
        let mut relations: Vec<Relation> = old_relations
            .into_iter()
            .filter(|r| {
                !changed_file_ids.contains(r.source_id.as_str())
                    && !changed_sym_prefixes
                        .iter()
                        .any(|p| r.source_id.starts_with(p))
            })
            .collect();
        let mut fresh = Self::build_relations(&new_files, &all_imports);
        fresh.extend(Self::build_call_relations(
            &self.config.call_graph,
            &new_files,
            &new_symbols,
            &all_calls,
        ));
        relations.extend(fresh);
        // Post-merge so the noise rules also govern edges kept from unchanged files
        // (a config change takes effect without waiting for a full reindex).
        let (edges_filtered, dropped_callees) =
            Self::filter_call_graph_noise(&self.config.call_graph, &mut relations, &new_symbols);
        stats.call_edges_filtered = edges_filtered;

        // Merge memories the same way: keep entries from unchanged still-existing files,
        // rebuild from re-parsed ones. Changed doc files are re-parsed explicitly — doc
        // memories come from scan_docs, which only runs on full index.
        let old_memories = self.store.load_memories().unwrap_or_default();
        let mut memories: Vec<MemoryEntry> = old_memories
            .into_iter()
            .filter(|m| !changed_paths.contains(m.path.as_str()) && current_paths.contains(&m.path))
            .collect();
        memories.extend(Self::build_memories(&all_comments));
        for path in &changed_paths {
            if is_doc_memory_source(path) {
                self.parse_md_file(&self.root.join(path), &mut memories);
            }
        }

        self.store.save_files(&new_files)?;
        self.store.save_symbols(&new_symbols)?;
        self.store.save_relations(&relations)?;
        self.persist_call_graph_meta(dropped_callees, true)?;
        self.store.save_fingerprints(&new_fingerprints)?;
        self.store.save_memories(&memories)?;
        self.store.save_schema_version()?;

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
            call_edges_filtered: 0,
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
        let mut all_calls: Vec<(String, Vec<RawCall>)> = Vec::new();

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
                Ok((entry, symbols, imports, comments, calls)) => {
                    new_fingerprints.insert(entry.path.clone(), entry.hash.clone());
                    stats.symbols_extracted += symbols.len();
                    if !imports.is_empty() {
                        all_imports.push((entry.path.clone(), imports));
                    }
                    if !comments.is_empty() {
                        all_comments.push((entry.path.clone(), comments));
                    }
                    if !calls.is_empty() {
                        all_calls.push((entry.path.clone(), calls));
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

        // Merge: keep old relations for unchanged files, add new ones for changed files.
        // Imports/test/config relations are file-level (source_id = file id); Calls are
        // symbol-level (source_id = `s:{file_id}:…`), so drop both shapes for changed files.
        let changed_file_ids: std::collections::HashSet<&str> = new_files
            .iter()
            .filter(|f| changed_set.contains(f.path.as_str()))
            .map(|f| f.id.as_str())
            .collect();
        let changed_sym_prefixes: Vec<String> = changed_file_ids
            .iter()
            .map(|fid| format!("s:{fid}:"))
            .collect();
        let mut relations: Vec<Relation> = old_relations
            .into_iter()
            .filter(|r| {
                !changed_file_ids.contains(r.source_id.as_str())
                    && !changed_sym_prefixes
                        .iter()
                        .any(|p| r.source_id.starts_with(p))
            })
            .collect();
        let mut new_relations = Self::build_relations(&new_files, &all_imports);
        new_relations.extend(Self::build_call_relations(
            &self.config.call_graph,
            &new_files,
            &new_symbols,
            &all_calls,
        ));
        relations.extend(new_relations);
        let (edges_filtered, dropped_callees) =
            Self::filter_call_graph_noise(&self.config.call_graph, &mut relations, &new_symbols);
        stats.call_edges_filtered = edges_filtered;

        // Merge memories: keep old for unchanged, add new for changed
        let old_memories = self.store.load_memories().unwrap_or_default();
        let mut memories: Vec<MemoryEntry> = old_memories
            .into_iter()
            .filter(|m| !changed_set.contains(m.path.as_str()))
            .collect();
        memories.extend(Self::build_memories(&all_comments));
        for path in changed_paths {
            if is_doc_memory_source(path) && self.root.join(path).exists() {
                self.parse_md_file(&self.root.join(path), &mut memories);
            }
        }

        self.store.save_files(&new_files)?;
        self.store.save_symbols(&new_symbols)?;
        self.store.save_relations(&relations)?;
        self.persist_call_graph_meta(dropped_callees, true)?;
        self.store.save_fingerprints(&new_fingerprints)?;
        self.store.save_memories(&memories)?;
        self.store.save_schema_version()?;

        info!(
            "changed-only index: {} changed, {} new, {} removed",
            stats.changed_files, stats.new_files, stats.removed_files
        );
        Ok(stats)
    }

    fn index_file(&mut self, path: &Path) -> Result<ParsedFile> {
        match self.read_for_index(path)? {
            ReadOutcome::Full(content) => self.index_file_with_content(path, content),
            ReadOutcome::Skipped { size, reason } => {
                debug!("indexing {} by name only ({})", path.display(), reason);
                Ok((
                    self.name_only_entry(path, size),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                ))
            }
        }
    }

    /// Decide whether to read a file's full content. Known binary/asset extensions
    /// and files over `max_file_bytes` are never read (cheap checks only); remaining
    /// files are read up to the cap and NUL-sniffed, dropping binary content. Either
    /// way they are still recorded by name.
    fn read_for_index(&self, path: &Path) -> std::io::Result<ReadOutcome> {
        let size = std::fs::metadata(path)?.len();
        if is_binary_extension(path) {
            return Ok(ReadOutcome::Skipped {
                size,
                reason: "binary-ext",
            });
        }
        if size > self.config.index.max_file_bytes {
            return Ok(ReadOutcome::Skipped {
                size,
                reason: "oversized",
            });
        }
        let content = std::fs::read(path)?;
        if is_binary(&content) {
            return Ok(ReadOutcome::Skipped {
                size: content.len() as u64,
                reason: "binary",
            });
        }
        Ok(ReadOutcome::Full(content))
    }

    /// Build a FileEntry for a file whose content we deliberately did not read,
    /// so it stays discoverable by path/name. Fingerprint is size-based, so the
    /// file is only re-evaluated when its size changes.
    fn name_only_entry(&self, path: &Path, size: u64) -> FileEntry {
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
        let id = format!("f:{}", &blake3::hash(rel_path.as_bytes()).to_hex()[..12]);
        let tags = crate::file_tags::derive_tags(&rel_path, &[], &[]);
        FileEntry {
            id,
            path: rel_path,
            extension: if ext.is_empty() { None } else { Some(ext) },
            language: Some(language.to_string()),
            size,
            hash: format!("skip:{size}"),
            indexed_at: Utc::now(),
            tags,
            purpose: None,
        }
    }

    fn index_file_with_content(&mut self, path: &Path, content: Vec<u8>) -> Result<ParsedFile> {
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

        let mut entry = FileEntry {
            id: file_id.clone(),
            path: rel_path.clone(),
            extension: if ext.is_empty() { None } else { Some(ext) },
            language: Some(language.to_string()),
            size,
            hash,
            indexed_at: Utc::now(),
            tags: Vec::new(),
            purpose: None,
        };

        let (symbols, imports, comments, calls) = if language.is_code() {
            let content_str = String::from_utf8_lossy(&content);
            match self
                .parser
                .parse(&content_str, language, &file_id, &rel_path)
            {
                Ok(result) => {
                    debug!(
                        "extracted {} symbols, {} imports, {} comments, {} calls from {}",
                        result.symbols.len(),
                        result.imports.len(),
                        result.comments.len(),
                        result.calls.len(),
                        rel_path
                    );
                    entry.purpose = result.module_doc;
                    (
                        result.symbols,
                        result.imports,
                        result.comments,
                        result.calls,
                    )
                }
                Err(e) => {
                    debug!("parsing failed for {}: {}", rel_path, e);
                    (Vec::new(), Vec::new(), Vec::new(), Vec::new())
                }
            }
        } else {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        };

        entry.tags = crate::file_tags::derive_tags(&rel_path, &imports, &symbols);

        Ok((entry, symbols, imports, comments, calls))
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

        // Workspace crate roots: import name → crate src dir.
        // "crates/kungfu-core/src/lib.rs" → "kungfu_core" → "crates/kungfu-core/src".
        // Lets cross-crate `use kungfu_types::…` resolve to the right crate instead of
        // falling through to the fuzzy stem fallback (which left coupling nearly empty).
        let mut crate_roots: HashMap<String, String> = HashMap::new();
        for f in files {
            if let Some(idx) = f.path.find("/src/") {
                let before = &f.path[..idx];
                let crate_dir = before.rsplit('/').next().unwrap_or(before);
                if !crate_dir.is_empty() {
                    crate_roots
                        .entry(crate_dir.replace('-', "_"))
                        .or_insert_with(|| format!("{before}/src"));
                }
            }
        }

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
                        dir_suffix_to_paths
                            .entry(dir_suffix)
                            .or_default()
                            .push(path_str);
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
                let resolved = resolve_import(
                    &imp.path,
                    &source_dir,
                    &path_to_id,
                    &stem_to_paths,
                    &suffix_to_paths,
                    &dir_suffix_to_paths,
                    &crate_roots,
                );
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
        Self::build_test_relations(files, &mut relations);
        Self::build_config_relations(files, &mut relations);

        // Deduplicate
        relations.sort_by(|a, b| {
            (&a.source_id, &a.target_id, &a.kind).cmp(&(&b.source_id, &b.target_id, &b.kind))
        });
        relations.dedup_by(|a, b| {
            a.source_id == b.source_id && a.target_id == b.target_id && a.kind == b.kind
        });

        relations
    }

    /// Resolve extracted call sites into symbol-level `Calls` relations.
    ///
    /// Precision over recall: an edge is emitted only when the callee name resolves
    /// unambiguously — it is unique project-wide, or (for free/path calls, not method calls)
    /// there is exactly one same-file definition. Ambiguous names (`clone`, a method shared by
    /// many types, an overloaded helper) are dropped rather than guessed, so callers/callees stay
    /// trustworthy. Method calls, whose receiver type we cannot infer, resolve only via a
    /// globally-unique name. Calls to unindexed symbols (std, external crates) produce no edge.
    fn build_call_relations(
        cfg: &kungfu_config::CallGraphConfig,
        files: &[FileEntry],
        symbols: &[Symbol],
        file_calls: &[(String, Vec<RawCall>)],
    ) -> Vec<Relation> {
        use kungfu_types::symbol::SymbolKind;

        if !cfg.enabled {
            return Vec::new();
        }

        let path_to_id: HashMap<&str, &str> = files
            .iter()
            .map(|f| (f.path.as_str(), f.id.as_str()))
            .collect();
        let path_to_lang: HashMap<&str, Option<&str>> = files
            .iter()
            .map(|f| (f.path.as_str(), f.language.as_deref()))
            .collect();

        let is_callable = |k: SymbolKind| matches!(k, SymbolKind::Function | SymbolKind::Method);
        let mut by_name: HashMap<&str, Vec<&Symbol>> = HashMap::new();
        let mut by_file_line: HashMap<(&str, usize), &Symbol> = HashMap::new();
        for s in symbols {
            if is_callable(s.kind) {
                by_name.entry(s.name.as_str()).or_default().push(s);
                by_file_line.insert((s.file_id.as_str(), s.span.start_line), s);
            }
        }

        let mut relations = Vec::new();
        for (path, calls) in file_calls {
            let file_id = match path_to_id.get(path.as_str()) {
                Some(id) => *id,
                None => continue,
            };
            let language = path_to_lang.get(path.as_str()).copied().flatten();
            for call in calls {
                let caller = match by_file_line.get(&(file_id, call.caller_line)) {
                    Some(c) => *c,
                    None => continue,
                };
                if crate::stoplist::is_ubiquitous_callable(&call.callee, language) {
                    continue;
                }
                let candidates = match by_name.get(call.callee.as_str()) {
                    Some(c) => c,
                    None => continue,
                };

                let target = if !call.is_method {
                    // Free/path call: a single same-file definition is a confident local target.
                    let same: Vec<&&Symbol> = candidates
                        .iter()
                        .filter(|c| c.file_id == file_id && c.id != caller.id)
                        .collect();
                    if same.len() == 1 {
                        Some(*same[0])
                    } else {
                        unique_global(candidates, caller)
                    }
                } else {
                    // Method call: only a project-wide unique name is safe to attribute.
                    unique_global(candidates, caller)
                };

                if let Some(t) = target {
                    relations.push(Relation {
                        source_id: caller.id.clone(),
                        target_id: t.id.clone(),
                        kind: RelationKind::Calls,
                        weight: 1.0,
                    });
                }
            }
        }

        relations.sort_by(|a, b| (&a.source_id, &a.target_id).cmp(&(&b.source_id, &b.target_id)));
        relations.dedup_by(|a, b| {
            a.source_id == b.source_id && a.target_id == b.target_id && a.kind == b.kind
        });
        relations
    }

    /// Enforce the call-graph noise rules on the final (merged) relation set,
    /// so they also govern edges carried over from unchanged files and a config
    /// change takes effect on the next indexing run of any kind:
    ///
    /// - `enabled = false` → no `Calls` relations are persisted at all;
    /// - `cross_file_only` → drop edges whose caller and callee share a file;
    /// - `max_caller_files = N` → a callee invoked from more than N distinct
    ///   files is utility-noise: drop its incoming edges.
    ///
    /// Returns how many edges the frequency cutoff dropped (surfaced in
    /// `IndexStats::call_edges_filtered`) plus the sorted, deduplicated names
    /// of the dropped callees — persisted via [`Self::persist_call_graph_meta`]
    /// so an empty callers result can honestly say the cutoff fired.
    fn filter_call_graph_noise(
        cfg: &kungfu_config::CallGraphConfig,
        relations: &mut Vec<Relation>,
        symbols: &[Symbol],
    ) -> (usize, Vec<String>) {
        if !cfg.enabled {
            relations.retain(|r| r.kind != RelationKind::Calls);
            return (0, Vec::new());
        }

        let file_of: HashMap<&str, &str> = symbols
            .iter()
            .map(|s| (s.id.as_str(), s.file_id.as_str()))
            .collect();

        if cfg.cross_file_only {
            let before = relations.len();
            relations.retain(|r| {
                if r.kind != RelationKind::Calls {
                    return true;
                }
                match (
                    file_of.get(r.source_id.as_str()),
                    file_of.get(r.target_id.as_str()),
                ) {
                    (Some(a), Some(b)) => a != b,
                    // Endpoint not in the symbol table (stale edge) — keep;
                    // the merge filters own stale-edge cleanup.
                    _ => true,
                }
            });
            debug!(
                "call graph: dropped {} same-file edges (cross_file_only)",
                before - relations.len()
            );
        }

        if cfg.max_caller_files == 0 {
            return (0, Vec::new());
        }

        let mut caller_files: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
        for r in relations.iter() {
            if r.kind != RelationKind::Calls {
                continue;
            }
            if let Some(&src_file) = file_of.get(r.source_id.as_str()) {
                caller_files
                    .entry(r.target_id.as_str())
                    .or_default()
                    .insert(src_file);
            }
        }
        let noisy: std::collections::HashSet<String> = caller_files
            .iter()
            .filter(|(_, files)| files.len() > cfg.max_caller_files)
            .map(|(id, _)| (*id).to_string())
            .collect();
        if noisy.is_empty() {
            return (0, Vec::new());
        }

        let name_of: HashMap<&str, &str> = symbols
            .iter()
            .map(|s| (s.id.as_str(), s.name.as_str()))
            .collect();
        let dropped_names: std::collections::BTreeSet<String> = noisy
            .iter()
            .filter_map(|id| name_of.get(id.as_str()).map(|n| (*n).to_string()))
            .collect();

        let before = relations.len();
        relations
            .retain(|r| r.kind != RelationKind::Calls || !noisy.contains(r.target_id.as_str()));
        let dropped = before - relations.len();
        debug!(
            "call graph: dropped {} edges to {} utility-noise callees (called from > {} files)",
            dropped,
            noisy.len(),
            cfg.max_caller_files
        );
        (dropped, dropped_names.into_iter().collect())
    }

    /// Persist the names of frequency-dropped callees (`call_graph_meta.json`).
    ///
    /// Dropped edges are never written to disk, so an incremental run recounts
    /// only the surviving + freshly rebuilt edges and cannot re-detect callees
    /// dropped in earlier runs. Incremental paths therefore merge with the
    /// existing shard (`merge = true`); a full index recounts everything and
    /// overwrites, which is also how a stale name ages out after a refactor.
    fn persist_call_graph_meta(&self, dropped_names: Vec<String>, merge: bool) -> Result<()> {
        let cfg = &self.config.call_graph;
        let cutoff_active = cfg.enabled && cfg.max_caller_files > 0;
        let mut names: std::collections::BTreeSet<String> = dropped_names.into_iter().collect();
        if merge && cutoff_active {
            names.extend(self.store.load_call_graph_meta().frequency_dropped_callees);
        }
        self.store.save_call_graph_meta(&CallGraphMeta {
            frequency_dropped_callees: names.into_iter().collect(),
        })
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

                let id = format!("mem:{}:{}", path.replace('/', ":"), comment.line);

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
        let config_files: Vec<&FileEntry> =
            files.iter().filter(|f| is_config_file(&f.path)).collect();

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
/// The sole project-wide callable with this name, if unique and not the caller itself.
fn unique_global<'a>(candidates: &[&'a Symbol], caller: &Symbol) -> Option<&'a Symbol> {
    match candidates {
        [only] if only.id != caller.id => Some(only),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_import<'a>(
    import_path: &str,
    source_dir: &str,
    path_to_id: &HashMap<&'a str, &str>,
    stem_to_paths: &HashMap<String, Vec<&'a str>>,
    suffix_to_paths: &HashMap<&'a str, Vec<&'a str>>,
    dir_suffix_to_paths: &HashMap<&'a str, Vec<&'a str>>,
    crate_roots: &HashMap<String, String>,
) -> Vec<&'a str> {
    let mut results = Vec::new();

    // 0. Workspace crate import: `kungfu_types::budget::Budget` → that crate's src dir.
    //    Checked before the generic fallbacks so it wins over a fuzzy stem match.
    let (crate_name, rest) = match import_path.split_once("::") {
        Some((c, r)) => (c, r),
        None => (import_path, ""),
    };
    if let Some(src_dir) = crate_roots.get(crate_name) {
        let module_path = rest.replace("::", "/");
        let candidates = if module_path.is_empty() {
            vec![format!("{src_dir}/lib.rs")]
        } else {
            vec![
                format!("{src_dir}/{module_path}.rs"),
                format!("{src_dir}/{module_path}/mod.rs"),
                // Re-export through the crate root (e.g. `kungfu_types::Budget`).
                format!("{src_dir}/lib.rs"),
            ]
        };
        for candidate in &candidates {
            if let Some((&path, _)) = path_to_id.get_key_value(candidate.as_str()) {
                results.push(path);
            }
        }
        if !results.is_empty() {
            return results;
        }
    }

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
            if prefix.is_empty() {
                None
            } else {
                Some(format!("{}/", prefix))
            }
        } else {
            None
        };

        // For super::, resolve relative to parent directory
        let super_prefix = if import_path.starts_with("super") {
            Path::new(source_dir)
                .parent()
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
                        let common = source_dir
                            .chars()
                            .zip(path.chars())
                            .take_while(|(a, b)| a == b)
                            .count();
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
        .rsplit(['/', ':', '.'])
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
                    let common = source_dir
                        .chars()
                        .zip(path.chars())
                        .take_while(|(a, b)| a == b)
                        .count();
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

/// Whether `scan_docs` would pick this file up as a doc-memory source: a root-level
/// markdown file or one under a known docs directory. Used by incremental paths to
/// re-parse only the changed docs instead of re-running the full scan.
fn is_doc_memory_source(path: &str) -> bool {
    if !path.to_lowercase().ends_with(".md") {
        return false;
    }
    !path.contains('/')
        || path.starts_with("docs/")
        || path.starts_with("doc/")
        || path.starts_with("adr/")
        || path.starts_with("decisions/")
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
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "shall", "should", "may", "might", "must", "can",
        "could", "this", "that", "these", "those", "it", "its", "of", "in", "to", "for", "with",
        "on", "at", "by", "from", "as", "into", "about", "not", "no", "but", "or", "and", "if",
        "then", "else", "when", "while", "todo", "fixme", "note", "hack", "xxx", "bug",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_binary_detects_nul() {
        assert!(is_binary(b"abc\0def"));
        assert!(!is_binary(b"plain text\nwith newlines"));
        // NUL beyond the 8 KiB sniff window is ignored.
        let mut late = vec![b'a'; 9000];
        late.push(0);
        assert!(!is_binary(&late));
    }

    fn temp_indexer_root() -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        let unique = format!(
            "kungfu-idx-test-{}-{:p}",
            std::process::id(),
            &dir as *const _
        );
        dir.push(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fresh_store(root: &Path) -> JsonStore {
        let index_dir = root.join(".kungfu").join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        JsonStore::new(&index_dir)
    }

    #[test]
    fn is_binary_extension_matches_assets_case_insensitively() {
        assert!(is_binary_extension(Path::new("a/b/photo.png")));
        assert!(is_binary_extension(Path::new("PHOTO.JPG")));
        assert!(is_binary_extension(Path::new("model.safetensors")));
        assert!(is_binary_extension(Path::new("data.sqlite")));
        assert!(!is_binary_extension(Path::new("src/main.rs")));
        assert!(!is_binary_extension(Path::new("README.md")));
        assert!(!is_binary_extension(Path::new("noext")));
    }

    #[test]
    fn read_for_index_skips_oversized_binary_and_assets_but_keeps_name() {
        let root = temp_indexer_root();
        let mut config = KungfuConfig::default();
        config.index.max_file_bytes = 16;

        let big = root.join("big.txt");
        std::fs::write(&big, vec![b'x'; 64]).unwrap();
        let bin = root.join("blob.txt"); // NUL content, under cap → caught by sniff
        std::fs::write(&bin, b"ok\0binary").unwrap();
        // A binary-extension asset is skipped regardless of size — even tiny text bytes.
        let photo = root.join("photo.png");
        std::fs::write(&photo, b"not really png").unwrap();
        let small = root.join("small.txt");
        std::fs::write(&small, b"hello").unwrap();

        let store = JsonStore::new(&root.join(".kungfu").join("index"));
        let indexer = Indexer::new(&root, config, &store);

        assert!(matches!(
            indexer.read_for_index(&big).unwrap(),
            ReadOutcome::Skipped {
                reason: "oversized",
                ..
            }
        ));
        assert!(matches!(
            indexer.read_for_index(&bin).unwrap(),
            ReadOutcome::Skipped {
                reason: "binary",
                ..
            }
        ));
        assert!(matches!(
            indexer.read_for_index(&photo).unwrap(),
            ReadOutcome::Skipped {
                reason: "binary-ext",
                ..
            }
        ));
        assert!(matches!(
            indexer.read_for_index(&small).unwrap(),
            ReadOutcome::Full(_)
        ));

        // A skipped file is still recorded by name, with a size-based fingerprint.
        let entry = indexer.name_only_entry(&big, 64);
        assert_eq!(entry.path, "big.txt");
        assert_eq!(entry.hash, "skip:64");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn full_index_builds_call_relations() {
        let root = temp_indexer_root();
        // Caller and callee in different files: cross_file_only (the default)
        // keeps exactly this kind of edge.
        std::fs::write(root.join("lib.rs"), "fn helper() {}\nfn shared() {} \n").unwrap();
        std::fs::write(root.join("main.rs"), "fn run() { helper(); }\n").unwrap();

        let store = fresh_store(&root);
        let mut indexer = Indexer::new(&root, KungfuConfig::default(), &store);
        indexer.index_full().unwrap();

        let symbols = store.load_symbols().unwrap();
        let relations = store.load_relations().unwrap();
        let id = |name: &str| {
            symbols
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.id.clone())
        };
        let run = id("run").expect("run symbol");
        let helper = id("helper").expect("helper symbol");

        // run() → helper() is a unique cross-file free call: must be linked.
        assert!(
            relations.iter().any(|r| r.kind == RelationKind::Calls
                && r.source_id == run
                && r.target_id == helper),
            "expected Calls edge run→helper, got {:?}",
            relations
                .iter()
                .filter(|r| r.kind == RelationKind::Calls)
                .collect::<Vec<_>>()
        );
        // No call to shared(): it must not appear as a Calls target.
        let shared = id("shared").expect("shared symbol");
        assert!(!relations
            .iter()
            .any(|r| r.kind == RelationKind::Calls && r.target_id == shared));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn incremental_with_no_changes_preserves_relations() {
        let root = temp_indexer_root();
        std::fs::write(root.join("lib.rs"), "fn helper() {}\n").unwrap();
        std::fs::write(root.join("main.rs"), "fn run() { helper(); }\n").unwrap();
        let store = fresh_store(&root);
        let mut indexer = Indexer::new(&root, KungfuConfig::default(), &store);

        indexer.index_full().unwrap();
        let calls_after_full = store
            .load_relations()
            .unwrap()
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .count();
        assert_eq!(calls_after_full, 1, "full index should link run→helper");

        // A no-op incremental (no file changed) must NOT wipe the graph — regression for the
        // bug where relations were rebuilt only from re-parsed files, zeroing them on no-ops.
        indexer.index_incremental().unwrap();
        let calls_after_incremental = store
            .load_relations()
            .unwrap()
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .count();
        assert_eq!(
            calls_after_incremental, calls_after_full,
            "incremental with no changes must preserve Calls relations"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn incremental_with_no_changes_preserves_memories() {
        let root = temp_indexer_root();
        std::fs::write(
            root.join("lib.rs"),
            "// TODO: tighten the retry budget\nfn helper() {}\n",
        )
        .unwrap();
        let index_dir = root.join(".kungfu").join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let store = JsonStore::new(&index_dir);
        let mut indexer = Indexer::new(&root, KungfuConfig::default(), &store);

        indexer.index_full().unwrap();
        let memories_after_full = store.load_memories().unwrap().len();
        assert!(
            memories_after_full > 0,
            "full index should extract the TODO comment memory"
        );

        // Same bug class as the relations wipe: memories were rebuilt only from
        // re-parsed files, so a no-op incremental zeroed comment/doc memories.
        indexer.index_incremental().unwrap();
        let memories_after_incremental = store.load_memories().unwrap().len();
        assert_eq!(
            memories_after_incremental, memories_after_full,
            "incremental with no changes must preserve memories"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn index_only_touches_just_the_given_file() {
        let root = temp_indexer_root();
        std::fs::write(root.join("a.rs"), "fn helper() {}\n").unwrap();
        std::fs::write(root.join("c.rs"), "fn run() { helper(); }\n").unwrap();
        std::fs::write(root.join("b.rs"), "fn unrelated() {}\n").unwrap();
        let store = fresh_store(&root);
        let mut indexer = Indexer::new(&root, KungfuConfig::default(), &store);

        indexer.index_full().unwrap();
        let calls_before = store
            .load_relations()
            .unwrap()
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .count();
        assert_eq!(calls_before, 1);

        // Edit only b.rs, reindex only b.rs — a.rs's graph must survive,
        // and the new symbol must become searchable.
        std::fs::write(root.join("b.rs"), "fn unrelated() {}\nfn fresh_fn() {}\n").unwrap();
        indexer.index_only(&["b.rs".to_string()]).unwrap();

        let symbols = store.load_symbols().unwrap();
        assert!(
            symbols.iter().any(|s| s.name == "fresh_fn"),
            "new symbol from the targeted file must be indexed"
        );
        let calls_after = store
            .load_relations()
            .unwrap()
            .iter()
            .filter(|r| r.kind == RelationKind::Calls)
            .count();
        assert_eq!(
            calls_after, calls_before,
            "untouched file's call graph must survive a targeted reindex"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn full_index_builds_ts_call_relations() {
        let root = temp_indexer_root();
        std::fs::write(root.join("util.ts"), "function helper() {}\n").unwrap();
        std::fs::write(root.join("app.ts"), "function run() { helper(); }\n").unwrap();
        let store = fresh_store(&root);
        let mut indexer = Indexer::new(&root, KungfuConfig::default(), &store);
        indexer.index_full().unwrap();

        let symbols = store.load_symbols().unwrap();
        let relations = store.load_relations().unwrap();
        let id = |name: &str| {
            symbols
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.id.clone())
        };
        let run = id("run").expect("run symbol");
        let helper = id("helper").expect("helper symbol");
        assert!(
            relations.iter().any(|r| r.kind == RelationKind::Calls
                && r.source_id == run
                && r.target_id == helper),
            "TS call graph must link run→helper, got {:?}",
            relations
                .iter()
                .filter(|r| r.kind == RelationKind::Calls)
                .collect::<Vec<_>>()
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ubiquitous_callee_is_not_linked_even_when_unique() {
        // A project defines exactly one `len`; a call to `len` in another file
        // must still not be attributed to it — the name is stop-listed.
        let root = temp_indexer_root();
        std::fs::write(root.join("a.rs"), "fn len() -> usize { 0 }\n").unwrap();
        std::fs::write(root.join("b.rs"), "fn run() { let _ = len(); }\n").unwrap();
        let store = fresh_store(&root);
        let mut indexer = Indexer::new(&root, KungfuConfig::default(), &store);
        indexer.index_full().unwrap();

        let relations = store.load_relations().unwrap();
        assert!(
            !relations.iter().any(|r| r.kind == RelationKind::Calls),
            "stop-listed callee must produce no Calls edge"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn same_file_calls_are_dropped_by_default_kept_when_configured() {
        let root = temp_indexer_root();
        std::fs::write(
            root.join("lib.rs"),
            "fn helper() {}\nfn run() { helper(); }\n",
        )
        .unwrap();

        // Default: cross_file_only = true → the same-file edge is not stored.
        let store = fresh_store(&root);
        let mut indexer = Indexer::new(&root, KungfuConfig::default(), &store);
        indexer.index_full().unwrap();
        assert!(
            !store
                .load_relations()
                .unwrap()
                .iter()
                .any(|r| r.kind == RelationKind::Calls),
            "same-file call must be dropped when cross_file_only is on (default)"
        );

        // Opt out: cross_file_only = false keeps the same-file edge.
        let mut config = KungfuConfig::default();
        config.call_graph.cross_file_only = false;
        let mut indexer = Indexer::new(&root, config, &store);
        indexer.index_full().unwrap();
        assert_eq!(
            store
                .load_relations()
                .unwrap()
                .iter()
                .filter(|r| r.kind == RelationKind::Calls)
                .count(),
            1,
            "cross_file_only = false must keep the same-file edge"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn frequency_cutoff_drops_utility_noise_callees() {
        let root = temp_indexer_root();
        std::fs::write(root.join("util.rs"), "fn tiny_helper() {}\n").unwrap();
        for i in 0..3 {
            std::fs::write(
                root.join(format!("caller{i}.rs")),
                format!("fn run{i}() {{ tiny_helper(); }}\n"),
            )
            .unwrap();
        }

        // Cutoff above the fan-in: all 3 edges survive.
        let store = fresh_store(&root);
        let mut config = KungfuConfig::default();
        config.call_graph.max_caller_files = 3;
        let mut indexer = Indexer::new(&root, config, &store);
        let stats = indexer.index_full().unwrap();
        assert_eq!(
            store
                .load_relations()
                .unwrap()
                .iter()
                .filter(|r| r.kind == RelationKind::Calls)
                .count(),
            3
        );
        assert_eq!(stats.call_edges_filtered, 0);
        assert!(
            store
                .load_call_graph_meta()
                .frequency_dropped_callees
                .is_empty(),
            "nothing dropped — the meta shard must record no names"
        );

        // Cutoff below the fan-in: the callee is utility-noise, edges dropped.
        let mut config = KungfuConfig::default();
        config.call_graph.max_caller_files = 2;
        let mut indexer = Indexer::new(&root, config, &store);
        let stats = indexer.index_full().unwrap();
        assert!(
            !store
                .load_relations()
                .unwrap()
                .iter()
                .any(|r| r.kind == RelationKind::Calls),
            "callee invoked from more files than max_caller_files must lose its edges"
        );
        assert_eq!(stats.call_edges_filtered, 3);
        assert_eq!(
            store.load_call_graph_meta().frequency_dropped_callees,
            vec!["tiny_helper".to_string()],
            "the dropped callee's name must be recorded in call_graph_meta.json"
        );

        // Incremental run: dropped edges are gone from disk, so the recount
        // cannot re-detect the callee — the merged shard must keep the name.
        let mut config = KungfuConfig::default();
        config.call_graph.max_caller_files = 2;
        let mut indexer = Indexer::new(&root, config, &store);
        indexer.index_incremental().unwrap();
        assert_eq!(
            store.load_call_graph_meta().frequency_dropped_callees,
            vec!["tiny_helper".to_string()],
            "incremental runs must not lose previously recorded dropped callees"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn disabled_call_graph_persists_no_call_edges() {
        let root = temp_indexer_root();
        std::fs::write(root.join("a.rs"), "fn helper() {}\n").unwrap();
        std::fs::write(root.join("b.rs"), "fn run() { helper(); }\n").unwrap();
        let store = fresh_store(&root);

        // Build with the graph on, then reindex with it off: even merged/old
        // edges must disappear.
        let mut indexer = Indexer::new(&root, KungfuConfig::default(), &store);
        indexer.index_full().unwrap();
        assert!(store
            .load_relations()
            .unwrap()
            .iter()
            .any(|r| r.kind == RelationKind::Calls));

        let mut config = KungfuConfig::default();
        config.call_graph.enabled = false;
        let mut indexer = Indexer::new(&root, config, &store);
        indexer.index_incremental().unwrap();
        assert!(
            !store
                .load_relations()
                .unwrap()
                .iter()
                .any(|r| r.kind == RelationKind::Calls),
            "call_graph.enabled = false must strip Calls relations on any indexing run"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn index_writes_schema_version() {
        let root = temp_indexer_root();
        std::fs::write(root.join("lib.rs"), "fn solo() {}\n").unwrap();
        let store = fresh_store(&root);
        assert_eq!(store.load_schema_version(), None);

        let mut indexer = Indexer::new(&root, KungfuConfig::default(), &store);
        indexer.index_full().unwrap();
        assert_eq!(
            store.load_schema_version(),
            Some(kungfu_storage::INDEX_SCHEMA_VERSION)
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
