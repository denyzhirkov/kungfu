//! The only network surface of kungfu: unauthenticated GETs against GitHub
//! Releases — the same endpoints `install.sh` already uses. Nothing is ever sent
//! but the request itself (no telemetry, no identifiers beyond the User-Agent).

use anyhow::{anyhow, Context, Result};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

pub const REPO: &str = "denyzhirkov/kungfu";

const USER_AGENT: &str = concat!("kungfu/", env!("CARGO_PKG_VERSION"));
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Refuse absurd payloads outright — the real binary is ~35MB.
const MAX_ASSET_BYTES: u64 = 300 * 1024 * 1024;

fn agent(read_timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(read_timeout)
        .user_agent(USER_AGENT)
        .build()
}

/// Latest published release tag, normalized to `MAJOR.MINOR.PATCH`.
pub fn latest_version(repo: &str, timeout: Duration) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let body = agent(timeout)
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("GET {url}"))?
        .into_string()
        .context("reading GitHub response")?;
    let value: serde_json::Value =
        serde_json::from_str(&body).context("parsing GitHub release JSON")?;
    let tag = value
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("no tag_name in the latest-release response for {repo}"))?;
    Ok(crate::version::normalize(tag))
}

pub fn release_asset_url(repo: &str, version: &str, asset: &str) -> String {
    format!("https://github.com/{repo}/releases/download/v{version}/{asset}")
}

/// `Ok(None)` for a 404 — releases published before checksums existed simply
/// have no `SHA256SUMS`, which is a documented degradation, not an error.
pub fn fetch_optional_text(url: &str, timeout: Duration) -> Result<Option<String>> {
    match agent(timeout).get(url).call() {
        Ok(resp) => Ok(Some(resp.into_string().context("reading response body")?)),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(anyhow::Error::new(e).context(format!("GET {url}"))),
    }
}

/// Stream an asset to `dest`. Returns the number of bytes written.
pub fn download_to(url: &str, dest: &Path, timeout: Duration) -> Result<u64> {
    let resp = agent(timeout)
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;

    if let Some(len) = resp
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
    {
        if len > MAX_ASSET_BYTES {
            return Err(anyhow!(
                "refusing to download {len} bytes from {url} (limit {MAX_ASSET_BYTES})"
            ));
        }
    }

    let mut file = std::fs::File::create(dest)
        .with_context(|| format!("failed to create {}", dest.display()))?;
    let mut reader = resp.into_reader().take(MAX_ASSET_BYTES);
    let written = std::io::copy(&mut reader, &mut file)
        .with_context(|| format!("failed to write {}", dest.display()))?;
    Ok(written)
}

/// Pick the hash for `asset` out of a `sha256sum`-style file (`<hex>  <name>`).
pub fn checksum_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == asset && hash.len() == 64).then(|| hash.to_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &str = "\
0000000000000000000000000000000000000000000000000000000000000001  kungfu-darwin-aarch64
0000000000000000000000000000000000000000000000000000000000000002 *kungfu-linux-x86_64
";

    #[test]
    fn finds_checksum_by_asset_name() {
        assert_eq!(
            checksum_for(SUMS, "kungfu-darwin-aarch64").as_deref(),
            Some("0000000000000000000000000000000000000000000000000000000000000001")
        );
    }

    #[test]
    fn tolerates_binary_mode_star_prefix() {
        assert!(checksum_for(SUMS, "kungfu-linux-x86_64").is_some());
    }

    #[test]
    fn missing_asset_yields_none() {
        assert!(checksum_for(SUMS, "kungfu-windows-x86_64.exe").is_none());
        assert!(checksum_for("garbage", "kungfu-darwin-aarch64").is_none());
    }

    #[test]
    fn asset_url_matches_release_layout() {
        assert_eq!(
            release_asset_url("owner/repo", "2.7.0", "kungfu-darwin-aarch64"),
            "https://github.com/owner/repo/releases/download/v2.7.0/kungfu-darwin-aarch64"
        );
    }
}
