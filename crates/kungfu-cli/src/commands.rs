use anyhow::Result;
use kungfu_core::{EmptyCallGraphCause, KungfuService};
use kungfu_memory::project_search::MemoryFilter;
use kungfu_project::agent_health::{self, CheckStatus};
use kungfu_project::{find_project_root, init_project, KUNGFU_VERSION};
use kungfu_types::memory::ProjectMemoryKind;
use kungfu_types::Budget;
use std::env;

use crate::{EmbeddingsCommands, MemoryCommands};

pub fn init(agent: Option<&str>, dry_run: bool, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let root = find_project_root(&cwd)?;

    match agent {
        None => {
            let project = init_project(&root)?;
            if json {
                let info = serde_json::json!({
                    "status": "initialized",
                    "root": project.root.to_string_lossy(),
                    "project_name": project.meta.name,
                });
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!("Initialized kungfu in {}", project.root.display());
                println!("  project: {}", project.meta.name);
                println!("  config:  .kungfu/config.toml");
                println!("\nRun 'kungfu index' to build the project index.");
            }
            Ok(())
        }
        Some("claude") => init_agent_claude(&root, dry_run, json),
        Some(other) => anyhow::bail!("unsupported agent '{}' (supported: claude)", other),
    }
}

fn init_agent_claude(root: &std::path::Path, dry_run: bool, json: bool) -> Result<()> {
    use kungfu_project::agent_init::{init_claude_integration, ActionStatus};

    // Ensure the kungfu project itself exists; don't fail if it already does.
    let kungfu_status = if root.join(kungfu_project::KUNGFU_DIR).exists() {
        "already current"
    } else if dry_run {
        "created"
    } else {
        init_project(root)?;
        "created"
    };

    let actions = init_claude_integration(root, dry_run)?;

    if json {
        let out = serde_json::json!({
            "status": "ok",
            "agent": "claude",
            "dry_run": dry_run,
            "root": root.to_string_lossy(),
            "actions": std::iter::once(serde_json::json!({
                "path": ".kungfu/",
                "status": kungfu_status,
                "detail": "kungfu project directory",
            }))
            .chain(actions.iter().map(|a| {
                serde_json::json!({
                    "path": a.path,
                    "status": a.status.as_str(),
                    "detail": a.detail,
                })
            }))
            .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    let verb = |status: &str| -> String {
        if dry_run && status != "already current" {
            format!("would be {status}")
        } else {
            status.to_string()
        }
    };

    println!(
        "Claude Code integration in {}{}:",
        root.display(),
        if dry_run {
            " (dry run — nothing written)"
        } else {
            ""
        }
    );
    println!(
        "  {:<22} {} — kungfu project directory",
        ".kungfu/",
        verb(kungfu_status)
    );
    for a in &actions {
        println!(
            "  {:<22} {} — {}",
            a.path,
            verb(a.status.as_str()),
            a.detail
        );
    }

    if dry_run {
        println!("\nRe-run without --dry-run to apply.");
    } else if actions
        .iter()
        .all(|a| a.status == ActionStatus::AlreadyCurrent)
    {
        println!("\nEverything already current — nothing to do.");
    } else {
        println!(
            "\nNext: restart Claude Code (or reconnect MCP via /mcp) to pick up the kungfu server."
        );
        println!("Then run 'kungfu index' if the project is not indexed yet.");
    }
    Ok(())
}

pub fn status(json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let info = service.status()?;

    if json {
        let out = serde_json::json!({
            "project_name": info.project_name,
            "root": info.root,
            "indexed_files": info.indexed_files,
            "indexed_symbols": info.indexed_symbols,
            "languages": info.languages,
            "has_git": info.has_git,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("Project: {}", info.project_name);
        println!("Root:    {}", info.root);
        println!("Files:   {}", info.indexed_files);
        println!("Symbols: {}", info.indexed_symbols);
        println!("Git:     {}", if info.has_git { "yes" } else { "no" });
        if !info.languages.is_empty() {
            println!("Languages:");
            let mut langs: Vec<_> = info.languages.iter().collect();
            langs.sort_by(|a, b| b.1.cmp(a.1));
            for (lang, count) in langs {
                println!("  {}: {}", lang, count);
            }
        }
    }
    Ok(())
}

pub fn doctor(json: bool, fix: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let mut checks: Vec<(&str, CheckStatus, String)> = Vec::new();
    // Binary pass/fail checks: true → ok, false → problem.
    let st = |ok: bool| {
        if ok {
            CheckStatus::Ok
        } else {
            CheckStatus::Problem
        }
    };

    // Check version
    checks.push(("version", CheckStatus::Ok, KUNGFU_VERSION.to_string()));

    // Release freshness — cache only, so `doctor` stays offline-safe and instant.
    let update_status = kungfu_update::status_from_cache(&update_config());
    checks.push((
        "update",
        if update_status.update_available {
            CheckStatus::Info
        } else {
            CheckStatus::Ok
        },
        format!(
            "{} [{}]",
            update_status.summary(),
            update_status.source.as_str()
        ),
    ));

    // Check project root
    match find_project_root(&cwd) {
        Ok(root) => {
            let kungfu_dir = root.join(".kungfu");
            checks.push((
                "project_root",
                CheckStatus::Ok,
                root.to_string_lossy().to_string(),
            ));
            checks.push((
                "kungfu_dir",
                st(kungfu_dir.exists()),
                if kungfu_dir.exists() {
                    ".kungfu exists".into()
                } else {
                    "missing — run 'kungfu init'".into()
                },
            ));

            if kungfu_dir.exists() {
                // Config
                let config_path = kungfu_dir.join("config.toml");
                let config_ok = config_path.exists();
                let config_detail = if config_ok {
                    match kungfu_config::KungfuConfig::load(&config_path) {
                        Ok(_) => "valid".to_string(),
                        Err(e) => format!("parse error: {}", e),
                    }
                } else {
                    "missing".to_string()
                };
                checks.push((
                    "config",
                    st(config_ok && !config_detail.starts_with("parse")),
                    config_detail,
                ));

                // Project metadata
                let project_path = kungfu_dir.join("project.json");
                let project_ok = project_path.exists();
                checks.push((
                    "project_meta",
                    st(project_ok),
                    if project_ok {
                        "project.json exists".into()
                    } else {
                        "missing".into()
                    },
                ));

                // Index
                let index_dir = kungfu_dir.join("index");
                let files_path = index_dir.join("files.json");
                let symbols_path = index_dir.join("symbols.json");
                let fp_path = index_dir.join("fingerprints.json");

                let has_files = files_path.exists();
                let has_symbols = symbols_path.exists();
                let has_fp = fp_path.exists();

                if has_files {
                    let file_count = std::fs::read_to_string(&files_path)
                        .ok()
                        .and_then(|c| {
                            serde_json::from_str::<Vec<serde_json::Value>>(&c)
                                .ok()
                                .map(|v| v.len())
                        })
                        .unwrap_or(0);
                    checks.push((
                        "index_files",
                        CheckStatus::Ok,
                        format!("{} files indexed", file_count),
                    ));
                } else {
                    checks.push((
                        "index_files",
                        CheckStatus::Problem,
                        "not indexed — run 'kungfu index'".into(),
                    ));
                }

                if has_symbols {
                    let sym_count = std::fs::read_to_string(&symbols_path)
                        .ok()
                        .and_then(|c| {
                            serde_json::from_str::<Vec<serde_json::Value>>(&c)
                                .ok()
                                .map(|v| v.len())
                        })
                        .unwrap_or(0);
                    checks.push((
                        "index_symbols",
                        CheckStatus::Ok,
                        format!("{} symbols extracted", sym_count),
                    ));
                } else {
                    checks.push(("index_symbols", CheckStatus::Problem, "no symbols".into()));
                }

                checks.push((
                    "index_fingerprints",
                    st(has_fp),
                    if has_fp {
                        "fingerprints tracked".into()
                    } else {
                        "no fingerprints".into()
                    },
                ));

                // Relations
                let relations_path = index_dir.join("relations.json");
                if relations_path.exists() {
                    let rel_count = std::fs::read_to_string(&relations_path)
                        .ok()
                        .and_then(|c| {
                            serde_json::from_str::<Vec<serde_json::Value>>(&c)
                                .ok()
                                .map(|v| v.len())
                        })
                        .unwrap_or(0);
                    checks.push((
                        "index_relations",
                        st(rel_count > 0),
                        format!("{} relations (imports, tests, configs)", rel_count),
                    ));
                } else {
                    checks.push((
                        "index_relations",
                        CheckStatus::Problem,
                        "no relations — reindex with 'kungfu index --full'".into(),
                    ));
                }

                // Symbol coverage: % of code files that have symbols
                if has_files && has_symbols {
                    let file_count = std::fs::read_to_string(&files_path)
                        .ok()
                        .and_then(|c| serde_json::from_str::<Vec<serde_json::Value>>(&c).ok())
                        .unwrap_or_default();
                    let sym_data = std::fs::read_to_string(&symbols_path)
                        .ok()
                        .and_then(|c| serde_json::from_str::<Vec<serde_json::Value>>(&c).ok())
                        .unwrap_or_default();

                    let code_files: std::collections::HashSet<String> = file_count
                        .iter()
                        .filter(|f| {
                            let lang = f.get("language").and_then(|l| l.as_str()).unwrap_or("");
                            if !matches!(
                                lang,
                                "rust"
                                    | "typescript"
                                    | "javascript"
                                    | "python"
                                    | "go"
                                    | "java"
                                    | "csharp"
                                    | "kotlin"
                                    | "c"
                                    | "cpp"
                            ) {
                                return false;
                            }
                            // Exclude tiny files and test fixtures from coverage
                            let size = f.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
                            if size < 100 {
                                return false;
                            }
                            let path = f.get("path").and_then(|p| p.as_str()).unwrap_or("");
                            !path.contains("/fixtures/")
                                && !path.contains("/resources/")
                                && !path.contains("/snapshots/")
                                && !path.contains("/__snapshots__/")
                                && !path.contains("/testdata/")
                                && !path.contains("/test_data/")
                        })
                        .filter_map(|f| f.get("id").and_then(|id| id.as_str()).map(String::from))
                        .collect();

                    let files_with_symbols: std::collections::HashSet<String> = sym_data
                        .iter()
                        .filter_map(|s| {
                            s.get("file_id")
                                .and_then(|id| id.as_str())
                                .map(String::from)
                        })
                        .collect();

                    let covered = code_files.intersection(&files_with_symbols).count();
                    let total = code_files.len();
                    let pct = if total > 0 { covered * 100 / total } else { 0 };

                    checks.push((
                        "symbol_coverage",
                        st(pct >= 50),
                        format!("{}/{}  code files have symbols ({}%)", covered, total, pct),
                    ));
                }

                // Directories
                let dirs = ["cache", "logs", "state"];
                for dir in &dirs {
                    let d = kungfu_dir.join(dir);
                    checks.push((
                        dir,
                        st(d.exists()),
                        if d.exists() {
                            "ok".into()
                        } else {
                            "missing".into()
                        },
                    ));
                }

                let service = KungfuService::open(&cwd);

                // Version coherence: which binary wrote the index? A running MCP
                // server does not pick up a replaced binary, so a mismatch usually
                // means a stale server (or a stale index) is in play.
                if has_files {
                    if let Ok(s) = &service {
                        match s.index_store_version() {
                            Some(v) if v == KUNGFU_VERSION => checks.push((
                                "index_version",
                                CheckStatus::Ok,
                                format!("index written by this version (v{})", v),
                            )),
                            Some(v) => checks.push((
                                "index_version",
                                CheckStatus::Warning,
                                format!(
                                    "index written by kungfu v{}, this binary is v{} — \
                                     run 'kungfu index --full', and restart the MCP server if one is running",
                                    v, KUNGFU_VERSION
                                ),
                            )),
                            None => checks.push((
                                "index_version",
                                CheckStatus::Warning,
                                "index has no version stamp (written by an older kungfu) — \
                                 run 'kungfu index --full', and restart the MCP server if one is running"
                                    .into(),
                            )),
                        }
                    }
                }

                // Manual memory store (.kungfu/memory). With --fix, absorb a stray
                // legacy project_memory.json left by an outdated binary.
                match &service {
                    Ok(s) => match s.memory_doctor(fix) {
                        Ok(d) => {
                            checks.push((
                                "memory_store",
                                CheckStatus::Ok,
                                format!(
                                    "{} entries ({} active, {} archived)",
                                    d.total, d.active, d.archived
                                ),
                            ));
                            if let Some(rep) = &d.absorbed {
                                checks.push((
                                    "memory_absorbed",
                                    CheckStatus::Ok,
                                    format!(
                                        "absorbed {} legacy entr{} ({} duplicate(s) skipped, {} renumbered)",
                                        rep.added.len(),
                                        if rep.added.len() == 1 { "y" } else { "ies" },
                                        rep.skipped_duplicates,
                                        rep.renumbered.len()
                                    ),
                                ));
                            } else if d.legacy_present {
                                checks.push((
                                    "memory_legacy",
                                    CheckStatus::Warning,
                                    "stray legacy project_memory.json present (outdated binary?) — \
                                     restart the MCP server, then run 'kungfu doctor --fix' to absorb it"
                                        .into(),
                                ));
                            }
                        }
                        Err(e) => checks.push((
                            "memory_store",
                            CheckStatus::Problem,
                            format!("error: {}", e),
                        )),
                    },
                    Err(e) => checks.push((
                        "memory_store",
                        CheckStatus::Problem,
                        format!("error: {}", e),
                    )),
                }

                // Embeddings staleness: only meaningful when the user opted in
                // (a manifest exists). Never auto-builds — building downloads
                // models and stays an explicit `kungfu embeddings build`.
                if let Ok(s) = &service {
                    if let Ok(es) = s.embeddings_status() {
                        if es.index_present {
                            if es.indexed_vectors < es.indexed_symbols {
                                checks.push((
                                    "embeddings",
                                    CheckStatus::Warning,
                                    format!("vectors lag the index — {}", es.hint),
                                ));
                            } else {
                                checks.push((
                                    "embeddings",
                                    CheckStatus::Ok,
                                    format!(
                                        "{} vectors in sync with {} symbols",
                                        es.indexed_vectors, es.indexed_symbols
                                    ),
                                ));
                            }
                        }
                    }
                }
            }

            // Claude Code integration (.mcp.json / CLAUDE.md rules block /
            // auto-reindex hook). Absence of the whole integration is only an
            // info line; --fix re-runs the same idempotent sync as
            // `kungfu init --agent claude`.
            let mut health = agent_health::check_claude_integration(&root);
            if fix && health.needs_sync {
                use kungfu_project::agent_init::{init_claude_integration, ActionStatus};
                match init_claude_integration(&root, false) {
                    Ok(actions) => {
                        for a in actions
                            .iter()
                            .filter(|a| a.status != ActionStatus::AlreadyCurrent)
                        {
                            checks.push((
                                "claude_fixed",
                                CheckStatus::Ok,
                                format!("{}: {}", a.path, a.detail),
                            ));
                        }
                        health = agent_health::check_claude_integration(&root);
                    }
                    Err(e) => checks.push((
                        "claude_fix",
                        CheckStatus::Problem,
                        format!("--fix could not repair the Claude integration: {}", e),
                    )),
                }
            }
            for c in health.checks {
                checks.push((c.name, c.status, c.detail));
            }

            // Binary/version coherence: agents launch the MCP server via the
            // PATH binary; doctor may be running from a different build.
            let bin = agent_health::path_binary_check(health.configured);
            checks.push((bin.name, bin.status, bin.detail));
        }
        Err(e) => {
            checks.push(("project_root", CheckStatus::Problem, e.to_string()));
        }
    }

    // Git
    checks.push((
        "git",
        if kungfu_git::is_git_repo(&cwd) {
            CheckStatus::Ok
        } else {
            CheckStatus::Warning
        },
        if kungfu_git::is_git_repo(&cwd) {
            "git repository detected".into()
        } else {
            "not a git repo (git features unavailable)".into()
        },
    ));

    // Parser support
    checks.push((
        "parsers",
        CheckStatus::Ok,
        "rust, typescript, javascript, python, go, java, csharp, kotlin, c, cpp".into(),
    ));

    if json {
        let items: Vec<_> = checks
            .iter()
            .map(|(name, status, detail)| {
                serde_json::json!({
                    "check": name,
                    "ok": !status.needs_attention(),
                    "status": status.as_str(),
                    "detail": detail,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        for (name, status, detail) in &checks {
            let icon = match status {
                CheckStatus::Ok => "OK",
                CheckStatus::Info => "--",
                CheckStatus::Warning => "!?",
                CheckStatus::Problem => "!!",
            };
            println!("  [{}] {}: {}", icon, name, detail);
        }
        println!();
        let problems = checks
            .iter()
            .filter(|(_, s, _)| *s == CheckStatus::Problem)
            .count();
        let warnings = checks
            .iter()
            .filter(|(_, s, _)| *s == CheckStatus::Warning)
            .count();
        if problems == 0 && warnings == 0 {
            println!("All checks passed.");
        } else {
            println!(
                "{} problem(s), {} warning(s) need attention.",
                problems, warnings
            );
        }
    }
    Ok(())
}

pub fn config_show(json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let _ = KungfuService::open(&cwd)?;
    let root = find_project_root(&cwd)?;
    let config_path = root.join(".kungfu").join("config.toml");
    let config = kungfu_config::KungfuConfig::load_merged(Some(&config_path))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        let toml_str = toml::to_string_pretty(&config)?;
        println!("{}", toml_str);
    }
    Ok(())
}

pub fn index(full: bool, changed: bool, only: Vec<String>, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;

    let start = std::time::Instant::now();
    let stats = if !only.is_empty() {
        service.index_paths(&only)?
    } else if full {
        service.index_full()?
    } else if changed {
        service.index_changed()?
    } else {
        service.index_incremental()?
    };
    let elapsed = start.elapsed();

    if json {
        let out = serde_json::json!({
            "total_files": stats.total_files,
            "new_files": stats.new_files,
            "changed_files": stats.changed_files,
            "removed_files": stats.removed_files,
            "symbols_extracted": stats.symbols_extracted,
            "call_edges_filtered": stats.call_edges_filtered,
            "elapsed_ms": elapsed.as_millis(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "Indexed {} files ({} symbols) in {:.1}s",
            stats.total_files,
            stats.symbols_extracted,
            elapsed.as_secs_f64()
        );
        if stats.new_files > 0 {
            println!("  new:     {}", stats.new_files);
        }
        if stats.changed_files > 0 {
            println!("  changed: {}", stats.changed_files);
        }
        if stats.removed_files > 0 {
            println!("  removed: {}", stats.removed_files);
        }
        if stats.call_edges_filtered > 0 {
            println!(
                "  call edges filtered as utility-noise: {}",
                stats.call_edges_filtered
            );
        }
    }
    Ok(())
}

pub fn clean(json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let root = find_project_root(&cwd)?;
    let kungfu_dir = root.join(".kungfu");

    let index_dir = kungfu_dir.join("index");
    let cache_dir = kungfu_dir.join("cache");

    let mut cleaned = Vec::new();
    if index_dir.exists() {
        std::fs::remove_dir_all(&index_dir)?;
        std::fs::create_dir_all(&index_dir)?;
        cleaned.push("index");
    }
    if cache_dir.exists() {
        std::fs::remove_dir_all(&cache_dir)?;
        std::fs::create_dir_all(cache_dir.join("summaries"))?;
        std::fs::create_dir_all(cache_dir.join("queries"))?;
        cleaned.push("cache");
    }

    if json {
        println!("{}", serde_json::json!({ "cleaned": cleaned }));
    } else {
        println!("Cleaned: {}", cleaned.join(", "));
    }
    Ok(())
}

pub fn commit_context(hash: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let packet = service.commit_context(hash, budget)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&packet)?);
    } else {
        println!("Query: {}", packet.query);
        println!("Items: {}", packet.items.len());
        for it in &packet.items {
            println!("  {}::{}  ({:.2})", it.path, it.name, it.score);
        }
    }
    Ok(())
}

pub fn pr_context(num: u32, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let packet = service.pr_context(num, budget)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&packet)?);
    } else {
        println!("Query: {}", packet.query);
        println!("Commits: {}", packet.history.len());
        println!("Items: {}", packet.items.len());
        for it in &packet.items {
            println!("  {}::{}  ({:.2})", it.path, it.name, it.score);
        }
    }
    Ok(())
}

pub fn embeddings(action: EmbeddingsCommands, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;

    match action {
        EmbeddingsCommands::Status => {
            let status = service.embeddings_status()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("model:               {}", status.model_id);
                println!("dim:                 {}", status.dim);
                println!("inference feature:   {}", status.inference_compiled);
                println!("weights installed:   {}", status.weights_installed);
                println!(
                    "vectors built:       {} / {} symbols (+{} file vectors)",
                    status.indexed_vectors, status.indexed_symbols, status.indexed_file_vectors
                );
                println!("weights dir:         {}", status.models_dir);
                println!();
                println!("→ {}", status.hint);
            }
            Ok(())
        }
        EmbeddingsCommands::Install => install_model(json),
        EmbeddingsCommands::Build => {
            let result = service.embeddings_build()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!(
                    "embedded {} symbols ({} skipped up-to-date, {} total in store)",
                    result.embedded, result.skipped_up_to_date, result.total_in_store
                );
            }
            Ok(())
        }
    }
}

#[cfg(feature = "semantic")]
fn install_model(json: bool) -> Result<()> {
    let dir = kungfu_embed::install_model(
        &kungfu_embed::default_models_dir(),
        kungfu_embed::DEFAULT_MODEL_ID,
    )?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "installed",
                "dir": dir,
            }))?
        );
    } else {
        println!(
            "Installed {} → {}",
            kungfu_embed::DEFAULT_MODEL_ID,
            dir.display()
        );
    }
    Ok(())
}

