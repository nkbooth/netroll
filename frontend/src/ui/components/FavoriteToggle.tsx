// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useState } from "react";
import type { CSSProperties, ReactElement } from "react";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../../features/auth/authApi";
import type { Problem } from "../../features/auth/authApi";
import { tokens } from "../tokens/tokens";

const wrapperStyle: CSSProperties = {
  display: "inline-flex",
  flexDirection: "column",
  gap: "var(--space-1)",
};

const buttonStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  padding: "var(--space-1)",
  background: "transparent",
  border: "none",
  color: "var(--text-muted)",
  cursor: "pointer",
};

const errorStyle: CSSProperties = {
  color: "var(--warn)",
  fontSize: tokens.typography.meta.fontSize,
};

/** Filled when favorited (accent star), outline otherwise — state is carried
 * by the icon SHAPE, not color alone; `aria-pressed` carries it for
 * assistive tech. */
const StarIcon = ({ filled }: { filled: boolean }): ReactElement => (
  <svg
    data-testid="favorite-icon"
    data-icon={filled ? "star-filled" : "star-outline"}
    aria-hidden="true"
    width="18"
    height="18"
    viewBox="0 0 24 24"
    fill={filled ? "var(--accent)" : "none"}
    stroke="currentColor"
    strokeWidth="2"
    strokeLinejoin="round"
  >
    <path d="M12 2.5l2.9 6 6.6.6-5 4.3 1.5 6.5-6-3.5-6 3.5 1.5-6.5-5-4.3 6.6-.6z" />
  </svg>
);

export interface FavoriteToggleProps {
  /** The current favorite state — the source of truth is the parent, which
   * updates it only AFTER the server confirms (await server, then reflect). */
  readonly favorited: boolean;
  /** Performs the favorite/unfavorite write for `next` and, on success, has the
   * parent update `favorited`. A rejection leaves `favorited` unchanged. */
  readonly onToggle: (next: boolean) => Promise<void>;
  /** Constant accessible label (ARIA APG toggle-button pattern — state lives in
   * `aria-pressed`, not the name). */
  readonly label?: string;
}

/**
 * Accessible favorite toggle: a single `aria-pressed` button whose star icon
 * swaps filled/outline with state (modeled on `ThemeToggle`). Follows the
 * "await server, then reflect" posture — it never shows an unconfirmed state.
 * On a failed write it surfaces the mapped problem message and leaves the
 * state unchanged (the parent, which owns `favorited`, only advances it on
 * success). Tokens-only styling.
 */
export function FavoriteToggle({
  favorited,
  onToggle,
  label = "Favorite this net",
}: FavoriteToggleProps): ReactElement {
  const [pending, setPending] = useState(false);
  // null = no error yet; a Problem (or undefined for a network failure) once a
  // write has failed — the tri-state used across the net surfaces.
  const [error, setError] = useState<Problem | undefined | null>(null);

  const handleClick = async (): Promise<void> => {
    if (pending) {
      return;
    }
    setPending(true);
    setError(null);
    try {
      await onToggle(!favorited);
    } catch (caught) {
      setError(caught instanceof ProblemError ? caught.problem : undefined);
    } finally {
      setPending(false);
    }
  };

  return (
    <span style={wrapperStyle}>
      <button
        type="button"
        aria-pressed={favorited}
        aria-label={label}
        disabled={pending}
        onClick={() => void handleClick()}
        style={buttonStyle}
      >
        <StarIcon filled={favorited} />
      </button>
      {error !== null && (
        <span role="alert" style={errorStyle}>
          {messageForProblem(error)}
        </span>
      )}
    </span>
  );
}
