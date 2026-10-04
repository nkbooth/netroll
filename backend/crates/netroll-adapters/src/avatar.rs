// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Filesystem [`AvatarStore`]: uploaded avatars as files in one directory.
//!
//! Files rather than object storage, so a self-hoster needs only a mounted
//! volume. The directory is created and proved writable at boot, so a write
//! failure here is a real runtime fault, never a late misconfiguration.

use std::path::{Path, PathBuf};

use netroll_domain::ports::{AvatarStore, AvatarStoreError, BoxFuture};

/// Stores avatars as files under a single directory.
pub struct FsAvatarStore {
    dir: PathBuf,
}

impl FsAvatarStore {
    /// Wraps `dir` as the avatar storage root.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Resolves `file_name` inside the root, refusing anything that is not a
    /// single plain filename.
    ///
    /// Defence in depth: every caller passes a domain-derived
    /// `<account-id>.<ext>`, but this store must not be a path-traversal
    /// primitive for a future caller that forgets to validate.
    fn resolve(&self, file_name: &str) -> Result<PathBuf, AvatarStoreError> {
        let mut components = Path::new(file_name).components();
        let only = components.next().filter(|_| components.next().is_none());
        match only {
            Some(std::path::Component::Normal(name)) => Ok(self.dir.join(name)),
            _ => Err(AvatarStoreError(format!(
                "refusing to resolve {file_name:?} as an avatar filename"
            ))),
        }
    }
}

impl AvatarStore for FsAvatarStore {
    fn put<'a>(
        &'a self,
        file_name: &'a str,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<(), AvatarStoreError>> {
        Box::pin(async move {
            let path = self.resolve(file_name)?;
            // Write-then-rename: a reader serving the previous avatar never
            // sees a half-written file, and a crash mid-write cannot leave a
            // truncated image as the account's avatar.
            let temp = path.with_extension("part");
            tokio::fs::write(&temp, bytes)
                .await
                .map_err(|e| AvatarStoreError(format!("writing {}: {e}", temp.display())))?;
            match tokio::fs::rename(&temp, &path).await {
                Ok(()) => Ok(()),
                Err(e) => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    Err(AvatarStoreError(format!(
                        "renaming into {}: {e}",
                        path.display()
                    )))
                }
            }
        })
    }

    fn delete<'a>(&'a self, file_name: &'a str) -> BoxFuture<'a, Result<(), AvatarStoreError>> {
        Box::pin(async move {
            let path = self.resolve(file_name)?;
            match tokio::fs::remove_file(&path).await {
                Ok(()) => Ok(()),
                // Already gone is success: callers delete on replace and on
                // account erasure, and neither should fail because the bytes
                // were absent.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(AvatarStoreError(format!(
                    "removing {}: {e}",
                    path.display()
                ))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch directory per test, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "netroll-avatar-store-{}-{tag}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("scratch dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn put_writes_the_bytes_under_the_given_name() {
        let dir = TempDir::new("put");
        let store = FsAvatarStore::new(&dir.0);

        store.put("acct-1.png", b"image-bytes").await.unwrap();

        let written = std::fs::read(dir.0.join("acct-1.png")).unwrap();
        assert_eq!(written, b"image-bytes");
    }

    #[tokio::test]
    async fn put_replaces_an_existing_avatar_and_leaves_no_partial_file() {
        let dir = TempDir::new("replace");
        let store = FsAvatarStore::new(&dir.0);

        store.put("acct-1.png", b"first").await.unwrap();
        store.put("acct-1.png", b"second").await.unwrap();

        assert_eq!(std::fs::read(dir.0.join("acct-1.png")).unwrap(), b"second");
        // The write-then-rename temp file must not survive as a stray.
        assert!(!dir.0.join("acct-1.part").exists());
        let entries: Vec<_> = std::fs::read_dir(&dir.0).unwrap().collect();
        assert_eq!(entries.len(), 1, "one avatar file, no leftovers");
    }

    #[tokio::test]
    async fn delete_removes_the_file_and_tolerates_an_already_absent_one() {
        let dir = TempDir::new("delete");
        let store = FsAvatarStore::new(&dir.0);
        store.put("acct-1.png", b"bytes").await.unwrap();

        store.delete("acct-1.png").await.unwrap();
        assert!(!dir.0.join("acct-1.png").exists());

        // Deleting twice is not an error — replace and account-erasure paths
        // both call this without checking first.
        store.delete("acct-1.png").await.unwrap();
    }

    #[tokio::test]
    async fn refuses_a_name_that_would_escape_the_storage_root() {
        let dir = TempDir::new("traversal");
        let store = FsAvatarStore::new(&dir.0);

        for hostile in ["../escape.png", "sub/dir.png", "/etc/passwd", "..", ""] {
            assert!(
                store.put(hostile, b"x").await.is_err(),
                "{hostile:?} must be refused"
            );
            assert!(
                store.delete(hostile).await.is_err(),
                "{hostile:?} must be refused"
            );
        }
        // Nothing was created outside (or inside) the root.
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    }
}
