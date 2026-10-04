// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter, useLocation } from "react-router";
import type { ReactElement } from "react";

import { messageForProblemType } from "../../errors/problemMessages";
import { SelfCheckInControl } from "./SelfCheckInControl";
import type { OwnCheckIn } from "./SelfCheckInControl";
import { createSessionStore } from "./sessionStore";
import { ProblemError } from "../auth/authApi";
import type { Account } from "../auth/authApi";
import type { SessionSummaryBody } from "./sessionWire";

/** A route probe that surfaces the `returnTo` the gate navigation carried. */
function Probe({ label }: { readonly label: string }): ReactElement {
  const location = useLocation();
  const returnTo = (location.state as { returnTo?: string } | null)?.returnTo ?? "";
  return <div data-testid={label}>{returnTo}</div>;
}

const account = (overrides: Partial<Account> = {}): Account => ({
  id: "a",
  email: "maria@example.com",
  emailVerifiedAt: "2026-07-16T00:00:00Z",
  consentRequired: false,
  requiredTermsVersion: "1",
  callsign: "W2BCD",
  displayName: "Maria",
  location: "Hartford, CT",
  grid: null,
  avatarUrl: null,
  gravatarUrl: "https://example.com/g",
  qrzCredentialsSet: false,
  isAdmin: false,
  ...overrides,
});

/** Renders the control at /live/abc plus location probes at the gate routes. */
function mountControl(props: Partial<Parameters<typeof SelfCheckInControl>[0]>): {
  addCheckInFn: ReturnType<typeof vi.fn>;
  updateCheckInFn: ReturnType<typeof vi.fn>;
  removeCheckInFn: ReturnType<typeof vi.fn>;
  store: ReturnType<typeof createSessionStore>;
} {
  const store = props.store ?? createSessionStore();
  const summary = {} as SessionSummaryBody;
  const addCheckInFn = vi.fn().mockResolvedValue(summary);
  const updateCheckInFn = vi.fn().mockResolvedValue(summary);
  const removeCheckInFn = vi.fn().mockResolvedValue(summary);

  const router = createMemoryRouter(
    [
      {
        path: "/live/:id",
        element: (
          <SelfCheckInControl
            sessionId="abc"
            store={store}
            account={props.account ?? null}
            ownCheckIn={props.ownCheckIn ?? null}
            mintClientEventId={props.mintClientEventId ?? (() => "cid-1")}
            scheduleTimeout={props.scheduleTimeout ?? (() => () => undefined)}
            addCheckInFn={props.addCheckInFn ?? addCheckInFn}
            updateCheckInFn={props.updateCheckInFn ?? updateCheckInFn}
            removeCheckInFn={props.removeCheckInFn ?? removeCheckInFn}
          />
        ),
      },
      { path: "/sign-in", element: <Probe label="sign-in" /> },
      { path: "/profile", element: <Probe label="profile" /> },
      { path: "/consent", element: <Probe label="consent" /> },
    ],
    { initialEntries: ["/live/abc"] },
  );
  render(<RouterProvider router={router} />);
  return { addCheckInFn, updateCheckInFn, removeCheckInFn, store };
}

