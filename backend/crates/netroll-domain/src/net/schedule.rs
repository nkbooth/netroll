// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Net scheduling: the typed [`Schedule`], its validation, and DST-correct
//! occurrence generation. Pure — the window bounds and `now` arrive from the
//! injected clock. An OCCURRENCE is a planned, read-only pointer with NO
//! snapshot, unlike a session, and carrying none is exactly why a definition
//! edit shows up in future occurrences.

use chrono::{Datelike, Duration, LocalResult, NaiveDate, TimeZone, Weekday};
use chrono_tz::Tz;
use thiserror::Error;
use uuid::Uuid;

/// How often a recurring schedule fires, as a lowercase wire/storage token.
/// The variant list is the ONLY place the frequency taxonomy lives (the
/// `enums.rs` precedent) — a new frequency is a one-line addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    /// Every day at the local time-of-day.
    Daily,
    /// Every week on the rule's weekday at the local time-of-day.
    Weekly,
    /// Every month on the rule's day-of-month at the local time-of-day.
    Monthly,
}

impl Frequency {
    /// The stable lowercase wire/storage spelling of this frequency.
    pub fn as_str(self) -> &'static str {
        match self {
            Frequency::Daily => "daily",
            Frequency::Weekly => "weekly",
            Frequency::Monthly => "monthly",
        }
    }
}

impl TryFrom<&str> for Frequency {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "daily" => Ok(Frequency::Daily),
            "weekly" => Ok(Frequency::Weekly),
            "monthly" => Ok(Frequency::Monthly),
            _ => Err(()),
        }
    }
}

/// A validated recurring rule: a frequency, an IANA timezone, a local
/// `HH:MM`, and — enforced by [`parse_schedule`] — a weekday iff weekly and a
/// day-of-month iff monthly. Constructed ONLY through the parser so the
/// weekly⇒weekday / monthly⇒day-of-month invariant always holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecurringSchedule {
    /// Recurrence frequency.
    pub frequency: Frequency,
    /// The IANA timezone the local time-of-day is interpreted in.
    pub timezone: Tz,
    /// Local hour, `0..=23`.
    pub hour: u8,
    /// Local minute, `0..=59`.
    pub minute: u8,
    /// Weekday — `Some` iff `frequency == Weekly`.
    pub weekday: Option<Weekday>,
    /// Day-of-month `1..=31` — `Some` iff `frequency == Monthly`.
    pub day_of_month: Option<u8>,
}

/// A net's schedule: either a single planned instant or a recurring rule,
/// both timezone-aware. Built only via [`parse_schedule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// A single planned occurrence at an absolute UTC instant, carrying the
    /// IANA timezone it was expressed in (for display labelling).
    OneOff {
        /// Absolute start instant, epoch millis (UTC).
        start_at_millis: u64,
        /// The IANA timezone the instant was expressed in.
        timezone: Tz,
    },
    /// A recurring rule generating occurrences into a rolling horizon.
    Recurring(RecurringSchedule),
}

/// The unvalidated inbound schedule, every field as it arrives on the wire.
/// [`parse_schedule`] turns it into a typed [`Schedule`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawSchedule {
    /// Schedule kind token: `one-off` or `recurring` (required).
    pub kind: Option<String>,
    /// IANA timezone name (required).
    pub timezone: Option<String>,
    /// One-off start instant as an RFC 3339 date-time (required iff one-off).
    pub one_off_start_at: Option<String>,
    /// Frequency token (required iff recurring).
    pub frequency: Option<String>,
    /// Local time-of-day as `HH:MM`, 24-hour (required iff recurring).
    pub time_of_day: Option<String>,
    /// Weekday token, lowercase full name (required iff weekly).
    pub weekday: Option<String>,
    /// Day-of-month `1..=31` (required iff monthly).
    pub day_of_month: Option<String>,
}

