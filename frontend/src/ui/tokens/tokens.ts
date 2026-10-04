// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * NetRoll design tokens — the single typed source of truth.
 *
 * Values are transcribed VERBATIM from the DESIGN.md frontmatter (the locked
 * visual-identity spine); `tokens.css` mirrors them 1:1 and a drift test welds
 * the two together. Dark is the product default theme; every color token
 * carries a hand-designed (not derived) light value.
 */

/** The two first-class themes. Dark is the default. */
export type Theme = "dark" | "light";

/** A color token: one CSS color per theme, same semantic meaning in both. */
export interface ColorToken {
  readonly dark: string;
  readonly light: string;
}

const color = (dark: string, light: string): ColorToken => ({ dark, light });

/**
 * The full NetRoll token map: per-theme colors plus the theme-independent
 * typography ramp, spacing scale, rounded scale, and motion budget.
 */
export const tokens = {
  colors: {
    // Core surfaces & ink
    bg: color("#0D1117", "#EEF2F5"),
    surface: color("#161B22", "#FFFFFF"),
    surface2: color("#1C232E", "#F1F5F8"),
    border: color("#2A313C", "#D3DCE3"),
    text: color("#D6DEE8", "#16202B"),
    textMuted: color("#8592A3", "#566574"),

    // Brand cyan (live / brand / your-turn signature) — cyan in BOTH themes
    accent: color("#2BC8DE", "#0C8599"),
    accentDeep: color("#0E7C8C", "#0B6E7D"),
    accentInk: color("#7DE3F2", "#0A5A67"),

    // Working-cursor coral (station being worked — never fights cyan)
    cursor: color("#FF7A59", "#E8590C"),
    cursorInk: color("#FFB59E", "#B8460A"),

    // Connection status — live (cyan in dark, green in light: RATIFIED)
    liveText: color("#6FE0F0", "#1E6B2E"),
    liveFill: color("rgba(43,200,222,.12)", "#E7F6EC"),
    liveBorder: color("#1C5C66", "#BFE6C9"),
    liveDot: color("#2BC8DE", "#2B8A3E"),

    // Connection status — catching-up (amber)
    catchText: color("#F0B24A", "#8A6410"),
    catchFill: color("rgba(233,168,58,.12)", "#FDF3DD"),
    catchBorder: color("#6B531D", "#F0DCA6"),

    // Connection status — out-of-sync (red)
    syncText: color("#FF8A8E", "#B42318"),
    syncFill: color("rgba(242,86,91,.12)", "#FEECEB"),
    syncBorder: color("#7A2E31", "#F4C7C4"),
    syncSolid: color("#F2565B", "#C92A2A"),

    // Connection status — net-paused (slate)
    pauseText: color("#A6B2C6", "#45505E"),
    pauseFill: color("rgba(85,98,122,.16)", "#EBEEF2"),
    pauseBorder: color("#3B455A", "#D4DBE2"),

    // Entry / roster semantics
    staying: color("#45D06A", "#1E7A34"),
    warn: color("#F0B24A", "#8A6410"),

    // Source badges — self (cyan-tinted) / staff (amber-tinted)
    selfText: color("#7DDDEB", "#0A5A67"),
    selfFill: color("rgba(43,200,222,.10)", "#E6F4F6"),
    selfBorder: color("#1C5C66", "#B7DEE4"),
    staffText: color("#F0B24A", "#8A6410"),
    staffFill: color("rgba(233,168,58,.10)", "#FBF1D9"),
    staffBorder: color("#6B531D", "#EAD6A0"),

    // Frequency pill
    freqText: color("#8CE6F2", "#0A5A67"),
    freqFill: color("#0C1B1E", "#E6F4F6"),
    freqBorder: color("#143E44", "#B7DEE4"),

    // Button label ink on filled cyan (both themes render near-white)
    onAccent: color("#EAFEFF", "#FFFFFF"),

    // Ink on a SOLID accent fill (the your-turn chip): near-black on dark,
    // where the fill is bright cyan; white on light, where it is a mid teal.
    accentContrast: color("#04222A", "#FFFFFF"),

    // Modal scrim — theme-split, since one opacity cannot serve both bases.
    modalBackdrop: color("rgba(4,8,12,.66)", "rgba(20,40,60,.44)"),

    // Device/browser chrome behind a framed panel. On light it deliberately
    // equals `bg` — a darker chrome band would read as a dark-theme artifact
    // on a daylight surface.
    chrome: color("#0B0F14", "#EEF2F5"),

    // Theme toggle (one of the two rationed pill shapes). The knob is the
    // accent on dark and plain white on light so it reads as the moved
    // affordance against its own track in both themes.
    toggleTrack: color("#1C232E", "#D3DCE3"),
    knob: color("#2BC8DE", "#FFFFFF"),
  },

  // Depth is tonal layering first, shadow second (DESIGN.md § Elevation &
  // Depth). These two are specified in that section's prose and in the locked
  // mockup's theme scope rather than the DESIGN.md frontmatter `colors:` map —
  // they are not colors, so they carry their own group.
  elevation: {
    shadow: color(
      "0 20px 50px rgba(0,0,0,.45)",
      "0 1px 2px rgba(20,40,60,.06), 0 10px 30px rgba(20,40,60,.08)",
    ),
    headGrad: color(
      "linear-gradient(180deg, #141A22, #12171E)",
      "linear-gradient(180deg, #F7FAFC, #FFFFFF)",
    ),
  },

  typography: {
    sans: {
      fontFamily:
        '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif',
      lineHeight: "1.45",
    },
    // The ONLY heritage nod. Reserved for callsigns, frequency, signal
    // reports, timestamps, elapsed clock, and grid squares — never body copy.
    mono: {
      fontFamily: 'ui-monospace, "SF Mono", Menlo, Consolas, monospace',
      letterSpacing: "0.01em",
    },
    sessionTitle: {
      fontSize: "21px",
      fontSizePhone: "17px",
      fontWeight: "700",
      letterSpacing: "-0.01em",
      lineHeight: "1.2",
    },
    wordmark: {
      fontSize: "16px",
      fontWeight: "800",
      letterSpacing: "-0.01em",
    },
    callsign: { fontSize: "16px", fontWeight: "800" },
    report: { fontSize: "15px", fontWeight: "800" },
    body: { fontSize: "14px", lineHeight: "1.45" },
    meta: { fontSize: "12px", fontWeight: "400" },
    labelCaps: { fontSize: "11px", fontWeight: "800", letterSpacing: "0.06em" },
    microCaps: { fontSize: "10px", fontWeight: "800", letterSpacing: "0.03em" },
    keycap: { fontSize: "11px", fontWeight: "800" },
  },

  spacing: {
    1: "4px",
    2: "8px",
    3: "12px",
    4: "16px",
    5: "20px",
    6: "26px",
    gutter: "12px",
    rowY: "14px",
    rowX: "20px",
    pageX: "22px",
    statusBar: "4px",
  },

  rounded: {
    sm: "6px",
    md: "9px",
    lg: "12px",
    xl: "16px",
    // Rationed to exactly ConnectionStatus + the theme toggle.
    full: "9999px",
    keycap: "5px",
    modal: "14px",
    freq: "10px",
  },

  motion: {
    durationFast: "140ms",
    ease: "ease-out",
    livePulse: "2s",
    cursorWashFade: "200ms",
  },
} as const;

