#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use serde::Serialize;

pub enum OutputMode {
    Pretty,
    Json,
}

pub fn print_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("serialization error: {}", e))
}

pub fn print_result<T: Serialize + std::fmt::Display>(value: &T, mode: &OutputMode) {
    match mode {
        OutputMode::Pretty => println!("{}", value),
        OutputMode::Json => println!("{}", print_json(value)),
    }
}
