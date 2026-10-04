// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Uploaded-avatar validation. The image type is decided by SNIFFING MAGIC
//! BYTES, never by the filename or the client's `Content-Type`: both are
//! attacker-controlled, and a `.png` that is really an HTML document served
//! from our own origin would be a stored-XSS primitive. The sniffed type then
//! decides the stored extension, so disk and bytes cannot disagree.

use thiserror::Error;

/// Largest accepted upload. Avatars render at 64px; a megabyte is already
/// generous for a source image and keeps a hostile upload from filling the
/// volume one request at a time (the HTTP layer enforces the same ceiling
/// while streaming, so oversize bodies are refused before buffering).
pub const MAX_AVATAR_BYTES: usize = 1024 * 1024;

/// The root-relative path prefix uploaded avatars are served from. Stored in
/// `accounts.avatar_url` as a same-origin path rather than an absolute URL, so
/// the reference survives a `PUBLIC_BASE_URL` change and never mixes schemes.
pub const AVATAR_PATH_PREFIX: &str = "/avatars/";

/// An accepted raster image type. Deliberately a closed set: SVG is excluded
/// because it is a script-bearing document, not an image, and would execute
/// when opened directly from our origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarImageType {
    /// PNG.
    Png,
    /// JPEG.
    Jpeg,
    /// WebP.
    Webp,
    /// GIF.
    Gif,
}

impl AvatarImageType {
    /// The file extension this type is stored under.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Gif => "gif",
        }
    }

    /// The `Content-Type` this type is served back with.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }

    /// Sniffs the type from a file's leading bytes, or `None` when the bytes
    /// are not one of the accepted image types.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(Self::Png);
        }
        // JPEG: SOI marker. The JFIF/Exif segment that follows varies, so the
        // two-byte SOI plus the following marker introducer is the reliable part.
        if bytes.len() >= 3 && bytes.starts_with(&[0xFF, 0xD8]) && bytes[2] == 0xFF {
            return Some(Self::Jpeg);
        }
        // WebP is a RIFF container whose form type is "WEBP" at offset 8.
        if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            return Some(Self::Webp);
        }
        if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            return Some(Self::Gif);
        }
        None
    }
}

/// Why an avatar upload was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AvatarError {
    /// No bytes were submitted.
    #[error("avatar file is empty")]
    Empty,
    /// Body exceeds [`MAX_AVATAR_BYTES`].
    #[error("avatar file is larger than 1 MB")]
    TooLarge,
    /// Leading bytes are not a PNG, JPEG, WebP, or GIF image.
    #[error("avatar must be a PNG, JPEG, WebP, or GIF image")]
    UnsupportedType,
}

/// A validated upload: the sniffed type plus the filename it is stored under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedAvatar {
    /// The type sniffed from the bytes, never the client-declared one.
    pub image_type: AvatarImageType,
    /// `<account-id>.<ext>` — one file per account, so an upload always
    /// replaces the previous file rather than accumulating orphans.
    pub file_name: String,
}

impl ValidatedAvatar {
    /// The root-relative path stored in `accounts.avatar_url`.
    pub fn stored_path(&self) -> String {
        format!("{AVATAR_PATH_PREFIX}{}", self.file_name)
    }
}

/// Validates uploaded avatar bytes for `account_id`, sniffing the image type
/// and deriving the stored filename.
///
/// Size is checked before the type so a huge non-image is rejected for the
/// reason the user can act on.
pub fn validate_avatar(account_id: &str, bytes: &[u8]) -> Result<ValidatedAvatar, AvatarError> {
    if bytes.is_empty() {
        return Err(AvatarError::Empty);
    }
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err(AvatarError::TooLarge);
    }
    let image_type = AvatarImageType::sniff(bytes).ok_or(AvatarError::UnsupportedType)?;
    Ok(ValidatedAvatar {
        file_name: format!("{account_id}.{}", image_type.extension()),
        image_type,
    })
}