#[cfg(not(feature = "semantic"))]
fn install_model(_json: bool) -> Result<()> {
    anyhow::bail!(
        "embedding model install requires the `semantic` feature; \
         rebuild with `cargo build --release --features semantic`"
    )
}

pub fn export(format: &str, json: bool) -> Result<()> {
    if format != "jsonl" {
        anyhow::bail!("unsupported export format: {} (try 'jsonl')", format);
    }
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let stats = service.export_jsonl(&mut handle)?;
    if json {
        // The data itself went to stdout already; the JSON wrapper goes to stderr to avoid mixing.
        eprintln!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        eprintln!(
            "exported {} files, {} symbols, {} relations, {} memories",
            stats.files, stats.symbols, stats.relations, stats.memories
        );
    }
    Ok(())
}

pub fn stats(json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let stats = service.usage_stats()?;

    if json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        println!("=== Kungfu Usage Stats ===");
        println!();
        println!("  Total calls:        {}", stats.total_calls);
        println!(
            "  Bytes served:       {} ({:.1} KB)",
            stats.total_bytes_served,
            stats.total_bytes_served as f64 / 1024.0
        );
        let tokens = stats.total_bytes_served / 4;
        println!("  Est. tokens served: {}", tokens);
        if stats.total_raw_bytes_baseline > 0 {
            let ratio =
                stats.total_raw_bytes_baseline as f64 / stats.total_bytes_served.max(1) as f64;
            let saved = stats
                .total_raw_bytes_baseline
                .saturating_sub(stats.total_bytes_served)
                / 4;
            println!("  Compression:        {:.1}x", ratio);
            println!(
                "  Est. tokens saved:  {} (vs reading referenced files)",
                saved
            );
        }
        if let Some(ref first) = stats.first_used {
            println!("  First used:         {}", first);
        }
        if let Some(ref last) = stats.last_used {
            println!("  Last used:          {}", last);
        }
        if !stats.per_command.is_empty() {
            println!();
            println!("  Per command:");
            let mut cmds: Vec<_> = stats.per_command.iter().collect();
            cmds.sort_by(|a, b| b.1.cmp(a.1));
            for (cmd, count) in cmds {
                println!("    {:<25} {:>6} calls", cmd, count);
            }
        }
        if stats.total_calls == 0 {
            println!();
            println!("  No usage recorded yet. Stats accumulate as you use kungfu.");
        }
    }
    Ok(())
}

