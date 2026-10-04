// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter, useLocation } from "react-router";

import { messageForProblemType } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import { expectNoAxeViolations } from "../../test/axe";
import { fetchRecentCheckIns } from "./checkInHistoryApi";
import type { CheckInHistoryEntry } from "./checkInHistoryApi";
import { ProfilePage } from "./ProfilePage";

// The history is its own request with its own lifecycle; default every test to
// an empty page so unrelated assertions aren't reading a failed section, and
// override per-call in the check-in-history tests below.
vi.mock("./checkInHistoryApi", () => ({
  fetchRecentCheckIns: vi
    .fn()
    .mockResolvedValue({ items: [], nextCursor: null }),
}));

/** One history entry on the wire — the six-field server contract. */
function historyEntry(overrides: Partial<CheckInHistoryEntry> = {}) {
  return {
    netSessionId: "0192f4a1-0000-7000-8000-00000000000a",
    netTitle: "Tuesday Traffic",
    band: "40m",
    mode: "ssb",
    via: null,
    callsign: "W1AW",
    checkedInAt: "2026-07-21T01:02:03+00:00",
    ...overrides,
  };
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

const GRAVATAR = "https://gravatar.com/avatar/abc123?d=mp";

function accountBody(overrides: Record<string, unknown> = {}) {
  return {
    id: "1",
    email: "op@example.com",
    emailVerifiedAt: "2026-07-15T00:00:00Z",
    consentRequired: false,
    requiredTermsVersion: "2026-07-15",
    callsign: null,
    displayName: null,
    location: null,
    grid: null,
    avatarUrl: null,
    gravatarUrl: GRAVATAR,
    qrzCredentialsSet: false,
    ...overrides,
  };
}

function meResponse(consentRequired: boolean, callsign: string | null = null) {
  return jsonResponse(200, accountBody({ consentRequired, callsign }));
}

/** Surfaces the gate-and-return state the real ConsentPage would consume. */
function ConsentProbe() {
  const location = useLocation();
  const state = location.state as { returnTo?: string } | null;
  return <div data-testid="consent">{state?.returnTo ?? ""}</div>;
}

function renderProfile(fetchMock: ReturnType<typeof vi.fn>) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(
    [
      { path: "/profile", element: <ProfilePage /> },
      { path: "/sign-in", element: <div data-testid="sign-in" /> },
      { path: "/consent", element: <ConsentProbe /> },
    ],
    { initialEntries: ["/profile"] },
  );
  render(<RouterProvider router={router} />);
  return router;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ProfilePage", () => {
  it("sends the signed-out to sign-in rather than showing the page", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(401, { type: "/errors/unauthenticated", status: 401 }),
      );

    renderProfile(fetchMock);

    expect(await screen.findByTestId("sign-in")).toBeInTheDocument();
  });

  it("steers an unconsented account to the gate, carrying /profile as the return destination", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(true));

    renderProfile(fetchMock);

    expect(await screen.findByTestId("consent")).toHaveTextContent("/profile");
  });

  it("frames the profile in an elevated panel with a tonal identity band", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, "W1AW"));

    renderProfile(fetchMock);

    // The callsign shows twice (identity chip + the callsign section) — this
    // assertion is about the band, so reach it structurally.
    await screen.findAllByText("W1AW");
    const band = document.querySelector("[data-identity-band]") as HTMLElement;
    expect(band).toHaveStyle({ background: "var(--head-grad)" });

    // Same frame as every other surface: elevated, and clipping the flush
    // identity band to its rounded corners.
    expect(band.closest("section")).toHaveStyle({
      background: "var(--surface)",
      boxShadow: "var(--shadow)",
      overflow: "hidden",
    });
  });

  it("shares the content measure every other surface uses", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, "W1AW"));

    renderProfile(fetchMock);

    await screen.findAllByText("W1AW");
    // A 480px column beside 1200px pages read as a different app; the profile
    // now flexes to the same measure and lets its field rows wrap into it.
    expect(screen.getByRole("main")).toHaveStyle({ maxWidth: "1200px" });
  });

  it("keeps the profile's sections in one panel rather than a grid of cards", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, "W1AW"));

    renderProfile(fetchMock);

    await screen.findAllByText("W1AW");
    // Anti-bento (DESIGN.md Do/Don't): the sections are hairline-divided
    // inside a single frame, so exactly one panel exists on the page.
    expect(document.querySelectorAll("section[style*='box-shadow']")).toHaveLength(
      1,
    );
  });

  it("renders the callsign input for a consented account", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

    renderProfile(fetchMock);

    expect(
      await screen.findByRole("textbox", { name: "Callsign" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /save callsign/i }),
    ).toBeInTheDocument();
  });

  it("offers a download-your-data link pointing at the export endpoint", async () => {
    // The personal-data export is a plain same-origin `<a download href>` — no
    // fetch/confirm. Assert the link's href + download attribute, not prose.
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, "W1AW"));

    renderProfile(fetchMock);

    const link = await screen.findByRole("link", {
      name: /download your data/i,
    });
    expect(link).toHaveAttribute("href", "/api/accounts/me/export");
    expect(link).toHaveAttribute("download");
  });

  it("has no WCAG 2.1 AA violations on the profile page", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

    renderProfile(fetchMock);
    await screen.findByRole("textbox", { name: "Callsign" });

    await expectNoAxeViolations(document.body);
  });

  it("submits the callsign and shows the server-normalized value on success", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockResolvedValueOnce(
        jsonResponse(200, accountBody({ callsign: "W1AW" })),
      );

    renderProfile(fetchMock);
    await user.type(
      await screen.findByRole("textbox", { name: "Callsign" }),
      "w1aw/p",
    );
    await user.click(screen.getByRole("button", { name: /save callsign/i }));

    // The header identity chip and the callsign-section display chip both
    // render the callsign now — assert at least one instance exists.
    expect((await screen.findAllByText("W1AW")).length).toBeGreaterThanOrEqual(
      1,
    );
    const putCall = fetchMock.mock.calls.find(
      ([url]) => url === "/api/accounts/me/callsign",
    ) as [string, RequestInit];
    expect(putCall).toBeDefined();
    expect(JSON.parse(putCall[1].body as string)).toEqual({
      callsign: "w1aw/p",
    });
  });

  it("hints at an implausible callsign before submission without blocking Save", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

    renderProfile(fetchMock);
    const input = await screen.findByRole("textbox", { name: "Callsign" });

    await user.type(input, "NODIGIT");
    expect(
      await screen.findByText(/doesn.t look like a callsign yet/i),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /save callsign/i }),
    ).not.toBeDisabled();

    await user.clear(input);
    await user.type(input, "W1AW");
    expect(
      screen.queryByText(/doesn.t look like a callsign yet/i),
    ).not.toBeInTheDocument();
  });

  it("shows the server-supplied detail when the callsign is rejected as malformed", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockResolvedValueOnce(
        jsonResponse(400, {
          type: "/errors/callsign-invalid",
          status: 400,
          detail: "callsign needs a digit separating its prefix and suffix",
        }),
      );

    renderProfile(fetchMock);
    await user.type(
      await screen.findByRole("textbox", { name: "Callsign" }),
      "ABC",
    );
    await user.click(screen.getByRole("button", { name: /save callsign/i }));

    expect(
      await screen.findByText(
        "callsign needs a digit separating its prefix and suffix",
      ),
    ).toBeInTheDocument();
  });

  it("prefills the profile inputs from the loaded account", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValueOnce(
      jsonResponse(
        200,
        accountBody({
          displayName: "Maria",
          location: "Hartford, CT",
          grid: "FN31pr",
          avatarUrl: "https://example.com/me.png",
        }),
      ),
    );

    renderProfile(fetchMock);

    expect(
      await screen.findByRole("textbox", { name: "Display name" }),
    ).toHaveValue("Maria");
    expect(screen.getByRole("textbox", { name: "Location" })).toHaveValue(
      "Hartford, CT",
    );
    expect(screen.getByRole("textbox", { name: "Grid square" })).toHaveValue(
      "FN31pr",
    );

    // Each field's label must be VISIBLE text, not just an aria-label — a
    // sighted user needs the caption on screen, not only in the a11y tree
    // (bug: fields previously carried aria-label with no rendered <span>).
    expect(screen.getByText("Display name")).toBeInTheDocument();
    expect(screen.getByText("Location")).toBeInTheDocument();
    expect(screen.getByText("Grid square")).toBeInTheDocument();

    // Avatar URL editing moved off the header's dominant visual — it lives
    // behind "Change avatar", not lost.
    await user.click(screen.getByRole("button", { name: /change avatar/i }));
    expect(screen.getByRole("textbox", { name: "Avatar URL" })).toHaveValue(
      "https://example.com/me.png",
    );
  });

  it("offers a real file picker for the avatar, not only a URL field", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, "W1AW"));
    renderProfile(fetchMock);

    await screen.findAllByText("W1AW");
    await user.click(screen.getByRole("button", { name: /change avatar/i }));

    // The whole point of item 4: a native picker, restricted to image types so
    // the OS dialog does not offer files the server will refuse.
    const picker = screen.getByLabelText(/choose an image/i);
    expect(picker).toHaveAttribute("type", "file");
    expect(picker.getAttribute("accept")).toContain("image/");
  });

  it("uploads the picked file and shows the stored avatar it returns", async () => {
    const user = userEvent.setup();
    const uploaded = { ...accountBody({}), avatarUrl: "/avatars/1.png" };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, "W1AW"))
      .mockResolvedValueOnce(jsonResponse(200, uploaded));
    renderProfile(fetchMock);

    await screen.findAllByText("W1AW");
    await user.click(screen.getByRole("button", { name: /change avatar/i }));
    await user.upload(
      screen.getByLabelText(/choose an image/i),
      new File([new Uint8Array([1, 2, 3])], "me.png", { type: "image/png" }),
    );

    await waitFor(() => {
      const [url, init] = fetchMock.mock.calls[1] as [string, RequestInit];
      expect(url).toBe("/api/accounts/me/avatar");
      expect(init.method).toBe("POST");
    });
    // The header now renders the server's stored path, so the change is
    // visible without a reload.
    await waitFor(() => {
      expect(screen.getByAltText(/your avatar/i)).toHaveAttribute(
        "src",
        "/avatars/1.png",
      );
    });
  });

  it("surfaces a rejected image as a mapped problem and keeps the old avatar", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(
          200,
          accountBody({ callsign: "W1AW", avatarUrl: "https://example.com/me.png" }),
        ),
      )
      .mockResolvedValueOnce(
        jsonResponse(422, { type: "/errors/avatar-invalid", status: 422 }),
      );
    renderProfile(fetchMock);

    await screen.findAllByText("W1AW");
    await user.click(screen.getByRole("button", { name: /change avatar/i }));
    await user.upload(
      screen.getByLabelText(/choose an image/i),
      new File(["nope"], "me.png", { type: "image/png" }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      messageForProblemType("/errors/avatar-invalid"),
    );
    // A refused upload must not blank the avatar that is still in effect.
    expect(screen.getByAltText(/your avatar/i)).toHaveAttribute(
      "src",
      "https://example.com/me.png",
    );
  });

  it("removes an uploaded avatar and falls back to the Gravatar", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(200, accountBody({ callsign: "W1AW", avatarUrl: "/avatars/1.png" })),
      )
      .mockResolvedValueOnce(
        jsonResponse(200, accountBody({ callsign: "W1AW", avatarUrl: null })),
      );
    renderProfile(fetchMock);

    await screen.findAllByText("W1AW");
    await user.click(screen.getByRole("button", { name: /change avatar/i }));
    await user.click(screen.getByRole("button", { name: /remove avatar/i }));

    await waitFor(() => {
      const [url, init] = fetchMock.mock.calls[1] as [string, RequestInit];
      expect(url).toBe("/api/accounts/me/avatar");
      expect(init.method).toBe("DELETE");
    });
    // The page's no-custom-avatar state is the initials tile, NOT the
    // Gravatar: `effectiveAvatarUrl` exists and is unit-tested but is wired
    // into no surface (flagged, deliberately not changed here).
    await waitFor(() => {
      expect(screen.queryByAltText(/your avatar/i)).not.toBeInTheDocument();
    });
    expect(screen.getByText("W1")).toBeInTheDocument();
  });

  it("offers no remove control when there is no uploaded avatar to remove", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(200, accountBody({ callsign: "W1AW", avatarUrl: null })),
      );
    renderProfile(fetchMock);

    await screen.findAllByText("W1AW");
    await user.click(screen.getByRole("button", { name: /change avatar/i }));

    // Nothing of ours is stored, so "Remove" would be a no-op button.
    expect(
      screen.queryByRole("button", { name: /remove avatar/i }),
    ).not.toBeInTheDocument();
  });

  it("shows an initials-fallback tile derived from the display name when no avatar URL is set", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(
      jsonResponse(
        200,
        accountBody({ avatarUrl: null, displayName: "Maria Booth" }),
      ),
    );

    renderProfile(fetchMock);

    // No custom avatar set — no <img> renders at all; the header shows a
    // decorative initials tile instead (the name is already announced by the
    // adjacent heading, so the tile itself carries no accessible name).
    await screen.findByText("Maria Booth");
    expect(
      screen.queryByRole("img", { name: "Your avatar" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("MB")).toBeInTheDocument();
  });

  it("falls back to the initials tile when the custom avatar image fails to load, without looping", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(
      jsonResponse(
        200,
        accountBody({
          avatarUrl: "https://example.com/broken.png",
          displayName: "Maria Booth",
        }),
      ),
    );

    renderProfile(fetchMock);
    const img = await screen.findByRole("img", { name: "Your avatar" });
    expect(img).toHaveAttribute("src", "https://example.com/broken.png");

    // A broken custom URL degrades to the initials tile.
    fireEvent.error(img);
    expect(
      screen.queryByRole("img", { name: "Your avatar" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("MB")).toBeInTheDocument();

    // Nothing left to error on — a second error event has nothing to loop.
    expect(screen.getByText("MB")).toBeInTheDocument();
  });

  it("submits trimmed profile values with blanks cleared, and success replaces the account state", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockResolvedValueOnce(
        jsonResponse(
          200,
          accountBody({
            displayName: "Maria",
            location: "Hartford, CT",
            grid: "FN31pr",
            avatarUrl: null,
          }),
        ),
      );

    renderProfile(fetchMock);
    await user.type(
      await screen.findByRole("textbox", { name: "Display name" }),
      "  Maria ",
    );
    await user.type(
      screen.getByRole("textbox", { name: "Location" }),
      "Hartford, CT",
    );
    await user.type(
      screen.getByRole("textbox", { name: "Grid square" }),
      "fn31pr",
    );
    // Avatar URL left blank — blank clears.
    await user.click(screen.getByRole("button", { name: /save profile/i }));

    const putCall = fetchMock.mock.calls.find(
      ([url]) => url === "/api/accounts/me/profile",
    ) as [string, RequestInit];
    expect(putCall).toBeDefined();
    expect(JSON.parse(putCall[1].body as string)).toEqual({
      displayName: "Maria",
      location: "Hartford, CT",
      grid: "fn31pr",
      avatarUrl: null,
    });

    // The server-normalized grid replaces the page's account state.
    expect(
      await screen.findByRole("textbox", { name: "Grid square" }),
    ).toHaveValue("FN31pr");
  });

  it("shows the server-supplied detail when the grid is rejected", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockResolvedValueOnce(
        jsonResponse(400, {
          type: "/errors/grid-invalid",
          status: 400,
          detail: "grid must start with two letters A through R",
        }),
      );

    renderProfile(fetchMock);
    await user.type(
      await screen.findByRole("textbox", { name: "Grid square" }),
      "SS11",
    );
    await user.click(screen.getByRole("button", { name: /save profile/i }));

    expect(
      await screen.findByText("grid must start with two letters A through R"),
    ).toBeInTheDocument();
  });

  it("renders the current email and a New email input for a consented account", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(
      jsonResponse(200, accountBody({ email: "op@example.com" })),
    );

    renderProfile(fetchMock);

    expect(
      await screen.findByRole("textbox", { name: "New email" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /send confirmation link/i }),
    ).toBeInTheDocument();
  });

  it("posts the trimmed, lowercased new email and shows a link-sent state naming it", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(200, accountBody({ email: "op@example.com" })),
      )
      .mockResolvedValueOnce(new Response(null, { status: 202 }));

    renderProfile(fetchMock);
    await user.type(
      await screen.findByRole("textbox", { name: "New email" }),
      "  NEW@Example.com  ",
    );
    await user.click(
      screen.getByRole("button", { name: /send confirmation link/i }),
    );

    // The submitted payload is normalized client-side (trim + lowercase).
    const postCall = (await vi.waitFor(() => {
      const call = fetchMock.mock.calls.find(
        ([url]) => url === "/api/accounts/me/email-change",
      );
      expect(call).toBeDefined();
      return call;
    })) as [string, RequestInit];
    expect(JSON.parse(postCall[1].body as string)).toEqual({
      email: "new@example.com",
    });

    // Success swaps to a link-sent state: the New email input is replaced by
    // a note naming the normalized target, and the current email is still
    // shown (it stays in effect until the link is confirmed).
    expect(await screen.findByText(/new@example\.com/)).toBeInTheDocument();
    expect(
      screen.queryByRole("textbox", { name: "New email" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getAllByText(/op@example\.com/).length,
    ).toBeGreaterThanOrEqual(1);
  });

  it("returns the email section to an editable input after a successful send", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(200, accountBody({ email: "op@example.com" })),
      )
      .mockResolvedValueOnce(new Response(null, { status: 202 }));

    renderProfile(fetchMock);
    await user.type(
      await screen.findByRole("textbox", { name: "New email" }),
      "typo@example.com",
    );
    await user.click(
      screen.getByRole("button", { name: /send confirmation link/i }),
    );

    // Link-sent state: the input is replaced by the confirmation note.
    await screen.findByText(/typo@example\.com/);
    expect(
      screen.queryByRole("textbox", { name: "New email" }),
    ).not.toBeInTheDocument();

    // The dead-end fix: an affordance returns to the editable input without a
    // page reload, so a mistyped address can be corrected.
    await user.click(screen.getByRole("button", { name: /different address/i }));
    expect(
      await screen.findByRole("textbox", { name: "New email" }),
    ).toBeInTheDocument();
  });

  it("shows the mapped message when a new-email request is rejected as taken", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(200, accountBody({ email: "op@example.com" })),
      )
      .mockResolvedValueOnce(
        jsonResponse(409, { type: "/errors/email-taken", status: 409 }),
      );

    renderProfile(fetchMock);
    await user.type(
      await screen.findByRole("textbox", { name: "New email" }),
      "taken@example.com",
    );
    await user.click(
      screen.getByRole("button", { name: /send confirmation link/i }),
    );

    const expected = messageForProblemType("/errors/email-taken");
    expect(await screen.findByText(expected)).toBeInTheDocument();
  });

  it("reveals the confirm step only after the first Delete account activation", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

    renderProfile(fetchMock);

    const deleteButton = await screen.findByRole("button", {
      name: "Delete account",
    });
    // Two-step guard: the confirm affordance is absent until the first
    // activation — one click cannot destroy the account.
    expect(
      screen.queryByRole("button", { name: "Confirm deletion" }),
    ).not.toBeInTheDocument();

    await user.click(deleteButton);

    expect(
      await screen.findByRole("button", { name: "Confirm deletion" }),
    ).toBeInTheDocument();
  });

  it("confirming deletion calls the API and redirects to sign-in on success", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));

    renderProfile(fetchMock);

    await user.click(
      await screen.findByRole("button", { name: "Delete account" }),
    );
    await user.click(screen.getByRole("button", { name: "Confirm deletion" }));

    // Success signs the user out everywhere → the app lands on /sign-in.
    expect(await screen.findByTestId("sign-in")).toBeInTheDocument();

    const deleteCall = fetchMock.mock.calls.find(
      ([url, init]) =>
        url === "/api/accounts/me" &&
        (init as RequestInit | undefined)?.method === "DELETE",
    );
    expect(deleteCall).toBeDefined();
  });

  it("does not send a second DELETE when the confirm button is clicked twice before the request settles", async () => {
    const user = userEvent.setup();
    let resolveDelete: (response: Response) => void = () => {};
    const pendingDelete = new Promise<Response>((resolve) => {
      resolveDelete = resolve;
    });
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockImplementationOnce(() => pendingDelete);

    renderProfile(fetchMock);

    await user.click(
      await screen.findByRole("button", { name: "Delete account" }),
    );
    const confirmButton = screen.getByRole("button", {
      name: "Confirm deletion",
    });

    // Two synchronous clicks with the DELETE still in flight — a native
    // disabled button drops the second click event before onClick fires
    // (the same `disabled={status === "loading"}` guard every other button
    // on this page uses), so this proves the guard, not just good luck.
    fireEvent.click(confirmButton);
    fireEvent.click(confirmButton);

    resolveDelete(new Response(null, { status: 204 }));
    await screen.findByTestId("sign-in");

    const deleteCalls = fetchMock.mock.calls.filter(
      ([url, init]) =>
        url === "/api/accounts/me" &&
        (init as RequestInit | undefined)?.method === "DELETE",
    );
    expect(deleteCalls).toHaveLength(1);
  });

  it("disables the callsign input while a callsign save is in flight", async () => {
    const user = userEvent.setup();
    let resolveSave: (response: Response) => void = () => {};
    const pending = new Promise<Response>((resolve) => {
      resolveSave = resolve;
    });
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockImplementationOnce(() => pending);

    renderProfile(fetchMock);
    const input = await screen.findByRole("textbox", { name: "Callsign" });
    expect(input).not.toBeDisabled();

    await user.type(input, "W1AW");
    await user.click(screen.getByRole("button", { name: /save callsign/i }));

    // In flight: a slow success handler clears the input, so it must be
    // frozen to protect any text typed after Save.
    expect(input).toBeDisabled();

    resolveSave(jsonResponse(200, accountBody({ callsign: "W1AW" })));
    // The header identity chip and the callsign-section display chip both
    // render the callsign now — assert at least one instance exists.
    expect((await screen.findAllByText("W1AW")).length).toBeGreaterThanOrEqual(
      1,
    );
  });

  it("disables the profile inputs while a profile save is in flight", async () => {
    const user = userEvent.setup();
    let resolveSave: (response: Response) => void = () => {};
    const pending = new Promise<Response>((resolve) => {
      resolveSave = resolve;
    });
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockImplementationOnce(() => pending);

    renderProfile(fetchMock);
    const displayName = await screen.findByRole("textbox", {
      name: "Display name",
    });
    expect(displayName).not.toBeDisabled();

    await user.type(displayName, "Maria");
    await user.click(screen.getByRole("button", { name: /save profile/i }));

    expect(displayName).toBeDisabled();

    resolveSave(jsonResponse(200, accountBody({ displayName: "Maria" })));
    await waitFor(() => expect(displayName).not.toBeDisabled());
  });

  it("disables the new-email input while an email-change send is in flight", async () => {
    const user = userEvent.setup();
    let resolveSend: (response: Response) => void = () => {};
    const pending = new Promise<Response>((resolve) => {
      resolveSend = resolve;
    });
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(200, accountBody({ email: "op@example.com" })),
      )
      .mockImplementationOnce(() => pending);

    renderProfile(fetchMock);
    const input = await screen.findByRole("textbox", { name: "New email" });
    expect(input).not.toBeDisabled();

    await user.type(input, "new@example.com");
    await user.click(
      screen.getByRole("button", { name: /send confirmation link/i }),
    );

    expect(input).toBeDisabled();

    resolveSend(new Response(null, { status: 202 }));
    await screen.findByText(/new@example\.com/);
  });

  it("surfaces an error alert without redirecting when deletion fails", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockResolvedValueOnce(
        jsonResponse(500, { type: "/errors/internal", status: 500 }),
      );

    renderProfile(fetchMock);

    await user.click(
      await screen.findByRole("button", { name: "Delete account" }),
    );
    await user.click(screen.getByRole("button", { name: "Confirm deletion" }));

    // The failure surfaces as an alert; the user is NOT signed out.
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(screen.queryByTestId("sign-in")).not.toBeInTheDocument();
  });

  it("shows the not-set state and never prefills the write-only password", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

    renderProfile(fetchMock);

    expect(await screen.findByText(/no qrz credentials set/i)).toBeInTheDocument();
    // The password input is a password field and starts empty — the stored
    // secret is never fetched or rendered (write-only).
    const password = screen.getByLabelText("QRZ password") as HTMLInputElement;
    expect(password.type).toBe("password");
    expect(password.value).toBe("");
  });

  it("reflects the credentials-set state without ever displaying a password", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(200, accountBody({ qrzCredentialsSet: true })),
      );

    renderProfile(fetchMock);

    expect(
      await screen.findByText(/qrz credentials are set/i),
    ).toBeInTheDocument();
    // Even when set, the password field is empty — nothing is prefilled.
    const password = screen.getByLabelText("QRZ password") as HTMLInputElement;
    expect(password.value).toBe("");
  });

  it("PUTs the callsign + password and flips to the set state on success", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));

    renderProfile(fetchMock);

    await user.type(
      await screen.findByLabelText("QRZ callsign"),
      "W1AW",
    );
    await user.type(screen.getByLabelText("QRZ password"), "s3cretpass");
    await user.click(
      screen.getByRole("button", { name: /save credentials/i }),
    );

    // The account flips to "set" locally after the successful PUT.
    expect(
      await screen.findByText(/qrz credentials are set/i),
    ).toBeInTheDocument();
    const putCall = fetchMock.mock.calls.find(
      ([url]) => url === "/api/accounts/me/qrz-credentials",
    ) as [string, RequestInit];
    expect(putCall).toBeDefined();
    expect(putCall[1].method).toBe("PUT");
    expect(JSON.parse(putCall[1].body as string)).toEqual({
      callsign: "W1AW",
      password: "s3cretpass",
    });
    expect((screen.getByLabelText("QRZ password") as HTMLInputElement).value).toBe(
      "",
    );
  });

  it("surfaces the 503 crypto-unavailable state as an alert", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(meResponse(false, null))
      .mockResolvedValueOnce(
        jsonResponse(503, {
          type: "/errors/crypto-unavailable",
          status: 503,
        }),
      );

    renderProfile(fetchMock);

    await user.type(await screen.findByLabelText("QRZ callsign"), "W1AW");
    await user.type(screen.getByLabelText("QRZ password"), "s3cretpass");
    await user.click(
      screen.getByRole("button", { name: /save credentials/i }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      messageForProblemType("/errors/crypto-unavailable"),
    );
  });

  it("clears stored credentials and returns to the not-set state", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(200, accountBody({ qrzCredentialsSet: true })),
      )
      .mockResolvedValueOnce(new Response(null, { status: 204 }));

    renderProfile(fetchMock);

    await user.click(await screen.findByRole("button", { name: "Clear" }));

    expect(
      await screen.findByText(/no qrz credentials set/i),
    ).toBeInTheDocument();
    const deleteCall = fetchMock.mock.calls.find(
      ([url]) => url === "/api/accounts/me/qrz-credentials",
    ) as [string, RequestInit];
    expect(deleteCall[1].method).toBe("DELETE");
  });

  it("renders the identity header with name, mono callsign chip, and a location · grid meta line", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(
      jsonResponse(
        200,
        accountBody({
          displayName: "Maria Booth",
          callsign: "N7QRP",
          location: "Boise, Idaho",
          grid: "DN13",
        }),
      ),
    );

    renderProfile(fetchMock);

    expect(await screen.findByText("Maria Booth")).toBeInTheDocument();
    // The header chip and the callsign-section display chip both render the
    // callsign text — assert at least one instance rather than an exact count.
    expect(screen.getAllByText("N7QRP").length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText(/Boise, Idaho/)).toBeInTheDocument();
    expect(screen.getByText("DN13")).toBeInTheDocument();
  });

  it("shows the callsign as a display chip with a valid-format indicator, revealing the edit form only via Change callsign", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, "N7QRP"));

    renderProfile(fetchMock);

    await screen.findByText(/valid format.*self-asserted/i);
    expect(
      screen.queryByRole("textbox", { name: "Callsign" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /change callsign/i }));

    expect(
      await screen.findByRole("textbox", { name: "Callsign" }),
    ).toBeInTheDocument();
  });

  it("shows a persistent 15-minute undo description in the danger zone, never the mock's 30-day copy", async () => {
    const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

    renderProfile(fetchMock);

    // Persistent — visible before Delete is even clicked — and matches the
    // locked-correct 15-minute undo window, not the design mock's incorrect
    // "30-day" copy.
    expect(await screen.findByText(/15 minutes/i)).toBeInTheDocument();
    expect(screen.queryByText(/30.day/i)).not.toBeInTheDocument();
  });

  describe("Recent check-ins", () => {
    it("shows a loading state while the history request is in flight", async () => {
      let resolveHistory: (
        value: Awaited<ReturnType<typeof fetchRecentCheckIns>>,
      ) => void = () => {};
      vi.mocked(fetchRecentCheckIns).mockReturnValueOnce(
        new Promise((resolve) => {
          resolveHistory = resolve;
        }),
      );
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      expect(
        await screen.findByText(/loading your recent check-ins/i),
      ).toBeInTheDocument();

      resolveHistory({ items: [], nextCursor: null });
      await screen.findByText(/no check-ins yet/i);
    });

    it("shows the empty state when the page is empty, with no load-more affordance", async () => {
      vi.mocked(fetchRecentCheckIns).mockResolvedValueOnce({
        items: [],
        nextCursor: null,
      });
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      expect(await screen.findByText(/no check-ins yet/i)).toBeInTheDocument();
      expect(
        screen.queryByRole("button", { name: /load more/i }),
      ).not.toBeInTheDocument();
    });

    it("renders the way in beside the band, and alone when the way in has no band", async () => {
      vi.mocked(fetchRecentCheckIns).mockResolvedValueOnce({
        items: [
          historyEntry({ band: "40m", mode: "ssb", via: "HF — 7.185 MHz" }),
          historyEntry({
            netSessionId: "0192f4a1-0000-7000-8000-00000000000b",
            // An internet-carried way in has no band and no mode; the way in
            // itself is what the row has to say, and fix left the
            // row with nothing at all to say for it.
            band: null,
            mode: null,
            via: "EchoLink — 12345",
          }),
        ],
        nextCursor: null,
      });
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      const rows = await screen.findAllByRole("listitem");
      expect(rows[0].querySelector("[data-via]")?.textContent).toBe("HF — 7.185 MHz");
      expect(rows[0].textContent).toContain("40m");
      const internet = rows[1].querySelector("[data-via]");
      expect(internet?.textContent).toBe("EchoLink — 12345");
      expect(rows[1].textContent).not.toContain("40m");
    });

    it("renders no way-in element at all when nobody recorded one", async () => {
      vi.mocked(fetchRecentCheckIns).mockResolvedValueOnce({
        items: [historyEntry({ via: null })],
        nextCursor: null,
      });
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      const rows = await screen.findAllByRole("listitem");
      expect(rows[0].querySelector("[data-via]")).toBeNull();
    });

    it("renders the history rows from the server page", async () => {
      vi.mocked(fetchRecentCheckIns).mockResolvedValueOnce({
        items: [
          historyEntry(),
          historyEntry({
            netSessionId: "0192f4a1-0000-7000-8000-00000000000b",
            netTitle: "Thursday Rag Chew",
            band: "20m",
          }),
        ],
        nextCursor: null,
      });
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      // Structure, not copy: two listitems inside one list.
      const rows = await screen.findAllByRole("listitem");
      expect(rows).toHaveLength(2);
      expect(rows[0].closest("ul")).not.toBeNull();
      // The dead placeholder anchor is gone — it went nowhere and was focusable.
      expect(
        screen.queryByRole("link", { name: /view all check-ins/i }),
      ).not.toBeInTheDocument();
      expect(
        screen.queryByRole("button", { name: /load more/i }),
      ).not.toBeInTheDocument();
    });

    it("renders no band chip and no separator for an internet-only net", async () => {
      // `band`/`mode` became NULLABLE on the wire the moment they moved into
      // the connection set — an internet-only net has neither — but the
      // TypeScript contract still declared them `string`, so `tsc` could not
      // see the hole and the row rendered an empty chip beside a bare "·".
      // An internet-only net is the row most likely to hit it.
      vi.mocked(fetchRecentCheckIns).mockResolvedValueOnce({
        items: [
          historyEntry({
            netTitle: "EchoLink Evening Net",
            band: null,
            mode: null,
          }),
        ],
        nextCursor: null,
      });
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      const rows = await screen.findAllByRole("listitem");
      expect(rows).toHaveLength(1);
      // Structure, not copy: the separator is punctuation the row emits only
      // when it has two tokens to separate.
      expect(rows[0].textContent).not.toContain("·");
      // And the net is still identified — the row is rendered, not dropped.
      expect(rows[0].textContent).toContain("EchoLink Evening Net");
    });

    it("surfaces a failed history load as an alert", async () => {
      vi.mocked(fetchRecentCheckIns).mockRejectedValueOnce(
        new ProblemError({ type: "/errors/internal", status: 500 }),
      );
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      const alerts = await screen.findAllByRole("alert");
      expect(alerts.length).toBeGreaterThan(0);
      expect(screen.queryAllByRole("listitem")).toHaveLength(0);
    });

    it("appends the next page and sends the server's cursor on load more", async () => {
      const user = userEvent.setup();
      vi.mocked(fetchRecentCheckIns)
        .mockResolvedValueOnce({
          items: [historyEntry()],
          nextCursor: "1754000000123:00000000-0000-7000-8000-0000000000ab",
        })
        .mockResolvedValueOnce({
          items: [
            historyEntry({
              netSessionId: "0192f4a1-0000-7000-8000-00000000000c",
              netTitle: "Saturday Swap",
            }),
          ],
          nextCursor: null,
        });
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      const loadMore = await screen.findByRole("button", {
        name: /load more/i,
      });
      await user.click(loadMore);

      await waitFor(() => {
        expect(screen.getAllByRole("listitem")).toHaveLength(2);
      });
      expect(vi.mocked(fetchRecentCheckIns)).toHaveBeenLastCalledWith(
        "1754000000123:00000000-0000-7000-8000-0000000000ab",
      );
      // `nextCursor: null` on page two retires the affordance.
      expect(
        screen.queryByRole("button", { name: /load more/i }),
      ).not.toBeInTheDocument();
    });

    it("keeps the rows on screen when a load more fails, surfacing an alert", async () => {
      const user = userEvent.setup();
      vi.mocked(fetchRecentCheckIns)
        .mockResolvedValueOnce({
          items: [historyEntry()],
          nextCursor: "1754000000123:00000000-0000-7000-8000-0000000000ab",
        })
        .mockRejectedValueOnce(
          new ProblemError({ type: "/errors/internal", status: 500 }),
        );
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      await user.click(
        await screen.findByRole("button", { name: /load more/i }),
      );

      await waitFor(() => {
        expect(screen.getAllByRole("alert").length).toBeGreaterThan(0);
      });
      // The page already fetched is still valid — a failed NEXT page must not
      // discard it.
      expect(screen.getAllByRole("listitem")).toHaveLength(1);
      // `toBeInTheDocument()` alone passes on a permanently disabled button,
      // and with the inert `|| checkInPageLoading` guard clause gone the
      // `disabled` binding is the sole double-submit protection — so a flag the `finally` never lowers leaves
      // that protection stuck on and Load-more dead for the session.
      await waitFor(() =>
        expect(
          screen.getByRole("button", { name: /load more/i }),
        ).not.toBeDisabled(),
      );
    });

    it("shows the server's own detail on a failed load more, not the shared email copy", async () => {
      // This element renders `checkInPagingProblem.detail` verbatim, which
      // makes every `/errors/validation` `detail` the backend can produce on
      // this read into USER-FACING COPY — an unrecognised query parameter among
      // them. The reason those are written for a human rather than for a log is
      // this render path.
      //
      // Also the counter-direction guard on the shared map: `/errors/validation`
      // resolves to a single generic sentence in `problemMessages.ts` that
      // cannot know which of the slug's ~28 causes it is answering for, and
      // paging a check-in history deserves better than that. The
      // `detail ?? shared` preference is what keeps the generic copy off this
      // surface.
      const user = userEvent.setup();
      vi.mocked(fetchRecentCheckIns)
        .mockResolvedValueOnce({
          items: [historyEntry()],
          nextCursor: "1754000000123:00000000-0000-7000-8000-0000000000ab",
        })
        .mockRejectedValueOnce(
          new ProblemError({
            type: "/errors/validation",
            status: 400,
            detail:
              "That request wasn't something the server could read — reload the page and try again.",
          }),
        );
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      await user.click(
        await screen.findByRole("button", { name: /load more/i }),
      );

      const alert = await waitFor(() => {
        const alerts = screen
          .getAllByRole("alert")
          .filter((node) =>
            /something the server could read/i.test(node.textContent ?? ""),
          );
        expect(alerts).toHaveLength(1);
        return alerts[0];
      });
      expect(alert.textContent).toContain(
        "That request wasn't something the server could read — reload the page and try again.",
      );
      expect(alert.textContent).not.toMatch(/email/i);
      // The already-fetched page survives, as on every other paging failure.
      expect(screen.getAllByRole("listitem")).toHaveLength(1);
    });

    it("renders both rows as distinct entries when two check-ins share a net session", async () => {
      // Reachable, not hypothetical: a moderator removes a self check-in (which
      // this history still shows — it folds no `checkin.removed`) and the
      // participant re-checks-in, so one session yields two rows. The list must
      // key on something unique or React reconciles the two into one.
      const consoleError = vi
        .spyOn(console, "error")
        .mockImplementation(() => {});
      vi.mocked(fetchRecentCheckIns).mockResolvedValueOnce({
        items: [
          historyEntry({ checkedInAt: "2026-07-22T02:00:00Z" }),
          historyEntry({ checkedInAt: "2026-07-21T01:00:00Z" }),
        ],
        nextCursor: null,
      });
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);

      const rows = await screen.findAllByRole("listitem");
      expect(rows).toHaveLength(2);
      // Distinct nodes carrying distinct instants — not one row rendered twice.
      expect(rows[0].isSameNode(rows[1])).toBe(false);
      expect(rows[0].textContent).not.toBe(rows[1].textContent);
      // React's reconciler cannot tell these two apart under a duplicate key,
      // and says so; a warning here means the list is mis-keyed.
      const duplicateKeyWarnings = consoleError.mock.calls.filter((call) =>
        call.some(
          (arg) => typeof arg === "string" && arg.includes("same key"),
        ),
      );
      expect(duplicateKeyWarnings).toHaveLength(0);
      consoleError.mockRestore();
    });

    it("issues one request and appends one page when load more is clicked twice in flight", async () => {
      let resolveSecond: (
        value: Awaited<ReturnType<typeof fetchRecentCheckIns>>,
      ) => void = () => {};
      vi.mocked(fetchRecentCheckIns)
        .mockResolvedValueOnce({
          items: [historyEntry()],
          nextCursor: "1754000000123:00000000-0000-7000-8000-0000000000ab",
        })
        .mockReturnValueOnce(
          new Promise((resolve) => {
            resolveSecond = resolve;
          }),
        );
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));
      // The module-level mock is shared across this file, so its call log
      // carries over; only calls made by THIS render should be counted.
      vi.mocked(fetchRecentCheckIns).mockClear();

      renderProfile(fetchMock);

      const loadMore = await screen.findByRole("button", {
        name: /load more/i,
      });
      // The affordance is OFFERED before the press. This adds
      // no fault detection over the call count below — a button hard-wired
      // `disabled` swallows the FIRST press too, so the count is 1 and
      // `toHaveBeenCalledTimes(2)` already reds. What it adds is LOCALISATION:
      // against the same mutant this line reds first and names `disabled` as
      // the cause instead of a call-count mismatch, and it states the start of
      // the transition that `toBeDisabled()` below measures. With the inert
      // `|| checkInPageLoading` guard clause gone from `loadMoreCheckIns`,
      // this binding is the ONLY thing blocking the second press.
      //
      // There is no cross-wiring case here by design: this page has ONE paging
      // surface and no sibling flag, so the two cross-tab cases in
      // `AdminPage.test.tsx` have no third instance. The asymmetry is
      // deliberate, not a gap.
      expect(loadMore).not.toBeDisabled();
      // Each `fireEvent` wraps its own `act()`, so the first press's
      // `disabled=true` is committed before the second is dispatched — which is
      // exactly the path the binding covers, and the reason the deleted clause
      // (which read a stale render closure) never participated.
      fireEvent.click(loadMore);
      fireEvent.click(loadMore);

      // Two presses, one outstanding request: the first page plus exactly one
      // load-more call. A second call would re-send the SAME cursor and append
      // the same page twice.
      expect(vi.mocked(fetchRecentCheckIns)).toHaveBeenCalledTimes(2);
      // …because the second press landed on a disabled button.
      expect(loadMore).toBeDisabled();

      resolveSecond({
        items: [
          historyEntry({
            netSessionId: "0192f4a1-0000-7000-8000-00000000000c",
          }),
        ],
        nextCursor: null,
      });

      await waitFor(() => {
        expect(screen.getAllByRole("listitem")).toHaveLength(2);
      });
      expect(vi.mocked(fetchRecentCheckIns)).toHaveBeenCalledTimes(2);
    });

    it("has no WCAG 2.1 AA violations when the history fails to load", async () => {
      // The page's other two axe assertions cover the empty and populated
      // states; the error branch's `role="alert"` is otherwise unchecked.
      vi.mocked(fetchRecentCheckIns).mockRejectedValueOnce(
        new ProblemError({ type: "/errors/internal", status: 500 }),
      );
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);
      await screen.findAllByRole("alert");

      await expectNoAxeViolations(document.body);
    });

    it("has no WCAG 2.1 AA violations with a populated history and a load-more button", async () => {
      // The page's other axe assertion runs on the default state, which leaves
      // the list and its affordance uncovered.
      vi.mocked(fetchRecentCheckIns).mockResolvedValueOnce({
        items: [historyEntry()],
        nextCursor: "1754000000123:00000000-0000-7000-8000-0000000000ab",
      });
      const fetchMock = vi.fn().mockResolvedValueOnce(meResponse(false, null));

      renderProfile(fetchMock);
      await screen.findByRole("button", { name: /load more/i });

      await expectNoAxeViolations(document.body);
    });
  });

  it("prefers the server's problem detail over the slug-map fallback on the avatar alert", async () => {
    // The avatar slug has NO map entry, so before the resolver
    // every avatar rejection rendered the generic fallback — the one place on
    // this page that discarded `detail` while eight siblings preferred it. The
    // assertion is the preference ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(
          200,
          accountBody({ callsign: "W1AW", avatarUrl: "https://example.com/me.png" }),
        ),
      )
      .mockResolvedValueOnce(
        jsonResponse(422, {
          type: "/errors/avatar-invalid",
          status: 422,
          detail,
        }),
      );
    renderProfile(fetchMock);

    await screen.findAllByText("W1AW");
    await user.click(screen.getByRole("button", { name: /change avatar/i }));
    await user.upload(
      screen.getByLabelText(/choose an image/i),
      new File(["nope"], "me.png", { type: "image/png" }),
    );

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/avatar-invalid"),
    );
  });

});
