//! Agent annotation flow: annotate_file merges into the index with honest
//! provenance (doc wins, agent fills gaps, stale annotations are marked),
//! survives reindexes via the sidecar, and annotation_queue only offers
//! files that genuinely lack a purpose.

use kungfu_core::KungfuService;
use std::collections::BTreeMap;

fn temp_project(tag: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!("kungfu_ann_{tag}_{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    // One file with an authored module doc, one without.
    std::fs::write(
        tmp.join("src/documented.rs"),
        "//! Authored purpose line.\n\npub fn a() {}\n",
    )
    .unwrap();
    std::fs::write(tmp.join("src/bare.rs"), "pub fn b() {}\n").unwrap();
    kungfu_project::init_project(&tmp).unwrap();
    tmp
}

fn purpose_of(service: &KungfuService, path: &str) -> (Option<String>, Option<String>) {
    let outline = service.file_outline(path).unwrap();
    (outline.purpose, outline.purpose_source)
}

#[test]
fn annotation_fills_gap_and_doc_wins() {
    let tmp = temp_project("fill");
    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();

    // Doc-sourced purpose is present from indexing.
    let (p, src) = purpose_of(&service, "src/documented.rs");
    assert_eq!(p.as_deref(), Some("Authored purpose line."));
    assert_eq!(src.as_deref(), Some("doc"));

    // Annotating the bare file applies immediately.
    let result = service
        .annotate_file(
            "src/bare.rs",
            "Agent-described helper.",
            BTreeMap::from([("helper".to_string(), "a thing".to_string())]),
        )
        .unwrap();
    assert_eq!(result.status, "applied");
    let (p, src) = purpose_of(&service, "src/bare.rs");
    assert_eq!(p.as_deref(), Some("Agent-described helper."));
    assert_eq!(src.as_deref(), Some("agent"));

    // Annotating the documented file stores but does not displace the doc.
    let result = service
        .annotate_file(
            "src/documented.rs",
            "Competing description.",
            BTreeMap::new(),
        )
        .unwrap();
    assert_eq!(result.status, "stored_doc_wins");
    let (p, src) = purpose_of(&service, "src/documented.rs");
    assert_eq!(p.as_deref(), Some("Authored purpose line."));
    assert_eq!(src.as_deref(), Some("doc"));

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn annotation_survives_reindex_and_goes_stale_on_change() {
    let tmp = temp_project("stale");
    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();
    service
        .annotate_file("src/bare.rs", "Agent-described helper.", BTreeMap::new())
        .unwrap();

    // Full reindex rebuilds files.json from scratch — the sidecar re-applies.
    service.index_full().unwrap();
    let (p, src) = purpose_of(&service, "src/bare.rs");
    assert_eq!(p.as_deref(), Some("Agent-described helper."));
    assert_eq!(src.as_deref(), Some("agent"));

    // Change the file: the annotation stays but must be marked stale.
    std::fs::write(
        tmp.join("src/bare.rs"),
        "pub fn b() {}\npub fn extra() {}\n",
    )
    .unwrap();
    service.index_full().unwrap();
    let (p, src) = purpose_of(&service, "src/bare.rs");
    assert_eq!(p.as_deref(), Some("Agent-described helper."));
    assert_eq!(src.as_deref(), Some("agent-stale"));

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn queue_lists_only_files_without_purpose() {
    let tmp = temp_project("queue");
    // A test file must not be offered for annotation.
    std::fs::create_dir_all(tmp.join("tests")).unwrap();
    std::fs::write(tmp.join("tests/bare_test.rs"), "fn t() {}\n").unwrap();
    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();

    let queue = service.annotation_queue(10).unwrap();
    let paths: Vec<&str> = queue.items.iter().map(|i| i.path.as_str()).collect();
    assert!(paths.contains(&"src/bare.rs"), "got: {paths:?}");
    assert!(
        !paths.contains(&"src/documented.rs"),
        "doc-purposed file must not queue"
    );
    assert!(
        !paths.contains(&"tests/bare_test.rs"),
        "test files must not queue"
    );

    // Once annotated, the file leaves the queue.
    service
        .annotate_file("src/bare.rs", "Agent-described helper.", BTreeMap::new())
        .unwrap();
    let queue = service.annotation_queue(10).unwrap();
    assert!(!queue.items.iter().any(|i| i.path == "src/bare.rs"));

    std::fs::remove_dir_all(&tmp).ok();
}
