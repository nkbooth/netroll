// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Pure CSV and ADIF export generation for a closed net session.
//!
//! Total, deterministic, no-I/O functions over the folded [`SessionState`]. It
//! lives in the domain, not the adapters, because it is a pure string build over
//! at most fifty rows; the handler does the I/O wiring.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::check_in::CheckInSource;
use crate::fold::{RosterEntry, SessionState};
use crate::net::wire::{NetConnectionWire, adif_fields_for_via_wire, resolve_via, via_label};

/// The LOCKED CSV contract. This array IS the column list, and its own length is
/// the only statement of how many columns there are — never write the count in
/// prose, because prose goes stale and the array does not.
///
/// Column ORDER is a wire contract downstream tools parse against, so it never
/// changes silently: a new column has a fixed, named position, never appended
/// blind or reordered.
const CSV_COLUMNS: [&str; 16] = [
    "callsign",
    "name",
    "location",
    "grid",
    "source",
    "entering_operator",
    "signal_report",
    "staying",
    "precedence",
    "traffic",
    "worked",
    // `notes` is the operator's private staff note; `public_note` is the prose
    // written for observers. The operator's own log holds BOTH, because this is
    // their record and not a public surface.
    "notes",
    "public_note",
    // The way in as its LABEL, never its connection id: a UUID is not a fact a
    // spreadsheet reader can act on. An empty cell means "nobody recorded it",
    // a different fact from the ADIF-export connection, never filled from it.
    "via",
    // WHICH STATION passed the traffic, as a callsign. Different from `via`
    // beside it (how the traffic travelled) and from `entering_operator` (which
    // ACCOUNT typed the row). An empty cell means "not relayed" and is never
    // filled in from either neighbour.
    "relayed_by",
    "checked_in_at",
];

/// Neutralizes a single CSV field against BOTH spreadsheet formula injection
/// and RFC-4180 structural ambiguity, in that order.
///
/// 1. **Formula-injection neutralization first**: a value whose first
///    character is `=`, `+`, `-`, `@`, a TAB, or a CR is prefixed with a single
///    apostrophe `'` — the OWASP-recommended, widely-recognized neutralizer —
///    so a spreadsheet treats it as literal text rather than evaluating it.
/// 2. **RFC-4180 quoting second:** if the (possibly-prefixed) value then
///    contains a comma, double-quote, CR, or LF, it is wrapped in
///    double-quotes with every internal `"` doubled.
///
/// The order matters: prefixing first means an injected value that ALSO contains
/// a comma still gets both the apostrophe and the quotes (e.g. `=A,B` →
/// `"'=A,B"`).
pub fn escape_csv_field(value: &str) -> String {
    let neutralized = match value.chars().next() {
        Some('=') | Some('+') | Some('-') | Some('@') | Some('\t') | Some('\r') => {
            let mut s = String::with_capacity(value.len() + 1);
            s.push('\'');
            s.push_str(value);
            s
        }
        _ => value.to_owned(),
    };
    let needs_quoting = neutralized.contains([',', '"', '\n', '\r']);
    if needs_quoting {
        format!("\"{}\"", neutralized.replace('"', "\"\""))
    } else {
        neutralized
    }
}

/// Joins one CSV record from its already-ordered cells, running EVERY cell
/// through [`escape_csv_field`] (the header tokens are static/safe, but escaping
/// them too keeps a single code path). RFC-4180's canonical `\r\n` terminator is
/// added by the caller.
fn csv_record<S: AsRef<str>>(cells: &[S]) -> String {
    cells
        .iter()
        .map(|c| escape_csv_field(c.as_ref()))
        .collect::<Vec<_>>()
        .join(",")
}

/// Renders an epoch-millis instant as an RFC 3339 UTC string, matching the HTTP
/// layer's `rfc3339` helper so a CSV `checked_in_at` cell reads identically to
/// the summary's `addedAt`.
fn rfc3339_utc(epoch_millis: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(epoch_millis as i64)
        .expect("stored timestamps are in chrono range")
        .to_rfc3339()
}

/// Serializes the folded roster to a CSV document: a fixed header row,
/// then one row per `state.roster` entry in fold order, terminated by `\r\n`
/// (RFC-4180). `entering_ops` maps a staff entry's `added_by` account id to that
/// operator's callsign (resolved by the handler); a `self`-sourced entry — whose
/// operator IS the station itself — always exports a blank entering operator.
/// Every cell passes through [`escape_csv_field`].
///
/// `snapshot` is the session's FROZEN connection set, with NO move overlaid.
/// Each row resolves its `via` LABEL against that snapshot as it stood at the
/// row's own `added_seq` ([`SessionState::connections_at`]), so a station worked
/// before a mid-session QSY exports the frequency it was worked on and not the
/// one the net moved to. It is a parameter rather than a `SessionState` field
/// because the fold has never seen a connection set: the snapshot is a column on
/// the session row, not an event.
///
/// **Named `snapshot`, not `connections`, on purpose.** The type cannot tell a
/// frozen set from a live one, so the name is what stops a caller passing
/// `live_connections(..)` here by habit and silently reintroducing the defect.
pub fn to_csv(
    state: &SessionState,
    entering_ops: &BTreeMap<Uuid, String>,
    snapshot: &[NetConnectionWire],
) -> String {
    let mut out = String::new();
    out.push_str(&csv_record(&CSV_COLUMNS));
    out.push_str("\r\n");
    for entry in &state.roster {
        // EXHAUSTIVE destructure, no `..` rest pattern: a new `RosterEntry`
        // field must fail to compile HERE until a human decides whether this
        // machine-parsed projection carries it.
        let RosterEntry {
            callsign,
            name,
            location,
            grid,
            source,
            added_by,
            signal_report,
            staying,
            precedence,
            traffic,
            worked,
            notes,
            public_note,
            via,
            relayed_by,
            added_at,
            // READ, not emitted: it picks the connection set the `via` cell
            // below resolves against. A log cursor is not a cell.
            added_seq,
            // Dropped: fold-internal, projected by NO export surface — the roster
            // row's own id, its optimistic-concurrency version, and the derived
            // per-field correction history.
            check_in_id: _,
            version: _,
            corrections: _,
        } = entry;
        // The set as it stood WHEN this station checked in.
        let connections = state.connections_at(*added_seq, snapshot);
        // The entering operator is the logging operator's callsign, but ONLY for
        // a staff entry: a self entry entered itself, so there is no distinct
        // entering operator and the cell is blank.
        let entering_operator = if *source == CheckInSource::Staff {
            added_by
                .and_then(|id| entering_ops.get(&id))
                .map(String::as_str)
                .unwrap_or("")
        } else {
            ""
        };
        let cells: [String; 16] = [
            callsign.as_str().to_owned(),
            name.as_ref().map(|n| n.as_str()).unwrap_or("").to_owned(),
            location
                .as_ref()
                .map(|l| l.as_str())
                .unwrap_or("")
                .to_owned(),
            // An absent grid is an EMPTY cell, never the string "None".
            grid.as_ref().map(|g| g.as_str()).unwrap_or("").to_owned(),
            source.as_str().to_owned(),
            entering_operator.to_owned(),
            signal_report
                .as_ref()
                .map(|r| r.as_str())
                .unwrap_or("")
                .to_owned(),
            staying.as_str().to_owned(),
            precedence.as_str().to_owned(),
            traffic.map(|t| t.get().to_string()).unwrap_or_default(),
            if *worked { "true" } else { "false" }.to_owned(),
            notes.as_ref().map(|n| n.as_str()).unwrap_or("").to_owned(),
            public_note
                .as_ref()
                .map(|n| n.as_str())
                .unwrap_or("")
                .to_owned(),
            // All four arms go through the one resolver rather than an
            // `unwrap_or`, so an EMPTY cell means `NotRecorded` and nothing
            // else: an unresolvable `via` says so in words and a free-text one
            // prints the operator's own, and neither collapses into "nobody
            // recorded it" or borrows the ADIF-export connection's label.
            // Resolved as at this row's own seq — borrowing the wrong MOMENT is
            // the same lie on the time axis.
            via_label(&resolve_via(via.as_ref(), &connections)).unwrap_or_default(),
            // No resolver and no fallback: a callsign is already the text a
            // spreadsheet reader acts on. An empty cell means NOT RELAYED, and
            // is never filled in from `entering_operator` above — that column
            // answers which ACCOUNT typed the row, and borrowing it would claim
            // every staff-logged station was relayed by whoever logged it.
            relayed_by
                .as_ref()
                .map(|c| c.as_str())
                .unwrap_or("")
                .to_owned(),
            rfc3339_utc(*added_at),
        ];
        out.push_str(&csv_record(&cells));
        out.push_str("\r\n");
    }
    out
}

