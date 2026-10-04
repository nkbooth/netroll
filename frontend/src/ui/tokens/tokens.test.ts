// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { cssVariables, tokens } from "./tokens";

describe("color tokens", () => {
  it("resolves the Live dot to cyan-family in dark and green-family in light (ratified hue rule)", () => {
    expect(tokens.colors.liveDot.dark).toBe("#2BC8DE");
    expect(tokens.colors.liveDot.light).toBe("#2B8A3E");
  });

  it("resolves the Live text per the same hue rule", () => {
    expect(tokens.colors.liveText.dark).toBe("#6FE0F0");
    expect(tokens.colors.liveText.light).toBe("#1E6B2E");
  });

  it("keeps the your-turn accent cyan in BOTH themes", () => {
    expect(tokens.colors.accent.dark).toBe("#2BC8DE");
    expect(tokens.colors.accent.light).toBe("#0C8599");
  });

  it("exposes a non-empty dark AND light value for every color token", () => {
    const entries = Object.entries(tokens.colors);

    expect(entries.length).toBeGreaterThan(0);
    for (const [name, value] of entries) {
      expect(value.dark, `${name}.dark`).toBeTruthy();
      expect(value.light, `${name}.light`).toBeTruthy();
    }
  });

  it("keeps dark tint fills as verbatim rgba() strings and light fills solid", () => {
    expect(tokens.colors.liveFill.dark).toBe("rgba(43,200,222,.12)");
    expect(tokens.colors.liveFill.light).toBe("#E7F6EC");
  });

  it("inks the your-turn chip against its solid cyan fill in both themes", () => {
    // DESIGN.md § components.your-turn-indicator: chip-text-dark #04222A,
    // chip-text-light = on-accent-light. The chip fill is solid accent, so
    // this ink is NOT --text (which would vanish on cyan).
    expect(tokens.colors.accentContrast.dark).toBe("#04222A");
    expect(tokens.colors.accentContrast.light).toBe(
      tokens.colors.onAccent.light,
    );
    expect(tokens.colors.accentContrast.dark).not.toBe(tokens.colors.text.dark);
  });

  it("scrims the modal backdrop per theme instead of reusing one opacity", () => {
    // DESIGN.md § components.check-in-modal: theme-split backdrops — a dark
    // scrim over a light page reads as a bug, not as depth.
    expect(tokens.colors.modalBackdrop.dark).toBe("rgba(4,8,12,.66)");
    expect(tokens.colors.modalBackdrop.light).toBe("rgba(20,40,60,.44)");
  });

  it("tones the browser/device chrome darker than bg on dark and equal to bg on light", () => {
    expect(tokens.colors.chrome.dark).toBe("#0B0F14");
    expect(tokens.colors.chrome.light).toBe("#EEF2F5");
  });

  it("fills the theme-toggle knob with the brand accent on dark and plain white on light", () => {
    // The knob must read as the ON affordance against its track in both
    // themes: accent-on-surface-2 in dark, white-on-border in light.
    expect(tokens.colors.knob.dark).toBe(tokens.colors.accent.dark);
    expect(tokens.colors.knob.light).toBe("#FFFFFF");
    expect(tokens.colors.toggleTrack.dark).toBe(tokens.colors.surface2.dark);
    expect(tokens.colors.toggleTrack.light).toBe(tokens.colors.border.light);
  });
});

describe("elevation tokens", () => {
  it("uses a single deep drop on dark and a soft double shadow on light", () => {
    expect(tokens.elevation.shadow.dark).toBe("0 20px 50px rgba(0,0,0,.45)");
    expect(tokens.elevation.shadow.light).toBe(
      "0 1px 2px rgba(20,40,60,.06), 0 10px 30px rgba(20,40,60,.08)",
    );
  });

  it("gives panel headers a tonal gradient, not a flat surface fill", () => {
    // Depth comes from tonal layering first (DESIGN.md § Elevation & Depth):
    // the header gradient must be a gradient in both themes, and must not
    // collapse to the panel's own surface color.
    for (const theme of ["dark", "light"] as const) {
      expect(tokens.elevation.headGrad[theme]).toContain("linear-gradient(180deg");
      expect(tokens.elevation.headGrad[theme]).not.toBe(
        tokens.colors.surface[theme],
      );
    }
  });

  it("exposes a non-empty dark AND light value for every elevation token", () => {
    const entries = Object.entries(tokens.elevation);

    expect(entries.length).toBeGreaterThan(0);
    for (const [name, value] of entries) {
      expect(value.dark, `${name}.dark`).toBeTruthy();
      expect(value.light, `${name}.light`).toBeTruthy();
    }
  });
});

describe("scales", () => {
  it("uses exactly the 4/8/12/16/20/26 spacing scale", () => {
    expect([
      tokens.spacing[1],
      tokens.spacing[2],
      tokens.spacing[3],
      tokens.spacing[4],
      tokens.spacing[5],
      tokens.spacing[6],
    ]).toEqual(["4px", "8px", "12px", "16px", "20px", "26px"]);
  });

  it("matches the DESIGN.md motion budget", () => {
    expect(tokens.motion.durationFast).toBe("140ms");
    expect(tokens.motion.ease).toBe("ease-out");
    expect(tokens.motion.livePulse).toBe("2s");
    expect(tokens.motion.cursorWashFade).toBe("200ms");
  });

  it("rations rounded.full to its documented 9999px pill value", () => {
    expect(tokens.rounded.full).toBe("9999px");
  });
});

describe("cssVariables", () => {
  it("emits dark values for the dark theme and light values for the light theme", () => {
    const dark = cssVariables("dark");
    const light = cssVariables("light");

    expect(dark["--live-dot"]).toBe("#2BC8DE");
    expect(light["--live-dot"]).toBe("#2B8A3E");
    expect(dark["--bg"]).toBe("#0D1117");
    expect(light["--bg"]).toBe("#EEF2F5");
  });

  it("uses identical variable names across themes so components never branch on theme", () => {
    expect(Object.keys(cssVariables("dark"))).toEqual(
      Object.keys(cssVariables("light")),
    );
  });

  it("carries the motion budget under the --motion-* names the stylesheet consumes", () => {
    const dark = cssVariables("dark");

    expect(dark["--motion-fast"]).toBe("140ms");
    expect(dark["--motion-ease"]).toBe("ease-out");
    expect(dark["--motion-live-pulse"]).toBe("2s");
    expect(dark["--motion-cursor-wash"]).toBe("200ms");
  });

  it("carries elevation under unprefixed --shadow / --head-grad names", () => {
    const dark = cssVariables("dark");
    const light = cssVariables("light");

    expect(dark["--shadow"]).toBe(tokens.elevation.shadow.dark);
    expect(light["--shadow"]).toBe(tokens.elevation.shadow.light);
    expect(dark["--head-grad"]).toBe(tokens.elevation.headGrad.dark);
    expect(light["--head-grad"]).toBe(tokens.elevation.headGrad.light);
  });
});
