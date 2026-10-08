//! Lazy freshness check (`ensure_fresh_index`): edits anywhere in the indexed
//! tree — not just top-level entries — must trigger a reindex, while an
//! untouched tree, a hidden-file write or an ignored entry must not.
//!
//! Filesystem mtime granularity can be coarse, so tests never rely on "written
//! later means newer": `age_index` backdates the recorded scan and the fixture
//! files explicitly, making any subsequent write strictly newer.

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

fn index_file(tmp: &Path, name: &str) -> PathBuf {
    tmp.join(".kungfu/index").join(name)
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

fn edit_tree_state(tmp: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = index_file(tmp, "tree_state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    edit(&mut state);
    let staged = index_file(tmp, "tree_state.json.test");
    std::fs::write(&staged, state.to_string()).unwrap();
    std::fs::rename(&staged, &path).unwrap();
}

/// Pretend the last scan ran 10s ago over files last edited 20s ago.
/// Directory mtimes stay "now" (newer than the scan) on purpose: the check
/// must not reindex just because a directory is newer when nothing indexable
/// was added to it.
fn age_index(tmp: &Path) {
    let now = SystemTime::now();
    let scanned_at = chrono::DateTime::<chrono::Utc>::from(now - Duration::from_secs(10));
    edit_tree_state(tmp, |state| {
        state["scanned_at"] = serde_json::json!(scanned_at);
        state["refreshed"] = serde_json::json!({});
    });
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
    let fp = index_file(&tmp, "fingerprints.json");
    let before = mtime(&fp);
    assert!(!ensure(&tmp), "nothing changed since indexing");
    assert_eq!(mtime(&fp), before, "no index write");

    age_index(&tmp);
    let before = mtime(&fp);
    assert!(
        !ensure(&tmp),
        "newer directories with no unknown entries are not stale"
    );
    assert_eq!(mtime(&fp), before, "no index write");
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
    assert!(!ensure(&tmp), "the reindex recorded a new scan — settled");
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
fn file_added_to_previously_empty_dir_is_indexed() {
    let tmp = temp_project("emptydir");
    age_index(&tmp);
    std::fs::create_dir_all(tmp.join("modules/sub")).unwrap();
    assert!(ensure(&tmp), "a new walkable directory is a new entry");
    assert!(!ensure(&tmp), "the empty dir is now recorded — settled");

    age_index(&tmp);
    std::fs::write(
        tmp.join("modules/sub/new_mod.rs"),
        "pub fn epsilon_late_fn() {}\n",
    )
    .unwrap();
    assert!(
        ensure(&tmp),
        "a file added to a dir that was empty at the last scan is detected"
    );
    assert!(has_symbol(&tmp, "epsilon_late_fn"));
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
    let fp = index_file(&tmp, "fingerprints.json");
    let before = mtime(&fp);

    assert!(!ensure(&tmp), "hidden files are never indexed");
    assert_eq!(mtime(&fp), before, "no index write");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn hidden_write_next_to_ignored_dir_does_not_trigger_reindex() {
    let tmp = temp_project("ignored");
    // .gitignore only applies inside a git repository.
    std::fs::create_dir_all(tmp.join(".git")).unwrap();
    std::fs::write(tmp.join(".gitignore"), "__pycache__/\n").unwrap();
    std::fs::create_dir_all(tmp.join("modules/__pycache__")).unwrap();
    std::fs::write(tmp.join("modules/__pycache__/zeta.cpython.pyc"), "x").unwrap();
    let service = KungfuService::open(&tmp).unwrap();
    service.index_full().unwrap();
    assert_eq!(
        service.status().unwrap().indexed_files,
        2,
        "__pycache__ is ignored, so it is not part of the index"
    );

    age_index(&tmp);
    std::fs::write(tmp.join("modules/.zeta.rs.swp"), "swap").unwrap();
    let fp = index_file(&tmp, "fingerprints.json");
    let before = mtime(&fp);

    assert!(
        !ensure(&tmp),
        "an ignored sibling is not a new entry, and the swap file is hidden"
    );
    assert_eq!(mtime(&fp), before, "no index write");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn edit_during_scan_is_not_lost() {
    let tmp = temp_project("race");
    let before = SystemTime::now();
    KungfuService::open(&tmp).unwrap().index_full().unwrap();
    let after = SystemTime::now();
    // An edit landing mid-scan, after the file was read, has an mtime inside
    // the scan window — it must not count as covered by the index.
    let mid_scan = before + (after.duration_since(before).unwrap() / 2);
    set_mtime(&tmp.join("modules/zeta.rs"), mid_scan);

    assert!(ensure(&tmp), "an edit during the scan stays visible");
    assert!(!ensure(&tmp));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn targeted_reindex_settles_without_hiding_other_edits() {
    let tmp = temp_project("hook");
    age_index(&tmp);
    // A hooked edit (reindexed right away) next to an unhooked shell edit.
    std::fs::write(
        tmp.join("modules/nested/deep/alpha.rs"),
        "pub fn alpha_hooked_fn() {}\n",
    )
    .unwrap();
    std::fs::write(tmp.join("modules/zeta.rs"), "pub fn zeta_shell_fn() {}\n").unwrap();
    KungfuService::open(&tmp)
        .unwrap()
        .index_paths(&["modules/nested/deep/alpha.rs".to_string()])
        .unwrap();

    assert!(
        ensure(&tmp),
        "the targeted reindex must not cover the unhooked edit"
    );
    assert!(has_symbol(&tmp, "zeta_shell_fn"));

    age_index(&tmp);
    std::fs::create_dir_all(tmp.join("modules/brand_new")).unwrap();
    std::fs::write(
        tmp.join("modules/brand_new/eta.rs"),
        "pub fn eta_hooked_fn() {}\n",
    )
    .unwrap();
    KungfuService::open(&tmp)
        .unwrap()
        .index_paths(&["modules/brand_new/eta.rs".to_string()])
        .unwrap();
    let fp = index_file(&tmp, "fingerprints.json");
    let before = mtime(&fp);
    assert!(
        !ensure(&tmp),
        "a hooked new file in a new dir is already accounted for"
    );
    assert_eq!(mtime(&fp), before, "no index write");
    assert!(has_symbol(&tmp, "eta_hooked_fn"));
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn index_without_tree_state_rescans_once() {
    let tmp = temp_project("legacy");
    std::fs::remove_file(index_file(&tmp, "tree_state.json")).unwrap();

    assert!(ensure(&tmp), "an index predating tree_state.json rescans");
    assert!(index_file(&tmp, "tree_state.json").exists());
    assert!(!ensure(&tmp));
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