pub fn onboard(json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let info = service.onboard()?;

    if json {
        println!("{}", serde_json::to_string_pretty(&info)?);
    } else {
        println!("# {}", info.project_name);
        println!();
        println!("## Overview");
        println!(
            "  Files:   {}  |  Symbols: {}",
            info.total_files, info.total_symbols
        );
        if let Some(ref primary) = info.primary_language {
            println!("  Primary: {}", primary);
        }
        println!();
        println!("## Architecture");
        println!("  {}", info.architecture);
        println!();
        println!("## Languages");
        for (lang, count) in &info.languages {
            println!("  {:<15} {:>5} files", lang, count);
        }
        println!();
        println!("## Structure");
        for (dir, count) in &info.top_dirs {
            println!("  {:<30} {:>5} files", format!("{}/", dir), count);
        }
        println!();
        println!("## Entry Points");
        for ep in &info.entrypoints {
            println!("  {}", ep);
        }
        println!();
        println!("## Key Symbols (most connected)");
        for sym in &info.key_symbols {
            println!("  {}", sym);
        }
        if !info.glossary.is_empty() {
            println!();
            println!("## Glossary");
            for entry in &info.glossary {
                let meaning = match (&entry.meaning, &entry.defined_at) {
                    (Some(m), _) => m.clone(),
                    (None, Some(at)) => format!("(defined at {at} — no doc)"),
                    (None, None) => "(no definition found)".to_string(),
                };
                let meaning = meaning.as_str();
                let usage = entry
                    .usage
                    .as_deref()
                    .map(|u| format!(" [{u}]"))
                    .unwrap_or_default();
                println!(
                    "  {:<16} {} ({}){}",
                    entry.term, meaning, entry.source, usage
                );
            }
        }
        println!();
        println!("## Naming: {}", info.naming_style);
        println!("## Tests:  {}", info.test_pattern);
    }
    Ok(())
}

