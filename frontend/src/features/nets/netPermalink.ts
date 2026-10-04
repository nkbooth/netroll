// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * The one place `/nets/t/{token}` is built.
 *
 * Eight production sites interpolated the path by hand across three features
 * before this helper — well past the three-strike DRY threshold, and the reason
 * the `encodeURIComponent` gap was a separately deferred item rather than a
 * one-line fix: there was no single site to fix it at. The route itself stays
 * declared in `router.tsx` (`nets/t/:token`); this only builds the href.
 */

/**
 * The in-app path to a net's public page, from its permalink token.
 *
 * The token is percent-encoded. It is server-minted and URL-safe today, so
 * this changes no rendered href — it is the fence that keeps a future token
 * alphabet from silently producing a broken link at eight call sites.
 */
export function netPermalink(token: string): string {
  return `/nets/t/${encodeURIComponent(token)}`;
}
