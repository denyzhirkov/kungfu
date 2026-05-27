use crate::params::{parse_budget, SymbolBudgetParam};
use crate::KungfuMcp;

pub(crate) fn callers(mcp: &KungfuMcp, params: SymbolBudgetParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let name = params.name.clone();
    let scope = params.scope.clone();
    mcp.cached_scoped("callers", &name, &budget_str, scope.as_deref(), || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let results = service.callers(&name, budget).map_err(|e| e.to_string())?;
        let items: Vec<_> = results
            .iter()
            .map(|(sym, reason)| {
                serde_json::json!({
                    "name": sym.name,
                    "kind": sym.kind.to_string(),
                    "path": sym.path,
                    "line": sym.span.start_line,
                    "signature": sym.signature,
                    "reason": reason,
                })
            })
            .collect();
        serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
    })
}

pub(crate) fn callees(mcp: &KungfuMcp, params: SymbolBudgetParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let name = params.name.clone();
    let scope = params.scope.clone();
    mcp.cached_scoped("callees", &name, &budget_str, scope.as_deref(), || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let results = service.callees(&name, budget).map_err(|e| e.to_string())?;
        let items: Vec<_> = results
            .iter()
            .map(|(sym, reason)| {
                serde_json::json!({
                    "name": sym.name,
                    "kind": sym.kind.to_string(),
                    "path": sym.path,
                    "line": sym.span.start_line,
                    "signature": sym.signature,
                    "reason": reason,
                })
            })
            .collect();
        serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
    })
}