pub fn affected(name: &str, depth: usize, staged: bool, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = if staged {
        service.affected_staged(depth)?
    } else {
        if name.is_empty() {
            anyhow::bail!("symbol name required (or use --staged)");
        }
        service.affected(name, depth)?
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Blast radius for: {}", result.symbol);
        println!("Risk: {}", result.risk);
        println!();
        if result.entries.is_empty() {
            println!("  No affected symbols found.");
        } else {
            println!(
                "{:<4} {:<30} {:<50} {:<10} Reason",
                "#", "Symbol", "Path", "Kind"
            );
            println!("{}", "-".repeat(110));
            for (i, e) in result.entries.iter().enumerate() {
                println!(
                    "{:<4} {:<30} {:<50} {:<10} {}",
                    i + 1,
                    truncate_str(&e.name, 29),
                    truncate_str(&e.path, 49),
                    e.kind,
                    e.reason,
                );
            }
        }
        if !result.test_files.is_empty() {
            println!();
            println!("Affected test files:");
            for t in &result.test_files {
                println!("  {}", t);
            }
        }
    }
    Ok(())
}

pub fn verify_change(depth: usize, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = service.verify_change(depth)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    if result["status"].as_str() == Some("clean") {
        println!("Working tree matches HEAD — nothing to verify.");
        return Ok(());
    }
    println!(
        "Changed: {} symbols in {} files  |  risk: {}",
        result["changed_symbols"].as_array().map_or(0, |a| a.len()),
        result["changed_files"].as_array().map_or(0, |a| a.len()),
        result["affected"]["risk"].as_str().unwrap_or("?"),
    );
    if let Some(contracts) = result["touched_contracts"].as_array() {
        if !contracts.is_empty() {
            println!("Public contracts touched:");
            for c in contracts {
                println!(
                    "  {}:{}  {}",
                    c["path"].as_str().unwrap_or(""),
                    c["line"].as_u64().unwrap_or(0),
                    c["name"].as_str().unwrap_or(""),
                );
            }
        }
    }
    if let Some(entries) = result["affected"]["entries"].as_array() {
        if !entries.is_empty() {
            println!(
                "Affected ({} total):",
                result["affected"]["total"].as_u64().unwrap_or(0)
            );
            for e in entries.iter().take(10) {
                println!(
                    "  d{}  {}  {}",
                    e["depth"].as_u64().unwrap_or(0),
                    e["path"].as_str().unwrap_or(""),
                    e["name"].as_str().unwrap_or(""),
                );
            }
        }
    }
    if let Some(tests) = result["suggested_tests"].as_array() {
        if tests.is_empty() {
            println!("Suggested tests: none found — consider adding coverage.");
        } else {
            println!("Suggested tests:");
            for t in tests.iter().take(15) {
                println!(
                    "  {}  {}",
                    t["path"].as_str().unwrap_or(""),
                    t["name"].as_str().unwrap_or(""),
                );
            }
        }
    }
    if let Some(note) = result["note"].as_str() {
        println!("note: {}", note);
    }
    Ok(())
}

pub fn smart_test(json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = service.smart_test()?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        if result.changed_symbols.is_empty() {
            println!("No changes detected in git diff.");
            return Ok(());
        }
        println!("Changed symbols:");
        for s in &result.changed_symbols {
            println!("  {}", s);
        }
        println!();
        if result.tests.is_empty() {
            println!("No relevant tests found.");
        } else {
            println!(
                "Run these {} tests (of {} total):",
                result.tests.len(),
                result.total_tests_in_project
            );
            println!();
            for t in &result.tests {
                println!("  {}::{}", t.test_path, t.test_name);
                println!("    reason: {}", t.reason);
            }
        }
    }
    Ok(())
}

pub fn review(json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = service.review()?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("=== Code Review Context ===");
        println!("Risk: {}", result.risk);
        println!("{}", result.summary);
        println!();
        if !result.changed_files.is_empty() {
            println!("Changed files:");
            for f in &result.changed_files {
                println!("  {}", f);
            }
        }
        if !result.changed_symbols.is_empty() {
            println!();
            println!("Changed symbols:");
            for s in &result.changed_symbols {
                println!("  {}", s);
            }
        }
        if !result.missing_co_changes.is_empty() {
            println!();
            println!("Missing co-changes (usually change together):");
            for m in &result.missing_co_changes {
                println!("  ⚠ {}", m);
            }
        }
        if !result.untested_changes.is_empty() {
            println!();
            println!("Untested changes:");
            for u in &result.untested_changes {
                println!("  ⚠ {}", u);
            }
        }
    }
    Ok(())
}

pub fn coupling(top: usize, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let entries = service.coupling(top)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        println!(
            "{:<4} {:<55} {:>7} {:>8} {:>10} {:>8}",
            "#", "File", "Fan-in", "Fan-out", "Co-change", "Risk"
        );
        println!("{}", "-".repeat(96));
        for (i, e) in entries.iter().enumerate() {
            println!(
                "{:<4} {:<55} {:>7} {:>8} {:>10} {:>8.1}",
                i + 1,
                truncate_str(&e.path, 54),
                e.fan_in,
                e.fan_out,
                e.co_change_count,
                e.risk_score,
            );
        }
    }
    Ok(())
}

pub fn hotspots(top: usize, churn: bool, files: bool, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let entries = service.hotspots(top, churn, files)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        let label = if files { "File" } else { "Symbol" };
        let size_label = if files { "Bytes" } else { "Lines" };
        if churn {
            println!(
                "{:<4} {:<40} {:<50} {:>6} {:>6} {:>10}",
                "#", label, "Path", size_label, "Churn", "Score"
            );
            println!("{}", "-".repeat(120));
            for (i, e) in entries.iter().enumerate() {
                println!(
                    "{:<4} {:<40} {:<50} {:>6} {:>6} {:>10.0}",
                    i + 1,
                    truncate_str(&e.name, 39),
                    truncate_str(&e.path, 49),
                    e.lines,
                    e.churn.unwrap_or(0),
                    e.score,
                );
            }
        } else {
            println!("{:<4} {:<40} {:<50} {:>6}", "#", label, "Path", size_label);
            println!("{}", "-".repeat(104));
            for (i, e) in entries.iter().enumerate() {
                println!(
                    "{:<4} {:<40} {:<50} {:>6}",
                    i + 1,
                    truncate_str(&e.name, 39),
                    truncate_str(&e.path, 49),
                    e.lines,
                );
            }
        }
    }
    Ok(())
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", truncated)
    }
}

pub fn watch() -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let root = find_project_root(&cwd)?;
    let config = service.config().clone();
    let index_dir = root.join(".kungfu").join("index");

    println!("Watching for changes. Press Ctrl+C to stop.");
    kungfu_index::watcher::watch_and_index(&root, config, &index_dir, |stats| {
        println!(
            "Re-indexed: {} files ({} new, {} changed, {} removed), {} symbols",
            stats.total_files,
            stats.new_files,
            stats.changed_files,
            stats.removed_files,
            stats.symbols_extracted
        );
    })
}

pub fn repo_outline(budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let outline = service.repo_outline(budget)?;

    if json {
        let dirs: Vec<_> = outline
            .top_dirs
            .iter()
            .map(|d| serde_json::json!({"path": d.path, "files": d.file_count}))
            .collect();
        let out = serde_json::json!({
            "project": outline.project_name,
            "total_files": outline.total_files,
            "total_symbols": outline.total_symbols,
            "languages": outline.languages,
            "directories": dirs,
            "entrypoints": outline.entrypoints,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "Project: {} ({} files, {} symbols)",
            outline.project_name, outline.total_files, outline.total_symbols
        );
        println!();
        println!("Languages:");
        let mut langs: Vec<_> = outline.languages.iter().collect();
        langs.sort_by(|a, b| b.1.cmp(a.1));
        for (lang, count) in langs {
            println!("  {}: {}", lang, count);
        }
        println!();
        println!("Top directories:");
        for dir in &outline.top_dirs {
            println!("  {}/ ({} files)", dir.path, dir.file_count);
        }
        if !outline.entrypoints.is_empty() {
            println!();
            println!("Entrypoints:");
            for ep in &outline.entrypoints {
                println!("  {}", ep);
            }
        }
    }
    Ok(())
}

pub fn file_outline(path: &str, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let outline = service.file_outline(path)?;

    if json {
        let symbols: Vec<_> = outline
            .symbols
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "kind": s.kind,
                    "signature": s.signature,
                    "line": s.line,
                    "exported": s.exported,
                })
            })
            .collect();
        let out = serde_json::json!({
            "path": outline.path,
            "language": outline.language,
            "purpose": outline.purpose,
            "tags": outline.tags,
            "symbols": symbols,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "{} ({})",
            outline.path,
            outline.language.as_deref().unwrap_or("unknown")
        );
        if let Some(ref purpose) = outline.purpose {
            println!("Purpose: {purpose}");
        }
        if !outline.tags.is_empty() {
            println!("Tags: {}", outline.tags.join(", "));
        }
        println!();
        for s in &outline.symbols {
            let exported = if s.exported { " [pub]" } else { "" };
            if let Some(ref sig) = s.signature {
                println!("  L{} {} {}{}", s.line, s.kind, sig, exported);
            } else {
                println!("  L{} {} {}{}", s.line, s.kind, s.name, exported);
            }
        }
    }
    Ok(())
}

