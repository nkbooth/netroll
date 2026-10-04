// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useSyncExternalStore } from "react";
import type { CSSProperties, ReactElement } from "react";

/**
 * Polite live-region utility. A single module-level store backs one
 * `aria-live="polite"` region mounted once in the app shell; any surface can
 * announce asynchronous changes (a theme-persist failure, a roster or
 * connection update) without prop-drilling or interrupting a screen reader
 * mid-read.
 */

type Listener = () => void;

const listeners = new Set<Listener>();
let currentMessage = "";

const subscribe = (listener: Listener): (() => void) => {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
};

const getSnapshot = (): string => currentMessage;

/** Politely announce a message to assistive tech via the mounted region. */
export function announce(message: string): void {
  currentMessage = message;
  for (const listener of listeners) {
    listener();
  }
}

/**
 * Returns the polite announcer. A hook (not a bare import) so consumers read
 * as UI code and the mechanism can gain context later without a call-site
 * change.
 */
export function useLiveAnnouncer(): (message: string) => void {
  return announce;
}

// Visually hidden but present for assistive tech — never `display:none`
// (which would drop it from the accessibility tree).
const visuallyHiddenStyle: CSSProperties = {
  position: "absolute",
  width: "1px",
  height: "1px",
  padding: 0,
  margin: "-1px",
  overflow: "hidden",
  clip: "rect(0 0 0 0)",
  whiteSpace: "nowrap",
  border: 0,
};

/**
 * The single polite live region. Mount exactly once (in `App`). Speaks
 * whatever was last announced, atomically, without interrupting.
 */
export function LiveRegion(): ReactElement {
  const message = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);

  return (
    <div
      role="status"
      aria-live="polite"
      aria-atomic="true"
      style={visuallyHiddenStyle}
    >
      {message}
    </div>
  );
}