/// Frames one ADIF `<TAG:byte_length>value` field. `byte_length` is the UTF-8
/// BYTE length, NOT the character count: an ADIF parser reads exactly that many
/// bytes, so a `.chars().count()` here silently corrupts every record carrying a
/// non-ASCII name or location (`José Ñ` is 8 bytes and 6 chars). The
/// length-prefixed framing is also why ADIF needs no delimiter escaping.
pub fn adif_field(tag: &str, value: &str) -> String {
    format!("<{}:{}>{}", tag, value.len(), value)
}

/// Maps a NetRoll band token to its ADIF band-enumeration value, or
/// `None` when there is no valid ADIF band (the record then relies on `<FREQ>`).
///
/// Every NetRoll band token is already a valid ADIF band enum EXCEPT:
/// - `2200m` → **`2190m`** — ADIF names the 136 kHz band `2190m`, not `2200m`;
///   a raw `2200m` would fail ADIF validation. This is a real, well-known ADIF
///   spec quirk, not a typo — do NOT "correct" it back to `2200m`.
/// - `other` → `None` — not an ADIF band.
///
/// An unrecognized token also yields `None` (defensive).
pub fn band_to_adif(token: &str) -> Option<&'static str> {
    match token {
        "2200m" => Some("2190m"),
        "630m" => Some("630m"),
        "160m" => Some("160m"),
        "80m" => Some("80m"),
        "60m" => Some("60m"),
        "40m" => Some("40m"),
        "30m" => Some("30m"),
        "20m" => Some("20m"),
        "17m" => Some("17m"),
        "15m" => Some("15m"),
        "12m" => Some("12m"),
        "10m" => Some("10m"),
        "6m" => Some("6m"),
        "4m" => Some("4m"),
        "2m" => Some("2m"),
        "1.25m" => Some("1.25m"),
        "70cm" => Some("70cm"),
        "33cm" => Some("33cm"),
        "23cm" => Some("23cm"),
        // `other` and any unknown token have no ADIF band equivalent.
        _ => None,
    }
}

/// Maps a NetRoll mode token to its ADIF MODE-enumeration value, or `None`
/// when there is no valid ADIF mode.
///
/// `digital` and `mixed` map to `None`: ADIF has no generic "digital" or "mixed"
/// MODE value (it requires a specific mode such as FT8/PSK31), so forcing one
/// would emit an invalid enum. Omitting `<MODE>` — the record still anchors on
/// `<FREQ>`/`<BAND>` — is the correct, spec-valid choice (do NOT guess a
/// substitute). An unrecognized token also yields `None`.
pub fn mode_to_adif(token: &str) -> Option<&'static str> {
    match token {
        "ssb" => Some("SSB"),
        "cw" => Some("CW"),
        "am" => Some("AM"),
        "fm" => Some("FM"),
        // `digital`/`mixed` (and any unknown token) have no single ADIF MODE value.
        _ => None,
    }
}

/// Formats an exact-Hz frequency as an ADIF `<FREQ>` MHz decimal string, trimming
/// trailing fractional zeros (`14_250_000` → `14.25`, `14_000_000` → `14`).
fn freq_mhz(hz: i64) -> String {
    let whole = hz / 1_000_000;
    let frac = (hz % 1_000_000).abs();
    if frac == 0 {
        whole.to_string()
    } else {
        let s = format!("{whole}.{frac:06}");
        s.trim_end_matches('0').to_owned()
    }
}

