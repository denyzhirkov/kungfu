use crate::params::{parse_budget, BudgetParam, FilePathParam};
use crate::KungfuMcp;

pub(crate) fn project_status(mcp: &KungfuMcp) -> Result<String, String> {
    let service = mcp.service()?;
    let info = service.status().map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&serde_json::json!({
        "project_name": info.project_name,
        "root": info.root,
        "indexed_files": info.indexed_files,
        "indexed_symbols": info.indexed_symbols,
        "languages": info.languages,
        "has_git": info.has_git,
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
        "symbols": symbols,
    }))
    .map_err(|e| e.to_string())?;
    mcp.record_served("file_outline", &out);
    Ok(out)
}