pub fn find_symbol(query: &str, budget: Budget, scope: Option<&str>, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let mut results = service.find_symbol(query, budget)?;

    // Apply scope filter
    if let Some(scope) = scope {
        results.retain(|r| r.item.path.starts_with(scope));
    }

    if json {
        let items: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.item.name,
                    "kind": r.item.kind.to_string(),
                    "path": r.item.path,
                    "signature": r.item.signature,
                    "line": r.item.span.start_line,
                    "score": r.score,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else if results.is_empty() {
        println!("No symbols found for '{}'", query);
    } else {
        for r in &results {
            let sig = r.item.signature.as_deref().unwrap_or(&r.item.name);
            println!(
                "  {:.2}  {}:{}  {} {}",
                r.score, r.item.path, r.item.span.start_line, r.item.kind, sig
            );
        }
    }
    Ok(())
}

pub fn get_symbol(name: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let symbol = service.get_symbol(name)?;

    match symbol {
        Some(sym) => {
            if json {
                let mut out = serde_json::to_value(&sym)?;
                // At medium+ budget, include sibling symbols from the same file
                if budget >= Budget::Medium {
                    let outline = service.file_outline(&sym.path)?;
                    let siblings: Vec<_> = outline
                        .symbols
                        .iter()
                        .filter(|s| s.name != sym.name)
                        .take(budget.top_k())
                        .map(|s| {
                            serde_json::json!({
                                "name": s.name,
                                "kind": s.kind,
                                "line": s.line,
                            })
                        })
                        .collect();
                    out["siblings"] = serde_json::json!(siblings);
                }
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("{} ({})", sym.name, sym.kind);
                println!("  path: {}:{}", sym.path, sym.span.start_line);
                if let Some(ref sig) = sym.signature {
                    println!("  sig:  {}", sig);
                }
                if sym.exported {
                    println!("  exported: yes");
                }
                if let Some(ref vis) = sym.visibility {
                    println!("  visibility: {}", vis);
                }
                if let Some(ref doc) = sym.doc_summary {
                    println!("  doc:  {}", doc);
                }
                // At medium+ budget, show sibling symbols
                if budget >= Budget::Medium {
                    let outline = service.file_outline(&sym.path)?;
                    let siblings: Vec<_> = outline
                        .symbols
                        .iter()
                        .filter(|s| s.name != sym.name)
                        .take(budget.top_k())
                        .collect();
                    if !siblings.is_empty() {
                        println!();
                        println!("  Siblings in {}:", sym.path);
                        for s in &siblings {
                            println!("    L{} {} {}", s.line, s.kind, s.name);
                        }
                    }
                }
            }
        }
        None => {
            if json {
                println!("null");
            } else {
                println!("Symbol '{}' not found", name);
            }
        }
    }
    Ok(())
}

pub fn search_text(query: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let results = service.search_text(query, budget)?;

    if json {
        let items: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.item.path,
                    "language": r.item.language,
                    "score": r.score,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else if results.is_empty() {
        println!("No results for '{}'", query);
    } else {
        for r in &results {
            println!(
                "  {:.2}  {} ({})",
                r.score,
                r.item.path,
                r.item.language.as_deref().unwrap_or("?")
            );
        }
    }
    Ok(())
}

pub fn ask_context(task: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let packet = service.ask_context(task, budget)?;
    let output = serde_json::to_string_pretty(&packet)?;
    service.track_call("ask_context", output.len(), 0);

    if json {
        println!("{}", output);
    } else {
        println!("Task:   {}", packet.query);
        if let Some(ref intent) = packet.intent {
            println!("Intent: {}", intent);
        }
        println!("Budget: {}", packet.budget);
        println!("Items:  {}", packet.items.len());
        if let Some(ref r) = packet.retrieval {
            match &r.vector_skipped {
                Some(reason) => println!("Layers: {} — {}", r.mode, reason),
                None => println!(
                    "Layers: {} ({} vector candidates)",
                    r.mode, r.vector_candidates
                ),
            }
        }
        println!();
        for item in &packet.items {
            println!(
                "  {:.2}  [{}] {} — {}",
                item.score, item.path, item.name, item.why
            );
            if let Some(ref sig) = item.signature {
                println!("        sig: {}", sig);
            }
            if let Some(ref snippet) = item.snippet {
                println!("        ---");
                for line in snippet.lines().take(10) {
                    println!("        {}", line);
                }
                let total = snippet.lines().count();
                if total > 10 {
                    println!("        ... ({} more lines)", total - 10);
                }
                println!();
            }
        }

        if !packet.memory_conflicts.is_empty() {
            println!();
            println!("Memory conflicts ({}):", packet.memory_conflicts.len());
            for c in &packet.memory_conflicts {
                println!("  on {} — {}", c.on, c.entry_ids.join(", "));
            }
        }
    }
    Ok(())
}

pub fn diff_context(budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let packet = service.diff_context(budget)?;
    let output = serde_json::to_string_pretty(&packet)?;
    service.track_call("diff_context", output.len(), 0);

    if json {
        println!("{}", output);
    } else if packet.items.is_empty() {
        println!("No changed files or relevant symbols found.");
    } else {
        println!("Diff context ({} items):", packet.items.len());
        for item in &packet.items {
            println!(
                "  {:.2}  [{}] {} — {}",
                item.score, item.path, item.name, item.why
            );
        }
    }
    Ok(())
}

pub fn semantic_search(query: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = service.semantic_search(query, budget)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let keywords = result["keywords"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let expanded = result["expanded_terms"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        // Vector mode reports `mode`; the keyword fallback reports `engine`.
        let engine = result["mode"]
            .as_str()
            .or_else(|| result["engine"].as_str())
            .unwrap_or("keyword");
        println!("Query:    {}", query);
        println!("Engine:   {}", engine);
        if !keywords.is_empty() {
            println!("Keywords: {}", keywords);
        }
        if !expanded.is_empty() {
            println!("Expanded: {}", expanded);
        }
        println!();

        if let Some(results) = result.get("results").and_then(|r| r.as_array()) {
            for r in results {
                let match_type = r["match_type"].as_str().unwrap_or("?");
                if match_type == "vector-file" {
                    println!(
                        "  {:.2} [f] {}  file — {}",
                        r["score"].as_f64().unwrap_or(0.0),
                        r["path"].as_str().unwrap_or(""),
                        r["purpose"].as_str().unwrap_or("(tags only)"),
                    );
                    continue;
                }
                let marker = if match_type == "semantic" { "~" } else { "=" };
                println!(
                    "  {:.2} [{}] {}:{}  {} {}",
                    r["score"].as_f64().unwrap_or(0.0),
                    marker,
                    r["path"].as_str().unwrap_or(""),
                    r["line"].as_u64().unwrap_or(0),
                    r["kind"].as_str().unwrap_or(""),
                    r["name"].as_str().unwrap_or(""),
                );
            }
        }
    }
    Ok(())
}

pub fn file_history(path: &str, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = service.file_history(path, 10)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else if let Some(commits) = result.get("commits").and_then(|c| c.as_array()) {
        println!("History of {}:", path);
        for c in commits {
            println!(
                "  {} {} {} — {}",
                c["hash"].as_str().unwrap_or(""),
                c["date"].as_str().unwrap_or("").get(..10).unwrap_or(""),
                c["author"].as_str().unwrap_or(""),
                c["message"].as_str().unwrap_or(""),
            );
        }
    }
    Ok(())
}

pub fn symbol_history(name: &str, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = service.symbol_history(name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        if let Some(err) = result.get("error") {
            println!("{}", err.as_str().unwrap_or("not found"));
            return Ok(());
        }
        println!(
            "{} at {}:{}",
            name,
            result["path"].as_str().unwrap_or(""),
            result["lines"].as_str().unwrap_or(""),
        );
        if let Some(blame) = result.get("blame").and_then(|b| b.as_array()) {
            if !blame.is_empty() {
                println!("  Blame:");
                for b in blame {
                    println!(
                        "    {} {} — {}",
                        b["hash"].as_str().unwrap_or(""),
                        b["author"].as_str().unwrap_or(""),
                        b["summary"].as_str().unwrap_or(""),
                    );
                }
            }
        }
        if let Some(commits) = result.get("recent_commits").and_then(|c| c.as_array()) {
            if !commits.is_empty() {
                println!("  Recent commits:");
                for c in commits {
                    println!(
                        "    {} {} {} — {}",
                        c["hash"].as_str().unwrap_or(""),
                        c["date"].as_str().unwrap_or("").get(..10).unwrap_or(""),
                        c["author"].as_str().unwrap_or(""),
                        c["message"].as_str().unwrap_or(""),
                    );
                }
            }
        }
    }
    Ok(())
}

/// Empty callers/callees: name the cause instead of a bare empty result —
/// disabled by config, stop-listed by design, no call graph built, edges
/// dropped by the frequency cutoff, an ambiguous multi-definition target, or
/// genuinely no edges — and point at `search-text` whenever the graph cannot
/// answer.
fn report_empty_call_graph(
    service: &KungfuService,
    name: &str,
    direction: &str,
    cause: EmptyCallGraphCause,
    json: bool,
) -> Result<()> {
    let (status, hint) = match &cause {
        EmptyCallGraphCause::Disabled => (
            "call_graph_disabled",
            Some(format!(
                "The call graph is disabled by config ([call_graph] enabled = false). Use `kungfu search-text {}` to find usages by name, or re-enable and reindex.",
                name
            )),
        ),
        EmptyCallGraphCause::FilteredUbiquitous => (
            "filtered_ubiquitous",
            Some(format!(
                "'{}' is on the ubiquitous-callables stop-list (std/utility name), so call edges to it are never stored. Use `kungfu search-text {}` to find usages by name.",
                name, name
            )),
        ),
        EmptyCallGraphCause::NotIndexed => (
            "call_graph_not_indexed",
            Some(format!(
                "No call relations are built for this project. Use `kungfu search-text {}` to find usages by name.",
                name
            )),
        ),
        EmptyCallGraphCause::FrequencyFiltered { max_caller_files } => (
            "frequency_filtered",
            Some(format!(
                "'{}' is called from more than {} distinct files, so its incoming call edges were dropped as utility-noise. Raise [call_graph] max_caller_files (currently {}) in .kungfu/config.toml and reindex, or use `kungfu search-text {}` to find call sites by name.",
                name, max_caller_files, max_caller_files, name
            )),
        ),
        EmptyCallGraphCause::AmbiguousTarget { definitions } => (
            "ambiguous_target",
            Some(format!(
                "'{}' has {} same-name definitions — call edges are only stored for unambiguous callees, so none were stored for this name. Use `kungfu search-text {}` to find call sites and narrow by path.",
                name, definitions, name
            )),
        ),
        EmptyCallGraphCause::NoEdges => ("no_edges", None),
    };
    let with_provenance = matches!(
        cause,
        EmptyCallGraphCause::NoEdges
            | EmptyCallGraphCause::FrequencyFiltered { .. }
            | EmptyCallGraphCause::AmbiguousTarget { .. }
    );
    if json {
        let mut body = serde_json::json!({
            "status": status,
            "name": name,
            "results": [],
        });
        if let Some(h) = &hint {
            body["hint"] = serde_json::json!(h);
        }
        match &cause {
            EmptyCallGraphCause::AmbiguousTarget { definitions } => {
                body["definitions"] = serde_json::json!(definitions);
            }
            EmptyCallGraphCause::FrequencyFiltered { max_caller_files } => {
                body["max_caller_files"] = serde_json::json!(max_caller_files);
            }
            _ => {}
        }
        if with_provenance {
            body["provenance"] = serde_json::json!(service.call_graph_provenance());
        }
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else if let Some(h) = hint {
        println!("No {} found for '{}': {}", direction, name, h);
    } else {
        println!(
            "No {} found for '{}' (note: {})",
            direction,
            name,
            service.call_graph_provenance()
        );
    }
    Ok(())
}

pub fn callers(name: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let results = service.callers(name, budget)?;

    if results.is_empty() {
        let cause = service.diagnose_empty_callers(name)?;
        return report_empty_call_graph(&service, name, "callers", cause, json);
    }
    if json {
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
        let body = serde_json::json!({
            "status": "ok",
            "name": name,
            "results": items,
            "provenance": service.call_graph_provenance(),
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!("Callers of '{}':", name);
        for (sym, _) in &results {
            let sig = sym.signature.as_deref().unwrap_or(&sym.name);
            println!(
                "  {}:{}  {} {}",
                sym.path, sym.span.start_line, sym.kind, sig
            );
        }
        println!("  (note: {})", service.call_graph_provenance());
    }
    Ok(())
}

pub fn test_subjects(name: &str, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let results = service.test_subjects(name)?;

    if json {
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
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else if results.is_empty() {
        println!("No production code found exercised by '{}'", name);
    } else {
        println!("'{}' exercises:", name);
        for (sym, reason) in &results {
            println!("  {}::{}  ({})", sym.path, sym.name, reason);
        }
    }
    Ok(())
}

pub fn callees(name: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let results = service.callees(name, budget)?;

    if results.is_empty() {
        let cause = service.diagnose_empty_callees(name)?;
        return report_empty_call_graph(&service, name, "callees", cause, json);
    }
    if json {
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
        let body = serde_json::json!({
            "status": "ok",
            "name": name,
            "results": items,
            "provenance": service.call_graph_provenance(),
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        println!("'{}' calls:", name);
        for (sym, _) in &results {
            let sig = sym.signature.as_deref().unwrap_or(&sym.name);
            println!(
                "  {}:{}  {} {}",
                sym.path, sym.span.start_line, sym.kind, sig
            );
        }
        println!("  (note: {})", service.call_graph_provenance());
    }
    Ok(())
}

pub fn explore_symbol(name: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = service.explore_symbol(name, budget)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        // Compact text output
        if let Some(err) = result.get("error") {
            println!("{}", err.as_str().unwrap_or("not found"));
            return Ok(());
        }
        if let Some(sym) = result.get("symbol") {
            println!(
                "{} ({}) at {}:{}",
                sym["name"].as_str().unwrap_or(""),
                sym["kind"].as_str().unwrap_or(""),
                sym["path"].as_str().unwrap_or(""),
                sym["line"].as_u64().unwrap_or(0),
            );
            if let Some(sig) = sym.get("signature").and_then(|s| s.as_str()) {
                println!("  sig: {}", sig);
            }
        }
        if let Some(snippet) = result.get("snippet").and_then(|s| s.as_str()) {
            println!("  ---");
            let total = snippet.lines().count();
            for line in snippet.lines().take(15) {
                println!("  {}", line);
            }
            if total > 15 {
                println!(
                    "  … {} more lines (use --json for the full snippet)",
                    total - 15
                );
            }
        }
        if let Some(siblings) = result.get("siblings_in_file").and_then(|s| s.as_array()) {
            if !siblings.is_empty() {
                println!();
                println!("  Siblings:");
                for s in siblings {
                    println!(
                        "    L{} {} {}",
                        s["line"].as_u64().unwrap_or(0),
                        s["kind"].as_str().unwrap_or(""),
                        s["name"].as_str().unwrap_or(""),
                    );
                }
            }
        }
        if let Some(others) = result.get("other_matches").and_then(|s| s.as_array()) {
            if !others.is_empty() {
                println!();
                println!("  Other matches:");
                for o in others {
                    println!(
                        "    {:.2}  {}:{}  {}",
                        o["score"].as_f64().unwrap_or(0.0),
                        o["path"].as_str().unwrap_or(""),
                        o["line"].as_u64().unwrap_or(0),
                        o["name"].as_str().unwrap_or(""),
                    );
                }
            }
        }
    }
    Ok(())
}

pub fn edit_context(name: &str, scope: Option<&str>, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = service.edit_context(name, scope)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    if let Some(err) = result.get("error") {
        println!("{}", err.as_str().unwrap_or("not found"));
        if let Some(hint) = result.get("hint").and_then(|h| h.as_str()) {
            println!("  hint: {}", hint);
        }
        return Ok(());
    }

    let sym = &result["symbol"];
    println!(
        "{} ({}) at {}:{}-{}  [{} lines]",
        sym["name"].as_str().unwrap_or(""),
        sym["kind"].as_str().unwrap_or(""),
        sym["path"].as_str().unwrap_or(""),
        result["span"]["start_line"].as_u64().unwrap_or(0),
        result["span"]["end_line"].as_u64().unwrap_or(0),
        result["line_count"].as_u64().unwrap_or(0),
    );
    if let Some(code) = result.get("code").and_then(|c| c.as_str()) {
        println!("---");
        println!("{}", code);
        println!("---");
    }
    if let Some(callees) = result.get("callees").and_then(|c| c.as_array()) {
        if !callees.is_empty() {
            println!("Calls:");
            for c in callees {
                println!(
                    "  {}:{}  {}",
                    c["path"].as_str().unwrap_or(""),
                    c["line"].as_u64().unwrap_or(0),
                    c["name"].as_str().unwrap_or(""),
                );
            }
        }
    }
    let callers_count = result["callers_count"].as_u64().unwrap_or(0);
    if callers_count > 0 {
        println!("Called by ({}):", callers_count);
        if let Some(callers) = result.get("callers").and_then(|c| c.as_array()) {
            for c in callers {
                println!(
                    "  {}:{}  {}",
                    c["path"].as_str().unwrap_or(""),
                    c["line"].as_u64().unwrap_or(0),
                    c["name"].as_str().unwrap_or(""),
                );
            }
        }
    } else if result["call_graph"].as_str() == Some("not_indexed") {
        println!("Call graph not indexed — callers/callees unavailable.");
    }
    if let Some(rationale) = result.get("rationale").and_then(|r| r.as_array()) {
        if !rationale.is_empty() {
            println!("Rationale:");
            for r in rationale {
                println!(
                    "  [{}] {}",
                    r["kind"].as_str().unwrap_or(""),
                    truncate_str(r["text"].as_str().unwrap_or(""), 100),
                );
            }
        }
    }
    Ok(())
}

pub fn explore_file(path: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = service.explore_file(path, budget)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "{} ({}) — {} symbols",
            result["path"].as_str().unwrap_or(""),
            result["language"].as_str().unwrap_or("unknown"),
            result["total_symbols"].as_u64().unwrap_or(0),
        );
        if let Some(syms) = result.get("key_symbols").and_then(|s| s.as_array()) {
            println!();
            println!("Key symbols:");
            for s in syms {
                let exported = if s["exported"].as_bool().unwrap_or(false) {
                    " [pub]"
                } else {
                    ""
                };
                if let Some(sig) = s.get("signature").and_then(|v| v.as_str()) {
                    println!(
                        "  L{} {} {}{}",
                        s["line"].as_u64().unwrap_or(0),
                        s["kind"].as_str().unwrap_or(""),
                        sig,
                        exported
                    );
                } else {
                    println!(
                        "  L{} {} {}{}",
                        s["line"].as_u64().unwrap_or(0),
                        s["kind"].as_str().unwrap_or(""),
                        s["name"].as_str().unwrap_or(""),
                        exported
                    );
                }
            }
        }
        if let Some(related) = result.get("related_files").and_then(|s| s.as_array()) {
            if !related.is_empty() {
                println!();
                println!("Related files:");
                for r in related {
                    println!(
                        "  {:.2}  {} ({})",
                        r["score"].as_f64().unwrap_or(0.0),
                        r["path"].as_str().unwrap_or(""),
                        r["language"].as_str().unwrap_or("?"),
                    );
                }
            }
        }
    }
    Ok(())
}

pub fn debug_trace(budget: Budget, json: bool) -> Result<()> {
    use std::io::Read;
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;

    let mut trace = String::new();
    std::io::stdin().read_to_string(&mut trace)?;
    if trace.trim().is_empty() {
        anyhow::bail!("no trace text on stdin");
    }

    let result = service.debug_trace(&trace, budget)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Frames: {}", result.frames.len());
        for (i, f) in result.frames.iter().enumerate() {
            let sym = f.symbol.as_deref().unwrap_or("?");
            let resolved = f.resolved_path.as_deref().unwrap_or("<unresolved>");
            println!(
                "  {}. {}:{}  [{}] -> {}",
                i, f.raw_path, f.line, sym, resolved
            );
        }
        println!("\nContext ({} items):", result.packet.items.len());
        for item in &result.packet.items {
            println!(
                "  - {}::{}  ({:.2}) {}",
                item.path, item.name, item.score, item.why
            );
        }
    }
    Ok(())
}

pub fn investigate(query: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let result = service.investigate(query, budget)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Query:  {}", result["query"].as_str().unwrap_or(""));
        if let Some(intent) = result.get("intent").and_then(|i| i.as_str()) {
            println!("Intent: {}", intent);
        }
        println!("Budget: {}", result["budget"].as_str().unwrap_or(""));

        if let Some(diff) = result.get("diff") {
            println!(
                "Diff:   {} changed files ({} relevant)",
                diff["total_changed_files"].as_u64().unwrap_or(0),
                diff.get("relevant_changed_files")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0),
            );
        }

        if let Some(items) = result.get("items").and_then(|i| i.as_array()) {
            println!("Items:  {}", items.len());
            println!();
            for item in items {
                println!(
                    "  {:.2}  [{}] {} — {}",
                    item["score"].as_f64().unwrap_or(0.0),
                    item["path"].as_str().unwrap_or(""),
                    item["name"].as_str().unwrap_or(""),
                    item["why"].as_str().unwrap_or(""),
                );
                if let Some(snippet) = item.get("snippet").and_then(|s| s.as_str()) {
                    println!("        ---");
                    let total = snippet.lines().count();
                    for line in snippet.lines().take(10) {
                        println!("        {}", line);
                    }
                    if total > 10 {
                        println!(
                            "        … {} more lines (use --json for the full snippet)",
                            total - 10
                        );
                    }
                    println!();
                }
            }
        }
    }
    Ok(())
}

pub fn mcp() -> Result<()> {
    let cwd = env::current_dir()?;
    let root = match kungfu_project::find_project_root(&cwd) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("kungfu: warning: {}", e);
            eprintln!("kungfu: using current directory as project root");
            cwd.clone()
        }
    };

    // Auto-init if not initialized
    if !root.join(".kungfu").exists() {
        eprintln!("kungfu: auto-initializing project...");
        if let Err(e) = kungfu_project::init_project(&root) {
            eprintln!("kungfu: warning: init failed: {}", e);
        }
    }

    // Auto-index if index is empty or missing
    if let Ok(service) = KungfuService::open(&root) {
        let index_dir = root.join(".kungfu").join("index");
        let needs_index = !index_dir.join("symbols.json").exists()
            || std::fs::metadata(index_dir.join("symbols.json"))
                .map(|m| m.len() < 10)
                .unwrap_or(true);
        if needs_index {
            eprintln!("kungfu: auto-indexing project...");
            match service.index_full() {
                Ok(stats) => eprintln!(
                    "kungfu: indexed {} files ({} symbols)",
                    stats.total_files, stats.symbols_extracted
                ),
                Err(e) => eprintln!("kungfu: warning: index failed: {}", e),
            }
        }
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(kungfu_mcp::run_stdio_server(root))?;
    Ok(())
}

pub fn change_timeline(name: &str, budget: Budget, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let events = service.change_timeline(name, budget)?;
    let output = serde_json::to_string_pretty(&events)?;
    service.track_call("change_timeline", output.len(), 0);

    if json {
        println!("{}", output);
    } else if events.is_empty() {
        println!("No timeline events for: {}", name);
    } else {
        println!("Timeline for {} ({} events):", name, events.len());
        println!();
        for e in &events {
            let date = e.date.as_deref().unwrap_or("—");
            println!("  [{}] {} ({})", e.event_type, e.detail, date);
        }
    }
    Ok(())
}

pub fn memory(action: MemoryCommands, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;

    match action {
        MemoryCommands::Add {
            content,
            kind,
            title,
            tags,
            files,
            symbols,
            pin,
        } => {
            let kind: ProjectMemoryKind = kind.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let entry =
                service.memory_add(kind, &content, title.as_deref(), tags, files, symbols, pin)?;
            service.track_call("memory_add", 0, 0);
            if json {
                println!("{}", serde_json::to_string_pretty(&entry)?);
            } else {
                println!("Added: {} [{}] {}", entry.id, entry.kind, entry.content);
                if entry.pinned {
                    println!("       (pinned)");
                }
            }
        }
        MemoryCommands::List { kind, tag, pinned } => {
            let filter = MemoryFilter {
                kind: kind
                    .as_deref()
                    .map(|k| k.parse().map_err(|e: String| anyhow::anyhow!(e)))
                    .transpose()?,
                tag,
                pinned_only: pinned,
                ..Default::default()
            };
            let entries = service.memory_list(&filter)?;
            service.track_call("memory_list", 0, 0);
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else if entries.is_empty() {
                println!("No memory entries found.");
            } else {
                for e in &entries {
                    let pin_marker = if e.pinned { " [pinned]" } else { "" };
                    let title = e.title.as_deref().unwrap_or(&e.content);
                    let title_short = truncate_str(title, 60);
                    println!("  {} [{}]{} {}", e.id, e.kind, pin_marker, title_short);
                }
                println!("\n{} entries", entries.len());
            }
        }
        MemoryCommands::Show { id } => {
            let entry = service.memory_show(&id)?;
            service.track_call("memory_show", 0, 0);
            if json {
                println!("{}", serde_json::to_string_pretty(&entry)?);
            } else {
                println!("ID:      {}", entry.id);
                println!("Kind:    {}", entry.kind);
                println!("Status:  {}", entry.status);
                if let Some(ref t) = entry.title {
                    println!("Title:   {}", t);
                }
                println!("Pinned:  {}", entry.pinned);
                println!("Created: {}", entry.created_at);
                println!("Updated: {}", entry.updated_at);
                if !entry.tags.is_empty() {
                    println!("Tags:    {}", entry.tags.join(", "));
                }
                if !entry.related_files.is_empty() {
                    println!("Files:   {}", entry.related_files.join(", "));
                }
                if !entry.related_symbols.is_empty() {
                    println!("Symbols: {}", entry.related_symbols.join(", "));
                }
                if let Some(ref s) = entry.supersedes {
                    println!("Supersedes: {}", s);
                }
                println!();
                println!("{}", entry.content);
            }
        }
        MemoryCommands::Search { query, kind, tag } => {
            let filter = MemoryFilter {
                kind: kind
                    .as_deref()
                    .map(|k| k.parse().map_err(|e: String| anyhow::anyhow!(e)))
                    .transpose()?,
                tag,
                ..Default::default()
            };
            let results = service.memory_search(&query, &filter)?;
            service.track_call("memory_search", 0, 0);
            if json {
                // Mirror the MCP tool: full entry only for confident hits; below the
                // threshold emit a stub + excerpt so weak matches don't flood context.
                const FULL_CONTENT_THRESHOLD: f64 = 0.45;
                let items: Vec<_> = results
                    .iter()
                    .map(|(score, e)| {
                        if *score >= FULL_CONTENT_THRESHOLD {
                            serde_json::json!({ "score": score, "entry": e })
                        } else {
                            serde_json::json!({
                                "score": score,
                                "id": e.id,
                                "kind": e.kind,
                                "title": e.title,
                                "excerpt": truncate_str(&e.content, 160),
                                "hint": "low relevance — run `kungfu memory show <id>` for the full entry",
                            })
                        }
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else if results.is_empty() {
                println!("No results for: {}", query);
            } else {
                for (score, e) in &results {
                    let pin_marker = if e.pinned { " [pinned]" } else { "" };
                    let title = e.title.as_deref().unwrap_or(&e.content);
                    println!(
                        "  {:.2}  {} [{}]{} {}",
                        score,
                        e.id,
                        e.kind,
                        pin_marker,
                        truncate_str(title, 50)
                    );
                }
            }
        }
        MemoryCommands::Update {
            id,
            content,
            title,
            tags,
            pin,
        } => {
            let tags_opt = if tags.is_empty() { None } else { Some(tags) };
            let entry =
                service.memory_update(&id, content.as_deref(), title.as_deref(), tags_opt, pin)?;
            service.track_call("memory_update", 0, 0);
            if json {
                println!("{}", serde_json::to_string_pretty(&entry)?);
            } else {
                println!("Updated: {}", entry.id);
            }
        }
        MemoryCommands::Archive { id } => {
            let entry = service.memory_archive(&id)?;
            service.track_call("memory_archive", 0, 0);
            if json {
                println!("{}", serde_json::to_string_pretty(&entry)?);
            } else {
                println!("Archived: {}", entry.id);
            }
        }
        MemoryCommands::Remove { id, yes } => {
            if !yes {
                eprintln!("Use --yes to confirm permanent deletion of {}", id);
                std::process::exit(1);
            }
            service.memory_remove(&id)?;
            service.track_call("memory_remove", 0, 0);
            if json {
                println!("{}", serde_json::json!({"removed": id}));
            } else {
                println!("Removed: {}", id);
            }
        }
        MemoryCommands::Pin { id } => {
            let entry = service.memory_pin(&id)?;
            service.track_call("memory_pin", 0, 0);
            if json {
                println!("{}", serde_json::to_string_pretty(&entry)?);
            } else {
                println!("Pinned: {}", entry.id);
            }
        }
        MemoryCommands::Unpin { id } => {
            let entry = service.memory_unpin(&id)?;
            service.track_call("memory_unpin", 0, 0);
            if json {
                println!("{}", serde_json::to_string_pretty(&entry)?);
            } else {
                println!("Unpinned: {}", entry.id);
            }
        }
    }
    Ok(())
}

pub fn annotate(path: &str, purpose: &str, terms: &[String], json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let mut term_map = std::collections::BTreeMap::new();
    for t in terms {
        let (k, v) = t
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--term expects term=meaning, got: {t}"))?;
        term_map.insert(k.trim().to_string(), v.trim().to_string());
    }
    let result = service.annotate_file(path, purpose, term_map)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}: {}", result.status, result.path);
        println!("  {}", result.purpose);
        if result.terms_recorded > 0 {
            println!("  terms recorded: {}", result.terms_recorded);
        }
        println!("  → {}", result.hint);
    }
    Ok(())
}

pub fn annotation_queue(limit: usize, json: bool) -> Result<()> {
    let cwd = env::current_dir()?;
    let service = KungfuService::open(&cwd)?;
    let queue = service.annotation_queue(limit)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&queue)?);
    } else {
        println!(
            "{} files without purpose (showing {}):",
            queue.total_unannotated,
            queue.items.len()
        );
        for item in &queue.items {
            let tags = if item.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", item.tags.join(", "))
            };
            println!("  {}{} — {}", item.path, tags, item.why);
        }
        println!();
        println!("→ {}", queue.instruction);
    }
    Ok(())
}

