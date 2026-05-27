use anyhow::Result;
use ignore::WalkBuilder;
use kungfu_config::KungfuConfig;
use std::path::{Path, PathBuf};
use tracing::debug;

pub fn scan_files(root: &Path, config: &KungfuConfig) -> Result<Vec<PathBuf>> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!config.index_hidden)
        .follow_links(config.follow_symlinks)
        .git_ignore(true)
        .git_global(true);

    // Add custom ignore paths
    let mut overrides = ignore::overrides::OverrideBuilder::new(root);
    for path in &config.ignore.paths {
        overrides.add(&format!("!{}", path))?;
    }
    builder.overrides(overrides.build()?);

    let mut files = Vec::new();
    for entry in builder.build() {
        let entry = entry?;
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            let path = entry.path().to_path_buf();
            // Filter by enabled language extensions
            if should_index(&path, config) {
                files.push(path);
            }
        }
    }

    debug!("scanned {} files", files.len());
    Ok(files)
}

fn should_index(path: &Path, config: &KungfuConfig) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let lang = kungfu_types::file::Language::from_extension(ext);
    let lang_str = lang.to_string();

    config.languages.enabled.iter().any(|l| l == &lang_str)
        || lang == kungfu_types::file::Language::Unknown
}
