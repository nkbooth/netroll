# Working in this repository

NetRoll: a Rust backend, a React frontend, one container image. Maps: `ARCHITECTURE.md` (the
shape), `CONTRIBUTING.md` (the rules), `docs/reference/vocabulary.md` (the words).

## Build and test

- The toolchains live in the devcontainer: `devpod up .` from the repo root opens it.
- Backend commands run through `scripts/dev-cargo.sh "<cargo command>"` from the repo root,
  never a bare `cargo` on the host. The script mounts the repo and the Podman socket the
  integration tests need.
- Frontend commands run from `frontend/` on the host's Node: `npm test`,
  `npx --no-install oxlint`, `npm run build`.
- Docs build with the pinned mkdocs-material image named in `CONTRIBUTING.md`, `--strict`.
- Run gates one at a time, unpiped, and read each exit status. Never run two test suites at
  once.

## Tests first

Write the failing test, confirm it fails for the right reason (a compile error is not a
failure), then implement. Tests assert behaviour and logic, never rendered text or message
strings. A green test proves nothing until you have seen it red.

## Comments

Explain WHY: a constraint, a rejected alternative, a wire contract. Never narrate what the code
does. Never cite a planning identifier. Module headers stay within five lines. Public items get
a doc comment; private ones do not.

## Commits and branches

- Conventional Commits. The subject is the user-visible change; the body is the reason.
- `no-story-refs`: the subject and body must not reference internal planning identifiers
  (epic, story, AC, FR or NFR numbers); describe the user-visible change instead. GitHub issue
  references such as `Refs: #123` are fine.
- No `Co-Authored-By` trailers.
- There are no private branches. Everything pushed is public immediately. Keep work local until
  it is ready, and push named refs (`git push origin HEAD`).
- The `pre-push` hook refuses `wip/*` branches and checks every outgoing commit (gitleaks,
  commitlint, `scripts/check-structure.sh`). `--no-verify` bypasses it; CI is the backstop.

## Do not

- Add a language beyond Rust, TypeScript and Bash.
- Add I/O, an async runtime, `sqlx`, `tokio`, `axum` or `fred` to `netroll-domain`.
- Run `npx <tool>` without `--no-install`; the pinned local tool is the one that matches.
- Run prettier. `oxlint` is the formatter of record and it does not rewrite files.
- Edit an applied migration, or add one with a plain `CREATE INDEX`.
