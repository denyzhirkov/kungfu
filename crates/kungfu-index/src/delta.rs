//! Carry the untouched part of the index across a targeted reindex.
//!
//! `index_only` and `index_incremental` re-parse only dirty files (changed,
//! new or removed paths). Everything else is copied from the previous index,
//! but a file id is a content hash, so every edit renames the dirty file and
//! all of its symbols. Edges from untouched files that point into a dirty file,
//! or whose callee name changed its definition count, must therefore be
//! retargeted the way a full index would resolve them, not copied verbatim.
//!
//! An untouched importer whose target id has no known successor (deleted,
//! byte-identical copies edited apart, or only some copies touched) is
//! re-parsed instead.
//!
//! The result matches a full index except for these known gaps, closed by the
//! next full index. Raw imports and call sites of untouched files are not
//! persisted, so their edges are only retargeted or dropped, never re-derived:
//! - Calls: an untouched caller gains no edge to a callable whose name became
//!   unique (a new function name, or the other definition of a duplicate
//!   removed).
//! - Imports: import resolution falls back in stages (exact path, suffix,
//!   stem), so a new file can become a better match for an untouched
//!   importer, which keeps its old target (or none) instead.
//! - With `cross_file_only = false`, a same-file call edge is kept even when
//!   its name's definition count changed elsewhere: whether it came from the
//!   local-definition rule or the unique-name rule is not recorded.
//! - Byte-identical files share an id. A re-parsed importer that belongs to
//!   such a set and was itself changed on disk without a reindex splits the
//!   set, and the set's own importers lose their edge.

use kungfu_types::file::FileEntry;
use kungfu_types::relation::{Relation, RelationKind};
use kungfu_types::symbol::{Symbol, SymbolKind};
use std::collections::{HashMap, HashSet};

/// The index before and after a targeted reindex.
pub(crate) struct Transition<'a> {
    pub old_files: &'a [FileEntry],
    pub old_symbols: &'a [Symbol],
    pub new_files: &'a [FileEntry],
    pub new_symbols: &'a [Symbol],
    /// Paths whose old data was replaced (re-parsed) or removed.
    pub dirty: &'a HashSet<String>,
}

/// Ids of the files carried over untouched.
pub(crate) fn kept_file_ids<'a>(
    files: &'a [FileEntry],
    dirty: &HashSet<String>,
) -> HashSet<&'a str> {
    files
        .iter()
        .filter(|f| !dirty.contains(&f.path))
        .map(|f| f.id.as_str())
        .collect()
}

/// Old symbols of untouched files, in their old order. One pass over the
/// symbol table; a file id shared by several paths is copied once.
pub(crate) fn kept_symbols(old: &[Symbol], kept: &HashSet<&str>) -> Vec<Symbol> {
    old.iter()
        .filter(|s| kept.contains(s.file_id.as_str()))
        .cloned()
        .collect()
}

/// Old id of each dirty path → its new id. `None` when the path is gone or
/// the dirty paths sharing the old id (identical content) now differ.
fn renamed_ids<'a>(
    old_files: &'a [FileEntry],
    new_files: &'a [FileEntry],
    dirty: &HashSet<String>,
    kept: &HashSet<&str>,
) -> HashMap<&'a str, Option<&'a str>> {
    let new_id_of: HashMap<&str, &str> = new_files
        .iter()
        .filter(|f| dirty.contains(&f.path))
        .map(|f| (f.path.as_str(), f.id.as_str()))
        .collect();
    let mut renamed: HashMap<&str, Option<&str>> = HashMap::new();
    for f in old_files {
        if !dirty.contains(&f.path) || kept.contains(f.id.as_str()) {
            continue;
        }
        let new_id = new_id_of.get(f.path.as_str()).copied();
        // Copies that share the old id only conflict when their successors
        // differ (edited apart, or one gone); copies re-parsed unchanged agree.
        renamed
            .entry(f.id.as_str())
            .and_modify(|e| {
                if *e != new_id {
                    *e = None
                }
            })
            .or_insert(new_id);
    }
    renamed
}