// The stylesheet consumes the motion budget under these names (Task-level
// contract with tokens.css); the map keeps token keys aligned to DESIGN.md.
const motionVariableNames = {
  durationFast: "--motion-fast",
  ease: "--motion-ease",
  livePulse: "--motion-live-pulse",
  cursorWashFade: "--motion-cursor-wash",
} as const satisfies Record<keyof typeof tokens.motion, string>;

const kebabCase = (key: string): string =>
  key.replace(/([a-z])([A-Z0-9])/g, "$1-$2").toLowerCase();

/**
 * Flatten the token map into CSS custom-property pairs for one theme.
 *
 * Variable names are identical across themes — only the values swap — so
 * components consume `var(--live-dot)` and never know which theme is active.
 */
export function cssVariables(theme: Theme): Record<string, string> {
  const variables: Record<string, string> = {};

  for (const [name, token] of Object.entries(tokens.colors)) {
    variables[`--${kebabCase(name)}`] = token[theme];
  }
  for (const [name, value] of Object.entries(tokens.spacing)) {
    variables[`--space-${kebabCase(name)}`] = value;
  }
  for (const [name, value] of Object.entries(tokens.rounded)) {
    variables[`--rounded-${kebabCase(name)}`] = value;
  }
  for (const [name, value] of Object.entries(tokens.motion)) {
    variables[motionVariableNames[name as keyof typeof tokens.motion]] = value;
  }
  for (const [name, token] of Object.entries(tokens.elevation)) {
    variables[`--${kebabCase(name)}`] = token[theme];
  }

  return variables;
}
