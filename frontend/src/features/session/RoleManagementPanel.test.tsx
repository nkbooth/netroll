// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { messageForProblemType } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import { RoleManagementPanel } from "./RoleManagementPanel";
import * as sessionApi from "./sessionApi";
import type { RoleGrant } from "./sessionApi";

/**
 * RoleManagementPanel behavior. Asserts the render-gate by role,
 * the grant/list/revoke CALLS + refetch, and error-slug→copy MAPPING (via the
 * shared harness, not hardcoded prose) — never rendered marketing text.
 */

function grant(overrides: Partial<RoleGrant> = {}): RoleGrant {
  return {
    accountId: "acct-1",
    callsign: "W1AW",
    role: "relay",
    grantedBy: "owner-1",
    grantedAt: "2026-07-17T00:00:00Z",
    ...overrides,
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("RoleManagementPanel render gate", () => {
  it("does NOT render for a viewer without ManageRoles (relay/logger/participant)", () => {
    const spy = vi.spyOn(sessionApi, "listRoles").mockResolvedValue([]);
    for (const role of ["relay", "logger", "participant"] as const) {
      const { container, unmount } = render(
        <RoleManagementPanel sessionId="s1" viewerRole={role} />,
      );
      expect(container.querySelector('[aria-label="Role management"]')).toBeNull();
      unmount();
    }
    // A non-manager never even queries the grants list.
    expect(spy).not.toHaveBeenCalled();
  });

  it("renders for an NCS and an Owner (ManageRoles holders)", async () => {
    vi.spyOn(sessionApi, "listRoles").mockResolvedValue([]);
    for (const role of ["net-control", "owner"] as const) {
      const { unmount } = render(<RoleManagementPanel sessionId="s1" viewerRole={role} />);
      expect(await screen.findByLabelText("Role management")).toBeTruthy();
      unmount();
    }
  });
});

describe("RoleManagementPanel role picker", () => {
  it("offers relay and logger only — never net-control or owner", async () => {
    vi.spyOn(sessionApi, "listRoles").mockResolvedValue([]);
    render(<RoleManagementPanel sessionId="s1" viewerRole="net-control" />);
    await screen.findByLabelText("Role management");

    const select = screen.getByLabelText("Role to grant") as HTMLSelectElement;
    const values = [...select.options].map((o) => o.value).sort();
    expect(values).toEqual(["logger", "relay"]);
  });
});

describe("RoleManagementPanel grant/list/revoke", () => {
  it("lists the current grants by callsign", async () => {
    vi.spyOn(sessionApi, "listRoles").mockResolvedValue([
      grant({ accountId: "a1", callsign: "W1AW", role: "relay" }),
      grant({ accountId: "a2", callsign: "K2XYZ", role: "logger" }),
    ]);
    render(<RoleManagementPanel sessionId="s1" viewerRole="owner" />);

    expect(await screen.findByText("W1AW")).toBeTruthy();
    expect(screen.getByText("K2XYZ")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Revoke W1AW" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Revoke K2XYZ" })).toBeTruthy();
  });

  it("grants Relay by callsign and refetches the list", async () => {
    const listSpy = vi
      .spyOn(sessionApi, "listRoles")
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([grant({ accountId: "a9", callsign: "N1CCK", role: "relay" })]);
    const grantSpy = vi
      .spyOn(sessionApi, "grantRole")
      .mockResolvedValue({ accountId: "a9", role: "relay" });

    render(<RoleManagementPanel sessionId="s1" viewerRole="net-control" />);
    await screen.findByLabelText("Role management");

    fireEvent.change(screen.getByLabelText("Grant role to station"), {
      target: { value: "n1cck" },
    });
    fireEvent.click(screen.getByRole("button", { name: /grant role/i }));

    await waitFor(() =>
      expect(grantSpy).toHaveBeenCalledWith("s1", { callsign: "n1cck", role: "relay" }),
    );
    // The panel refetches its own list after the grant (no WS delta).
    await waitFor(() => expect(listSpy).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("N1CCK")).toBeTruthy();
  });

  it("revokes a grant by accountId and the row disappears after refetch", async () => {
    const listSpy = vi
      .spyOn(sessionApi, "listRoles")
      .mockResolvedValueOnce([grant({ accountId: "a1", callsign: "W1AW", role: "relay" })])
      .mockResolvedValueOnce([]);
    const revokeSpy = vi.spyOn(sessionApi, "revokeRole").mockResolvedValue(undefined);

    render(<RoleManagementPanel sessionId="s1" viewerRole="owner" />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke W1AW" }));

    await waitFor(() => expect(revokeSpy).toHaveBeenCalledWith("s1", "a1"));
    await waitFor(() => expect(listSpy).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByText("W1AW")).toBeNull());
  });
});

