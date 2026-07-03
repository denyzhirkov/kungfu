//! Read-only health checks for the Claude Code integration written by
//! `kungfu init --agent claude`, consumed by `kungfu doctor`.
//!
//! Detection reuses the constants and template renderers from
//! [`crate::templates`] — no marker strings or hook fingerprints are duplicated
//! here. Repairs go through [`crate::agent_init::init_claude_integration`],
//! which doctor's `--fix` calls verbatim (idempotent, merge-safe).

use crate::templates;
use serde_json::Value;
use std::path::Path;

/// Severity of a single doctor check line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    /// Informational — not a health issue (e.g. no integration set up at all).
    Info,
    /// Degraded but working; a next action exists.
    Warning,
    /// Broken; needs user attention.
    Problem,
}

impl CheckStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Info => "info",
            CheckStatus::Warning => "warning",
            CheckStatus::Problem => "problem",
        }
    }

    pub fn needs_attention(&self) -> bool {
        matches!(self, CheckStatus::Warning | CheckStatus::Problem)
    }
}

#[derive(Debug)]
pub struct IntegrationCheck {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug)]
pub struct ClaudeIntegrationHealth {
    /// At least one kungfu artifact (MCP entry, rules block, reindex hook) exists.
    pub configured: bool,
    pub checks: Vec<IntegrationCheck>,
    /// True when re-running the init sync (`kungfu doctor --fix`) would change something.
    pub needs_sync: bool,
}

/// Per-artifact detection result, folded into checks by
/// [`check_claude_integration`] depending on whether the integration as a
/// whole is configured.
enum Artifact {
    Present(CheckStatus, String),
    Absent,
    /// File exists but cannot be interpreted — always a problem.
    Unreadable(String),
}

/// Inspect the three Claude Code integration artifacts under `root`.
///
/// Absence of the whole integration is not a health problem: it yields a
/// single `Info` check. Partial or stale integration yields per-artifact
/// warnings/problems that `kungfu doctor --fix` can repair via the init sync.
pub fn check_claude_integration(root: &Path) -> ClaudeIntegrationHealth {
    let artifacts: [(&'static str, Artifact, &'static str); 3] = [
        (
            "claude_mcp",
            mcp_artifact(root),
            "kungfu server entry missing from .mcp.json",
        ),
        (
            "claude_rules",
            rules_artifact(root),
            "kungfu rules block missing from CLAUDE.md",
        ),
        (
            "claude_hook",
            hook_artifact(root),
            "auto-reindex hook missing from .claude/settings.json",
        ),
    ];

    let configured = artifacts
        .iter()
        .any(|(_, a, _)| matches!(a, Artifact::Present(..)));

    let mut checks = Vec::new();
    for (name, artifact, absent_msg) in artifacts {
        match artifact {
            Artifact::Present(status, detail) => checks.push(IntegrationCheck {
                name,
                status,
                detail,
            }),
            Artifact::Unreadable(detail) => checks.push(IntegrationCheck {
                name,
                status: CheckStatus::Problem,
                detail,
            }),
            Artifact::Absent if configured => checks.push(IntegrationCheck {
                name,
                status: CheckStatus::Warning,
                detail: format!("{absent_msg} — run 'kungfu doctor --fix' to add it"),
            }),
            Artifact::Absent => {}
        }
    }

    let needs_sync = configured && checks.iter().any(|c| c.status != CheckStatus::Ok);

    if !configured {
        checks.push(IntegrationCheck {
            name: "claude_integration",
            status: CheckStatus::Info,
            detail: "no Claude Code integration detected — run 'kungfu init --agent claude' to set it up"
                .to_string(),
        });
    }

    ClaudeIntegrationHealth {
        configured,
        checks,
        needs_sync,
    }
}

