// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";

import { tokens } from "../tokens/tokens";

const CLAMP_LINES = 2;

// `meta` carries no lineHeight of its own; `sans.lineHeight` ("1.45", = body's)
// is the ramp's only one, and it is UNITLESS — so it and the `em` below both
// resolve against THIS element's font-size, and 1.45 × 2em is exactly two line
// boxes at meta's 12px. Setting it locally is what keeps that exact rather than
// inherited: an ancestor changing its line-height would otherwise turn a
// two-line clamp into a one-and-a-half-line one.
const CLAMP_LINE_HEIGHT = tokens.typography.sans.lineHeight;

const descriptionPreviewStyle: CSSProperties = {
  // A bare <p> inherits the UA's `1em 0`, and that gap is exactly what a net
  // WITHOUT a description must not leave behind.
  margin: 0,
  marginTop: "var(--space-1)",
  fontSize: tokens.typography.meta.fontSize,
  color: "var(--text-muted)",
  lineHeight: CLAMP_LINE_HEIGHT,
  maxHeight: `calc(${CLAMP_LINE_HEIGHT} * ${CLAMP_LINES}em)`,
  overflow: "hidden",
  // MAX_DESCRIPTION_CHARS bounds characters, not words, so a 2000-character
  // run with no space in it is a legal description. `anywhere` (not
  // `break-word`) also shrinks the min-content contribution, which is what a
  // grid cell on `minmax(0, 1.7fr)` needs to stay inside its column.
  overflowWrap: "anywhere",
  display: "-webkit-box",
  WebkitBoxOrient: "vertical",
  WebkitLineClamp: CLAMP_LINES,
};

/**
 * A net's description as a two-line clamped preview, for listing rows that show
 * several nets at once. Renders nothing at all — not an empty element — when
 * there is no description, so a net without one leaves no gap and no
 * placeholder.
 *
 * Three things about it are deliberate and each has been "tidied away" before:
 *
 * - **The public net detail page does NOT use this component.** That page's job
 * is the full text, so it carries its own style object with no clamp. Routing
 * it through here in the name of consistency would both relocate its single
 * description render and truncate the one surface that must not truncate.
 * - **The preview deliberately does NOT preserve the author's line breaks.**
 * Inside a two-line budget, an author who separates paragraphs with a blank
 * line would spend the entire preview on line one plus a blank, and the row
 * would say nothing. Collapsing newlines is what makes the clamp useful; the
 * detail page preserves them instead.
 * - **`maxHeight` and `-webkit-line-clamp` are not redundant.** `maxHeight` is
 * the bound that is actually enforced (and, being an exact multiple of the
 * line-height set on this same element, it cuts at a line boundary);
 * `-webkit-line-clamp` only adds the trailing ellipsis so a clipped preview
 * reads as clipped. Where the prefixed property is not understood, the bound
 * still holds — deleting either one loses a different half of the job.
 *
 * The full text stays in the DOM: the clamp is visual, so screen readers and
 * in-page find still reach the whole description.
 */
export function DescriptionPreview({
  description,
}: {
  description: string | null;
}): ReactElement | null {
  if (description === null || description === "") {
    return null;
  }

  return (
    <p data-testid="net-description-preview" style={descriptionPreviewStyle}>
      {description}
    </p>
  );
}
