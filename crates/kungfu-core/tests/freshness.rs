//! Lazy freshness check (`ensure_fresh_index`): edits anywhere in the indexed
//! tree — not just top-level entries — must trigger a reindex, while an
//! untouched tree or a hidden-file change must not.
//!
//! Filesystem mtime granularity can be a full second, so tests never rely on
//! "written later means newer": `age_index` backdates the index and fixture
//! files explicitly, making any subsequent write strictly newer than the index.

use kungfu_core::KungfuService;
use kungfu_types::budget::Budget;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

fn temp_project(tag: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!("kungfu_fresh_{tag}_{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    // Not `src/`: the old heuristic sampled top-level entries of src/, crates/, …
    // and would pass these tests by accident.
    std::fs::create_dir_all(tmp.join("modules/nested/deep")).unwrap();
    std::fs::write(
        tmp.join("modules/nested/deep/alpha.rs"),
        "pub fn alpha_original_fn() {}\n",
    )
    .unwrap();
    std::fs::write(tmp.join("modules/zeta.rs"), "pub fn zeta_doomed_fn() {}\n").unwrap();
    kungfu_project::init_project(&tmp).unwrap();
    KungfuService::open(&tmp).unwrap().index_full().unwrap();
    tmp
}

fn fingerprints(tmp: &Path) -> PathBuf {
    tmp.join(".kungfu/index/fingerprints.json")
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

fn set_mtime(path: &Path, t: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// Pretend the index was written 10s ago over files last edited 20s ago.
/// Directory mtimes stay "now" (newer than the index) on purpose: the check
/// must not reindex just because a directory is newer when nothing indexable
/// was added to it.
fn age_index(tmp: &Path) {
    let now = SystemTime::now();
    set_mtime(&fingerprints(tmp), now - Duration::from_secs(10));
    for rel in ["modules/nested/deep/alpha.rs", "modules/zeta.rs"] {
        set_mtime(&tmp.join(rel), now - Duration::from_secs(20));
    }
}

/// A fresh service per call, as the MCP server does.
fn ensure(tmp: &Path) -> bool {
    KungfuService::open(tmp)
        .unwrap()
        .ensure_fresh_index()
        .unwrap()
}

fn has_symbol(tmp: &Path, name: &str) -> bool {
    KungfuService::open(tmp)
        .unwrap()
        .find_symbol(name, Budget::Small)
        .unwrap()
        .iter()
        .any(|r| r.item.name == name)
}

#[test]
fn untouched_tree_is_fresh_without_reindex() {
    let tmp = temp_project("untouched");
    let before = mtime(&fingerprints(&tmp));
    assert!(!ensure(&tmp), "nothing changed since indexing");
    assert_eq!(mtime(&fingerprints(&tmp)), before, "no index write");

    age_index(&tmp);
    let before = mtime(&fingerprints(&tmp));
    assert!(
        !ensure(&tmp),
        "newer directories with no unknown entries are not stale"
    );
    assert_eq!(mtime(&fingerprints(&tmp)), before, "no index write");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn nested_edit_is_reindexed_and_settles() {
    let tmp = temp_project("edit");
    age_index(&tmp);
    std::fs::write(
        tmp.join("modules/nested/deep/alpha.rs"),
        "pub fn alpha_renamed_fn() {}\n",
    )
    .unwrap();

    assert!(ensure(&tmp), "a nested edit makes the index stale");
    assert!(has_symbol(&tmp, "alpha_renamed_fn"));
    assert!(!has_symbol(&tmp, "alpha_original_fn"));
    assert!(!ensure(&tmp), "the reindex rewrote fingerprints — settled");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn new_file_in_nested_dir_is_indexed() {
    let tmp = temp_project("new");
    age_index(&tmp);
    std::fs::write(
        tmp.join("modules/nested/deep/gamma.rs"),
        "pub fn gamma_added_fn() {}\n",
    )
    .unwrap();

    assert!(
        ensure(&tmp),
        "a new file in a nested dir makes the index stale"
    );
    assert!(has_symbol(&tmp, "gamma_added_fn"));
    assert!(!ensure(&tmp));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn new_subdirectory_is_indexed() {
    let tmp = temp_project("subdir");
    age_index(&tmp);
    std::fs::create_dir_all(tmp.join("modules/nested/fresh_mod")).unwrap();
    std::fs::write(
        tmp.join("modules/nested/fresh_mod/delta.rs"),
        "pub fn delta_added_fn() {}\n",
    )
    .unwrap();

    assert!(ensure(&tmp), "a new subdirectory makes the index stale");
    assert!(has_symbol(&tmp, "delta_added_fn"));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn deleted_file_is_dropped() {
    let tmp = temp_project("delete");
    assert!(has_symbol(&tmp, "zeta_doomed_fn"));
    std::fs::remove_file(tmp.join("modules/zeta.rs")).unwrap();

    assert!(ensure(&tmp), "a removed indexed file makes the index stale");
    assert!(!has_symbol(&tmp, "zeta_doomed_fn"));
    assert!(!ensure(&tmp));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn hidden_file_does_not_trigger_reindex() {
    let tmp = temp_project("hidden");
    age_index(&tmp);
    std::fs::write(tmp.join("modules/nested/.DS_Store"), "junk").unwrap();
    let before = mtime(&fingerprints(&tmp));

    assert!(!ensure(&tmp), "hidden files are never indexed");
    assert_eq!(mtime(&fingerprints(&tmp)), before, "no index write");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn future_mtime_does_not_loop() {
    let tmp = temp_project("future");
    set_mtime(
        &tmp.join("modules/zeta.rs"),
        SystemTime::now() + Duration::from_secs(3600),
    );

    assert!(
        !ensure(&tmp),
        "a future mtime is not an edit — reindexing would repeat every call"
    );
    std::fs::remove_dir_all(&tmp).ok();
}
