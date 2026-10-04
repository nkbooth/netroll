// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { expectNoAxeViolations } from "../../test/axe";
import { Panel, PanelColumnHeads } from "./Panel";

describe("Panel", () => {
  it("frames its children with the theme's elevation token", () => {
    render(
      <Panel>
        <p data-testid="body">roster</p>
      </Panel>,
    );

    const panel = screen.getByTestId("body").closest("section");
    expect(panel).toHaveStyle({
      background: "var(--surface)",
      boxShadow: "var(--shadow)",
    });
  });

  it("clips content to the rounded frame so full-bleed rows keep the corners", () => {
    render(
      <Panel>
        <p data-testid="body">roster</p>
      </Panel>,
    );

    expect(screen.getByTestId("body").closest("section")).toHaveStyle({
      overflow: "hidden",
    });
  });

  it("gives a titled panel a tonal header band separated by a hairline", () => {
    render(
      <Panel title="On the air now">
        <p>roster</p>
      </Panel>,
    );

    const heading = screen.getByRole("heading", { name: /on the air now/i });
    const band = heading.parentElement;
    expect(band).toHaveStyle({ background: "var(--head-grad)" });
    expect(band?.style.borderBottom).toBe("1px solid var(--border)");
  });

  it("omits the header band entirely when untitled, rather than banding empty space", () => {
    render(
      <Panel>
        <p data-testid="body">roster</p>
      </Panel>,
    );

    expect(screen.queryByRole("heading")).not.toBeInTheDocument();
    // The body is the panel's first child: nothing precedes it.
    const panel = screen.getByTestId("body").closest("section");
    expect(panel?.firstElementChild).toBe(screen.getByTestId("body"));
  });

  it("titles at heading level 2 by default and honors an explicit level", () => {
    const { unmount } = render(<Panel title="Upcoming">rows</Panel>);
    expect(screen.getByRole("heading", { level: 2 })).toBeInTheDocument();
    unmount();

    render(
      <Panel title="Upcoming" titleLevel={3}>
        rows
      </Panel>,
    );
    expect(screen.getByRole("heading", { level: 3 })).toBeInTheDocument();
  });

  it("names the section with its own title so the landmark is identifiable", () => {
    render(<Panel title="Upcoming">rows</Panel>);

    // A titled panel is a real region: AT should reach it by name rather than
    // relying on the caller to repeat the title in an aria-label.
    expect(
      screen.getByRole("region", { name: /upcoming/i }),
    ).toBeInTheDocument();
  });

  it("seats an aside in the same header band as the title", () => {
    render(
      <Panel title="On the air now" headerAside={<span>3 live</span>}>
        rows
      </Panel>,
    );

    const heading = screen.getByRole("heading", { name: /on the air now/i });
    expect(heading.parentElement).toContainElement(screen.getByText("3 live"));
  });

  it("merges caller style without dropping the frame", () => {
    render(
      <Panel style={{ marginTop: "40px" }}>
        <p data-testid="body">rows</p>
      </Panel>,
    );

    expect(screen.getByTestId("body").closest("section")).toHaveStyle({
      marginTop: "40px",
      background: "var(--surface)",
    });
  });

  it("has no accessibility violations", async () => {
    const { container } = render(
      <Panel title="Upcoming">
        <p>rows</p>
      </Panel>,
    );
    await expectNoAxeViolations(container);
  });
});

describe("PanelColumnHeads", () => {
  const TEMPLATE = "26px 1.9fr 92px 74px";

  it("lays the labels out on the caller's column template", () => {
    render(
      <PanelColumnHeads
        template={TEMPLATE}
        labels={["#", "Net", "Freq", "When"]}
      />,
    );

    const strip = screen.getByTestId("panel-column-heads");
    expect(strip).toHaveStyle({
      display: "grid",
      gridTemplateColumns: TEMPLATE,
    });
    expect(strip.children).toHaveLength(4);
  });

  it("renders as a presentational strip, not a data table AT will announce", () => {
    render(
      <PanelColumnHeads template={TEMPLATE} labels={["#", "Net"]} />,
    );

    // Rows below are cards/list items, so a columnheader role here would
    // promise a table structure that does not exist.
    expect(screen.queryByRole("columnheader")).not.toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("right-aligns any label the caller marks trailing", () => {
    render(
      <PanelColumnHeads
        template={TEMPLATE}
        labels={["#", "Net", { label: "When", align: "end" }]}
      />,
    );

    expect(screen.getByText("When")).toHaveStyle({ textAlign: "right" });
    expect(screen.getByText("Net")).toHaveStyle({ textAlign: "left" });
  });
});
