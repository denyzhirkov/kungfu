use crate::params::{parse_budget, HistoryParam};
use crate::KungfuMcp;

// Not cached: git state (new commits) changes without an index change, so a cached
// answer keyed on the index fingerprint could go stale.
pub(crate) fn history(mcp: &KungfuMcp, params: HistoryParam) -> Result<String, String> {
    let path = params.path.filter(|s| !s.is_empty());
    let symbol = params.symbol.filter(|s| !s.is_empty());
    if path.is_some() == symbol.is_some() {
        return Err("history takes exactly one of `path` or `symbol`".to_string());
    }
    let budget = parse_budget(params.budget.as_deref().or(Some("small")));
    let service = mcp.service()?;
    let result = service
        .history(path.as_deref(), symbol.as_deref(), budget)
        .map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    mcp.record_served("history", &out);
    Ok(out)
}
