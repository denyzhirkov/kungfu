use crate::params::{parse_budget, BudgetParam, FilePathParam, ReindexParam};
use crate::KungfuMcp;

/// Agent-driven freshness: reindex exactly the files the agent just touched,
/// instead of waiting for the lazy mtime-based staleness check to guess.
pub(crate) fn reindex(mcp: &KungfuMcp, params: ReindexParam) -> Result<String, String> {
    // Untracked open on purpose: ensure_fresh_index would run its own incremental
    // scan first, defeating the point of the explicit targeted reindex.
    let service = mcp.service_untracked()?;
    let stats = service
        .index_paths(&params.paths)
        .map_err(|e| e.to_string())?;
    if let Ok(mut cache) = mcp.cache.lock() {
        cache.clear();
    }
    // Vectors follow the index, in the background — semantic_search on the
    // edited symbols catches up within moments without blocking this call.
    crate::cache::spawn_embeddings_sync(mcp.project_root.clone());
    serde_json::to_string_pretty(&serde_json::json!({
        "status": "reindexed",
        "paths": params.paths,
        "new_files": stats.new_files,
        "changed_files": stats.changed_files,
        "removed_files": stats.removed_files,
        "symbols_extracted": stats.symbols_extracted,
        "total_files": stats.total_files,
        "call_edges_filtered": stats.call_edges_filtered,
    }))
    .map_err(|e| e.to_string())
}

pub(crate) fn project_status(mcp: &KungfuMcp) -> Result<String, String> {
    let service = mcp.service()?;
    let info = service.status().map_err(|e| e.to_string())?;
    // Cache-only: never a network call inside a tool handler.
    let update = kungfu_update::status_from_cache(&service.config().update);
    let out = serde_json::to_string_pretty(&serde_json::json!({
        "project_name": info.project_name,
        "root": info.root,
        "indexed_files": info.indexed_files,
        "indexed_symbols": info.indexed_symbols,
        "languages": info.languages,
        "has_git": info.has_git,
        "kungfu_version": update.current,
        "update": {
            "latest": update.latest,
            "available": update.update_available,
            "source": update.source.as_str(),
            "hint": update.update_available.then_some(
                "run `kungfu update` in a terminal, then restart this session — a running server keeps the old binary"
            ),
        },
    }))
    .map_err(|e| e.to_string())?;
    mcp.record_served("project_status", &out);
    Ok(out)
}

pub(crate) fn repo_outline(mcp: &KungfuMcp, params: BudgetParam) -> Result<String, String> {
    let budget = parse_budget(params.budget.as_deref());
    let service = mcp.service()?;
    let outline = service.repo_outline(budget).map_err(|e| e.to_string())?;

    let dirs: Vec<_> = outline
        .top_dirs
        .iter()
        .map(|d| serde_json::json!({"path": d.path, "files": d.file_count}))
        .collect();

    let out = serde_json::to_string_pretty(&serde_json::json!({
        "project": outline.project_name,
        "total_files": outline.total_files,
        "total_symbols": outline.total_symbols,
        "languages": outline.languages,
        "directories": dirs,
        "entrypoints": outline.entrypoints,
    }))
    .map_err(|e| e.to_string())?;
    mcp.record_served("repo_outline", &out);
    Ok(out)
}

pub(crate) fn file_outline(mcp: &KungfuMcp, params: FilePathParam) -> Result<String, String> {
    let service = mcp.service()?;
    let outline = service
        .file_outline(&params.path)
        .map_err(|e| e.to_string())?;

    let symbols: Vec<_> = outline
        .symbols
        .iter()
        .map(|s| {
            serde_json::json!({
                "name": s.name,
                "kind": s.kind,
                "signature": s.signature,
                "line": s.line,
                "exported": s.exported,
            })
        })
        .collect();

    let out = serde_json::to_string_pretty(&serde_json::json!({
        "path": outline.path,
        "language": outline.language,
        "purpose": outline.purpose,
        "purpose_source": outline.purpose_source,
        "tags": outline.tags,
        "symbols": symbols,
    }))
    .map_err(|e| e.to_string())?;
    mcp.record_served("file_outline", &out);
    Ok(out)
}