fn mcp_artifact(root: &Path) -> Artifact {
    let path = root.join(".mcp.json");
    let content = match read_artifact(&path, ".mcp.json") {
        Ok(Some(c)) => c,
        Ok(None) => return Artifact::Absent,
        Err(a) => return a,
    };
    let doc: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return Artifact::Unreadable(format!(
                ".mcp.json is not valid JSON: {e} — fix the syntax (or remove the file), then re-run 'kungfu doctor'"
            ))
        }
    };
    match doc.get("mcpServers").and_then(|s| s.get("kungfu")) {
        None => Artifact::Absent,
        Some(entry) if *entry == templates::mcp_server_entry() => Artifact::Present(
            CheckStatus::Ok,
            "kungfu MCP server registered in .mcp.json".to_string(),
        ),
        Some(_) => Artifact::Present(
            CheckStatus::Warning,
            "kungfu server entry in .mcp.json differs from the default ('kungfu mcp') — \
             run 'kungfu doctor --fix' to rewrite it (custom entries will be replaced)"
                .to_string(),
        ),
    }
}

fn rules_artifact(root: &Path) -> Artifact {
    let path = root.join("CLAUDE.md");
    let content = match read_artifact(&path, "CLAUDE.md") {
        Ok(Some(c)) => c,
        Ok(None) => return Artifact::Absent,
        Err(a) => return a,
    };
    let Some(start) = content.find(templates::RULES_MARKER_START_PREFIX) else {
        return Artifact::Absent;
    };
    let Some(end_rel) = content[start..].find(templates::RULES_MARKER_END) else {
        return Artifact::Present(
            CheckStatus::Problem,
            format!(
                "CLAUDE.md has '{}' without a matching '{}' — remove or complete the block, then run 'kungfu init --agent claude'",
                templates::RULES_MARKER_START_PREFIX,
                templates::RULES_MARKER_END
            ),
        );
    };
    let end = start + end_rel + templates::RULES_MARKER_END.len();

    let marker_line = content[start..].lines().next().unwrap_or("");
    let version = marker_line
        .strip_prefix(templates::RULES_MARKER_START_PREFIX)
        .and_then(|rest| rest.split("-->").next())
        .map(str::trim)
        .unwrap_or("");

    if version != templates::RULES_VERSION {
        let shown = if version.is_empty() {
            "unversioned".to_string()
        } else {
            version.to_string()
        };
        return Artifact::Present(
            CheckStatus::Warning,
            format!(
                "CLAUDE.md rules block is {shown}, current is {} — run 'kungfu doctor --fix' to update it in place",
                templates::RULES_VERSION
            ),
        );
    }

    if content[start..end] == templates::render_claude_rules_block() {
        Artifact::Present(
            CheckStatus::Ok,
            format!("CLAUDE.md rules block {} present", templates::RULES_VERSION),
        )
    } else {
        Artifact::Present(
            CheckStatus::Warning,
            format!(
                "CLAUDE.md rules block {} was modified — run 'kungfu doctor --fix' to restore the template",
                templates::RULES_VERSION
            ),
        )
    }
}

fn hook_artifact(root: &Path) -> Artifact {
    let path = root.join(".claude").join("settings.json");
    let content = match read_artifact(&path, ".claude/settings.json") {
        Ok(Some(c)) => c,
        Ok(None) => return Artifact::Absent,
        Err(a) => return a,
    };
    let doc: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return Artifact::Unreadable(format!(
                ".claude/settings.json is not valid JSON: {e} — fix the syntax, then re-run 'kungfu doctor'"
            ))
        }
    };
    let post = doc
        .get("hooks")
        .and_then(|h| h.get("PostToolUse"))
        .and_then(Value::as_array);
    match post {
        Some(arr) if crate::agent_init::has_kungfu_reindex_hook(arr) => Artifact::Present(
            CheckStatus::Ok,
            "auto-reindex hook present in .claude/settings.json".to_string(),
        ),
        _ => Artifact::Absent,
    }
}

fn read_artifact(path: &Path, display: &str) -> Result<Option<String>, Artifact> {
    match std::fs::read_to_string(path) {
        Ok(c) => Ok(Some(c)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Artifact::Unreadable(format!(
            "failed to read {display}: {e}"
        ))),
    }
}

