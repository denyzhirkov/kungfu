//! A project initialised inside a larger git repository must still be a git
//! project, scoped to its own files with project-relative paths.

use kungfu_core::KungfuService;
use kungfu_types::budget::Budget;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

fn nested_project(tag: &str, commit: bool) -> (PathBuf, PathBuf, KungfuService) {
    let repo = std::env::temp_dir().join(format!("kungfu_nested_{tag}_{}", std::process::id()));
    std::fs::remove_dir_all(&repo).ok();
    let sub = repo.join("proj");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("keep.rs"), "pub fn keep_widget() {}\n").unwrap();
    std::fs::write(repo.join("outside.rs"), "pub fn outside_fn() {}\n").unwrap();
    git(&repo, &["init", "-q"]);
    if commit {
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
    }
    kungfu_project::init_project(&sub).unwrap();
    let svc = KungfuService::open(&sub).unwrap();
    svc.index_full().unwrap();
    (repo, sub, svc)
}

#[test]
fn verify_change_and_ask_context_work_in_nested_root() {
    let (repo, sub, svc) = nested_project("commits", true);
    std::fs::write(
        sub.join("keep.rs"),
        "pub fn keep_widget() { let _x = 1; }\n",
    )
    .unwrap();
    std::fs::write(sub.join("fresh.rs"), "pub fn fresh_widget() {}\n").unwrap();
    std::fs::write(
        repo.join("outside.rs"),
        "pub fn outside_fn() { let _y = 1; }\n",
    )
    .unwrap();
    std::fs::write(repo.join("outside_new.rs"), "pub fn o2() {}\n").unwrap();

    let out = svc.verify_change(1).unwrap();
    assert_eq!(out["status"], "changes");
    let changed: Vec<&str> = out["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(changed, ["fresh.rs", "keep.rs"]);

    svc.index_full().unwrap();
    let packet = svc.ask_context("keep_widget", Budget::Small).unwrap();
    assert_eq!(packet.changed_files, ["fresh.rs", "keep.rs"]);
    assert!(packet
        .items
        .iter()
        .any(|i| i.path == "keep.rs" && i.why.contains("recently changed")));
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn nested_root_in_repo_without_commits_works() {
    let (repo, sub, svc) = nested_project("nohead", false);
    std::fs::write(sub.join("fresh.rs"), "pub fn fresh_widget() {}\n").unwrap();

    let out = svc.verify_change(1).unwrap();
    assert_eq!(out["status"], "changes");
    let packet = svc.ask_context("keep_widget", Budget::Small).unwrap();
    assert!(packet.changed_files.contains(&"fresh.rs".to_string()));
    assert!(packet.changed_files.iter().all(|f| !f.contains("outside")));
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn untracked_project_caps_changed_files_and_skips_bonus() {
    let (repo, sub, _svc) = nested_project("untracked", true);
    for i in 0..60 {
        std::fs::write(
            sub.join(format!("m{i}.rs")),
            format!("pub fn gen_widget_{i}() {{}}\n"),
        )
        .unwrap();
    }
    let svc = KungfuService::open(&sub).unwrap();
    svc.index_full().unwrap();

    let packet = svc.ask_context("gen_widget", Budget::Small).unwrap();
    assert_eq!(packet.changed_files.len(), 50);
    assert!(packet.changed_files_note.is_some());
    assert!(packet
        .items
        .iter()
        .all(|i| !i.why.contains("recently changed")));

    let inv = svc.investigate("gen_widget", Budget::Small).unwrap();
    assert!(inv["diff"]["changed_files_note"].is_string());
    assert!(inv["diff"]["total_changed_files"].as_u64().unwrap() > 50);
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn small_change_set_has_no_note_and_exact_path_match() {
    let (repo, sub, svc) = nested_project("exact", true);
    std::fs::write(sub.join("eep.rs"), "pub fn keep_widget_twin() {}\n").unwrap();
    svc.index_full().unwrap();
    let packet = svc.ask_context("keep_widget", Budget::Small).unwrap();
    assert!(packet.changed_files_note.is_none());
    // `eep.rs` is a suffix of `keep.rs`; only the exact file may be marked.
    let keep: Vec<_> = packet
        .items
        .iter()
        .filter(|i| i.path == "keep.rs")
        .collect();
    assert!(
        !keep.is_empty(),
        "keep.rs must be retrieved for this check to mean anything"
    );
    assert!(keep.iter().all(|i| !i.why.contains("recently changed")));
    std::fs::remove_dir_all(&repo).ok();
}
