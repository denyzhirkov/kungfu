//! Config-file symbols share result lists with code under a penalty applied
//! before every cut, lifted by a config-flavoured query or an infrastructure
//! repo; every penalty and share-based lift is declared, and an outline of a
//! capped or broken config file says it is partial.

use kungfu_core::KungfuService;
use kungfu_types::budget::Budget;
use kungfu_types::context::ConfigRankingStatus;
use std::path::{Path, PathBuf};

fn project(tag: &str, files: &[(&str, String)]) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "kungfu_config_ranking_{tag}_{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&tmp).ok();
    for (path, body) in files {
        let path = tmp.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    kungfu_project::init_project(&tmp).unwrap();
    KungfuService::open(&tmp).unwrap().index_full().unwrap();
    tmp
}

fn open(tmp: &Path) -> KungfuService {
    KungfuService::open(tmp).unwrap()
}

/// src/net.rs + two other code files + a package.json over the default cap
/// (1 name + 1 retry key + dependencies + 60 entries = 63 config symbols).
fn app_with_package_json(tag: &str) -> PathBuf {
    let deps: Vec<String> = (0..60)
        .map(|i| format!("    \"dep-{i:02}\": \"^1.0.{i}\""))
        .collect();
    project(
        tag,
        &[
            (
                "src/net.rs",
                "pub fn retry_timeout_ms() -> u64 { 30 }\n".into(),
            ),
            ("src/a.rs", "pub fn alpha() {}\n".into()),
            ("src/b.rs", "pub fn beta() {}\n".into()),
            (
                "package.json",
                format!(
                    "{{\n  \"name\": \"app\",\n  \"retry_timeout_ms\": 5000,\n  \"dependencies\": {{\n{}\n  }}\n}}\n",
                    deps.join(",\n")
                ),
            ),
        ],
    )
}

/// The reviewer's repro: 15 code files, one of them the real answer, and
/// locale catalogues whose keys out-match it on raw score.
fn app_with_locales(tag: &str, locales: usize) -> PathBuf {
    let mut files: Vec<(String, String)> = vec![(
        "src/auth.rs".into(),
        "pub fn process_user_login(u: &str) -> bool { !u.is_empty() }\n".into(),
    )];
    files.extend((1..15).map(|i| (format!("src/f{i}.rs"), format!("pub fn f{i}() {{}}\n"))));
    files.extend((0..locales).map(|i| {
        (
            format!("locales/l{i:02}.json"),
            // Distinct content per file: identical files share a content-hash id.
            format!("{{\n  \"user_login\": \"{i}\",\n  \"user_login_title\": \"b\",\n  \"user_login_button\": \"c\"\n}}\n"),
        )
    }));
    let files: Vec<(&str, String)> = files.iter().map(|(p, b)| (p.as_str(), b.clone())).collect();
    project(tag, &files)
}

fn position(paths: &[&str], path: &str) -> usize {
    paths
        .iter()
        .position(|p| *p == path)
        .unwrap_or_else(|| panic!("{path} missing from {paths:?}"))
}