/// True when `path` is a reference to an avatar this instance stores itself
/// (as opposed to an operator-supplied external `https://` URL).
///
/// The check is prefix-plus-shape, not a bare `starts_with`: a stored path
/// always names exactly one file directly under the prefix, so anything
/// carrying a further `/` or a `..` segment is not one of ours.
pub fn is_stored_avatar_path(path: &str) -> bool {
    let Some(file_name) = path.strip_prefix(AVATAR_PATH_PREFIX) else {
        return false;
    };
    !file_name.is_empty()
        && !file_name.contains('/')
        && !file_name.contains('\\')
        && file_name != "."
        && file_name != ".."
        && !file_name.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal byte prefixes that are enough to identify each type.
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F'];
    const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00";

    fn webp() -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0x1A, 0x00, 0x00, 0x00]);
        bytes.extend_from_slice(b"WEBPVP8 ");
        bytes
    }

    #[test]
    fn sniffs_each_accepted_image_type_from_its_magic_bytes() {
        assert_eq!(AvatarImageType::sniff(PNG), Some(AvatarImageType::Png));
        assert_eq!(AvatarImageType::sniff(JPEG), Some(AvatarImageType::Jpeg));
        assert_eq!(AvatarImageType::sniff(GIF), Some(AvatarImageType::Gif));
        assert_eq!(AvatarImageType::sniff(&webp()), Some(AvatarImageType::Webp));
    }

    #[test]
    fn refuses_a_document_masquerading_as_an_image() {
        // The whole point of sniffing: an HTML or SVG payload served back from
        // our own origin would run as script in the viewer's session.
        assert_eq!(AvatarImageType::sniff(b"<!doctype html><script>"), None);
        assert_eq!(AvatarImageType::sniff(b"<svg xmlns=\"http://www\">"), None);
        assert_eq!(AvatarImageType::sniff(b"GIF"), None);
        assert_eq!(AvatarImageType::sniff(b"RIFF____NOTWEBP"), None);
        assert_eq!(AvatarImageType::sniff(&[]), None);
    }

    #[test]
    fn names_the_stored_file_after_the_account_and_sniffed_type() {
        let validated = validate_avatar("acct-1", PNG).expect("png accepted");

        // One file per account: an upload replaces, never accumulates.
        assert_eq!(validated.file_name, "acct-1.png");
        assert_eq!(validated.stored_path(), "/avatars/acct-1.png");
        assert_eq!(validated.image_type, AvatarImageType::Png);
    }

    #[test]
    fn extension_follows_the_sniffed_bytes_not_the_claimed_name() {
        // A JPEG uploaded as "portrait.png" is still stored as .jpg, so the
        // extension we later serve a Content-Type from cannot lie.
        let validated = validate_avatar("acct-1", JPEG).expect("jpeg accepted");
        assert_eq!(validated.file_name, "acct-1.jpg");
        assert_eq!(validated.image_type.content_type(), "image/jpeg");
    }

    #[test]
    fn rejects_empty_oversize_and_unsupported_uploads() {
        assert_eq!(validate_avatar("a", &[]), Err(AvatarError::Empty));
        assert_eq!(
            validate_avatar("a", b"<!doctype html>"),
            Err(AvatarError::UnsupportedType)
        );

        let mut oversize = PNG.to_vec();
        oversize.resize(MAX_AVATAR_BYTES + 1, 0);
        assert_eq!(validate_avatar("a", &oversize), Err(AvatarError::TooLarge));
    }

    #[test]
    fn reports_size_before_type_so_the_message_is_actionable() {
        // A 2 MB non-image is too large AND unsupported; "too large" is the
        // thing the uploader can actually do something about.
        let oversize = vec![b'x'; MAX_AVATAR_BYTES + 1];
        assert_eq!(validate_avatar("a", &oversize), Err(AvatarError::TooLarge));
    }

    #[test]
    fn accepts_an_upload_of_exactly_the_maximum_size() {
        let mut exact = PNG.to_vec();
        exact.resize(MAX_AVATAR_BYTES, 0);
        assert!(validate_avatar("a", &exact).is_ok());
    }

    #[test]
    fn recognizes_paths_this_instance_stores_itself() {
        assert!(is_stored_avatar_path("/avatars/acct-1.png"));
        assert!(!is_stored_avatar_path("https://example.com/a.png"));
        assert!(!is_stored_avatar_path("/avatars/"));
    }

    #[test]
    fn refuses_a_traversal_dressed_up_as_a_stored_path() {
        // These must NOT be treated as ours: accepting one would let a crafted
        // profile write point the account at a file outside the avatar volume.
        assert!(!is_stored_avatar_path("/avatars/../../etc/passwd"));
        assert!(!is_stored_avatar_path("/avatars/sub/dir.png"));
        assert!(!is_stored_avatar_path("/avatars/..%2fetc"));
        assert!(!is_stored_avatar_path("/avatars/.."));
        assert!(!is_stored_avatar_path("/avatars/a\\b.png"));
    }
}