/// Why a submitted schedule was rejected — one variant per field and reason.
/// `Display` is `"<field>: <reason>"` so the HTTP `detail` names the offending
/// field; distinct fields yield distinct text.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScheduleError {
    /// Kind was absent (required).
    #[error("kind: is required")]
    KindRequired,
    /// Kind token is neither `one-off` nor `recurring`.
    #[error("kind: is not a recognized schedule kind")]
    KindUnknown,
    /// Timezone was absent (required).
    #[error("timezone: is required")]
    TimezoneRequired,
    /// Timezone name is not in the IANA database.
    #[error("timezone: is not a recognized IANA timezone")]
    TimezoneUnknown,
    /// One-off start instant was absent (required for a one-off).
    #[error("start: is required for a one-off schedule")]
    StartRequired,
    /// One-off start instant is not a valid RFC 3339 date-time.
    #[error("start: must be an RFC 3339 date-time")]
    StartInvalid,
    /// Frequency was absent (required for a recurring schedule).
    #[error("frequency: is required for a recurring schedule")]
    FrequencyRequired,
    /// Frequency token is not a known frequency.
    #[error("frequency: is not a recognized frequency")]
    FrequencyUnknown,
    /// Time-of-day was absent (required for a recurring schedule).
    #[error("time of day: is required for a recurring schedule")]
    TimeOfDayRequired,
    /// Time-of-day is not a valid 24-hour `HH:MM`.
    #[error("time of day: must be HH:MM (24-hour)")]
    TimeOfDayInvalid,
    /// Weekday absent for a weekly schedule.
    #[error("weekday: is required for a weekly schedule")]
    WeekdayRequired,
    /// Weekday token is not a known weekday.
    #[error("weekday: is not a recognized weekday")]
    WeekdayUnknown,
    /// Day-of-month absent for a monthly schedule.
    #[error("day of month: is required for a monthly schedule")]
    DayOfMonthRequired,
    /// Day-of-month is not an integer in `1..=31`.
    #[error("day of month: must be between 1 and 31")]
    DayOfMonthInvalid,
}

/// A planned occurrence read back from storage: a `(definition_id,
/// scheduled_start_at)` pointer and NOTHING else. It carries NO definition
/// snapshot, no roster, no event log, no `seq` — that is what distinguishes it
/// from a `net_session`, and is why a definition edit is reflected
/// in future occurrences (discovery reads the live definition). Times are
/// epoch millis, the domain's currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetOccurrence {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The definition this occurrence is planned for.
    pub definition_id: Uuid,
    /// Absolute planned start instant, epoch millis (UTC).
    pub scheduled_start_at_millis: u64,
}

/// Parses a lowercase full weekday name (`monday`..`sunday`) to a
/// [`chrono::Weekday`]. Case-sensitive, mirroring the enum tokens.
fn parse_weekday(token: &str) -> Result<Weekday, ScheduleError> {
    match token {
        "monday" => Ok(Weekday::Mon),
        "tuesday" => Ok(Weekday::Tue),
        "wednesday" => Ok(Weekday::Wed),
        "thursday" => Ok(Weekday::Thu),
        "friday" => Ok(Weekday::Fri),
        "saturday" => Ok(Weekday::Sat),
        "sunday" => Ok(Weekday::Sun),
        _ => Err(ScheduleError::WeekdayUnknown),
    }
}

/// The lowercase full-name wire token for a weekday (the inverse of
/// [`parse_weekday`]).
pub fn weekday_token(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "monday",
        Weekday::Tue => "tuesday",
        Weekday::Wed => "wednesday",
        Weekday::Thu => "thursday",
        Weekday::Fri => "friday",
        Weekday::Sat => "saturday",
        Weekday::Sun => "sunday",
    }
}

/// Parses a 24-hour `HH:MM` local time-of-day into `(hour, minute)`.
fn parse_time_of_day(input: &str) -> Result<(u8, u8), ScheduleError> {
    let (h, m) = input
        .split_once(':')
        .ok_or(ScheduleError::TimeOfDayInvalid)?;
    if h.len() != 2 || m.len() != 2 {
        return Err(ScheduleError::TimeOfDayInvalid);
    }
    let hour: u8 = h.parse().map_err(|_| ScheduleError::TimeOfDayInvalid)?;
    let minute: u8 = m.parse().map_err(|_| ScheduleError::TimeOfDayInvalid)?;
    if hour > 23 || minute > 59 {
        return Err(ScheduleError::TimeOfDayInvalid);
    }
    Ok((hour, minute))
}

