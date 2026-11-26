//! Config files are indexed as `config_key` / `config_block` symbols, capped
//! per file with the total recorded; lock files are recorded by name only.

use kungfu_config::KungfuConfig;
use kungfu_index::Indexer;
use kungfu_storage::JsonStore;
use kungfu_types::symbol::SymbolKind;
use std::path::PathBuf;

fn temp_root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kungfu-config-symbols-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn package_json_is_capped_and_lock_file_is_name_only() {
    let root = temp_root();
    let deps: Vec<String> = (0..60)
        .map(|i| format!("    \"dep-{i:02}\": \"^1.0.{i}\""))
        .collect();
    let package_json = format!(
        "{{\n  \"name\": \"app\",\n  \"dependencies\": {{\n{}\n  }}\n}}\n",
        deps.join(",\n")
    );
    std::fs::write(root.join("package.json"), &package_json).unwrap();
    std::fs::write(
        root.join("package-lock.json"),
        "{\n  \"name\": \"app\",\n  \"packages\": {\n    \"\": {}\n  }\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

    let index_dir = root.join(".kungfu").join("index");
    std::fs::create_dir_all(&index_dir).unwrap();
    let store = JsonStore::new(&index_dir);
    Indexer::new(&root, KungfuConfig::default(), &store)
        .index_full()
        .unwrap();

    let files = store.load_files().unwrap();
    let symbols = store.load_symbols().unwrap();
    let file = |path: &str| files.iter().find(|f| f.path == path).unwrap();

    let package: Vec<_> = symbols
        .iter()
        .filter(|s| s.path == "package.json")
        .collect();
    assert_eq!(package.len(), 50, "default max_config_symbols_per_file");
    assert!(package.iter().all(|s| s.kind.is_config()));
    let names: Vec<&str> = package.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"name"));
    assert!(names.contains(&"dependencies"));
    assert!(names.contains(&"dependencies.dep-00"));
    let deps_block = package.iter().find(|s| s.name == "dependencies").unwrap();
    assert_eq!(deps_block.kind, SymbolKind::ConfigBlock);
    // name + dependencies + 60 entries.
    assert_eq!(file("package.json").config_symbols_total, Some(62));
    assert!(!file("package.json").config_parse_errors);

    let lock = file("package-lock.json");
    assert!(
        lock.hash.starts_with("skip:"),
        "lock file read: {}",
        lock.hash
    );
    assert!(!symbols.iter().any(|s| s.path == "package-lock.json"));
    assert_eq!(file("main.rs").config_symbols_total, None);

    let _ = std::fs::remove_dir_all(&root);
}
