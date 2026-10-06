//! Merging an image's layers into one tar stream with whiteouts applied.

use std::path::Path;

use ocirender::{ImageSpec, LayerBlob};

use crate::error::{Error, Result};
use crate::images::{ImageInfo, ImageStore, block_on};

/// Writes the merged filesystem of `image` to `out` as a tar (ocirender applies
/// whiteouts and keeps owners, modes, xattrs, device nodes and hardlinks).
/// Pure tar work: no filesystem-level operation touches an image's files.
pub(crate) fn merge_layers(store: &ImageStore, image: &ImageInfo, out: &Path) -> Result<()> {
    let blobs: Vec<LayerBlob> = image
        .layers
        .iter()
        .enumerate()
        .map(|(index, l)| LayerBlob {
            path: store.blob_path(&l.digest),
            media_type: l.media_type.clone(),
            index,
        })
        .collect();
    let total = blobs.len();
    block_on(async {
        let layers = tokio_stream::iter(blobs.into_iter().map(Ok::<_, std::io::Error>));
        ocirender::convert_streaming(
            layers,
            total,
            ImageSpec::Tar {
                path: out.to_path_buf(),
            },
        )
        .await
    })
    .map_err(|e| Error::system(format!("merging layers of {}: {e}", image.id)))
}
