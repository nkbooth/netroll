# NetRoll frontend

The NetRoll SPA — React 19 and TypeScript on Vite, built to `dist/` and served by the
`netroll-app` binary in production. This file orients you inside `frontend/`; the full
development guide is [`CONTRIBUTING.md`](../CONTRIBUTING.md).

## Scripts

| Command | What it does |
|---------|--------------|
| `npm run dev` | Vite dev server on 5173. |
| `npm run build` | `tsc -b` then `vite build`. A type error fails the build. |
| `npm run lint` | Oxlint. |
| `npm test` | Vitest, once. |
| `npm run test:e2e` | Playwright. Needs `npx playwright install --with-deps chromium` first. |
| `npm run preview` | Serve the built bundle. |

Run these inside the devcontainer. The dev server expects the backend running and Postgres up
on the host — see [CONTRIBUTING.md](../CONTRIBUTING.md#development-environment).

## Layout

| Path | What's in it |
|------|--------------|
| `src/features/` | Feature slices: `auth`, `nets`, `session`, `discovery`, `profile`, `consent`, `abuse`, `appConfig`, `botMitigation`. |
| `src/ui/tokens/` | Design tokens. Every semantic token has a dark and a light value. |
| `src/ui/components/` | Shared components — RosterEntry, WorkingCursor, ConnectionStatus, YourTurnIndicator, ReplayingState. |
| `src/ui/layout/` | Responsive layout primitives. |
| `src/ui/a11y/` | Keyboard, live-region, and status-display helpers. |
| `src/errors/` | RFC 9457 problem+json to user-facing message mapping. |
| `src/router.tsx` | Routes. React Router 8 in SPA mode, not Framework Mode. |
| `e2e/` | Playwright specs, including the reconnect scenario. |

## Conventions

**The session store mirrors the Rust domain.** A single Zustand store folds session events in
`seq` order, implementing the same state machines `netroll-domain` declares — the Entry Lifecycle
and the Connection Lifecycle. Change one side and you must change the other; that shared contract
is what stops client and server drifting. See
[ARCHITECTURE.md](../ARCHITECTURE.md).

**Dark is the default theme.** Tokens carry both values, and the theme choice persists across
reloads.

**Status is never colour alone.** Every status display shows colour, icon, and label together,
and `prefers-reduced-motion` disables transitions and the live-dot pulse. CI fails the build on
WCAG 2.1 AA violations of load-bearing UI.

**The server is the authority on errors.** Client-side validation is UX guidance only. Known
problem `type` slugs map to plain messages; unmapped failures degrade to a generic one rather
than showing internals.

**Tests assert behavior, not text.** Test state transitions and mapping logic, not rendered
strings — a copy change must not break a test.

## Related

- [Contributing: environment, gates and rules](../CONTRIBUTING.md)
- [Architecture](../ARCHITECTURE.md)
- [Vocabulary](../docs/reference/vocabulary.md)
- [WebSocket protocol](../docs/reference/websocket-protocol.md)
- [Favicon sources](public/README-favicon.md)
