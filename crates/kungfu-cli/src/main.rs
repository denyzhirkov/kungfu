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

    /// Search across files and symbols. Use --semantic for query expansion
    Search {
        /// Search query
        query: String,

        #[arg(long, default_value = "small")]
        budget: String,

        /// Enable semantic search with query expansion
        #[arg(long)]
        semantic: bool,
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

    /// Show how a symbol or file evolved: churn, decisions, recent changes
    #[command(name = "change-timeline")]
    ChangeTimeline {
        /// Symbol or file name
        name: String,

        #[arg(long, default_value = "small")]
        budget: String,
    },

    /// Project memory management
    Memory {
        #[command(subcommand)]
        action: MemoryCommands,
    },

    /// Show accumulated usage statistics
    Stats,

    /// Watch filesystem and re-index on changes
    Watch,

    /// Start MCP server over stdio
    Mcp,
}

#[derive(Subcommand)]
pub enum MemoryCommands {
    /// Add a new project memory entry
    Add {
        /// Content of the memory entry
        content: String,

        /// Kind: fact, decision, warning, session_summary
        #[arg(long, default_value = "fact")]
        kind: String,

        /// Short title
        #[arg(long)]
        title: Option<String>,

        /// Tags (can be repeated)
        #[arg(long = "tag")]
        tags: Vec<String>,

        /// Related file paths (can be repeated)
        #[arg(long = "file")]
        files: Vec<String>,

        /// Related symbol names (can be repeated)
        #[arg(long = "symbol")]
        symbols: Vec<String>,

        /// Pin this entry for higher priority in context
        #[arg(long)]
        pin: bool,
    },

    /// List project memory entries
    List {
        /// Filter by kind: fact, decision, warning, session_summary
        #[arg(long)]
        kind: Option<String>,

        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,

        /// Show only pinned entries
        #[arg(long)]
        pinned: bool,
    },

    /// Show a single memory entry in detail
    Show {
        /// Memory entry ID (e.g. mem_0001)
        id: String,
    },

    /// Search project memory
    Search {
        /// Search query
        query: String,

        /// Filter by kind
        #[arg(long)]
        kind: Option<String>,

        /// Filter by tag
        #[arg(long)]
        tag: Option<String>,
    },

    /// Update a memory entry
    Update {
        /// Memory entry ID
        id: String,

        /// New content
        #[arg(long)]
        content: Option<String>,

        /// New title
        #[arg(long)]
        title: Option<String>,

        /// Replace tags
        #[arg(long = "tag")]
        tags: Vec<String>,

        /// Set pinned state
        #[arg(long)]
        pin: Option<bool>,
    },

    /// Archive a memory entry
    Archive {
        /// Memory entry ID
        id: String,
    },

    /// Remove a memory entry permanently
    Remove {
        /// Memory entry ID
        id: String,

        /// Skip confirmation
        #[arg(long)]
        yes: bool,
    },

    /// Pin a memory entry
    Pin {
        /// Memory entry ID
        id: String,
    },

    /// Unpin a memory entry
    Unpin {
        /// Memory entry ID
        id: String,
    },
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
        Commands::Search { query, budget, semantic } => {
            if semantic {
                commands::semantic_search(&query, parse_budget(&budget), json)
            } else {
                commands::search_text(&query, parse_budget(&budget), json)
            }
        }
        Commands::AskContext { task, budget } => {
            commands::ask_context(&task, parse_budget(&budget), json)
        }
        Commands::DiffContext { budget } => {
            commands::diff_context(parse_budget(&budget), json)
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
        Commands::ChangeTimeline { name, budget } => {
            commands::change_timeline(&name, parse_budget(&budget), json)
        }
        Commands::Memory { action } => commands::memory(action, json),
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
