# Contributing to NetRoll

The rules a first pull request meets, in the order they bite. The shape of the code is in
[ARCHITECTURE.md](ARCHITECTURE.md); the words it uses are in
[docs/reference/vocabulary.md](docs/reference/vocabulary.md).

## There are no private branches here

The repository is public. Every branch you push and every commit message on it is visible the
moment you push, and a commit stays fetchable by hash after its branch is deleted. Keep
experiments local until they are ready to be read by strangers. `scripts/bootstrap-hooks.sh`
sets `push.default nothing` in your clone, so a bare `git push` refuses and you name the ref you
are publishing every time.

## Tests first, and green

Write the failing test before the code, and watch it fail for the right reason before you make it
pass. A compile error is not a failing test. Tests assert behaviour and logic, never rendered
text or message strings: a test that pins a state transition survives a copy change; one that
pins a sentence does not.

These are the gates a pull request must pass. Run each one on its own, unpiped, and read its exit
status. The backend ones run through `scripts/dev-cargo.sh` from the repository root; the
frontend ones run from `frontend/`, either inside the devcontainer or on a host Node.

```
scripts/dev-cargo.sh "cargo fmt --all -- --check"
scripts/dev-cargo.sh "cargo clippy --workspace --all-targets --locked -- -D warnings"
scripts/dev-cargo.sh "cargo test --workspace --locked --no-fail-fast"
cd frontend && npm test
cd frontend && npx --no-install oxlint
cd frontend && npm run build
```

`dev-cargo.sh` exists because the PostgreSQL integration tests use testcontainers, which needs a
container socket the devcontainer shell does not have. The script is a one-shot container that
mounts the repository and the rootless Podman socket and runs its argument with the working
directory at `backend/`. There are no mocked SQL tests; adapters are tested against a real
PostgreSQL.

The documentation is a gate too, at the same pinned image CI uses:

```
podman run --rm -v "$PWD":/docs:ro docker.io/squidfunk/mkdocs-material@sha256:51b87149d227691486b5f08993d28c65ca7e4990010664b697265b8e6fcd5287 build --strict --site-dir /tmp/site
```

CI adds the security gates (`gitleaks git --redact --verbose`, `cargo audit`, `npm audit`), the
end-to-end Playwright run, commitlint on every commit and on the pull request title, and an SBOM
freshness check. The dependency audits also run weekly against `main`.

## Commit messages

Commits follow [Conventional Commits](https://www.conventionalcommits.org/), enforced by the
`commit-msg` hook and again in CI. The subject describes the user-visible change; the body says
why. Pull request titles are commits too: the repository squash-merges, so the title becomes the
subject of the one commit that reaches `main`, and it is what the release tooling reads.

One rule is ours, and CI prints it as:

> `no-story-refs`: the subject and body must not reference internal planning identifiers (epic,
> story, AC, FR or NFR numbers); describe the user-visible change instead. GitHub issue references
> such as `Refs: #123` are fine.

## Docs move with the code

A user-visible change updates `docs/`. The documentation ships inside the image and is served at
`/docs` by every instance, so `mkdocs build --strict` is a gate: a broken internal link fails the
build, and a new page must be added to the `nav` in `mkdocs.yml` or the build fails on the omitted
file.

## Licence

NetRoll is licensed under the [Reciprocal Public License 1.5](LICENSE.md). It is a reciprocal
licence. The licence's word for running the software is "Deploy", which it defines as using,
serving, sublicensing or distributing it for anything other than your own personal use or
research, including use inside your own business or organisation. When you deploy a modified
version, the licence requires you to make your changes available in source form under the same
licence; its preamble puts it as "once you start running the software you have to start
sharing the software". By opening a pull request you offer your contribution under RPL-1.5,
which is the fourth box in the pull request template.

Every source file opens with the licence's two-line notice, an SPDX identifier and a copyright
line. `scripts/license-notices.sh apply` adds it to a new file, and CI fails a file without it.

## Review

`main` changes only through a pull request: required checks, no force-push, no deletion. Every
pull request is reviewed by the maintainer. The pull request template's four boxes are the
checklist: tests first and green, a conventional subject with no internal references, docs
updated for a user-visible change, and the contribution offered under RPL-1.5.
[SECURITY.md](SECURITY.md) says how to report a vulnerability privately, and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) applies everywhere.

## Development environment

All toolchains live in the devcontainer, so nothing but the prerequisites below has to be
installed on the host.

**Prerequisites on the host:** Podman or Docker with compose, and `devpod`.

1. **Enable the git hooks**, once per clone, on the host:

   ```
   scripts/bootstrap-hooks.sh
   ```

   This points the clone at the repository's hooks (a gitleaks scan that blocks secret-shaped
   strings, and commitlint on every message) and sets `push.default nothing`. Both writes are
   local to the clone and safe to re-run.

   The `pre-push` hook refuses a `wip/*` branch (rename it to `pr/*` when it is ready to be
   seen) and runs gitleaks, commitlint and `scripts/check-structure.sh` over the commits you
   are about to publish. `git push --no-verify` skips it; CI runs the same checks anyway.

