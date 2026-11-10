//! Single source of truth for the Claude Code integration templates written by
//! `kungfu init --agent claude`.
//!
//! Deliberately kept as plain constants plus tiny render helpers so the Claude
//! Code plugin packaging can reuse the exact same content without pulling in
//! any of the init/merge logic. If you change the rules body, bump
//! [`RULES_VERSION`] so existing blocks get replaced in place on re-run.

use serde_json::{json, Value};

/// Version of the CLAUDE.md rules block. Embedded in the start marker;
/// a version bump makes `init --agent claude` replace stale blocks in place.
pub const RULES_VERSION: &str = "v2";

/// Prefix shared by every versioned start marker — used to locate an existing
/// block regardless of which version wrote it.
pub const RULES_MARKER_START_PREFIX: &str = "<!-- kungfu:rules:start";

/// End marker of the CLAUDE.md rules block (not versioned).
pub const RULES_MARKER_END: &str = "<!-- kungfu:rules:end -->";

/// Routing rules sent as the MCP server `instructions` on initialize. Clients
/// that defer MCP tools show only tool names plus these instructions at session
/// start, and Claude Code truncates them at 2048 chars — keep them under that
/// (enforced by a test). Same content as the README "Routing rules" block.
pub const MCP_INSTRUCTIONS: &str = r#"kungfu — context retrieval. Use it BEFORE Read / grep / find: it returns ranked,
scoped packets instead of whole files; raw reads are the fallback. Start every task
with `ask_context("<task>", budget: "tiny")` and escalate the budget only if the
packet is clearly insufficient. Open a raw file only once kungfu points you at it.

Tools may be deferred: load the ones you need in ONE ToolSearch call, e.g.
"+kungfu ask_context edit_context verify_change".

Route by situation:

| Situation | First call |
|---|---|
| New task / "figure out X" | `ask_context` |
| Where is a named symbol defined/used? | `find_symbol` → `explore_symbol`, then `callers` / `callees` |
| Concept with no known name | `semantic_search` |
| Which files deal with X? | `find_files` (paths + contained symbols) |
| Understand a file > 50 lines | `file_outline` (budget "small" adds related files), then a targeted Read |
| About to edit a symbol | `edit_context` (full verbatim body — no follow-up Read) |
| "Why is it like this / what changed?" | `history` (a file path or a symbol) |
| Refactor touching > 1 file | `affected` + `coupling` before editing |
| Bug with no clear file | `hotspots`, then `debug_trace` on the stack trace |
| New to the repo | `repo_outline` (include_onboard: conventions, glossary) |

If kungfu's edit hook is installed (plugin or `kungfu init --agent claude`), files
you Edit/Write are reindexed automatically; otherwise `reindex` the paths you
changed. Always `reindex` after shell edits, codegen or git checkout. After edits,
`verify_change` (blast radius, tests, missed co-changes, risk).
`memory_search` before implementing (there may already be a decision or warning);
`memory_add` to persist new ones — pin sparingly.

Skip kungfu only for: a one-line edit in a file already open this session; a file
< 50 lines whose exact path you know; pure shell ops; reading a config/lock file by
exact path; an exact literal/regex search across files (Grep). Otherwise, if you
reach for Read / grep / find — stop and route above."#;

/// Pointer block appended to the project's CLAUDE.md. The routing table itself
/// lives in [`MCP_INSTRUCTIONS`]. Same content as the README "CLAUDE.md block"
/// — keep the two in sync.
pub const CLAUDE_RULES_BODY: &str = r#"## kungfu — context retrieval

kungfu (MCP) is connected; its routing rules — which tool to call first for each
situation — are in the kungfu MCP server instructions. Start every task with
`ask_context("<task>", budget: "tiny")`; raw Read / grep / find are the fallback."#;

/// Current versioned start marker, e.g. `<!-- kungfu:rules:start v2 -->`.
pub fn rules_marker_start() -> String {
    format!("{RULES_MARKER_START_PREFIX} {RULES_VERSION} -->")
}

/// Full marked rules block, ready to insert into CLAUDE.md.
pub fn render_claude_rules_block() -> String {
    format!(
        "{}\n{}\n{}",
        rules_marker_start(),
        CLAUDE_RULES_BODY.trim_end(),
        RULES_MARKER_END
    )
}

/// `.mcp.json` server entry for the kungfu MCP server (Claude Code format).
pub fn mcp_server_entry() -> Value {
    json!({ "command": "kungfu", "args": ["mcp"] })
}