/// Compare this binary's version against the `kungfu` binary on PATH.
///
/// Agents launch the MCP server via the PATH binary (see the `.mcp.json`
/// template), so a mismatch means doctor is being run from a different build
/// than agents use. A running MCP process is deliberately not inspected —
/// that cannot be done reliably from here.
pub fn path_binary_check(integration_configured: bool) -> IntegrationCheck {
    let name = "binary_version";
    let (status, detail) = match std::process::Command::new("kungfu")
        .arg("--version")
        .output()
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if integration_configured {
                (
                    CheckStatus::Warning,
                    "'kungfu' not found on PATH — .mcp.json launches 'kungfu mcp', so agents cannot start the MCP server; install kungfu or add it to PATH"
                        .to_string(),
                )
            } else {
                (
                    CheckStatus::Info,
                    "'kungfu' not found on PATH (fine if you run it another way)".to_string(),
                )
            }
        }
        Err(e) => (
            CheckStatus::Warning,
            format!("could not run 'kungfu --version': {e}"),
        ),
        Ok(out) if !out.status.success() => (
            CheckStatus::Warning,
            format!("'kungfu --version' exited with {}", out.status),
        ),
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            match parse_version_output(&text) {
                Some(v) if v == crate::KUNGFU_VERSION => (
                    CheckStatus::Ok,
                    format!("'kungfu' on PATH matches this binary (v{v})"),
                ),
                Some(v) => (
                    CheckStatus::Warning,
                    format!(
                        "this binary is v{} but 'kungfu' on PATH is v{v} — agents use the PATH binary; align versions, then restart the MCP server (a running server keeps the old binary)",
                        crate::KUNGFU_VERSION
                    ),
                ),
                None => (
                    CheckStatus::Warning,
                    format!(
                        "could not parse 'kungfu --version' output: {:?}",
                        text.trim()
                    ),
                ),
            }
        }
    };
    IntegrationCheck {
        name,
        status,
        detail,
    }
}

