// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { cloneElement, useId, useState } from "react";
import type { CSSProperties, ReactElement, ReactNode } from "react";

import { tokens } from "../tokens/tokens";
import { motionDuration } from "../tokens/motion";

/**
 * Accessible tooltip primitive: its content is reachable on hover,
 * on focus, AND on focus-within (a nested control gaining focus), so keyboard
 * users can reach the same wayfinding help pointer users get. The bubble is
 * associated to its trigger via `aria-describedby`, and its reveal routes
 * through `motionDuration()` to inherit the welded reduced-motion behavior.
 */

const wrapperStyle: CSSProperties = {
  position: "relative",
  display: "inline-flex",
};

const bubbleStyle: CSSProperties = {
  position: "absolute",
  bottom: "calc(100% + var(--space-1))",
  left: "50%",
  transform: "translateX(-50%)",
  padding: "var(--space-1) var(--space-2)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-sm)",
  fontSize: tokens.typography.meta.fontSize,
  whiteSpace: "nowrap",
  zIndex: 1,
};

export interface TooltipProps {
  /** The help content revealed on hover/focus/focus-within. */
  readonly content: ReactNode;
  /** The single trigger element the tooltip describes. */
  readonly children: ReactElement<{ "aria-describedby"?: string }>;
}

/** Hover/focus/focus-within tooltip wrapping its trigger. */
export function Tooltip({ content, children }: TooltipProps): ReactElement {
  const [visible, setVisible] = useState(false);
  const tooltipId = useId();

  const show = (): void => setVisible(true);
  const hide = (): void => setVisible(false);

  // Associate the description with the actual trigger element (not the
  // wrapper) so a screen reader announces it on focus.
  const trigger = visible
    ? cloneElement(children, { "aria-describedby": tooltipId })
    : children;

  return (
    // onFocus/onBlur bubble from descendants in React, so a nested control
    // gaining focus (focus-within) reveals the tooltip just like direct focus.
    <span
      style={wrapperStyle}
      onMouseEnter={show}
      onMouseLeave={hide}
      onFocus={show}
      onBlur={hide}
    >
      {trigger}
      {visible && (
        <span
          role="tooltip"
          id={tooltipId}
          style={{
            ...bubbleStyle,
            transition: `opacity ${motionDuration("durationFast")} var(--motion-ease)`,
          }}
        >
          {content}
        </span>
      )}
    </span>
  );
}
