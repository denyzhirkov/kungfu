use crate::params::{AffectedParam, CouplingParam, HotspotsParam};
use crate::KungfuMcp;

pub(crate) fn hotspots(mcp: &KungfuMcp, params: HotspotsParam) -> Result<String, String> {
    let top = params.top.unwrap_or(20);
    let churn = params.churn.unwrap_or(false);
    let files = params.files.unwrap_or(false);
    let service = mcp.service()?;
    let entries = service
        .hotspots(top, churn, files)
        .map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?;
    mcp.record_served("hotspots", &out);
    Ok(out)
}

pub(crate) fn onboard(mcp: &KungfuMcp) -> Result<String, String> {
    mcp.cached("onboard", "", "", || {
        let service = mcp.service()?;
        let info = service.onboard().map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
    })
}

pub(crate) fn affected(mcp: &KungfuMcp, params: AffectedParam) -> Result<String, String> {
    let depth = params.depth.unwrap_or(3);
    let staged = params.staged.unwrap_or(false);
    let name = params.name.clone();
    let cache_key = if staged {
        format!("staged:{}", depth)
    } else {
        format!("{}:{}", name, depth)
    };
    mcp.cached("affected", &cache_key, "", || {
        let service = mcp.service()?;
        let result = if staged {
            service.affected_staged(depth).map_err(|e| e.to_string())?
        } else {
            if name.is_empty() {
                return Err("symbol name required (or set staged=true)".to_string());
            }
            service.affected(&name, depth).map_err(|e| e.to_string())?
        };
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    })
}

pub(crate) fn review(mcp: &KungfuMcp) -> Result<String, String> {
    // No cache — depends on current diff state
    let service = mcp.service()?;
    let result = service.review().map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    mcp.record_served("review", &out);
    Ok(out)
}

pub(crate) fn verify_change(
    mcp: &KungfuMcp,
    params: crate::params::VerifyChangeParam,
) -> Result<String, String> {
    // No cache — depends on current diff state
    let depth = params.depth.unwrap_or(3);
    let service = mcp.service()?;
    let result = service.verify_change(depth).map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    mcp.record_served("verify_change", &out);
    Ok(out)
}

pub(crate) fn embeddings_status(mcp: &KungfuMcp) -> Result<String, String> {
    let service = mcp.service()?;
    let status = service.embeddings_status().map_err(|e| e.to_string())?;
    let mut value = serde_json::to_value(&status).map_err(|e| e.to_string())?;
    value["job_running"] = serde_json::json!(
        crate::cache::EMBED_JOB_RUNNING.load(std::sync::atomic::Ordering::SeqCst)
    );
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

pub(crate) fn coupling(mcp: &KungfuMcp, params: CouplingParam) -> Result<String, String> {
    let top = params.top.unwrap_or(20);
    mcp.cached("coupling", &top.to_string(), "", || {
        let service = mcp.service()?;
        let entries = service.coupling(top).map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
    })
}
