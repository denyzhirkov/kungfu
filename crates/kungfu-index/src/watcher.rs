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

    let mut watcher =
        notify::recommended_watcher(move |res: Result<Event, notify::Error>| match res {
            Ok(event) => {
                if is_relevant_event(&event) {
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
            Ok(event) => {
                // Skip events in .kungfu directory
                let dominated_by_kungfu = event
                    .paths
                    .iter()
                    .all(|p| p.to_string_lossy().contains(".kungfu"));
                if dominated_by_kungfu {
                    continue;
                }

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