2. **Open the devcontainer:**

   ```
   devpod up .
   ```

   The image in `.devcontainer/` provides rustup, Node and npm.

3. **Start the backing services**, on the host:

   ```
   podman compose up -d
   ```

   PostgreSQL listens on 5432 and Mailpit, a mail catcher, on 1025 with a web UI at
   `http://localhost:8025`. Development mail lands there, so magic links do too.

4. **Build the frontend once, then run the backend**, inside the devcontainer, from the
   repository root:

   ```
   (cd frontend && npm install && npm run build)
   set -a; . ./.env.template; set +a; cargo run --manifest-path backend/Cargo.toml
   ```

   The binary serves the built single-page app, so it needs `frontend/dist` to exist: without
   it the app exits at boot with `static dir "frontend/dist" does not exist or has no
   index.html`. Set `STATIC_DIR` to serve from somewhere else. The cargo workspace is
   `backend/`, which is why the manifest path is explicit while the working directory stays at
   the root where `.env.template` lives.

   `.env.template` holds a working plain-text value for every variable. `KEK` and
   `BOT_MITIGATION_SECRET` are blank on purpose: the app boots with QRZ features off and bot
   mitigation disabled until you mint a value (each variable's comment says how).

5. **Run the frontend dev server**, from `frontend/`, to iterate on the SPA:

   ```
   npm run dev
   ```

   It serves from memory on 5173 and never writes `frontend/dist`, so it does not replace the
   build in step 4.

   Vite serves on 5173, which is what `PUBLIC_BASE_URL` in the template points at, so magic
   links generated in development open the dev server. Request one at `http://localhost:5173`
   and find it in Mailpit.

**Troubleshooting.**

- *PostgreSQL refuses to start after an image bump.* PostgreSQL 18 images keep data in a
  version-specific directory under a single `/var/lib/postgresql` mount; the compose file mounts
  there. A volume created against an older `.../data` path makes the entrypoint refuse to start.
- *Mail fails with an authentication error.* Leave `SMTP_USERNAME` empty. An empty username
  selects the unauthenticated transport; a set one makes the app attempt an AUTH exchange that
  Mailpit rejects.
- *The app refuses to boot.* Read the message. A required variable missing, or an optional one
  set to an invalid value, is a deliberate hard boot error; the
  [configuration reference](docs/reference/configuration.md) lists every rule.
- *The app refuses to boot with "migration … was previously applied but is missing in the
  resolved migrations".* Your development database was created by an older migration set. Point
  `DATABASE_URL` at a fresh database (`podman exec netroll-postgres createdb -U netroll <name>`)
  or recreate the volume.
- *Backend tests cannot reach a container runtime.* Run them through `scripts/dev-cargo.sh`,
  not from the devcontainer shell.

## Repository layout

| Path | What is in it |
|---|---|
| `backend/crates/netroll-domain` | The rules: types, state machines, fold and replay, port traits. No I/O. |
| `backend/crates/netroll-adapters` | Implementations of the ports: PostgreSQL, SMTP, the egress client, crypto, callbook lookups. |
| `backend/crates/netroll-app` | The axum binary: HTTP routes, the WebSocket, middleware, jobs, composition. |
| `backend/migrations` | sqlx migrations, applied by the app at boot. One initial schema plus whatever has been added since; forward-only, never edited once applied. A plain `CREATE INDEX` is legal only in the initial schema; every later index is `CREATE INDEX CONCURRENTLY` in a `-- no-transaction` file holding exactly one statement. `backend/migrations/README.md` has the rules and the rebuild recipe. |
| `frontend/src/features` | Feature slices: auth, nets, session, discovery, profile, admin. |
| `frontend/src/ui` | Design tokens, layout primitives, shared components, accessibility helpers. |
| `docs/` | The documentation site, built with MkDocs and served by the app at `/docs`. |
| `deploy/` | The production compose file, the env template for an operator, and the example reverse proxy. |
| `scripts/` | Host-side glue: hooks bootstrap, the cargo runner, the checks CI also runs. |

**Conventions.** API paths are kebab-case and plural; JSON is camelCase; errors are RFC 9457
problem+json with a stable `type` slug. Database names are snake_case and ids are UUIDv7. Events
are named `noun.verb` in the past tense: `checkin.added`, `session.closed`. Errors propagate:
domain errors are typed with `thiserror` and never swallowed. Comments explain why, never what.

## Releases

release-please cuts a release from the Conventional Commits on `main`: `fix:` bumps the patch
version, `feat:` the minor, `feat!:` the major, and `docs:` bumps the patch because the
documentation ships inside the image. A published release builds the container image and
deploys the public instance. Batch documentation edits into one change, or you cut a release
per typo.
