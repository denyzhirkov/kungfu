//! How config-file symbols (`ConfigKey` / `ConfigBlock`) rank against code.
//!
//! Config symbols share the result list with code but carry a penalty, so a
//! package.json with fifty keys cannot crowd functions out of an ordinary
//! search. The penalty is lifted when the query is about config, or when the
//! project (or the searched scope) is mostly infrastructure config — a
//! Terraform or Helm repo. Every penalty and every share-based lift is
//! declared in the response ([`ConfigRanking::note`]).

use kungfu_types::context::ConfigRankingNote;
use kungfu_types::file::{FileEntry, Language};
use kungfu_types::symbol::{Symbol, SymbolKind};
use kungfu_types::Budget;
use std::cmp::Ordering;
use std::path::Path;

/// Score multiplier for config symbols while the penalty applies. An exact
/// config-key match (1.0) lands at 0.6: below a code prefix match (0.9) and a
/// good substring match, above weak fuzzy noise. The only config factor:
/// every search path and ask_context use it.
pub const CONFIG_PENALTY: f64 = 0.6;

/// A project is config-heavy when its infrastructure-config files are MORE
/// than this share of (those + code files) — see [`share_side`] for which
/// files count. A starting value, not a measured optimum: the share is meant
/// to recognise infrastructure/config repositories, not apps with config.
pub const CONFIG_HEAVY_SHARE: f64 = 0.5;

enum ShareSide {
    Config,
    Code,
}

/// Which side of the config-heavy share a (non-test) file counts on.
/// - Config: infrastructure formats only — Yaml (k8s, Ansible, Helm,
///   compose), Hcl (Terraform), Env, Dockerfile.
/// - Code: the code languages.
/// - Neither: the tooling formats of code projects — Toml, Ini, Makefile (a
///   workspace of 30 one-file crates has 30 Cargo.toml) — and data-leaning
///   formats — Json, Csv, Xml (i18n catalogues, fixtures, manifests); also
///   Markdown and unknown files.
fn share_side(lang: Language) -> Option<ShareSide> {
    match lang {
        Language::Yaml | Language::Hcl | Language::Env | Language::Dockerfile => {
            Some(ShareSide::Config)
        }
        lang if lang.is_code() => Some(ShareSide::Code),
        _ => None,
    }
}

/// How config symbols rank for one request. Decided once per request
/// ([`ConfigRanking::decide`]) and threaded to every scoring path, so the
/// symbol, file and semantic searches of one request agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigRanking {
    /// Config symbols compete with a [`CONFIG_PENALTY`] handicap.
    Penalized,
    /// No penalty: the query names config vocabulary ([`query_wants_config`]).
    QueryWantsConfig,
    /// No penalty: infrastructure config dominates ([`CONFIG_HEAVY_SHARE`]).
    ConfigHeavy,
    /// No policy at all: a name lookup that must resolve the symbol the user
    /// named (explore_symbol, edit_context), not a ranked list.
    Raw,
}

impl ConfigRanking {
    /// `words` are the lowercased query words; `scope` (a path prefix) narrows
    /// the config-heavy share to the files being searched.
    pub fn decide(words: &[&str], files: &[FileEntry], scope: Option<&str>) -> Self {
        if query_wants_config(words) {
            Self::QueryWantsConfig
        } else if ConfigShare::of(files, scope).is_config_heavy() {
            Self::ConfigHeavy
        } else {
            Self::Penalized
        }
    }

    /// The per-candidate penalty decision — the one place it is made. Config
    /// is penalised unless the ranking is lifted; config in a file on a test
    /// path stays penalised even then (fixtures are not the project's
    /// configuration). Raw name lookups never penalise.
    pub fn penalizes(self, is_config: bool, path: &str) -> bool {
        is_config
            && match self {
                Self::Penalized => true,
                Self::QueryWantsConfig | Self::ConfigHeavy => {
                    kungfu_types::test_paths::is_test_path(path)
                }
                Self::Raw => false,
            }
    }

    /// Score multiplier for a candidate.
    pub fn factor(self, is_config: bool, path: &str) -> f64 {
        if self.penalizes(is_config, path) {
            CONFIG_PENALTY
        } else {
            1.0
        }
    }

