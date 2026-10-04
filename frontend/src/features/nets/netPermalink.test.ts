// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { netPermalink } from "./netPermalink";

describe("netPermalink", () => {
  it("builds the route the router declares", () => {
    expect(netPermalink("tok-def-1")).toBe("/nets/t/tok-def-1");
  });

  it("percent-encodes a token that would otherwise change the path", () => {
    // The property, not the alphabet: a token carrying `/` must not invent a
    // path segment, and one carrying `?` must not start a query string.
    expect(netPermalink("a/b")).toBe("/nets/t/a%2Fb");
    expect(netPermalink("a?b=1")).toBe("/nets/t/a%3Fb%3D1");
    expect(netPermalink("a b")).toBe("/nets/t/a%20b");
  });

  it("leaves the path prefix alone whatever the token is", () => {
    for (const token of ["", "tok", "a/b", "../escape"]) {
      expect(netPermalink(token).startsWith("/nets/t/")).toBe(true);
      expect(netPermalink(token).slice("/nets/t/".length)).not.toContain("/");
    }
  });
});