describe("SelfCheckInControl", () => {
  it("routes a signed-out viewer to /sign-in carrying returnTo (the /live path)", async () => {
    mountControl({ account: null });
    await userEvent.click(screen.getByRole("button", { name: /check in/i }));
    const probe = await screen.findByTestId("sign-in");
    expect(probe.textContent).toBe("/live/abc");
  });

  it("routes a callsign-less viewer to /profile carrying returnTo", async () => {
    mountControl({ account: account({ callsign: null }) });
    await userEvent.click(screen.getByRole("button", { name: /check in/i }));
    const probe = await screen.findByTestId("profile");
    expect(probe.textContent).toBe("/live/abc");
  });

  it("optimistically self-checks-in: a source=self pending row + the forced-callsign POST", async () => {
    const store = createSessionStore();
    const addCheckInFn = vi.fn().mockResolvedValue({} as SessionSummaryBody);
    mountControl({ account: account(), store, addCheckInFn });

    await userEvent.click(screen.getByRole("button", { name: /check in/i }));

    await waitFor(() => expect(addCheckInFn).toHaveBeenCalledTimes(1));
    // The POST carries the account callsign + the minted clientEventId; the server
    // forces the callsign regardless, so no other field is sent.
    expect(addCheckInFn).toHaveBeenCalledWith("abc", {
      callsign: "W2BCD",
      clientEventId: "cid-1",
    });
    // The optimistic pending row is tagged source=self (the cyan Self badge).
    const pending = store.getState().pending.get("cid-1");
    expect(pending?.source).toBe("self");
    expect(pending?.callsign).toBe("W2BCD");
  });

  it("rolls back the optimistic row and surfaces an alert when the add fails", async () => {
    const store = createSessionStore();
    const addCheckInFn = vi
      .fn()
      .mockRejectedValue(new ProblemError({ type: "/errors/forbidden", status: 403 }));
    const cancel = vi.fn();
    mountControl({
      account: account(),
      store,
      addCheckInFn,
      scheduleTimeout: () => cancel,
    });

    await userEvent.click(screen.getByRole("button", { name: /check in/i }));

    await waitFor(() => expect(screen.getByRole("alert")).toBeTruthy());
    // The pending row was rolled back and the armed timer disarmed.
    expect(store.getState().pending.has("cid-1")).toBe(false);
    expect(cancel).toHaveBeenCalled();
  });

  it("toggles the viewer's own staying with the CAS version and forced callsign", async () => {
    const updateCheckInFn = vi.fn().mockResolvedValue({} as SessionSummaryBody);
    const ownCheckIn: OwnCheckIn = { checkInId: "ci-9", staying: "in-and-out", version: 1 };
    mountControl({ account: account(), ownCheckIn, updateCheckInFn });

    await userEvent.click(screen.getByRole("button", { name: /stay for comments/i }));

    await waitFor(() => expect(updateCheckInFn).toHaveBeenCalledTimes(1));
    expect(updateCheckInFn).toHaveBeenCalledWith("abc", "ci-9", {
      callsign: "W2BCD",
      staying: "staying-for-comments",
      expectedVersion: 1,
    });
  });

  it("checks the viewer out of their own entry via the version CAS", async () => {
    const removeCheckInFn = vi.fn().mockResolvedValue({} as SessionSummaryBody);
    const ownCheckIn: OwnCheckIn = {
      checkInId: "ci-9",
      staying: "staying-for-comments",
      version: 2,
    };
    mountControl({ account: account(), ownCheckIn, removeCheckInFn });

    await userEvent.click(screen.getByRole("button", { name: /check out/i }));

    await waitFor(() => expect(removeCheckInFn).toHaveBeenCalledTimes(1));
    expect(removeCheckInFn).toHaveBeenCalledWith("abc", "ci-9", 2);
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // This control once held only the problem's `type` in state, so the
    // server's explanation was thrown away before the render could see it. The assertion is the preference ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const addCheckInFn = vi
      .fn()
      .mockRejectedValue(
        new ProblemError({ type: "/errors/forbidden", status: 403, detail }),
      );
    mountControl({ account: account(), addCheckInFn });

    await userEvent.click(screen.getByRole("button", { name: /check in/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/forbidden"),
    );
  });

  it("composes a lock-held holder callsign into a sentence rather than showing it bare", async () => {
    // `updateCheckIn` throws `/errors/lock-held`, whose
    // `detail` is a BARE callsign. This control reaches that rejection too,
    // and a blanket detail-first ordering would render the callsign alone.
    const holder = "W2BCD";
    const updateCheckInFn = vi
      .fn()
      .mockRejectedValue(
        new ProblemError({ type: "/errors/lock-held", status: 409, detail: holder }),
      );
    const ownCheckIn: OwnCheckIn = {
      checkInId: "ci-9",
      staying: "in-and-out",
      version: 1,
    };
    mountControl({ account: account(), ownCheckIn, updateCheckInFn });

    await userEvent.click(screen.getByRole("button", { name: /stay for comments/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(holder);
    expect(alert.textContent).not.toBe(holder);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/lock-held"),
    );
  });

});
