//! Root-disk methods of `io.clankerd.Guest`: formatting a blank disk and
//! unpacking the merged image tar onto it, done in the guest so that owners,
//! modes, xattrs, device nodes and mtimes get real Linux semantics.
//!
//! `UnpackTar` and `PopulateDisk` are *stream-in* calls: right after the call
//! message the client sends the raw tar bytes and half-closes its write side
//! (EOF ends the stream). guestd then answers with one reply.

use serde::{Deserialize, Serialize};

/// `FormatExt4 { device, size?, label? }` -> `{}`. Runs the static mke2fs from the boot directory.
pub const METHOD_FORMAT_EXT4: &str = "io.clankerd.Guest.FormatExt4";
/// `UnpackTar { target }` + tar stream -> `UnpackSummary`. Unpacks into an existing directory.
pub const METHOD_UNPACK_TAR: &str = "io.clankerd.Guest.UnpackTar";
/// `PopulateDisk { device, size? }` + tar stream -> `UnpackSummary`.
/// Formats the disk, mounts it, unpacks the tar onto it, syncs and unmounts.
pub const METHOD_POPULATE_DISK: &str = "io.clankerd.Guest.PopulateDisk";

/// The request was malformed (bad path, device missing).
pub const ERROR_INVALID_PARAMETER: &str = "io.clankerd.Guest.InvalidParameter";
/// The operation failed; the reply parameters carry `{"message": ...}`.
pub const ERROR_FAILED: &str = "io.clankerd.Guest.Failed";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatExt4 {
    /// Block device or regular file to format.
    pub device: String,
    /// Bytes; a regular file is extended to this size first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnpackTar {
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PopulateDisk {
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnpackSummary {
    pub entries: u64,
    /// Total size of regular file contents written.
    pub bytes: u64,
}

/// Parameters of an error reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub message: String,
}
