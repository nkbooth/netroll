// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ProblemError } from "./authApi";
import { useAuthRequest } from "./useAuthRequest";

describe("useAuthRequest state machine", () => {
  it("walks idle → loading → success for a resolving request", async () => {
    let release: (value: string) => void = () => {};
    const pending = new Promise<string>((resolve) => {
      release = resolve;
    });
    const { result } = renderHook(() => useAuthRequest(() => pending));

    expect(result.current.state.status).toBe("idle");

    act(() => {
      void result.current.run();
    });
    expect(result.current.state.status).toBe("loading");

    act(() => release("done"));
    await waitFor(() =>
      expect(result.current.state).toEqual({
        status: "success",
        data: "done",
      }),
    );
  });

  it("walks idle → loading → error and captures the problem slug", async () => {
    const failing = () =>
      Promise.reject(
        new ProblemError({ type: "/errors/magic-link-expired", status: 401 }),
      );
    const { result } = renderHook(() => useAuthRequest(failing));

    act(() => {
      void result.current.run();
    });

    await waitFor(() => expect(result.current.state.status).toBe("error"));
    expect(result.current.state).toMatchObject({
      problem: { type: "/errors/magic-link-expired" },
    });
  });

  it("treats non-problem failures as an error state with no slug", async () => {
    const failing = () => Promise.reject(new Error("network down"));
    const { result } = renderHook(() => useAuthRequest(failing));

    act(() => {
      void result.current.run();
    });

    await waitFor(() => expect(result.current.state.status).toBe("error"));
    expect(result.current.state).toMatchObject({ problem: undefined });
  });

  it("drops an out-of-order stale resolution and keeps the latest run", async () => {
    let resolveFirst: (value: string) => void = () => {};
    let resolveSecond: (value: string) => void = () => {};
    const first = new Promise<string>((resolve) => {
      resolveFirst = resolve;
    });
    const second = new Promise<string>((resolve) => {
      resolveSecond = resolve;
    });
    const queue = [first, second];
    let call = 0;
    const { result } = renderHook(() => useAuthRequest(() => queue[call++]));

    act(() => {
      void result.current.run();
    });
    act(() => {
      void result.current.run();
    });

    // The second (latest) run resolves first and wins.
    act(() => resolveSecond("second"));
    await waitFor(() =>
      expect(result.current.state).toEqual({
        status: "success",
        data: "second",
      }),
    );

    // The first (stale) run resolves late — it must NOT overwrite the latest.
    await act(async () => {
      resolveFirst("first");
      await first;
    });
    expect(result.current.state).toEqual({ status: "success", data: "second" });
  });

  it("reset invalidates an in-flight run so its late resolution is ignored", async () => {
    let release: (value: string) => void = () => {};
    const pending = new Promise<string>((resolve) => {
      release = resolve;
    });
    const { result } = renderHook(() => useAuthRequest(() => pending));

    act(() => {
      void result.current.run();
    });
    expect(result.current.state.status).toBe("loading");

    act(() => result.current.reset());
    expect(result.current.state.status).toBe("idle");

    await act(async () => {
      release("late");
      await pending;
    });
    // The run was invalidated by reset; its late resolution must not land.
    expect(result.current.state.status).toBe("idle");
  });

  it("reset returns the machine to idle", async () => {
    const { result } = renderHook(() =>
      useAuthRequest(() => Promise.resolve(1)),
    );

    act(() => {
      void result.current.run();
    });
    await waitFor(() => expect(result.current.state.status).toBe("success"));

    act(() => result.current.reset());
    expect(result.current.state.status).toBe("idle");
  });
});
