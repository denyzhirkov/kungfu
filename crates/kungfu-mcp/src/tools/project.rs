use crate::params::{parse_budget, FileOutlineParam, ReindexParam, RepoOutlineParam};
use crate::KungfuMcp;
use kungfu_types::Budget;

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
    let embeddings = service.embeddings_status().map_err(|e| e.to_string())?;
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
        "embeddings": {
            "ready": embeddings.ready,
            "feature": embeddings.inference_compiled,
            "model": embeddings.model_id,
            "weights_installed": embeddings.weights_installed,
            "vectors": embeddings.indexed_vectors,
            "file_vectors": embeddings.indexed_file_vectors,
            "symbols": embeddings.indexed_symbols,
            "job_running": crate::cache::EMBED_JOB_RUNNING.load(std::sync::atomic::Ordering::SeqCst),
            "hint": embeddings.hint,
        },
    }))
    .map_err(|e| e.to_string())?;
    mcp.record_served("project_status", &out);
    Ok(out)
}

pub(crate) fn repo_outline(mcp: &KungfuMcp, params: RepoOutlineParam) -> Result<String, String> {
    let budget = parse_budget(params.budget.as_deref());
    let service = mcp.service()?;
    let outline = service.repo_outline(budget).map_err(|e| e.to_string())?;

    let dirs: Vec<_> = outline
        .top_dirs
        .iter()
        .map(|d| serde_json::json!({"path": d.path, "files": d.file_count}))
        .collect();

    let mut value = serde_json::json!({
        "project": outline.project_name,
        "total_files": outline.total_files,
        "total_symbols": outline.total_symbols,
        "languages": outline.languages,
        "directories": dirs,
        "entrypoints": outline.entrypoints,
    });
    if params.include_onboard.unwrap_or(false) {
        // Only what the outline above lacks — languages, dirs, entrypoints and totals
        // are already there.
        let info = service.onboard().map_err(|e| e.to_string())?;
        value["onboard"] = serde_json::json!({
            "primary_language": info.primary_language,
            "architecture": info.architecture,
            "key_symbols": info.key_symbols,
            "naming_style": info.naming_style,
            "test_pattern": info.test_pattern,
            "glossary": info.glossary,
        });
    }
    let out = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    mcp.record_served("repo_outline", &out);
    Ok(out)
}

pub(crate) fn file_outline(mcp: &KungfuMcp, params: FileOutlineParam) -> Result<String, String> {
    let budget = match params.budget.as_deref() {
        None => Budget::Tiny,
        some => parse_budget(some),
    };
    let service = mcp.service()?;
    let result = service
        .file_outline_at(&params.path, budget)
        .map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    mcp.record_served("file_outline", &out);
    Ok(out)
}
