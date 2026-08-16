#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

pub type LineRange = (usize, usize);
pub type FileChangedLines = (String, Vec<LineRange>);

pub fn is_git_repo(root: &Path) -> bool {
    root.join(".git").exists()
}

pub fn changed_files(root: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .current_dir(root)
        .output()
        .context("failed to run git diff")?;

    if !output.status.success() {
        // Try without HEAD for initial commit
        let output = Command::new("git")
            .args(["diff", "--name-only"])
            .current_dir(root)
            .output()
            .context("failed to run git diff")?;

        let text = String::from_utf8_lossy(&output.stdout);
        return Ok(text
            .lines()
            .map(String::from)
            .filter(|s| !s.is_empty())
            .collect());
    }

    let text = String::from_utf8_lossy(&output.stdout);

    // Also get untracked files
    let untracked = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(root)
        .output()
        .context("failed to run git ls-files")?;

    let untracked_text = String::from_utf8_lossy(&untracked.stdout);

    let mut files: Vec<String> = text
        .lines()
        .chain(untracked_text.lines())
        .map(String::from)
        .filter(|s| !s.is_empty())
        .collect();

    files.sort();
    files.dedup();
    Ok(files)
}

/// Compact git log for a file: last N commits with date, author, message.
pub fn file_log(root: &Path, file_path: &str, max_entries: usize) -> Result<Vec<LogEntry>> {
    let output = Command::new("git")
        .args([
            "log",
            "--follow",
            &format!("-{}", max_entries),
            "--format=%H|%ai|%an|%s",
            "--",
            file_path,
        ])
        .current_dir(root)
        .output()
        .context("failed to run git log")?;

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(4, '|').collect();
            if parts.len() == 4 {
                Some(LogEntry {
                    hash: parts[0][..8].to_string(),
                    date: parts[1].to_string(),
                    author: parts[2].to_string(),
                    message: parts[3].to_string(),
                })
            } else {
                None
            }
        })
        .collect())
}

