# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Production image for NetRoll by N1CCK — ONE deployable artifact: the
# netroll-app binary serving the built SPA.
#
# Built two ways, so plain OCI/Dockerfile syntax only (no podman extensions):
#   - CI: buildx via docker/build-push-action (.github/workflows/deploy.yml)
#   - locally: podman build -f Containerfile .
#
# The dev image lives in .devcontainer/Containerfile and is a different thing;
# do not merge the two.

FROM registry.access.redhat.com/ubi9/ubi:latest AS build

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN dnf install -y \
        gcc \
        make \
        openssl-devel \
        pkgconf-pkg-config \
        tar \
        gzip \
    && dnf clean all

# Node 22 LTS from the UBI9 AppStream module. Vite 8 requires >= 22.12 —
# gate the build on it so a stale module snapshot fails here, not at vite.
RUN dnf module enable -y nodejs:22 \
    && dnf install -y nodejs npm \
    && dnf clean all \
    && node -e 'const [maj, min] = process.versions.node.split(".").map(Number); if (maj < 22 || (maj === 22 && min < 12)) { console.error(`node ${process.versions.node} < 22.12 (Vite 8 floor)`); process.exit(1); }'

# rustup pinned by version AND checksum — unlike the dev image's curl|sh,
# a tampered or truncated download fails the production build loudly.
# Toolchain matches backend/rust-toolchain.toml.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
ARG RUSTUP_VERSION=1.29.0
ARG RUSTUP_SHA256=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10
RUN curl --proto '=https' --tlsv1.2 -sSf -o /tmp/rustup-init \
        "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init" \
    && echo "${RUSTUP_SHA256}  /tmp/rustup-init" | sha256sum -c - \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --no-modify-path --default-toolchain 1.97 --profile minimal \
    && rm /tmp/rustup-init

WORKDIR /src

# Backend first: the cargo layer is the expensive one, and frontend edits are
# the common case — this order keeps a frontend-only change from invalidating
# the Rust build cache.
# --locked: lockfiles are law — a build that wants to rewrite Cargo.lock is a
# broken pin and must fail, not "fix" itself.
COPY backend/ backend/
RUN cd backend && cargo build --release --locked

COPY frontend/ frontend/
RUN cd frontend && npm ci && npm run build

# Docs site (docs/ + mkdocs.yml) → static HTML served by the same binary at
# /docs, so the "one artifact" rule holds for documentation too. Pinned by
# digest (mkdocs-material, mkdocs 1.6.1) for the same reason rustup is pinned
# above: a build-time toolchain must not drift silently. Nothing from this
# stage reaches the runtime image except generated HTML/CSS/JS, so it adds no
# Python to the shipped attack surface and nothing for cargo/npm SBOMs to miss.
# --strict makes a broken internal link fail the build, matching the
# clippy -D warnings posture.
FROM docker.io/squidfunk/mkdocs-material@sha256:51b87149d227691486b5f08993d28c65ca7e4990010664b697265b8e6fcd5287 AS docs
# The digest above is ALSO pinned in .github/workflows/ci.yml (the frontend
# job's `mkdocs build --strict` step), which runs this same build on
# every PR. Bump both together, or the PR check and this release build become
# two definitions of the docs build.

