#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod file_tags;
pub mod indexer;
pub mod scanner;
pub mod stoplist;
pub mod watcher;

pub use indexer::{IndexStats, Indexer};
pub use stoplist::{is_stoplisted_name, is_ubiquitous_callable};