#[test]
fn neutral_query_ranks_code_above_config_and_declares_it() {
    let tmp = app_with_package_json("neutral");
    let service = open(&tmp);

    let ranked = service
        .find_symbol_ranked("retry timeout", Budget::Small, None)
        .unwrap();
    let paths: Vec<&str> = ranked
        .results
        .iter()
        .map(|r| r.item.path.as_str())
        .collect();
    assert!(position(&paths, "src/net.rs") < position(&paths, "package.json"));
    let note = ranked.config_ranking.expect("demotion must be declared");
    assert_eq!(note.status, ConfigRankingStatus::Penalized);
    assert_eq!(note.factor, kungfu_search::CONFIG_PENALTY);

    let files = service
        .find_files("retry_timeout_ms", Budget::Small, None)
        .unwrap();
    let paths: Vec<&str> = files.results.iter().map(|h| h.item.path.as_str()).collect();
    assert!(position(&paths, "src/net.rs") < position(&paths, "package.json"));
    assert!(files.config_ranking.is_some());

    let packet = service.ask_context("retry timeout", Budget::Small).unwrap();
    let paths: Vec<&str> = packet.items.iter().map(|i| i.path.as_str()).collect();
    assert!(position(&paths, "src/net.rs") < position(&paths, "package.json"));
    let config_item = packet
        .items
        .iter()
        .find(|i| i.path == "package.json")
        .unwrap();
    assert!(
        config_item.why.contains("config penalty"),
        "{}",
        config_item.why
    );
    let retrieval = packet.retrieval.unwrap();
    assert_eq!(
        retrieval.config_ranking.unwrap().status,
        ConfigRankingStatus::Penalized
    );

    // investigate carries the declaration through packet.retrieval.
    let inv = service.investigate("retry timeout", Budget::Small).unwrap();
    assert_eq!(inv["retrieval"]["config_ranking"]["status"], "penalized");

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn config_query_lifts_the_penalty_without_a_note() {
    let tmp = app_with_package_json("config_query");
    let service = open(&tmp);

    let ranked = service
        .find_symbol_ranked("retry timeout package.json", Budget::Small, None)
        .unwrap();
    assert!(ranked.config_ranking.is_none());
    let score = |path: &str| {
        ranked
            .results
            .iter()
            .find(|r| r.item.path == path)
            .map(|r| r.score)
            .unwrap()
    };
    assert!(score("package.json") >= score("src/net.rs"));

    let packet = service
        .ask_context("retry timeout package.json", Budget::Small)
        .unwrap();
    assert!(packet.retrieval.unwrap().config_ranking.is_none());
    let paths: Vec<&str> = packet.items.iter().map(|i| i.path.as_str()).collect();
    assert!(position(&paths, "package.json") < position(&paths, "src/net.rs"));

    std::fs::remove_dir_all(&tmp).ok();
}

/// Review item 1: the penalty must apply before ask_context's per-strategy
/// top-k cut, or 13 locale files fill the pool and the code answer vanishes.
#[test]
fn ask_context_penalises_before_the_cut() {
    let tmp = app_with_locales("cut", 13);
    let service = open(&tmp);

    let symbols = service
        .find_symbol_ranked("user login", Budget::Small, None)
        .unwrap();
    assert_eq!(symbols.results[0].item.path, "src/auth.rs");

    let packet = service.ask_context("user login", Budget::Full).unwrap();
    let paths: Vec<&str> = packet.items.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(paths[0], "src/auth.rs", "{paths:?}");
    assert!(packet.retrieval.unwrap().config_ranking.is_some());

    std::fs::remove_dir_all(&tmp).ok();
}

/// Review item 3: locale JSON is data, not infrastructure — 16 catalogues
/// against 15 source files keep the app penalised (and declared).
#[test]
fn locale_heavy_app_stays_penalized() {
    let tmp = app_with_locales("locales", 16);
    let ranked = open(&tmp)
        .find_symbol_ranked("user login", Budget::Small, None)
        .unwrap();
    assert_eq!(ranked.results[0].item.path, "src/auth.rs");
    assert_eq!(
        ranked.config_ranking.unwrap().status,
        ConfigRankingStatus::Penalized
    );
    std::fs::remove_dir_all(&tmp).ok();
}

/// Review item 2: only one same-name config entry escapes the penalty, so
/// code matches survive and the demotion is declared.
#[test]
fn only_one_exact_config_match_is_spared() {
    let mut files: Vec<(String, String)> = vec![(
        "src/ver.rs".into(),
        "pub fn version_string() -> String { String::new() }\npub fn get_version() -> u32 { 1 }\n"
            .into(),
    )];
    files.extend((1..7).map(|i| (format!("src/f{i}.rs"), format!("pub fn f{i}() {{}}\n"))));
    files.extend(["a", "b", "c", "d", "e"].iter().map(|p| {
        (
            format!("packages/{p}/package.json"),
            format!("{{\n  \"name\": \"{p}\",\n  \"version\": \"1.0.0\"\n}}\n"),
        )
    }));
    let files: Vec<(&str, String)> = files.iter().map(|(p, b)| (p.as_str(), b.clone())).collect();
    let tmp = project("spare_one", &files);

    let ranked = open(&tmp)
        .find_symbol_ranked("version", Budget::Small, None)
        .unwrap();
    let rows: Vec<(&str, &str, f64)> = ranked
        .results
        .iter()
        .map(|r| (r.item.path.as_str(), r.item.name.as_str(), r.score))
        .collect();
    assert_eq!(rows[0].0, "packages/a/package.json", "{rows:?}");
    assert!(rows[0].2 > 1.0, "the spared one keeps its exact score");
    assert_eq!(rows[1].1, "version_string", "{rows:?}");
    assert!(rows.iter().any(|r| r.1 == "get_version"), "{rows:?}");
    let spared = rows
        .iter()
        .filter(|r| r.0.ends_with("package.json") && r.2 > 1.0);
    assert_eq!(spared.count(), 1, "{rows:?}");
    assert!(ranked.config_ranking.is_some());

    std::fs::remove_dir_all(&tmp).ok();
}

/// Review items 3c/10: an infrastructure repo ranks config equally and says so.
#[test]
fn terraform_repo_is_config_heavy_and_declares_it() {
    let mut files: Vec<(String, String)> = (0..10)
        .map(|i| {
            (
                format!("infra/m{i}.tf"),
                format!(
                    "resource \"aws_s3_bucket\" \"logs_{i}\" {{\n  bucket = \"logs-{i}\"\n}}\n"
                ),
            )
        })
        .collect();
    files.push((
        "scripts/sync.py".into(),
        "def sync_logs_bucket():\n    pass\n".into(),
    ));
    files.push(("scripts/util.py".into(), "def helper():\n    pass\n".into()));
    let files: Vec<(&str, String)> = files.iter().map(|(p, b)| (p.as_str(), b.clone())).collect();
    let tmp = project("terraform", &files);
    let service = open(&tmp);

    let ranked = service
        .find_symbol_ranked("logs bucket", Budget::Small, None)
        .unwrap();
    assert!(
        ranked.results.iter().any(|r| r.item.kind.is_config()),
        "config symbols must compete"
    );
    let note = ranked.config_ranking.expect("share lift must be declared");
    assert_eq!(note.status, ConfigRankingStatus::ConfigHeavy);
    assert_eq!(note.factor, 1.0);

    let packet = service.ask_context("logs bucket", Budget::Small).unwrap();
    assert_eq!(
        packet.retrieval.unwrap().config_ranking.unwrap().status,
        ConfigRankingStatus::ConfigHeavy
    );
    assert!(packet
        .items
        .iter()
        .all(|i| !i.why.contains("config penalty")));

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn semantic_keyword_path_penalises_and_declares() {
    let tmp = app_with_package_json("semantic");
    let out = open(&tmp)
        .semantic_search("retry timeout", Budget::Small)
        .unwrap();
    assert_eq!(out["mode"], "keyword_fallback");
    let results = out["results"].as_array().unwrap();
    let pos = |path: &str| {
        results
            .iter()
            .position(|r| r["path"] == path)
            .unwrap_or_else(|| panic!("{path} missing: {results:?}"))
    };
    assert!(pos("src/net.rs") < pos("package.json"));
    assert_eq!(out["config_ranking"]["status"], "penalized");
    assert!(out.get("config_penalty").is_none());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn name_lookups_are_not_penalised() {
    // The key exists in config and in code; explore/edit resolve by name, so
    // the config entry keeps its exact-match score instead of 0.6.
    let tmp = app_with_package_json("lookup");
    let service = open(&tmp);
    let explored = service
        .explore_symbol("retry_timeout_ms", Budget::Small)
        .unwrap();
    let other = &explored["other_matches"][0];
    assert_eq!(other["path"], "package.json", "{explored}");
    assert!(other["score"].as_f64().unwrap() >= 1.0, "{explored}");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn outline_of_capped_config_file_declares_truncation() {
    let tmp = app_with_package_json("outline");
    let service = open(&tmp);

    let outline = service.file_outline("package.json").unwrap();
    assert_eq!(outline.symbols.len(), 50);
    assert_eq!(outline.config_symbols_total, Some(63));
    assert!(!outline.config_parse_errors);
    let note = outline.note.as_deref().unwrap();
    assert!(note.contains("showing 50 of 63"), "{note}");
    assert!(note.contains("max_config_symbols_per_file"), "{note}");

    let json = service
        .file_outline_at("package.json", Budget::Tiny)
        .unwrap();
    assert_eq!(json["config_symbols_total"], 63);
    assert!(json["note"].as_str().unwrap().contains("showing 50 of 63"));

    // A complete outline keeps its shape: no limit fields at all.
    let code = service.file_outline_at("src/net.rs", Budget::Tiny).unwrap();
    assert!(code.get("note").is_none());
    assert!(code.get("config_symbols_total").is_none());
    assert!(code.get("config_parse_errors").is_none());

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn outline_of_broken_config_file_declares_parse_errors() {
    let tmp = project(
        "broken",
        &[
            ("src/a.rs", "pub fn a() {}\n".into()),
            (
                "broken.json",
                "{\n  \"a\": 1\n  \"b\": 2,\n  \"c\": {\n}\n".into(),
            ),
        ],
    );
    let service = open(&tmp);
    let outline = service.file_outline("broken.json").unwrap();
    assert!(outline.config_parse_errors);
    assert_eq!(outline.config_symbols_total, None);
    assert!(outline
        .note
        .as_deref()
        .unwrap()
        .contains("parsed with syntax errors"));
    let json = service
        .file_outline_at("broken.json", Budget::Tiny)
        .unwrap();
    assert_eq!(json["config_parse_errors"], true);
    std::fs::remove_dir_all(&tmp).ok();
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// The round-2 repro: 15 code files (one has the answer only in a comment)
/// and 16 locale catalogues whose keys match on name. With `dirty_locales`
/// the code is committed and only the catalogues are uncommitted.
fn app_with_locale_hints(tag: &str, dirty_locales: bool) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "kungfu_config_ranking_{tag}_{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("locales")).unwrap();
    std::fs::write(
        tmp.join("src/auth.rs"),
        "pub fn process_user_login(u: &str) -> bool { !u.is_empty() }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("src/session.rs"),
        "pub fn start_session(token: &str) -> bool {\n    // validates the user login token\n    !token.is_empty()\n}\n",
    )
    .unwrap();
    for i in 1..14 {
        std::fs::write(
            tmp.join(format!("src/f{i}.rs")),
            format!("pub fn f{i}() {{}}\n"),
        )
        .unwrap();
    }
    if dirty_locales {
        git(&tmp, &["init", "-q"]);
        git(&tmp, &["add", "src"]);
        git(&tmp, &["commit", "-q", "-m", "code"]);
    }
    for i in 0..16 {
        std::fs::write(
            tmp.join(format!("locales/l{i:02}.json")),
            // Distinct content per file: identical files share a content-hash id.
            format!(
                "{{\n  \"user_login\": \"{i}\",\n  \"user_login_title\": \"b\",\n  \"user_login_button\": \"c\",\n  \"signup_form_validates_email_hint\": \"d\"\n}}\n"
            ),
        )
        .unwrap();
    }
    kungfu_project::init_project(&tmp).unwrap();
    KungfuService::open(&tmp).unwrap().index_full().unwrap();
    tmp
}

/// Round 2, item 1: penalised config candidates must not count toward the
/// sparse-pool gates, or content grep (B2) never runs and a code answer that
/// lives only in a comment is lost.
#[test]
fn sparse_pool_gates_ignore_penalised_config_candidates() {
    let tmp = app_with_locale_hints("gates", false);
    let service = open(&tmp);
    for query in ["validates", "user login"] {
        let packet = service.ask_context(query, Budget::Full).unwrap();
        let session = packet.items.iter().find(|i| i.name == "start_session");
        assert!(
            session.is_some_and(|i| i.why.starts_with("content match")),
            "{query}: {:?}",
            packet
                .items
                .iter()
                .map(|i| (&i.path, &i.name, i.score))
                .collect::<Vec<_>>()
        );
    }
    std::fs::remove_dir_all(&tmp).ok();
}

/// Round 2, item 1 (bonuses): a penalised config candidate's additive bonuses
/// are scaled like its base score, so "recently changed" cannot undo the
/// penalty: the dirty key scores exactly the factor times what it would
/// score unpenalised.
#[test]
fn bonuses_of_penalised_config_candidates_are_scaled() {
    let tmp = app_with_locale_hints("dirty", true);
    let packet = open(&tmp).ask_context("validates", Budget::Full).unwrap();
    let key = packet
        .items
        .iter()
        .find(|i| i.name == "signup_form_validates_email_hint")
        .unwrap();
    assert!(key.why.contains("recently changed"), "{}", key.why);
    assert!(key.why.contains("config penalty"), "{}", key.why);
    // Substring match of "validates" in the 32-char key: 0.4 + 0.4 * 9/32.
    let raw = 0.4 + 0.4 * 9.0 / 32.0;
    let expected = kungfu_search::CONFIG_PENALTY * (raw + 0.3);
    assert!(
        (key.score - expected).abs() < 1e-9,
        "{} vs {expected}",
        key.score
    );
    let session = packet
        .items
        .iter()
        .find(|i| i.name == "start_session")
        .unwrap();
    assert!(!session.why.contains("recently changed"));
    std::fs::remove_dir_all(&tmp).ok();
}

/// Round 4: in a Terraform repo the resource block whose name matches must
/// not be pushed off the page by keys that match only through their value
/// text (an output's description, a reference) or by its own children.
#[test]
fn matching_resource_block_ranks_first() {
    let tmp = project(
        "security_group",
        &[
            (
                "main.tf",
                "resource \"aws_security_group\" \"web\" {\n  name   = \"web\"\n  vpc_id = var.vpc_id\n}\n"
                    .into(),
            ),
            (
                "outputs.tf",
                "output \"web_sg_id\" {\n  description = \"ID of Security Group web\"\n  value       = aws_security_group.web.id\n}\n"
                    .into(),
            ),
        ],
    );
    let ranked = open(&tmp)
        .find_symbol_ranked("security group", Budget::Small, None)
        .unwrap();
    let names: Vec<&str> = ranked
        .results
        .iter()
        .map(|r| r.item.name.as_str())
        .collect();
    assert_eq!(names[0], "aws_security_group.web", "{names:?}");
    std::fs::remove_dir_all(&tmp).ok();
}

fn score_of(
    ranked: &kungfu_core::Ranked<kungfu_search::SearchResult<kungfu_types::symbol::Symbol>>,
    path: &str,
) -> f64 {
    ranked
        .results
        .iter()
        .find(|r| r.item.path == path)
        .map(|r| r.score)
        .unwrap_or_else(|| panic!("{path} missing"))
}

/// Round 5 decision: config under test paths stays penalised when a config
/// query lifts the ranking — fixtures are not the project's configuration.
#[test]
fn query_lift_keeps_test_fixture_config_penalised() {
    let tmp = project(
        "fixture_query",
        &[
            ("src/a.rs", "pub fn alpha() {}\n".into()),
            ("src/b.rs", "pub fn beta() {}\n".into()),
            ("deploy/app.yaml", "log_level: info\n".into()),
            ("tests/fixtures/app.yaml", "log_level: debug\n".into()),
        ],
    );
    let ranked = open(&tmp)
        .find_symbol_ranked("log level yaml", Budget::Small, None)
        .unwrap();
    let real = score_of(&ranked, "deploy/app.yaml");
    let fixture = score_of(&ranked, "tests/fixtures/app.yaml");
    assert!(
        (fixture - real * kungfu_search::CONFIG_PENALTY).abs() < 1e-9,
        "{real} vs {fixture}"
    );
    let note = ranked
        .config_ranking
        .expect("the fixture penalty is declared");
    assert_eq!(note.status, ConfigRankingStatus::Penalized);
    assert!(note.hint.contains("test paths"), "{}", note.hint);
    std::fs::remove_dir_all(&tmp).ok();
}

/// Round 5 decision, config-heavy side: real config ranks on equal terms,
/// the fixture stays penalised, and the note says both.
#[test]
fn config_heavy_project_keeps_test_fixture_config_penalised() {
    let tmp = project(
        "fixture_heavy",
        &[
            ("k8s/web.yaml", "replicas: 3\nlog_level: info\n".into()),
            ("k8s/api.yaml", "replicas: 2\n".into()),
            ("k8s/worker.yaml", "replicas: 1\n".into()),
            ("scripts/deploy.py", "def deploy():\n    pass\n".into()),
            (
                "tests/fixtures/web.yaml",
                "replicas: 9\nlog_level: debug\n".into(),
            ),
        ],
    );
    let service = open(&tmp);
    let ranked = service
        .find_symbol_ranked("log level", Budget::Small, None)
        .unwrap();
    let real = score_of(&ranked, "k8s/web.yaml");
    let fixture = score_of(&ranked, "tests/fixtures/web.yaml");
    assert!(
        (fixture - real * kungfu_search::CONFIG_PENALTY).abs() < 1e-9,
        "{real} vs {fixture}"
    );
    let note = ranked.config_ranking.unwrap();
    assert_eq!(note.status, ConfigRankingStatus::ConfigHeavy);
    assert!(note.hint.contains("test paths"), "{}", note.hint);

    let packet = service.ask_context("log level", Budget::Small).unwrap();
    let why = |path: &str| {
        packet
            .items
            .iter()
            .find(|i| i.path == path)
            .map(|i| i.why.clone())
            .unwrap_or_else(|| panic!("{path} missing: {:?}", packet.items))
    };
    assert!(!why("k8s/web.yaml").contains("config penalty"));
    assert!(why("tests/fixtures/web.yaml").contains("config penalty"));
    std::fs::remove_dir_all(&tmp).ok();
}
