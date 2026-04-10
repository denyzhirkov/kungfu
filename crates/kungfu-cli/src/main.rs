mod commands;

use clap::{Parser, Subcommand};
use kungfu_types::Budget;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "kungfu", version, about = "Context retrieval and distillation engine for coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize kungfu in the current project
    Init,

    /// Show project status and index health
    Status,

    /// Validate installation, config, and index integrity
    Doctor,

    /// Show current configuration
    #[command(name = "config")]
    Config,

    /// Build or update the project index
    Index {
        /// Force full rebuild
        #[arg(long)]
        full: bool,

        /// Index only changed files
        #[arg(long)]
        changed: bool,
    },

    /// Remove caches and indexes
    Clean,

    /// Show compact repo structure
    #[command(name = "repo-outline")]
    RepoOutline {
        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Show file structure and symbols
    #[command(name = "file-outline")]
    FileOutline {
        /// Path to the file
        path: String,
    },

    /// Search symbols by name
    #[command(name = "find-symbol")]
    FindSymbol {
        /// Symbol name or pattern
        query: String,

        #[arg(long, default_value = "small")]
        budget: String,

        /// Limit results to files under this path prefix
        #[arg(long)]
        scope: Option<String>,
    },

    /// Get detailed symbol info
    #[command(name = "get-symbol")]
    GetSymbol {
        /// Symbol name
        name: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Search text across indexed files
    #[command(name = "search-text")]
    SearchText {
        /// Search query
        query: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Find related files
    #[command(name = "related")]
    Related {
        /// File path
        path: String,

        #[arg(long, default_value = "medium")]
        budget: String,
    },

    /// Build minimal context packet for a query
    #[command(name = "context")]
    Context {
        /// Natural language query
        query: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Smart context retrieval: parse intent, multi-strategy search, ranked packet
    #[command(name = "ask-context")]
    AskContext {
        /// Task description in natural language
        task: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Build context from git diff
    #[command(name = "diff-context")]
    DiffContext {
        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Semantic search: find symbols by concept with query expansion
    #[command(name = "semantic-search")]
    SemanticSearch {
        /// Query (e.g. "auth logic", "database connection")
        query: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Show git history for a file
    #[command(name = "file-history")]
    FileHistory {
        /// File path
        path: String,
    },

    /// Show git blame + commits for a symbol
    #[command(name = "symbol-history")]
    SymbolHistory {
        /// Symbol name
        name: String,
    },

    /// Composite: explore a symbol — find + detail + related + snippet in one call
    #[command(name = "explore-symbol")]
    ExploreSymbol {
        /// Symbol name
        name: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Composite: explore a file — outline + related files + key symbols in one call
    #[command(name = "explore-file")]
    ExploreFile {
        /// File path
        path: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Find all symbols that call the given symbol
    #[command(name = "callers")]
    Callers {
        /// Symbol name
        name: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Find all symbols called by the given symbol
    #[command(name = "callees")]
    Callees {
        /// Symbol name
        name: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Composite: investigate a query — smart context + diff awareness in one call
    #[command(name = "investigate")]
    Investigate {
        /// Natural language query
        query: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Show largest symbols or files (hotspots), optionally weighted by git churn
    Hotspots {
        /// Number of results
        #[arg(long, default_value = "20")]
        top: usize,

        /// Weight by git change frequency (LOC × commits)
        #[arg(long)]
        churn: bool,

        /// Show file-level hotspots instead of symbol-level
        #[arg(long)]
        files: bool,
    },

    /// Generate project onboarding summary: architecture, patterns, key symbols
    Onboard,

    /// Blast radius analysis: transitive callers and dependents of a symbol
    Affected {
        /// Symbol name
        name: String,

        /// Max depth of transitive analysis
        #[arg(long, default_value = "3")]
        depth: usize,
    },

    /// Find minimal test set to run based on git diff
    #[command(name = "smart-test")]
    SmartTest,

    /// Code review context: risks, missing co-changes, untested code
    Review,

    /// Analyze module coupling: fan-in, fan-out, co-change frequency
    Coupling {
        /// Number of results
        #[arg(long, default_value = "20")]
        top: usize,
    },

    /// Search design rationale: TODOs, doc sections, ADR decisions
    #[command(name = "search-rationale")]
    SearchRationale {
        /// Search query
        query: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Show how a symbol or file evolved: churn, decisions, recent changes
    #[command(name = "change-timeline")]
    ChangeTimeline {
        /// Symbol or file name
        name: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Show accumulated usage statistics
    Stats,

    /// Watch filesystem and re-index on changes
    Watch,

    /// Start MCP server over stdio
    Mcp,
}

fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_target(false)
        .init();

    let json = cli.json;

    let result = match cli.command {
        Commands::Init => commands::init(json),
        Commands::Status => commands::status(json),
        Commands::Doctor => commands::doctor(json),
        Commands::Config => commands::config_show(json),
        Commands::Index { full, changed } => commands::index(full, changed, json),
        Commands::Clean => commands::clean(json),
        Commands::RepoOutline { budget } => {
            commands::repo_outline(parse_budget(&budget), json)
        }
        Commands::FileOutline { path } => commands::file_outline(&path, json),
        Commands::FindSymbol { query, budget, scope } => {
            commands::find_symbol(&query, parse_budget(&budget), scope.as_deref(), json)
        }
        Commands::GetSymbol { name, budget } => {
            commands::get_symbol(&name, parse_budget(&budget), json)
        }
        Commands::SearchText { query, budget } => {
            commands::search_text(&query, parse_budget(&budget), json)
        }
        Commands::Related { path, budget } => {
            commands::related(&path, parse_budget(&budget), json)
        }
        Commands::Context { query, budget } => {
            commands::context(&query, parse_budget(&budget), json)
        }
        Commands::AskContext { task, budget } => {
            commands::ask_context(&task, parse_budget(&budget), json)
        }
        Commands::DiffContext { budget } => {
            commands::diff_context(parse_budget(&budget), json)
        }
        Commands::SemanticSearch { query, budget } => {
            commands::semantic_search(&query, parse_budget(&budget), json)
        }
        Commands::FileHistory { path } => commands::file_history(&path, json),
        Commands::SymbolHistory { name } => commands::symbol_history(&name, json),
        Commands::Callers { name, budget } => {
            commands::callers(&name, parse_budget(&budget), json)
        }
        Commands::Callees { name, budget } => {
            commands::callees(&name, parse_budget(&budget), json)
        }
        Commands::ExploreSymbol { name, budget } => {
            commands::explore_symbol(&name, parse_budget(&budget), json)
        }
        Commands::ExploreFile { path, budget } => {
            commands::explore_file(&path, parse_budget(&budget), json)
        }
        Commands::Investigate { query, budget } => {
            commands::investigate(&query, parse_budget(&budget), json)
        }
        Commands::Hotspots { top, churn, files } => commands::hotspots(top, churn, files, json),
        Commands::Onboard => commands::onboard(json),
        Commands::Affected { name, depth } => commands::affected(&name, depth, json),
        Commands::SmartTest => commands::smart_test(json),
        Commands::Review => commands::review(json),
        Commands::Coupling { top } => commands::coupling(top, json),
        Commands::SearchRationale { query, budget } => {
            commands::search_rationale(&query, parse_budget(&budget), json)
        }
        Commands::ChangeTimeline { name, budget } => {
            commands::change_timeline(&name, parse_budget(&budget), json)
        }
        Commands::Stats => commands::stats(json),
        Commands::Watch => commands::watch(),
        Commands::Mcp => commands::mcp(),
    };

    if let Err(e) = result {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
}

fn parse_budget(s: &str) -> Budget {
    s.parse().unwrap_or(Budget::Small)
}
