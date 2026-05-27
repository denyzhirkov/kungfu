use crate::params::{parse_budget, QueryParam, SymbolNameParam};
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
        serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
    })
}

pub(crate) fn get_symbol(mcp: &KungfuMcp, params: SymbolNameParam) -> Result<String, String> {
    let name = params.name.clone();
    mcp.cached("get_symbol", &name, "", || {
        let service = mcp.service()?;
        match service.get_symbol(&name).map_err(|e| e.to_string())? {
            Some(sym) => serde_json::to_string_pretty(&sym).map_err(|e| e.to_string()),
            None => Ok(format!("Symbol '{}' not found", name)),
        }
    })
}

pub(crate) fn search_text(mcp: &KungfuMcp, params: QueryParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let query = params.query.clone();
    let scope = params.scope.clone();
    mcp.cached_scoped("search_text", &query, &budget_str, scope.as_deref(), || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let results = service
            .search_text(&query, budget)
            .map_err(|e| e.to_string())?;
        let items: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.item.path,
                    "language": r.item.language,
                    "score": r.score,
                })
            })
            .collect();
        serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
    })
}

pub(crate) fn find_files(mcp: &KungfuMcp, params: QueryParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let query = params.query.clone();
    let scope = params.scope.clone();
    mcp.cached_scoped("find_files", &query, &budget_str, scope.as_deref(), || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let results = service
            .search_text(&query, budget)
            .map_err(|e| e.to_string())?;
        let items: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.item.path,
                    "language": r.item.language,
                    "score": r.score,
                })
            })
            .collect();
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

pub(crate) fn find_related_symbols(
    mcp: &KungfuMcp,
    params: SymbolNameParam,
) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let name = params.name.clone();
    mcp.cached("find_related_symbols", &name, &budget_str, || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let sym = service.get_symbol(&name).map_err(|e| e.to_string())?;
        match sym {
            Some(s) => {
                let file_outline = service.file_outline(&s.path).map_err(|e| e.to_string())?;
                let items: Vec<_> = file_outline
                    .symbols
                    .iter()
                    .filter(|os| os.name != name)
                    .take(budget.top_k())
                    .map(|os| {
                        serde_json::json!({
                            "name": os.name,
                            "kind": os.kind,
                            "path": s.path,
                            "line": os.line,
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
            }
            None => Ok(format!("Symbol '{}' not found", name)),
        }
    })
}
