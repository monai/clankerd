//! The local image cache: pulling OCI images (linux/arm64) into a blob cache,
//! listing them, and resolving references to digests.
//!
//! Layout under the cache directory (all named under `clankerd`):
//!
//! * `blobs/sha256/<hex>`: layer blobs, exactly as the registry served them.
//! * `images/<hex>.json`: one [`ImageInfo`] per manifest digest `<hex>`.
//! * `bases/<hex>.ext4`: the built base root disk of an image (see `rootdisk`).
//! * `tmp/`: scratch space (merged tars, partial downloads, partial bases).

use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// One layer of an image, in manifest order (oldest first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerInfo {
    pub digest: String,
    pub media_type: String,
    /// Compressed size in bytes.
    pub size: u64,
}

/// A cached image. `id` is the digest of its linux/arm64 manifest, which is
/// what machines are pinned to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageInfo {
    /// `sha256:<hex>`.
    pub id: String,
    /// Canonical references that resolved to this image, e.g.
    /// `ghcr.io/monai/clankers:slim`.
    pub references: Vec<String>,
    pub layers: Vec<LayerInfo>,
    /// The image config JSON, as published.
    pub config: String,
    pub pulled: SystemTime,
}

impl ImageInfo {
    /// Sum of the compressed layer sizes.
    pub fn size(&self) -> u64 {
        self.layers.iter().map(|l| l.size).sum()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ImageStore {
    root: PathBuf,
    insecure_registries: Vec<String>,
}

impl ImageStore {
    pub fn open(root: &Path, insecure_registries: Vec<String>) -> Result<Self> {
        for sub in ["blobs/sha256", "images", "bases", "tmp"] {
            fs::create_dir_all(root.join(sub))?;
        }
        Ok(ImageStore {
            root: root.to_path_buf(),
            insecure_registries,
        })
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn blob_path(&self, digest: &str) -> PathBuf {
        self.root
            .join("blobs/sha256")
            .join(digest.strip_prefix("sha256:").unwrap_or(digest))
    }

    pub fn base_path(&self, id: &str) -> PathBuf {
        self.root.join("bases").join(format!("{}.ext4", hex_of(id)))
    }

    fn image_path(&self, id: &str) -> PathBuf {
        self.root
            .join("images")
            .join(format!("{}.json", hex_of(id)))
    }

    /// All cached images, newest pull first.
    pub fn list(&self) -> Result<Vec<ImageInfo>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.root.join("images"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json")
                && let Ok(bytes) = fs::read(&path)
                && let Ok(info) = serde_json::from_slice::<ImageInfo>(&bytes)
            {
                out.push(info);
            }
        }
        out.sort_by_key(|i| std::cmp::Reverse(i.pulled));
        Ok(out)
    }

    /// Finds a cached image by reference (`name[:tag]`, `name@sha256:...`) or
    /// by image id (`sha256:<hex>` or an unambiguous hex prefix).
    pub fn find(&self, reference: &str) -> Result<Option<ImageInfo>> {
        let images = self.list()?;
        if let Some(info) = images.iter().find(|i| i.id == reference) {
            return Ok(Some(info.clone()));
        }
        let canonical = canonical_reference(reference).ok();
        if let Some(canonical) = &canonical {
            if let Some(info) = images.iter().find(|i| i.references.contains(canonical)) {
                return Ok(Some(info.clone()));
            }
            // A digest reference matches the image with that id even if it was
            // pulled under another name.
            if let Some((_, digest)) = canonical.split_once('@')
                && let Some(info) = images.iter().find(|i| i.id == digest)
            {
                return Ok(Some(info.clone()));
            }
        }
        let hex = reference.strip_prefix("sha256:").unwrap_or(reference);
        if hex.len() >= 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            let mut by_prefix = images.iter().filter(|i| hex_of(&i.id).starts_with(hex));
            if let (Some(info), None) = (by_prefix.next(), by_prefix.next()) {
                return Ok(Some(info.clone()));
            }
        }
        Ok(None)
    }

    /// Pulls `reference` (anonymous auth), downloading any missing layer blobs,
    /// and records it in the cache.
    pub fn pull(&self, reference: &str) -> Result<ImageInfo> {
        let parsed: Reference = reference.parse().map_err(|e| {
            Error::invalid_parameter(format!("invalid image reference \"{reference}\": {e}"))
        })?;
        let canonical = parsed.whole();
        let info = block_on(self.pull_async(&parsed, &canonical))?;
        self.record(info)
    }

    async fn pull_async(&self, reference: &Reference, canonical: &str) -> Result<ImageInfo> {
        let unavailable = |what: &str, e: &dyn std::fmt::Display| {
            Error::unavailable(format!("{what} {canonical}: {e}"))
        };
        // ring, not aws-lc: aws-lc-sys does not link with zig for darwin. Fails
        // harmlessly when a provider is already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::new(ClientConfig {
            protocol: if self.insecure_registries.is_empty() {
                ClientProtocol::Https
            } else {
                ClientProtocol::HttpsExcept(self.insecure_registries.clone())
            },
            platform_resolver: Some(Box::new(|entries| {
                entries
                    .iter()
                    .find(|e| {
                        e.platform.as_ref().is_some_and(|p| {
                            p.os.to_string() == "linux" && p.architecture.to_string() == "arm64"
                        })
                    })
                    .map(|e| e.digest.clone())
            })),
            ..Default::default()
        });
        let (manifest, digest, config) = client
            .pull_manifest_and_config(reference, &RegistryAuth::Anonymous)
            .await
            .map_err(|e| unavailable("pulling manifest of", &e))?;

