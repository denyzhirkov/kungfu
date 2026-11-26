//! `--json` output of find-symbol and search is a bare array in every run,
//! whether or not the config ranking acted; the ranking note goes to stderr.

use std::path::{Path, PathBuf};
use std::process::Command;

fn temp_project() -> PathBuf {
    let tmp = std::env::temp_dir().join(format!("kungfu_cli_json_shapes_{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("src/engine.rs"),
        "pub fn compute_widget_total() -> u32 { 1 }\npub fn bare() {}\n",
    )
    .unwrap();
    std::fs::write(tmp.join("package.json"), "{\n  \"widget_total\": 3\n}\n").unwrap();
    kungfu_project::init_project(&tmp).unwrap();
    kungfu_core::KungfuService::open(&tmp)
        .unwrap()
        .index_full()
        .unwrap();
    tmp
}

fn run(dir: &Path, args: &[&str]) -> (serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_kungfu"))
        .args(args)
        .current_dir(dir)
        .env("KUNGFU_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    let stdout: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    (stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn json_is_a_bare_array_and_the_ranking_note_goes_to_stderr() {
    let tmp = temp_project();
    for cmd in ["find-symbol", "search"] {
        let (demoted, stderr) = run(&tmp, &["--json", cmd, "widget total"]);
        assert!(demoted.is_array(), "{cmd}: {demoted}");
        assert_eq!(demoted[0]["path"], "src/engine.rs", "{cmd}: {demoted}");
        assert!(stderr.contains("Ranking: "), "{cmd}: stderr {stderr:?}");

        let (plain, stderr) = run(&tmp, &["--json", cmd, "bare"]);
        assert!(plain.is_array(), "{cmd}: {plain}");
        assert!(!stderr.contains("Ranking: "), "{cmd}: stderr {stderr:?}");
    }
    std::fs::remove_dir_all(&tmp).ok();
}
