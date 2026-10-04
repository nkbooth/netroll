// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useCallback, useRef, useState } from "react";

import { ProblemError } from "./authApi";
import type { Problem } from "./authApi";

/**
 * Request lifecycle for non-session async calls — the mandated vocabulary,
 * no ad-hoc booleans.
 */
export type AuthRequestState<T> =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "success"; data: T }
  | { status: "error"; problem?: Problem };

/**
 * Minimal state machine around one async request: `run` walks
 * idle → loading → success/error; `reset` returns to idle. Problem+json
 * failures keep their slug for message mapping; anything else becomes a
 * slug-less error.
 *
 * A monotonic generation guard makes `run` immune to out-of-order responses:
 * a rapid re-run (or a `reset`) invalidates any earlier in-flight call, so a
 * late-arriving stale resolution can never overwrite the latest state.
 */
export function useAuthRequest<T>(request: () => Promise<T>): {
  state: AuthRequestState<T>;
  run: () => Promise<void>;
  reset: () => void;
} {
  const [state, setState] = useState<AuthRequestState<T>>({ status: "idle" });
  // Always call the latest closure without re-creating `run`.
  const requestRef = useRef(request);
  requestRef.current = request;
  // Bumped on every `run` and `reset`; a resolution only commits when its
  // captured generation is still current.
  const generationRef = useRef(0);

  const run = useCallback(async () => {
    const generation = ++generationRef.current;
    setState({ status: "loading" });
    try {
      const data = await requestRef.current();
      if (generation === generationRef.current) {
        setState({ status: "success", data });
      }
    } catch (error) {
      if (generation === generationRef.current) {
        setState({
          status: "error",
          problem: error instanceof ProblemError ? error.problem : undefined,
        });
      }
    }
  }, []);

  const reset = useCallback(() => {
    generationRef.current += 1;
    setState({ status: "idle" });
  }, []);

  return { state, run, reset };
}
