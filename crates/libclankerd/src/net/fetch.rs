//! gvproxy: a pinned GitHub release asset, sha256-verified and cached.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::digest::hex;

use crate::error::{Error, Result};

/// Pinned gvisor-tap-vsock release. `gvproxy-darwin` is a universal (x86_64 +
/// arm64) Mach-O; the sha256 is the asset digest GitHub reports and the
/// release's `sha256sums` file lists (checked when pinned).
pub const GVPROXY_VERSION: &str = "v0.8.9";
pub const GVPROXY_URL: &str =
    "https://github.com/containers/gvisor-tap-vsock/releases/download/v0.8.9/gvproxy-darwin";
pub const GVPROXY_SHA256: &str = "c6f7b4bc7f21bf810b5cf54e04d979b014c5d96472a03a9e97fe62a00940067c";

/// Downloads one pinned file into `<cache>/gvproxy/<version>/gvproxy`.
#[derive(Debug, Clone)]
pub struct GvproxyFetcher {
    cache_dir: PathBuf,
    version: String,
    url: String,
    sha256: String,
}

impl GvproxyFetcher {
    pub fn new(
        cache_dir: impl Into<PathBuf>,
        version: impl Into<String>,
        url: impl Into<String>,
        sha256: impl Into<String>,
    ) -> Self {
        GvproxyFetcher {
            cache_dir: cache_dir.into(),
            version: version.into(),
            url: url.into(),
            sha256: sha256.into().to_ascii_lowercase(),
        }
    }

    /// The release this build of libclankerd is pinned to.
    pub fn pinned(cache_dir: &Path) -> Self {
        Self::new(cache_dir, GVPROXY_VERSION, GVPROXY_URL, GVPROXY_SHA256)
    }

    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    fn target(&self) -> PathBuf {
        self.cache_dir
            .join("gvproxy")
            .join(&self.version)
            .join("gvproxy")
    }

    /// The verified binary, downloading it when missing or corrupt.
    pub fn ensure(&self) -> Result<PathBuf> {
        let target = self.target();
        if fs::read(&target).is_ok_and(|b| hex(&Sha256::digest(&b)) == self.sha256) {
            return Ok(target);
        }
        let dir = target.parent().expect("target has a parent");
        fs::create_dir_all(dir)?;
        let part = target.with_extension(format!("part{}", std::process::id()));
        let result = crate::images::block_on(self.download(&part)).and_then(|()| {
            fs::set_permissions(&part, fs::Permissions::from_mode(0o755))?;
            fs::rename(&part, &target)?;
            Ok(())
        });
        if let Err(e) = result {
            let _ = fs::remove_file(&part);
            // Leave no empty directories behind a failed first download.
            let _ = fs::remove_dir(dir);
            let _ = fs::remove_dir(dir.parent().expect("version dir has a parent"));
            return Err(e);
        }
        Ok(target)
    }

    async fn download(&self, part: &Path) -> Result<()> {
        let unavailable = |e: &dyn std::fmt::Display| {
            Error::unavailable(format!(
                "downloading gvproxy {} from {}: {e}",
                self.version, self.url
            ))
        };
        // ring, not aws-lc (see images.rs); fails harmlessly if installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut response = reqwest::get(&self.url)
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| unavailable(&e))?;
        let mut file = fs::File::create(part)?;
        let mut hasher = Sha256::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| unavailable(&e))? {
            hasher.update(&chunk);
            file.write_all(&chunk)?;
        }
        file.flush()?;
        let got = hex(&hasher.finalize());
        if got != self.sha256 {
            return Err(Error::unavailable(format!(
                "gvproxy {} from {} failed checksum verification: expected sha256 {}, got {got}",
                self.version, self.url, self.sha256
            )));
        }
        Ok(())
    }
}
