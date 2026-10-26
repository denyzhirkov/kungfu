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

/// Run a git query and return stdout; any failure is reported, never
/// swallowed (a silent empty result would pass for a clean tree).
fn git_query(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim())
}

/// What the working tree is diffed against. A repository without commits has
/// no `HEAD`; the base is then the empty tree, so every staged file shows up
/// as added instead of the diff failing or passing for clean.
#[derive(Debug, Clone)]
pub struct DiffBase {
    rev: String,
    pub has_commits: bool,
}

impl DiffBase {
    /// Revision to pass to `git diff`.
    pub fn rev(&self) -> &str {
        &self.rev
    }
}

/// Resolve the diff base for `root`. HEAD is detected by exit status, not by
/// matching git's (localised) error text.
pub fn diff_base(root: &Path) -> Result<DiffBase> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .current_dir(root)
        .output()
        .context("failed to run git rev-parse")?;
    if output.status.success() {
        return Ok(DiffBase {
            rev: "HEAD".to_string(),
            has_commits: true,
        });
    }
    // `--verify -q` turns an unresolvable ref into a silent exit 1; anything
    // louder (not a repository, corrupt object store) is a real failure.
    if output.status.code() != Some(1) || !output.stderr.is_empty() {
        anyhow::bail!(
            "git rev-parse HEAD failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // The empty tree's id depends on the repository's hash algorithm
    // (SHA-1: 4b825dc…, SHA-256 differs), so ask git rather than hard-code it.
    // Hash empty input as a tree without `-w`: read-only, nothing is written to
    // the user's object store. `output()` gives the child a closed stdin.
    let out = git_query(root, &["hash-object", "-t", "tree", "--stdin"])?;
    let rev = String::from_utf8_lossy(&out).trim().to_string();
    anyhow::ensure!(
        !rev.is_empty() && rev.chars().all(|c| c.is_ascii_hexdigit()),
        "git hash-object returned no empty-tree id in {}",
        root.display()
    );
    Ok(DiffBase {
        rev,
        has_commits: false,
    })
}

/// Get symbols changed in git diff by parsing diff output for modified line ranges.
/// Paths are relative to `root` and limited to it (`--relative`), so a project
/// nested inside a larger repository sees only its own files. Renames are
/// reported as delete + add (`--no-renames`) so the new path carries hunks.
pub fn diff_changed_lines(root: &Path, base: &DiffBase) -> Result<Vec<FileChangedLines>> {
    let out = git_query(
        root,
        &[
            "diff",
            "-U0",
            "--relative",
            "--no-renames",
            base.rev(),
            "--",
        ],
    )?;
    Ok(parse_unified_diff(&String::from_utf8_lossy(&out)))
}

/// Tracked files deleted relative to the base (staged or not).
/// `diff_changed_lines` cannot see them: a deletion has no added lines.
pub fn deleted_files(root: &Path, base: &DiffBase) -> Result<Vec<String>> {
    diff_names(root, base, Some("--diff-filter=D"))
}

/// Every tracked file that differs from the base in any way: content, mode
/// bits, binary payload or deletion. Binary and mode-only changes carry no
/// `@@` hunks, so this is the only place they show up.
pub fn changed_paths(root: &Path, base: &DiffBase) -> Result<Vec<String>> {
    diff_names(root, base, None)
}

fn diff_names(root: &Path, base: &DiffBase, filter: Option<&str>) -> Result<Vec<String>> {
    let mut args = vec!["diff", "--name-only", "-z", "--relative", "--no-renames"];
    args.extend(filter);
    args.extend([base.rev(), "--"]);
    Ok(split_nul(&git_query(root, &args)?))
}

/// New files git does not track yet (ignored ones and kungfu's own `.kungfu`
/// state excluded, at any depth). `git diff HEAD` never lists these.
pub fn untracked_files(root: &Path) -> Result<Vec<String>> {
    let out = git_query(
        root,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
            ":(exclude).kungfu",
            ":(exclude)**/.kungfu/**",
        ],
    )?;
    Ok(split_nul(&out))
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    let mut files: Vec<String> = bytes
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    files.sort();
    files
}

