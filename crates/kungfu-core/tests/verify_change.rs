//! verify_change must list every changed file: modified, deleted and untracked.

use kungfu_core::KungfuService;
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

fn temp_repo(tag: &str) -> (PathBuf, KungfuService) {
    let tmp = std::env::temp_dir().join(format!("kungfu_verify_{tag}_{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("keep.rs"), "pub fn keep_fn() {}\n").unwrap();
    std::fs::write(tmp.join("gone.rs"), "pub fn gone_fn() {}\n").unwrap();
    // Deliberately no .gitignore for `.kungfu/`: `init` does not add one, so
    // verify_change itself must not report kungfu's own state as changes.
    std::fs::create_dir_all(tmp.join("pkg")).unwrap();
    std::fs::write(tmp.join("pkg/old.rs"), "pub fn test_old() {}\n").unwrap();
    git(&tmp, &["init", "-q"]);
    git(&tmp, &["add", "."]);
    git(&tmp, &["commit", "-q", "-m", "init"]);
    kungfu_project::init_project(&tmp).unwrap();
    let svc = KungfuService::open(&tmp).unwrap();
    svc.index_full().unwrap();
    (tmp, svc)
}

fn strings(v: &serde_json::Value) -> Vec<&str> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect()
}

#[test]
fn deleted_and_modified_files_are_both_reported() {
    let (tmp, svc) = temp_repo("mixed");
    std::fs::remove_file(tmp.join("gone.rs")).unwrap();
    std::fs::write(tmp.join("keep.rs"), "pub fn keep_fn() { let _x = 1; }\n").unwrap();

    let out = svc.verify_change(1).unwrap();
    assert_eq!(out["status"], "changes");
    assert_eq!(strings(&out["changed_files"]), ["gone.rs", "keep.rs"]);
    assert_eq!(strings(&out["deleted_files"]), ["gone.rs"]);
    assert!(out["deleted_note"].is_string());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn deletion_only_diff_is_not_clean() {
    let (tmp, svc) = temp_repo("delonly");
    std::fs::remove_file(tmp.join("gone.rs")).unwrap();

    let out = svc.verify_change(1).unwrap();
    assert_eq!(out["status"], "changes");
    assert_eq!(strings(&out["changed_files"]), ["gone.rs"]);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn untracked_file_is_reported_with_its_symbols() {
    let (tmp, svc) = temp_repo("untracked");
    std::fs::write(tmp.join("fresh.rs"), "pub fn fresh_fn() {}\n").unwrap();

    let out = svc.verify_change(1).unwrap();
    assert_eq!(out["status"], "changes");
    assert_eq!(strings(&out["changed_files"]), ["fresh.rs"]);
    assert_eq!(strings(&out["untracked_files"]), ["fresh.rs"]);
    assert!(strings(&out["changed_symbols"]).contains(&"fresh.rs::fresh_fn"));
    assert!(out.get("deleted_files").is_none());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn untouched_repo_is_clean() {
    let (tmp, svc) = temp_repo("clean");
    assert_eq!(svc.verify_change(1).unwrap()["status"], "clean");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn untracked_files_feed_blast_radius_and_tests() {
    let (tmp, svc) = temp_repo("blast");
    std::fs::write(tmp.join("pkg/new.rs"), "pub fn newest_fn() {}\n").unwrap();

    let out = svc.verify_change(1).unwrap();
    let tests = out["suggested_tests"].as_array().unwrap();
    assert!(
        tests
            .iter()
            .any(|t| t["name"] == "test_old" && t["reason"] == "co-located test"),
        "untracked module must drive test selection: {out}"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn lists_are_capped_with_an_honest_marker() {
    let (tmp, svc) = temp_repo("cap");
    std::fs::create_dir_all(tmp.join("gen")).unwrap();
    for i in 0..60 {
        std::fs::write(
            tmp.join(format!("gen/f{i}.rs")),
            format!("pub fn gen_{i}() {{}}\n"),
        )
        .unwrap();
    }

    let out = svc.verify_change(1).unwrap();
    assert_eq!(out["changed_files"].as_array().unwrap().len(), 50);
    assert_eq!(out["untracked_files"].as_array().unwrap().len(), 50);
    assert!(out["changed_files_note"]
        .as_str()
        .unwrap()
        .contains("50 of 60"));
    assert!(out["untracked_files_note"].is_string());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn removing_lines_only_is_not_clean() {
    let (tmp, svc) = temp_repo("rmlines");
    std::fs::write(tmp.join("keep.rs"), "").unwrap();

    let out = svc.verify_change(1).unwrap();
    assert_eq!(out["status"], "changes");
    assert_eq!(strings(&out["changed_files"]), ["keep.rs"]);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn binary_change_without_hunks_is_not_clean() {
    let (tmp, svc) = temp_repo("binary");
    std::fs::write(tmp.join("logo.png"), [0x89, b'P', b'N', b'G', 0, 1]).unwrap();
    git(&tmp, &["add", "logo.png"]);
    git(&tmp, &["commit", "-q", "-m", "logo"]);
    std::fs::write(tmp.join("logo.png"), [0x89, b'P', b'N', b'G', 7, 7]).unwrap();

    let out = svc.verify_change(1).unwrap();
    assert_eq!(out["status"], "changes", "{out}");
    assert_eq!(strings(&out["changed_files"]), ["logo.png"]);
    assert_eq!(strings(&out["no_hunk_files"]), ["logo.png"]);
    assert!(out["no_hunk_note"].is_string());
    assert!(strings(&out["changed_symbols"]).is_empty());
    std::fs::remove_dir_all(&tmp).ok();
}

/// `git init` on an existing codebase, files staged, nothing committed yet.
#[test]
fn staged_files_without_any_commit_are_reported() {
    let tmp = std::env::temp_dir().join(format!("kungfu_verify_nohead_{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("staged.rs"), "pub fn staged_fn() {}\n").unwrap();
    std::fs::write(tmp.join("loose.rs"), "pub fn loose_fn() {}\n").unwrap();
    git(&tmp, &["init", "-q"]);
    git(&tmp, &["add", "staged.rs"]);
    kungfu_project::init_project(&tmp).unwrap();
    let svc = KungfuService::open(&tmp).unwrap();
    svc.index_full().unwrap();

    let out = svc.verify_change(1).unwrap();
    assert_eq!(out["status"], "changes", "{out}");
    assert_eq!(strings(&out["changed_files"]), ["loose.rs", "staged.rs"]);
    assert_eq!(strings(&out["untracked_files"]), ["loose.rs"]);
    let symbols = strings(&out["changed_symbols"]);
    assert!(symbols.contains(&"staged.rs::staged_fn"), "{out}");
    assert!(symbols.contains(&"loose.rs::loose_fn"), "{out}");
    assert!(out["base_note"].as_str().unwrap().contains("no commits"));

    // The sibling tools on the same repository work and say so too.
    let staged = svc.affected_staged(1).unwrap();
    assert!(staged.symbol.contains("staged_fn"), "{}", staged.symbol);
    assert!(staged.note.as_deref().unwrap().contains("no commits"));
    assert!(svc
        .smart_test()
        .unwrap()
        .changed_symbols
        .contains(&"staged.rs::staged_fn".to_string()));
    let review = svc.review().unwrap();
    assert_eq!(review.changed_files, ["staged.rs"]);
    assert!(review.summary.contains("no commits"));
    std::fs::remove_dir_all(&tmp).ok();
}

/// Scaffolded app before the first commit: thousands of symbols. The walk
/// is capped and says so instead of taking minutes.
#[test]
fn blast_radius_walk_is_capped_with_an_honest_note() {
    let (tmp, svc) = temp_repo("walkcap");
    std::fs::create_dir_all(tmp.join("gen")).unwrap();
    for i in 0..250 {
        std::fs::write(
            tmp.join(format!("gen/w{i}.rs")),
            format!("pub fn walk_{i}() {{}}\n"),
        )
        .unwrap();
    }

    let started = std::time::Instant::now();
    let out = svc.verify_change(3).unwrap();
    let note = out["affected"]["note"].as_str().unwrap();
    assert!(note.contains("200 of 250 changed symbols"), "{note}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "verify_change took {:?}",
        started.elapsed()
    );

    let staged = svc.affected_staged(3).unwrap();
    assert!(
        staged.note.is_none(),
        "untracked files are not staged: {:?}",
        staged.note
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn staged_rename_is_not_clean() {
    let (tmp, svc) = temp_repo("rename");
    git(&tmp, &["mv", "keep.rs", "kept.rs"]);

    let out = svc.verify_change(1).unwrap();
    assert_eq!(out["status"], "changes");
    assert_eq!(strings(&out["changed_files"]), ["keep.rs", "kept.rs"]);
    assert_eq!(strings(&out["deleted_files"]), ["keep.rs"]);
    std::fs::remove_dir_all(&tmp).ok();
}