    /// What the policy does to a candidate, for the response declaration:
    /// penalise it, rank it equally because of the project share, or nothing
    /// worth declaring (code, a query-driven lift, a raw lookup).
    pub fn act(self, is_config: bool, path: &str) -> Option<ConfigAct> {
        if self.penalizes(is_config, path) {
            Some(ConfigAct::Penalized)
        } else if is_config && self == Self::ConfigHeavy {
            Some(ConfigAct::Lifted)
        } else {
            None
        }
    }

    /// The declaration for a response, from what the policy did in it. A
    /// query-driven lift alone needs none: the caller asked for config.
    pub fn note(self, budget: Budget, acts: ConfigActs) -> Option<ConfigRankingNote> {
        match self {
            Self::Penalized if acts.penalized => {
                Some(ConfigRankingNote::penalized(CONFIG_PENALTY, budget))
            }
            Self::ConfigHeavy if acts.lifted => {
                Some(ConfigRankingNote::config_heavy(budget, acts.penalized))
            }
            Self::ConfigHeavy | Self::QueryWantsConfig if acts.penalized => Some(
                ConfigRankingNote::test_config_penalized(CONFIG_PENALTY, budget),
            ),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigAct {
    Penalized,
    Lifted,
}

/// What the config policy did across one response; drives its note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigActs {
    pub penalized: bool,
    pub lifted: bool,
}

impl ConfigActs {
    pub fn record(&mut self, act: Option<ConfigAct>) {
        match act {
            Some(ConfigAct::Penalized) => self.penalized = true,
            Some(ConfigAct::Lifted) => self.lifted = true,
            None => {}
        }
    }
}

/// Infrastructure-config vs counted file totals behind the config-heavy
/// decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigShare {
    pub config: usize,
    /// Config + code files counted by [`share_side`]. Test-path files and
    /// name-only entries (never parsed) are not counted.
    pub counted: usize,
}

impl ConfigShare {
    /// One pass over the already-loaded file list — no disk access.
    pub fn of(files: &[FileEntry], scope: Option<&str>) -> Self {
        let mut share = Self::default();
        for f in files {
            if scope.is_some_and(|s| !f.path.starts_with(s))
                || is_name_only(f)
                || kungfu_types::test_paths::is_test_path(&f.path)
            {
                continue;
            }
            match share_side(Language::from_path(Path::new(&f.path))) {
                Some(ShareSide::Config) => {
                    share.config += 1;
                    share.counted += 1;
                }
                Some(ShareSide::Code) => share.counted += 1,
                None => {}
            }
        }
        share
    }