/// Reject anything but a bare hex hash. `hash` lands as a positional arg to
/// `git show`; unvalidated, a value like `--output=...` is parsed as an
/// option instead of a revision, turning attacker-controlled commit data
/// (reachable via MCP history tools) into arbitrary file writes.
///
/// Bounds: 4 is git's minimum abbreviation; 64 covers SHA-256 repositories.
/// Hex-only also rules out a leading `-`, so no `--end-of-options` is needed
/// (that flag would break git < 2.24 for no extra safety).
fn validate_hash(hash: &str) -> Result<()> {
    anyhow::ensure!(
        (4..=64).contains(&hash.len()) && hash.chars().all(|c| c.is_ascii_hexdigit()),
        "invalid commit hash `{hash}`: expected 4-64 hex characters (a full or abbreviated commit SHA)"
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
/// A hunk that only removes lines (`+N,0`) is recorded as the single line N so
/// the file is not lost; deleted files (`+++ /dev/null`) carry no hunks.
fn parse_unified_diff(text: &str) -> Vec<FileChangedLines> {
    let mut result: Vec<FileChangedLines> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_ranges: Vec<LineRange> = Vec::new();

    for line in text.lines() {
        if let Some(header) = line.strip_prefix("+++ ") {
            if let Some(file) = current_file.take() {
                if !current_ranges.is_empty() {
                    result.push((file, std::mem::take(&mut current_ranges)));
                }
            }
            current_ranges.clear();
            current_file = header.strip_prefix("b/").map(String::from);
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
                        current_ranges.push((start, start + count - 1));
                    } else {
                        let at = start.max(1);
                        current_ranges.push((at, at));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn validate_hash_accepts_full_and_abbreviated_shas() {
        assert!(validate_hash("7558d1d").is_ok());
        assert!(validate_hash("7558D1DA").is_ok());
        assert!(validate_hash(&"a".repeat(40)).is_ok());
        assert!(validate_hash(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn validate_hash_rejects_options_refs_and_bad_lengths() {
        for bad in [
            "",
            "abc",
            &"a".repeat(65),
            "--output=/tmp/x",
            "-p",
            "HEAD",
            "HEAD~1",
            "main",
            "7558d1d -- x",
        ] {
            assert!(validate_hash(bad).is_err(), "accepted {bad:?}");
        }
    }

    /// Throwaway repo with one commit under the system temp dir.
    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kungfu-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);
        dir
    }

    #[test]
    fn option_shaped_hash_never_reaches_git() {
        let repo = temp_repo("inject");
        let target = repo.join("pwned.txt");
        let hash = format!("--output={}", target.display());

        assert!(commit_meta(&repo, &hash).is_err());
        assert!(commit_files(&repo, &hash).is_err());
        assert!(commit_changed_lines(&repo, &hash).is_err());
        assert!(!target.exists(), "git show wrote {}", target.display());

        let _ = std::fs::remove_dir_all(&repo);
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn base(dir: &Path) -> DiffBase {
        diff_base(dir).unwrap()
    }

    fn write(repo: &Path, rel: &str, body: &str) {
        let path = repo.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn commit_all(repo: &Path) {
        git_in(repo, &["add", "."]);
        git_in(repo, &["commit", "-q", "-m", "files"]);
    }

    #[test]
    fn deleted_and_untracked_files_are_listed() {
        let repo = temp_repo("deleted");
        write(&repo, "gone.rs", "fn a() {}\n");
        write(&repo, "kept.rs", "fn b() {}\n");
        commit_all(&repo);
        std::fs::remove_file(repo.join("gone.rs")).unwrap();
        write(&repo, "fresh.rs", "fn c() {}\n");

        assert_eq!(deleted_files(&repo, &base(&repo)).unwrap(), vec!["gone.rs"]);
        assert_eq!(untracked_files(&repo).unwrap(), vec!["fresh.rs"]);
        assert!(diff_changed_lines(&repo, &base(&repo)).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn untracked_files_skip_kungfu_state_at_any_depth() {
        let repo = temp_repo("state");
        write(&repo, ".kungfu/index/files.json", "[]");
        write(&repo, "sub/.kungfu/index/files.json", "[]");
        write(&repo, "sub/real.rs", "fn r() {}\n");

        assert_eq!(untracked_files(&repo).unwrap(), vec!["sub/real.rs"]);

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn removed_lines_only_keep_the_file_in_the_diff() {
        let repo = temp_repo("rmlines");
        write(&repo, "a.rs", "one\ntwo\nthree\n");
        commit_all(&repo);
        write(&repo, "a.rs", "one\nthree\n");

        let lines = diff_changed_lines(&repo, &base(&repo)).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "a.rs");

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn staged_rename_is_a_delete_plus_an_add() {
        let repo = temp_repo("rename");
        write(&repo, "old.rs", "fn a() {}\nfn b() {}\n");
        commit_all(&repo);
        git_in(&repo, &["mv", "old.rs", "new.rs"]);

        assert_eq!(deleted_files(&repo, &base(&repo)).unwrap(), vec!["old.rs"]);
        let lines = diff_changed_lines(&repo, &base(&repo)).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "new.rs");

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn nested_project_sees_only_its_own_files_root_relative() {
        let repo = temp_repo("nested");
        write(&repo, "sub/a.rs", "fn a() {}\n");
        write(&repo, "sub/gone.rs", "fn g() {}\n");
        write(&repo, "outside.rs", "fn o() {}\n");
        commit_all(&repo);
        write(&repo, "sub/a.rs", "fn a() { 1; }\n");
        write(&repo, "outside.rs", "fn o() { 1; }\n");
        std::fs::remove_file(repo.join("sub/gone.rs")).unwrap();
        write(&repo, "sub/new.rs", "fn n() {}\n");
        let sub = repo.join("sub");

        let lines = diff_changed_lines(&sub, &base(&sub)).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "a.rs");
        assert_eq!(deleted_files(&sub, &base(&sub)).unwrap(), vec!["gone.rs"]);
        assert_eq!(untracked_files(&sub).unwrap(), vec!["new.rs"]);

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Repository without commits: staged files diff against the empty tree,
    /// so they are reported as added; a directory that is not a repository is
    /// an error, not an empty diff.
    #[test]
    fn repo_without_commits_diffs_against_the_empty_tree() {
        let dir = std::env::temp_dir().join(format!("kungfu-git-nohead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git_in(&dir, &["init", "-q"]);
        write(&dir, "staged.rs", "fn s() {}\n");
        write(&dir, "loose.rs", "fn l() {}\n");
        git_in(&dir, &["add", "staged.rs"]);

        let b = base(&dir);
        assert!(!b.has_commits);
        assert_eq!(b.rev(), "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
        assert!(deleted_files(&dir, &b).unwrap().is_empty());
        let lines = diff_changed_lines(&dir, &b).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "staged.rs");
        assert_eq!(changed_paths(&dir, &b).unwrap(), vec!["staged.rs"]);
        assert_eq!(untracked_files(&dir).unwrap(), vec!["loose.rs"]);

        let not_a_repo = dir.join("plain");
        std::fs::create_dir_all(&not_a_repo).unwrap();
        std::fs::remove_dir_all(dir.join(".git")).unwrap();
        assert!(diff_base(&not_a_repo).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binary_and_mode_only_changes_are_changed_paths_without_hunks() {
        let repo = temp_repo("binary");
        std::fs::write(repo.join("logo.png"), [0x89, b'P', b'N', b'G', 0, 1, 2]).unwrap();
        write(&repo, "run.sh", "#!/bin/sh\n");
        commit_all(&repo);
        std::fs::write(repo.join("logo.png"), [0x89, b'P', b'N', b'G', 9, 9, 9]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(repo.join("run.sh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        let b = base(&repo);
        let mut expected = vec!["logo.png"];
        if cfg!(unix) {
            expected.push("run.sh");
        }
        assert_eq!(changed_paths(&repo, &b).unwrap(), expected);
        assert!(diff_changed_lines(&repo, &b).unwrap().is_empty());
        assert!(deleted_files(&repo, &b).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn real_hash_still_resolves() {
        let repo = temp_repo("real");
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let head = String::from_utf8_lossy(&out.stdout).trim().to_string();

        let meta = commit_meta(&repo, &head[..7]).unwrap();
        assert_eq!(meta.message, "init");

        let _ = std::fs::remove_dir_all(&repo);
    }
}