/// Untouched files that import an old id whose successor is unknown: no
/// single successor (see [`renamed_ids`]), or the id is shared by a dirty path
/// and an untouched byte-identical copy, so which copy they imported is not
/// recorded. Resolving that needs their raw imports; the caller re-parses them.
pub(crate) fn orphaned_importers(
    old_files: &[FileEntry],
    old_relations: &[Relation],
    new_files: &[FileEntry],
    dirty: &HashSet<String>,
) -> Vec<String> {
    let kept = kept_file_ids(new_files, dirty);
    let renamed = renamed_ids(old_files, new_files, dirty, &kept);
    let split: HashSet<&str> = old_files
        .iter()
        .filter(|f| dirty.contains(&f.path) && kept.contains(f.id.as_str()))
        .map(|f| f.id.as_str())
        .collect();
    let importers: HashSet<&str> = old_relations
        .iter()
        .filter(|r| {
            r.kind == RelationKind::Imports
                && kept.contains(r.source_id.as_str())
                && (matches!(renamed.get(r.target_id.as_str()), Some(None))
                    || split.contains(r.target_id.as_str()))
        })
        .map(|r| r.source_id.as_str())
        .collect();
    if importers.is_empty() {
        return Vec::new();
    }
    new_files
        .iter()
        .filter(|f| !dirty.contains(&f.path) && importers.contains(f.id.as_str()))
        .map(|f| f.path.clone())
        .collect()
}

fn is_callable(s: &Symbol) -> bool {
    matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
}

/// `Imports` and `Calls` edges whose source file is untouched, made consistent
/// with the new state (see the module docs). `old_edges` is the pre-cutoff
/// graph: persisted relations plus the frequency-dropped calls.
pub(crate) fn carry_relations<'r>(
    t: &Transition,
    old_edges: impl Iterator<Item = &'r Relation>,
) -> Vec<Relation> {
    let kept = kept_file_ids(t.new_files, t.dirty);
    let renamed = renamed_ids(t.old_files, t.new_files, t.dirty, &kept);

    // Callable names whose definition count changed: their unique-name
    // resolution may differ now, wherever the definitions live.
    let mut count_delta: HashMap<&str, i64> = HashMap::new();
    for s in t.old_symbols.iter().filter(|s| is_callable(s)) {
        if renamed.contains_key(s.file_id.as_str()) {
            *count_delta.entry(s.name.as_str()).or_default() -= 1;
        }
    }
    for s in t.new_symbols.iter().filter(|s| is_callable(s)) {
        if !kept.contains(s.file_id.as_str()) {
            *count_delta.entry(s.name.as_str()).or_default() += 1;
        }
    }
    let recounted: HashSet<&str> = count_delta
        .into_iter()
        .filter(|(_, d)| *d != 0)
        .map(|(n, _)| n)
        .collect();

    let old_sym: HashMap<&str, &Symbol> =
        t.old_symbols.iter().map(|s| (s.id.as_str(), s)).collect();

    let mut out = Vec::new();
    // Cross-file calls to re-resolve: (edge, caller, callee name).
    let mut pending: Vec<(&Relation, &Symbol, &str)> = Vec::new();
    for r in old_edges {
        match r.kind {
            RelationKind::Imports => {
                if !kept.contains(r.source_id.as_str()) {
                    continue;
                }
                // The importer is untouched and the path set decides import
                // resolution, so the edge still points at the same path.
                let target = if kept.contains(r.target_id.as_str()) {
                    Some(r.target_id.as_str())
                } else {
                    renamed.get(r.target_id.as_str()).copied().flatten()
                };
                if let Some(target) = target.filter(|t| *t != r.source_id) {
                    out.push(Relation {
                        target_id: target.to_string(),
                        ..r.clone()
                    });
                }
            }
            RelationKind::Calls => {
                let (Some(caller), Some(callee)) = (
                    old_sym.get(r.source_id.as_str()),
                    old_sym.get(r.target_id.as_str()),
                ) else {
                    continue;
                };
                if !kept.contains(caller.file_id.as_str()) {
                    continue;
                }
                let same_file = caller.file_id == callee.file_id;
                if kept.contains(callee.file_id.as_str())
                    && (same_file || !recounted.contains(callee.name.as_str()))
                {
                    out.push(r.clone());
                } else {
                    // A cross-file edge only ever comes from the unique-name rule
                    // (an untouched caller's own definitions did not change).
                    pending.push((r, caller, callee.name.as_str()));
                }
            }
            // TestFor/ConfigFor are rebuilt from the file list; the indexer
            // emits no other kind, so a full index would not have them either.
            _ => {}
        }
    }

    if !pending.is_empty() {
        let names: HashSet<&str> = pending.iter().map(|(_, _, n)| *n).collect();
        let mut candidates: HashMap<&str, Vec<&Symbol>> = HashMap::new();
        for s in t.new_symbols {
            if is_callable(s) && names.contains(s.name.as_str()) {
                candidates.entry(s.name.as_str()).or_default().push(s);
            }
        }
        for (r, caller, name) in pending {
            if let Some([only]) = candidates.get(name).map(Vec::as_slice) {
                if only.id != caller.id {
                    out.push(Relation {
                        target_id: only.id.clone(),
                        ..r.clone()
                    });
                }
            }
        }
    }
    out
}
