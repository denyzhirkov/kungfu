//! A fresh clone that tracks `.kungfu/memory/` has a partial `.kungfu/`;
//! `init_project` completes it and the tracked memory stays readable.

use kungfu_core::KungfuService;
use kungfu_memory::project_search::MemoryFilter;
use kungfu_types::budget::Budget;
use kungfu_types::memory::ProjectMemoryKind;

#[test]
fn init_completes_partial_dir_and_memory_survives() {
    let tmp = std::env::temp_dir().join(format!("kungfu_partial_init_{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/lib.rs"), "pub fn alpha() {}\n").unwrap();

    // Create a memory in a fully initialized project, then strip everything but memory/.
    kungfu_project::init_project(&tmp).unwrap();
    let id = {
        let s = KungfuService::open(&tmp).unwrap();
        s.memory_add(
            ProjectMemoryKind::Decision,
            "zebrafish retention policy",
            None,
            vec![],
            vec![],
            vec![],
            false,
        )
        .unwrap()
        .id
    };
    for entry in std::fs::read_dir(tmp.join(".kungfu")).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().and_then(|n| n.to_str()) != Some("memory") {
            if path.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
        }
    }
    assert!(kungfu_project::is_partial(&tmp));
    assert!(KungfuService::open(&tmp).is_err());

    kungfu_project::init_project(&tmp).unwrap();
    let s = KungfuService::open(&tmp).unwrap();
    let listed = s.memory_list(&MemoryFilter::default()).unwrap();
    assert!(listed.iter().any(|m| m.id == id));
    let found = s
        .memory_search("zebrafish", &MemoryFilter::default(), Budget::Small)
        .unwrap();
    assert!(format!("{found:?}").contains(&id));

    s.index_full().unwrap();
    assert!(tmp.join(".kungfu/index/symbols.json").exists());
    std::fs::remove_dir_all(&tmp).ok();
}
