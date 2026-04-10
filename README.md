# kungfu

Local context retrieval engine for coding agents. Indexes codebases, resolves dependencies, and delivers the smallest useful context packet — so agents read fewer files and waste fewer tokens.

## Why

Agents burn tokens exploring codebases: reading files, grepping, guessing structure. Kungfu replaces that with one call:

```
$ kungfu ask-context "add C++ language support" --budget tiny
Task:   add C++ language support
Intent: lookup
Budget: tiny
Items:  3

  0.50  [crates/kungfu-parse/src/lib.rs] extract_symbols
  0.45  [crates/kungfu-config/src/lib.rs] LanguagesConfig
  0.45  [crates/kungfu-types/src/file.rs] Language
```

**275 tokens** instead of reading files manually (~7,500+ tokens per file).

## Install

Recommended — single binary, no dependencies:

```sh
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/denyzhirkov/kungfu/master/install.sh | sh

# Windows (PowerShell)
irm https://raw.githubusercontent.com/denyzhirkov/kungfu/master/install.ps1 | iex
```

Supports macOS (ARM64, x86_64), Linux (ARM64, x86_64), and Windows (x86_64).

Build from source:

```sh
git clone https://github.com/denyzhirkov/kungfu.git
cd kungfu
cargo build --release
cp target/release/kungfu ~/.local/bin/
```

## Quick start

```sh
kungfu init          # initialize in project root
kungfu index         # build the index
kungfu doctor        # verify everything works
```

## CLI commands

### Core retrieval

```sh
# Smart context — the main command
kungfu ask-context "find where JWT refresh is implemented" --budget small
kungfu ask-context "impact of changing Budget enum" --budget medium
kungfu ask-context "fix crash in python parser" --budget tiny

# Simple context (no intent detection)
kungfu context "incremental indexing" --budget small

# Context from git changes
kungfu diff-context --budget small
```

### Search

```sh
kungfu find-symbol AuthService          # search symbols by name
kungfu get-symbol refreshToken          # exact symbol lookup
kungfu search-text "refresh token"      # text search across files
kungfu related src/auth/service.ts      # find related files (imports, tests, configs)
```

### Structure

```sh
kungfu repo-outline                     # compact repo map
kungfu file-outline src/auth/service.ts # symbols in a file
```

### Analysis

```sh
kungfu onboard                          # project summary: architecture, patterns, key symbols
kungfu affected AuthService --depth 3   # blast radius: who depends on this symbol?
kungfu smart-test                       # minimal test set based on git diff
kungfu review                           # code review context: risks, missing co-changes
kungfu coupling --top 10                # module coupling: fan-in, fan-out, co-change
kungfu hotspots --churn                 # largest/most changed code
```

### Maintenance

```sh
kungfu index --full                     # full rebuild
kungfu index --changed                  # reindex only git-changed files
kungfu doctor                           # validate installation and index
kungfu watch                            # auto re-index on file changes
kungfu clean                            # wipe index and cache
kungfu config                           # show current configuration
```

All commands support `--json` for machine output and `--budget tiny|small|medium|full`.

## ask-context

The highest-value command. Given a task description, it:

