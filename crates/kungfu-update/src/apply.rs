//! Replacing the running binary in place.
//!
//! The swap is a same-directory `rename`, the same tmp+rename discipline the
//! index storage uses: either the old binary or the new one is visible, never a
//! half-written file. On Unix a running `kungfu mcp` keeps its old inode alive,
//! so an update can never break a session that is already in flight — but that
//! process also keeps running the OLD code until it is restarted, which is why
//! every success path carries a restart hint.

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::github;

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const METADATA_TIMEOUT: Duration = Duration::from_secs(10);

/// Release asset for the current platform. `None` on a platform we don't publish
/// binaries for (the user built from source and must update the same way).
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("kungfu-darwin-aarch64"),
        ("macos", "x86_64") => Some("kungfu-darwin-x86_64"),
        ("linux", "x86_64") => Some("kungfu-linux-x86_64"),
        ("linux", "aarch64") => Some("kungfu-linux-aarch64"),
        ("windows", "x86_64") => Some("kungfu-windows-x86_64.exe"),
        _ => None,
    }
}

/// Whether the new binary could be matched against a published checksum.
/// Surfaced in the output rather than silently assumed — an unverifiable
/// download is a weaker guarantee and the user gets to see that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checksum {
    Verified,
    Unavailable,
}

impl Checksum {
    pub fn as_str(&self) -> &'static str {
        match self {
            Checksum::Verified => "verified",
            Checksum::Unavailable => "unavailable (release has no SHA256SUMS)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Applied {
    pub from: String,
    pub to: String,
    pub path: PathBuf,
    pub checksum: Checksum,
}

/// The real file behind `kungfu` on PATH — symlinks resolved, so the installer's
/// `/usr/local/bin/kungfu -> ~/.local/bin/kungfu` link keeps pointing at the
/// replaced file instead of being overwritten with a regular file.
pub fn target_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("cannot locate the running kungfu binary")?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// Download release `version` and swap it in for the running binary.
pub fn apply(repo: &str, version: &str, current: &str) -> Result<Applied> {
    let asset = asset_name().ok_or_else(|| {
        anyhow!(
            "no published binary for {}-{}; update the way you installed (cargo build --release)",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;

    let target = target_path()?;
    let dir = target
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", target.display()))?
        .to_path_buf();
    ensure_writable(&dir, &target)?;
    cleanup_leftovers(&dir);

    let tmp = dir.join(format!(".kungfu-update-{}.tmp", std::process::id()));
    let guard = TmpGuard(tmp.clone());

    let url = github::release_asset_url(repo, version, asset);
    let bytes = github::download_to(&url, &tmp, DOWNLOAD_TIMEOUT)?;
    if bytes == 0 {
        bail!("downloaded an empty file from {url}");
    }

    let checksum = verify_checksum(repo, version, asset, &tmp)?;
    make_executable(&tmp)?;
    smoke_test(&tmp, version)?;
    swap(&tmp, &target)?;
    guard.disarm();

    Ok(Applied {
        from: current.to_string(),
        to: version.to_string(),
        path: target,
        checksum,
    })
}

/// Fail before downloading 35MB if the install location needs elevation.
fn ensure_writable(dir: &Path, target: &Path) -> Result<()> {
    let probe = dir.join(format!(".kungfu-write-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(anyhow!(
            "cannot write to {} ({e}) — kungfu lives at {}. Re-run with elevated permissions, \
             or reinstall: curl -fsSL https://raw.githubusercontent.com/{}/master/install.sh | sh",
            dir.display(),
            target.display(),
            github::REPO
        )),
    }
}

/// A previous Windows swap parks the old exe next to the new one; drop it once
/// nothing holds it open any more.
fn cleanup_leftovers(dir: &Path) {
    let _ = std::fs::remove_file(dir.join("kungfu.exe.old"));
}

fn verify_checksum(repo: &str, version: &str, asset: &str, file: &Path) -> Result<Checksum> {
    let url = github::release_asset_url(repo, version, "SHA256SUMS");
    let sums = match github::fetch_optional_text(&url, METADATA_TIMEOUT) {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(Checksum::Unavailable),
        Err(e) => {
            tracing::debug!("SHA256SUMS unreachable: {e:#}");
            return Ok(Checksum::Unavailable);
        }
    };
    let Some(expected) = github::checksum_for(&sums, asset) else {
        return Ok(Checksum::Unavailable);
    };
    let actual = sha256_file(file)?;
    if actual != expected {
        bail!("checksum mismatch for {asset}: expected {expected}, got {actual}");
    }
    Ok(Checksum::Verified)
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).context("failed to read for hashing")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("failed to chmod {}", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Run the freshly downloaded binary before trusting it. Catches a wrong-arch
/// asset, a truncated download and a proxy error page in one shot — things a
/// checksum alone cannot (older releases ship none).
fn smoke_test(path: &Path, expected_version: &str) -> Result<()> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .with_context(|| format!("downloaded binary at {} would not run", path.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !stdout.contains(expected_version) {
        bail!(
            "downloaded binary failed its self-check (expected version {expected_version}, got {:?})",
            stdout.trim()
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn swap(tmp: &Path, target: &Path) -> Result<()> {
    std::fs::rename(tmp, target).with_context(|| format!("failed to replace {}", target.display()))
}

/// Windows refuses to overwrite a running image, so the old exe is renamed out
/// of the way first and reaped on the next update.
#[cfg(windows)]
fn swap(tmp: &Path, target: &Path) -> Result<()> {
    let parked = target.with_file_name("kungfu.exe.old");
    let _ = std::fs::remove_file(&parked);
    std::fs::rename(target, &parked)
        .with_context(|| format!("failed to move {} aside", target.display()))?;
    match std::fs::rename(tmp, target) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Put the old binary back rather than leaving the user without one.
            let _ = std::fs::rename(&parked, target);
            Err(anyhow::Error::new(e).context(format!(
                "failed to install new binary at {}",
                target.display()
            )))
        }
    }
}

/// Removes the partial download unless the swap took ownership of it.
struct TmpGuard(PathBuf);

impl TmpGuard {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for TmpGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_name_covers_the_published_matrix() {
        // The published matrix is the release workflow's; on any platform we
        // build for, a name must exist and match the release naming scheme.
        if let Some(name) = asset_name() {
            assert!(name.starts_with("kungfu-"), "unexpected asset {name}");
        }
    }

    #[test]
    fn sha256_matches_known_vector() {
        let dir = std::env::temp_dir().join(format!("kungfu-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tmp_guard_removes_partial_download() {
        let dir = std::env::temp_dir().join(format!("kungfu-guard-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("partial.tmp");
        std::fs::write(&path, b"x").unwrap();
        drop(TmpGuard(path.clone()));
        assert!(!path.exists(), "guard must clean up on drop");

        std::fs::write(&path, b"x").unwrap();
        TmpGuard(path.clone()).disarm();
        assert!(path.exists(), "disarmed guard must leave the file alone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
