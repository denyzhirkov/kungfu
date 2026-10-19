//! A targeted reindex (`index_only`, `index_incremental`) after an edit must
//! leave the same index as a full rebuild of the edited tree: files, symbols,
//! relations and memories compared as sets (order and timestamps aside).

use kungfu_config::KungfuConfig;
use kungfu_index::Indexer;
use kungfu_storage::JsonStore;
use kungfu_types::relation::RelationKind;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

fn temp_root(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kungfu-equiv-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let abs = root.join(path);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, content).unwrap();
    }
}

fn index_dir(root: &Path) -> PathBuf {
    root.join(".kungfu").join("index")
}

fn store(root: &Path) -> JsonStore {
    std::fs::create_dir_all(index_dir(root)).unwrap();
    JsonStore::new(&index_dir(root))
}

fn copy_dir(from: &Path, to: &Path) {
    let _ = std::fs::remove_dir_all(to);
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

fn sorted_json<T: serde::Serialize>(items: &[T], drop_field: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = items
        .iter()
        .map(|item| {
            let mut v = serde_json::to_value(item).unwrap();
            if let (Some(field), Some(obj)) = (drop_field, v.as_object_mut()) {
                obj.remove(field);
            }
            v.to_string()
        })
        .collect();
    out.sort();
    out
}

#[derive(Debug)]
struct Snapshot {
    files: Vec<String>,
    symbols: Vec<String>,
    relations: Vec<String>,
    memories: Vec<String>,
}

fn snapshot(root: &Path) -> Snapshot {
    let store = JsonStore::new(&index_dir(root));
    Snapshot {
        files: sorted_json(&store.load_files().unwrap(), Some("indexed_at")),
        symbols: sorted_json(&store.load_symbols().unwrap(), None),
        relations: sorted_json(&store.load_relations().unwrap(), None),
        memories: sorted_json(&store.load_memories().unwrap(), None),
    }
}

/// Field by field, so a failure names the shard that diverged.
fn assert_same(targeted: &Snapshot, full: &Snapshot, run: &str) {
    assert_eq!(
        targeted.files, full.files,
        "{run}: files diverged from index_full"
    );
    assert_eq!(
        targeted.symbols, full.symbols,
        "{run}: symbols diverged from index_full"
    );
    assert_eq!(
        targeted.relations, full.relations,
        "{run}: relations diverged from index_full"
    );
    assert_eq!(
        targeted.memories, full.memories,
        "{run}: memories diverged from index_full"
    );
}

/// A file written with new content, or deleted (`None`).
type Edit<'a> = (&'a str, Option<&'a str>);

/// Index `before`, apply `edits`, then bring the index up to date with
/// `index_only`, `index_incremental` and `index_full`; return the three.
fn reindex_three_ways(
    config: &KungfuConfig,
    before: &[(&str, &str)],
    edits: &[Edit],
) -> (Snapshot, Snapshot, Snapshot) {
    let root = temp_root("tree");
    write(&root, before);
    Indexer::new(&root, config.clone(), &store(&root))
        .index_full()
        .unwrap();
    let pristine = root.join(".kungfu").join("pristine");
    copy_dir(&index_dir(&root), &pristine);

    for (path, content) in edits {
        match content {
            Some(content) => write(&root, &[(path, content)]),
            None => std::fs::remove_file(root.join(path)).unwrap(),
        }
    }
    let edited: Vec<String> = edits.iter().map(|(p, _)| p.to_string()).collect();

    Indexer::new(&root, config.clone(), &store(&root))
        .index_only(&edited)
        .unwrap();
    let only = snapshot(&root);

    copy_dir(&pristine, &index_dir(&root));
    Indexer::new(&root, config.clone(), &store(&root))
        .index_incremental()
        .unwrap();
    let incremental = snapshot(&root);

    std::fs::remove_dir_all(index_dir(&root)).unwrap();
    Indexer::new(&root, config.clone(), &store(&root))
        .index_full()
        .unwrap();
    let full = snapshot(&root);

    std::fs::remove_dir_all(&root).ok();
    (only, incremental, full)
}

fn relation_kinds(s: &Snapshot) -> Vec<RelationKind> {
    s.relations
        .iter()
        .map(|r| {
            let v: serde_json::Value = serde_json::from_str(r).unwrap();
            serde_json::from_value(v["kind"].clone()).unwrap()
        })
        .collect()
}

const TREE: &[(&str, &str)] = &[
    ("README.md", "# Demo\n\nWhy the demo exists.\n"),
    ("app/Cargo.toml", "[package]\nname = \"app\"\n"),
    ("app/build.rs", "fn build_main() { helper(); }\n"),
    (
        "app/src/util.rs",
        "/// Doc for helper.\npub fn helper() {}\n\npub fn only_in_util() {}\n\n\
         // TODO: tidy this up\npub fn tidy() { popular(); }\n",
    ),
    (
        "app/src/api.rs",
        "use crate::util::helper;\n\npub fn serve() { helper(); only_in_util(); dup(); popular(); }\n",
    ),
    ("app/src/other.rs", "pub fn dup() {}\n\npub fn popular() {}\n"),
    (
        "app/src/main.rs",
        "use crate::api::serve;\nuse crate::util::helper;\n\n\
         fn main() { serve(); helper(); popular(); twin_fn(); }\n",
    ),
    // Byte-identical files share one id and one set of symbol ids.
    ("app/src/twin_a.rs", "pub fn twin_fn() {}\n"),
    ("app/src/twin_b.rs", "pub fn twin_fn() {}\n"),
    (
        "app/src/util_test.rs",
        "use crate::util::helper;\n\nfn test_helper() { helper(); }\n",
    ),
];

/// A content edit that shifts every line (all ids of util.rs change), rewords
/// a comment, removes `only_in_util` (its edge from api.rs must go), adds a
/// second `dup` (api.rs's unique-name edge to other.rs must go) and stops
/// calling `popular` (with a cutoff of 2 caller files that flips it back).
const UTIL_EDIT: &[Edit] = &[(
    "app/src/util.rs",
    Some(
        "//! Utilities.\n\n/// Doc for helper, reworded.\npub fn helper() {}\n\npub fn dup() {}\n\n\
         // TODO: tidy this up later\npub fn tidy() {}\n",
    ),
)];

fn assert_equivalent(config: KungfuConfig) -> Snapshot {
    let full = assert_equivalent_after(config, UTIL_EDIT);
    assert!(relation_kinds(&full).contains(&RelationKind::TestFor));
    // The twins' shared `twin_fn` is one definition, so main links to it.
    assert!(full.relations.iter().any(|r| r.contains(":twin_fn\"")));
    full
}

fn assert_equivalent_after(config: KungfuConfig, edits: &[Edit]) -> Snapshot {
    let (only, incremental, full) = reindex_three_ways(&config, TREE, edits);

    // Guard against a vacuous pass: the fixture exercises every edge kind.
    let kinds: BTreeSet<RelationKind> = relation_kinds(&full).into_iter().collect();
    assert!(kinds.contains(&RelationKind::Imports), "{kinds:?}");
    assert!(kinds.contains(&RelationKind::ConfigFor), "{kinds:?}");
    assert!(kinds.contains(&RelationKind::Calls), "{kinds:?}");
    assert!(!full.memories.is_empty());

    assert_same(&only, &full, "index_only");
    assert_same(&incremental, &full, "index_incremental");
    full
}

#[test]
fn targeted_reindex_matches_full_index() {
    assert_equivalent(KungfuConfig::default());
}

#[test]
fn targeted_reindex_matches_full_index_when_cutoff_flips() {
    let mut config = KungfuConfig::default();
    config.call_graph.max_caller_files = 2;
    let full = assert_equivalent(config);
    // `popular` had 3 caller files before the edit (cut) and 2 after (kept).
    assert!(
        full.relations.iter().any(|r| r.contains(":popular\"")),
        "edit should bring popular's edges back under the cutoff"
    );
}

#[test]
fn targeted_reindex_matches_full_index_with_same_file_calls() {
    let mut config = KungfuConfig::default();
    config.call_graph.cross_file_only = false;
    assert_equivalent(config);
}

/// Known gap (see `delta` module docs): an untouched caller gains no edge to
/// a name that the edit made resolvable. A full index links it.
#[test]
fn targeted_reindex_does_not_link_untouched_callers_to_new_names() {
    let before: &[(&str, &str)] = &[
        ("lib.rs", "pub fn existing() {}\n"),
        ("main.rs", "fn run() { brand_new(); }\n"),
    ];
    let after: &[Edit] = &[(
        "lib.rs",
        Some("pub fn existing() {}\npub fn brand_new() {}\n"),
    )];
    let (only, incremental, full) = reindex_three_ways(&KungfuConfig::default(), before, after);

    let calls = |s: &Snapshot| {
        relation_kinds(s)
            .into_iter()
            .filter(|k| *k == RelationKind::Calls)
            .count()
    };
    assert_eq!(calls(&full), 1, "full index links run → brand_new");
    assert_eq!(calls(&only), 0);
    assert_eq!(calls(&incremental), 0);
    // Everything else still matches.
    assert_eq!(only.symbols, full.symbols);
    assert_eq!(incremental.symbols, full.symbols);
}

#[test]
fn targeted_reindex_matches_full_index_after_delete() {
    // util.rs is imported by three untouched files and tested by util_test.rs.
    assert_equivalent_after(KungfuConfig::default(), &[("app/src/util.rs", None)]);
}

#[test]
fn targeted_reindex_matches_full_index_after_rename() {
    let util = TREE
        .iter()
        .find(|(p, _)| *p == "app/src/util.rs")
        .unwrap()
        .1;
    assert_equivalent_after(
        KungfuConfig::default(),
        &[
            ("app/src/util.rs", None),
            ("app/src/helpers.rs", Some(util)),
        ],
    );
}

#[test]
fn targeted_reindex_matches_full_index_after_delete_and_add() {
    assert_equivalent_after(
        KungfuConfig::default(),
        &[
            ("app/src/other.rs", None),
            (
                "app/src/extra.rs",
                Some("pub fn dup() {}\n\npub fn popular() {}\n\npub fn extra() {}\n"),
            ),
        ],
    );
}

/// Every byte-identical copy edited apart: the shared old id has no single
/// successor, so the untouched importer is re-parsed rather than losing its
/// import edge.
#[test]
fn targeted_reindex_reresolves_importers_of_twins_edited_apart() {
    let before: &[(&str, &str)] = &[
        ("src/twin_a.rs", "pub fn twin() {}\n"),
        ("src/twin_b.rs", "pub fn twin() {}\n"),
        ("src/main.rs", "use crate::twin_a::twin;\n\nfn main() {}\n"),
    ];
    let edits: &[Edit] = &[
        ("src/twin_a.rs", Some("pub fn twin() {}\npub fn a() {}\n")),
        ("src/twin_b.rs", Some("pub fn twin() {}\npub fn b() {}\n")),
    ];
    let (only, incremental, full) = reindex_three_ways(&KungfuConfig::default(), before, edits);
    assert!(
        relation_kinds(&full).contains(&RelationKind::Imports),
        "fixture must produce the main → twin_a import"
    );
    assert_same(&only, &full, "index_only");
    assert_same(&incremental, &full, "index_incremental");
}

/// Orphaned importers that are themselves identical copies are re-parsed
/// unchanged; their shared id must keep its single successor, or their own
/// untouched importers lose the edge.
#[test]
fn targeted_reindex_keeps_edges_into_identical_orphaned_importers() {
    let imp = "use crate::a::gamma_fn;\n\npub fn imp_x() {}\n";
    let before: &[(&str, &str)] = &[
        ("src/a.rs", "pub fn gamma_fn() {}\n"),
        ("src/b.rs", "pub fn gamma_fn() {}\n"),
        ("src/i1.rs", imp),
        ("src/i2.rs", imp),
        ("src/main.rs", "use crate::i1::imp_x;\n\nfn main() {}\n"),
    ];
    let (only, incremental, full) =
        reindex_three_ways(&KungfuConfig::default(), before, &[("src/a.rs", None)]);
    assert!(
        relation_kinds(&full).contains(&RelationKind::Imports),
        "fixture must produce the main → i1 import"
    );
    assert_same(&only, &full, "index_only");
    assert_same(&incremental, &full, "index_incremental");
}
