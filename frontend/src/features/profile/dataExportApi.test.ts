// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { exportUrl } from "./dataExportApi";

describe("dataExportApi", () => {
  it("builds the same-origin personal-data export URL", () => {
    // A plain `<a download href>` navigation to this URL carries the session
    // cookie automatically and the server sets `Content-Disposition: attachment`,
    // so the builder only composes the same-origin path — no fetch/Blob dance.
    expect(exportUrl()).toBe("/api/accounts/me/export");
  });
});
