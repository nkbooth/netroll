// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The docs image digest is pinned in the Containerfile and in the CI docs
//! step, and they must agree: bump one and miss the other and every PR
//! validates against the old theme while the release ships the new one.
//! `include_str!` is what makes this hold — a compile-time dependency edge, so
//! editing either file rebuilds this test.

/// The release build's definition of the docs image.
const CONTAINERFILE: &str = include_str!("../../../../Containerfile");

/// The PR check's definition of the docs image.
const CI_WORKFLOW: &str = include_str!("../../../../.github/workflows/ci.yml");

/// The image reference both files pin. Kept as the bare repository so a tag
/// slipping in where a digest belongs reads as zero pins, not as a match.
const DOCS_IMAGE: &str = "docker.io/squidfunk/mkdocs-material@";

/// Every digest `source` pins `DOCS_IMAGE` at.
fn pinned_digests(source: &str) -> Vec<&str> {
    source
        .split_whitespace()
        .filter_map(|token| token.strip_prefix(DOCS_IMAGE))
        .collect()
}

#[test]
fn the_docs_image_digest_is_pinned_identically_in_the_containerfile_and_ci() {
    let release = pinned_digests(CONTAINERFILE);
    let pull_request = pinned_digests(CI_WORKFLOW);

    assert_eq!(
        release.len(),
        1,
        "the Containerfile must pin {DOCS_IMAGE} by digest exactly once (found {}). A tag \
         instead of a digest reads as zero pins here",
        release.len()
    );
    assert_eq!(
        pull_request.len(),
        1,
        "ci.yml's docs-build step must pin {DOCS_IMAGE} by digest exactly once (found {})",
        pull_request.len()
    );
    assert_eq!(
        release[0], pull_request[0],
        "the docs image digest has drifted between the release build and the PR check, so \
         the two are now different definitions of the docs build. Bump both together:\n  \
         Containerfile (FROM … AS docs): {}\n  .github/workflows/ci.yml (mkdocs build --strict): {}",
        release[0], pull_request[0]
    );
}
