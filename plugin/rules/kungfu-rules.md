## kungfu — context retrieval (use BEFORE Read / grep / find)

Default to kungfu; raw file reads are the fallback, not the first move — it returns
ranked, scoped packets instead of whole files. Start every task with
`ask_context("<task>", budget: "tiny")` and escalate the budget only if the packet
is clearly insufficient. Open a raw file only once kungfu points you at it.

Route by situation:

| Situation | First call |
|---|---|
| New task / "figure out X" | `ask_context` (or `investigate`) |
| Where is a named symbol defined/used? | `find_symbol` → `explore_symbol`, then `callers` / `callees` |
| Concept with no known name ("where does rate limiting live") | `semantic_search` |
| Understand a file > 50 lines | `file_outline` / `explore_file`, then a targeted Read of that range |
| About to edit a symbol | `edit_context` (full verbatim body + contracts — no follow-up Read) |
| "Why is it like this / what changed?" | `file_history` / `symbol_history` / `change_timeline` |
| Refactor touching > 1 file | `affected` + `coupling` + `smart_test` before editing |
| Bug with no clear file | `hotspots`, then `debug_trace` on the stack trace |

After edits: `reindex` the changed paths, then `verify_change` for the blast radius
and minimal test set. `memory_search` before implementing (there may already be a
decision or warning); `memory_add` to persist new ones — pin sparingly.

Skip kungfu only for: a one-line edit in a file already open this session; a file
< 50 lines whose exact path you know; pure shell ops; reading a config/lock file by
exact path. Otherwise, if you reach for Read / grep / find — stop and route above.