    pub fn is_config_heavy(self) -> bool {
        self.counted > 0 && self.config as f64 > CONFIG_HEAVY_SHARE * self.counted as f64
    }
}

/// The indexer fingerprints files it records by name only as `skip:<size>`.
fn is_name_only(f: &FileEntry) -> bool {
    f.hash.starts_with("skip:")
}

/// Apply `ranking` to the config symbols in `pool`: multiply the scores of
/// those it penalises and return every entry it acted on, with the act.
///
/// One exact full-name config match is spared when no code symbol in the
/// pool has that exact name — the name exists only in config, so that entry
/// is the answer. Only one: further same-name config entries (five
/// package.json `version` keys) are penalised like any other. A config entry
/// outside test paths is preferred; one on a test path is spared only when
/// the pool holds nothing outside test paths. Among the candidates: best
/// score, then shortest path, then the first path in order.
pub fn apply_config_penalty<T, F>(
    ranking: ConfigRanking,
    query_lower: &str,
    pool: &mut [T],
    mut parts: F,
) -> Vec<(usize, ConfigAct)>
where
    F: FnMut(&mut T) -> (&Symbol, &mut f64),
{
    if ranking == ConfigRanking::Raw {
        return Vec::new();
    }
    let is_test = |s: &Symbol| kungfu_types::test_paths::is_test_path(&s.path);
    let exact = |s: &Symbol| s.name.to_lowercase() == query_lower;
    let mut code_exact = false;
    let mut non_test_exists = false;
    // (index, outside test paths, score, path)
    let mut spared: Option<(usize, bool, f64, String)> = None;
    for (i, t) in pool.iter_mut().enumerate() {
        let (s, score) = parts(t);
        non_test_exists |= !is_test(s);
        if !exact(s) {
            continue;
        }
        if !s.kind.is_config() {
            code_exact = true;
            continue;
        }
        let real = !is_test(s);
        let better = spared
            .as_ref()
            .is_none_or(|(_, best_real, best, best_path)| {
                real.cmp(best_real)
                    .then_with(|| score.total_cmp(best))
                    .then_with(|| best_path.len().cmp(&s.path.len()))
                    .then_with(|| best_path.as_str().cmp(&s.path))
                    == Ordering::Greater
            });
        if better {
            spared = Some((i, real, *score, s.path.clone()));
        }
    }
    let spared = spared
        .filter(|(_, real, _, _)| !code_exact && (*real || !non_test_exists))
        .map(|(i, _, _, _)| i);

    let mut acted = Vec::new();
    for (i, t) in pool.iter_mut().enumerate() {
        let (s, score) = parts(t);
        if *score <= 0.0 || Some(i) == spared {
            continue;
        }
        let Some(act) = ranking.act(s.kind.is_config(), &s.path) else {
            continue;
        };
        *score *= ranking.factor(true, &s.path);
        acted.push((i, act));
    }
    acted
}

/// Order between equal-scored symbols. Code keeps its existing relative order
/// (code-vs-code is never reordered) and goes before config; config symbols
/// are ordered deterministically: top-level before nested, blocks before
/// keys, then path, then line — so a matching resource block is not pushed
/// off the page by its own children in store order.
pub fn config_tie_break(a: &Symbol, b: &Symbol) -> Ordering {
    match (a.kind.is_config(), b.kind.is_config()) {
        (false, false) => Ordering::Equal,
        (false, true) => Ordering::Less,
        (true, false) => Ordering::Greater,
        (true, true) => {
            let block = |s: &Symbol| s.kind != SymbolKind::ConfigBlock;
            a.parent_symbol_id
                .is_some()
                .cmp(&b.parent_symbol_id.is_some())
                .then_with(|| block(a).cmp(&block(b)))
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.span.start_line.cmp(&b.span.start_line))
        }
    }
}

/// Check if the query suggests interest in config files. Lifting the config
/// penalty for a code-centric query lets config keys crowd the code back out,
/// so ordinary code vocabulary is deliberately left out: `json`, `xml`, `csv`
/// (serialization code), `package` (Go/Java packages), `setup` (test setup),
/// `make`, `variable`, `resource`, `compose` (Jetpack Compose, function
/// composition), `tf` (TensorFlow), `environment`, `cfg` (Rust attribute).
/// A plausible config file name (`package.json`, `main.tf`, `.env`) counts.
/// `words` must be lowercased; surrounding punctuation is ignored.
pub fn query_wants_config(words: &[&str]) -> bool {
    words
        .iter()
        .any(|w| is_config_word(trim_word(w)) || names_config_file(w))
}

/// Strip sentence punctuation around a word, keeping a leading `.` (`.env`).
fn trim_word(w: &str) -> &str {
    w.trim_start_matches(|c: char| !(c.is_alphanumeric() || c == '.'))
        .trim_end_matches(|c: char| !c.is_alphanumeric())
}

fn is_config_word(w: &str) -> bool {
    matches!(
        w,
        "config"
            | "configs"
            | "configuration"
            | "configurations"
            | "conf"
            | "settings"
            | "env"
            | "dotenv"
            | "yaml"
            | "yml"
            | "toml"
            | "ini"
            | "hcl"
            | "terraform"
            | "tfvars"
            | "docker"
            | "dockerfile"
            | "containerfile"
            | "docker-compose"
            | "makefile"
            | "cargo"
            | "npm"
            | "pyproject"
            | "tsconfig"
            | "kubernetes"
            | "k8s"
            | "helm"
            | "kustomize"
            | "ci"
    )
}

/// Config file names people type as-is (lowercased, as query words are).
const KNOWN_CONFIG_FILES: &[&str] = &[
    "package.json",
    "tsconfig.json",
    "composer.json",
    "cargo.toml",
    "pyproject.toml",
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
    "dockerfile",
    "makefile",
    "pom.xml",
    ".env",
];

