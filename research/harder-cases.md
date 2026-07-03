# Harder benchmark cases (v2, cases 021-050)

Expansion of the quality benchmark from 20 to 50 cases, targeting failure modes
where kungfu currently scores poorly, so the next quality work (query expansion,
MMR packet assembly, vector candidates) has measurable headroom.

## Baselines

| Metric | old baseline (20 cases, recorded) | old 20 cases re-run | v2 (50 cases) |
|---|---|---|---|
| Aggregate | 69.48 (`baseline-score.json`) | 66.29 | 48.31 |
| Min case | 62.59 | 18.81 | 2.46 |
| Cases passing must_include | 20/20 | 19/20 | 33/50 |

The recorded 69.48 baseline is **not reproducible** on the current binary: it was
produced by an older kungfu build against an older self-index. The comparable
number going forward is the re-run column (binary v2.5.24, fixed harness).
`baseline-score.json` is kept untouched for the record; `baseline-score-v2.json`
is the new reference.

## Harness bugs found and fixed (scripts/eval.sh)

1. **`search-text` cases silently scored on empty output.** The CLI subcommand was
   renamed `search-text` -> `search`; eval.sh still invoked `search-text`, the error
   went to `/dev/null`, and the case scored against an empty string. Fixed by mapping
   the schema command `search-text` to `kungfu search`. (case-004 was affected.)
2. **`context` command mapping was dead** for the same reason (`context` subcommand
   no longer exists); mapped to `ask-context`. No case currently uses it.

## Ground-truth repair in old cases

- **case-004**: `must_include` pointed at `kungfu-core/src/lib.rs`, but `detect_intent`
  moved to `kungfu-core/src/helpers.rs` when lib.rs was split into modules. Repaired.
  Score is unchanged (retrieval finds neither file), so the old-20 aggregate is not
  perturbed by the repair. Self-targeting cases drift with the codebase — prefer the
  pinned external repos for new cases (done here: 27 of 30 new cases target pinned repos).

## Reproducibility

Target repos are plain git clones under `kungfu-bench/` (gitignored). Exact commits are
now pinned in `research/bench-repos.lock.json` (url + commit per repo). Clones must be
clean except the untracked `.kungfu/` index dir.

## Failure-mode categories

- **concept-recall** — natural-language concept query whose answer uses different
  vocabulary than the query (e.g. "multiplex network io" -> `ae.c`).
- **identifier-mismatch** — query words in space/snake form vs camelCase or abbreviated
  symbols (`web socket session` -> `WebSocketSession`, `ws` -> `WebSocket`).
- **over-selection** — feature-understanding query with a tight `max_files` limit;
  penalizes shotgun file selection.
- **cross-file** — correct answer requires caller+callee files together (multiple
  `must_include` entries; partial credit via must ratio).

## Per-category scores (all 50 cases)

| Category | Cases | Avg score |
|---|---|---|
| concept-recall | 11 | 27.74 |
| cross-file | 5 | 33.48 |
| debug | 3 | 53.31 |
| feature-understanding | 6 | 67.31 |
| file-discovery | 6 | 68.24 |
| identifier-mismatch | 8 | 53.58 |
| orientation | 2 | 68.63 |
| over-selection | 6 | 31.46 |
| refactor-prep | 3 | 71.74 |

## New cases (021-050) with current scores

| Case | Category | Project | Query | Score |
|---|---|---|---|---|
| case-021 | concept-recall | redis | where is key expiration handled | 28.51 (FAIL) |
| case-022 | concept-recall | redis | what happens when the memory limit is reached and keys must be freed | 12.03 (FAIL) |
| case-023 | concept-recall | redis | how does the server multiplex network io events | 9.63 (FAIL) |
| case-024 | concept-recall | go | how does graceful shutdown of the http server work | 75.66 (PASS) |
| case-025 | concept-recall | django | where are user passwords hashed and verified | 58.3 (PASS) |
| case-026 | concept-recall | django | how are pending database migrations applied to the database | 4.92 (FAIL) |
| case-027 | concept-recall | flask | how does an incoming request get dispatched to the matching view function | 19.66 (FAIL) |
| case-028 | concept-recall | react | how does react decide which pending update to process first | 13.03 (FAIL) |
| case-029 | concept-recall | cargo | how does cargo choose which version of a dependency to use | 9.15 (FAIL) |
| case-030 | concept-recall | deno | where are file system permission checks enforced | 57.56 (PASS) |
| case-031 | concept-recall | home-assistant | where are automation triggers attached and evaluated | 16.64 (FAIL) |
| case-032 | identifier-mismatch | ktor | web socket session | 64.48 (PASS) |
| case-033 | identifier-mismatch | react | use effect | 16.43 (FAIL) |
| case-034 | identifier-mismatch | next.js | get server side props | 67.11 (PASS) |
| case-035 | identifier-mismatch | aspnetcore | ws connection middleware | 2.46 (FAIL) |
| case-036 | identifier-mismatch | django | get or create | 67.61 (PASS) |
| case-037 | identifier-mismatch | gin | serve static files from a directory | 75.38 (PASS) |
| case-038 | identifier-mismatch | spring-boot | rest template builder | 64.51 (PASS) |
| case-039 | identifier-mismatch | express | how do application settings get enabled and disabled | 70.7 (PASS) |
| case-040 | over-selection | spring-boot | how does auto configuration decide which beans to enable | 14.34 (FAIL) |
| case-041 | over-selection | aspnetcore | how does the kestrel server accept incoming connections | 3.81 (FAIL) |
| case-042 | over-selection | django | how does a request pass through the middleware chain to a view | 5.73 (FAIL) |
| case-043 | over-selection | langchain | how do retrievers fetch relevant documents for a query | 52.2 (PASS) |
| case-044 | over-selection | ktor | how are response bodies serialized through content negotiation | 45 (PASS) |
| case-045 | over-selection | kungfu | how are context packets assembled and trimmed to fit the budget | 67.68 (PASS) |
| case-046 | cross-file | kungfu | how does an ask-context request flow from the cli command into the ranked context packet | 19.14 (FAIL) |
| case-047 | cross-file | redis | how does a client command travel from socket read to the command handler | 12.12 (FAIL) |
| case-048 | cross-file | flask | how does url_for resolve an endpoint name into a url | 69.44 (PASS) |
| case-049 | cross-file | axum | how does the router match a request path and invoke the handler | 7.75 (FAIL) |
| case-050 | cross-file | gin | how does calling next in middleware continue the handler chain | 58.95 (PASS) |

27 of 30 new cases score below 70 today — the headroom this expansion exists to provide.

## Ground-truth verification method

Every `must_include` was verified against the pinned checkout before the case was
accepted: the canonical file was located with `find`/`grep` and the defining symbol
confirmed in it (e.g. `activeExpireCycle` in `redis/src/expire.c`, `MigrationExecutor`
in `django/db/migrations/executor.py`, `func (s *Server) Shutdown` in
`go/src/net/http/server.go`, `useEffect` export in `react/packages/react/src/ReactHooks.js`,
`url_for` in both `flask/src/flask/helpers.py` and `app.py`). Failing cases were then
spot-checked by running the query manually and confirming the output contains only
vocabulary-adjacent noise (test files, unrelated defines) while the canonical file is
absent — i.e. the miss is a retrieval failure, not a wrong expectation.

