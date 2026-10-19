use anyhow::Result;
use chrono::Utc;
use kungfu_config::KungfuConfig;
use kungfu_parse::{CommentKind, Parser, RawCall, RawComment, RawImport};
use kungfu_storage::JsonStore;
use kungfu_types::file::{FileEntry, Language, TreeState};
use kungfu_types::memory::{MemoryEntry, MemoryKind};
use kungfu_types::relation::{CallGraphMeta, Relation, RelationKind};
use kungfu_types::symbol::Symbol;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tracing::{debug, info, warn};

use crate::{delta, scanner};

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

/// A file's state relative to the previous index, from [`Indexer::probe_all`].
enum Probe {
    Unchanged(String),
    Changed,
    Unreadable(std::io::Error),
}

/// Below this many files a thread spawn costs more than it saves.
const PROBE_PARALLEL_MIN_FILES: usize = 1024;
const PROBE_MAX_WORKERS: usize = 8;

/// Decide whether to read a file's full content. Known binary/asset extensions
/// and files over `max_file_bytes` are never read (cheap checks only); remaining
/// files are read up to the cap and NUL-sniffed, dropping binary content. Either
/// way they are still recorded by name.
fn read_for_index(path: &Path, max_file_bytes: u64) -> std::io::Result<ReadOutcome> {
    let size = std::fs::metadata(path)?.len();
    if is_binary_extension(path) {
        return Ok(ReadOutcome::Skipped {
            size,
            reason: "binary-ext",
        });
    }
    if size > max_file_bytes {
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

/// Parse output of the files a run (re)indexed.
#[derive(Default)]
struct Reparsed {
    symbols: Vec<Symbol>,
    imports: Vec<(String, Vec<RawImport>)>,
    comments: Vec<(String, Vec<RawComment>)>,
    calls: Vec<(String, Vec<RawCall>)>,
}

impl Reparsed {
    fn absorb(
        &mut self,
        path: &str,
        symbols: Vec<Symbol>,
        imports: Vec<RawImport>,
        comments: Vec<RawComment>,
        calls: Vec<RawCall>,
    ) {
        self.symbols.extend(symbols);
        if !imports.is_empty() {
            self.imports.push((path.to_string(), imports));
        }
        if !comments.is_empty() {
            self.comments.push((path.to_string(), comments));
        }
        if !calls.is_empty() {
            self.calls.push((path.to_string(), calls));
        }
    }
}

/// Sizes of the index a targeted run saved.
struct Merged {
    symbols: usize,
    relations: usize,
    call_edges_filtered: usize,
}

/// What the call-graph frequency cutoff removed.
struct Cut {
    /// Dropped edges, persisted so a targeted run can recount exactly.
    edges: Vec<Relation>,
    /// Sorted, deduplicated names of the dropped callees.
    names: Vec<String>,
}

/// One symbol per id, first wins — the set the store keeps. Byte-identical
/// files share symbol ids; resolving calls over the duplicates would count
/// one definition twice and diverge from what targeted runs see.
fn dedup_symbols(symbols: &mut Vec<Symbol>) {
    let mut seen = HashSet::with_capacity(symbols.len());
    symbols.retain(|s| seen.insert(s.id.clone()));
}

/// A symbol shared by byte-identical files (one id, see [`dedup_symbols`])
/// names the lexicographically first of those paths. Scan order is not
/// stable, and a carried-over path may belong to a copy since deleted.
fn canonical_symbol_paths(symbols: &mut [Symbol], files: &[FileEntry]) {
    let mut first_path: HashMap<&str, &str> = HashMap::with_capacity(files.len());
    for f in files {
        first_path
            .entry(f.id.as_str())
            .and_modify(|path| {
                if f.path.as_str() < *path {
                    *path = f.path.as_str();
                }
            })
            .or_insert(f.path.as_str());
    }
    for s in symbols.iter_mut() {
        if let Some(path) = first_path.get(s.file_id.as_str()) {
            if s.path != *path {
                s.path = (*path).to_string();
            }
        }
    }
}

fn dedup_relations(relations: &mut Vec<Relation>) {
    relations.sort_by(|a, b| {
        (&a.source_id, &a.target_id, &a.kind).cmp(&(&b.source_id, &b.target_id, &b.kind))
    });
    relations.dedup_by(|a, b| {
        a.source_id == b.source_id && a.target_id == b.target_id && a.kind == b.kind
    });
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

        let scan_started = Utc::now();
        let tree = scanner::scan_tree(&self.root, &self.config)?;
        let paths = tree.files;
        let mut files = Vec::new();
        let mut fingerprints = HashMap::new();
        let mut parsed = Reparsed::default();

        for path in &paths {
            match self.index_file(path) {
                Ok((entry, symbols, imports, comments, calls)) => {
                    fingerprints.insert(entry.path.clone(), entry.hash.clone());
                    parsed.absorb(&entry.path, symbols, imports, comments, calls);
                    files.push(entry);
                }
                Err(e) => {
                    warn!("failed to index {}: {}", path.display(), e);
                }
            }
        }
        let mut all_symbols = parsed.symbols;
        dedup_symbols(&mut all_symbols);
        canonical_symbol_paths(&mut all_symbols, &files);

        let mut relations = Self::build_import_relations(&files, &parsed.imports);
        relations.extend(Self::build_layout_relations(&files));
        relations.extend(Self::build_call_relations(
            &self.config.call_graph,
            &files,
            &all_symbols,
            &parsed.calls,
        ));
        let cut =
            Self::filter_call_graph_noise(&self.config.call_graph, &mut relations, &all_symbols);
        let mut memories = Self::build_memories(&parsed.comments);
        let doc_memories = self.scan_docs();
        memories.extend(doc_memories);

        let stats = IndexStats {
            total_files: files.len(),
            new_files: files.len(),
            changed_files: 0,
            removed_files: 0,
            symbols_extracted: all_symbols.len(),
            call_edges_filtered: cut.edges.len(),
        };

        self.apply_annotations(&mut files);
        self.store.save_files(&files)?;
        self.store.save_symbols(all_symbols)?;
        self.store.save_relations(&relations)?;
        self.store.save_dropped_calls(&cut.edges)?;
        self.persist_call_graph_meta(cut.names, false)?;
        self.store.save_fingerprints(&fingerprints)?;
        let memory_count = memories.len();
        self.store.save_memories(memories)?;
        self.store.save_schema_version()?;
        self.save_scanned_tree_state(scan_started, &tree.dirs)?;

        info!(
            "indexed {} files, {} symbols, {} relations, {} memories",
            stats.total_files,
            stats.symbols_extracted,
            relations.len(),
            memory_count
        );
        Ok(stats)
    }

    pub fn index_incremental(&mut self) -> Result<IndexStats> {
        let old_fingerprints = self.store.load_fingerprints()?;
        let old_files = self.store.files_arc()?;
        let old_by_path: HashMap<&str, &FileEntry> =
            old_files.iter().map(|f| (f.path.as_str(), f)).collect();

        let scan_started = Utc::now();
        let tree = scanner::scan_tree(&self.root, &self.config)?;
        let paths = tree.files;

        let mut new_fingerprints = HashMap::new();
        let mut new_files = Vec::new();
        let mut dirty: HashSet<String> = HashSet::new();
        let mut reparsed = Reparsed::default();

        let mut stats = IndexStats {
            total_files: 0,
            new_files: 0,
            changed_files: 0,
            removed_files: 0,
            symbols_extracted: 0,
            call_edges_filtered: 0,
        };

        // Build set of current paths
        let current_paths: HashSet<String> = paths
            .iter()
            .filter_map(|p| p.strip_prefix(&self.root).ok())
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let probes = self.probe_all(&paths, &old_fingerprints);
        for (path, (rel_path, probe)) in paths.iter().zip(probes) {
            match probe {
                Probe::Unchanged(hash) => {
                    if let Some(old_file) = old_by_path.get(rel_path.as_str()) {
                        new_files.push((*old_file).clone());
                    }
                    new_fingerprints.insert(rel_path, hash);
                    continue;
                }
                Probe::Changed => {}
                Probe::Unreadable(e) => {
                    warn!("cannot read {}: {}", path.display(), e);
                    dirty.insert(rel_path);
                    continue;
                }
            }
            // Read again: the probe drops content so a mass change (a branch
            // switch) does not hold the whole tree in memory.
            let read = match self.read_for_index(path) {
                Ok(read) => read,
                Err(e) => {
                    warn!("cannot read {}: {}", path.display(), e);
                    dirty.insert(rel_path);
                    continue;
                }
            };
            if old_fingerprints.contains_key(&rel_path) {
                stats.changed_files += 1;
            } else {
                stats.new_files += 1;
            }
            dirty.insert(rel_path);

            match read {
                ReadOutcome::Full(content) => match self.index_file_with_content(path, content) {
                    Ok((entry, symbols, imports, comments, calls)) => {
                        new_fingerprints.insert(entry.path.clone(), entry.hash.clone());
                        reparsed.absorb(&entry.path, symbols, imports, comments, calls);
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
        for f in old_files.iter() {
            if !current_paths.contains(&f.path) {
                dirty.insert(f.path.clone());
            }
        }

        stats.total_files = new_files.len();
        self.reparse_orphaned_importers(
            &mut new_files,
            &mut dirty,
            &mut reparsed,
            &mut new_fingerprints,
        )?;
        let merged =
            self.save_targeted(&mut new_files, &dirty, reparsed, &new_fingerprints, |m| {
                !dirty.contains(&m.path) && current_paths.contains(&m.path)
            })?;
        stats.symbols_extracted = merged.symbols;
        stats.call_edges_filtered = merged.call_edges_filtered;
        self.save_scanned_tree_state(scan_started, &tree.dirs)?;

        info!(
            "incremental index: {} total, {} new, {} changed, {} removed, {} symbols, {} relations",
            stats.total_files,
            stats.new_files,
            stats.changed_files,
            stats.removed_files,
            stats.symbols_extracted,
            merged.relations
        );
        Ok(stats)
    }

    /// Index only the specified files (by relative path), keeping everything else unchanged.
    pub fn index_only(&mut self, changed_paths: &[String]) -> Result<IndexStats> {
        let started = Utc::now();
        let old_fingerprints = self.store.load_fingerprints()?;
        let mut dirty: HashSet<String> = changed_paths.iter().cloned().collect();

        let mut new_fingerprints = old_fingerprints.clone();
        let mut new_files: Vec<FileEntry> = self
            .store
            .files_arc()?
            .iter()
            .filter(|f| !dirty.contains(&f.path))
            .cloned()
            .collect();
        let mut reparsed = Reparsed::default();

        let mut stats = IndexStats {
            total_files: 0,
            new_files: 0,
            changed_files: 0,
            removed_files: 0,
            symbols_extracted: 0,
            call_edges_filtered: 0,
        };

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
                    reparsed.absorb(&entry.path, symbols, imports, comments, calls);
                    new_files.push(entry);
                }
                Err(e) => {
                    warn!("failed to index {}: {}", abs_path.display(), e);
                }
            }
        }

        stats.total_files = new_files.len();
        self.reparse_orphaned_importers(
            &mut new_files,
            &mut dirty,
            &mut reparsed,
            &mut new_fingerprints,
        )?;
        let merged =
            self.save_targeted(&mut new_files, &dirty, reparsed, &new_fingerprints, |m| {
                !dirty.contains(&m.path)
            })?;
        stats.call_edges_filtered = merged.call_edges_filtered;
        self.refresh_tree_state(changed_paths, &new_files, started)?;

        info!(
            "changed-only index: {} changed, {} new, {} removed",
            stats.changed_files, stats.new_files, stats.removed_files
        );
        Ok(stats)
    }

    /// Re-parse the untouched importers of files whose old id lost its single
    /// successor (see [`delta::orphaned_importers`]), so their imports resolve
    /// as a full index would. They join `dirty` and `reparsed`.
    fn reparse_orphaned_importers(
        &mut self,
        new_files: &mut [FileEntry],
        dirty: &mut HashSet<String>,
        reparsed: &mut Reparsed,
        fingerprints: &mut HashMap<String, String>,
    ) -> Result<()> {
        let old_files = self.store.files_arc()?;
        let old_relations = self.store.relations_arc()?;
        let paths = delta::orphaned_importers(&old_files, &old_relations, new_files, dirty);
        for path in paths {
            let abs = self.root.join(&path);
            let (entry, symbols, imports, comments, calls) = match self.index_file(&abs) {
                Ok(parsed) => parsed,
                Err(e) => {
                    warn!("failed to index {}: {}", abs.display(), e);
                    continue;
                }
            };
            let Some(slot) = new_files.iter_mut().find(|f| f.path == path) else {
                continue;
            };
            fingerprints.insert(entry.path.clone(), entry.hash.clone());
            reparsed.absorb(&entry.path, symbols, imports, comments, calls);
            *slot = entry;
            dirty.insert(path);
        }
        Ok(())
    }

    /// Merge a targeted reindex into the stored index and save it. Untouched
    /// files keep their symbols, edges and memories; dirty paths are rebuilt
    /// from `reparsed`. See [`delta`] for how carried edges follow the renamed
    /// (content-hashed) ids of dirty files.
    fn save_targeted(
        &self,
        new_files: &mut [FileEntry],
        dirty: &HashSet<String>,
        reparsed: Reparsed,
        fingerprints: &HashMap<String, String>,
        keep_memory: impl Fn(&MemoryEntry) -> bool,
    ) -> Result<Merged> {
        let cfg = &self.config.call_graph;
        let old_files = self.store.files_arc()?;
        let old_symbols = self.store.symbols_arc()?;
        let old_relations = self.store.relations_arc()?;
        let old_dropped = self.store.load_dropped_calls();

        let mut symbols =
            delta::kept_symbols(&old_symbols, &delta::kept_file_ids(new_files, dirty));
        symbols.extend(reparsed.symbols);
        dedup_symbols(&mut symbols);
        canonical_symbol_paths(&mut symbols, new_files);

        let transition = delta::Transition {
            old_files: &old_files,
            old_symbols: &old_symbols,
            new_files,
            new_symbols: &symbols,
            dirty,
        };
        let mut relations = delta::carry_relations(
            &transition,
            old_relations.iter().chain(old_dropped.iter().flatten()),
        );
        relations.extend(Self::build_import_relations(new_files, &reparsed.imports));
        relations.extend(Self::build_layout_relations(new_files));
        relations.extend(Self::build_call_relations(
            cfg,
            new_files,
            &symbols,
            &reparsed.calls,
        ));
        dedup_relations(&mut relations);
        // Post-merge so the noise rules also govern edges kept from untouched
        // files (a config change takes effect without waiting for a full reindex).
        let cut = Self::filter_call_graph_noise(cfg, &mut relations, &symbols);

        let old_memories = self.store.memories_arc().unwrap_or_default();
        let (kept_memories, replaced): (Vec<&MemoryEntry>, Vec<&MemoryEntry>) =
            old_memories.iter().partition(|m| keep_memory(m));
        // Doc memories come from scan_docs, which only runs on a full index.
        let mut fresh_memories = Self::build_memories(&reparsed.comments);
        for path in dirty {
            let abs = self.root.join(path);
            if is_doc_memory_source(path) && abs.is_file() {
                self.parse_md_file(&abs, &mut fresh_memories);
            }
        }
        // Skip rewriting the memories shard when the dirty files' memories came
        // out identical. Attached doc comments carry the content-hashed symbol
        // id, so this holds mostly for files without such comments.
        let memories_unchanged = replaced.len() == fresh_memories.len()
            && serde_json::to_string(&replaced)? == serde_json::to_string(&fresh_memories)?;

        self.apply_annotations(new_files);
        self.store.save_files(new_files)?;
        let symbol_count = symbols.len();
        self.store.save_symbols(symbols)?;
        self.store.save_relations(&relations)?;
        // Without the dropped-calls sidecar (index from an older binary) the
        // earlier cutoff victims are unknown: keep their names and leave the
        // sidecar to the next full index, which can write a complete one.
        if old_dropped.is_some() {
            self.store.save_dropped_calls(&cut.edges)?;
        }
        self.persist_call_graph_meta(cut.names, old_dropped.is_none())?;
        self.store.save_fingerprints(fingerprints)?;
        if !memories_unchanged {
            let mut memories: Vec<MemoryEntry> = kept_memories.into_iter().cloned().collect();
            memories.extend(fresh_memories);
            self.store.save_memories(memories)?;
        }
        self.store.save_schema_version()?;

        Ok(Merged {
            symbols: symbol_count,
            relations: relations.len(),
            call_edges_filtered: cut.edges.len(),
        })
    }

    fn rel_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    }

    /// Record what a whole-tree scan saw. It covers every change before
    /// `scanned_at`, so earlier targeted refreshes are dropped.
    fn save_scanned_tree_state(
        &self,
        scanned_at: chrono::DateTime<Utc>,
        dirs: &[std::path::PathBuf],
    ) -> Result<()> {
        self.store.save_tree_state(&TreeState {
            scanned_at,
            dirs: dirs.iter().map(|d| self.rel_path(d)).collect(),
            refreshed: HashMap::new(),
        })
    }

    /// After a targeted reindex: re-read files get their own reference time,
    /// newly created ancestor directories are recorded, and each touched
    /// directory whose listing holds nothing unknown to the index is marked
    /// checked. The freshness check then neither re-flags these edits nor
    /// forgets other ones (the scan reference itself does not move). Without a
    /// tree state (index predates it) there is nothing to refresh; the next
    /// full/incremental run creates one.
    fn refresh_tree_state(
        &self,
        changed_paths: &[String],
        files: &[FileEntry],
        started: chrono::DateTime<Utc>,
    ) -> Result<()> {
        let Some(mut state) = (*self.store.tree_state()).clone() else {
            return Ok(());
        };
        let mut dirs: std::collections::BTreeSet<String> = state.dirs.drain(..).collect();
        let indexed: std::collections::HashSet<&str> =
            files.iter().map(|f| f.path.as_str()).collect();
        let mut to_check = std::collections::BTreeSet::new();
        for rel in changed_paths {
            let is_indexed = indexed.contains(rel.as_str());
            if is_indexed {
                state.refreshed.insert(rel.clone(), started);
            } else {
                state.refreshed.remove(rel);
            }
            let mut child = Path::new(rel);
            while let Some(dir) = child.parent() {
                let dir_rel = dir.to_string_lossy().to_string();
                to_check.insert(dir_rel.clone());
                // An already-recorded dir means only its own listing changed;
                // a new one also changed its parent's.
                if !is_indexed || !dirs.insert(dir_rel) {
                    break;
                }
                child = dir;
            }
        }
        for dir in to_check {
            let abs = self.root.join(&dir);
            if !abs.is_dir() {
                continue;
            }
            let checked_at = Utc::now();
            let entries = match scanner::scan_dirs_shallow(&self.root, &[abs], &self.config) {
                Ok(entries) => entries,
                Err(e) => {
                    debug!("cannot list {dir}: {e:#}");
                    continue;
                }
            };
            let all_known = entries.iter().all(|e| {
                let rel = self.rel_path(e);
                indexed.contains(rel.as_str()) || dirs.contains(&rel)
            });
            if all_known {
                state.refreshed.insert(dir, checked_at);
            }
        }
        state.dirs = dirs.into_iter().collect();
        self.store.save_tree_state(&state)
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

    fn read_for_index(&self, path: &Path) -> std::io::Result<ReadOutcome> {
        read_for_index(path, self.config.index.max_file_bytes)
    }

    /// Read and fingerprint every path against the previous fingerprints, in
    /// scan order. Reading and hashing the whole tree dominates an incremental
    /// run, so it is spread over threads. No content is kept; the sequential
    /// parse (one parser) re-reads the changed files.
    fn probe_all(
        &self,
        paths: &[std::path::PathBuf],
        old_fingerprints: &HashMap<String, String>,
    ) -> Vec<(String, Probe)> {
        let max_bytes = self.config.index.max_file_bytes;
        let root = self.root.as_path();
        let probe_chunk = |chunk: &[std::path::PathBuf]| -> Vec<(String, Probe)> {
            chunk
                .iter()
                .map(|path| {
                    let rel_path = path
                        .strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .to_string();
                    let probe = match read_for_index(path, max_bytes) {
                        Err(e) => Probe::Unreadable(e),
                        Ok(read) => {
                            let hash = match &read {
                                ReadOutcome::Full(content) => {
                                    blake3::hash(content).to_hex().to_string()
                                }
                                ReadOutcome::Skipped { size, .. } => format!("skip:{size}"),
                            };
                            if old_fingerprints.get(&rel_path) == Some(&hash) {
                                Probe::Unchanged(hash)
                            } else {
                                Probe::Changed
                            }
                        }
                    };
                    (rel_path, probe)
                })
                .collect()
        };

        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(PROBE_MAX_WORKERS);
        if workers < 2 || paths.len() < PROBE_PARALLEL_MIN_FILES {
            return probe_chunk(paths);
        }
        let probe_chunk = &probe_chunk;
        std::thread::scope(|scope| {
            let handles: Vec<_> = paths
                .chunks(paths.len().div_ceil(workers))
                .map(|chunk| scope.spawn(move || probe_chunk(chunk)))
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
                .collect()
        })
    }

    /// Merge agent annotations (`.kungfu/annotations.json`) into the file
    /// entries. The authored module doc always wins; annotations fill the gaps
    /// with `purpose_source: agent`, downgraded to `agent-stale` when the file
    /// changed since it was annotated (retrieval honesty — a stale description
    /// must be distinguishable from a current one). Runs on every index path,
    /// so annotation edits take effect on the next index of any kind.
    fn apply_annotations(&self, files: &mut [FileEntry]) {
        let annotations = match self.store.annotations().load() {
            Ok(a) => a,
            Err(e) => {
                warn!("skipping annotations merge: {e:#}");
                return;
            }
        };
        if annotations.is_empty() {
            return;
        }
        for file in files.iter_mut() {
            if file.purpose_source.as_deref() == Some("doc") {
                continue;
            }
            if let Some(ann) = annotations.get(&file.path) {
                file.purpose = Some(ann.purpose.clone());
                file.purpose_source = Some(if ann.content_hash == file.hash {
                    "agent".to_string()
                } else {
                    "agent-stale".to_string()
                });
            } else if file.purpose_source.is_some() {
                // Annotation was removed — drop the carried-over agent purpose.
                file.purpose = None;
                file.purpose_source = None;
            }
        }
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
            purpose_source: None,
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
            purpose_source: None,
        };

        let (mut symbols, imports, comments, calls) = if language.is_code() {
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
                    if entry.purpose.is_some() {
                        entry.purpose_source = Some("doc".to_string());
                    }
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

        // Backstop against index blow-ups: a byte-sane file that yields a
        // pathological number of symbols is truncated. The byte cap in
        // `read_for_index` cannot catch this — the file is small.
        let cap = self.config.index.max_symbols_per_file;
        if symbols.len() > cap {
            warn!(
                "{}: {} symbols exceeds max_symbols_per_file ({}); truncating — \
                 raise [index] max_symbols_per_file if this file is legitimately this large",
                rel_path,
                symbols.len(),
                cap
            );
            symbols.truncate(cap);
        }

        entry.tags = crate::file_tags::derive_tags(&rel_path, &imports, &symbols);

        Ok((entry, symbols, imports, comments, calls))
    }

    /// Resolve the collected imports of `file_imports` against all `files`.
    fn build_import_relations(
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

        dedup_relations(&mut relations);
        relations
    }

    /// `TestFor` and `ConfigFor` relations. They derive from the file list
    /// alone, so every run rebuilds them in full.
    fn build_layout_relations(files: &[FileEntry]) -> Vec<Relation> {
        let mut relations = Vec::new();
        Self::build_test_relations(files, &mut relations);
        Self::build_config_relations(files, &mut relations);
        dedup_relations(&mut relations);
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
    /// Returns the edges the frequency cutoff dropped (their count is
    /// `IndexStats::call_edges_filtered`) plus the sorted, deduplicated names
    /// of the dropped callees — persisted via [`Self::persist_call_graph_meta`]
    /// so an empty callers result can honestly say the cutoff fired.
    fn filter_call_graph_noise(
        cfg: &kungfu_config::CallGraphConfig,
        relations: &mut Vec<Relation>,
        symbols: &[Symbol],
    ) -> Cut {
        let none = || Cut {
            edges: Vec::new(),
            names: Vec::new(),
        };
        if !cfg.enabled {
            relations.retain(|r| r.kind != RelationKind::Calls);
            return none();
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
            return none();
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
            return none();
        }

        let name_of: HashMap<&str, &str> = symbols
            .iter()
            .map(|s| (s.id.as_str(), s.name.as_str()))
            .collect();
        let dropped_names: std::collections::BTreeSet<String> = noisy
            .iter()
            .filter_map(|id| name_of.get(id.as_str()).map(|n| (*n).to_string()))
            .collect();

        let (kept, dropped): (Vec<Relation>, Vec<Relation>) = std::mem::take(relations)
            .into_iter()
            .partition(|r| r.kind != RelationKind::Calls || !noisy.contains(r.target_id.as_str()));
        *relations = kept;
        debug!(
            "call graph: dropped {} edges to {} utility-noise callees (called from > {} files)",
            dropped.len(),
            noisy.len(),
            cfg.max_caller_files
        );
        Cut {
            edges: dropped,
            names: dropped_names.into_iter().collect(),
        }
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
        // Directory and grandparent directory of a path, computed once per
        // file: popular stems (`main`, `mod`) pair hundreds of tests with
        // hundreds of sources.
        type Dirs = (String, Option<String>);
        fn dirs(path: &str) -> Dirs {
            let dir = Path::new(path)
                .parent()
                .unwrap_or(Path::new(""))
                .to_string_lossy()
                .to_string();
            let up = Path::new(&dir)
                .parent()
                .map(|p| p.to_string_lossy().to_string());
            (dir, up)
        }
        // `child` lies strictly below `dir`.
        fn is_under(child: &str, dir: &str) -> bool {
            child.len() > dir.len() && child.starts_with(dir) && child.as_bytes()[dir.len()] == b'/'
        }

        // Build stem→file lookup (only non-test source files)
        let mut source_by_stem: HashMap<String, Vec<(&FileEntry, Dirs)>> = HashMap::new();
        for f in files {
            if !is_test_file(&f.path) {
                let stem = extract_stem(&f.path);
                if !stem.is_empty() {
                    source_by_stem
                        .entry(stem)
                        .or_default()
                        .push((f, dirs(&f.path)));
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
                let (test_dir, test_up) = dirs(&f.path);

                // Score candidates by directory proximity, only keep close ones
                let mut scored: Vec<(&FileEntry, u8)> = Vec::new();
                for (source, (src_dir, src_up)) in sources {
                    // Same directory (e.g. foo.rs + foo_test.rs)
                    if *src_dir == test_dir {
                        scored.push((source, 0));
                    // Sibling: tests/test_foo.py ↔ src/foo.py (share parent)
                    } else if matches!((&test_up, src_up), (Some(a), Some(b)) if !a.is_empty() && a == b)
                    {
                        scored.push((source, 1));
                    // Test dir is child of source dir (e.g. src/foo.rs + src/__tests__/foo.test.ts)
                    } else if is_under(&test_dir, src_dir) {
                        scored.push((source, 2));
                    // Source dir is child of test dir parent
                    } else if let Some(tp) = &test_up {
                        if !tp.is_empty() && is_under(src_dir, tp) {
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

        let parent_dir = |path: &str| {
            Path::new(path)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        // Code files by directory, so each config looks at its own dir only.
        let mut code_by_dir: HashMap<String, Vec<&FileEntry>> = HashMap::new();
        for f in files {
            if is_config_file(&f.path) {
                continue;
            }
            let ext = Path::new(&f.path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if Language::from_extension(ext).is_code() {
                code_by_dir.entry(parent_dir(&f.path)).or_default().push(f);
            }
        }

        for config in &config_files {
            let config_dir = parent_dir(&config.path);

            // Skip root-level configs — they relate to everything, which means nothing
            if config_dir.is_empty() {
                continue;
            }

            // Same directory only — no recursive descent
            for f in code_by_dir.get(&config_dir).into_iter().flatten() {
                if f.id != config.id {
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
    fn probe_all_pairs_each_path_with_its_own_content_across_threads() {
        // Above PROBE_PARALLEL_MIN_FILES, so the threaded branch runs (given
        // more than one core); results must stay in path order.
        let root = temp_indexer_root();
        let n = PROBE_PARALLEL_MIN_FILES + 100;
        let mut paths = Vec::with_capacity(n);
        let mut old = HashMap::new();
        for i in 0..n {
            let path = root.join(format!("f{i:05}.txt"));
            let content = format!("file number {i}\n");
            std::fs::write(&path, &content).unwrap();
            // Even files keep their fingerprint, odd ones changed.
            let hash = if i % 2 == 0 {
                blake3::hash(content.as_bytes()).to_hex().to_string()
            } else {
                "stale".to_string()
            };
            old.insert(format!("f{i:05}.txt"), hash);
            paths.push(path);
        }
        let store = JsonStore::new(&root.join(".kungfu").join("index"));
        let indexer = Indexer::new(&root, KungfuConfig::default(), &store);

        let probes = indexer.probe_all(&paths, &old);
        assert_eq!(probes.len(), n);
        for (i, (rel, probe)) in probes.iter().enumerate() {
            assert_eq!(rel, &format!("f{i:05}.txt"));
            match probe {
                Probe::Unchanged(hash) => {
                    assert_eq!(i % 2, 0, "{rel} reported unchanged");
                    assert_eq!(hash, &old[rel]);
                }
                Probe::Changed => assert_eq!(i % 2, 1, "{rel} reported changed"),
                Probe::Unreadable(e) => panic!("{rel}: {e}"),
            }
        }
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