/// A word naming a config file: a known config file name, or a name with a
/// config extension other than `.json` / `.xml` / `.csv` — those three collide
/// with member access in code (`response.json`). Call, index or assignment
/// syntax (`config.yaml()`, `x["a.toml"]`) is code, never a file name.
fn names_config_file(raw: &str) -> bool {
    if raw.contains(['(', ')', '[', ']', '{', '}', '=']) || raw.contains("::") {
        return false;
    }
    let w = trim_word(raw);
    let name = w.rsplit('/').next().unwrap_or(w);
    if KNOWN_CONFIG_FILES.contains(&name) {
        return true;
    }
    let lang = Language::from_path(Path::new(name));
    name.contains('.')
        && lang.is_config()
        && !matches!(lang, Language::Json | Language::Xml | Language::Csv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::symbol::{Span, SymbolKind};

    fn file(path: &str) -> FileEntry {
        FileEntry {
            id: format!("f:{path}"),
            path: path.to_string(),
            extension: None,
            language: None,
            size: 1,
            hash: "h".to_string(),
            indexed_at: Default::default(),
            tags: vec![],
            purpose: None,
            purpose_source: None,
            config_symbols_total: None,
            config_parse_errors: false,
        }
    }

    fn name_only(path: &str) -> FileEntry {
        FileEntry {
            hash: "skip:10".to_string(),
            ..file(path)
        }
    }

    fn sym(name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            id: format!("s:{name}:{kind}"),
            file_id: "f".to_string(),
            name: name.to_string(),
            kind,
            language: "x".to_string(),
            path: "p".to_string(),
            signature: None,
            span: Span {
                start_line: 1,
                end_line: 1,
                start_col: 0,
                end_col: 0,
            },
            parent_symbol_id: None,
            exported: true,
            visibility: None,
            doc_summary: None,
        }
    }

    fn words(q: &str) -> Vec<&str> {
        q.split_whitespace().collect()
    }

    fn sym_at(name: &str, kind: SymbolKind, path: &str) -> Symbol {
        Symbol {
            path: path.to_string(),
            id: format!("s:{path}:{name}"),
            ..sym(name, kind)
        }
    }

    fn share(config: usize, counted: usize) -> ConfigShare {
        ConfigShare { config, counted }
    }

    #[test]
    fn config_heavy_needs_more_than_half_of_counted_files() {
        let half = [file("main.tf"), file("src/lib.rs")];
        assert_eq!(ConfigShare::of(&half, None), share(1, 2));
        assert!(
            !ConfigShare::of(&half, None).is_config_heavy(),
            "50% is not >50%"
        );

        let majority = [file("main.tf"), file("vars.tf"), file("src/lib.rs")];
        assert!(ConfigShare::of(&majority, None).is_config_heavy());
        assert!(!ConfigShare::of(&[], None).is_config_heavy());
    }

    #[test]
    fn terraform_repo_is_config_heavy() {
        let mut files: Vec<FileEntry> = (0..10).map(|i| file(&format!("mod/m{i}.tf"))).collect();
        files.push(file("scripts/a.py"));
        files.push(file("scripts/b.py"));
        assert_eq!(ConfigShare::of(&files, None), share(10, 12));
        assert_eq!(
            ConfigRanking::decide(&words("bucket"), &files, None),
            ConfigRanking::ConfigHeavy
        );
    }

    #[test]
    fn yaml_infrastructure_repo_is_config_heavy() {
        let mut files: Vec<FileEntry> = (0..8)
            .map(|i| file(&format!("roles/r{i}/tasks/main.yml")))
            .collect();
        files.extend((0..4).map(|i| file(&format!("k8s/deploy{i}.yaml"))));
        files.push(file("Dockerfile"));
        files.push(file("library/filter.py"));
        files.push(file("library/module.py"));
        assert_eq!(ConfigShare::of(&files, None), share(13, 15));
        assert!(ConfigShare::of(&files, None).is_config_heavy());
    }

    #[test]
    fn rust_workspace_of_one_file_crates_is_not_config_heavy() {
        let mut files = vec![file("Cargo.toml"), file(".github/workflows/ci.yml")];
        for i in 0..30 {
            files.push(file(&format!("crates/c{i}/Cargo.toml")));
            files.push(file(&format!("crates/c{i}/src/lib.rs")));
        }
        files.push(file("rustfmt.toml"));
        files.push(file("Makefile"));
        files.push(file("setup.cfg"));
        assert_eq!(ConfigShare::of(&files, None), share(1, 31));
        assert!(!ConfigShare::of(&files, None).is_config_heavy());
    }

    #[test]
    fn locale_json_does_not_make_an_app_config_heavy() {
        // 16 locale catalogues vs 15 source files: data, not infrastructure.
        let mut files: Vec<FileEntry> = (0..16)
            .map(|i| file(&format!("locales/l{i}.json")))
            .collect();
        files.extend((0..15).map(|i| file(&format!("src/f{i}.rs"))));
        files.push(file("data/rows.csv"));
        files.push(file("pom.xml"));
        assert_eq!(ConfigShare::of(&files, None), share(0, 15));
        assert_eq!(
            ConfigRanking::decide(&words("user login"), &files, None),
            ConfigRanking::Penalized
        );
    }

    #[test]
    fn config_share_ignores_test_paths_unparseable_and_name_only_files() {
        let files = [
            file("src/lib.rs"),
            file("compose.yaml"),
            file("Cargo.toml"),
            file("README.md"),
            file("notes.txt"),
            file("nginx.conf"),
            name_only("package-lock.json"),
            name_only("big.yaml"),
            // Test fixtures are not the project, on either side.
            file("tests/fixtures/a.yaml"),
            file("tests/fixtures/b.yaml"),
            file("tests/smoke.rs"),
        ];
        assert_eq!(ConfigShare::of(&files, None), share(1, 2));
    }

    #[test]
    fn config_share_respects_scope() {
        let files = [
            file("src/a.rs"),
            file("src/b.rs"),
            file("src/c.rs"),
            file("infra/main.tf"),
            file("infra/vars.tf"),
        ];
        assert!(!ConfigShare::of(&files, None).is_config_heavy());
        assert!(ConfigShare::of(&files, Some("infra/")).is_config_heavy());
        assert_eq!(
            ConfigRanking::decide(&words("bucket"), &files, Some("infra/")),
            ConfigRanking::ConfigHeavy
        );
        assert_eq!(
            ConfigRanking::decide(&words("bucket"), &files, None),
            ConfigRanking::Penalized
        );
    }

    #[test]
    fn query_wants_config_for_formats_tools_and_file_names() {
        for q in [
            "terraform module",
            "docker compose services",
            "helm values",
            "k8s deployment",
            "yml workflow",
            "scripts in package.json",
            "scripts in package.json?",
            "variables in main.tf",
            "infra/main.tf variables",
            "what is in .env.local",
            "pom.xml dependencies",
            "env vars",
            "ci matrix",
            "max_config_symbols_per_file config",
            "where is the config,",
            "(config)",
            "config?",
        ] {
            assert!(query_wants_config(&words(q)), "{q}");
        }
    }

    #[test]
    fn query_wants_config_ignores_code_vocabulary() {
        for q in [
            "parse json response",
            "xml parser",
            "export csv",
            "resource handle cleanup",
            "variable naming",
            "make target",
            "package manager resolve",
            "setup test fixture",
            "tf model training",
            "compose views",
            "environment detection",
            "cfg attribute",
            "max_config_symbols_per_file",
            "config.rs",
            "await response.json()",
            "response.json",
            "export data.csv",
            "parse feed.xml",
            "load data.json",
            "self.config.yaml()",
            "settings[\"x.yaml\"]",
        ] {
            assert!(!query_wants_config(&words(q)), "{q}");
        }
    }

    #[test]
    fn decide_prefers_query_signal_then_project_share() {
        let code = [file("src/a.rs"), file("compose.yaml"), file("src/b.rs")];
        let infra = [file("main.tf"), file("vars.tf"), file("src/a.rs")];
        assert_eq!(
            ConfigRanking::decide(&words("budget"), &code, None),
            ConfigRanking::Penalized
        );
        assert_eq!(
            ConfigRanking::decide(&words("cargo toml"), &code, None),
            ConfigRanking::QueryWantsConfig
        );
        assert_eq!(
            ConfigRanking::decide(&words("budget"), &infra, None),
            ConfigRanking::ConfigHeavy
        );
    }

    #[test]
    fn notes_say_what_the_policy_did() {
        use kungfu_types::context::ConfigRankingStatus as Status;
        let pen = ConfigActs {
            penalized: true,
            lifted: false,
        };
        let lift = ConfigActs {
            penalized: false,
            lifted: true,
        };
        let both = ConfigActs {
            penalized: true,
            lifted: true,
        };
        let note = |r: ConfigRanking, b: Budget, a: ConfigActs| r.note(b, a);

        let n = note(ConfigRanking::Penalized, Budget::Small, pen).unwrap();
        assert_eq!((n.status, n.factor), (Status::Penalized, CONFIG_PENALTY));
        let tiny = note(ConfigRanking::Penalized, Budget::Tiny, pen).unwrap();
        assert!(tiny.hint.len() < n.hint.len());
        assert!(note(
            ConfigRanking::Penalized,
            Budget::Small,
            ConfigActs::default()
        )
        .is_none());

        let n = note(ConfigRanking::ConfigHeavy, Budget::Small, lift).unwrap();
        assert_eq!((n.status, n.factor), (Status::ConfigHeavy, 1.0));
        assert!(!n.hint.contains("test paths"));
        let n = note(ConfigRanking::ConfigHeavy, Budget::Small, both).unwrap();
        assert_eq!(n.status, Status::ConfigHeavy);
        assert!(n.hint.contains("test paths"), "{}", n.hint);

        // Lifted, but only fixtures were acted on: they were penalised.
        for lifted in [ConfigRanking::ConfigHeavy, ConfigRanking::QueryWantsConfig] {
            let n = note(lifted, Budget::Small, pen).unwrap();
            assert_eq!((n.status, n.factor), (Status::Penalized, CONFIG_PENALTY));
            assert!(n.hint.contains("test paths"), "{}", n.hint);
        }
        assert!(note(
            ConfigRanking::QueryWantsConfig,
            Budget::Small,
            ConfigActs::default()
        )
        .is_none());
        assert!(note(ConfigRanking::Raw, Budget::Small, both).is_none());
    }

    #[test]
    fn test_path_config_stays_penalised_when_the_ranking_is_lifted() {
        for lifted in [ConfigRanking::QueryWantsConfig, ConfigRanking::ConfigHeavy] {
            let mut pool = vec![
                (sym_at("env_flag", SymbolKind::Function, "src/env.rs"), 0.8),
                (
                    sym_at("env.mode", SymbolKind::ConfigKey, "config/app.yaml"),
                    0.8,
                ),
                (
                    sym_at("env.mode", SymbolKind::ConfigKey, "tests/fixtures/app.yaml"),
                    0.8,
                ),
            ];
            let acted = apply_config_penalty(lifted, "env", &mut pool, |e| (&e.0, &mut e.1));
            assert_eq!(pool[0].1, 0.8);
            assert_eq!(pool[1].1, 0.8, "{lifted:?}: real config ranks equally");
            assert!(
                (pool[2].1 - 0.8 * CONFIG_PENALTY).abs() < 1e-9,
                "{lifted:?}"
            );
            assert!(acted.contains(&(2, ConfigAct::Penalized)));
            assert_eq!(
                acted.contains(&(1, ConfigAct::Lifted)),
                lifted == ConfigRanking::ConfigHeavy
            );
        }
        // Raw name lookups never penalise, fixtures included.
        assert!(!ConfigRanking::Raw.penalizes(true, "tests/fixtures/app.yaml"));
    }

    #[test]
    fn exact_name_spare_prefers_real_config_over_fixtures() {
        // Same exact name in a fixture (better score) and in real config: the
        // real one is spared, the fixture penalised.
        let mut pool = vec![
            (
                sym_at("timeout", SymbolKind::ConfigKey, "tests/fixtures/a.yaml"),
                1.01,
            ),
            (
                sym_at("timeout", SymbolKind::ConfigKey, "deploy/app.yaml"),
                1.0,
            ),
        ];
        assert_eq!(
            penalize(ConfigRanking::Penalized, "timeout", &mut pool),
            [0]
        );
        assert_eq!(pool[1].1, 1.0);

        // Only the fixture has the name, but a non-test candidate exists: the
        // fixture is not rescued.
        let mut pool = vec![
            (
                sym_at("timeout", SymbolKind::ConfigKey, "tests/fixtures/a.yaml"),
                1.0,
            ),
            (
                sym_at("request_timeout", SymbolKind::Function, "src/net.rs"),
                0.7,
            ),
        ];
        assert_eq!(
            penalize(ConfigRanking::Penalized, "timeout", &mut pool),
            [0]
        );

        // Nothing outside test paths at all: the fixture may be spared.
        let mut pool = vec![
            (
                sym_at("timeout", SymbolKind::ConfigKey, "tests/fixtures/a.yaml"),
                1.0,
            ),
            (
                sym_at("other", SymbolKind::ConfigKey, "tests/fixtures/b.yaml"),
                0.5,
            ),
        ];
        assert_eq!(
            penalize(ConfigRanking::Penalized, "timeout", &mut pool),
            [1]
        );
    }

    fn penalize(ranking: ConfigRanking, query: &str, pool: &mut [(Symbol, f64)]) -> Vec<usize> {
        apply_config_penalty(ranking, query, pool, |e| (&e.0, &mut e.1))
            .into_iter()
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn penalty_demotes_config_only_when_penalized() {
        let pool = || {
            vec![
                (sym("timeout_ms", SymbolKind::Function), 0.8),
                (sym("timeout", SymbolKind::ConfigKey), 0.8),
            ]
        };
        let mut p = pool();
        assert_eq!(penalize(ConfigRanking::Penalized, "time", &mut p), [1]);
        assert!(p[0].1 > p[1].1, "code outranks the equal config hit");
        assert!((p[1].1 - 0.8 * CONFIG_PENALTY).abs() < 1e-9);

        // A share lift acts (and is declared) but keeps equal terms.
        let mut p = pool();
        assert_eq!(penalize(ConfigRanking::ConfigHeavy, "time", &mut p), [1]);
        assert_eq!(p[0].1, p[1].1);

        for untouched in [ConfigRanking::QueryWantsConfig, ConfigRanking::Raw] {
            let mut p = pool();
            assert!(penalize(untouched, "time", &mut p).is_empty());
            assert_eq!(p[0].1, p[1].1, "{untouched:?}: equal terms");
        }
    }

    #[test]
    fn exact_name_existing_only_in_config_is_spared() {
        // The key exists only in config: nothing in code competes for the name.
        let mut pool = vec![
            (sym("bucket_policy_doc", SymbolKind::Function), 0.7),
            (sym("bucket_policy", SymbolKind::ConfigBlock), 1.0),
        ];
        assert!(penalize(ConfigRanking::Penalized, "bucket_policy", &mut pool).is_empty());
        assert_eq!(pool[1].1, 1.0);

        // Same name in code and config: the code definition wins.
        let mut pool = vec![
            (sym("timeout", SymbolKind::Field), 1.0),
            (sym("timeout", SymbolKind::ConfigKey), 1.0),
        ];
        assert_eq!(
            penalize(ConfigRanking::Penalized, "timeout", &mut pool),
            [1]
        );
        assert!(pool[0].1 > pool[1].1);
    }

    #[test]
    fn only_one_same_name_config_entry_is_spared() {
        let mut pool = vec![
            (
                sym_at("version", SymbolKind::ConfigKey, "packages/b/package.json"),
                1.0,
            ),
            (
                sym_at("version_string", SymbolKind::Function, "src/ver.rs"),
                0.9,
            ),
            (
                sym_at("version", SymbolKind::ConfigKey, "packages/a/package.json"),
                1.0,
            ),
            (
                sym_at("version", SymbolKind::ConfigKey, "package.json"),
                1.0,
            ),
            (
                sym_at("version", SymbolKind::ConfigKey, "packages/c/package.json"),
                1.01,
            ),
        ];
        // Best score wins the exemption: c (1.01).
        assert_eq!(
            penalize(ConfigRanking::Penalized, "version", &mut pool),
            [0, 2, 3]
        );
        assert_eq!(pool[4].1, 1.01);

        // Equal scores: the shortest path, then the first path in order.
        let mut pool = vec![
            (
                sym_at("version", SymbolKind::ConfigKey, "packages/b/package.json"),
                1.0,
            ),
            (
                sym_at("version", SymbolKind::ConfigKey, "packages/a/package.json"),
                1.0,
            ),
        ];
        assert_eq!(
            penalize(ConfigRanking::Penalized, "version", &mut pool),
            [0]
        );
        let mut pool = vec![
            (
                sym_at("version", SymbolKind::ConfigKey, "packages/b/package.json"),
                1.0,
            ),
            (
                sym_at("version", SymbolKind::ConfigKey, "package.json"),
                1.0,
            ),
        ];
        assert_eq!(
            penalize(ConfigRanking::Penalized, "version", &mut pool),
            [0]
        );
    }
}
