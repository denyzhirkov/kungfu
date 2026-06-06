use anyhow::Result;
use kungfu_config::KungfuConfig;
use kungfu_storage::JsonStore;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::{IndexStats, Indexer};

pub fn watch_and_index(
    root: &Path,
    config: KungfuConfig,
    index_dir: &Path,
    on_reindex: impl Fn(&IndexStats),
) -> Result<()> {
    info!("watching {} for changes...", root.display());

    let (tx, rx) = mpsc::channel();

    // notify does NOT honour .gitignore — a recursive watch on the project root delivers
    // events for ignored trees too (target/, node_modules/, .git/, …). A large, churning
    // build dir would otherwise flood the unbounded channel and trigger a re-index storm
    // that wedges the server. Drop ignored-path events here in the callback, before they
    // ever reach the channel.
    let ignore_names = config.ignore.paths.clone();

    let mut watcher =
        notify::recommended_watcher(move |res: Result<Event, notify::Error>| match res {
            Ok(event) => {
                if is_relevant_event(&event) && !event_all_ignored(&event, &ignore_names) {
                    let _ = tx.send(event);
                }
            }
            Err(e) => warn!("watch error: {}", e),
        })?;

    watcher.watch(root, RecursiveMode::Recursive)?;

    let debounce = Duration::from_millis(500);
    let mut last_index = Instant::now() - debounce;

    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(_) => {
                let now = Instant::now();
                if now.duration_since(last_index) < debounce {
                    // Drain remaining events in debounce window
                    while rx.recv_timeout(Duration::from_millis(100)).is_ok() {}
                }

                debug!("change detected, re-indexing...");
                let store = JsonStore::new(index_dir);
                let mut indexer = Indexer::new(root, config.clone(), &store);
                match indexer.index_incremental() {
                    Ok(stats) => {
                        if stats.new_files > 0 || stats.changed_files > 0 || stats.removed_files > 0
                        {
                            on_reindex(&stats);
                        }
                    }
                    Err(e) => warn!("re-index failed: {}", e),
                }
                last_index = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}

fn is_relevant_event(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// True when every path in the event lies under an ignored directory, so the event can be
/// dropped. Mirrors the scanner's ignore set (`config.ignore.paths`: target, node_modules,
/// .git, .kungfu, …) by matching directory-name components.
fn event_all_ignored(event: &Event, ignore_names: &[String]) -> bool {
    !event.paths.is_empty()
        && event
            .paths
            .iter()
            .all(|p| path_has_ignored_component(p, ignore_names))
}

fn path_has_ignored_component(path: &Path, ignore_names: &[String]) -> bool {
    path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        ignore_names.iter().any(|n| n.as_str() == name)
    })
}
