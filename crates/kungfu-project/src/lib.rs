#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod agent_health;
pub mod agent_init;
pub mod templates;

use anyhow::{bail, Context, Result};
use kungfu_config::KungfuConfig;
use kungfu_types::project::ProjectMeta;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::info;

pub const KUNGFU_DIR: &str = ".kungfu";
pub const CONFIG_FILE: &str = "config.toml";
pub const PROJECT_FILE: &str = "project.json";
pub const KUNGFU_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Project {
    pub root: PathBuf,
    pub kungfu_dir: PathBuf,
    pub config: KungfuConfig,
    pub meta: ProjectMeta,
}

impl Project {
    pub fn open(start_dir: &Path) -> Result<Self> {
        let root = find_project_root(start_dir)?;
        let kungfu_dir = root.join(KUNGFU_DIR);

        if !kungfu_dir.exists() {
            bail!("not a kungfu project (no .kungfu directory found). Run 'kungfu init' first.");
        }
        if !kungfu_dir.join(PROJECT_FILE).is_file() {
            bail!(
                "{} is partially initialized ({} is missing). Run 'kungfu init' at {} to complete it; existing memory and annotations are kept.",
                kungfu_dir.display(),
                PROJECT_FILE,
                root.display()
            );
        }

        let config_path = kungfu_dir.join(CONFIG_FILE);
        let config = KungfuConfig::load_merged(Some(&config_path))?;

        let meta_path = kungfu_dir.join(PROJECT_FILE);
        let meta_content =
            std::fs::read_to_string(&meta_path).with_context(|| "failed to read project.json")?;
        let meta: ProjectMeta =
            serde_json::from_str(&meta_content).with_context(|| "failed to parse project.json")?;

        Ok(Self {
            root,
            kungfu_dir,
            config,
            meta,
        })
    }

    pub fn index_dir(&self) -> PathBuf {
        self.kungfu_dir.join("index")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.kungfu_dir.join("cache")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.kungfu_dir.join("logs")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.kungfu_dir.join("state")
    }
}

/// `.kungfu/project.json` is the initialization marker.
pub fn is_initialized(root: &Path) -> bool {
    root.join(KUNGFU_DIR).join(PROJECT_FILE).is_file()
}

/// `.kungfu/` exists without `project.json`: a fresh clone that tracks
/// `.kungfu/memory/`, a crashed init, a partial copy.
pub fn is_partial(root: &Path) -> bool {
    root.join(KUNGFU_DIR).is_dir() && !is_initialized(root)
}

/// Rebuildable state that may be foreign in a partial `.kungfu/` (copied from
/// another checkout). Only these known paths are ever removed; embeddings live
/// under `index/`, downloaded `models/` are kept.
const DERIVED_DIRS: [&str; 3] = ["index", "cache", "state"];

/// Existing derived dirs `init_project` would remove from a partial `.kungfu/`.
pub fn stale_derived_dirs(root: &Path) -> Vec<PathBuf> {
    if !is_partial(root) {
        return Vec::new();
    }
    DERIVED_DIRS
        .iter()
        .map(|n| root.join(KUNGFU_DIR).join(n))
        .filter(|d| d.is_dir())
        .collect()
}

const INIT_LOCK: &str = "init.lock";
const LOCK_STALE_AFTER: Duration = Duration::from_secs(300);
const LOCK_WAIT: Duration = Duration::from_secs(10);

/// Exclusive marker held while init writes into `.kungfu/`, so two sessions
/// auto-initializing a fresh clone do not delete each other's work.
struct InitLock(PathBuf);