        let mut layers = Vec::new();
        for layer in &manifest.layers {
            let path = self.blob_path(&layer.digest);
            let have = fs::metadata(&path).map(|m| m.len() as i64).ok();
            if have != Some(layer.size) {
                let part = path.with_extension(format!("part{}", std::process::id()));
                let file = tokio::fs::File::create(&part).await?;
                client
                    .pull_blob(reference, layer, file)
                    .await
                    .map_err(|e| unavailable(&format!("pulling layer {} of", layer.digest), &e))?;
                fs::rename(&part, &path)?;
            }
            layers.push(LayerInfo {
                digest: layer.digest.clone(),
                media_type: layer.media_type.clone(),
                size: layer.size.max(0) as u64,
            });
        }
        Ok(ImageInfo {
            id: digest,
            references: vec![canonical.to_owned()],
            layers,
            config,
            pulled: SystemTime::now(),
        })
    }

    /// Persists `info`, merging in the references of an existing record.
    fn record(&self, mut info: ImageInfo) -> Result<ImageInfo> {
        let path = self.image_path(&info.id);
        if let Ok(bytes) = fs::read(&path)
            && let Ok(old) = serde_json::from_slice::<ImageInfo>(&bytes)
        {
            let refs: BTreeSet<String> =
                old.references.into_iter().chain(info.references).collect();
            info.references = refs.into_iter().collect();
        }
        // A tag that moved to this digest no longer names the older image.
        for other in self.list()? {
            if other.id != info.id && other.references.iter().any(|r| info.references.contains(r)) {
                let mut other = other;
                other.references.retain(|r| !info.references.contains(r));
                self.write(&other)?;
            }
        }
        self.write(&info)?;
        Ok(info)
    }

    fn write(&self, info: &ImageInfo) -> Result<()> {
        crate::store::write_atomic(
            &self.image_path(&info.id),
            &serde_json::to_vec_pretty(info)?,
        )
    }
}

/// `name[:tag][@digest]` with registry and `library/` defaults filled in.
fn canonical_reference(reference: &str) -> std::result::Result<String, ()> {
    reference
        .parse::<Reference>()
        .map(|r| r.whole())
        .map_err(|_| ())
}

fn hex_of(id: &str) -> &str {
    id.strip_prefix("sha256:").unwrap_or(id)
}

/// Runs a future on a private runtime; libclankerd's API is synchronous.
pub(crate) fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("building the tokio runtime")
        .block_on(f)
}
