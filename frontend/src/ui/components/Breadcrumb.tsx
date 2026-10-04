// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";

import { tokens } from "../tokens/tokens";

/**
 * Wayfinding breadcrumb primitive: an ordered trail rendered as
 * a named `nav` landmark, the terminal crumb marked `aria-current="page"` and
 * earlier crumbs keyboard-reachable links. Feature-agnostic (`ui/`) —
 * it renders whatever trail it is given from tokens only.
 *
 * The primitive and the discovery root's single `Nets` crumb ship together;
 * the full `Nets › band · category › title › console` trail lands with the
 * live-session surface.
 */

/** One crumb in the trail. The terminal (current) crumb omits `href`. */
export interface Crumb {
  /** Visible crumb text. */
  readonly label: string;
  /** Target for a non-terminal crumb; omit for the current-page crumb. */
  readonly href?: string;
}

export interface BreadcrumbProps {
  /** The trail, root-first; the last entry is the current page. */
  readonly items: readonly Crumb[];
  /** Accessible name for the `nav` landmark. */
  readonly label?: string;
}

const navStyle: CSSProperties = {
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
};

const listStyle: CSSProperties = {
  display: "flex",
  flexWrap: "wrap",
  alignItems: "center",
  gap: "var(--space-1)",
  listStyle: "none",
  margin: 0,
  padding: 0,
};

const linkStyle: CSSProperties = {
  color: "var(--accent-ink)",
};

const currentStyle: CSSProperties = {
  color: "var(--text)",
  fontWeight: 700,
};

const separatorStyle: CSSProperties = {
  color: "var(--text-muted)",
};

/** Ordered breadcrumb trail; last crumb is the current page. */
export function Breadcrumb({
  items,
  label = "Breadcrumb",
}: BreadcrumbProps): ReactElement {
  return (
    <nav aria-label={label} style={navStyle}>
      <ol style={listStyle}>
        {items.map((crumb, index) => {
          const isCurrent = index === items.length - 1;
          return (
            <li
              key={`${crumb.label}-${index}`}
              style={{ display: "inline-flex", alignItems: "center", gap: "var(--space-1)" }}
            >
              {index > 0 && (
                <span aria-hidden="true" style={separatorStyle}>
                  ›
                </span>
              )}
              {isCurrent || crumb.href === undefined ? (
                <span aria-current={isCurrent ? "page" : undefined} style={currentStyle}>
                  {crumb.label}
                </span>
              ) : (
                <a href={crumb.href} style={linkStyle}>
                  {crumb.label}
                </a>
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}
