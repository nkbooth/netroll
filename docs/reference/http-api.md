---
title: "HTTP API"
description: "NetRoll's REST endpoints — authentication, accounts, net definitions, sessions, discovery, and admin — with their methods, authorization, and error format."
docType: "reference"
audience: ["contributor"]
lastVerified: "2026-10-03"
---

# HTTP API

!!! note "Hand-maintained"
    This page is written by hand and was verified against the 1.0.0 routes. A generated
    reference is planned. If this page and the server disagree, the server is right.

Every HTTP endpoint the NetRoll backend serves. Paths are kebab-case and plural, request and
response bodies are camelCase JSON, and errors are RFC 9457 problem+json.

This page lists the surface. Request and response field shapes are defined by the handlers in
`backend/crates/netroll-app/src/http/`.

## Conventions

| Aspect | Rule |
|--------|------|
| Base path | All endpoints are under `/api`, except the health probe at `/healthz`. |
| Ids | UUIDv7, in the path. |
| JSON casing | camelCase in both directions. |
| Errors | RFC 9457 `application/problem+json` with a stable `type` slug clients map to messages. |
| Sessions | Server-side and revocable, carried in a Secure, HttpOnly, SameSite cookie. |
| Rate limits | Auth, check-in, lookup, net creation, favoriting, and public reads are throttled. Over the limit returns HTTP 429 with retry information. |

Authorization is capability-based and evaluated server-side against the specific object. No
client-supplied role or id is trusted. A request outside the acting role's capabilities returns
HTTP 403 regardless of what the UI offered.

## Authentication and session

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/api/magic-links` | None | Request a single-use magic link for an email address. Rate-limited per email. |
| `POST` | `/api/sessions` | Magic-link token | Consume a magic link. Verifies the email and establishes a session. |
| `DELETE` | `/api/sessions/current` | Session | Sign out. Revokes the session server-side. |
| `POST` | `/api/consents` | Session | Record consent. Required before gated actions. |
| `POST` | `/api/email-changes` | Change token | Confirm a change of identifying email. Invalidates existing sessions. |
| `GET` | `/api/form-tokens` | None | Issue a bot-mitigation form token and signature. Present only when `BOT_MITIGATION_SECRET` is set. |
| `GET` | `/api/app-config` | None | Client bootstrap configuration — which optional features this instance has enabled. |

## Account

All require a session. Export, check-in history, and delete are deliberately not consent-gated:
reading or removing your own data is a right, available even before consent is recorded.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/accounts/me` | The current account. |
| `DELETE` | `/api/accounts/me` | Request deletion. Enters the 15-minute grace window. |
| `GET` | `/api/accounts/me/export` | Personal data export. |
| `GET` | `/api/accounts/me/check-ins` | The caller's own self-check-in history, newest first. Paginated. Check-ins an operator logged on your behalf are not included, and later edits or removals are not reflected. Each row's way in is described as it stood when that check-in was logged, so a net that moved frequency afterwards does not rewrite it. |
| `PUT` | `/api/accounts/me/callsign` | Reserve or change the callsign. Format-validated and unique. |
| `PUT` | `/api/accounts/me/profile` | Display name, location, and grid. |
| `POST` | `/api/accounts/me/avatar` | Upload an avatar. 1 MB cap; type determined from magic bytes. |
| `DELETE` | `/api/accounts/me/avatar` | Remove the avatar and fall back to Gravatar. |
| `PUT` | `/api/accounts/me/qrz-credentials` | Store QRZ credentials. Write-only; returns HTTP 503 when no `KEK` is configured. |
| `DELETE` | `/api/accounts/me/qrz-credentials` | Clear stored QRZ credentials. |
| `POST` | `/api/accounts/me/email-change` | Request a change of identifying email. Sends a link to the new address. |

