use crate::params::{
    parse_budget, CommitContextParam, FilePathParam, PrContextParam, SymbolNameParam,
};
use crate::KungfuMcp;

pub(crate) fn file_history(mcp: &KungfuMcp, params: FilePathParam) -> Result<String, String> {
    let service = mcp.service()?;
    let result = service
        .file_history(&params.path, 10)
        .map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    mcp.record_served("file_history", &out);
    Ok(out)
}

pub(crate) fn symbol_history(mcp: &KungfuMcp, params: SymbolNameParam) -> Result<String, String> {
    let name = params.name.clone();
    mcp.cached("symbol_history", &name, "", || {
        let service = mcp.service()?;
        let result = service.symbol_history(&name).map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    })
}

pub(crate) fn change_timeline(mcp: &KungfuMcp, params: SymbolNameParam) -> Result<String, String> {
    let name = params.name.clone();
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    mcp.cached("change_timeline", &name, &budget_str, || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let result = service
            .change_timeline(&name, budget)
            .map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    })
}

pub(crate) fn commit_context(
    mcp: &KungfuMcp,
    params: CommitContextParam,
) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let hash = params.hash.clone();
    mcp.cached("commit_context", &hash, &budget_str, || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let packet = service
            .commit_context(&hash, budget)
            .map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&packet).map_err(|e| e.to_string())
    })
}

pub(crate) fn pr_context(mcp: &KungfuMcp, params: PrContextParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let num = params.num;
    mcp.cached("pr_context", &num.to_string(), &budget_str, || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let packet = service.pr_context(num, budget).map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&packet).map_err(|e| e.to_string())
    })
}
