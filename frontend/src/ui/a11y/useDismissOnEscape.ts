// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useRef } from "react";

/**
 * Keyboard-operability contract for NetRoll, documented here so every later
 * surface inherits it:
 * - Tab order follows the DOM; no positive `tabIndex`.
 * - Focus is always visible (the `:focus-visible` accent ring in index.css).
 * - Native controls activate on Enter/Space (use real `<button>`/`<a>`).
 * - Escape dismisses the topmost (most-recently opened) dismissible surface.
 *
 * This hook is the Escape half of that contract. It is intentionally NOT a
 * Modal — the first (check-in detail
 * modal) consumes this hook plus a focus trap. When several dismissibles are
 * open, only the topmost registration fires, so Escape peels one layer at a
 * time.
 */

// Shared registration order across all hook instances; the last entry is
// "topmost" and is the only one that handles an Escape.
const registrations: symbol[] = [];

/**
 * Invoke `onDismiss` when Escape is pressed, but only while this registration
 * is the topmost active one. Pass `active = false` to suspend it without
 * unmounting.
 */
export function useDismissOnEscape(
  onDismiss: () => void,
  active = true,
): void {
  const callbackRef = useRef(onDismiss);
  callbackRef.current = onDismiss;

  useEffect(() => {
    if (!active) {
      return;
    }
    const token = Symbol("dismiss-on-escape");
    registrations.push(token);

    const onKeyDown = (event: KeyboardEvent): void => {
      if (
        event.key === "Escape" &&
        registrations[registrations.length - 1] === token
      ) {
        callbackRef.current();
      }
    };
    document.addEventListener("keydown", onKeyDown);

    return () => {
      document.removeEventListener("keydown", onKeyDown);
      const index = registrations.indexOf(token);
      if (index !== -1) {
        registrations.splice(index, 1);
      }
    };
  }, [active]);
}
