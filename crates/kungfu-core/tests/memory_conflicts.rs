//! ask_context surfaces only memory conflicts grounded in a shared related_symbol
//! that touch the returned project memory, capped by budget.

use kungfu_core::KungfuService;
use kungfu_types::budget::Budget;
use kungfu_types::memory::ProjectMemoryKind;

fn service(tag: &str) -> (KungfuService, std::path::PathBuf) {
    let tmp = std::env::temp_dir().join(format!("kungfu_mem_conf_{tag}_{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("src/lib.rs"),
        "pub fn parse_budget(input: &str) -> u32 { input.len() as u32 }\n",
    )
    .unwrap();
    kungfu_project::init_project(&tmp).unwrap();
    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();
    (service, tmp)
}

fn add(s: &KungfuService, content: &str, tags: &[&str], symbols: &[&str], pinned: bool) -> String {
    s.memory_add(
        ProjectMemoryKind::Decision,
        content,
        None,
        tags.iter().map(|t| t.to_string()).collect(),
        vec![],
        symbols.iter().map(|t| t.to_string()).collect(),
        pinned,
    )
    .unwrap()
    .id
}

#[test]
fn shared_tag_without_symbols_is_not_a_conflict() {
    let (s, tmp) = service("tag_only");
    add(&s, "use sqlite for storage", &["mcp"], &[], true);
    add(&s, "use json for storage", &["mcp"], &[], true);
    add(&s, "use yaml for storage", &["mcp"], &[], true);
    let packet = s.ask_context("parse budget input", Budget::Medium).unwrap();
    assert!(!packet.project_memory.is_empty());
    assert!(packet.memory_conflicts.is_empty());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn returned_entries_sharing_a_symbol_conflict_once() {
    let (s, tmp) = service("symbol");
    let a = add(&s, "use sqlite", &[], &["Store"], true);
    let b = add(&s, "use json", &[], &["Store"], true);
    let packet = s.ask_context("parse budget input", Budget::Medium).unwrap();
    assert_eq!(packet.memory_conflicts.len(), 1);
    let ids = &packet.memory_conflicts[0].entry_ids;
    assert!(ids.contains(&a) && ids.contains(&b));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn cluster_not_touching_returned_entries_is_hidden() {
    let (s, tmp) = service("untouched");
    add(&s, "pinned unrelated note", &[], &[], true);
    add(&s, "lorem ipsum alpha", &[], &["Zeta"], false);
    add(&s, "lorem ipsum beta", &[], &["Zeta"], false);
    let packet = s.ask_context("parse budget input", Budget::Medium).unwrap();
    assert!(packet.memory_conflicts.is_empty());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn conflict_cap_follows_budget_and_is_marked() {
    let (s, tmp) = service("cap");
    add(&s, "pinned anchor", &[], &["S1", "S2"], true);
    add(&s, "lorem one", &[], &["S1"], false);
    add(&s, "lorem two", &[], &["S2"], false);
    let packet = s.ask_context("parse budget input", Budget::Tiny).unwrap();
    assert_eq!(packet.memory_conflicts.len(), 1);
    assert!(!packet.memory_conflicts[0].entry_ids.is_empty());
    let note = packet.memory_conflicts_note.as_deref().unwrap();
    assert_eq!(
        note,
        "showing 1 of 2 conflict clusters — raise budget to see the rest"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn full_budget_note_does_not_suggest_raising_budget() {
    let (s, tmp) = service("full_note");
    add(
        &s,
        "pinned anchor",
        &[],
        &["S1", "S2", "S3", "S4", "S5", "S6"],
        true,
    );
    for i in 1..=6 {
        add(&s, &format!("lorem {i}"), &[], &[&format!("S{i}")], false);
    }
    let packet = s.ask_context("parse budget input", Budget::Full).unwrap();
    assert_eq!(packet.memory_conflicts.len(), 5);
    let note = packet.memory_conflicts_note.as_deref().unwrap();
    assert!(note.contains("showing 5 of 6") && !note.contains("raise budget"));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn popular_symbol_group_is_clipped_and_flagged() {
    let (s, tmp) = service("clip");
    add(&s, "pinned anchor", &[], &["Big"], true);
    for i in 0..25 {
        add(&s, &format!("lorem {i}"), &[], &["Big"], false);
    }
    let packet = s.ask_context("parse budget input", Budget::Medium).unwrap();
    assert_eq!(packet.memory_conflicts.len(), 1);
    assert!(packet.memory_conflicts[0].entry_ids.len() <= 20);
    let note = packet.memory_conflicts_note.as_deref().unwrap();
    assert!(note.contains("clipped to 20"));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn cap_keeps_most_relevant_cluster_not_alphabetical() {
    let (s, tmp) = service("relevance");
    // Strong query/symbol match -> ranks first among returned memory.
    let top = add(
        &s,
        "parse budget input decision",
        &[],
        &["parse_budget", "Zed"],
        true,
    );
    add(&s, "pinned filler a", &[], &["Alpha"], true);
    add(&s, "pinned filler b", &[], &["Beta"], true);
    add(&s, "lorem zed", &[], &["Zed"], false);
    add(&s, "lorem alpha", &[], &["Alpha"], false);
    add(&s, "lorem beta", &[], &["Beta"], false);
    let packet = s.ask_context("parse budget input", Budget::Small).unwrap();
    assert_eq!(packet.project_memory[0].id, top);
    assert_eq!(packet.memory_conflicts.len(), 2);
    assert_eq!(packet.memory_conflicts[0].on, "symbol:Zed");
    std::fs::remove_dir_all(&tmp).ok();
}
