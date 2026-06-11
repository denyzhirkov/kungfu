use crate::params::{
    parse_budget, AskContextParam, BudgetParam, DebugTraceParam, FilePathBudgetParam, QueryParam,
    SymbolBudgetParam,
};
use crate::KungfuMcp;

pub(crate) fn ask_context(mcp: &KungfuMcp, params: AskContextParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let query = params.query.clone();
    let scope = params.scope.clone();
    let include = params.include.clone();
    mcp.cached_scoped("ask_context", &query, &budget_str, scope.as_deref(), || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let mut packet = service
            .ask_context(&query, budget)
            .map_err(|e| e.to_string())?;

        // Apply layer filtering if include is specified
        if let Some(ref layers) = include {
            let want_rationale = layers.iter().any(|l| l == "rationale");
            let want_history = layers.iter().any(|l| l == "history");

            if !want_rationale {
                packet.rationale.clear();
                packet.evidence.clear();
            }
            if want_history {
                // Collect history for top matched items
                if let Some(top) = packet.items.first() {
                    let events = service
                        .change_timeline(&top.name, budget)
                        .unwrap_or_default();
                    packet.history = events;
                }
            }
        }

        serde_json::to_string_pretty(&packet).map_err(|e| e.to_string())
    })
}

pub(crate) fn diff_context(mcp: &KungfuMcp, params: BudgetParam) -> Result<String, String> {
    let budget = parse_budget(params.budget.as_deref());
    let service = mcp.service()?;
    let packet = service.diff_context(budget).map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&packet).map_err(|e| e.to_string())?;
    mcp.record_served("diff_context", &out);
    Ok(out)
}

pub(crate) fn explore_symbol(mcp: &KungfuMcp, params: SymbolBudgetParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let name = params.name.clone();
    let scope = params.scope.clone();
    mcp.cached_scoped(
        "explore_symbol",
        &name,
        &budget_str,
        scope.as_deref(),
        || {
            let budget = parse_budget(Some(&budget_str));
            let service = mcp.service()?;
            let result = service
                .explore_symbol(&name, budget)
                .map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
        },
    )
}

pub(crate) fn explore_file(mcp: &KungfuMcp, params: FilePathBudgetParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let path = params.path.clone();
    mcp.cached("explore_file", &path, &budget_str, || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let result = service
            .explore_file(&path, budget)
            .map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    })
}

pub(crate) fn investigate(mcp: &KungfuMcp, params: QueryParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let query = params.query.clone();
    let scope = params.scope.clone();
    mcp.cached_scoped("investigate", &query, &budget_str, scope.as_deref(), || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let result = service
            .investigate(&query, budget)
            .map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    })
}

pub(crate) fn debug_trace(mcp: &KungfuMcp, params: DebugTraceParam) -> Result<String, String> {
    let budget = parse_budget(params.budget.as_deref());
    let service = mcp.service()?;
    let result = service
        .debug_trace(&params.trace, budget)
        .map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    mcp.record_served("debug_trace", &out);
    Ok(out)
}