/// Validates a [`RawSchedule`], failing fast on the FIRST invalid field so no
/// schedule or occurrence row is ever written for a bad request. Common
/// fields (kind, timezone) are validated before kind-specific ones.
pub fn parse_schedule(raw: RawSchedule) -> Result<Schedule, ScheduleError> {
    // Kind (required) then timezone (required) — the common fields lead so a
    // request bad in a common AND a kind-specific field surfaces the common
    // error first (deterministic fail-fast, the documented ordering).
    let kind = match raw.kind.as_deref() {
        None => return Err(ScheduleError::KindRequired),
        Some(s) if s.trim().is_empty() => return Err(ScheduleError::KindRequired),
        Some(s) => s,
    };

    let timezone = match raw.timezone.as_deref() {
        None => return Err(ScheduleError::TimezoneRequired),
        Some(s) if s.trim().is_empty() => return Err(ScheduleError::TimezoneRequired),
        Some(s) => s
            .parse::<Tz>()
            .map_err(|_| ScheduleError::TimezoneUnknown)?,
    };

    match kind {
        "one-off" => {
            let start = raw.one_off_start_at.as_deref().unwrap_or_default();
            if start.trim().is_empty() {
                return Err(ScheduleError::StartRequired);
            }
            let instant = chrono::DateTime::parse_from_rfc3339(start)
                .map_err(|_| ScheduleError::StartInvalid)?;
            let millis = instant.timestamp_millis();
            // A pre-epoch instant is almost certainly a typo'd year (e.g.
            // 1926 for 2026) — reject it rather than silently clamping to
            // epoch 0, which would schedule the net for 1970-01-01.
            if millis < 0 {
                return Err(ScheduleError::StartInvalid);
            }
            let start_at_millis = millis as u64;
            Ok(Schedule::OneOff {
                start_at_millis,
                timezone,
            })
        }
        "recurring" => {
            let frequency = match raw.frequency.as_deref() {
                None => return Err(ScheduleError::FrequencyRequired),
                Some(s) if s.trim().is_empty() => return Err(ScheduleError::FrequencyRequired),
                Some(s) => Frequency::try_from(s).map_err(|()| ScheduleError::FrequencyUnknown)?,
            };

            let (hour, minute) = match raw.time_of_day.as_deref() {
                None => return Err(ScheduleError::TimeOfDayRequired),
                Some(s) if s.trim().is_empty() => return Err(ScheduleError::TimeOfDayRequired),
                Some(s) => parse_time_of_day(s)?,
            };

            let (weekday, day_of_month) = match frequency {
                Frequency::Daily => (None, None),
                Frequency::Weekly => {
                    let token = match raw.weekday.as_deref() {
                        None => return Err(ScheduleError::WeekdayRequired),
                        Some(s) if s.trim().is_empty() => {
                            return Err(ScheduleError::WeekdayRequired);
                        }
                        Some(s) => s,
                    };
                    (Some(parse_weekday(token)?), None)
                }
                Frequency::Monthly => {
                    let dom_str = match raw.day_of_month.as_deref() {
                        None => return Err(ScheduleError::DayOfMonthRequired),
                        Some(s) if s.trim().is_empty() => {
                            return Err(ScheduleError::DayOfMonthRequired);
                        }
                        Some(s) => s,
                    };
                    let dom: u8 = dom_str
                        .trim()
                        .parse()
                        .map_err(|_| ScheduleError::DayOfMonthInvalid)?;
                    if !(1..=31).contains(&dom) {
                        return Err(ScheduleError::DayOfMonthInvalid);
                    }
                    (None, Some(dom))
                }
            };

            Ok(Schedule::Recurring(RecurringSchedule {
                frequency,
                timezone,
                hour,
                minute,
                weekday,
                day_of_month,
            }))
        }
        _ => Err(ScheduleError::KindUnknown),
    }
}

