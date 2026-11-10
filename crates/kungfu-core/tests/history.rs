//! `history` must serve a file or a symbol, reject anything else, and not repeat commits.

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

fn temp_repo(tag: &str) -> (PathBuf, KungfuService) {
    let tmp = std::env::temp_dir().join(format!("kungfu_history_{tag}_{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("lib.rs"), "pub fn alpha_fn() {}\n").unwrap();
    git(&tmp, &["init", "-q"]);
    git(&tmp, &["add", "."]);
    git(&tmp, &["commit", "-q", "-m", "add alpha"]);
    std::fs::write(tmp.join("lib.rs"), "pub fn alpha_fn() { let _x = 1; }\n").unwrap();
    git(&tmp, &["commit", "-q", "-am", "tweak alpha"]);
    kungfu_project::init_project(&tmp).unwrap();
    let svc = KungfuService::open(&tmp).unwrap();
    svc.index_full().unwrap();
    (tmp, svc)
}

#[test]
fn file_mode_has_commits_and_timeline_without_duplicates() {
    let (tmp, svc) = temp_repo("file");
    let out = svc.history(Some("lib.rs"), None, Budget::Small).unwrap();
    assert_eq!(out["mode"], "file");
    assert_eq!(out["commits"].as_array().unwrap().len(), 2);
    // Both commits are shown, so the introduction is not repeated in the timeline.
    assert!(out["timeline"].as_array().unwrap().is_empty());
    assert!(out.get("hint").is_none());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn capped_log_reports_true_introduction_and_cap_hint() {
    let (tmp, svc) = temp_repo("cap");
    for i in 0..4 {
        std::fs::write(
            tmp.join("lib.rs"),
            format!("pub fn alpha_fn() {{ let _x = {i}; }}\n"),
        )
        .unwrap();
        git(&tmp, &["commit", "-q", "-am", &format!("edit {i}")]);
    }
    let out = svc.history(Some("lib.rs"), None, Budget::Tiny).unwrap();
    assert_eq!(out["commits"].as_array().unwrap().len(), 3);
    assert!(out["hint"].as_str().unwrap().contains("larger budget"));
    let intro = out["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event_type"] == "introduced")
        .unwrap();
    assert!(intro["detail"].as_str().unwrap().contains("add alpha"));
    let full = svc.history(Some("lib.rs"), None, Budget::Full).unwrap();
    assert!(full.get("hint").is_none());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn file_mode_never_resolves_to_a_different_file() {
    let (tmp, svc) = temp_repo("fuzzy");
    std::fs::create_dir_all(tmp.join("pkg/src")).unwrap();
    std::fs::write(tmp.join("pkg/src/lib.rs"), "pub fn beta_fn() {}\n").unwrap();
    git(&tmp, &["add", "."]);
    git(&tmp, &["commit", "-q", "-m", "add beta"]);
    svc.index_full().unwrap();
    let out = svc
        .history(Some("src/lib.rs"), None, Budget::Small)
        .unwrap();
    assert_eq!(out["status"], "no_commits");
    assert!(out["timeline"].as_array().unwrap().is_empty());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn symbol_mode_has_blame_and_timeline() {
    let (tmp, svc) = temp_repo("symbol");
    let out = svc.history(None, Some("alpha_fn"), Budget::Small).unwrap();
    assert_eq!(out["mode"], "symbol");
    assert_eq!(out["path"], "lib.rs");
    assert!(!out["blame"].as_array().unwrap().is_empty());
    assert_eq!(out["recent_commits"].as_array().unwrap().len(), 2);
    assert!(out["timeline"].is_array());
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn untracked_path_is_flagged_and_bad_params_error() {
    let (tmp, svc) = temp_repo("errs");
    let out = svc.history(Some("nope.rs"), None, Budget::Small).unwrap();
    assert_eq!(out["status"], "no_commits");
    assert!(out["hint"].is_string());
    assert!(svc.history(None, None, Budget::Small).is_err());
    assert!(svc
        .history(Some("lib.rs"), Some("alpha_fn"), Budget::Small)
        .is_err());
    assert!(svc
        .history(None, Some("missing_sym"), Budget::Small)
        .is_err());
    std::fs::remove_dir_all(&tmp).ok();
}
