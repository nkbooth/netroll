// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement, ReactNode } from "react";

/**
 * Shared bordered/surface card chrome used across the design handoff's mock
 * surfaces (discovery hero, session header, auth cards, profile, etc). Only
 * the surface/border/radius are opinionated — callers own padding, width, and
 * layout via `style`, since those vary per surface.
 */

export interface CardProps {
  readonly children: ReactNode;
  readonly style?: CSSProperties;
}

const cardStyle: CSSProperties = {
  background: "var(--surface)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-xl)",
};

/** Bordered, surface-colored, rounded-xl card container. */
export function Card({ children, style }: CardProps): ReactElement {
  return <div style={{ ...cardStyle, ...style }}>{children}</div>;
}
