//! Hex and `sha256:` digest helpers shared by images, root disks and downloads.

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The hex part of `sha256:<hex>` (an id without the prefix is returned as is).
pub(crate) fn hex_of(id: &str) -> &str {
    id.strip_prefix("sha256:").unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_prefix() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
        assert_eq!(hex_of("sha256:ab"), "ab");
        assert_eq!(hex_of("ab"), "ab");
    }
}
