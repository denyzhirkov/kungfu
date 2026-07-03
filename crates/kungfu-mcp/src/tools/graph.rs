use crate::params::{parse_budget, SymbolBudgetParam};
use crate::KungfuMcp;
use kungfu_core::EmptyCallGraphCause;

pub(crate) fn callers(mcp: &KungfuMcp, params: SymbolBudgetParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let name = params.name.clone();
    let scope = params.scope.clone();
    mcp.cached_scoped("callers", &name, &budget_str, scope.as_deref(), || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let results = service.callers(&name, budget).map_err(|e| e.to_string())?;
        if results.is_empty() {
            let cause = service
                .diagnose_empty_callers(&name)
                .map_err(|e| e.to_string())?;
            return call_graph_diagnostic(&service, &name, cause);
        }
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
        graph_response(&service, &name, items)
    })
}

/// Uniform call-graph response shape (the empty diagnostics already use objects,
/// so non-empty results match), with provenance on how edges are derived and
/// which noise filters were active when the graph was built.
fn graph_response(
    service: &kungfu_core::KungfuService,
    name: &str,
    items: Vec<serde_json::Value>,
) -> Result<String, String> {
    serde_json::to_string_pretty(&serde_json::json!({
        "status": "ok",
        "name": name,
        "results": items,
        "provenance": service.call_graph_provenance(),
    }))
    .map_err(|e| e.to_string())
}

/// Tell apart the empty-result cases so the agent is never left guessing:
/// the graph is disabled by config, the queried name is stop-listed by design,
/// the graph was never built, the frequency cutoff dropped the edges, the
/// target is ambiguous (many same-name definitions, edges never stored), or it
/// exists and this symbol simply has no edges.
fn call_graph_diagnostic(
    service: &kungfu_core::KungfuService,
    name: &str,
    cause: EmptyCallGraphCause,
) -> Result<String, String> {
    let body = match cause {
        EmptyCallGraphCause::Disabled => serde_json::json!({
            "status": "call_graph_disabled",
            "name": name,
            "hint": format!(
                "The call graph is disabled by config ([call_graph] enabled = false). Use search_text(\"{}\") to find usages by name, or re-enable and reindex.",
                name
            ),
            "results": [],
        }),
        EmptyCallGraphCause::FilteredUbiquitous => serde_json::json!({
            "status": "filtered_ubiquitous",
            "name": name,
            "hint": format!(
                "\"{}\" is on the ubiquitous-callables stop-list (std/utility name), so call edges to it are never stored. Use search_text(\"{}\") to find usages by name.",
                name, name
            ),
            "results": [],
        }),
        EmptyCallGraphCause::NotIndexed => serde_json::json!({
            "status": "call_graph_not_indexed",
            "name": name,
            "hint": format!(
                "No call relations are built for this project, so callers/callees are unavailable. Use search_text(\"{}\") to find usages by name.",
                name
            ),
            "results": [],
        }),
        EmptyCallGraphCause::FrequencyFiltered { max_caller_files } => serde_json::json!({
            "status": "frequency_filtered",
            "name": name,
            "max_caller_files": max_caller_files,
            "hint": format!(
                "'{}' is called from more than {} distinct files, so its incoming call edges were dropped as utility-noise ([call_graph] max_caller_files = {}). Raise the limit and reindex, or use search_text(\"{}\") to find call sites by name.",
                name, max_caller_files, max_caller_files, name
            ),
            "provenance": service.call_graph_provenance(),
            "results": [],
        }),
        EmptyCallGraphCause::AmbiguousTarget { definitions } => serde_json::json!({
            "status": "ambiguous_target",
            "name": name,
            "definitions": definitions,
            "hint": format!(
                "'{}' has {} same-name definitions — call edges are only stored for unambiguous callees, so none were stored for this name. Use search_text(\"{}\") with the scope parameter (e.g. scope: 'src/subsystem') to list call sites in the area you care about.",
                name, definitions, name
            ),
            "provenance": service.call_graph_provenance(),
            "results": [],
        }),
        EmptyCallGraphCause::NoEdges => serde_json::json!({
            "status": "no_edges",
            "name": name,
            "provenance": service.call_graph_provenance(),
            "results": [],
        }),
    };
    serde_json::to_string_pretty(&body).map_err(|e| e.to_string())
}

pub(crate) fn callees(mcp: &KungfuMcp, params: SymbolBudgetParam) -> Result<String, String> {
    let budget_str = params.budget.as_deref().unwrap_or("small").to_string();
    let name = params.name.clone();
    let scope = params.scope.clone();
    mcp.cached_scoped("callees", &name, &budget_str, scope.as_deref(), || {
        let budget = parse_budget(Some(&budget_str));
        let service = mcp.service()?;
        let results = service.callees(&name, budget).map_err(|e| e.to_string())?;
        if results.is_empty() {
            let cause = service
                .diagnose_empty_callees(&name)
                .map_err(|e| e.to_string())?;
            return call_graph_diagnostic(&service, &name, cause);
        }
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
        graph_response(&service, &name, items)
    })
}
