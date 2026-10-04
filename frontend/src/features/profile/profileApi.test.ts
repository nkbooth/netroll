// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Account } from "../auth/authApi";
import {
  effectiveAvatarUrl,
  removeAvatar,
  updateProfile,
  uploadAvatar,
} from "./profileApi";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

function stubFetch(response: Response): ReturnType<typeof vi.fn> {
  const mock = vi.fn().mockResolvedValue(response);
  vi.stubGlobal("fetch", mock);
  return mock;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

const account: Account = {
  id: "1",
  email: "op@example.com",
  emailVerifiedAt: "2026-07-15T00:00:00Z",
  consentRequired: false,
  requiredTermsVersion: "2026-07-15",
  callsign: "W1AW",
  displayName: "Maria",
  location: "Hartford, CT",
  grid: "FN31pr",
  avatarUrl: null,
  gravatarUrl: "https://gravatar.com/avatar/abc123?d=mp",
  qrzCredentialsSet: false,
  isAdmin: false,
};

describe("updateProfile", () => {
  it("PUTs the fields with same-origin credentials and returns the parsed account", async () => {
    const mock = stubFetch(jsonResponse(200, account));

    const result = await updateProfile({
      displayName: "Maria",
      location: "Hartford, CT",
      grid: "fn31pr",
      avatarUrl: null,
    });

    expect(result).toEqual(account);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/accounts/me/profile");
    expect(init.method).toBe("PUT");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({
      displayName: "Maria",
      location: "Hartford, CT",
      grid: "fn31pr",
      avatarUrl: null,
    });
  });

  it("surfaces an invalid grid as a ProblemError carrying the slug", async () => {
    stubFetch(
      jsonResponse(400, { type: "/errors/grid-invalid", status: 400 }),
    );

    await expect(updateProfile({ grid: "SS11" })).rejects.toMatchObject({
      problem: { type: "/errors/grid-invalid" },
    });
  });
});

describe("effectiveAvatarUrl", () => {
  it("prefers the custom avatar URL when set", () => {
    expect(
      effectiveAvatarUrl({
        ...account,
        avatarUrl: "https://example.com/me.png",
      }),
    ).toBe("https://example.com/me.png");
  });

  it("falls back to the Gravatar URL when no custom avatar is set", () => {
    expect(effectiveAvatarUrl({ ...account, avatarUrl: null })).toBe(
      "https://gravatar.com/avatar/abc123?d=mp",
    );
  });
});

describe("uploadAvatar", () => {
  it("posts the chosen file as multipart, letting the browser set the boundary", async () => {
    const mock = stubFetch(
      jsonResponse(200, { ...account, avatarUrl: "/avatars/1.png" }),
    );
    const file = new File([new Uint8Array([1, 2, 3])], "me.png", {
      type: "image/png",
    });

    const result = await uploadAvatar(file);

    expect(result.avatarUrl).toBe("/avatars/1.png");
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/accounts/me/avatar");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    // FormData body, and NO hand-set content-type: setting one would omit the
    // multipart boundary the server needs to parse the body at all.
    expect(init.body).toBeInstanceOf(FormData);
    expect((init.body as FormData).get("file")).toBe(file);
    expect(init.headers).toBeUndefined();
  });

  it("throws the mapped problem when the server rejects the image", async () => {
    stubFetch(
      jsonResponse(422, { type: "/errors/avatar-invalid", status: 422 }),
    );
    const file = new File(["nope"], "me.png", { type: "image/png" });

    await expect(uploadAvatar(file)).rejects.toMatchObject({
      problem: { type: "/errors/avatar-invalid" },
    });
  });
});

describe("removeAvatar", () => {
  it("deletes the avatar and resolves with the updated account", async () => {
    const mock = stubFetch(jsonResponse(200, { ...account, avatarUrl: null }));

    const result = await removeAvatar();

    expect(result.avatarUrl).toBeNull();
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/accounts/me/avatar");
    expect(init.method).toBe("DELETE");
    expect(init.credentials).toBe("same-origin");
  });
});
