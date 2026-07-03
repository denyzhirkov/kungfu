//! Retrieval-honesty tests for empty callers/callees results: the diagnosis
//! must name WHY the result is empty (ambiguous target, frequency cutoff,
//! genuinely no edges) and degrade to plain `no_edges` when the
//! `call_graph_meta.json` sidecar is missing.

use kungfu_core::{EmptyCallGraphCause, KungfuService};
use kungfu_types::budget::Budget;

fn temp_project(tag: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!("kungfu_cg_diag_{tag}_{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    // One unambiguous cross-file edge that survives every filter, so the
    // project has a call graph and empty results are per-symbol, not global.
    std::fs::write(tmp.join("src/anchor_a.rs"), "pub fn anchor_callee() {}\n").unwrap();
    std::fs::write(
        tmp.join("src/anchor_b.rs"),
        "pub fn anchor_caller() { anchor_callee(); }\n",
    )
    .unwrap();
    kungfu_project::init_project(&tmp).unwrap();
    tmp
}

#[test]
fn ambiguous_target_names_definition_count() {
    let tmp = temp_project("ambiguous");
    // Two same-name definitions: the callee never resolves, no edges stored.
    std::fs::write(tmp.join("src/a.rs"), "pub fn dup_name() {}\n").unwrap();
    std::fs::write(tmp.join("src/b.rs"), "pub fn dup_name() { let _ = 2; }\n").unwrap();
    std::fs::write(tmp.join("src/user.rs"), "pub fn user() { dup_name(); }\n").unwrap();

    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();

    assert!(service
        .callers("dup_name", Budget::Small)
        .unwrap()
        .is_empty());
    assert_eq!(
        service.diagnose_empty_callers("dup_name").unwrap(),
        EmptyCallGraphCause::AmbiguousTarget { definitions: 2 },
        "multi-definition callee with no edges must be reported as ambiguous"
    );

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn frequency_filtered_is_detected_from_the_meta_shard() {
    let tmp = temp_project("frequency");
    std::fs::write(tmp.join("src/util.rs"), "pub fn busy_helper() {}\n").unwrap();
    for i in 0..2 {
        std::fs::write(
            tmp.join(format!("src/caller{i}.rs")),
            format!("pub fn run{i}() {{ busy_helper(); }}\n"),
        )
        .unwrap();
    }
    std::fs::write(
        tmp.join(".kungfu/config.toml"),
        "[call_graph]\nmax_caller_files = 1\n",
    )
    .unwrap();

    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();

    assert!(
        service
            .callers("busy_helper", Budget::Small)
            .unwrap()
            .is_empty(),
        "cutoff of 1 must drop the edges from 2 caller files"
    );
    assert_eq!(
        service.diagnose_empty_callers("busy_helper").unwrap(),
        EmptyCallGraphCause::FrequencyFiltered {
            max_caller_files: 1
        },
        "a recorded dropped callee must be reported as frequency-filtered"
    );

    // Missing sidecar (index written by an older binary): degrade to no_edges,
    // never guess.
    std::fs::remove_file(tmp.join(".kungfu/index/call_graph_meta.json")).unwrap();
    assert_eq!(
        service.diagnose_empty_callers("busy_helper").unwrap(),
        EmptyCallGraphCause::NoEdges,
        "without the meta shard the diagnosis must fall back to plain no_edges"
    );

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn unique_uncalled_symbol_is_plain_no_edges() {
    let tmp = temp_project("no_edges");
    std::fs::write(tmp.join("src/a.rs"), "pub fn lonely_fn() {}\n").unwrap();
    std::fs::write(tmp.join("src/b.rs"), "pub fn other() { let _ = 1; }\n").unwrap();

    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();

    assert!(service
        .callers("lonely_fn", Budget::Small)
        .unwrap()
        .is_empty());
    assert_eq!(
        service.diagnose_empty_callers("lonely_fn").unwrap(),
        EmptyCallGraphCause::NoEdges
    );
    // Unknown names keep the pre-existing behavior: plain no_edges.
    assert_eq!(
        service.diagnose_empty_callers("does_not_exist").unwrap(),
        EmptyCallGraphCause::NoEdges
    );

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn callees_of_ambiguous_source_stay_no_edges() {
    let tmp = temp_project("callees_dir");
    // Two same-name sources with no resolvable callees: the callers-side causes
    // (ambiguity of the NAME as a target) do not explain an empty outgoing set.
    std::fs::write(tmp.join("src/a.rs"), "pub fn dup_src() {}\n").unwrap();
    std::fs::write(tmp.join("src/b.rs"), "pub fn dup_src() { let _ = 2; }\n").unwrap();

    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();

    assert!(service
        .callees("dup_src", Budget::Small)
        .unwrap()
        .is_empty());
    assert_eq!(
        service.diagnose_empty_callees("dup_src").unwrap(),
        EmptyCallGraphCause::NoEdges,
        "ambiguous_target is a callers-direction cause and must not leak into callees"
    );

    std::fs::remove_dir_all(&tmp).ok();
}