1. **Detects intent** — lookup, debug, understand, or impact
2. **Runs multiple search strategies** — symbol search, text search, related files, import chains
3. **Applies contextual bonuses** — changed files (+0.3), test/config proximity (+0.15), language weighting, [tunable weights](#tuning)
4. **Returns a ranked packet** with signatures and code snippets

### Intent detection

| Intent | Triggers | Extra strategies |
|--------|----------|-----------------|
| `lookup` | find, where, show, search | symbol + text search |
| `debug` | bug, fix, error, crash | + related files, error symbol boost |
| `understand` | how, explain, what, why | + sibling symbols from same file |
| `impact` | impact, refactor, rename, delete | + import chain, sibling symbols |

### Budget levels

| Budget | Items | Snippets | Use case |
|--------|-------|----------|----------|
| `auto` | adaptive | adaptive | **Default** — adapts to project size |
| `tiny` | 3 | none | Quick pointers — "where to look" |
| `small` | 5 | 20 lines | Signatures + context |
| `medium` | 8 | 40 lines | Deeper exploration |
| `full` | 12 | 100 lines | Complete picture |

## MCP server

```sh
kungfu mcp
```

Add to your agent config (Claude Code, Cursor, etc.):

```json
{
  "mcpServers": {
    "kungfu": {
      "command": "kungfu",
      "args": ["mcp"]
    }
  }
}
```

### 28 MCP tools

| Tool | Description |
|------|-------------|
| `project_status` | File count, symbol count, languages, git status |
| `repo_outline` | Top directories, language distribution, entrypoints |
| `file_outline` | Symbols, signatures, exports for a file |
| `find_symbol` | Search symbols by name (exact + fuzzy + stem) |
| `get_symbol` | Detailed symbol info by exact name |
| `search_text` | Text search across indexed files |
| `find_files` | Find files by path pattern |
| `semantic_search` | Find symbols by concept with query expansion |
| `find_related_files` | Related files by imports, tests, configs, proximity |
| `find_related_symbols` | Related symbols in same file |
| `get_minimal_context` | Smallest high-confidence context set |
| `build_task_context` | Ranked context packet for a task |
| `ask_context` | Smart retrieval: intent + multi-strategy search + rationale + evidence |
| `diff_context` | Context focused on git changes |
| `explore_symbol` | Composite: find + detail + related + snippet in one call |
| `explore_file` | Composite: outline + related files + key symbols |
| `investigate` | Composite: ask_context + diff awareness |
| `callers` | Call graph: who calls this symbol? |
| `callees` | Call graph: what does this symbol call? |
| `file_history` | Git log for a file: recent commits |
| `symbol_history` | Git blame + commits for a symbol |
| `search_rationale` | Search design decisions, TODOs, doc sections by query |
| `change_timeline` | How code evolved: introduced, churn, decisions, recent changes |
| `onboard` | Project summary: architecture, patterns, key symbols, naming |
| `affected` | Blast radius: transitive callers/dependents of a symbol |
| `smart_test` | Minimal test set based on git diff |
| `review` | Code review context: risks, missing co-changes, untested code |
| `coupling` | Module coupling: fan-in, fan-out, co-change frequency |
| `usage_stats` | Token savings, cache hit rate, calls served |

## Agent rules

Add to `CLAUDE.md` or system prompt of your project:

### MCP (recommended)

```markdown
## Context retrieval
- Before reading files, use `kungfu` MCP tools to understand the codebase.
- Use `ask_context` with the task description to get a ranked context packet.
- Use `find_symbol` / `get_symbol` to locate code by name instead of reading whole files.
- Use `explore_symbol` for a complete picture: definition + related symbols + snippet in one call.
- Use `file_outline` before reading a file — often the outline is enough.
- Use `onboard` when first encountering a project for architecture overview.
- After git changes, use `diff_context` to focus on what changed.
- Use `investigate` for complex tasks — combines ask_context + diff awareness.
- Use `affected` before refactoring to check blast radius.
- Use `smart_test` to find which tests to run after changes.
- Use `search_rationale` to find design decisions, TODOs, and doc context for a topic.
- Use `change_timeline` to understand how a symbol or file evolved over time.
- Use `ask_context` with `include: ["code", "rationale"]` to get both code and design context.
- Only read full files when the above tools confirm you need them.
- Prefer tiny/small budget. Escalate to medium/full only when needed.
```

### CLI (alternative)

```markdown
## Context retrieval
- Before reading files, run `kungfu ask-context "<task>" --budget small` via Bash.
- Use `kungfu find-symbol <name>` to locate code by name instead of reading whole files.
- Use `kungfu file-outline <path>` before reading a file — often the outline is enough.
- Use `kungfu onboard` when first encountering a project.
- After git changes, run `kungfu diff-context` to focus on what changed.
- Use `kungfu affected <symbol>` before refactoring to check blast radius.
- Only read full files when kungfu output confirms you need them.
- Prefer tiny/small budget. Escalate to medium/full only when needed.
```

### Recommended workflow

```
Task received
    │
    ▼
ask_context("task description", budget: "tiny")    ← MCP tool or: kungfu ask-context "..." --budget tiny --json
    │
    ├── Got enough? → Start working
    │
    ├── Need more detail? → escalate to budget: "small"
    │
    ├── Need specific symbol? → find_symbol / get_symbol
    │
    ├── Need file structure? → file_outline
    │
    └── Only then → Read full file
```

### How an agent uses kungfu

Agent receives task: *"fix the authentication bug"*

**Without kungfu** — agent explores blindly:
```
Glob("**/*.ts")           → 200 files, which to read?
Grep("auth")              → 30 matches across many files
Read("src/auth/service.ts")    → 500 lines, 2000 tokens
Read("src/auth/middleware.ts") → 300 lines, 1200 tokens
Read("src/auth/controller.ts") → 400 lines, 1600 tokens
... more files, more tokens
```

**With kungfu** — one MCP call:
```json
→ tools/call: ask_context({"query": "authentication bug fix", "budget": "small"})

← {
    "intent": "debug",
    "items": [
      {
        "name": "AuthController",
        "path": "src/modules/auth/auth.controller.ts",
        "signature": "class AuthController",
        "why": "symbol name match",
        "score": 0.50,
        "snippet": "export class AuthController {\n  constructor(\n    private readonly authService: AuthService,\n  ) {}\n\n  @Post('login')\n  async login(@Body() dto: LoginDto) {\n    return this.authService.login(dto);\n  }\n  ..."
      },
      {
        "name": "AuthService",
        "path": "src/modules/auth/auth.service.ts",
        "signature": "class AuthService",
        "why": "symbol name match",
        "score": 0.50,
        "snippet": "export class AuthService {\n  async login(dto: LoginDto) { ... }\n  async validateToken(token: string) { ... }\n  ..."
      }
    ],
    "rationale": [
      {
        "source": "docs/adr/001-auth-jwt.md",
        "kind": "decision",
        "text": "We chose JWT for stateless session management to avoid server-side session storage",
        "relevance": 0.72
      }
    ],
    "evidence": [
      {
        "source": "docs/adr/001-auth-jwt.md",
        "excerpt": "We chose JWT for stateless session management..."
      }
    ]
  }
```

Agent immediately knows where to look *and why it was designed that way*. Then reads only the relevant file.

### Token savings (open-source projects)

| Project | Lang | Files | Query | kungfu | naive grep+read | Savings |
|---------|------|-------|-------|--------|-----------------|---------|
| [ruff](https://github.com/astral-sh/ruff) | Rust | 9,702 | "how does the linter check rules" | 722 | 137,767 | **190x** |
| [ollama](https://github.com/ollama/ollama) | Go | 1,834 | "how does model loading and inference work" | 1,100 | 110,122 | **100x** |
| [SolidJS](https://github.com/solidjs/solid) | TS | 168 | "how does the reactive signal system work" | 459 | 33,577 | **73x** |
| [leptos](https://github.com/leptos-rs/leptos) | Rust | 1,453 | "how does reactive rendering work" | 593 | 40,772 | **68x** |
| [tRPC](https://github.com/trpc/trpc) | TS | 1,290 | "how does the RPC client call server procedures" | 680 | 43,773 | **64x** |
| [pydantic](https://github.com/pydantic/pydantic) | Python | 729 | "how does field validation work" | 977 | 53,864 | **55x** |
| [FastAPI](https://github.com/fastapi/fastapi) | Python | 2,882 | "how does dependency injection work" | 624 | 17,089 | **27x** |

## How it works

### Indexing
- Scans project files respecting `.gitignore` and configurable ignore rules
- Parses code with [tree-sitter](https://tree-sitter.github.io/) — Rust, TypeScript, JavaScript, Python, Go, Java, C#, Kotlin, C, C++
- Extracts symbols: functions, classes, structs, methods, traits, interfaces, types, constants
- Extracts imports from AST and resolves them to actual files in the project
- Builds relations: `imports`, `test_for`, `config_for`, `calls`
- Extracts function call graph from AST (callers/callees)
- Extracts structured comments (TODO, FIXME, NOTE, doc comments) and populates `doc_summary` on symbols
- Parses markdown documentation and ADR files into searchable memory entries
- Incremental re-indexing via blake3 file fingerprints

### Search & ranking
- Exact match (1.0), prefix (0.9), contains (0.7), stem (0.6), fuzzy (0.4)
- Exact phrase matching: `snake_case`, `camelCase`, space-separated
- Simple English stemming: "ranking" finds "rank", "indexing" finds "index"
- Path matching with filename boost (0.9) vs directory match (0.6)

### Context assembly
- Multi-strategy search: symbols, text, related files, import chains, semantic expansion
- Deduplication by (path, name)
- Changed-file bonus from git
- Test/config proximity bonuses based on query intent
- Language importance weighting (primary language prioritized)
- Budget-controlled output with code snippets

### Memory & rationale layer

Kungfu answers not only *"where is this?"* but also *"why is it like this?"*:

- **Comment extraction** — TODO, FIXME, NOTE, and doc comments are extracted from tree-sitter ASTs during indexing
- **Doc & ADR parsing** — markdown files in `docs/`, `adr/`, `decisions/` are parsed into searchable memory entries; ADR "Decision" sections get higher ranking weight
- **Rationale in context** — `ask_context` returns a `rationale[]` field with matched design decisions, doc excerpts, and annotated comments alongside code items
- **Evidence fragments** — verbatim excerpts from source docs/comments are attached as `evidence[]` for agent trust and traceability
- **Layered context** — use `include: ["code", "rationale", "history"]` to control what layers are returned
- **Change timeline** — `change_timeline` shows how code evolved: when introduced, churn rate, linked decisions, recent changes
- **Doc summaries** — `doc_summary` on symbols is auto-populated from preceding doc comments (first sentence, up to 120 chars)

### Storage
```
.kungfu/
  config.toml          # project configuration
  project.json         # project metadata
  index/
    files.json         # indexed files with hashes
    symbols.json       # extracted symbols with spans
    relations.json     # import/test/config relations
    fingerprints.json  # blake3 hashes for incremental rebuilds
    memories.json      # rationale: comments, doc sections, ADR decisions
```

## Configuration

`.kungfu/config.toml`:

```toml
project_name = "my-project"

[ignore]
paths = ["node_modules", "dist", "build", ".git", "target"]

[languages]
enabled = ["typescript", "javascript", "rust", "go", "python", "java", "csharp", "kotlin", "c", "cpp", "json", "markdown", "yaml", "toml"]

[search]
default_budget = "small"
default_top_k = 5

[index]
incremental = true

[git]
enabled = true
```

## Benchmarks

Tested across popular open-source projects (Apple Silicon):

| Project | Language | Files | Symbols | Index | ask-context |
|---------|----------|------:|--------:|------:|------------:|
| express | JS       |   201 |   1,948 |  0.5s |       227ms |
| flask   | Python   |   217 |   1,629 |  0.6s |       228ms |
| gin     | Go       |   118 |   1,487 |  0.7s |       217ms |
| axum    | Rust     |   474 |   2,771 |  1.3s |       269ms |
| cargo   | Rust     | 2,718 |  12,009 | 17.4s |       783ms |
| django  | Python   | 6,907 |  42,917 | 37.9s |     2,125ms |
| ruff    | Rust     | 9,702 |  42,239 | 67.2s |     2,300ms |
| go      | Go       |14,022 | 105,497 |186.8s |     4,661ms |

See [BENCHMARKS.md](BENCHMARKS.md) for full results across all 21 tools.

## License

MIT
