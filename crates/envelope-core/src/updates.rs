//! Whether a newer Envelope is out: one request to GitHub's public API for
//! the latest release. Nothing about the user or their mail is sent.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::Result;

const LATEST: &str = "https://api.github.com/repos/burakgon/envelope/releases/latest";

/// A release newer than the running version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    /// Without the `v`: `0.5.0`.
    pub version: String,
    /// Its page, with the notes and the downloads.
    pub url: String,
}

#[derive(Deserialize)]
struct Latest {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// The latest release when it is newer than `current`.
pub async fn newer_release(current: &str) -> Result<Option<Release>> {
    let latest: Latest = reqwest::Client::builder()
        .user_agent(concat!("Envelope/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()?
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let version = latest.tag_name.trim_start_matches('v').to_string();
    Ok(
        (!latest.draft && !latest.prerelease && is_newer(&version, current)).then_some(Release {
            version,
            url: latest.html_url,
        }),
    )
}

/// Whether version `a` comes after `b` (`major.minor.patch`; a missing part
/// counts as 0, and anything after a `-` or `+` is ignored).
pub fn is_newer(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Option<[u64; 3]> {
        let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
        let mut out = [0; 3];
        for (i, p) in core.split('.').enumerate() {
            *out.get_mut(i)? = p.parse().ok()?;
        }
        Some(out)
    }
    matches!((parts(a), parts(b)), (Some(a), Some(b)) if a > b)
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.5.0", "0.4.0"));
        assert!(is_newer("0.10.0", "0.9.3"));
        assert!(is_newer("v1.0", "0.99.99"));
        assert!(is_newer("0.4.1", "0.4.0"));
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"));
        assert!(!is_newer("0.4.0-beta.1", "0.4.0"));
        assert!(!is_newer("nightly", "0.4.0"));
        assert!(!is_newer("1.2.3.4", "0.4.0"));
    }
}