describe("RoleManagementPanel error mapping", () => {
  it("surfaces owner-not-found copy on a grant to an unknown callsign", async () => {
    vi.spyOn(sessionApi, "listRoles").mockResolvedValue([]);
    vi.spyOn(sessionApi, "grantRole").mockRejectedValue(
      new ProblemError({ type: "/errors/owner-not-found", status: 404 }),
    );

    render(<RoleManagementPanel sessionId="s1" viewerRole="net-control" />);
    await screen.findByLabelText("Role management");
    fireEvent.change(screen.getByLabelText("Grant role to station"), {
      target: { value: "zz9zzz" },
    });
    fireEvent.click(screen.getByRole("button", { name: /grant role/i }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(messageForProblemType("/errors/owner-not-found"));
  });

  it("surfaces forbidden copy on an over-ceiling grant", async () => {
    vi.spyOn(sessionApi, "listRoles").mockResolvedValue([]);
    vi.spyOn(sessionApi, "grantRole").mockRejectedValue(
      new ProblemError({ type: "/errors/forbidden", status: 403 }),
    );

    render(<RoleManagementPanel sessionId="s1" viewerRole="net-control" />);
    await screen.findByLabelText("Role management");
    fireEvent.change(screen.getByLabelText("Grant role to station"), {
      target: { value: "w1aw" },
    });
    fireEvent.click(screen.getByRole("button", { name: /grant role/i }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(messageForProblemType("/errors/forbidden"));
  });

  it("surfaces role-grant-not-found copy on revoking a missing grant", async () => {
    vi.spyOn(sessionApi, "listRoles").mockResolvedValue([
      grant({ accountId: "a1", callsign: "W1AW", role: "relay" }),
    ]);
    vi.spyOn(sessionApi, "revokeRole").mockRejectedValue(
      new ProblemError({ type: "/errors/role-grant-not-found", status: 404 }),
    );

    render(<RoleManagementPanel sessionId="s1" viewerRole="owner" />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke W1AW" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe(messageForProblemType("/errors/role-grant-not-found"));
  });

  it("reconciles the stale row after a role-grant-not-found revoke, instead of leaving it clickable forever (review finding)", async () => {
    // The list is stale relative to the server (the grant is already gone).
    // A revoke against it 404s, but the panel must still refetch so the
    // now-nonexistent row's Revoke control disappears rather than persisting
    // indefinitely for the operator to click again and again.
    const listSpy = vi
      .spyOn(sessionApi, "listRoles")
      .mockResolvedValueOnce([grant({ accountId: "a1", callsign: "W1AW", role: "relay" })])
      .mockResolvedValueOnce([]);
    vi.spyOn(sessionApi, "revokeRole").mockRejectedValue(
      new ProblemError({ type: "/errors/role-grant-not-found", status: 404 }),
    );

    render(<RoleManagementPanel sessionId="s1" viewerRole="owner" />);
    fireEvent.click(await screen.findByRole("button", { name: "Revoke W1AW" }));

    await screen.findByRole("alert");
    // The failed op still triggers a reload, reconciling the stale row away.
    await waitFor(() => expect(listSpy).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByText("W1AW")).toBeNull());
  });
  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    vi.spyOn(sessionApi, "listRoles").mockResolvedValue([]);
    vi.spyOn(sessionApi, "grantRole").mockRejectedValue(
      new ProblemError({ type: "/errors/owner-not-found", status: 404, detail }),
    );

    render(<RoleManagementPanel sessionId="s1" viewerRole="net-control" />);
    await screen.findByLabelText("Role management");
    fireEvent.change(screen.getByLabelText("Grant role to station"), {
      target: { value: "zz9zzz" },
    });
    fireEvent.click(screen.getByRole("button", { name: /grant role/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/owner-not-found"),
    );
  });

});
