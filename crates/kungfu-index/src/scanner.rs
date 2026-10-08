use anyhow::Result;
use ignore::WalkBuilder;
use kungfu_config::KungfuConfig;
use std::path::{Path, PathBuf};
use tracing::debug;

/// What a project scan found: files to index and every directory walked
/// (including empty ones and the root itself).
pub struct ScannedTree {
    pub files: Vec<PathBuf>,
    pub dirs: Vec<PathBuf>,
}

pub fn scan_tree(root: &Path, config: &KungfuConfig) -> Result<ScannedTree> {
    let mut tree = ScannedTree {
        files: Vec::new(),
        dirs: Vec::new(),
    };
    for entry in walk_builder(root, &[root.to_path_buf()], config)?.build() {
        let entry = entry?;
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            tree.dirs.push(entry.into_path());
        } else if ft.is_file() && should_index(entry.path(), config) {
            tree.files.push(entry.into_path());
        }
    }

    debug!(
        "scanned {} files in {} dirs",
        tree.files.len(),
        tree.dirs.len()
    );
    Ok(tree)
}

/// Entries directly inside `dirs` that `scan_tree` would collect: files it
/// would index and directories it would descend into, under the same
/// gitignore (including parent ignore files), hidden, `ignore.paths` and
/// language rules — one level deep.
pub fn scan_dirs_shallow(
    root: &Path,
    dirs: &[PathBuf],
    config: &KungfuConfig,
) -> Result<Vec<PathBuf>> {
    if dirs.is_empty() {
        return Ok(Vec::new());
    }
    let mut builder = walk_builder(root, dirs, config)?;
    builder.max_depth(Some(1));
    let mut found = Vec::new();
    for entry in builder.build() {
        // One unreadable entry must not hide the others from the caller.
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                debug!("skipping unreadable entry: {e}");
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let Some(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() || (ft.is_file() && should_index(entry.path(), config)) {
            found.push(entry.into_path());
        }
    }
    Ok(found)
}

/// Walker over `starts` with the project's ignore rules. Overrides stay
/// anchored at `root`, so a walk started in a subdirectory applies
/// `ignore.paths` exactly as a walk from the root does.
fn walk_builder(root: &Path, starts: &[PathBuf], config: &KungfuConfig) -> Result<WalkBuilder> {
    let mut builder = WalkBuilder::new(starts.first().map_or(root, |p| p.as_path()));
    for start in starts.iter().skip(1) {
        builder.add(start);
    }
    // kungfu's own state dir is never project content, whatever the config
    // says (`ignore.paths` replaces its defaults; `index_hidden` lets dot
    // dirs in). Indexing it would make every index write look like an edit.
    // Matched by name at any depth so a nested `init --here` project's state
    // dir stays out of the parent index too. Same name as
    // `kungfu_project::KUNGFU_DIR`.
    builder
        .hidden(!config.index_hidden)
        .follow_links(config.follow_symlinks)
        .git_ignore(true)
        .git_global(true)
        .filter_entry(|entry| {
            !(entry.file_name() == ".kungfu" && entry.file_type().is_some_and(|t| t.is_dir()))
        });

    let mut overrides = ignore::overrides::OverrideBuilder::new(root);
    for path in &config.ignore.paths {
        overrides.add(&format!("!{}", path))?;
    }
    builder.overrides(overrides.build()?);
    Ok(builder)
}

fn should_index(path: &Path, config: &KungfuConfig) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let lang = kungfu_types::file::Language::from_extension(ext);
    let lang_str = lang.to_string();

    config.languages.enabled.iter().any(|l| l == &lang_str)
        || lang == kungfu_types::file::Language::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shallow_scan_applies_the_full_scan_rules() {
        let root = std::env::temp_dir().join(format!("kungfu_scan_shallow_{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let pkg = root.join("pkg");
        for dir in ["__pycache__", "node_modules", "sub/empty", ".hidden_dir"] {
            std::fs::create_dir_all(pkg.join(dir)).unwrap();
        }
        // .gitignore only applies inside a git repository.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".gitignore"), "__pycache__/\n*.log\n").unwrap();
        for file in ["a.rs", "notes.txt", ".DS_Store", "run.log"] {
            std::fs::write(pkg.join(file), "x").unwrap();
        }
        let config = KungfuConfig::default();
        let rel = |p: &PathBuf| p.strip_prefix(&root).unwrap().to_string_lossy().to_string();

        let mut shallow: Vec<String> =
            scan_dirs_shallow(&root, std::slice::from_ref(&pkg), &config)
                .unwrap()
                .iter()
                .map(rel)
                .collect();
        shallow.sort();
        // gitignored (__pycache__, *.log), ignore.paths (node_modules) and
        // hidden entries are excluded; nested content is not listed.
        assert_eq!(shallow, ["pkg/a.rs", "pkg/notes.txt", "pkg/sub"]);

        let tree = scan_tree(&root, &config).unwrap();
        let dirs: Vec<String> = tree.dirs.iter().map(rel).collect();
        assert!(
            dirs.contains(&"pkg/sub/empty".to_string()),
            "empty dirs are recorded"
        );
        assert!(!dirs.iter().any(|d| d.contains("__pycache__")));
        std::fs::remove_dir_all(&root).ok();
    }
}