## Discovery and favorites

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/api/discovery` | None | Active-now sessions and the filterable upcoming list. Listed nets only. Rate-limited. Both collections are bounded, and a cut one is **stated** in `applied.truncated` (below) — there is no cursor and no `?limit=`. |
| `GET` | `/api/favorites` | Session | One page of the account's favorited nets, newest-favorited first. Paged (`?limit=`, `?cursor=` — see [Pagination](#pagination)) and strict. Rate-limited per account. |
| `GET` | `/api/favorites/membership?ids=` | Session | Which of the named nets the account has favorited: `?ids=<uuid>,<uuid>,…` answers `{ "favorited": [<uuid>, …] }`, the subset of `ids` the account holds, in one request regardless of how many favorites the account has. At most `MAX_PAGE_LIMIT` (200) ids per request — the same bound as `?limit=`. Over the cap is **`400 /errors/validation`, refused and never truncated**: answering the first 200 and dropping the rest would report "not favorited" for every id past the cap, which is the same silently-changed answer strictness exists to prevent — a caller with more nets on screen sends them in batches. A malformed id, an unknown query key and a missing `ids` key are likewise `400 /errors/validation`; no session is `401`. Strict: a dropped or misspelt `ids` key would otherwise answer "none favorited" for every net with a `200`, and that is the one wrong answer the star would believe. Shares `GET /api/favorites`' per-account rate limit. This is what the star on a public net page and the discovery list ask, replacing a walk of every favorites page. Membership stays here, on an authenticated read, and never on the public net or discovery bodies: public reads are account-less. |
| `PUT` | `/api/favorites/{netDefinitionId}` | Session | Favorite a net. Idempotent. |
| `DELETE` | `/api/favorites/{netDefinitionId}` | Session | Unfavorite a net. Idempotent. |

`GET /api/discovery` answers with `{ activeNow, upcoming, applied }`. `applied` states what the
server actually did with the query, keyed by **query-parameter** name (`q` and `type`, not `name` and
`netType`):

```json
{ "applied": { "q": "sunday", "band": "20m", "sort": "time" } }
```

A dimension that was not filtered is **absent**, not `null` — so the key set is the answer. Read it
as a *positive* statement of what the server applied, and nothing further. Absence covers a key you
sent blank exactly as it covers one you misspelled: `?band=` applied no band filter and does not
appear, and `?bnad=20m` produces the same `applied` object as sending no `bnad` at all. The
difference between what you sent and what comes back is therefore **not** a rejection list, and a
client that needs to tell a typo from a parameter it never sent has to answer that from its own list
of parameter names rather than from this object. Values are post-trim and post-validation. `sort` is
always present, so a fall-back to the default ordering can never be silent.

The filters and the sort in `applied` describe `upcoming` only; `activeNow` takes no query and is
never filtered. One further key describes **either** collection:

```json
{ "applied": { "sort": "time", "truncated": ["activeNow", "upcoming"] } }
```

`truncated` names the collections the server **cut**, by their wire names and in envelope order
(`activeNow` before `upcoming`). Each collection has a fixed ceiling on the server, and a response
whose raw match count exceeds a ceiling serves at most the ceiling's worth of rows for that collection
and names it here. The key is **omitted when nothing was cut** — the same omit-when-absent rule as
every other key on this object — so a missing `truncated` is the statement that both collections are
complete, and a collection matching *exactly* its ceiling is complete and is not named. There is no
cursor and no `?limit=` on this read, and the two ceilings are not the same kind of thing.

Narrowing the filters *usually* reaches past `upcoming`'s ceiling, but not always, and the exception is
structural rather than rare: the filters select **definitions** while the ceiling counts
**occurrences**, so a single net with more scheduled occurrences than the ceiling cannot be narrowed
past it by any query — nor can a row the server matched but could not serve. `activeNow` takes no query
at all and is not reachable past its ceiling by any means. Treat "narrow and retry" as the usual
remedy, never as a guarantee.

The two ceilings also differ in intent. `activeNow`'s is a **backstop**, set high enough that reaching
it means something is wrong on the instance rather than that a busy evening happened. `upcoming`'s is
an ordinary result cap that a broad query reaches routinely, and is the one a future cursor would
replace.

**A cut collection can come back short of its ceiling — even empty — while `truncated` names it.**
The server decides the flag on the raw rows the query matched and only then drops a row it cannot
serve (an upcoming definition whose connection rows are missing; a live session whose snapshot or log
this version cannot read). Deciding it after the drop would let one such row inside a full window read
as the end of the list, silently hiding every row behind it — the same price the paged reads document
under [Pagination](#pagination). Treat `truncated` as the only signal that more matched; never infer
completeness from a count.

## Net definitions

Mutations require ownership of the definition.

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/net-definitions` | Create a definition, born with its ways in: the body carries the scalar fields plus a `connections` list in the same per-connection shape `PUT …/{id}/connections` accepts. A body with no connections is refused with `400 /errors/net-definition-invalid`, and so is one carrying a retired flat key (`plannedFrequency`/`band`/`mode`, …) at the top level, with or without a list — the facts belong to a connection. Requires consent and a reserved callsign. Subject to `MAX_NETS_PER_USER`. |
| `GET` | `/api/net-definitions` | One page of the active definitions the account owns, newest-created first. Paged (`?limit=`, `?cursor=` — see [Pagination](#pagination)) and strict. |
| `GET` | `/api/net-definitions/{id}` | Fetch one definition. |
| `PUT` | `/api/net-definitions/{id}` | Update its scalar fields (title, description, geography, category, type, duration, visibility). Increments `definitionVersion`. Carries no connection fact; the list is replaced through `PUT …/{id}/connections`, and a body that names one here — `connections`, or a retired flat key such as `band` or `plannedFrequency` — is refused with `400 /errors/net-definition-invalid` rather than silently ignored. |
| `DELETE` | `/api/net-definitions/{id}` | Delete it. Never orphans past sessions. |
| `GET` | `/api/net-definitions/by-token/{token}` | Fetch a net by its link token. Every net has one, whatever its visibility; a Listed net's discovery title links here. |
| `POST` | `/api/net-definitions/{id}/owners` | Add a co-owner. Subject to `MAX_OWNERS_PER_NET`. |
| `DELETE` | `/api/net-definitions/{id}/owners/{accountId}` | Remove a co-owner. |
| `GET` | `/api/net-definitions/{id}/schedule` | Fetch the stored schedule rule. `204` when the net has no schedule. |
| `PUT` | `/api/net-definitions/{id}/schedule` | Set a one-off or recurring schedule. |
| `DELETE` | `/api/net-definitions/{id}/schedule` | Clear the schedule. |
| `GET` | `/api/net-definitions/{id}/occurrences` | Upcoming occurrences generated from the schedule. |
| `PUT` | `/api/net-definitions/{id}/connections` | Replace the ordered list of ways the net can be reached (HF, repeater, EchoLink, AllStar, DMR, D-Star, YSF, URF, or `other`). Requires `expectedDefinitionVersion`; a stale value is refused with `409 /errors/stale-version`, scoped to the connection list alone. Increments `definitionVersion`. The connection at position `0` is the one an ADIF export describes a QSO with. At most 32 connections. The write speaks the read's vocabulary: frequencies and offsets are sent as exact Hz integers under the same keys they are served on (`plannedFrequencyHz`, `repeaterOffsetHz`), so a `connections` array taken from `GET` and sent back unmodified is accepted. That `GET` is the definition's own, or the `definition.connections` nested in a session body — never a session body's top-level `connections`. That array is byte-for-byte the same shape (same ids, positions and keys) but states a different fact: its `plannedFrequencyHz` is the frequency the net was actually on (see the note under Sessions), and sending it here is not refused — it returns `200`, bumps `definitionVersion`, and silently rewrites the definition's planned frequencies to wherever the net last QSY'd. The retired decimal-MHz string spellings (`plannedFrequency`, `repeaterOffset`) are refused with the entry's index, never silently dropped. `position` may be present and, when it is, must equal the entry's place in the array. An `id` the definition does not already hold is treated as a new connection, not an error; naming one `id` twice is a `400`. A `400` about one entry carries that entry's zero-based array index as the `connectionIndex` extension member, so an editor can put the message against the row that caused it; a refusal about the set as a whole (empty, too many, a duplicate `id`) omits the member. |
| `GET` | `/api/net-definitions/{id}/delivery-config` | Current email, generic-webhook and Discord targets. |
| `PUT` | `/api/net-definitions/{id}/delivery-config` | Set targets (a replace: an omitted key clears that target). Both `webhookUrl` and `discordWebhookUrl` are validated against the egress rules, with no exemption for any host. See [webhook payload](webhook-payload.md) for what a generic endpoint receives, and [Discord message](discord-message.md) for what a Discord channel receives. |
| `DELETE` | `/api/net-definitions/{id}/delivery-config` | Clear targets, disabling delivery. |

## Sessions

| Method | Path | Capability | Description |
|--------|------|-----------|-------------|
| `POST` | `/api/net-sessions` | Owner | Start a session. Takes `definitionId` and nothing else — a session no longer carries an operating frequency of its own, because an internet-only net has none to give. Snapshots the definition, connection list included, **by value**: a later edit to the net's connections does not move a running session. |
| `GET` | `/api/net-sessions/{id}` | Session | Folded session summary, including `viewerRole`. |
| `GET` | `/api/net-sessions/{id}/events?since={seq}` | Session | Events with `seq` greater than `since`, in order. The resume path. |
| `POST` | `/api/net-sessions/{id}/close` | Owner | Close the session. Triggers the summary and delivery. |
| `POST` | `/api/net-sessions/{id}/frequency` | NCS | Move ONE way in to a new frequency. Requires `connectionId` naming a connection **this session froze at start**, plus `operatingFrequency` as a decimal-MHz string. A `connectionId` the session does not carry is a `404 /errors/net-connection-not-found`; one that carries no frequency at all — an EchoLink or DMR way — is a `422 /errors/connection-has-no-frequency`. Returns the folded summary with that connection moved and no other. |
| `POST` | `/api/net-sessions/{id}/handoff` | NCS | Transfer net control to another qualified operator. |
| `POST` | `/api/net-sessions/{id}/claim-control` | Owner, Logger | Claim control of a stalled session. |
| `POST` | `/api/net-sessions/{id}/check-ins` | NCS, Logger, Relay, self | Add a check-in. Carries a client event id for optimistic reconciliation. |
| `PUT` | `/api/net-sessions/{id}/check-ins/{checkInId}` | NCS, Logger, own entry | Edit an entry. Version compare-and-swap; a stale write returns HTTP 409. |
| `DELETE` | `/api/net-sessions/{id}/check-ins/{checkInId}` | NCS | Remove an entry. |
| `POST` | `/api/net-sessions/{id}/check-ins/{checkInId}/lock` | NCS, Logger | Acquire the editing soft lock. ~15s sliding lease. |
| `DELETE` | `/api/net-sessions/{id}/check-ins/{checkInId}/lock` | Lock holder | Release the lock. |
| `POST` | `/api/net-sessions/{id}/check-ins/{checkInId}/moderate` | NCS | Remove or block the station behind an entry. |
| `POST` | `/api/net-sessions/{id}/worked-station` | NCS | Set the working station. At most one at a time. |
| `POST` | `/api/net-sessions/{id}/reorder` | NCS | Reorder the roster. |
| `POST` | `/api/net-sessions/{id}/roster-order-mode` | NCS | Set the standing roster ordering mode (`manual` or `worked-sink`). |
| `PUT` | `/api/net-sessions/{id}/net-note` | NCS, Logger | Set the net-level note. |
| `POST` | `/api/net-sessions/{id}/roles` | Owner, NCS | Grant a session-scoped role. |
| `GET` | `/api/net-sessions/{id}/roles` | Owner, NCS | List role grants. |
| `DELETE` | `/api/net-sessions/{id}/roles/{accountId}` | Owner, NCS | Revoke a grant. |
| `GET` | `/api/net-sessions/{id}/roster-memory` | NCS, Logger | Remembered stations for prefill on this net. |
| `GET` | `/api/net-sessions/{id}/check-in-autofill` | NCS, Logger | Best-effort callbook lookup. Never blocks a check-in. Rate-limited and cached. |
| `GET` | `/api/net-sessions/{id}/export` | NCS, Owner | Download the session as CSV or ADIF. Each station's way in, and the ADIF `FREQ`, describe the connection as it stood when that station checked in, not as the session ended. |

On every session body — the folded summary above, the redacted public view below, the WebSocket
snapshots that mirror them, and the close webhook — the top-level `connections[n].plannedFrequencyHz`
is the frequency in force (current while live, final at close), never the definition's planned
value. That value is not dropped: the same body's `definition.connections[n].plannedFrequencyHz`
(frozen at session start) still carries it.

## Public session reads

The read-only surface, available without an account. Snapshots and events are redacted to the
public view.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/net-sessions/{id}/live` | Redacted public session view. |
| `GET` | `/api/net-sessions/{id}/live/events?since={seq}` | Redacted resume path. |

## WebSocket

| Path | Description |
|------|-------------|
| `/api/net-sessions/{id}/ws` | Authenticated stream. Snapshot then deltas, plus ephemeral lock advisories. |
| `/api/net-sessions/{id}/live/ws` | Public read-only stream. Redacted snapshot and deltas; never emits lock frames. |

See the [WebSocket protocol](websocket-protocol.md) for the message shapes.

## Abuse and admin

Admin endpoints require an account whose email is listed in `ADMIN_ACCOUNT_EMAILS`. The set is
deliberately bounded — there is no unbounded superuser path — and every action is audit-logged.

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `POST` | `/api/abuse-reports` | None | File an abuse report. |
| `GET` | `/api/admin/abuse-reports` | Admin | List unresolved reports, oldest first. Paginated. |
| `POST` | `/api/admin/abuse-reports/{id}/resolve` | Admin | Resolve a report. |
| `GET` | `/api/admin/search?q=&type=` | Admin | Find accounts, nets, sessions, and reports by name or ID. |
| `POST` | `/api/admin/accounts/{id}/disable` | Admin | Disable an account. |
| `POST` | `/api/admin/accounts/{id}/reenable` | Admin | Re-enable an account. |
| `GET` | `/api/admin/audit-log` | Admin | Read the security audit log, newest first. Paginated. Contains no secrets or personal data. |

### Search

Search returns `{"items": [...], "truncatedTypes": [...]}`. Each item carries `objectType`
(`account`, `net-definition`, `net-session`, or `abuse-report`), `id`, `label`, `sublabel`,
`disabledAt`, and `inactiveAt`. A no-match is an empty `items` list, not a `404`.

Matching is deliberately asymmetric:

| Field | Match |
|-------|-------|
| Callsign, display name | Prefix |
| Net and session title | Substring |
| **Email address** | **Exact only** |
| **Any ID** | **Exact only** |

Callsigns, names, and net titles are public radio data already shown on public net pages. Email
stays exact so this cannot be used to enumerate addresses. Pass a bare ID to resolve it back to
whichever object it names — that is how an ID copied out of the audit log becomes readable.

`?type=` narrows to one object type — and is one of only two parameters this read accepts, so a
misspelling is a `400` rather than a silently wider search. Results are capped per type, so one noisy
type cannot crowd out the others — and `truncatedTypes` names every searched type for which more
matched than were served, so a cut result can be told from a complete one (a type matching exactly
the cap is complete). It is always present: an empty array means every searched type is complete,
and a type that was not searched never appears. Its values are `account`, `net-definition` and
`net-session`; an `abuse-report` is reached only by its id and is never cut. Narrow the term to
reach the rest; there is no cursor. Every search is audit-logged, recording only how many results were
shown — never the search term, which may itself be an email address.

No per-user secret is readable through any of this.

### Audit-log filters

`GET /api/admin/audit-log` accepts three filters, freely combined:

| Param | Answers |
|-------|---------|
| `?actor=<id>` | "What did this account do?" |
| `?object=<id>` | "What touched this object?" |
| `?action=<verb>` | One verb from the closed vocabulary. |

`object` matches an audit record three ways: as the thing acted on, as the session the action
happened in, or as the net that session belongs to. One ID therefore reaches everything that
touched an account, a report, a session, **or a net** — including role grants, whose target is the
grantee's account rather than the session, and every session under a net.

A malformed ID or an unknown action verb is a `400`. Filters are never silently ignored: doing so
would return the unfiltered log while the caller believes a restriction applied.

An **unrecognised parameter** is a `400` for the same reason. A typo'd `?actorr=<id>` would otherwise
be dropped and answer with the unfiltered log — the one wrong answer on this surface most likely to
be believed. An unknown *value* and an unknown *key* get the same refusal because they cause the
same harm.

### Unrecognised parameters

Seven reads are **strict**: they refuse a parameter they do not recognise with a `400`
`/errors/validation`, rather than ignoring it.

| Read | Accepts |
|------|---------|
| `GET /api/admin/audit-log` | `limit`, `cursor`, `actor`, `object`, `action` |
| `GET /api/admin/abuse-reports` | `limit`, `cursor` |
| `GET /api/admin/search` | `q`, `type` |
| `GET /api/accounts/me/check-ins` | `limit`, `cursor` |
| `GET /api/favorites` | `limit`, `cursor` |
| `GET /api/favorites/membership` | `ids` |
| `GET /api/net-definitions` | `limit`, `cursor` |

A read is strict because its handler takes an `AppStrictQuery<…>` argument (`http/mod.rs`), so the
table above is the list of those handlers and this is how to check it is still complete — if the
number disagrees with the row count, the table is stale, not the grep:

```
grep -rn ": AppStrictQuery<" backend/crates/netroll-app/src/ | wc -l
```

Dated snapshot, 2026-09-06: **6**, taken *before* the membership read's handler landed; the
seventh row is that handler.

These are the reads where dropping a key changes what the answer *means*: it becomes the unfiltered,
restarted or — for `membership` — empty answer while the caller believes a restriction applied.
**Do not send a cache-buster, a tracking parameter, or any extra key to them** — it is a `400`.

On those seven reads the same `400` covers three causes: a parameter the read does not recognise, a
repeated single-valued parameter, and a wrong-typed value (`?limit=abc`). The `detail` string is one
cause-neutral sentence rather than a per-cause diagnosis, because it is rendered verbatim to the
person on the check-in history surface — this table is where the precise contract lives.

Every other read is **lenient** and ignores what it does not recognise, including `GET /api/discovery`
and the WebSocket resume cursors. There a dropped filter widens a list to that endpoint's own
no-filter answer.

That widening *is* a misrepresentation if nothing says it happened: `?bnad=20m` returns every net
with a `200`, and a client that shows "filtered by band" beside it is stating something untrue. The
`applied` object — documented above, under "Discovery and favorites" — is what stops that, by
stating the filters and the sort the server actually used. **It is not, on its own, enough for the
`?bnad=20m` case**, because it is a positive statement: the response to `?bnad=20m` is byte-identical
to the response to a request carrying no `bnad` at all, so nothing in it names the dropped key. A
client answers that half from its own list of parameter names, by naming the keys in the URL that it
did not send — which is what the discovery page does. Leniency is paid for by the two together.
The forward rule the exception follows from: **a filtered read whose URL is publicly
shareable cannot be strict**, because third parties append their own keys to a shared link and
correctness would then depend on channels outside this project's control. Discovery is the only
such read; every strict read sits behind a session.

### Pagination

The paged reads — the two admin list reads, the account check-in history, the account's favorites
(`GET /api/favorites`) and its owned definitions (`GET /api/net-definitions`) — accept `?limit=`
(default 50, maximum 200) and `?cursor=`, and return:

```json
{ "items": [], "nextCursor": "1754000000123:0192f4a1-..." }
```

`nextCursor` is `null` on the last page. Treat the cursor as opaque: pass back exactly what the
previous page returned. A cursor the server did not issue is a `400`, not a silent restart.

**A page can run short of `limit` — down to zero items — while `nextCursor` is still a string.**
The favorites and owned-definitions reads split the page on the raw rows and only *then* drop a row
whose connection set is missing (split first so the cursor always advances past a damaged row and
a walk stays total, at the price of a page that can come back short; split
*after* the skip and a dropped row on the page edge reads as the end of the list, silently losing
every row behind it). The three older paged reads do not do this today, but the contract is the
envelope's, not any one endpoint's: **the only end-of-list signal is `nextCursor === null`.** A
client that stops on `items.length < limit`, or on an empty `items`, truncates the list without
any error to tell it so.

An audit-log cursor is bound to the filters that produced it — **send the same filters alongside
it**. Replaying a cursor under different filters is a `400` rather than a silent re-anchor, which
would hand back a partial slice that looks like a complete one.

## Health

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/healthz` | None | Liveness probe. `200` with `{"status":"ok"}` when the database answers, `503` with `{"status":"unavailable"}` when it doesn't answer within two seconds. Never cached and never rate-limited. The container health check and the deploy script both poll it. |

## Error format

Errors are RFC 9457 problem+json. Both the `type` slug and the `detail` string are written to be
shown to a person, and the ordering is **`detail` first, slug map second**: `detail` is the only
part of the response that can name *which* field failed and *what* would make it acceptable, and a
slug that covers two dozen causes cannot. That is why the `detail` strings are written as
user-facing copy rather than as log lines. Unmapped types with no `detail` degrade to a generic
message rather than showing internals.

**The SPA follows that ordering at every surface.** Dated snapshot, 2026-08-30:
**44 call sites across 26 files** resolve through the single `messageForProblem(problem)` in
`frontend/src/errors/problemMessages.ts`, and none hand-rolls the ordering (`AdminPage`'s one
`adminMessage` helper fronts seven of that page's alerts). Re-measure rather than trust the
figures — the previous two versions of this paragraph were both asserted rather than counted, and
both were wrong:

```
# 44 call sites
grep -rn "messageForProblem(" frontend/src --include="*.ts" --include="*.tsx" \
  | grep -v "\.test\." | grep -v "problemMessages.ts" | wc -l
# 26 files
grep -rl "messageForProblem(" frontend/src --include="*.ts" --include="*.tsx" \
  | grep -v "\.test\." | grep -v "problemMessages.ts" | wc -l
```

**Three slugs are excluded from the preference**, because their `detail` is a bare token rather
than a sentence and preferring it makes the message worse: `/errors/lock-held` (a bare callsign,
which the resolver composes into a sentence instead of showing raw), `/errors/staying-invalid` and
`/errors/precedence-invalid` (both `"unrecognized … token: <echo>"`, which name a type — they are
wrong at source and the exclusion is a render-side stopgap until they are rewritten). The set and
its measurement live in one place, beside the slug map. An empty-string `detail` counts as absent:
showing it would leave a `role="alert"` announcing nothing.

| Status | Meaning in NetRoll |
|--------|--------------------|
| `400` | Field-level validation failure at the adapter boundary. No row is written. |
| `401` | No session, or the session was revoked. |
| `403` | The acting role lacks the capability for this object. |
| `404` | Not found, including an unlisted net reached without a valid link token. |
| `409` | Version conflict on a concurrent edit. Re-read and reconcile. |
| `410` | The record existed and is permanently unreadable — `/errors/unreplayable-log`, a session logged before a net could list more than one way to reach it. Not retryable, and not a 404: the caller is remembering it correctly. |
| `422` | The request is well formed and the objects exist, but the operation does not apply to them — retuning a way in that has no frequency. |
| `429` | Rate limit exceeded. Retry information is included. |
| `503` | A feature whose optional configuration is absent — QRZ credential endpoints without a `KEK`. From `/healthz`, the database isn't answering. |

## See also

- [WebSocket protocol](websocket-protocol.md)
- [Roles and permissions](roles-and-permissions.md)
- [ARCHITECTURE.md](https://github.com/nkbooth/netroll/blob/main/ARCHITECTURE.md) on GitHub
- [Configuration](configuration.md)
