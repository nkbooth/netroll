// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Deletes the avatar files belonging to erased accounts.
//!
//! Blob storage has no foreign keys, so a hard-deleted account row would leave
//! its uploaded image on the volume forever — the erased user's photo surviving
//! their erasure. Both erase paths route through here.

use netroll_domain::avatar::{AVATAR_PATH_PREFIX, is_stored_avatar_path};
use netroll_domain::ports::AvatarStore;

/// Deletes the stored avatar behind one `avatar_url`, if it is one of ours.
///
/// An external `https://` URL is not ours to delete, and a failed delete is
/// logged rather than propagated: the account row is already gone, so returning
/// an error here would only abort the rest of a sweep that is otherwise
/// succeeding. The leaked file becomes an operator cleanup item, visible in the
/// log.
pub async fn delete_stored_avatar(
    // `+ Send + Sync` is load-bearing: without it the returned future is not
    // `Send`, and neither the axum handler nor the spawned finalizer compiles.
    store: &(dyn AvatarStore + Send + Sync),
    avatar_url: Option<&str>,
) {
    let Some(path) = avatar_url.filter(|url| is_stored_avatar_path(url)) else {
        return;
    };
    let Some(file_name) = path.strip_prefix(AVATAR_PATH_PREFIX) else {
        return;
    };
    if let Err(err) = store.delete(file_name).await {
        // No account id: the row is erased, and re-emitting an identifier for a
        // just-erased account in a log line works against the erasure.
        tracing::warn!(error = %err, "avatar file for an erased account not removed");
    }
}

/// Deletes the stored avatars for a batch of erased accounts.
pub async fn delete_stored_avatars(
    store: &(dyn AvatarStore + Send + Sync),
    avatar_urls: &[Option<String>],
) {
    for avatar_url in avatar_urls {
        delete_stored_avatar(store, avatar_url.as_deref()).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use netroll_domain::ports::{AvatarStoreError, BoxFuture};

    use super::*;

    /// Records delete calls; `fail` makes every delete report an error.
    struct RecordingStore {
        deleted: Mutex<Vec<String>>,
        fail: bool,
    }

    impl RecordingStore {
        fn new(fail: bool) -> Self {
            Self {
                deleted: Mutex::new(Vec::new()),
                fail,
            }
        }

        fn deleted(&self) -> Vec<String> {
            self.deleted.lock().expect("lock").clone()
        }
    }

    impl AvatarStore for RecordingStore {
        fn put<'a>(
            &'a self,
            _file_name: &'a str,
            _bytes: &'a [u8],
        ) -> BoxFuture<'a, Result<(), AvatarStoreError>> {
            Box::pin(async { Ok(()) })
        }

        fn delete<'a>(&'a self, file_name: &'a str) -> BoxFuture<'a, Result<(), AvatarStoreError>> {
            Box::pin(async move {
                self.deleted
                    .lock()
                    .expect("lock")
                    .push(file_name.to_owned());
                if self.fail {
                    Err(AvatarStoreError("disk on fire".into()))
                } else {
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn deletes_the_file_behind_a_stored_path() {
        let store = RecordingStore::new(false);

        delete_stored_avatar(&store, Some("/avatars/acct-1.png")).await;

        assert_eq!(store.deleted(), vec!["acct-1.png"]);
    }

    #[tokio::test]
    async fn ignores_an_external_url_and_an_absent_avatar() {
        let store = RecordingStore::new(false);

        delete_stored_avatar(&store, Some("https://example.com/me.png")).await;
        delete_stored_avatar(&store, None).await;

        // Neither is ours to delete.
        assert!(store.deleted().is_empty());
    }

    #[tokio::test]
    async fn a_failed_delete_does_not_stop_the_rest_of_the_batch() {
        // The whole batch's rows are already erased; aborting on the first
        // storage error would leave every later file behind too.
        let store = RecordingStore::new(true);
        let batch = vec![
            Some("/avatars/a.png".to_owned()),
            Some("https://example.com/b.png".to_owned()),
            Some("/avatars/c.jpg".to_owned()),
            None,
        ];

        delete_stored_avatars(&store, &batch).await;

        assert_eq!(store.deleted(), vec!["a.png", "c.jpg"]);
    }

    #[tokio::test]
    async fn refuses_a_traversal_path_that_reached_the_database_somehow() {
        let store = RecordingStore::new(false);

        delete_stored_avatar(&store, Some("/avatars/../../etc/passwd")).await;

        assert!(store.deleted().is_empty());
    }
}
