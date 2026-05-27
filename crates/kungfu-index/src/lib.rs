#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod indexer;
pub mod scanner;
pub mod watcher;

pub use indexer::{IndexStats, Indexer};