/// Generates the UTC-instant (epoch-millis) start times in the half-open
/// window `(from_millis, to_millis]`, ascending, for `schedule`. Pure and
/// deterministic — the same args always yield the same vector (the property
/// the `ON CONFLICT` dedup relies on). DST-correct: a recurring local `HH:MM`
/// maps to its wall-clock instant in the schedule's timezone each period.
pub fn occurrences_between(schedule: &Schedule, from_millis: u64, to_millis: u64) -> Vec<u64> {
    if to_millis <= from_millis {
        return Vec::new();
    }
    match schedule {
        Schedule::OneOff {
            start_at_millis, ..
        } => {
            // Half-open (from, to]: from exclusive, to inclusive.
            if *start_at_millis > from_millis && *start_at_millis <= to_millis {
                vec![*start_at_millis]
            } else {
                Vec::new()
            }
        }
        Schedule::Recurring(rule) => recurring_occurrences(rule, from_millis, to_millis),
    }
}

/// Resolves a local wall-clock time in `tz` to a concrete instant, applying
/// the documented DST policies: an AMBIGUOUS local time (fall-back overlap)
/// uses the EARLIEST (pre-transition) instant; a NONEXISTENT local time
/// (spring-forward gap) uses the NEXT VALID instant. Never `.unwrap()`s a
/// `LocalResult`.
fn resolve_local(tz: &Tz, naive: chrono::NaiveDateTime) -> Option<chrono::DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt),
        LocalResult::Ambiguous(earliest, _latest) => Some(earliest),
        LocalResult::None => {
            // Spring-forward gap: this local time never happened. Probe
            // forward minute-by-minute out of the gap (gaps are at most a
            // couple of hours) to the next valid instant.
            let mut probe = naive;
            for _ in 0..(6 * 60) {
                probe += Duration::minutes(1);
                match tz.from_local_datetime(&probe) {
                    LocalResult::Single(dt) => return Some(dt),
                    LocalResult::Ambiguous(earliest, _) => return Some(earliest),
                    LocalResult::None => continue,
                }
            }
            None
        }
    }
}

/// Generates recurring occurrence instants by walking candidate LOCAL dates
/// day-by-day and applying a per-date filter (daily = every day; weekly =
/// matching weekday; monthly = matching day-of-month, which naturally SKIPS
/// months lacking that day). Each candidate date's local `HH:MM` is resolved
/// to a UTC instant via [`resolve_local`], filtered to the half-open window,
/// then sorted ascending and de-duplicated.
fn recurring_occurrences(rule: &RecurringSchedule, from_millis: u64, to_millis: u64) -> Vec<u64> {
    let tz = rule.timezone;
    let (Some(from_utc), Some(to_utc)) = (
        chrono::DateTime::from_timestamp_millis(from_millis as i64),
        chrono::DateTime::from_timestamp_millis(to_millis as i64),
    ) else {
        return Vec::new();
    };

    // A one-day margin on each side: an in-window instant can map to a local
    // date just outside [from_date, to_date] because of the tz offset. The
    // (from, to] instant filter removes any extras the margin admits.
    let start_date = from_utc.with_timezone(&tz).date_naive() - Duration::days(1);
    let end_date = to_utc.with_timezone(&tz).date_naive() + Duration::days(1);

    let mut out: Vec<u64> = Vec::new();
    let mut date = start_date;
    while date <= end_date {
        let is_candidate = match rule.frequency {
            Frequency::Daily => true,
            Frequency::Weekly => Some(date.weekday()) == rule.weekday,
            Frequency::Monthly => Some(date.day() as u8) == rule.day_of_month,
        };
        if is_candidate
            && let Some(naive) = day_at(date, rule.hour, rule.minute)
            && let Some(instant) = resolve_local(&tz, naive)
        {
            let millis = instant.timestamp_millis();
            if millis >= 0 && (millis as u64) > from_millis && (millis as u64) <= to_millis {
                out.push(millis as u64);
            }
        }
        date += Duration::days(1);
    }

    out.sort_unstable();
    out.dedup();
    out
}

