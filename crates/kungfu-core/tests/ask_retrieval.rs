//! ask_context retrieval-honesty integration tests: the packet must declare
//! whether the vector layer ran, and the keyword-only path must work without
//! any embedding store present.

use kungfu_core::KungfuService;
use kungfu_types::budget::Budget;

fn temp_project(tag: &str) -> std::path::PathBuf {
    let tmp =
        std::env::temp_dir().join(format!("kungfu_ask_retrieval_{tag}_{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("src/lib.rs"),
        "/// Parse a budget string into a numeric limit.\n\
         pub fn parse_budget(input: &str) -> u32 { input.len() as u32 }\n\
         pub fn unrelated_helper() {}\n",
    )
    .unwrap();
    kungfu_project::init_project(&tmp).unwrap();
    tmp
}

#[test]
fn without_embeddings_packet_declares_keyword_only_and_still_answers() {
    let tmp = temp_project("no_embed");
    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();

    let packet = service
        .ask_context("parse budget input", Budget::Small)
        .unwrap();

    let retrieval = packet
        .retrieval
        .as_ref()
        .expect("ask_context must always declare its retrieval mode");
    assert_eq!(retrieval.mode, "keyword_only");
    assert_eq!(retrieval.vector_candidates, 0);
    let reason = retrieval
        .vector_skipped
        .as_deref()
        .expect("keyword_only must say why the vector layer did not run");
    assert!(
        reason.contains("embeddings build"),
        "skip reason should name the enabling action, got: {reason}"
    );

    // Keyword path itself is unchanged: the name match is still found.
    assert!(
        packet.items.iter().any(|i| i.name == "parse_budget"),
        "keyword retrieval must still find parse_budget"
    );

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn single_keyword_query_declares_vector_skip() {
    let tmp = temp_project("single_kw");
    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();

    let packet = service.ask_context("parse_budget", Budget::Small).unwrap();

    let retrieval = packet.retrieval.as_ref().expect("retrieval mode missing");
    assert_eq!(retrieval.mode, "keyword_only");
    let reason = retrieval.vector_skipped.as_deref().unwrap_or_default();
    assert!(
        reason.contains("single-keyword"),
        "short queries must declare the skip, got: {reason}"
    );

    std::fs::remove_dir_all(&tmp).ok();
}
