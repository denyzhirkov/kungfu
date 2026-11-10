use crate::params::{parse_budget, QueryParam};
use crate::KungfuMcp;

pub(crate) fn find_symbol(mcp: &KungfuMcp, params: QueryParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let query = params.query.clone();
    let scope = params.scope.clone();
    mcp.cached_scoped("find_symbol", &query, &budget_str, scope.as_deref(), || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let results = service
            .find_symbol(&query, budget)
            .map_err(|e| e.to_string())?;
        let items: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.item.name,
                    "kind": r.item.kind.to_string(),
                    "path": r.item.path,
                    "signature": r.item.signature,
                    "line": r.item.span.start_line,
                    "score": r.score,
                })
            })
            .collect();
        if items.is_empty() {
            return serde_json::to_string_pretty(&serde_json::json!({
                "results": [],
                "hint": format!("no symbol name matches '{query}' — for concept-level lookup try semantic_search; for file paths try find_files"),
            }))
            .map_err(|e| e.to_string());
        }
        serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
    })
}

pub(crate) fn find_files(mcp: &KungfuMcp, params: QueryParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let query = params.query.clone();
    let scope = params.scope.clone().filter(|s| !s.is_empty());
    mcp.cached_scoped("find_files", &query, &budget_str, scope.as_deref(), || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let results = service
            .find_files(&query, budget, scope.as_deref())
            .map_err(|e| e.to_string())?;
        let items: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.item.path,
                    "language": r.item.language,
                    "score": r.score,
                    "match": r.matched.to_string(),
                })
            })
            .collect();
        if items.is_empty() {
            let place = scope
                .as_deref()
                .map(|s| format!(" in scope '{s}'"))
                .unwrap_or_default();
            return serde_json::to_string_pretty(&serde_json::json!({
                "results": [],
                "hint": format!("no file path or contained symbol matches '{query}'{place} — try semantic_search (concepts) or find_symbol (names)"),
            }))
            .map_err(|e| e.to_string());
        }
        serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
    })
}

pub(crate) fn semantic_search(mcp: &KungfuMcp, params: QueryParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let query = params.query.clone();
    let scope = params.scope.clone();
    mcp.cached_scoped(
        "semantic_search",
        &query,
        &budget_str,
        scope.as_deref(),
        || {
            let budget = parse_budget(Some(&budget_str));
            let service = mcp.service()?;
            let result = service
                .semantic_search(&query, budget)
                .map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        },
    )
}
