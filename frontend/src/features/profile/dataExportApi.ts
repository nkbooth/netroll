// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Personal-data export URL builder. Mirrors `sessionApi.exportUrl`: a plain same-origin `<a download href>` navigation to
 * this URL carries the session cookie automatically, and the server already
 * responds with `Content-Disposition: attachment`, so the browser downloads the
 * JSON file directly — there is no `fetch`/Blob dance and no problem+json to
 * parse on the frontend (a session-less user cannot reach this authenticated
 * page, so the 401 case degrades to a plain failed navigation). This is a pure
 * URL builder with no fetch involved.
 */

/** Builds the same-origin personal-data export download URL. */
export function exportUrl(): string {
  return "/api/accounts/me/export";
}
