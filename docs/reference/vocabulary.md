---
title: "Vocabulary"
description: "The one word NetRoll uses for each concept, in code and in prose, with the synonyms the code and the documentation are held not to use."
docType: "reference"
audience: ["contributor"]
lastVerified: "2026-09-14"
---

# Vocabulary

This is the list the code and the documentation are held to. The [glossary](glossary.md)
defines terms for readers; this page fixes which word is used when there is more than one
candidate. One row per concept: the name the code uses, the name prose and the UI use where
it differs, and the synonyms that are rejected. Keep every rejected word backticked and
comma-separated: the column is written to be greppable, and a parenthesised note after a term
scopes it rather than adding another rejected word.

| Concept | Code term | Prose / UI term | Rejected synonyms |
|---|---|---|---|
| Net | `NetDefinition` | "net" for the thing people join; "net definition" when the stored record is meant | `net template`, `listing` (for the record), `event` |
| Session | `SessionState`, `SessionLifecycle`; transitions in `session_sm.rs` | "session" | `net instance`, `run`, `meeting`, "net" when one live run is meant |
| Occurrence | `NetOccurrence` | "occurrence"; "upcoming net" on the directory | `slot`, `instance` |
| Check-in | `RosterEntry` (the record), `CheckInSource`, the `check_in` module | "check-in" as a noun, hyphenated; "check in" only as a verb | `checkin` (prose only; identifiers are exempt), `entry` when the check-in is meant, `attendee` |
| Roster | `RosterEntry`, `RosterOrderMode` | "roster" | `check-in list`, `attendance`, `log` (for the roster) |
| Connection | `NetConnection`, `NetConnectionKind`, `NetConnectionSet` | "way in" | `channel`, `frequency` when the connection is meant, `link`, `path` |
| Via | `Via` (`Connection`, `Unlisted`), `ViaWire`, `ViaDisplay` | "via": which way in a check-in arrived | `through`, `on`, `over` as the field's name in prose, `route` |
| Staying status | `StayingStatus` (`StayingForComments`, `InAndOut`) | "staying for comments", "in and out" | `short time`, `quick check-in`, `leaving` |
| Net control | `Role::NetControl`, `ControlState` | "net control" for the person and the role; "NCS" as the abbreviation | `controller`, `host`, `moderator`, `admin`, `operator` when the role is meant |
| Roles | `Role` (`Owner`, `NetControl`, `Logger`, `Relay`, `Participant`) | the five words as spelled | `co-owner`, `staff` as a role name (it is a tier), `helper`, `scribe` |
| Relayed by | `RosterEntry::relayed_by`, `relayedBy` on the wire | "relayed by": the station that passed the traffic (the glossary separates the act, the role and the field) | `relayed via` (via is the way in) |
| Working station | `Capability::SetWorkedStation`, `RosterEntry::worked` | "working station"; "worked" once done | `current speaker`, `active station`, `cursor` in prose |
| Soft lock | `SessionLocks`, `LOCK_TTL_MILLIS`, `LeaseView` | "soft lock": the sliding lease that makes an entry read-only for other operators while one edits it | `hard lock`, `edit lock`, `advisory lock`, `lease` when the operator-facing lock is meant |
| Visibility | `Visibility` (`Listed`, `Unlisted`) | "listed", "unlisted" | `public`, `private` for a net (the code's `public_*` route helpers are identifiers, exempt) |
| Discovery | `DiscoveryQuery`, `DiscoveryFilters` | "the directory" in the README and UI prose; "discovery" in contributor prose | `search page`, `landing`, `hero page` |
| Link token | `NetDefinition::link_token`, `linkToken` on the wire | "link token", "permalink" | `share code`, `secret URL`, `invite` |

The word `register` is deliberately absent from this table: it is reserved for the test
coverage registers in the backend suites and is not a word for the roster, so it is neither
canonical nor rejected.

Identifiers are never renamed for this list. It governs comments, doc comments, test names,
fixture strings and prose. A rejected word inside a type name, a wire field or a route helper
is exempt, which is why the code column and the rejected column can both be true at once.

## See also

- [Glossary](glossary.md): what each term means, for every reader.
- [Roles and permissions](roles-and-permissions.md): what each role may do.