/// Shell command of the auto-reindex hook — same shape as documented in the
/// README ("Auto-reindex on edit"). Reads the PostToolUse payload from stdin;
/// always exits 0 and prints nothing. The shell guard covers plugin/binary skew:
/// a pre-2.7.2 binary rejects `--from-hook` with clap's exit 2, which Claude Code
/// would feed back to the model as a hook error after every edit.
pub const REINDEX_HOOK_COMMAND: &str = "kungfu index --from-hook 2>/dev/null || true";

/// PostToolUse matcher for the auto-reindex hook. NotebookEdit is left out:
/// it reports `notebook_path`, and `.ipynb` is not indexed.
pub const REINDEX_HOOK_MATCHER: &str = "Edit|Write";

/// Substring identifying a current kungfu auto-reindex hook, however the user
/// may have customized it. Used to avoid installing a duplicate.
pub const REINDEX_HOOK_FINGERPRINT: &str = "kungfu index --from-hook";

/// Exact command of the pre-`--from-hook` template hook. Only a hook with this
/// exact command is replaced in place by init and flagged outdated by doctor.
pub const LEGACY_REINDEX_HOOK_COMMAND: &str =
    "jq -r '.tool_input.file_path // empty' | xargs -I{} kungfu index --only {} >/dev/null 2>&1 || true";

/// Substring identifying a user-written `--only` reindex hook. It still
/// reindexes, so init leaves it alone and doctor only notes it.
pub const LEGACY_REINDEX_HOOK_FINGERPRINT: &str = "kungfu index --only";

/// Shell command of the plugin's SessionStart hook. With the binary on PATH it is
/// the release check: one line only when a newer kungfu exists (cached 24h, silent
/// on network failure), so a stale binary is noticed while restarting is cheap.
/// Without it, it says so — otherwise the MCP server and edit hook fail silently.
/// It never downloads anything: installing stays an explicit user action.
/// POSIX shell syntax: Claude Code runs hooks with `sh -c`, or Git Bash on Windows.
/// The PowerShell fallback (Windows without Git Bash) can't evaluate `command -v`.
pub const UPDATE_CHECK_HOOK_COMMAND: &str = "command -v kungfu >/dev/null 2>&1 && { kungfu update --check --quiet 2>/dev/null || true; } || echo \"kungfu plugin: the kungfu binary is not on PATH, so the kungfu MCP server and edit hook are inactive. Install it (https://github.com/denyzhirkov/kungfu#install), then restart the session.\"";

/// Full SessionStart hook entry for the update check.
pub fn update_check_hook_entry() -> Value {
    json!({
        "hooks": [
            { "type": "command", "command": UPDATE_CHECK_HOOK_COMMAND }
        ]
    })
}

/// Full PostToolUse hook entry for `.claude/settings.json`.
pub fn reindex_hook_entry() -> Value {
    json!({
        "matcher": REINDEX_HOOK_MATCHER,
        "hooks": [
            { "type": "command", "command": REINDEX_HOOK_COMMAND }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_instructions_fit_claude_code_limit() {
        let n = MCP_INSTRUCTIONS.chars().count();
        assert!(
            n <= 2048,
            "MCP_INSTRUCTIONS is {n} chars; Claude Code truncates at 2048"
        );
    }

    #[cfg(unix)]
    #[test]
    fn session_start_hook_reports_missing_binary() {
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", UPDATE_CHECK_HOOK_COMMAND])
            .env("PATH", "/nonexistent")
            .output()
            .expect("run sh");
        assert!(out.status.success(), "hook must never fail the session");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("not on PATH") && stdout.contains("#install"),
            "missing-binary hint expected, got: {stdout}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn session_start_hook_passes_through_when_binary_present() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kf-hook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let run = |script: &str| {
            let bin = dir.join("kungfu");
            std::fs::write(&bin, script).expect("fake kungfu");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            std::process::Command::new("/bin/sh")
                .args(["-c", UPDATE_CHECK_HOOK_COMMAND])
                .env("PATH", format!("{}:/bin:/usr/bin", dir.display()))
                .output()
                .expect("run sh")
        };
        // Up to date: silent. A failing update check stays silent too (no hint).
        for script in ["#!/bin/sh\nexit 0\n", "#!/bin/sh\nexit 3\n"] {
            let out = run(script);
            assert!(out.status.success());
            assert!(
                out.stdout.is_empty(),
                "expected silence, got {:?}",
                out.stdout
            );
        }
        let out = run("#!/bin/sh\necho 'kungfu 9.9.9 is available'\n");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "kungfu 9.9.9 is available"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