/// Combines a local date with an `HH:MM` into a naive local datetime.
fn day_at(date: NaiveDate, hour: u8, minute: u8) -> Option<chrono::NaiveDateTime> {
    date.and_hms_opt(hour as u32, minute as u32, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recurring_raw() -> RawSchedule {
        RawSchedule {
            kind: Some("recurring".to_owned()),
            timezone: Some("America/New_York".to_owned()),
            frequency: Some("weekly".to_owned()),
            time_of_day: Some("20:00".to_owned()),
            weekday: Some("tuesday".to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn frequency_round_trips_and_rejects_unknown_and_miscased() {
        for (variant, token) in [
            (Frequency::Daily, "daily"),
            (Frequency::Weekly, "weekly"),
            (Frequency::Monthly, "monthly"),
        ] {
            assert_eq!(variant.as_str(), token);
            assert_eq!(Frequency::try_from(token), Ok(variant));
        }
        assert_eq!(Frequency::try_from("Daily"), Err(()), "case-sensitive");
        assert_eq!(Frequency::try_from("yearly"), Err(()));
    }

    #[test]
    fn parses_a_one_off_to_an_absolute_instant() {
        let schedule = parse_schedule(RawSchedule {
            kind: Some("one-off".to_owned()),
            timezone: Some("America/New_York".to_owned()),
            one_off_start_at: Some("2026-08-01T00:00:00Z".to_owned()),
            ..Default::default()
        })
        .expect("valid one-off");
        match schedule {
            Schedule::OneOff {
                start_at_millis,
                timezone,
            } => {
                // 2026-08-01T00:00:00Z == 1_785_542_400_000 ms.
                assert_eq!(start_at_millis, 1_785_542_400_000);
                assert_eq!(timezone, chrono_tz::America::New_York);
            }
            other => panic!("expected one-off, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_weekly_recurring_rule() {
        let schedule = parse_schedule(recurring_raw()).expect("valid weekly");
        match schedule {
            Schedule::Recurring(r) => {
                assert_eq!(r.frequency, Frequency::Weekly);
                assert_eq!(r.timezone, chrono_tz::America::New_York);
                assert_eq!(r.hour, 20);
                assert_eq!(r.minute, 0);
                assert_eq!(r.weekday, Some(Weekday::Tue));
                assert_eq!(r.day_of_month, None);
            }
            other => panic!("expected recurring, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_monthly_recurring_rule() {
        let schedule = parse_schedule(RawSchedule {
            kind: Some("recurring".to_owned()),
            timezone: Some("UTC".to_owned()),
            frequency: Some("monthly".to_owned()),
            time_of_day: Some("09:30".to_owned()),
            day_of_month: Some("15".to_owned()),
            ..Default::default()
        })
        .expect("valid monthly");
        match schedule {
            Schedule::Recurring(r) => {
                assert_eq!(r.frequency, Frequency::Monthly);
                assert_eq!(r.day_of_month, Some(15));
                assert_eq!(r.weekday, None);
                assert_eq!(r.hour, 9);
                assert_eq!(r.minute, 30);
            }
            other => panic!("expected recurring, got {other:?}"),
        }
    }

    #[test]
    fn rejects_a_pre_epoch_one_off_instant_instead_of_clamping_to_zero() {
        // A typo'd year (e.g. 1926 instead of 2026) must be rejected, not
        // silently reinterpreted as epoch 0 (1970-01-01).
        let err = parse_schedule(RawSchedule {
            kind: Some("one-off".to_owned()),
            timezone: Some("America/New_York".to_owned()),
            one_off_start_at: Some("1926-08-01T00:00:00Z".to_owned()),
            ..Default::default()
        })
        .expect_err("pre-epoch instant rejects");
        assert_eq!(err, ScheduleError::StartInvalid);
    }

    #[test]
    fn rejects_unknown_timezone() {
        let err = parse_schedule(RawSchedule {
            timezone: Some("Mars/Olympus_Mons".to_owned()),
            ..recurring_raw()
        })
        .expect_err("unknown tz rejects");
        assert_eq!(err, ScheduleError::TimezoneUnknown);
        assert!(err.to_string().starts_with("timezone:"));
    }

    #[test]
    fn rejects_unknown_frequency() {
        let err = parse_schedule(RawSchedule {
            frequency: Some("hourly".to_owned()),
            ..recurring_raw()
        })
        .expect_err("unknown frequency rejects");
        assert_eq!(err, ScheduleError::FrequencyUnknown);
    }

    #[test]
    fn rejects_malformed_time_of_day() {
        for bad in ["8:00", "20:60", "24:00", "2000", "abc", "20:0"] {
            let err = parse_schedule(RawSchedule {
                time_of_day: Some(bad.to_owned()),
                ..recurring_raw()
            })
            .expect_err("malformed time rejects");
            assert_eq!(err, ScheduleError::TimeOfDayInvalid, "input {bad}");
        }
    }

    #[test]
    fn weekly_without_weekday_is_rejected() {
        let err = parse_schedule(RawSchedule {
            weekday: None,
            ..recurring_raw()
        })
        .expect_err("weekly needs a weekday");
        assert_eq!(err, ScheduleError::WeekdayRequired);
    }

    #[test]
    fn monthly_without_or_with_invalid_day_of_month_is_rejected() {
        let missing = parse_schedule(RawSchedule {
            kind: Some("recurring".to_owned()),
            timezone: Some("UTC".to_owned()),
            frequency: Some("monthly".to_owned()),
            time_of_day: Some("09:30".to_owned()),
            day_of_month: None,
            ..Default::default()
        })
        .expect_err("monthly needs a day-of-month");
        assert_eq!(missing, ScheduleError::DayOfMonthRequired);

        for bad in ["0", "32", "-1", "abc"] {
            let err = parse_schedule(RawSchedule {
                kind: Some("recurring".to_owned()),
                timezone: Some("UTC".to_owned()),
                frequency: Some("monthly".to_owned()),
                time_of_day: Some("09:30".to_owned()),
                day_of_month: Some(bad.to_owned()),
                ..Default::default()
            })
            .expect_err("invalid day-of-month rejects");
            assert_eq!(err, ScheduleError::DayOfMonthInvalid, "input {bad}");
        }
    }

    #[test]
    fn two_different_bad_fields_yield_two_different_details() {
        // The distinct-detail invariant: field name leads, so distinct
        // fields carry distinct problem `detail` text.
        let bad_tz = parse_schedule(RawSchedule {
            timezone: Some("nope".to_owned()),
            ..recurring_raw()
        })
        .expect_err("bad tz");
        let bad_freq = parse_schedule(RawSchedule {
            frequency: Some("hourly".to_owned()),
            ..recurring_raw()
        })
        .expect_err("bad frequency");
        assert_ne!(bad_tz.to_string(), bad_freq.to_string());
        assert!(bad_tz.to_string().starts_with("timezone:"));
        assert!(bad_freq.to_string().starts_with("frequency:"));
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let err = parse_schedule(RawSchedule {
            kind: Some("cron".to_owned()),
            ..recurring_raw()
        })
        .expect_err("unknown kind rejects");
        assert_eq!(err, ScheduleError::KindUnknown);
    }

    /// Epoch millis for an RFC 3339 UTC string — independent ground truth for
    /// the DST vectors (computed from the wire string, not via the tz logic
    /// under test).
    fn ms(rfc3339: &str) -> u64 {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .expect("valid rfc3339")
            .timestamp_millis() as u64
    }

    fn ny() -> Tz {
        chrono_tz::America::New_York
    }

    fn weekly(tz: Tz, weekday: Weekday, hour: u8, minute: u8) -> Schedule {
        Schedule::Recurring(RecurringSchedule {
            frequency: Frequency::Weekly,
            timezone: tz,
            hour,
            minute,
            weekday: Some(weekday),
            day_of_month: None,
        })
    }

    fn daily(tz: Tz, hour: u8, minute: u8) -> Schedule {
        Schedule::Recurring(RecurringSchedule {
            frequency: Frequency::Daily,
            timezone: tz,
            hour,
            minute,
            weekday: None,
            day_of_month: None,
        })
    }

    fn monthly(tz: Tz, day_of_month: u8, hour: u8, minute: u8) -> Schedule {
        Schedule::Recurring(RecurringSchedule {
            frequency: Frequency::Monthly,
            timezone: tz,
            hour,
            minute,
            weekday: None,
            day_of_month: Some(day_of_month),
        })
    }

    #[test]
    fn one_off_appears_only_when_inside_the_window() {
        let start = ms("2026-08-01T00:00:00Z");
        let schedule = Schedule::OneOff {
            start_at_millis: start,
            timezone: ny(),
        };
        // In window (half-open, from-exclusive to-inclusive).
        assert_eq!(
            occurrences_between(&schedule, start - 1, start + 1),
            vec![start]
        );
        // The `from` bound is exclusive.
        assert_eq!(
            occurrences_between(&schedule, start, start + 10),
            Vec::<u64>::new()
        );
        // The `to` bound is inclusive.
        assert_eq!(
            occurrences_between(&schedule, start - 10, start),
            vec![start]
        );
        // Entirely before / after the window.
        assert_eq!(
            occurrences_between(&schedule, start + 1, start + 100),
            Vec::<u64>::new()
        );
    }

    #[test]
    fn weekly_new_york_is_dst_correct_across_spring_forward() {
        // US spring-forward 2026 is Sunday March 8. A "Tuesday 20:00
        // America/New_York" net lands at 20:00 LOCAL each week — so its UTC
        // hour shifts across the boundary: Mar 3 is EST (UTC-5), Mar 10 is EDT
        // (UTC-4). The expected instants are computed by-hand from the offset,
        // NOT from the tz code under test.
        let schedule = weekly(ny(), Weekday::Tue, 20, 0);
        let from = ms("2026-03-01T00:00:00Z");
        let to = ms("2026-03-15T00:00:00Z");
        let expected = vec![
            ms("2026-03-04T01:00:00Z"), // Tue Mar 3 20:00 EST
            ms("2026-03-11T00:00:00Z"), // Tue Mar 10 20:00 EDT (UTC hour shifted)
        ];
        assert_eq!(occurrences_between(&schedule, from, to), expected);
    }

    #[test]
    fn daily_utc_window_is_exact_and_ascending() {
        let schedule = daily(chrono_tz::UTC, 12, 0);
        let from = ms("2026-06-01T00:00:00Z");
        let to = ms("2026-06-03T23:59:59Z");
        assert_eq!(
            occurrences_between(&schedule, from, to),
            vec![
                ms("2026-06-01T12:00:00Z"),
                ms("2026-06-02T12:00:00Z"),
                ms("2026-06-03T12:00:00Z"),
            ]
        );
    }

    #[test]
    fn monthly_day_fifteen_utc() {
        let schedule = monthly(chrono_tz::UTC, 15, 9, 30);
        let from = ms("2026-01-01T00:00:00Z");
        let to = ms("2026-03-31T23:59:59Z");
        assert_eq!(
            occurrences_between(&schedule, from, to),
            vec![
                ms("2026-01-15T09:30:00Z"),
                ms("2026-02-15T09:30:00Z"),
                ms("2026-03-15T09:30:00Z"),
            ]
        );
    }

    #[test]
    fn monthly_day_thirtyone_skips_short_months() {
        // Policy: a day-of-month that does not exist in a given month SKIPS
        // that month (does not roll into the next). Jan/Mar have a 31st; Feb
        // (28 days in 2026) does not.
        let schedule = monthly(chrono_tz::UTC, 31, 0, 0);
        let from = ms("2026-01-01T00:00:00Z");
        let to = ms("2026-04-01T00:00:00Z");
        assert_eq!(
            occurrences_between(&schedule, from, to),
            vec![
                ms("2026-01-31T00:00:00Z"),
                // no February — skipped, not rolled to Mar 1
                ms("2026-03-31T00:00:00Z"),
            ]
        );
    }

    #[test]
    fn ambiguous_fall_back_local_time_uses_the_earliest_instant() {
        // US fall-back 2026 is Sunday November 1: 02:00 EDT → 01:00 EST, so
        // 01:30 local occurs twice. Policy: EARLIEST (pre-transition, EDT).
        // 01:30 EDT (UTC-4) == 05:30 UTC; the later 01:30 EST would be 06:30.
        let schedule = daily(ny(), 1, 30);
        let from = ms("2026-11-01T00:00:00Z");
        let to = ms("2026-11-01T12:00:00Z");
        assert_eq!(
            occurrences_between(&schedule, from, to),
            vec![ms("2026-11-01T05:30:00Z")],
        );
    }

    #[test]
    fn nonexistent_spring_forward_local_time_uses_the_next_valid_instant() {
        // On Mar 8 2026 the wall clock jumps 02:00 → 03:00 EST→EDT, so 02:30
        // does not exist. Policy: the NEXT valid instant, i.e. 03:00 EDT
        // (UTC-4) == 07:00 UTC.
        let schedule = daily(ny(), 2, 30);
        let from = ms("2026-03-08T00:00:00Z");
        let to = ms("2026-03-08T12:00:00Z");
        assert_eq!(
            occurrences_between(&schedule, from, to),
            vec![ms("2026-03-08T07:00:00Z")],
        );
    }

    #[test]
    fn generation_is_deterministic_and_idempotent() {
        let schedule = weekly(ny(), Weekday::Tue, 20, 0);
        let from = ms("2026-01-01T00:00:00Z");
        let to = ms("2026-04-01T00:00:00Z");
        let first = occurrences_between(&schedule, from, to);
        let second = occurrences_between(&schedule, from, to);
        assert_eq!(
            first, second,
            "pure & deterministic — the ON CONFLICT premise"
        );
        assert!(!first.is_empty());
    }
}

#[cfg(test)]
mod proptest_generation {
    use super::*;
    use chrono::Timelike;
    use proptest::prelude::*;

    /// Strategy over a valid recurring schedule in UTC (no DST gaps/overlaps,
    /// so the local-HH:MM invariant holds exactly; DST edges are pinned by the
    /// explicit vectors) and a bounded window.
    fn recurring_and_window() -> impl Strategy<Value = (Schedule, u64, u64)> {
        // Base instant: 2026-01-01T00:00:00Z in millis.
        const BASE: u64 = 1_767_225_600_000;
        const DAY: u64 = 86_400_000;
        (
            0u8..3,    // frequency selector
            0u8..24,   // hour
            0u8..60,   // minute
            0u8..7,    // weekday
            1u8..=28,  // day-of-month (28 exists in every month)
            0u64..90,  // window start offset (days)
            1u64..120, // window length (days)
        )
            .prop_map(|(f, hour, minute, wd, dom, start_off, len)| {
                let frequency = match f {
                    0 => Frequency::Daily,
                    1 => Frequency::Weekly,
                    _ => Frequency::Monthly,
                };
                let weekday = matches!(frequency, Frequency::Weekly)
                    .then(|| Weekday::try_from(wd).expect("0..7 is a valid weekday"));
                let day_of_month = matches!(frequency, Frequency::Monthly).then_some(dom);
                let schedule = Schedule::Recurring(RecurringSchedule {
                    frequency,
                    timezone: chrono_tz::UTC,
                    hour,
                    minute,
                    weekday,
                    day_of_month,
                });
                let from = BASE + start_off * DAY;
                let to = from + len * DAY;
                (schedule, from, to)
            })
    }

    proptest! {
        #[test]
        fn every_instant_matches_the_rule_and_the_vector_is_strictly_ascending(
            (schedule, from, to) in recurring_and_window()
        ) {
            let out = occurrences_between(&schedule, from, to);
            let Schedule::Recurring(rule) = schedule else { unreachable!() };

            // Strictly ascending, no duplicates.
            for pair in out.windows(2) {
                prop_assert!(pair[0] < pair[1], "strictly ascending, no dupes");
            }

            for &millis in &out {
                // Inside the half-open window.
                prop_assert!(millis > from && millis <= to);
                let local = chrono::DateTime::from_timestamp_millis(millis as i64)
                    .unwrap()
                    .with_timezone(&rule.timezone);
                // Local HH:MM equals the rule (exact in UTC — no DST shift).
                prop_assert_eq!(local.hour() as u8, rule.hour);
                prop_assert_eq!(local.minute() as u8, rule.minute);
                prop_assert_eq!(local.second(), 0);
                // Frequency's day filter.
                match rule.frequency {
                    Frequency::Daily => {}
                    Frequency::Weekly => {
                        prop_assert_eq!(local.weekday(), rule.weekday.unwrap());
                    }
                    Frequency::Monthly => {
                        prop_assert_eq!(local.day() as u8, rule.day_of_month.unwrap());
                    }
                }
            }
        }
    }
}