# overrides/ is theme.custom_dir and is a REQUIRED build input: it
# externalises the theme's inline bootstrap scripts so the docs work under the
# production CSP. Dropping this COPY does not ship a quietly broken docs site
# — mkdocs refuses to start: "Config value 'theme': The path set in custom_dir
# ('/src/overrides') does not exist." That is deliberate. The local preview
# bind-mounts the whole repo, so a missing COPY would otherwise be invisible
# until production; naming the directory in mkdocs.yml is what converts that
# into a build failure. Bumping the digest above means re-diffing the vendored
# partials in overrides/ against the new upstream; they carry their provenance
# in their own headers.
#
# docs/ also carries a vendored third-party bundle,
# docs/assets/javascripts/mermaid-11.16.1.min.js, wired through extra_javascript
# so the theme's mermaid integration finds the library same-origin instead of
# fetching it from unpkg.com, which `script-src 'self'` refuses. Unlike
# custom_dir, extra_javascript is NOT validated: mkdocs emits the <script> tag
# for whatever path it is given and --strict still passes, so losing that file
# from the build context WOULD ship a quietly broken docs site (diagrams gone,
# CSP-refused fetch in the console). .containerignore must therefore never grow
# a rule that reaches docs/assets/javascripts/. Bumping the digest above means
# re-pinning mermaid to the major the new bundle expects — see the
# extra_javascript comment in mkdocs.yml.
WORKDIR /src
COPY mkdocs.yml mkdocs.yml
COPY docs/ docs/
COPY overrides/ overrides/
RUN mkdocs build --strict --site-dir /site

# Enforce what the comment above only describes. mkdocs validates custom_dir
# and fails hard when overrides/ is missing, but it does NOT validate the paths
# in extra_javascript or in a <script src> a template emits: it writes the tag
# for whatever path it is given and --strict still exits 0. So a vendored file
# lost from the build context ships a green build and a dark site — the browser
# 404s the script, Material's bundle dies again on __md_get, and the diagrams
# fall back to a CSP-refused unpkg.com fetch. Assert on the artifact instead of
# trusting the context. The grep also fails when the mermaid pin drifts out of
# sync with mkdocs.yml, because the filename carries the version.
RUN grep -q 'mermaid-11.16.1.min.js' /site/live-sessions/live-updates/index.html \
    && test -s /site/assets/javascripts/mermaid-11.16.1.min.js \
    && test -s /site/assets/javascripts/md-mermaid-csp.js \
    && test -s /site/assets/javascripts/md-bootstrap.js \
    && test -s /site/assets/javascripts/md-palette.js \
    && test -s /site/assets/javascripts/md-content.js

FROM registry.access.redhat.com/ubi9/ubi-minimal:latest

# shadow-utils only exists long enough to mint the unprivileged runtime user.
RUN microdnf install -y shadow-utils \
    && useradd --uid 1001 --no-create-home --shell /sbin/nologin netroll \
    && microdnf remove -y shadow-utils \
    && microdnf clean all

COPY --from=build /src/backend/target/release/netroll-app /app/netroll-app
COPY --from=build /src/frontend/dist /app/static

# Docs land INSIDE the SPA bundle dir, so the existing ServeDir resolves
# /docs/* to real files before it ever reaches the index.html fallback — no
# route and no reverse-proxy rule needed (see static.rs). The SPA has no
# /docs client route, so this cannot shadow one.
COPY --from=docs /site /app/static/docs

# Uploaded avatars. The directory is created HERE and owned by the runtime
# user because the image sets no WORKDIR (cwd is `/`) and `/app` belongs to
# root: the binary proves AVATAR_DIR writable at boot, so without this the
# container cannot start at all. Baking it in also gives a mounted named
# volume the right ownership, which it inherits from the image.
RUN mkdir -p /app/data/avatars && chown -R 1001:1001 /app/data

# STATIC_DIR and AVATAR_DIR are set explicitly: the binary validates both at
# boot, and their dev-default relative paths resolve against a working
# directory this image does not have.
# PORT=3000 is the contract with deploy/compose.prod.yml (host 3001 -> 3000)
# and deploy/.env.production.tpl.
ENV PORT=3000 \
    STATIC_DIR=/app/static \
    AVATAR_DIR=/app/data/avatars

EXPOSE 3000

# Docker honours this; podman ignores it for OCI images, which is what the
# release build publishes, so deploy/compose.prod.yml declares the same check.
# The URL hard-codes 3000 because PORT above is the image's contract.
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 \
    CMD ["/usr/bin/curl", "-fsS", "-o", "/dev/null", "http://127.0.0.1:3000/healthz"]

USER netroll
ENTRYPOINT ["/app/netroll-app"]
