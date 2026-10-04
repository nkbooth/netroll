// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { tokens } from "../tokens/tokens";
import { DescriptionPreview } from "./DescriptionPreview";

/** The two-line bound as jsdom serialises it. Derived from the same token the
 * component derives its bound from, and from the same line count, so changing
 * either moves the component and this expectation apart. */
const twoLineBound = `calc(${Number(tokens.typography.sans.lineHeight) * 2}em)`;

describe("DescriptionPreview", () => {
  it("renders a node when the net has a description", () => {
    const { container } = render(
      <DescriptionPreview description="Weekly traffic handling for the section." />,
    );

    expect(container.firstChild).not.toBeNull();
  });

  it("renders nothing at all for a null description", () => {
    const { container } = render(<DescriptionPreview description={null} />);

    // An emitted-but-empty element still contributes margin and is still a
    // node a screen reader walks — "no gap" is only provable as absence.
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing at all for an empty-string description", () => {
    const { container } = render(<DescriptionPreview description="" />);

    expect(container.firstChild).toBeNull();
  });

  it("bounds the preview to a two-line box derived from the type ramp", () => {
    const { container } = render(
      <DescriptionPreview description={"one\ntwo\nthree\nfour\nfive"} />,
    );
    const preview = container.firstChild as HTMLElement;

    // The bound is an exact multiple of a line-height set on this same
    // element, which is what makes it cut at a line boundary rather than
    // through a row of glyphs.
    expect(preview).toHaveStyle({
      lineHeight: tokens.typography.sans.lineHeight,
      maxHeight: twoLineBound,
      overflow: "hidden",
    });
  });

  it("carries the webkit line-clamp polish alongside the enforced bound", () => {
    const { container } = render(
      <DescriptionPreview description={"one\ntwo\nthree"} />,
    );
    const preview = container.firstChild as HTMLElement;

    // The ellipsis half of the clamp. `maxHeight` is what is enforced; these
    // three are what make a clipped preview read as clipped rather than as a
    // sentence that stopped.
    expect(preview.style.display).toBe("-webkit-box");
    expect(preview.style.getPropertyValue("-webkit-box-orient")).toBe("vertical");
    expect(preview.style.getPropertyValue("-webkit-line-clamp")).toBe("2");
  });

  it("does NOT preserve the author's line breaks in the listing", () => {
    const { container } = render(
      <DescriptionPreview description={"first paragraph\n\nsecond paragraph"} />,
    );
    const preview = container.firstChild as HTMLElement;

    // Inside a two-line clamp, a preserved blank line spends the whole budget
    // on line 1 and the preview says nothing. The collapse is what makes the
    // clamp useful — the detail page does the opposite job on the same
    // data, deliberately.
    expect(preview.style.whiteSpace).not.toBe("pre-line");
    expect(preview.style.whiteSpace).not.toBe("pre-wrap");
  });

  it("wraps an unbroken run rather than letting it overflow its column", () => {
    const { container } = render(
      <DescriptionPreview description={"x".repeat(2000)} />,
    );

    expect(container.firstChild as HTMLElement).toHaveStyle({
      overflowWrap: "anywhere",
    });
  });

  it("is a paragraph with no inherited UA margin", () => {
    const { container } = render(<DescriptionPreview description="a net" />);
    const preview = container.firstChild as HTMLElement;

    // A bare <p> inherits the UA's `1em 0` — that IS the forbidden gap,
    // showing up on the nets that DO have a description. The `margin`
    // shorthand reads back empty once `margin-top` is a custom property, so
    // the surviving longhand is what carries the proof.
    expect(preview.tagName).toBe("P");
    expect(preview.style.marginBottom).toBe("0px");
  });
});