impl Drop for InitLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `Ok(None)` means another process finished initializing while we waited.
fn acquire_init_lock(root: &Path, kungfu_dir: &Path) -> Result<Option<InitLock>> {
    std::fs::create_dir_all(kungfu_dir)?;
    let path = kungfu_dir.join(INIT_LOCK);
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(Some(InitLock(path))),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_initialized(root) {
                    return Ok(None);
                }
                let stale = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > LOCK_STALE_AFTER);
                if stale {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                if Instant::now() >= deadline {
                    bail!(
                        "another kungfu init holds {} (remove it if no init is running)",
                        path.display()
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Initializes `root`, or completes a partial `.kungfu/` in place. User data
/// (memory/, annotations.json, config.toml) and unknown files are kept; the
/// derived dirs in [`DERIVED_DIRS`] are dropped and rebuilt on first use.
pub fn init_project(root: &Path) -> Result<Project> {
    let kungfu_dir = root.join(KUNGFU_DIR);

    if is_initialized(root) {
        bail!("kungfu is already initialized in {}", root.display());
    }

    let was_partial = kungfu_dir.is_dir();
    let Some(_lock) = acquire_init_lock(root, &kungfu_dir)? else {
        return Project::open(root);
    };
    if is_initialized(root) {
        return Project::open(root);
    }

    if was_partial {
        for name in DERIVED_DIRS {
            let dir = kungfu_dir.join(name);
            if dir.is_dir() {
                std::fs::remove_dir_all(&dir)
                    .with_context(|| format!("failed to remove stale {}", dir.display()))?;
                info!("removed stale derived dir {}", dir.display());
            }
        }
    }

    // Create directory structure
    std::fs::create_dir_all(kungfu_dir.join("index"))?;
    std::fs::create_dir_all(kungfu_dir.join("cache").join("summaries"))?;
    std::fs::create_dir_all(kungfu_dir.join("cache").join("queries"))?;
    std::fs::create_dir_all(kungfu_dir.join("logs"))?;
    std::fs::create_dir_all(kungfu_dir.join("state"))?;

    // Detect project name from directory
    let project_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed")
        .to_string();

    // Create config
    let config_path = kungfu_dir.join(CONFIG_FILE);
    let config = if config_path.is_file() {
        KungfuConfig::load_merged(Some(&config_path))?
    } else {
        let config = KungfuConfig::default().with_project_name(&project_name);
        config.save(&config_path)?;
        config
    };

    // Create project metadata
    let meta = ProjectMeta {
        name: project_name,
        root: root.to_string_lossy().to_string(),
        created_at: chrono::Utc::now(),
        kungfu_version: KUNGFU_VERSION.to_string(),
    };
    let meta_json = serde_json::to_string_pretty(&meta)?;
    // project.json is the marker, so it lands last and atomically.
    let tmp_path = kungfu_dir.join(format!("{PROJECT_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp_path, meta_json)?;
    std::fs::rename(&tmp_path, kungfu_dir.join(PROJECT_FILE))?;

    info!("initialized kungfu project at {}", root.display());

    Ok(Project {
        root: root.to_path_buf(),
        kungfu_dir,
        config,
        meta,
    })
}

const ROOT_MARKERS: [&str; 7] = [
    ".git",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "Makefile",
];

/// Nearest initialized `.kungfu/` (one with `project.json`; a partial dir does
/// not count) that belongs to the same repository as `start`.
///
/// Inside a git repo (some ancestor, inclusive, has `.git` as a file or
/// directory) the walk goes up to that repository boundary, inclusive, and
/// never beyond it, skipping project markers on the way (sub-packages).
/// Outside any git repo there is no boundary to trust, so the walk stops at
/// the first project marker: a stray `.kungfu/` above a marker project is not
/// picked up.
pub fn find_indexed_root(start: &Path) -> Option<PathBuf> {
    walk_to_boundary(start, is_initialized)
}

fn walk_to_boundary(start: &Path, has_dir: fn(&Path) -> bool) -> Option<PathBuf> {
    if start.ancestors().any(|d| d.join(".git").exists()) {
        for dir in start.ancestors() {
            if has_dir(dir) {
                return Some(dir.to_path_buf());
            }
            if dir.join(".git").exists() {
                return None;
            }
        }
        return None;
    }
    for dir in start.ancestors() {
        if has_dir(dir) {
            return Some(dir.to_path_buf());
        }
        if ROOT_MARKERS.iter().any(|m| dir.join(m).exists()) {
            return None;
        }
    }
    None
}

/// Nearest partial `.kungfu/` (no `project.json`) under the same boundary
/// rule as [`find_indexed_root`], so a partial repo root stays visible from
/// sub-packages.
pub fn find_partial_root(start: &Path) -> Option<PathBuf> {
    walk_to_boundary(start, is_partial)
}

/// Project root for `start`: the indexed root if one exists within the
/// repository, else a partial root (so `Project::open` can say so), else the
/// nearest project-marker directory (see [`find_marker_root`]).
pub fn find_project_root(start_dir: &Path) -> Result<PathBuf> {
    match find_indexed_root(start_dir).or_else(|| find_partial_root(start_dir)) {
        Some(root) => Ok(root),
        None => find_marker_root(start_dir),
    }
}

/// Walk up from start_dir to the first directory with `.kungfu` or a project
/// marker (.git, Cargo.toml, ...). This is where `kungfu init` creates `.kungfu/`.
pub fn find_marker_root(start_dir: &Path) -> Result<PathBuf> {
    let mut current = start_dir.to_path_buf();

    loop {
        // Check for .kungfu directory first
        if current.join(KUNGFU_DIR).exists() {
            return Ok(current);
        }

        for marker in &ROOT_MARKERS {
            if current.join(marker).exists() {
                return Ok(current);
            }
        }

        if !current.pop() {
            break;
        }
    }

    // Fallback to start_dir
    Ok(start_dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kungfu-project-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    fn mark_initialized(dir: &Path) {
        std::fs::create_dir_all(dir.join(".kungfu")).unwrap();
        std::fs::write(dir.join(".kungfu").join(PROJECT_FILE), "{}").unwrap();
    }

    fn sub_package(ws: &Path) -> PathBuf {
        let pkg = ws.join("crates/foo");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("Cargo.toml"), "[package]\n").unwrap();
        pkg
    }

    #[test]
    fn sub_package_resolves_to_indexed_root() {
        let ws = temp_dir("subpkg");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        mark_initialized(&ws);
        let pkg = sub_package(&ws);

        assert_eq!(find_indexed_root(&pkg), Some(ws.clone()));
        assert_eq!(find_project_root(&pkg).unwrap(), ws);
    }

    #[test]
    fn kungfu_above_git_boundary_is_ignored() {
        let outer = temp_dir("boundary");
        mark_initialized(&outer);
        let repo = outer.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let pkg = sub_package(&repo);

        assert_eq!(find_indexed_root(&pkg), None);
        assert_eq!(find_project_root(&pkg).unwrap(), pkg);
    }

    #[test]
    fn git_file_is_a_boundary() {
        let outer = temp_dir("gitfile");
        mark_initialized(&outer);
        let sub = outer.join("submodule");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();

        assert_eq!(find_indexed_root(&sub), None);
    }

    #[test]
    fn no_kungfu_keeps_marker_behavior() {
        let ws = temp_dir("nomarker");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        let pkg = sub_package(&ws);

        assert_eq!(find_indexed_root(&pkg), None);
        assert_eq!(find_project_root(&pkg).unwrap(), pkg);
    }

    #[test]
    fn kungfu_above_non_git_marker_project_is_ignored() {
        let outer = temp_dir("nongit");
        std::fs::remove_dir_all(outer.join(".git")).unwrap();
        mark_initialized(&outer);
        let proj = outer.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\n").unwrap();

        // The temp dir itself may sit inside a git checkout; only assert when it does not.
        if !outer.ancestors().any(|d| d.join(".git").exists()) {
            assert_eq!(find_indexed_root(&proj), None);
            assert_eq!(find_project_root(&proj).unwrap(), proj);
        }
    }

    #[test]
    fn non_git_project_with_own_kungfu_is_found() {
        let proj = temp_dir("nongit-own");
        std::fs::remove_dir_all(proj.join(".git")).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\n").unwrap();
        mark_initialized(&proj);

        if !proj.ancestors().any(|d| d.join(".git").exists()) {
            assert_eq!(find_indexed_root(&proj), Some(proj.clone()));
        }
    }

    #[test]
    fn partial_dir_is_not_an_indexed_root() {
        let ws = temp_dir("partial-root");
        std::fs::create_dir_all(ws.join(".kungfu/memory")).unwrap();
        let pkg = sub_package(&ws);

        assert!(is_partial(&ws));
        assert_eq!(find_indexed_root(&ws), None);
        assert_eq!(find_indexed_root(&pkg), None);
        let err = Project::open(&ws).err().unwrap().to_string();
        assert!(err.contains("partially initialized"), "{err}");
    }

    #[test]
    fn partial_root_is_visible_from_sub_package() {
        let ws = temp_dir("partial-sub");
        std::fs::write(ws.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::create_dir_all(ws.join(".kungfu/memory")).unwrap();
        let pkg = sub_package(&ws);

        assert_eq!(find_indexed_root(&pkg), None);
        assert_eq!(find_partial_root(&pkg), Some(ws.clone()));
        assert_eq!(find_project_root(&pkg).unwrap(), ws);
        let err = Project::open(&pkg).err().unwrap().to_string();
        assert!(err.contains("partially initialized"), "{err}");
        assert!(err.contains(&ws.display().to_string()), "{err}");
    }

    #[test]
    fn stale_lock_is_taken_over_and_removed_after_init() {
        let ws = temp_dir("lock");
        let kd = ws.join(".kungfu");
        std::fs::create_dir_all(&kd).unwrap();
        let lock = kd.join(INIT_LOCK);
        let f = std::fs::File::create(&lock).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
            .unwrap();

        init_project(&ws).unwrap();
        assert!(!lock.exists());
        assert!(is_initialized(&ws));
    }

    #[test]
    fn init_completes_partial_dir_and_keeps_data() {
        let ws = temp_dir("partial-init");
        let kd = ws.join(".kungfu");
        std::fs::create_dir_all(kd.join("memory")).unwrap();
        std::fs::write(kd.join("memory/mem_0001.md"), "keep").unwrap();
        std::fs::write(kd.join("annotations.json"), "{}").unwrap();
        std::fs::write(kd.join("config.toml"), "# mine\n").unwrap();
        std::fs::write(kd.join("notes.txt"), "unknown").unwrap();
        std::fs::create_dir_all(kd.join("index")).unwrap();
        std::fs::write(kd.join("index/symbols.json"), "foreign").unwrap();

        let project = init_project(&ws).unwrap();
        assert_eq!(project.root, ws);
        assert!(is_initialized(&ws) && !is_partial(&ws));
        assert_eq!(
            std::fs::read_to_string(kd.join("memory/mem_0001.md")).unwrap(),
            "keep"
        );
        assert!(kd.join("annotations.json").is_file());
        assert!(kd.join("index").is_dir());
        assert!(!kd.join("index/symbols.json").exists());
        assert_eq!(
            std::fs::read_to_string(kd.join("config.toml")).unwrap(),
            "# mine\n"
        );
        assert!(kd.join("notes.txt").is_file());
        assert_eq!(find_indexed_root(&ws), Some(ws.clone()));
        assert!(Project::open(&ws).is_ok());
    }

    #[test]
    fn init_refuses_initialized_dir() {
        let ws = temp_dir("already");
        init_project(&ws).unwrap();
        let err = init_project(&ws).err().unwrap().to_string();
        assert!(err.contains("already initialized"), "{err}");
    }
}
