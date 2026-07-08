use crate::params::{AnnotateFileParam, AnnotationQueueParam};
use crate::KungfuMcp;

pub(crate) fn annotate_file(mcp: &KungfuMcp, params: AnnotateFileParam) -> Result<String, String> {
    let service = mcp.service()?;
    let result = service
        .annotate_file(
            &params.path,
            &params.purpose,
            params.terms.unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
}

pub(crate) fn annotation_queue(
    mcp: &KungfuMcp,
    params: AnnotationQueueParam,
) -> Result<String, String> {
    let service = mcp.service()?;
    let queue = service
        .annotation_queue(params.limit.unwrap_or(10))
        .map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&queue).map_err(|e| e.to_string())?;
    mcp.record_served("annotation_queue", &out);
    Ok(out)
}