/// Git blame for a line range: who last changed each line.
pub fn blame_lines(
    root: &Path,
    file_path: &str,
    start_line: usize,
    end_line: usize,
) -> Result<Vec<BlameLine>> {
    let output = Command::new("git")
        .args([
            "blame",
            "--porcelain",
            &format!("-L{},{}", start_line, end_line),
            "--",
            file_path,
        ])
        .current_dir(root)
        .output()
        .context("failed to run git blame")?;

    let text = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();
    let mut current_author = String::new();
    let mut current_date = String::new();
    let mut current_hash = String::new();
    let mut current_summary = String::new();

    for line in text.lines() {
        if line.len() >= 40 && line.chars().take(40).all(|c| c.is_ascii_hexdigit()) {
            current_hash = line[..8].to_string();
        } else if let Some(author) = line.strip_prefix("author ") {
            current_author = author.to_string();
        } else if let Some(date) = line.strip_prefix("author-time ") {
            // Unix timestamp — convert to date string
            if let Ok(ts) = date.parse::<i64>() {
                current_date = format_timestamp(ts);
            }
        } else if let Some(summary) = line.strip_prefix("summary ") {
            current_summary = summary.to_string();
        } else if line.starts_with('\t') {
            // Content line — emit blame entry
            results.push(BlameLine {
                hash: current_hash.clone(),
                author: current_author.clone(),
                date: current_date.clone(),
                summary: current_summary.clone(),
            });
        }
    }

    // Deduplicate consecutive identical blame entries
    results.dedup_by(|a, b| a.hash == b.hash);
    Ok(results)
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub hash: String,
    pub date: String,
    pub author: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct BlameLine {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub summary: String,
}

fn format_timestamp(ts: i64) -> String {
    // Simple UTC date formatting without chrono
    let days = ts / 86400;
    let y = 1970 + (days * 4 + 2) / 1461; // rough year
    format!("{}", y)
}

/// Count how many commits touched each file (git churn).
pub fn file_commit_counts(root: &Path) -> Result<HashMap<String, usize>> {
    let output = Command::new("git")
        .args(["log", "--format=", "--name-only"])
        .current_dir(root)
        .output()
        .context("failed to run git log --name-only")?;

    let text = String::from_utf8_lossy(&output.stdout);
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in text.lines() {
        if !line.is_empty() {
            *counts.entry(line.to_string()).or_default() += 1;
        }
    }
    Ok(counts)
}

/// Find files that frequently change together (co-change analysis).
/// Returns pairs: for each file, the list of files that co-changed with it and how many times.
pub fn co_change_pairs(
    root: &Path,
    min_count: usize,
) -> Result<HashMap<String, Vec<(String, usize)>>> {
    let output = Command::new("git")
        .args(["log", "--format=format:COMMIT", "--name-only", "-n", "500"])
        .current_dir(root)
        .output()
        .context("failed to run git log for co-change")?;

    let text = String::from_utf8_lossy(&output.stdout);

    // Parse commits: group files per commit
    let mut commits: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for line in text.lines() {
        if line == "COMMIT" {
            if !current.is_empty() {
                commits.push(std::mem::take(&mut current));
            }
        } else if !line.is_empty() {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        commits.push(current);
    }

    // Count co-occurrences
    let mut pairs: HashMap<(String, String), usize> = HashMap::new();
    for files in &commits {
        if files.len() > 50 {
            continue; // skip huge commits (merges, bulk changes)
        }
        for i in 0..files.len() {
            for j in (i + 1)..files.len() {
                let a = &files[i];
                let b = &files[j];
                let key = if a < b {
                    (a.clone(), b.clone())
                } else {
                    (b.clone(), a.clone())
                };
                *pairs.entry(key).or_default() += 1;
            }
        }
    }

    // Build adjacency list
    let mut result: HashMap<String, Vec<(String, usize)>> = HashMap::new();
    for ((a, b), count) in pairs {
        if count >= min_count {
            result
                .entry(a.clone())
                .or_default()
                .push((b.clone(), count));
            result.entry(b).or_default().push((a, count));
        }
    }

    // Sort each list by count descending
    for v in result.values_mut() {
        v.sort_by(|a, b| b.1.cmp(&a.1));
    }

    Ok(result)
}

/// Get files changed in the current diff (staged + unstaged + untracked).
pub fn diff_files(root: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .current_dir(root)
        .output()
        .context("failed to run git diff")?;

    let text = String::from_utf8_lossy(&output.stdout);
    let staged = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(root)
        .output()
        .context("failed to run git diff --cached")?;
    let staged_text = String::from_utf8_lossy(&staged.stdout);

    let mut files: Vec<String> = text
        .lines()
        .chain(staged_text.lines())
        .map(String::from)
        .filter(|s| !s.is_empty())
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// Get symbols changed in git diff by parsing diff output for modified line ranges.
pub fn diff_changed_lines(root: &Path) -> Result<Vec<FileChangedLines>> {
    let output = Command::new("git")
        .args(["diff", "-U0", "HEAD"])
        .current_dir(root)
        .output()
        .context("failed to run git diff -U0")?;
    Ok(parse_unified_diff(&String::from_utf8_lossy(&output.stdout)))
}

/// Reject anything but a bare hex hash. `hash` lands as a positional arg to
/// `git show`; unvalidated, a value like `--output=...` is parsed as an
/// option instead of a revision, turning attacker-controlled commit data
/// (reachable via MCP history tools) into arbitrary file writes.
fn validate_hash(hash: &str) -> Result<()> {
    anyhow::ensure!(
        !hash.is_empty() && hash.len() <= 40 && hash.chars().all(|c| c.is_ascii_hexdigit()),
        "invalid commit hash: {hash}"
    );
    Ok(())
}

/// Files + changed line ranges introduced by a specific commit.
pub fn commit_changed_lines(root: &Path, hash: &str) -> Result<Vec<FileChangedLines>> {
    validate_hash(hash)?;
    let output = Command::new("git")
        .args(["show", "-U0", "--format=", hash])
        .current_dir(root)
        .output()
        .context("failed to run git show -U0")?;
    if !output.status.success() {
        anyhow::bail!(
            "git show failed for {}: {}",
            hash,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(parse_unified_diff(&String::from_utf8_lossy(&output.stdout)))
}

/// Files touched by a commit (any change).
pub fn commit_files(root: &Path, hash: &str) -> Result<Vec<String>> {
    validate_hash(hash)?;
    let output = Command::new("git")
        .args(["show", "--name-only", "--format=", hash])
        .current_dir(root)
        .output()
        .context("failed to run git show --name-only")?;
    if !output.status.success() {
        anyhow::bail!(
            "git show failed for {}: {}",
            hash,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(String::from)
        .filter(|s| !s.is_empty())
        .collect())
}

/// Commit metadata (hash, ISO date, author, subject).
pub fn commit_meta(root: &Path, hash: &str) -> Result<LogEntry> {
    validate_hash(hash)?;
    let output = Command::new("git")
        .args(["show", "-s", "--format=%H|%ai|%an|%s", hash])
        .current_dir(root)
        .output()
        .context("failed to run git show -s")?;
    if !output.status.success() {
        anyhow::bail!(
            "git show failed for {}: {}",
            hash,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().unwrap_or("");
    let parts: Vec<&str> = line.splitn(4, '|').collect();
    if parts.len() != 4 {
        anyhow::bail!("unexpected git show format for {}", hash);
    }
    Ok(LogEntry {
        hash: parts[0][..parts[0].len().min(8)].to_string(),
        date: parts[1].to_string(),
        author: parts[2].to_string(),
        message: parts[3].to_string(),
    })
}

/// Shared parser for `git diff -U0` / `git show -U0` output: collects per-file
/// (path, [(start, end)]) ranges from the `+++ b/` headers and `@@` hunks.
fn parse_unified_diff(text: &str) -> Vec<FileChangedLines> {
    let mut result: Vec<FileChangedLines> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_ranges: Vec<LineRange> = Vec::new();

    for line in text.lines() {
        if let Some(file_path) = line.strip_prefix("+++ b/") {
            if let Some(ref file) = current_file {
                if !current_ranges.is_empty() {
                    result.push((file.clone(), std::mem::take(&mut current_ranges)));
                }
            }
            current_file = Some(file_path.to_string());
            current_ranges.clear();
        } else if line.starts_with("@@ ") {
            // Parse @@ -old +new,count @@
            if let Some(plus_part) = line.split(' ').nth(2) {
                let plus_part = plus_part.trim_start_matches('+');
                let parts: Vec<&str> = plus_part.split(',').collect();
                if let Ok(start) = parts[0].parse::<usize>() {
                    let count = parts
                        .get(1)
                        .and_then(|c| c.parse::<usize>().ok())
                        .unwrap_or(1);
                    if count > 0 {
                        current_ranges.push((start, start + count.saturating_sub(1)));
                    }
                }
            }
        }
    }
    if let Some(file) = current_file {
        if !current_ranges.is_empty() {
            result.push((file, current_ranges));
        }
    }

    result
}

pub fn staged_files(root: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(root)
        .output()
        .context("failed to run git diff --cached")?;

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .map(String::from)
        .filter(|s| !s.is_empty())
        .collect())
}