/// Extract the version token from `kungfu --version` output (`kungfu 2.5.25`).
fn parse_version_output(text: &str) -> Option<&str> {
    let token = text.lines().next()?.split_whitespace().last()?;
    token
        .chars()
        .next()
        .filter(char::is_ascii_digit)
        .map(|_| token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_init::init_claude_integration;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kungfu-agent-health-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn find<'a>(h: &'a ClaudeIntegrationHealth, name: &str) -> &'a IntegrationCheck {
        h.checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("check {name} missing: {:?}", h.checks))
    }

    #[test]
    fn no_integration_is_a_single_info_line() {
        let root = temp_root("none");
        let h = check_claude_integration(&root);
        assert!(!h.configured);
        assert!(!h.needs_sync);
        assert_eq!(h.checks.len(), 1);
        assert_eq!(h.checks[0].name, "claude_integration");
        assert_eq!(h.checks[0].status, CheckStatus::Info);
        assert!(h.checks[0].detail.contains("kungfu init --agent claude"));
    }

    #[test]
    fn unrelated_files_still_count_as_no_integration() {
        let root = temp_root("unrelated");
        std::fs::write(
            root.join(".mcp.json"),
            r#"{ "mcpServers": { "other": { "command": "x" } } }"#,
        )
        .unwrap();
        std::fs::write(root.join("CLAUDE.md"), "# My rules\n").unwrap();
        let h = check_claude_integration(&root);
        assert!(!h.configured);
        assert_eq!(h.checks.len(), 1);
        assert_eq!(h.checks[0].status, CheckStatus::Info);
    }

    #[test]
    fn healthy_setup_reports_all_ok() {
        let root = temp_root("healthy");
        init_claude_integration(&root, false).unwrap();
        let h = check_claude_integration(&root);
        assert!(h.configured);
        assert!(!h.needs_sync);
        assert_eq!(h.checks.len(), 3);
        assert!(h.checks.iter().all(|c| c.status == CheckStatus::Ok));
    }

    #[test]
    fn missing_hook_is_a_warning_and_fix_repairs_it() {
        let root = temp_root("no-hook");
        init_claude_integration(&root, false).unwrap();
        std::fs::remove_file(root.join(".claude/settings.json")).unwrap();

        let h = check_claude_integration(&root);
        assert!(h.configured);
        assert!(h.needs_sync);
        let hook = find(&h, "claude_hook");
        assert_eq!(hook.status, CheckStatus::Warning);
        assert!(hook.detail.contains(".claude/settings.json"));
        assert!(hook.detail.contains("kungfu doctor --fix"));

        // --fix runs the same sync as init; a re-check must be clean.
        init_claude_integration(&root, false).unwrap();
        let h = check_claude_integration(&root);
        assert!(!h.needs_sync);
        assert_eq!(find(&h, "claude_hook").status, CheckStatus::Ok);
    }

    #[test]
    fn missing_mcp_entry_is_a_warning() {
        let root = temp_root("no-mcp");
        init_claude_integration(&root, false).unwrap();
        std::fs::write(root.join(".mcp.json"), "{ \"mcpServers\": {} }").unwrap();

        let h = check_claude_integration(&root);
        assert!(h.needs_sync);
        let mcp = find(&h, "claude_mcp");
        assert_eq!(mcp.status, CheckStatus::Warning);
        assert!(mcp.detail.contains(".mcp.json"));
    }

    #[test]
    fn customized_mcp_entry_is_a_warning() {
        let root = temp_root("custom-mcp");
        init_claude_integration(&root, false).unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{ "mcpServers": { "kungfu": { "command": "/opt/kungfu", "args": ["mcp"] } } }"#,
        )
        .unwrap();

        let h = check_claude_integration(&root);
        let mcp = find(&h, "claude_mcp");
        assert_eq!(mcp.status, CheckStatus::Warning);
        assert!(mcp.detail.contains("differs from the default"));
    }

    #[test]
    fn outdated_rules_version_is_a_warning() {
        let root = temp_root("old-rules");
        init_claude_integration(&root, false).unwrap();
        let old_block = format!(
            "{} v0 -->\nold rules\n{}",
            templates::RULES_MARKER_START_PREFIX,
            templates::RULES_MARKER_END
        );
        std::fs::write(root.join("CLAUDE.md"), &old_block).unwrap();

        let h = check_claude_integration(&root);
        assert!(h.needs_sync);
        let rules = find(&h, "claude_rules");
        assert_eq!(rules.status, CheckStatus::Warning);
        assert!(rules.detail.contains("v0"));
        assert!(rules.detail.contains(templates::RULES_VERSION));

        // Fix replaces the block in place; re-check is clean.
        init_claude_integration(&root, false).unwrap();
        let h = check_claude_integration(&root);
        assert_eq!(find(&h, "claude_rules").status, CheckStatus::Ok);
    }

    #[test]
    fn modified_current_version_block_is_a_warning() {
        let root = temp_root("edited-rules");
        init_claude_integration(&root, false).unwrap();
        let block = format!(
            "{}\nhand-edited body\n{}",
            templates::rules_marker_start(),
            templates::RULES_MARKER_END
        );
        std::fs::write(root.join("CLAUDE.md"), &block).unwrap();

        let h = check_claude_integration(&root);
        let rules = find(&h, "claude_rules");
        assert_eq!(rules.status, CheckStatus::Warning);
        assert!(rules.detail.contains("modified"));
    }

    #[test]
    fn unterminated_rules_block_is_a_problem() {
        let root = temp_root("unterminated");
        std::fs::write(
            root.join("CLAUDE.md"),
            format!(
                "{} v1 -->\nno end marker\n",
                templates::RULES_MARKER_START_PREFIX
            ),
        )
        .unwrap();

        let h = check_claude_integration(&root);
        assert!(h.configured);
        let rules = find(&h, "claude_rules");
        assert_eq!(rules.status, CheckStatus::Problem);
        assert!(rules.detail.contains(templates::RULES_MARKER_END));
    }

    #[test]
    fn invalid_mcp_json_is_a_problem() {
        let root = temp_root("bad-json");
        std::fs::write(root.join(".mcp.json"), "{ not json").unwrap();
        let h = check_claude_integration(&root);
        let mcp = find(&h, "claude_mcp");
        assert_eq!(mcp.status, CheckStatus::Problem);
        assert!(mcp.detail.contains(".mcp.json"));
        assert!(mcp.detail.contains("not valid JSON"));
    }

    #[test]
    fn parse_version_output_variants() {
        assert_eq!(parse_version_output("kungfu 2.5.25\n"), Some("2.5.25"));
        assert_eq!(parse_version_output("2.5.25"), Some("2.5.25"));
        assert_eq!(parse_version_output(""), None);
        assert_eq!(parse_version_output("garbage output"), None);
    }
}