/// Merged update settings for wherever the command was run: global config,
/// overridden by the project's `.kungfu/config.toml`, overridden by environment.
/// Missing or unreadable config degrades to defaults — never to an error.
pub fn update_config() -> kungfu_config::UpdateConfig {
    let project_config = env::current_dir()
        .ok()
        .and_then(|cwd| find_project_root(&cwd).ok())
        .map(|root| root.join(".kungfu").join("config.toml"))
        .filter(|p| p.exists());
    kungfu_config::KungfuConfig::load_merged(project_config.as_deref())
        .unwrap_or_default()
        .update
}

/// `kungfu update` — check for and install a new release.
///
/// Explicit invocations always hit the network; `--quiet` (the SessionStart-hook
/// shape) respects the `[update] check` setting and the 24h cache instead.
pub fn update(check_only: bool, to: Option<String>, quiet: bool, json: bool) -> Result<()> {
    let config = update_config();
    let repo = kungfu_update::github::REPO;

    if check_only {
        let status = if quiet {
            kungfu_update::ensure_checked(&config, repo)?
        } else {
            kungfu_update::check_now(repo, kungfu_update::CHECK_TIMEOUT)?
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "current": status.current,
                    "latest": status.latest,
                    "update_available": status.update_available,
                    "checked_at": status.checked_at,
                    "source": status.source.as_str(),
                }))?
            );
        } else if status.update_available || !quiet {
            println!("{}", status.summary());
        }
        return Ok(());
    }

    let target = match to {
        Some(pinned) => kungfu_update::version::normalize(&pinned),
        None => {
            let status = kungfu_update::check_now(repo, kungfu_update::CHECK_TIMEOUT)?;
            let Some(latest) = status.latest.clone() else {
                anyhow::bail!("could not determine the latest release; retry or use --to X.Y.Z");
            };
            if !status.update_available {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "status": "up_to_date",
                            "current": status.current,
                            "latest": latest,
                        }))?
                    );
                } else {
                    println!("{}", status.summary());
                }
                return Ok(());
            }
            latest
        }
    };

    let applied = kungfu_update::apply::apply(repo, &target, kungfu_update::CURRENT_VERSION)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "updated",
                "from": applied.from,
                "to": applied.to,
                "path": applied.path.to_string_lossy(),
                "checksum": applied.checksum.as_str(),
                "restart_required": "restart the MCP server / Claude Code session to load the new binary",
            }))?
        );
    } else {
        println!("kungfu {} → {}", applied.from, applied.to);
        println!("  {}", applied.path.display());
        println!("  checksum: {}", applied.checksum.as_str());
        println!();
        println!("Restart Claude Code (or reconnect MCP via /mcp) — a running server keeps the old binary.");
        println!("The index migrates itself on the next call if the schema changed.");
    }
    Ok(())
}
