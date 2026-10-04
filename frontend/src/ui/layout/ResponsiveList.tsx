// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useSyncExternalStore } from "react";
import type { CSSProperties, ReactElement, ReactNode } from "react";

import {
  BREAKPOINT_OPERATOR,
  layoutModeForWidth,
} from "./responsive";
import type { LayoutInfo } from "./responsive";

/**
 * Responsive layout primitive: renders its children in row or
 * stacked-card mode using the pure `layoutModeForWidth` mapping, switching as
 * the viewport crosses the breakpoints. Wide content scrolls inside THIS
 * container (`overflow-x: auto`), never the page body. Styled from tokens
 * only. This is the inheritable baseline — its first real consumers are
 * Discovery and the live roster; centered
 * single-column forms have no list content and are intentionally not wrapped.
 */

const subscribeToWidth = (onChange: () => void): (() => void) => {
  window.addEventListener("resize", onChange);
  return () => window.removeEventListener("resize", onChange);
};

/**
 * The current layout decision, re-read as the viewport crosses a breakpoint.
 * Exported for surfaces that must render *around* a `ResponsiveList` in step
 * with it — a column-head strip, for one, labels columns that only exist in
 * row mode.
 */
export const useLayout = (): LayoutInfo => {
  const width = useSyncExternalStore(
    subscribeToWidth,
    () => window.innerWidth,
    () => BREAKPOINT_OPERATOR,
  );
  return layoutModeForWidth(width);
};

// The scroll boundary: wide content stays inside here so the body never
// scrolls horizontally.
const containerStyle: CSSProperties = {
  overflowX: "auto",
  maxWidth: "100%",
};

const rowStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
};

const stackedStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-3)",
};

export interface ResponsiveListProps {
  readonly children: ReactNode;
}

/** Row/stacked-card list container driven by the viewport breakpoints. */
export function ResponsiveList({ children }: ResponsiveListProps): ReactElement {
  const { mode, density } = useLayout();

  return (
    <div
      data-testid="responsive-list"
      data-layout-mode={mode}
      data-layout-density={density}
      style={containerStyle}
    >
      <div style={mode === "row" ? rowStyle : stackedStyle}>{children}</div>
    </div>
  );
}
