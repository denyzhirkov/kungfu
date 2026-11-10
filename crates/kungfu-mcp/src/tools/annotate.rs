use crate::params::AnnotateFileParam;
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