/// Serializes the folded session to a valid ADIF `.adi` document from the
/// net-control perspective: a header block terminated by `<EOH>`, then one
/// QSO record per roster station terminated by `<EOR>`. Each record carries
/// `<CALL>` and `<QSO_DATE>`/`<TIME_ON>` (UTC of the check-in), and — when this
/// record's own way in supplies them — `<FREQ>`, `<BAND>`, `<MODE>`,
/// `<PROP_MODE>`, plus `<RST_SENT>`, `<NAME>`, `<QTH>`, `<NOTES>` from the
/// entry itself.
///
/// **`<FREQ>` is not emitted on every record**: a station that came in over
/// EchoLink has no frequency to state, and stating one anyway is a fabrication.
/// It IS still the unmappable-band anchor wherever a frequency exists, including
/// for an RF way in whose band token this build cannot parse.
///
/// **Band, mode and frequency are DERIVED PER QSO**, from the connection each
/// station actually checked in on — never a scalar triple describing the whole
/// session, which is what stamps `HF, 20m` onto four hundred EchoLink contacts
/// in a file that reaches LoTW and QRZ and cannot be recalled. They come from
/// the frozen `snapshot` and each record's own `via`, resolved as at that
/// record's own `added_seq`, so a station worked on 14.230 before the net moved
/// to 14.250 exports `<FREQ>14.23`. `<BAND>` stays the owner's token and is
/// deliberately not re-derived.
///
/// **Named `snapshot`, not `connections`, on purpose** — see [`to_csv`].
///
/// `created_at_millis` is the export instant the handler supplies for
/// `<CREATED_TIMESTAMP>`, a parameter so this stays pure and deterministic.
/// `<OPERATOR>` and `<STATION_CALLSIGN>` are omitted: a net has no single stable
/// control callsign across handoffs.
pub fn to_adif(
    state: &SessionState,
    snapshot: &[NetConnectionWire],
    created_at_millis: u64,
) -> String {
    let created = DateTime::<Utc>::from_timestamp_millis(created_at_millis as i64)
        .expect("stored timestamps are in chrono range")
        .format("%Y%m%d %H%M%S")
        .to_string();

    let mut out = String::new();
    // A free-text preamble before the first `<...>` tag is spec-legal and ignored
    // by importers.
    out.push_str("NetRoll ADIF export\n");
    out.push_str(&adif_field("ADIF_VER", "3.1.4"));
    out.push('\n');
    out.push_str(&adif_field("PROGRAMID", "NetRoll"));
    out.push('\n');
    out.push_str(&adif_field("CREATED_TIMESTAMP", &created));
    out.push('\n');
    out.push_str("<EOH>\n");

    for entry in &state.roster {
        // EXHAUSTIVE destructure, no `..` rest pattern: a new `RosterEntry`
        // field must fail to compile HERE until a human decides whether this
        // projection carries it.
        //
        // ADIF is a SPEC-BOUNDED tag set, so a field lands here only when the
        // spec has a tag for it. Dropped: `source`/`added_by` are provenance
        // with no ADIF equivalent; `staying`/`precedence`/`traffic`/`worked` are
        // net-control run-state, not QSO data; `check_in_id`/`version`/
        // `corrections` are fold-internal. `roster_projection_sites.rs` reds
        // when the dropped set moves.
        //
        // `via` DOES land here — not as an identifier, which ADIF has no tag
        // for, but as the `BAND`/`MODE`/`FREQ`/`PROP_MODE` derived from it.
        //
        // `public_note` does NOT. ADIF has exactly one note tag, `<NOTES>`, and
        // the staff note occupies it; emitting the public note there would
        // duplicate or silently overwrite an operator's own commentary in their
        // own export. None of ADIF 3.x names a second note.
        //
        // `added_seq` is READ, not `_`-bound, and not emitted: it selects the
        // connection set the record's `via` resolves against, which is how
        // `<FREQ>` becomes the frequency this QSO was worked on.
        //
        // `relayed_by` does NOT join the record, with the spec read rather than
        // assumed. **ADIF 3.1.4, the version this file's `ADIF_VER` declares,
        // names no relay tag**: `relay` occurs once in the whole specification,
        // inside the award-sponsor name "American Radio Relay League". Every
        // callsign-bearing QSO field names a DIFFERENT party — `OPERATOR` and
        // `STATION_CALLSIGN` the logging station, `OWNER_CALLSIGN` its owner,
        // `CONTACTED_OP`/`EQ_CALL` the contacted station, `GUEST_OP`
        // import-only, and `QSL_VIA` the QSL-card routing manager, which is the
        // trap a reader grepping for `VIA` hits first. An `APP_NETROLL_*` or
        // `USERDEF` field is spec-legal and refused: it would be a tag NetRoll
        // writes and no importer reads, in a file reaching LoTW and QRZ where an
        // invented tag cannot be recalled.
        let RosterEntry {
            callsign,
            added_at,
            added_seq,
            signal_report,
            name,
            location,
            grid,
            notes,
            via,
            relayed_by: _,
            public_note: _,
            source: _,
            added_by: _,
            staying: _,
            precedence: _,
            traffic: _,
            worked: _,
            check_in_id: _,
            version: _,
            corrections: _,
        } = entry;
        let dt = DateTime::<Utc>::from_timestamp_millis(*added_at as i64)
            .expect("stored timestamps are in chrono range");
        // The set as it stood WHEN this station checked in. The
        // `NotRecorded` fallback below picks from this same set, so an
        // unrecorded-via QSO also gets the frequency in force at ITS seq.
        let connections = state.connections_at(*added_seq, snapshot);
        // THIS entry's own way in, not the session's. The four `via`
        // answers stay distinct inside the resolver — an unresolvable or
        // free-text `via` omits every tag rather than borrowing the export
        // connection's, and only an UNRECORDED one falls back. That fallback is
        // the ONE in the whole tree.
        let adif = adif_fields_for_via_wire(via.as_ref(), &connections);
        out.push_str(&adif_field("CALL", callsign.as_str()));
        out.push_str(&adif_field("QSO_DATE", &dt.format("%Y%m%d").to_string()));
        out.push_str(&adif_field("TIME_ON", &dt.format("%H%M%S").to_string()));
        // FREQ is emitted when this QSO's way in has one — it anchors a record
        // whose band has no ADIF equivalent (`other`) or whose mode is omitted
        // (`digital`/`mixed`). An internet-carried way in has none and the tag is
        // OMITTED, never fabricated.
        if let Some(hz) = adif.freq_hz {
            out.push_str(&adif_field("FREQ", &freq_mhz(hz)));
        }
        if let Some(b) = adif.band {
            out.push_str(&adif_field("BAND", b));
        }
        if let Some(m) = adif.mode {
            out.push_str(&adif_field("MODE", m));
        }
        // ADIF's own propagation-mode value for an internet-carried QSO: `ECH`
        // for EchoLink, which ADIF names specifically, and `INTERNET` for the
        // rest. (`INT` is NOT a member of the Propagation_Mode enumeration in
        // 3.1.4 — the version declared above — or in 3.1.7.)
        if let Some(prop) = adif.prop_mode {
            out.push_str(&adif_field("PROP_MODE", prop));
        }
        if let Some(report) = signal_report {
            out.push_str(&adif_field("RST_SENT", report.as_str()));
        }
        if let Some(name) = name {
            out.push_str(&adif_field("NAME", name.as_str()));
        }
        if let Some(location) = location {
            out.push_str(&adif_field("QTH", location.as_str()));
        }
        // `<GRIDSQUARE>` sits beside `<QTH>`: a place name and a locator are a
        // pair. The canonical `Grid` string is already valid ADIF at 2, 4, 6 or
        // 8 characters, so no transformation is needed.
        if let Some(grid) = grid {
            out.push_str(&adif_field("GRIDSQUARE", grid.as_str()));
        }
        if let Some(notes) = notes {
            out.push_str(&adif_field("NOTES", notes.as_str()));
        }
        out.push_str("<EOR>\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callsign::parse_callsign;
    use crate::check_in::{
        Precedence, StayingStatus, parse_location, parse_name, parse_note, parse_signal_report,
        parse_traffic_count,
    };
    use crate::fold::{FrequencyMove, RosterEntry};
    use crate::net::connection::{NetConnection, NetConnectionKind, NetConnectionSet, Via};
    use crate::net::enums::{Band, Mode};
    use crate::net::wire::{UNRESOLVABLE_VIA_LABEL, wire_connections};
    use crate::profile::parse_grid;

    fn base_entry(id: u128, call: &str, added_at: u64) -> RosterEntry {
        RosterEntry {
            check_in_id: Uuid::from_u128(id),
            callsign: parse_callsign(call).expect("valid callsign"),
            added_at,
            // A seq no fixture moves before, so every older test still reads
            // the snapshot's planned value.
            added_seq: 1,
            added_by: None,
            source: CheckInSource::Staff,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            name: None,
            location: None,
            grid: None,
            precedence: Precedence::Routine,
            traffic: None,
            notes: None,
            public_note: None,
            worked: false,
            version: 1,
            via: None,
            corrections: Vec::new(),
            relayed_by: None,
        }
    }

    /// The `20m`/`ssb`/14.230 MHz HF way in these tests used to pass as a scalar
    /// triple, now expressed as the connection set `to_adif`/`to_csv` resolve
    /// each entry's `via` against.
    fn hf_only() -> Vec<NetConnectionWire> {
        rf_only(14_230_000, Band::TwentyMeters, Mode::Ssb)
    }

    /// One HF way in with the given frequency, band and mode.
    fn rf_only(hz: i64, band: Band, mode: Mode) -> Vec<NetConnectionWire> {
        wire_connections(
            &NetConnectionSet::new(vec![NetConnection {
                id: Uuid::from_u128(0xF1),
                position: 0,
                kind: NetConnectionKind::Hf {
                    planned_frequency_hz: hz,
                    band,
                    mode,
                },
            }])
            .expect("one connection is a valid set"),
        )
    }

    /// An entry that was relayed by `call` — the ordinary on-air case, a station
    /// with no NetRoll account behind it.
    fn relayed_by(mut entry: RosterEntry, call: &str) -> RosterEntry {
        entry.relayed_by = Some(parse_callsign(call).expect("valid callsign"));
        entry
    }

    fn state_with(roster: Vec<RosterEntry>) -> SessionState {
        SessionState {
            roster,
            ..Default::default()
        }
    }

    // --- escape_csv_field ------------------------------------------

    #[test]
    fn a_plain_value_is_emitted_bare() {
        assert_eq!(escape_csv_field("W1AW"), "W1AW");
        assert_eq!(escape_csv_field("routine"), "routine");
    }

    #[test]
    fn a_leading_formula_character_is_neutralized_with_an_apostrophe() {
        // `=`, `+`, `-`, `@`, TAB, CR leading values are made inert so a
        // spreadsheet cannot evaluate them as formulas.
        assert_eq!(escape_csv_field("=1+2"), "'=1+2");
        assert_eq!(escape_csv_field("+1"), "'+1");
        assert_eq!(escape_csv_field("-1"), "'-1");
        assert_eq!(escape_csv_field("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(escape_csv_field("\tTAB"), "'\tTAB");
        // A leading CR is neutralized AND then RFC-4180 quoted, because the
        // prefixed value still contains a bare CR (a lone CR would otherwise
        // corrupt the record structure).
        assert_eq!(escape_csv_field("\rCR"), "\"'\rCR\"");
    }

    #[test]
    fn a_value_with_a_comma_or_quote_or_newline_is_rfc4180_quoted() {
        assert_eq!(escape_csv_field("Hartford, CT"), "\"Hartford, CT\"");
        // An internal double-quote is doubled and the whole field wrapped.
        assert_eq!(escape_csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_csv_field("line1\nline2"), "\"line1\nline2\"");
    }

    #[test]
    fn injection_neutralization_happens_before_rfc4180_quoting() {
        // A value that is BOTH a formula AND contains a comma gets the apostrophe
        // FIRST, then the whole thing is quoted; that ordering is the contract.
        assert_eq!(escape_csv_field("=A,B"), "\"'=A,B\"");
    }

    // --- to_csv ----------------------------------------------------

    #[test]
    fn to_csv_emits_the_locked_header_row_first() {
        let csv = to_csv(&state_with(vec![]), &BTreeMap::new(), &hf_only());
        let first_line = csv.lines().next().expect("a header line");
        assert_eq!(
            first_line,
            "callsign,name,location,grid,source,entering_operator,signal_report,staying,precedence,traffic,worked,notes,public_note,via,relayed_by,checked_in_at"
        );
    }

    #[test]
    fn to_csv_emits_the_grid_cell_and_leaves_it_empty_when_absent() {
        // The grid rides its own column immediately after
        // `location`. An entry WITHOUT a grid emits an EMPTY cell — never the
        // string "None" — which is what the two rows below actually prove:
        // the first entry has a grid set, the second has `grid: None` (the
        // `base_entry` default, left untouched).
        let mut with_grid = base_entry(1, "W1AW", 0);
        with_grid.location = parse_location("Hartford CT").unwrap();
        with_grid.grid = Some(parse_grid("FN31pr").unwrap());
        let without_grid = base_entry(2, "N1CCK", 0);

        let csv = to_csv(
            &state_with(vec![with_grid, without_grid]),
            &BTreeMap::new(),
            &hf_only(),
        );
        let mut lines = csv.lines().skip(1);
        assert_eq!(
            lines.next().expect("first data row"),
            "W1AW,,Hartford CT,FN31pr,staff,,,in-and-out,routine,,false,,,,,1970-01-01T00:00:00+00:00"
        );
        assert_eq!(
            lines.next().expect("second data row"),
            "N1CCK,,,,staff,,,in-and-out,routine,,false,,,,,1970-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn to_adif_emits_gridsquare_beside_qth_only_when_a_grid_is_present() {
        // The canonical `Grid` string is valid ADIF as-is. A grid-less entry
        // emits NO tag, per the file's emit-only-when-present rule — proved by
        // the second entry, which leaves `base_entry`'s `grid: None` untouched.
        let mut with_grid = base_entry(1, "W1AW", 0);
        with_grid.location = parse_location("Hartford CT").unwrap();
        with_grid.grid = Some(parse_grid("FN31pr").unwrap());
        let without_grid = base_entry(2, "N1CCK", 0);

        let adif = to_adif(&state_with(vec![with_grid, without_grid]), &hf_only(), 0);
        assert!(
            adif.contains("<QTH:11>Hartford CT<GRIDSQUARE:6>FN31pr"),
            "the grid tag follows QTH: {adif}"
        );
        assert_eq!(
            adif.matches("<GRIDSQUARE:").count(),
            1,
            "only the grid-bearing entry emits a tag: {adif}"
        );
    }

    #[test]
    fn to_csv_projects_every_field_of_a_staff_entry_with_a_resolved_entering_operator() {
        let operator_id = Uuid::from_u128(900);
        let mut entry = base_entry(1, "W1AW", 0);
        entry.added_by = Some(operator_id);
        entry.source = CheckInSource::Staff;
        entry.name = parse_name("Maria").unwrap();
        entry.location = parse_location("Hartford, CT").unwrap();
        entry.signal_report = parse_signal_report("599").unwrap();
        entry.staying = StayingStatus::StayingForComments;
        entry.precedence = Precedence::Emergency;
        entry.traffic = parse_traffic_count(Some(3)).unwrap();
        entry.notes = parse_note("handled traffic").unwrap();
        // The operator's own log holds BOTH per-station notes.
        entry.public_note = parse_note("relaying for W1BBB").unwrap();
        // This test's name says EVERY field, so the relaying station
        // joins it. W3REL is neither the checked-in station nor the entering
        // operator, which is what makes the locked row below discriminate all
        // three from each other.
        entry.relayed_by = Some(parse_callsign("W3REL").expect("valid callsign"));
        entry.worked = true;

        let mut ops = BTreeMap::new();
        ops.insert(operator_id, "K1OP".to_owned());

        let csv = to_csv(&state_with(vec![entry]), &ops, &hf_only());
        let row = csv.lines().nth(1).expect("a data row");
        // location "Hartford, CT" carries a comma, so it is quoted; every other
        // cell is its plain token/value.
        // The entry carries no grid, so its cell (index 3) is EMPTY.
        assert_eq!(
            row,
            "W1AW,Maria,\"Hartford, CT\",,staff,K1OP,599,staying-for-comments,emergency,3,true,handled traffic,relaying for W1BBB,,W3REL,1970-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn to_csv_leaves_entering_operator_blank_for_a_self_entry() {
        // A self entry entered itself; even with an added_by present in the map,
        // its entering_operator column is blank (decision).
        let account = Uuid::from_u128(901);
        let mut entry = base_entry(2, "N1CCK", 0);
        entry.source = CheckInSource::SelfService;
        entry.added_by = Some(account);
        let mut ops = BTreeMap::new();
        ops.insert(account, "N1CCK".to_owned());

        let csv = to_csv(&state_with(vec![entry]), &ops, &hf_only());
        let row = csv.lines().nth(1).expect("a data row");
        let cells: Vec<&str> = row.split(',').collect();
        // Index 3 is `grid`, 4 is `source`, 5 is `entering_operator`.
        assert_eq!(cells[3], "", "no grid on this entry");
        assert_eq!(cells[4], "self");
        assert_eq!(cells[5], "", "self entry has a blank entering operator");
    }

    #[test]
    fn to_csv_neutralizes_a_formula_injection_in_a_captured_field() {
        let mut entry = base_entry(3, "W1AW", 0);
        entry.name = parse_name("=cmd|calc").unwrap();
        let csv = to_csv(&state_with(vec![entry]), &BTreeMap::new(), &hf_only());
        let row = csv.lines().nth(1).expect("a data row");
        // The name cell begins with an apostrophe so a spreadsheet cannot evaluate
        // it — the raw bytes carry the neutralizer.
        assert!(
            row.contains(",'=cmd|calc,"),
            "expected a neutralized name cell in {row}"
        );
    }

    // --- adif_field — the byte-length landmine ---------------------

    #[test]
    fn adif_field_length_prefix_is_the_utf8_byte_length_not_char_count() {
        // `José Ñ` is 8 UTF-8 bytes but only 6 chars — the prefix MUST be 8, or
        // every ADIF parser mis-reads the record. This is the single most
        // important ADIF correctness point.
        assert_eq!(adif_field("NAME", "José Ñ"), "<NAME:8>José Ñ");
        assert_eq!(adif_field("CALL", "W1AW"), "<CALL:4>W1AW");
    }

    // --- band_to_adif / mode_to_adif -------------------------------

    #[test]
    fn band_2200m_maps_to_the_adif_2190m_quirk() {
        assert_eq!(band_to_adif("2200m"), Some("2190m"));
    }

    #[test]
    fn band_other_and_unknown_map_to_none() {
        assert_eq!(band_to_adif("other"), None);
        assert_eq!(band_to_adif("not-a-band"), None);
    }

    #[test]
    fn ordinary_bands_pass_through_unchanged() {
        assert_eq!(band_to_adif("20m"), Some("20m"));
        assert_eq!(band_to_adif("70cm"), Some("70cm"));
        assert_eq!(band_to_adif("1.25m"), Some("1.25m"));
    }

    #[test]
    fn modes_map_to_their_adif_enum_and_digital_mixed_omit() {
        assert_eq!(mode_to_adif("ssb"), Some("SSB"));
        assert_eq!(mode_to_adif("cw"), Some("CW"));
        assert_eq!(mode_to_adif("am"), Some("AM"));
        assert_eq!(mode_to_adif("fm"), Some("FM"));
        // No valid ADIF enum exists for these — omit rather than fabricate.
        assert_eq!(mode_to_adif("digital"), None);
        assert_eq!(mode_to_adif("mixed"), None);
    }

    // --- to_adif ---------------------------------------------------

    #[test]
    fn to_adif_header_ends_with_eoh_and_carries_the_program_id() {
        let adif = to_adif(&state_with(vec![]), &hf_only(), 0);
        assert!(adif.contains("<ADIF_VER:5>3.1.4"));
        assert!(adif.contains("<PROGRAMID:7>NetRoll"));
        assert!(adif.contains("<CREATED_TIMESTAMP:15>19700101 000000"));
        assert!(adif.contains("<EOH>"));
    }

    #[test]
    fn to_adif_emits_one_eor_per_roster_entry() {
        let roster = vec![
            base_entry(1, "W1AW", 0),
            base_entry(2, "N1CCK", 0),
            base_entry(3, "K1XYZ", 0),
        ];
        let adif = to_adif(&state_with(roster), &hf_only(), 0);
        assert_eq!(adif.matches("<EOR>").count(), 3);
        assert_eq!(adif.matches("<CALL:").count(), 3);
    }

    #[test]
    fn to_adif_derives_qso_date_and_time_on_in_utc_from_added_at() {
        // added_at = 0 → 1970-01-01T00:00:00Z → QSO_DATE 19700101, TIME_ON 000000.
        let adif = to_adif(&state_with(vec![base_entry(1, "W1AW", 0)]), &hf_only(), 0);
        assert!(adif.contains("<QSO_DATE:8>19700101"));
        assert!(adif.contains("<TIME_ON:6>000000"));
    }

    #[test]
    fn to_adif_freq_is_always_emitted_in_mhz() {
        let adif = to_adif(
            &state_with(vec![base_entry(1, "W1AW", 0)]),
            &rf_only(14_250_000, Band::TwentyMeters, Mode::Ssb),
            0,
        );
        assert!(adif.contains("<FREQ:5>14.25"));
    }

    #[test]
    fn to_adif_maps_band_2200m_to_2190m_and_ssb_to_uppercase() {
        let adif = to_adif(
            &state_with(vec![base_entry(1, "W1AW", 0)]),
            &rf_only(135_700, Band::TwentyTwoHundredMeters, Mode::Ssb),
            0,
        );
        assert!(adif.contains("<BAND:5>2190m"));
        assert!(adif.contains("<MODE:3>SSB"));
    }

    #[test]
    fn to_adif_omits_band_for_other_and_mode_for_digital_relying_on_freq() {
        let adif = to_adif(
            &state_with(vec![base_entry(1, "W1AW", 0)]),
            &rf_only(14_250_000, Band::Other, Mode::Digital),
            0,
        );
        assert!(!adif.contains("<BAND:"), "no ADIF band for `other`");
        assert!(!adif.contains("<MODE:"), "no ADIF mode for `digital`");
        // The record still anchors on FREQ.
        assert!(adif.contains("<FREQ:"));
        assert!(adif.contains("<EOR>"));
    }

    #[test]
    fn to_adif_emits_optional_fields_only_when_present() {
        let mut entry = base_entry(1, "W1AW", 0);
        entry.signal_report = parse_signal_report("599").unwrap();
        entry.name = parse_name("José Ñ").unwrap();
        entry.location = parse_location("Hartford").unwrap();
        let adif = to_adif(&state_with(vec![entry]), &hf_only(), 0);
        assert!(adif.contains("<RST_SENT:3>599"));
        // The non-ASCII name proves the byte-length framing end-to-end.
        assert!(adif.contains("<NAME:8>José Ñ"));
        assert!(adif.contains("<QTH:8>Hartford"));

        // An entry with none of the optionals emits none of those tags.
        let bare = to_adif(&state_with(vec![base_entry(2, "N1CCK", 0)]), &hf_only(), 0);
        assert!(!bare.contains("<RST_SENT:"));
        assert!(!bare.contains("<NAME:"));
        assert!(!bare.contains("<QTH:"));
        assert!(!bare.contains("<NOTES:"));
    }

    // --- Band, mode and propagation PER QSO ---------------------

    /// A net reachable over EchoLink first and 20m HF second — the cross-mode
    /// shape these exports exist for.
    fn cross_mode() -> Vec<NetConnectionWire> {
        wire_connections(
            &NetConnectionSet::new(vec![
                NetConnection {
                    id: Uuid::from_u128(0xE1),
                    position: 0,
                    kind: NetConnectionKind::EchoLink {
                        node: "12345".to_owned(),
                    },
                },
                NetConnection {
                    id: Uuid::from_u128(0xF1),
                    position: 1,
                    kind: NetConnectionKind::Hf {
                        planned_frequency_hz: 14_230_000,
                        band: Band::TwentyMeters,
                        mode: Mode::Ssb,
                    },
                },
            ])
            .expect("two connections are a valid set"),
        )
    }

    /// The QSO record for `call`, sliced out of a whole `.adi` document.
    fn record_for<'a>(adif: &'a str, call: &str) -> &'a str {
        let needle = format!("<CALL:{}>{call}<", call.len());
        let start = adif
            .find(&needle)
            .unwrap_or_else(|| panic!("no record for {call} in {adif}"));
        let end = adif[start..]
            .find("<EOR>")
            .expect("every record is terminated");
        &adif[start..start + end]
    }

    #[test]
    fn two_qsos_on_one_cross_mode_net_export_different_band_and_prop_mode() {
        let mut over_echolink = base_entry(1, "W1AW", 0);
        over_echolink.via = Some(Via::Connection(Uuid::from_u128(0xE1)));
        let mut over_hf = base_entry(2, "N1CCK", 0);
        over_hf.via = Some(Via::Connection(Uuid::from_u128(0xF1)));

        let adif = to_adif(&state_with(vec![over_echolink, over_hf]), &cross_mode(), 0);
        let echolink_record = record_for(&adif, "W1AW");
        let hf_record = record_for(&adif, "N1CCK");

        assert!(echolink_record.contains("<PROP_MODE:3>ECH"));
        assert!(
            !echolink_record.contains("<BAND:") && !echolink_record.contains("<FREQ:"),
            "an EchoLink QSO has no band to state, so the tag is omitted not fabricated: {echolink_record}"
        );
        assert!(hf_record.contains("<BAND:3>20m"));
        assert!(hf_record.contains("<MODE:3>SSB"));
        assert!(hf_record.contains("<FREQ:5>14.23"));
        assert!(
            !hf_record.contains("<PROP_MODE:"),
            "an HF QSO travelled by ordinary propagation and ADIF's default says so"
        );
    }

    #[test]
    fn every_internet_only_kind_carries_adifs_own_propagation_value_and_no_band() {
        // `INT` is NOT a member of ADIF's Propagation_Mode enumeration in 3.1.4
        // — the version this file declares — or in 3.1.7. `ECH` and `INTERNET`
        // are.
        for (kind, expected) in [
            (
                NetConnectionKind::EchoLink {
                    node: "1".to_owned(),
                },
                "ECH",
            ),
            (
                NetConnectionKind::AllStar {
                    node: "2".to_owned(),
                },
                "INTERNET",
            ),
            (
                NetConnectionKind::Dmr {
                    talkgroup: "31337".to_owned(),
                    network: Some("Brandmeister".to_owned()),
                },
                "INTERNET",
            ),
            (
                NetConnectionKind::DStar {
                    reflector: "REF030 C".to_owned(),
                },
                "INTERNET",
            ),
            (
                NetConnectionKind::Ysf {
                    reflector: "America Link".to_owned(),
                },
                "INTERNET",
            ),
            (
                NetConnectionKind::Urf {
                    reflector: "URF307 B".to_owned(),
                },
                "INTERNET",
            ),
        ] {
            let connections = wire_connections(
                &NetConnectionSet::new(vec![NetConnection {
                    id: Uuid::from_u128(0xA1),
                    position: 0,
                    kind,
                }])
                .expect("one connection is a valid set"),
            );
            let mut entry = base_entry(1, "W1AW", 0);
            entry.via = Some(Via::Connection(Uuid::from_u128(0xA1)));
            let adif = to_adif(&state_with(vec![entry]), &connections, 0);
            assert!(
                adif.contains(&format!("<PROP_MODE:{}>{expected}", expected.len())),
                "expected {expected}: {adif}"
            );
            assert!(!adif.contains("<BAND:"));
            assert!(!adif.contains("<FREQ:"));
        }
    }

    #[test]
    fn an_unrecorded_via_falls_back_for_the_adif_and_for_nothing_else() {
        // Both halves in ONE run: the `.adi` carries the export connection's
        // band AND the CSV for the SAME entry says nothing.
        let entry = base_entry(1, "W1AW", 0);
        assert_eq!(entry.via, None);
        let state = state_with(vec![entry]);
        let connections = cross_mode();

        let adif = to_adif(&state, &connections, 0);
        assert!(
            adif.contains("<BAND:3>20m"),
            "export falls back to the session's ADIF-export connection: {adif}"
        );

        let csv = to_csv(&state, &BTreeMap::new(), &connections);
        let row = csv.lines().nth(1).expect("a data row");
        let cells = row.split(',').collect::<Vec<_>>();
        assert_eq!(
            cells[13], "",
            "the CSV `via` cell stays EMPTY — the fallback is an export decision, never data"
        );
    }

    #[test]
    fn an_unresolvable_via_exports_no_band_and_the_csv_names_the_fault() {
        let mut entry = base_entry(1, "W1AW", 0);
        entry.via = Some(Via::Connection(Uuid::from_u128(0xDEAD)));
        let state = state_with(vec![entry]);
        let connections = cross_mode();

        let adif = to_adif(&state, &connections, 0);
        assert!(
            !adif.contains("<BAND:") && !adif.contains("<FREQ:") && !adif.contains("<PROP_MODE:"),
            "borrowing the export connection's band here is the lie this refuses: {adif}"
        );

        let csv = to_csv(&state, &BTreeMap::new(), &connections);
        let cell = csv.lines().nth(1).expect("a data row").split(',').nth(13);
        assert_eq!(cell, Some(UNRESOLVABLE_VIA_LABEL));
        assert_ne!(
            cell,
            Some(""),
            "`unresolvable` and `not recorded` are different facts and never collapse"
        );
    }

    #[test]
    fn the_csv_via_cell_is_the_connections_label_and_never_its_id() {
        let mut entry = base_entry(1, "W1AW", 0);
        entry.via = Some(Via::Connection(Uuid::from_u128(0xF1)));
        let csv = to_csv(&state_with(vec![entry]), &BTreeMap::new(), &cross_mode());
        let row = csv.lines().nth(1).expect("a data row");
        let cell = row.split(',').nth(13).expect("the via cell");
        assert_eq!(cell, "HF — 14.230 MHz");
        assert!(!row.contains(&Uuid::from_u128(0xF1).to_string()));
    }

    #[test]
    fn a_free_text_via_prints_the_operators_words_in_the_csv_and_no_adif_tags() {
        let mut entry = base_entry(1, "W1AW", 0);
        entry.via = Some(Via::Unlisted("phone patch".to_owned()));
        let state = state_with(vec![entry]);
        let connections = cross_mode();
        let csv = to_csv(&state, &BTreeMap::new(), &connections);
        assert_eq!(
            csv.lines().nth(1).expect("a data row").split(',').nth(13),
            Some("phone patch")
        );
        let adif = to_adif(&state, &connections, 0);
        assert!(!adif.contains("<BAND:") && !adif.contains("<PROP_MODE:"));
    }

    // --- The relaying station through the exports ---------------

    #[test]
    fn the_csv_names_the_relaying_station_between_the_way_in_and_the_timestamp() {
        // The column sits AFTER `via` and BEFORE `checked_in_at`: it was
        // inserted there rather than appended.
        let via_at = CSV_COLUMNS
            .iter()
            .position(|c| *c == "via")
            .expect("the way in is a column");
        let relay_at = CSV_COLUMNS
            .iter()
            .position(|c| *c == "relayed_by")
            .expect("the relaying station is a column");
        let stamp_at = CSV_COLUMNS
            .iter()
            .position(|c| *c == "checked_in_at")
            .expect("the timestamp is a column");
        assert_eq!(relay_at, via_at + 1);
        assert_eq!(stamp_at, relay_at + 1);
        assert_eq!(stamp_at, CSV_COLUMNS.len() - 1, "the timestamp stays last");
    }

    #[test]
    fn a_relayed_row_carries_the_relaying_stations_callsign_in_its_own_cell() {
        let state = state_with(vec![relayed_by(
            base_entry(1, "W1AW", 1_700_000_000_000),
            "W3REL",
        )]);
        let csv = to_csv(&state, &BTreeMap::new(), &hf_only());
        let row = csv.lines().nth(1).expect("one data row");
        let cells: Vec<&str> = row.split(',').collect();
        let at = CSV_COLUMNS
            .iter()
            .position(|c| *c == "relayed_by")
            .expect("the column exists");
        assert_eq!(cells[at], "W3REL");
    }

    #[test]
    fn an_unrelayed_rows_cell_is_empty_and_is_never_filled_in_from_the_entering_operator() {
        // The trap: `entering_operator` answers WHICH ACCOUNT TYPED the
        // entry and `relayed_by` WHICH STATION PASSED THE TRAFFIC. Borrowing one
        // for the other would claim every staff-logged station was relayed by
        // whoever logged it.
        let operator = Uuid::from_u128(0xA1);
        let mut entry = base_entry(1, "W1AW", 1_700_000_000_000);
        entry.added_by = Some(operator);
        let mut ops = BTreeMap::new();
        ops.insert(operator, "K1OP".to_owned());

        let csv = to_csv(&state_with(vec![entry]), &ops, &hf_only());
        let row = csv.lines().nth(1).expect("one data row");
        let cells: Vec<&str> = row.split(',').collect();
        let entering_at = CSV_COLUMNS
            .iter()
            .position(|c| *c == "entering_operator")
            .expect("the column exists");
        let relay_at = CSV_COLUMNS
            .iter()
            .position(|c| *c == "relayed_by")
            .expect("the column exists");
        assert_eq!(
            cells[entering_at], "K1OP",
            "the entering operator genuinely resolves, so the next assertion is not vacuous"
        );
        assert_eq!(
            cells[relay_at], "",
            "an empty cell means NOT RELAYED and is never borrowed from the logging operator"
        );
    }

    #[test]
    fn the_adif_for_a_relayed_entry_is_byte_identical_to_the_same_entry_unrelayed() {
        // ADIF 3.1.4 names no relay tag — verified against the full spec
        // text at drafting time — so the record omits it entirely. Every
        // callsign-bearing QSO field names a DIFFERENT party, and an
        // application-defined `APP_` tag would be one NetRoll writes and nobody
        // reads. An invented tag reaches LoTW and QRZ and cannot be recalled.
        let plain = base_entry(1, "W1AW", 1_700_000_000_000);
        let relayed = relayed_by(plain.clone(), "W3REL");
        let connections = hf_only();
        let a = to_adif(&state_with(vec![plain]), &connections, 1_700_000_000_000);
        let b = to_adif(&state_with(vec![relayed]), &connections, 1_700_000_000_000);
        assert_eq!(a, b, "the relaying station reaches no ADIF tag at all");
        assert!(
            !a.to_ascii_uppercase().contains("W3REL"),
            "and the callsign's own bytes are absent from the file"
        );
    }

    // --- Each record's frequency is the one IT was worked on -----

    /// An entry that arrived at `added_seq` over `via`.
    fn entry_at(id: u128, call: &str, added_seq: u64, via: Via) -> RosterEntry {
        let mut entry = base_entry(id, call, 1_700_000_000_000 + added_seq * 1_000);
        entry.added_seq = added_seq;
        entry.via = Some(via);
        entry
    }

    /// A folded state whose roster is `roster` and whose log carried `moves`, in
    /// seq order — both the ordered run and the last-write-wins map the fold
    /// would have produced, so `live_connections` and `connections_at` agree.
    fn state_moved(roster: Vec<RosterEntry>, moves: Vec<FrequencyMove>) -> SessionState {
        let connection_frequencies = moves
            .iter()
            .map(|m| (m.connection_id, m.operating_frequency_hz))
            .collect();
        SessionState {
            roster,
            frequency_moves: moves,
            connection_frequencies,
            ..Default::default()
        }
    }

    fn hf_id() -> Uuid {
        hf_only()[0].id
    }

    fn moved(seq: u64, connection_id: Uuid, hz: i64) -> FrequencyMove {
        FrequencyMove {
            seq,
            connection_id,
            operating_frequency_hz: hz,
        }
    }

    fn adif_records(adif: &str) -> Vec<&str> {
        adif.split("<EOR>")
            .filter(|record| record.contains("<CALL:"))
            .collect()
    }

    fn via_cell(csv: &str, row: usize) -> &str {
        csv.lines()
            .nth(row)
            .expect("a data row")
            .split(',')
            .nth(13)
            .expect("the via cell")
    }

    #[test]
    fn a_qso_worked_before_a_move_exports_the_frequency_it_was_worked_on() {
        // On the file that reaches LoTW: added at seq 2, the net
        // QSYs at seq 3, a second station at seq 4. The snapshot is FROZEN —
        // the caller never overlays it — and each record overlays it as at its
        // own seq.
        let hf = hf_id();
        let state = state_moved(
            vec![
                entry_at(1, "W1AW", 2, Via::Connection(hf)),
                entry_at(2, "W1ABC", 4, Via::Connection(hf)),
            ],
            vec![moved(3, hf, 14_250_000)],
        );
        let snapshot = hf_only();

        let adif = to_adif(&state, &snapshot, 1_700_000_000_000);
        let records = adif_records(&adif);
        assert_eq!(records.len(), 2);
        assert!(records[0].contains("<FREQ:5>14.23"), "{}", records[0]);
        assert!(records[1].contains("<FREQ:5>14.25"), "{}", records[1]);
        // The owner's band token is not re-derived after a move.
        assert!(records[0].contains("<BAND:3>20m") && records[1].contains("<BAND:3>20m"));

        let csv = to_csv(&state, &BTreeMap::new(), &snapshot);
        assert_eq!(via_cell(&csv, 1), "HF — 14.230 MHz");
        assert_eq!(via_cell(&csv, 2), "HF — 14.250 MHz");
    }

    #[test]
    fn an_unrecorded_via_falls_back_to_the_export_connection_as_at_its_own_seq() {
        // The ONE permitted fallback picks from the as-at set too: a station
        // nobody recorded a way in for, worked before the move, is described
        // by the frequency in force THEN — not the one the net finished on.
        let hf = hf_id();
        let mut before = base_entry(1, "W1AW", 1_700_000_002_000);
        before.added_seq = 2;
        let mut after = base_entry(2, "W1ABC", 1_700_000_004_000);
        after.added_seq = 4;
        let state = state_moved(vec![before, after], vec![moved(3, hf, 14_250_000)]);

        let adif = to_adif(&state, &hf_only(), 1_700_000_000_000);
        let records = adif_records(&adif);
        assert!(records[0].contains("<FREQ:5>14.23"), "{}", records[0]);
        assert!(records[1].contains("<FREQ:5>14.25"), "{}", records[1]);
    }

    #[test]
    fn a_via_corrected_after_a_move_exports_the_new_connections_frequency_as_at_the_add_seq() {
        // The `checkin.updated` edge: added at seq 2 on A, B
        // moved at seq 3, via corrected to B at seq 4. The correction fixes
        // WHICH way in; the add fixes WHEN — so B's PLANNED frequency, the one
        // B was on when this station actually checked in, never the moved one.
        let a = Uuid::from_u128(0xA);
        let b = Uuid::from_u128(0xB);
        let snapshot = wire_connections(
            &NetConnectionSet::new(vec![
                NetConnection {
                    id: a,
                    position: 0,
                    kind: NetConnectionKind::Hf {
                        planned_frequency_hz: 14_230_000,
                        band: Band::TwentyMeters,
                        mode: Mode::Ssb,
                    },
                },
                NetConnection {
                    id: b,
                    position: 1,
                    kind: NetConnectionKind::Hf {
                        planned_frequency_hz: 7_200_000,
                        band: Band::TwentyMeters,
                        mode: Mode::Ssb,
                    },
                },
            ])
            .expect("two connections are a valid set"),
        );
        // The fold's output after the correction: `via` is B, `added_seq` is 2.
        let state = state_moved(
            vec![entry_at(1, "W1AW", 2, Via::Connection(b))],
            vec![moved(3, b, 7_250_000)],
        );

        let adif = to_adif(&state, &snapshot, 1_700_000_000_000);
        assert!(
            adif.contains("<FREQ:3>7.2"),
            "B's planned frequency: {adif}"
        );
        assert!(
            !adif.contains("7.25"),
            "never the frequency B moved to: {adif}"
        );
        let csv = to_csv(&state, &BTreeMap::new(), &snapshot);
        assert_eq!(via_cell(&csv, 1), "HF — 7.200 MHz");
    }

    #[test]
    fn a_never_moving_net_exports_the_snapshot_unchanged() {
        // Why the never-moving fixtures are not evidence: with no moves the
        // as-at set IS the snapshot, on every seq.
        let hf = hf_id();
        let state = state_with(vec![
            entry_at(1, "W1AW", 2, Via::Connection(hf)),
            entry_at(2, "W1ABC", 4, Via::Connection(hf)),
        ]);
        let csv = to_csv(&state, &BTreeMap::new(), &hf_only());
        assert_eq!(via_cell(&csv, 1), via_cell(&csv, 2));
        assert_eq!(via_cell(&csv, 1), "HF — 14.230 MHz");
    }
}
