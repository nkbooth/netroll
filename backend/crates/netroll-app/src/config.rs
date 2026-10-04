// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
use std::path::{Path, PathBuf};

use netroll_adapters::crypto::InstanceKek;
use netroll_adapters::mail::{SmtpConfig, is_deliverable_address, is_valid_sender};
use netroll_domain::auth::normalize_email;
use thiserror::Error;

use crate::csp::{HostSource, HttpScheme, Policy, is_directive_name};

/// Startup configuration errors — each aborts boot with a diagnostic instead
/// of silently falling back to a default.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// `PORT` was set but is not a usable TCP port (1–65535).
    #[error("PORT must be a number between 1 and 65535, got {0:?}")]
    InvalidPort(String),
    /// `STATIC_DIR` was set to an empty or whitespace-only value.
    #[error("STATIC_DIR is set but empty")]
    EmptyStaticDir,
    /// `AVATAR_DIR` was set to an empty or whitespace-only value.
    #[error("AVATAR_DIR is set but empty")]
    EmptyAvatarDir,
    /// A hard boot error rather than a per-request 500 later: an operator who
    /// mounted the volume wrong should learn at startup, not from a failed upload.
    #[error("AVATAR_DIR {0:?} is not usable: {1}")]
    AvatarDirUnusable(PathBuf, String),
    /// Unset or empty. There is no sane fallback for a connection string.
    #[error("DATABASE_URL must be set to a Postgres connection string")]
    MissingDatabaseUrl,
    /// Unset or empty. A localhost fallback would silently email dead links
    /// from a production deploy.
    #[error(
        "PUBLIC_BASE_URL must be set to the public origin emailed links point at (e.g. https://netroll.example)"
    )]
    MissingPublicBaseUrl,
    /// Not an `https://` origin, nor an `http://` one on a loopback host.
    ///
    /// It seeds emailed links AND the WebSocket origin of the derived
    /// Content-Security-Policy: an authority-less value would boot, name no
    /// WebSocket origin, and have every live-session socket refused in the
    /// browser with no server-side error; plain `http` on a real host would
    /// derive a `ws://` origin no `https://` page may open. Echoes the value —
    /// an origin is not a secret.
    #[error(
        "PUBLIC_BASE_URL must be an https:// origin (http:// only on localhost, 127.0.0.1 or [::1]), got {0:?}"
    )]
    InvalidPublicBaseUrl(String),
    /// `SMTP_HOST` was set to an empty or whitespace-only value.
    #[error("SMTP_HOST is set but empty")]
    EmptySmtpHost,
    /// `SMTP_PORT` was set but is not a usable TCP port (1–65535).
    #[error("SMTP_PORT must be a number between 1 and 65535, got {0:?}")]
    InvalidSmtpPort(String),
    /// Unset or empty. A placeholder fallback sender would silently mail every
    /// user from a domain that cannot receive replies.
    #[error(
        "MAIL_FROM must be set to the sender address outgoing email goes out as (e.g. \"NetRoll by N1CCK <no-reply@netroll.example>\")"
    )]
    MissingMailFrom,
    /// `MAIL_FROM` was set to something the mailer cannot send as.
    #[error("MAIL_FROM must be one address, bare or as \"Display Name <user@domain>\", got {0:?}")]
    InvalidMailFrom(String),
    /// The resolved static dir is missing or does not contain `index.html`.
    #[error(
        "static dir {0:?} does not exist or has no index.html — build the frontend or set STATIC_DIR"
    )]
    StaticDirInvalid(PathBuf),
    /// Set but not valid base64 of exactly 32 bytes. A config mistake must
    /// surface at boot, not at the first credential write; an ABSENT KEK is
    /// fine, see [`resolve_kek`]. Fixed message: never echo a secret.
    #[error(
        "KEK must be base64-encoded 32 bytes for AES-256 (mint one with `openssl rand -base64 32`)"
    )]
    InvalidKek,
    /// Set but not a positive integer. A malformed cap is a hard boot error,
    /// never a silent fallback to the default.
    #[error("MAX_NETS_PER_USER must be a positive integer, got {0:?}")]
    InvalidMaxNetsPerUser(String),
    /// Set but not a positive integer. Same fail-loud posture as
    /// [`ConfigError::InvalidMaxNetsPerUser`].
    #[error("MAX_OWNERS_PER_NET must be a positive integer, got {0:?}")]
    InvalidMaxOwnersPerNet(String),
    /// Set but not a positive integer that fits a `u32`. Load-bearing: a `0`
    /// would silently mute ALL magic-link mail, and magic links are this
    /// instance's only authentication path.
    #[error("MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR must be a positive integer, got {0:?}")]
    InvalidMagicLinkAggregateSendsPerHour(String),
    /// Set but not a positive integer that fits a `u32`. A `0` would mute the
    /// reserve that keeps returning users signing in while a campaign has the
    /// general pool saturated.
    #[error("MAGIC_LINK_RESERVE_SENDS_PER_HOUR must be a positive integer, got {0:?}")]
    InvalidMagicLinkReserveSendsPerHour(String),
    /// Set but not a positive integer of days no larger than
    /// [`MAX_TOKEN_RETENTION_DAYS`]. A `0` would collapse the cutoff onto `now`,
    /// leaving no history to correlate a sign-in against the audit log. The
    /// upper bound exists because the days-to-millis conversion multiplies and
    /// release builds run with `overflow-checks = false`, so an absurd value
    /// would wrap silently into an arbitrary, shorter window.
    #[error(
        "TOKEN_RETENTION_DAYS must be a positive integer of at most {MAX_TOKEN_RETENTION_DAYS} days, got {0:?}"
    )]
    InvalidTokenRetentionDays(String),
    /// Set but shorter than [`MIN_BOT_MITIGATION_SECRET_BYTES`]. A short secret
    /// is brute-forceable from the `(payload, signature)` pairs anyone can
    /// collect off the public, ungated `GET /api/form-tokens` endpoint. A
    /// MISSING secret disables mitigation, which is safe; a WEAK one defeats it
    /// while looking configured, which is not.
    #[error(
        "BOT_MITIGATION_SECRET must be at least {MIN_BOT_MITIGATION_SECRET_BYTES} bytes (mint one with `openssl rand -base64 32`)"
    )]
    WeakBotMitigationSecret,
    /// A non-empty list with a blank or non-address entry. An entirely unset
    /// value means "no admins configured" and is not an error; a list the
    /// operator clearly intended, with a bad entry, must not silently grant or
    /// deny the wrong set. Echoes the offending entry, which is not a secret.
    #[error("ADMIN_ACCOUNT_EMAILS has a malformed or empty entry: {0:?}")]
    InvalidAdminAllowlist(String),
    /// Set but blank. An operator who sets it blank meant to set SOMETHING, so
    /// this is a config mistake rather than "use the derived default".
    #[error("CSP_POLICY is set but empty")]
    EmptyCspPolicy,
    /// Carries a CR, LF, or a byte outside visible ASCII. Reports a REASON
    /// rather than the value: a CR/LF-bearing value is header-injection-shaped
    /// and the boot log is not the place to reprint it, so that case reports
    /// only the length.
    #[error("CSP_POLICY is not a valid HTTP header value: {0}")]
    InvalidCspPolicy(String),
    /// Header-safe, but no clause names a CSP directive. A browser discards
    /// every unknown directive, so such a value enforces NOTHING while looking
    /// configured. Not a quality check: one recognized directive is enough, so
    /// a policy of only `frame-ancestors 'none'` stays legal.
    #[error("CSP_POLICY names no CSP directive, so a browser would enforce nothing: {0:?}")]
    UnrecognizedCspPolicy(String),
    /// Something other than `true`/`false`. A boolean flag with a typo must
    /// never silently enforce OR silently disarm — either direction is a policy
    /// change the operator did not make.
    #[error("CSP_REPORT_ONLY must be `true` or `false`, got {0:?}")]
    InvalidCspReportOnly(String),
    /// Not an http(s) URL whose authority can stand as a CSP host-source. The
    /// value is interpolated into the derived `connect-src`, so one carrying
    /// `;`, `,`, whitespace or a byte no host may hold could split or extend
    /// the policy — the one [`AppConfig`] setting whose breakage reaches past
    /// the operator's own analytics. Blank stays "off"; only a present,
    /// unreducible value is refused.
    #[error(
        "PLAUSIBLE_SCRIPT_HOST must be an http(s) URL on a plain host (e.g. https://analytics.example), got {0:?}"
    )]
    InvalidPlausibleScriptHost(String),
}

/// Resolves the listen port from the raw `PORT` env value.
///
/// Unset falls back to 3000; a set-but-invalid value (non-numeric,
/// out of range, or 0 — which would bind an OS-assigned ephemeral port)
/// is a hard error rather than a silent fallback.
pub fn resolve_port(raw: Option<String>) -> Result<u16, ConfigError> {
    match raw {
        None => Ok(3000),
        Some(value) => match value.trim().parse::<u16>() {
            Ok(port) if port != 0 => Ok(port),
            _ => Err(ConfigError::InvalidPort(value)),
        },
    }
}

/// Resolves the SPA bundle directory from the raw `STATIC_DIR` env value.
///
/// Unset falls back to `frontend/dist`; a set-but-empty value is a hard
/// error rather than being treated as a valid path.
pub fn resolve_static_dir(raw: Option<String>) -> Result<PathBuf, ConfigError> {
    match raw {
        None => Ok(PathBuf::from("frontend/dist")),
        Some(value) if value.trim().is_empty() => Err(ConfigError::EmptyStaticDir),
        Some(value) => Ok(PathBuf::from(value)),
    }
}

/// Resolves the uploaded-avatar directory from `AVATAR_DIR`.
///
/// Unset falls back to `data/avatars` so uploads work out of the box in a single
/// container; a set-but-empty value is a hard error rather than a valid path.
pub fn resolve_avatar_dir(raw: Option<String>) -> Result<PathBuf, ConfigError> {
    match raw {
        None => Ok(PathBuf::from("data/avatars")),
        Some(value) if value.trim().is_empty() => Err(ConfigError::EmptyAvatarDir),
        Some(value) => Ok(PathBuf::from(value.trim())),
    }
}

/// Creates the avatar directory if absent and proves it is writable, so a
/// misconfigured volume fails at boot instead of on a user's first upload.
pub fn prepare_avatar_dir(dir: &Path) -> Result<(), ConfigError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| ConfigError::AvatarDirUnusable(dir.to_path_buf(), e.to_string()))?;
    // create_dir_all succeeds on an existing read-only directory, so writability
    // is probed explicitly with a file that is removed immediately.
    let probe = dir.join(".write-probe");
    std::fs::write(&probe, b"")
        .map_err(|e| ConfigError::AvatarDirUnusable(dir.to_path_buf(), e.to_string()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Resolves the Postgres connection string from `DATABASE_URL`.
///
/// Required: the app cannot boot without a database and must not guess at one.
pub fn resolve_database_url(raw: Option<String>) -> Result<String, ConfigError> {
    match raw {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(ConfigError::MissingDatabaseUrl),
    }
}

/// Resolves the public origin emailed links point at, from `PUBLIC_BASE_URL`.
///
/// Required: a silent dev-URL fallback would break every emailed link in
/// production with no signal. A set value must be an `https://` origin, or an
/// `http://` one on a loopback host. The derived Content-Security-Policy names
/// the WebSocket origin from it through the same [`HostSource::parse`], so what
/// boots here is exactly what the policy can derive. A trailing slash is
/// stripped so link construction can always join with `/path`.
pub fn resolve_public_base_url(raw: Option<String>) -> Result<String, ConfigError> {
    let trimmed = match raw.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => value,
        _ => return Err(ConfigError::MissingPublicBaseUrl),
    };
    let origin = HostSource::parse(trimmed)
        .ok_or_else(|| ConfigError::InvalidPublicBaseUrl(trimmed.to_owned()))?;
    if origin.scheme() == HttpScheme::Http && !origin.is_loopback() {
        return Err(ConfigError::InvalidPublicBaseUrl(trimmed.to_owned()));
    }
    Ok(trimmed.trim_end_matches('/').to_owned())
}

/// Resolves SMTP settings from `SMTP_*` and `MAIL_FROM`.
///
/// Connection defaults target the compose mail catcher (`localhost:1025`, no
/// auth); an empty username selects the unauthenticated dev transport.
/// `MAIL_FROM` has no default.
pub fn resolve_smtp(
    host: Option<String>,
    port: Option<String>,
    username: Option<String>,
    password: Option<String>,
    from: Option<String>,
) -> Result<SmtpConfig, ConfigError> {
    let host = match host {
        None => "localhost".to_owned(),
        Some(value) if value.trim().is_empty() => return Err(ConfigError::EmptySmtpHost),
        Some(value) => value,
    };
    let port = match port {
        None => 1025,
        Some(value) => match value.trim().parse::<u16>() {
            Ok(port) if port != 0 => port,
            _ => return Err(ConfigError::InvalidSmtpPort(value)),
        },
    };
    let from = match from {
        Some(value) if !value.trim().is_empty() => value.trim().to_owned(),
        _ => return Err(ConfigError::MissingMailFrom),
    };
    if !is_valid_sender(&from) {
        return Err(ConfigError::InvalidMailFrom(from));
    }
    Ok(SmtpConfig {
        host,
        port,
        username: username.unwrap_or_default(),
        password: password.unwrap_or_default(),
        from,
    })
}

/// Resolves the instance key-encryption key from `KEK`.
///
/// Optional at boot: absent resolves to `Ok(None)`, the app boots, and the
/// credential cipher fails closed so QRZ endpoints return 503. A PRESENT value
/// must be valid base64 of exactly 32 bytes, or boot fails loudly.
pub fn resolve_kek(raw: Option<String>) -> Result<Option<InstanceKek>, ConfigError> {
    match raw {
        Some(value) if !value.trim().is_empty() => {
            // Scrub the raw base64 text immediately: a plain `String` would sit
            // un-scrubbed until Rust drops it, unlike the `Zeroizing` types the
            // rest of the KEK path uses.
            let scrubbed = zeroize::Zeroizing::new(value);
            InstanceKek::from_base64(scrubbed.trim())
                .map(Some)
                .map_err(|_| ConfigError::InvalidKek)
        }
        _ => Ok(None),
    }
}

/// Default per-account cap on active (non-archived) owned nets. Set from
/// observed usage: operators run a handful of recurring nets, so this bounds
/// spam without constraining legitimate owners.
const DEFAULT_MAX_NETS_PER_USER: usize = 7;

/// Default per-net cap on owners. Set from observed real-world
/// usage: club nets share ownership across a small committee, not a whole roster.
const DEFAULT_MAX_OWNERS_PER_NET: usize = 5;

/// Parses a resource-cap value: a positive integer, ignoring surrounding
/// whitespace. Rejects 0, negatives, non-numeric, empty, and overflow (all of
/// which `usize::from_str` or the `>= 1` guard catch) as `None`.
fn parse_cap(value: &str) -> Option<usize> {
    match value.trim().parse::<usize>() {
        Ok(n) if n >= 1 => Some(n),
        _ => None,
    }
}

/// Resolves the max-nets-per-user cap from `MAX_NETS_PER_USER`.
///
/// Unset falls back to [`DEFAULT_MAX_NETS_PER_USER`]; a set-but-invalid value
/// is a hard boot error rather than a silent fallback.
pub fn resolve_max_nets_per_user(raw: Option<String>) -> Result<usize, ConfigError> {
    match raw {
        None => Ok(DEFAULT_MAX_NETS_PER_USER),
        Some(value) => parse_cap(&value).ok_or(ConfigError::InvalidMaxNetsPerUser(value)),
    }
}

/// Resolves the max-owners-per-net cap from `MAX_OWNERS_PER_NET`. Same
/// fail-loud posture as [`resolve_max_nets_per_user`].
pub fn resolve_max_owners_per_net(raw: Option<String>) -> Result<usize, ConfigError> {
    match raw {
        None => Ok(DEFAULT_MAX_OWNERS_PER_NET),
        Some(value) => parse_cap(&value).ok_or(ConfigError::InvalidMaxOwnersPerNet(value)),
    }
}

/// Default instance-wide magic-link send budget, in emails per hour. Real
/// signup and sign-in volume is a handful of sends per day, so 60/hour sits
/// orders of magnitude above legitimate traffic while bounding an
/// email-bombing campaign to a volume a relay's reputation survives.
const DEFAULT_MAGIC_LINK_SENDS_PER_HOUR: u32 = 60;

/// Resolves the instance-wide magic-link send cap from
/// `MAGIC_LINK_AGGREGATE_SENDS_PER_HOUR`.
///
/// Unset falls back to [`DEFAULT_MAGIC_LINK_SENDS_PER_HOUR`]; a set-but-invalid
/// value is a hard boot error rather than a silent fallback.
pub fn resolve_magic_link_aggregate_sends_per_hour(
    raw: Option<String>,
) -> Result<u32, ConfigError> {
    match raw {
        None => Ok(DEFAULT_MAGIC_LINK_SENDS_PER_HOUR),
        Some(value) => parse_cap(&value)
            .and_then(|sends| u32::try_from(sends).ok())
            .ok_or(ConfigError::InvalidMagicLinkAggregateSendsPerHour(value)),
    }
}

/// Default RESERVE magic-link send budget, in emails per hour. Drawn on ONLY
/// once the general [`DEFAULT_MAGIC_LINK_SENDS_PER_HOUR`] pool is exhausted,
/// and only by addresses that have proven control here, so it keeps returning
/// users signing in during a campaign against UNKNOWN addresses. A budget, not
/// a guarantee: an attacker who first proves control of ~10 addresses can drain
/// it within their per-address quotas, and raising this raises the guarantee and
/// the outbound ceiling together. Sized above the general pool because it serves
/// the population that must never be locked out, while keeping TOTAL outbound
/// mail bounded.
///
/// # Sizing the bound: burst plus sustained, not sustained alone
///
/// Each budget is a GCRA bucket whose burst equals its per-hour number and whose
/// replenish period is `3600s / number`, and `governor` hands a FRESH bucket its
/// full burst immediately. So a pool of N admits N at once and then replenishes
/// up to another N over the next hour: up to **2N in the first hour after boot**,
/// settling to N/hour sustained. On the defaults that is up to 120 general +
/// 240 reserve = **up to 360 sends in a fresh-boot hour**, against a sustained
/// 180/hour. A crash-looping process resets BOTH buckets on every boot, so the
/// figure that matters when sizing SMTP relay reputation is the burst one.
const DEFAULT_MAGIC_LINK_RESERVE_SENDS_PER_HOUR: u32 = 120;

/// Resolves the proven-control reserve budget from
/// `MAGIC_LINK_RESERVE_SENDS_PER_HOUR`.
///
/// Unset falls back to [`DEFAULT_MAGIC_LINK_RESERVE_SENDS_PER_HOUR`]; a
/// set-but-invalid value is a hard boot error rather than a silent fallback.
pub fn resolve_magic_link_reserve_sends_per_hour(raw: Option<String>) -> Result<u32, ConfigError> {
    match raw {
        None => Ok(DEFAULT_MAGIC_LINK_RESERVE_SENDS_PER_HOUR),
        Some(value) => parse_cap(&value)
            .and_then(|sends| u32::try_from(sends).ok())
            .ok_or(ConfigError::InvalidMagicLinkReserveSendsPerHour(value)),
    }
}

/// Default retention window for expired tokens and dead sessions, in days. ONE
/// window covers all three tables: they hold the same class of short-lived
/// credential artifact. Thirty days keeps enough history to correlate a sign-in
/// against the audit log while bounding growth hard.
const DEFAULT_TOKEN_RETENTION_DAYS: u64 = 30;

/// Upper bound on `TOKEN_RETENTION_DAYS`: a hundred years. Not a policy
/// opinion — any window past a human lifetime means "never prune" equally well
/// — but the days-to-millis conversion at the spawn site must not overflow.
/// `parse_cap` accepts any positive `usize`, and release builds run with
/// `overflow-checks` off, so an unbounded value would wrap into an arbitrary
/// SHORTER window with no error. Rejecting at the resolver keeps "invalid config
/// never boots" in one place rather than scattering `checked_mul` at consumers.
const MAX_TOKEN_RETENTION_DAYS: u64 = 36_500;

/// Milliseconds in a day. Named so the days→millis conversion reads the same at
/// the bound check and at the spawn site.
pub const MILLIS_PER_DAY: u64 = 24 * 60 * 60 * 1_000;

/// Resolves the token/session retention window from `TOKEN_RETENTION_DAYS`.
///
/// Unset falls back to [`DEFAULT_TOKEN_RETENTION_DAYS`]; a set-but-invalid value,
/// including one above [`MAX_TOKEN_RETENTION_DAYS`], is a hard boot error.
pub fn resolve_token_retention_days(raw: Option<String>) -> Result<u64, ConfigError> {
    match raw {
        None => Ok(DEFAULT_TOKEN_RETENTION_DAYS),
        Some(value) => parse_cap(&value)
            .and_then(|days| u64::try_from(days).ok())
            .filter(|days| *days <= MAX_TOKEN_RETENTION_DAYS)
            .ok_or(ConfigError::InvalidTokenRetentionDays(value)),
    }
}

/// Minimum byte length for a present `BOT_MITIGATION_SECRET`. The secret keys
/// an HMAC verifiable offline once an attacker holds even one
/// `(payload, signature)` pair from the public, ungated `GET /api/form-tokens`
/// endpoint, so a guessable secret silently defeats mitigation while LOOKING
/// configured. 128 bits is a permissive floor, well below the documented
/// 32-byte recommendation but enough to rule out placeholder values.
const MIN_BOT_MITIGATION_SECRET_BYTES: usize = 16;

/// Resolves the bot-mitigation HMAC signing secret from `BOT_MITIGATION_SECRET`.
///
/// PRESENCE is the on/off switch: absent disables mitigation and the signup and
/// net-creation endpoints behave as before. A present value must be at least
/// [`MIN_BOT_MITIGATION_SECRET_BYTES`] long, because a DISABLED control is safe
/// and a WEAK one is not. Wrapped so the bytes are zeroed on drop.
pub fn resolve_bot_mitigation_secret(
    raw: Option<String>,
) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, ConfigError> {
    match raw {
        Some(value) if !value.trim().is_empty() => {
            let trimmed = value.trim();
            if trimmed.len() < MIN_BOT_MITIGATION_SECRET_BYTES {
                return Err(ConfigError::WeakBotMitigationSecret);
            }
            Ok(Some(zeroize::Zeroizing::new(trimmed.as_bytes().to_vec())))
        }
        _ => Ok(None),
    }
}

/// Resolves the platform-admin email allowlist from `ADMIN_ACCOUNT_EMAILS`, a
/// comma-separated list.
///
/// Optional: absent resolves to an empty `Vec`, meaning "no admins configured",
/// and `is_admin` then denies everyone. A PRESENT, non-empty value containing a
/// blank or non-address entry is a hard boot error rather than a silent partial
/// parse.
///
/// Each entry is normalized through the SAME [`normalize_email`] rule the
/// account model stores and the sign-in path uses — never a second
/// normalization contract — so `is_admin` can compare on the stored form.
pub fn resolve_admin_allowlist(raw: Option<String>) -> Result<Vec<String>, ConfigError> {
    let Some(value) = raw else {
        return Ok(Vec::new());
    };
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut admins = Vec::new();
    for entry in value.split(',') {
        let normalized = normalize_email(entry);
        if normalized.is_empty() || !is_deliverable_address(&normalized) {
            return Err(ConfigError::InvalidAdminAllowlist(entry.to_owned()));
        }
        admins.push(normalized);
    }
    Ok(admins)
}

/// Resolves the whole-policy override from `CSP_POLICY`.
///
/// Unset resolves to `Ok(None)` and the caller derives the default policy. A set
/// value REPLACES the policy verbatim: no merging, no derivation, the operator
/// owns it. Blank, header-unsafe, and directive-less values are all hard boot
/// errors, never a silent fallback.
pub fn resolve_csp_policy(raw: Option<String>) -> Result<Option<String>, ConfigError> {
    let Some(value) = raw else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::EmptyCspPolicy);
    }
    if trimmed.bytes().any(|byte| byte == b'\r' || byte == b'\n') {
        // Header-injection-shaped: describe it, never reprint it.
        return Err(ConfigError::InvalidCspPolicy(format!(
            "contains a CR or LF byte ({} bytes; value not echoed)",
            trimmed.len()
        )));
    }
    if let Some(byte) = trimmed.bytes().find(|byte| !(0x20..=0x7e).contains(byte)) {
        return Err(ConfigError::InvalidCspPolicy(format!(
            "byte 0x{byte:02x} is outside visible ASCII in {trimmed:?}"
        )));
    }
    // A browser discards every clause whose name it does not know, so a value
    // naming none enforces nothing while looking configured. One recognized
    // name is enough; this is not a judgement on the policy's quality.
    if !Policy::parse(trimmed)
        .directive_names()
        .any(is_directive_name)
    {
        return Err(ConfigError::UnrecognizedCspPolicy(trimmed.to_owned()));
    }
    Ok(Some(trimmed.to_owned()))
}

/// Resolves the report-only switch from `CSP_REPORT_ONLY`.
///
/// Unset resolves to `false` (enforce). `true`/`false` are the only legal
/// values; a misspelt flag that silently enforced, or silently disarmed, would
/// be a policy change the operator never made.
pub fn resolve_csp_report_only(raw: Option<String>) -> Result<bool, ConfigError> {
    let Some(value) = raw else {
        return Ok(false);
    };
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("true") {
        Ok(true)
    } else if trimmed.eq_ignore_ascii_case("false") {
        Ok(false)
    } else {
        Err(ConfigError::InvalidCspReportOnly(value))
    }
}

/// The public, client-visible instance settings served by `GET
/// /api/app-config`: optional third-party integrations an operator opts into.
///
/// Everything here is PUBLIC by construction — it ships to any visitor,
/// account or not, so no secret may ever be added to this struct.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppConfig {
    /// The Plausible `data-domain` this instance reports as. `None` disables
    /// analytics entirely (the default): no script is served, nothing loads.
    pub plausible_domain: Option<String>,
    /// Script origin for a SELF-HOSTED Plausible. `None` with a domain set
    /// means Plausible's own cloud host.
    pub plausible_script_host: Option<String>,
    /// Ko-fi username behind the footer support link. `None` hides the link.
    pub kofi_username: Option<String>,
}

/// Trims an optional env value to `None` when unset or blank.
///
/// Absence is the disabled state for every integration below, so a compose file
/// with `PLAUSIBLE_DOMAIN=` must read as "off" rather than as a
/// configured-but-empty domain that would emit a broken script tag.
fn optional_setting(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Resolves the self-hosted Plausible origin from `PLAUSIBLE_SCRIPT_HOST`,
/// ahead of [`resolve_app_config`].
///
/// Absent resolves to `Ok(None)`. A present value must be an http(s) URL the
/// derived Content-Security-Policy can reduce to a host-source: it feeds
/// `connect-src`, so this is the one [`AppConfig`] setting a typo could turn
/// into something other than broken analytics. The value is kept verbatim, path
/// included, because the tracker appends `/api/event` and a sub-path install is
/// legitimate; only the policy reduces it to an origin.
pub fn resolve_plausible_script_host(raw: Option<String>) -> Result<Option<String>, ConfigError> {
    let Some(host) = optional_setting(raw) else {
        return Ok(None);
    };
    if HostSource::parse(&host).is_none() {
        return Err(ConfigError::InvalidPlausibleScriptHost(host));
    }
    Ok(Some(host))
}

/// Resolves the public instance settings from their raw env values.
///
/// No variant is an error: a typo'd domain or Ko-fi name can only break the
/// operator's own analytics or donation link. The script host is the exception,
/// because it also shapes the Content-Security-Policy, so `main` runs it
/// through [`resolve_plausible_script_host`] first.
pub fn resolve_app_config(
    plausible_domain: Option<String>,
    plausible_script_host: Option<String>,
    kofi_username: Option<String>,
) -> AppConfig {
    let plausible_domain = optional_setting(plausible_domain);
    AppConfig {
        // A script host without a domain has nothing to report as, so it is
        // ignored rather than half-configuring analytics.
        plausible_script_host: plausible_domain
            .as_ref()
            .and_then(|_| optional_setting(plausible_script_host)),
        plausible_domain,
        kofi_username: optional_setting(kofi_username),
    }
}

/// True when a configured `max_nets_per_user` sits at or above the net-creation
/// rate-limiter's burst, which under-throttles: a legitimate user filling their
/// full quota in one sitting is rate-limited before ever reaching the cap. Not
/// a hard boot error, because the burst is a compiled-in constant an operator
/// cannot fix from env alone; callers log a boot-time warning instead.
pub fn cap_may_be_throttled_by_rate_limiter(
    max_nets_per_user: usize,
    net_creation_burst: u32,
) -> bool {
    u64::try_from(max_nets_per_user).unwrap_or(u64::MAX) >= u64::from(net_creation_burst)
}

/// Verifies at startup that the static dir exists and contains `index.html`,
/// so a misconfigured deploy fails at boot instead of 404ing every route.
pub fn validate_static_dir(dir: &Path) -> Result<(), ConfigError> {
    if dir.is_dir() && dir.join("index.html").is_file() {
        Ok(())
    } else {
        Err(ConfigError::StaticDirInvalid(dir.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_defaults_to_3000_when_unset() {
        assert_eq!(resolve_port(None).unwrap(), 3000);
    }

    #[test]
    fn port_parses_a_valid_value_ignoring_surrounding_whitespace() {
        assert_eq!(resolve_port(Some("8080".into())).unwrap(), 8080);
        assert_eq!(resolve_port(Some(" 8080\n".into())).unwrap(), 8080);
    }

    #[test]
    fn port_rejects_non_numeric_out_of_range_zero_and_empty() {
        for bad in ["abc", "70000", "0", "", "80.0", "-1"] {
            assert!(
                matches!(
                    resolve_port(Some(bad.into())),
                    Err(ConfigError::InvalidPort(_))
                ),
                "expected InvalidPort for {bad:?}"
            );
        }
    }

    #[test]
    fn avatar_dir_defaults_when_unset_and_trims_a_provided_path() {
        assert_eq!(
            resolve_avatar_dir(None).unwrap(),
            PathBuf::from("data/avatars")
        );
        assert_eq!(
            resolve_avatar_dir(Some("  /srv/avatars \n".into())).unwrap(),
            PathBuf::from("/srv/avatars")
        );
    }

    #[test]
    fn avatar_dir_rejects_a_set_but_empty_value() {
        // A blank AVATAR_DIR is a config mistake, not "use the default": the
        // operator clearly meant to point it somewhere.
        assert!(matches!(
            resolve_avatar_dir(Some("   ".into())),
            Err(ConfigError::EmptyAvatarDir)
        ));
    }

    #[test]
    fn prepare_avatar_dir_creates_a_missing_directory_and_proves_it_writable() {
        let base = std::env::temp_dir().join(format!("netroll-avatar-{}", std::process::id()));
        let nested = base.join("nested/avatars");
        let _ = std::fs::remove_dir_all(&base);

        prepare_avatar_dir(&nested).expect("creates the directory tree");

        assert!(nested.is_dir());
        // The probe file must not be left behind for the static handler to serve.
        assert!(!nested.join(".write-probe").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn prepare_avatar_dir_fails_when_the_path_is_a_file() {
        // A volume mounted as a file (or a stray file at the path) would make
        // every upload fail later; catch it at boot.
        let file = std::env::temp_dir().join(format!("netroll-avatar-file-{}", std::process::id()));
        std::fs::write(&file, b"not a directory").expect("fixture written");

        assert!(matches!(
            prepare_avatar_dir(&file),
            Err(ConfigError::AvatarDirUnusable(_, _))
        ));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn app_config_is_entirely_off_when_nothing_is_configured() {
        // The default posture: no analytics script, no donation link.
        assert_eq!(resolve_app_config(None, None, None), AppConfig::default());
        // A blank value (compose `PLAUSIBLE_DOMAIN=`) reads as off, not as a
        // configured-but-empty domain that would emit a broken script tag.
        assert_eq!(
            resolve_app_config(Some("  ".into()), Some("".into()), Some(" ".into())),
            AppConfig::default()
        );
    }

    #[test]
    fn app_config_trims_and_carries_the_configured_integrations() {
        let config = resolve_app_config(
            Some(" netroll.radio ".into()),
            Some(" https://analytics.example ".into()),
            Some(" n1cck ".into()),
        );

        assert_eq!(config.plausible_domain.as_deref(), Some("netroll.radio"));
        assert_eq!(
            config.plausible_script_host.as_deref(),
            Some("https://analytics.example")
        );
        assert_eq!(config.kofi_username.as_deref(), Some("n1cck"));
    }

    #[test]
    fn app_config_ignores_a_script_host_with_no_domain_to_report_as() {
        // Half-configured analytics would load a script that reports nothing;
        // the domain is what makes Plausible work at all.
        let config = resolve_app_config(None, Some("https://analytics.example".into()), None);

        assert_eq!(config.plausible_domain, None);
        assert_eq!(config.plausible_script_host, None);
    }

    #[test]
    fn static_dir_defaults_when_unset_and_uses_the_provided_path() {
        assert_eq!(
            resolve_static_dir(None).unwrap(),
            PathBuf::from("frontend/dist")
        );
        assert_eq!(
            resolve_static_dir(Some("custom/bundle".into())).unwrap(),
            PathBuf::from("custom/bundle")
        );
    }

    #[test]
    fn static_dir_rejects_empty_and_whitespace_only_values() {
        for bad in ["", "   ", "\n"] {
            assert!(
                matches!(
                    resolve_static_dir(Some(bad.into())),
                    Err(ConfigError::EmptyStaticDir)
                ),
                "expected EmptyStaticDir for {bad:?}"
            );
        }
    }

    #[test]
    fn database_url_is_required_missing_and_empty_are_hard_errors() {
        for bad in [None, Some("".into()), Some("   \n".into())] {
            assert!(
                matches!(
                    resolve_database_url(bad.clone()),
                    Err(ConfigError::MissingDatabaseUrl)
                ),
                "expected MissingDatabaseUrl for {bad:?}"
            );
        }
    }

    #[test]
    fn database_url_passes_a_set_value_through_unchanged() {
        assert_eq!(
            resolve_database_url(Some("postgres://u:p@db:5432/netroll".into())).unwrap(),
            "postgres://u:p@db:5432/netroll"
        );
    }

    #[test]
    fn public_base_url_is_required_missing_and_empty_are_hard_errors() {
        // A silent localhost fallback would boot fine in prod and email
        // links that point at nobody's server — fail at boot instead.
        for bad in [None, Some("".into()), Some("  \n".into())] {
            assert!(
                matches!(
                    resolve_public_base_url(bad.clone()),
                    Err(ConfigError::MissingPublicBaseUrl)
                ),
                "expected MissingPublicBaseUrl for {bad:?}"
            );
        }
    }

    #[test]
    fn public_base_url_strips_the_trailing_slash() {
        assert_eq!(
            resolve_public_base_url(Some("https://netroll.example/".into())).unwrap(),
            "https://netroll.example"
        );
        assert_eq!(
            resolve_public_base_url(Some("http://localhost:5173".into())).unwrap(),
            "http://localhost:5173"
        );
    }

    #[test]
    fn public_base_url_rejects_anything_but_an_http_s_origin_with_a_host() {
        // Boot is the only place this can fail loud: the derived CSP names the
        // WebSocket origin from this value, and one it cannot reduce would ship
        // a policy under which every live-session socket is refused in the
        // browser with no server-side error at all.
        for bad in [
            "netroll.example",
            "ftp://netroll.example",
            "wss://netroll.example",
            "https://",
            "https:///nets",
            "https://netroll.example; frame-ancestors *",
        ] {
            assert!(
                matches!(
                    resolve_public_base_url(Some(bad.into())),
                    Err(ConfigError::InvalidPublicBaseUrl(_))
                ),
                "expected InvalidPublicBaseUrl for {bad:?}"
            );
        }
    }

    #[test]
    fn public_base_url_allows_plain_http_only_on_a_loopback_host() {
        // The dev default (.env.template, vite.config.ts) is http://localhost:5173
        // and must keep booting; an http origin on a REAL host would derive a
        // ws:// origin no https page may open.
        for dev in [
            "http://localhost:5173",
            "http://LOCALHOST",
            "http://127.0.0.1:3000",
            "http://[::1]:3000",
        ] {
            assert!(
                resolve_public_base_url(Some(dev.into())).is_ok(),
                "{dev:?} is loopback and must boot"
            );
        }
        for bad in [
            "http://netroll.example",
            "http://192.168.1.20:3000",
            "http://localhost.example",
        ] {
            assert!(
                matches!(
                    resolve_public_base_url(Some(bad.into())),
                    Err(ConfigError::InvalidPublicBaseUrl(_))
                ),
                "expected InvalidPublicBaseUrl for {bad:?}"
            );
        }
    }

    /// A valid `MAIL_FROM` for cases exercising the other SMTP inputs.
    fn mail_from() -> Option<String> {
        Some("NetRoll by N1CCK <no-reply@n1cck.radio>".into())
    }

    #[test]
    fn smtp_defaults_target_the_dev_mail_catcher() {
        let smtp = resolve_smtp(None, None, None, None, mail_from()).unwrap();
        assert_eq!(smtp.host, "localhost");
        assert_eq!(smtp.port, 1025);
        assert!(smtp.username.is_empty());
        assert!(smtp.password.is_empty());
    }

    #[test]
    fn smtp_rejects_empty_host_and_unusable_ports() {
        assert!(matches!(
            resolve_smtp(Some(" ".into()), None, None, None, mail_from()),
            Err(ConfigError::EmptySmtpHost)
        ));
        for bad in ["abc", "0", "70000", ""] {
            assert!(
                matches!(
                    resolve_smtp(None, Some(bad.into()), None, None, mail_from()),
                    Err(ConfigError::InvalidSmtpPort(_))
                ),
                "expected InvalidSmtpPort for {bad:?}"
            );
        }
    }

    #[test]
    fn mail_from_is_required_at_boot() {
        // No fallback sender: an unset MAIL_FROM once meant production mail
        // went out from a placeholder.invalid domain with no signal.
        for missing in [None, Some(String::new()), Some("   ".into())] {
            assert!(
                matches!(
                    resolve_smtp(None, None, None, None, missing.clone()),
                    Err(ConfigError::MissingMailFrom)
                ),
                "expected MissingMailFrom for {missing:?}"
            );
        }
    }

    #[test]
    fn mail_from_accepts_bare_and_display_name_forms_and_trims() {
        assert_eq!(
            resolve_smtp(None, None, None, None, Some("no-reply@n1cck.radio".into()))
                .unwrap()
                .from,
            "no-reply@n1cck.radio"
        );
        assert_eq!(
            resolve_smtp(
                None,
                None,
                None,
                None,
                Some("  NetRoll by N1CCK <no-reply@n1cck.radio>  ".into())
            )
            .unwrap()
            .from,
            "NetRoll by N1CCK <no-reply@n1cck.radio>"
        );
    }

    #[test]
    fn mail_from_must_be_a_sendable_mailbox() {
        // Rejected at boot rather than on the first failed send: a sender the
        // adapter cannot parse turns every email into a runtime error.
        for bad in ["netroll", "no-reply@", "<no-reply@n1cck.radio", "a@b, c@d"] {
            assert!(
                matches!(
                    resolve_smtp(None, None, None, None, Some(bad.into())),
                    Err(ConfigError::InvalidMailFrom(_))
                ),
                "expected InvalidMailFrom for {bad:?}"
            );
        }
    }

    #[test]
    fn kek_absent_or_empty_resolves_to_none_so_the_app_still_boots() {
        // Absent KEK is a valid boot state: the app runs every non-QRZ feature
        // and the cipher fails closed (boot posture).
        assert!(resolve_kek(None).expect("absent KEK is ok").is_none());
        for empty in ["", "   ", "\n"] {
            assert!(
                resolve_kek(Some(empty.into()))
                    .expect("empty KEK is ok")
                    .is_none(),
                "empty KEK {empty:?} resolves to None, not an error"
            );
        }
    }

    #[test]
    fn kek_valid_base64_of_32_bytes_resolves_to_some() {
        use base64::Engine;
        let value = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        assert!(
            resolve_kek(Some(value))
                .expect("valid KEK resolves")
                .is_some()
        );
    }

    #[test]
    fn a_present_but_malformed_kek_is_a_hard_boot_error() {
        use base64::Engine;
        // Not base64 at all.
        assert!(matches!(
            resolve_kek(Some("not valid base64!!!".into())),
            Err(ConfigError::InvalidKek)
        ));
        // Valid base64 but the wrong number of bytes (AES-256 needs exactly 32).
        for wrong_len in [16usize, 31, 33, 64] {
            let value = base64::engine::general_purpose::STANDARD.encode(vec![0u8; wrong_len]);
            assert!(
                matches!(resolve_kek(Some(value)), Err(ConfigError::InvalidKek)),
                "a {wrong_len}-byte KEK must be rejected at boot"
            );
        }
    }

    #[test]
    fn max_nets_per_user_defaults_to_7_when_unset() {
        assert_eq!(resolve_max_nets_per_user(None).unwrap(), 7);
    }

    #[test]
    fn max_owners_per_net_defaults_to_5_when_unset() {
        assert_eq!(resolve_max_owners_per_net(None).unwrap(), 5);
    }

    #[test]
    fn caps_parse_a_valid_value_ignoring_whitespace() {
        assert_eq!(resolve_max_nets_per_user(Some(" 12\n".into())).unwrap(), 12);
        assert_eq!(resolve_max_owners_per_net(Some("3".into())).unwrap(), 3);
    }

    #[test]
    fn max_nets_per_user_rejects_zero_negative_non_numeric_and_empty() {
        for bad in [
            "0",
            "-1",
            "abc",
            "",
            "  ",
            "3.5",
            "99999999999999999999999999",
        ] {
            assert!(
                matches!(
                    resolve_max_nets_per_user(Some(bad.into())),
                    Err(ConfigError::InvalidMaxNetsPerUser(_))
                ),
                "expected InvalidMaxNetsPerUser for {bad:?}"
            );
        }
    }

    #[test]
    fn max_owners_per_net_rejects_zero_negative_non_numeric_and_empty() {
        for bad in ["0", "-4", "xyz", "", " "] {
            assert!(
                matches!(
                    resolve_max_owners_per_net(Some(bad.into())),
                    Err(ConfigError::InvalidMaxOwnersPerNet(_))
                ),
                "expected InvalidMaxOwnersPerNet for {bad:?}"
            );
        }
    }

    #[test]
    fn magic_link_aggregate_sends_per_hour_defaults_when_unset() {
        assert_eq!(
            resolve_magic_link_aggregate_sends_per_hour(None).unwrap(),
            DEFAULT_MAGIC_LINK_SENDS_PER_HOUR
        );
    }

    #[test]
    fn magic_link_aggregate_sends_per_hour_parses_a_valid_value_ignoring_whitespace() {
        assert_eq!(
            resolve_magic_link_aggregate_sends_per_hour(Some(" 120\n".into())).unwrap(),
            120
        );
        assert_eq!(
            resolve_magic_link_aggregate_sends_per_hour(Some("5".into())).unwrap(),
            5
        );
    }

    #[test]
    fn magic_link_aggregate_sends_per_hour_rejects_zero_negative_non_numeric_empty_and_overflow() {
        // A zero cap would silently mute ALL magic-link mail (nobody could sign
        // in); an unparseable one must never fall back to the default.
        for bad in [
            "0",
            "-1",
            "abc",
            "",
            "  ",
            "60.5",
            "4294967296",
            "99999999999999999999999999",
        ] {
            assert!(
                matches!(
                    resolve_magic_link_aggregate_sends_per_hour(Some(bad.into())),
                    Err(ConfigError::InvalidMagicLinkAggregateSendsPerHour(_))
                ),
                "expected InvalidMagicLinkAggregateSendsPerHour for {bad:?}"
            );
        }
    }

    #[test]
    fn magic_link_reserve_sends_per_hour_defaults_when_unset() {
        assert_eq!(
            resolve_magic_link_reserve_sends_per_hour(None).unwrap(),
            DEFAULT_MAGIC_LINK_RESERVE_SENDS_PER_HOUR
        );
    }

    #[test]
    fn magic_link_reserve_sends_per_hour_parses_a_valid_value_ignoring_whitespace() {
        assert_eq!(
            resolve_magic_link_reserve_sends_per_hour(Some(" 240\n".into())).unwrap(),
            240
        );
        assert_eq!(
            resolve_magic_link_reserve_sends_per_hour(Some("7".into())).unwrap(),
            7
        );
    }

    #[test]
    fn magic_link_reserve_sends_per_hour_rejects_zero_negative_non_numeric_empty_and_overflow() {
        // A zero reserve would mute the budget that keeps returning users
        // signing in while the general pool is saturated — fail loud, never fall
        // back to the default.
        for bad in [
            "0",
            "-1",
            "abc",
            "",
            "  ",
            "60.5",
            "4294967296",
            "99999999999999999999999999",
        ] {
            assert!(
                matches!(
                    resolve_magic_link_reserve_sends_per_hour(Some(bad.into())),
                    Err(ConfigError::InvalidMagicLinkReserveSendsPerHour(_))
                ),
                "expected InvalidMagicLinkReserveSendsPerHour for {bad:?}"
            );
        }
    }

    #[test]
    fn token_retention_days_defaults_when_unset() {
        assert_eq!(
            resolve_token_retention_days(None).unwrap(),
            DEFAULT_TOKEN_RETENTION_DAYS
        );
    }

    #[test]
    fn token_retention_days_parses_a_valid_value_ignoring_whitespace() {
        assert_eq!(
            resolve_token_retention_days(Some(" 90\n".into())).unwrap(),
            90
        );
        assert_eq!(resolve_token_retention_days(Some("1".into())).unwrap(), 1);
    }

    #[test]
    fn token_retention_days_rejects_zero_negative_non_numeric_and_empty() {
        // A zero window collapses the cutoff onto `now`, so every token and
        // session is removed the instant it dies — no retained history at all
        // to correlate a sign-in against the audit log. Fail loud rather than
        // silently substituting the default for a value the operator set.
        for bad in ["0", "-1", "abc", "", "  ", "30.5"] {
            assert!(
                matches!(
                    resolve_token_retention_days(Some(bad.into())),
                    Err(ConfigError::InvalidTokenRetentionDays(_))
                ),
                "expected InvalidTokenRetentionDays for {bad:?}"
            );
        }
    }

    #[test]
    fn token_retention_days_rejects_windows_that_would_overflow_the_millis_conversion() {
        // The caller multiplies the resolved days by MILLIS_PER_DAY, and release
        // builds run with overflow-checks off — so an unbounded value would wrap
        // to an arbitrary SHORTER window instead of erroring. Reject at the
        // resolver so an invalid value never boots — never a silent fallback.
        assert_eq!(
            resolve_token_retention_days(Some(MAX_TOKEN_RETENTION_DAYS.to_string()))
                .unwrap()
                .checked_mul(MILLIS_PER_DAY),
            Some(MAX_TOKEN_RETENTION_DAYS * MILLIS_PER_DAY),
            "the bound itself must be accepted and must convert without overflow"
        );

        for over in [
            MAX_TOKEN_RETENTION_DAYS + 1,
            u64::MAX / MILLIS_PER_DAY,
            u64::MAX / MILLIS_PER_DAY + 1,
            u64::MAX,
        ] {
            let resolved = resolve_token_retention_days(Some(over.to_string()));
            assert!(
                matches!(resolved, Err(ConfigError::InvalidTokenRetentionDays(_))),
                "expected InvalidTokenRetentionDays for {over}"
            );
        }

        // Every value the resolver ACCEPTS must survive the conversion — the
        // property the bound exists to guarantee, asserted rather than asserted
        // by comment.
        for good in ["1", "30", "3650", "36500"] {
            let days = resolve_token_retention_days(Some(good.into())).unwrap();
            assert!(
                days.checked_mul(MILLIS_PER_DAY).is_some(),
                "accepted {good} days must convert to millis without overflow"
            );
        }
    }

    #[test]
    fn bot_mitigation_secret_is_none_when_unset_or_blank_and_some_when_set() {
        // Absent/blank ⇒ disabled (the off switch).
        assert!(resolve_bot_mitigation_secret(None).unwrap().is_none());
        for blank in ["", "   ", "\n"] {
            assert!(
                resolve_bot_mitigation_secret(Some(blank.into()))
                    .unwrap()
                    .is_none(),
                "blank secret {blank:?} disables mitigation"
            );
        }
        // A present, long-enough value enables it and carries the trimmed key
        // bytes.
        let secret = resolve_bot_mitigation_secret(Some(" a-plenty-long-s3cret-key \n".into()))
            .expect("resolves")
            .expect("a set secret enables mitigation");
        assert_eq!(secret.as_slice(), b"a-plenty-long-s3cret-key");
    }

    #[test]
    fn a_present_but_short_bot_mitigation_secret_is_a_hard_boot_error() {
        // A weak secret is brute-forceable from public token pairs — unlike an
        // absent one (a safe, disabled state), this must fail loud rather than
        // silently accept a value that looks configured but isn't secure.
        for weak in ["x", "short-secret"] {
            assert!(
                matches!(
                    resolve_bot_mitigation_secret(Some(weak.into())),
                    Err(ConfigError::WeakBotMitigationSecret)
                ),
                "expected WeakBotMitigationSecret for {weak:?}"
            );
        }
    }

    #[test]
    fn a_bot_mitigation_secret_at_exactly_the_minimum_length_is_accepted() {
        let value = "a".repeat(MIN_BOT_MITIGATION_SECRET_BYTES);
        assert!(
            resolve_bot_mitigation_secret(Some(value))
                .expect("resolves")
                .is_some()
        );
    }

    #[test]
    fn cap_may_be_throttled_detects_a_cap_at_or_above_the_burst() {
        assert!(
            !cap_may_be_throttled_by_rate_limiter(7, 12),
            "default cap sits comfortably below the default burst"
        );
        assert!(
            cap_may_be_throttled_by_rate_limiter(12, 12),
            "a cap equal to the burst is flagged"
        );
        assert!(
            cap_may_be_throttled_by_rate_limiter(50, 12),
            "a cap raised well above the burst is flagged"
        );
        assert!(
            !cap_may_be_throttled_by_rate_limiter(11, 12),
            "one below the burst is not flagged"
        );
    }

    #[test]
    fn admin_allowlist_is_empty_when_unset_or_blank_so_the_app_still_boots() {
        // Unset/blank ⇒ no admins configured (the safe default, not a boot error
        // — the optional-KEK posture). The admin surface is simply unreachable.
        assert!(resolve_admin_allowlist(None).unwrap().is_empty());
        for blank in ["", "   ", "\n"] {
            assert!(
                resolve_admin_allowlist(Some(blank.into()))
                    .unwrap()
                    .is_empty(),
                "blank allowlist {blank:?} means no admins, not an error"
            );
        }
    }

    #[test]
    fn admin_allowlist_parses_and_normalizes_a_comma_separated_list() {
        let admins = resolve_admin_allowlist(Some(" Admin@Example.COM , op@example.org ".into()))
            .expect("a well-formed list resolves");
        // Each entry is trimmed + ASCII-lowercased (the stored/sign-in form).
        assert_eq!(admins, vec!["admin@example.com", "op@example.org"]);
    }

    #[test]
    fn admin_allowlist_with_a_blank_or_malformed_entry_is_a_hard_boot_error() {
        // A present list the operator INTENDED as admins, with a trailing comma
        // (empty entry) or a non-address token, must fail loud rather than
        // silently grant/deny the wrong set.
        for bad in [
            "admin@example.com,",
            "admin@example.com,,op@example.org",
            "not-an-email",
        ] {
            assert!(
                matches!(
                    resolve_admin_allowlist(Some(bad.into())),
                    Err(ConfigError::InvalidAdminAllowlist(_))
                ),
                "expected InvalidAdminAllowlist for {bad:?}"
            );
        }
    }

    #[test]
    fn csp_report_only_defaults_false_and_parses_true_false_case_insensitively() {
        // Unset ⇒ enforce: report-only is a diagnostic posture an operator opts
        // into for a rollout, never the resting state.
        assert!(!resolve_csp_report_only(None).unwrap());
        for truthy in ["true", "TRUE", "True", " true\n", "TRUE "] {
            assert!(
                resolve_csp_report_only(Some(truthy.into())).unwrap(),
                "{truthy:?} selects report-only"
            );
        }
        for falsy in ["false", "FALSE", " False\n"] {
            assert!(
                !resolve_csp_report_only(Some(falsy.into())).unwrap(),
                "{falsy:?} selects enforce"
            );
        }
    }

    #[test]
    fn csp_report_only_rejects_anything_else_as_a_boot_error() {
        // A typo must never silently enforce OR silently disarm — a flag that
        // reads `yes` as false would enforce a policy the operator meant to
        // only observe, and one that reads `1` as true would disarm it.
        for bad in ["yes", "1", "", " ", "on", "enforce"] {
            assert!(
                matches!(
                    resolve_csp_report_only(Some(bad.into())),
                    Err(ConfigError::InvalidCspReportOnly(_))
                ),
                "expected InvalidCspReportOnly for {bad:?}"
            );
        }
        // Trimming is part of the contract: a trailing space is not a typo.
        assert!(resolve_csp_report_only(Some("TRUE ".into())).unwrap());
    }

    #[test]
    fn csp_policy_override_is_none_when_unset() {
        assert_eq!(resolve_csp_policy(None).unwrap(), None);
    }

    #[test]
    fn csp_policy_override_trims_and_passes_a_set_value_through() {
        // Verbatim (trimmed), no merging with the derived default: an operator
        // who overrides owns the whole policy.
        assert_eq!(
            resolve_csp_policy(Some("  default-src 'none'; script-src 'self' \n".into()))
                .unwrap()
                .as_deref(),
            Some("default-src 'none'; script-src 'self'")
        );
    }

    #[test]
    fn csp_policy_override_rejects_blank_and_non_header_values() {
        // Blank is a config mistake, not "use the default": the operator
        // clearly meant to set a policy (the EmptyStaticDir posture).
        for blank in ["", "   ", "\n"] {
            assert!(
                matches!(
                    resolve_csp_policy(Some(blank.into())),
                    Err(ConfigError::EmptyCspPolicy)
                ),
                "expected EmptyCspPolicy for {blank:?}"
            );
        }
        // CR/LF, DEL, and any byte >= 0x7F are not visible ASCII and never
        // valid in the value the app would put on the wire.
        for bad in [
            "a\r\nb",
            "default-src 'self'\nscript-src 'none'",
            "default-src 'self' \u{7f}",
            "default-src 'self' \u{e9}",
        ] {
            assert!(
                matches!(
                    resolve_csp_policy(Some(bad.into())),
                    Err(ConfigError::InvalidCspPolicy(_))
                ),
                "expected InvalidCspPolicy for {bad:?}"
            );
        }
        // A CR/LF-bearing value is header-injection-shaped: the diagnostic
        // must describe it without reprinting it.
        let Err(ConfigError::InvalidCspPolicy(reason)) =
            resolve_csp_policy(Some("a\r\nX-Injected: b".into()))
        else {
            panic!("a CR/LF value must be rejected");
        };
        assert!(
            !reason.contains("X-Injected"),
            "a CR/LF value must not be echoed back, got {reason:?}"
        );
    }

    #[test]
    fn csp_policy_override_must_name_at_least_one_csp_directive() {
        // A header-safe string a browser discards whole (`hello`, an underscore
        // typo) enforces nothing while looking configured — the same silent
        // disarm CSP_REPORT_ONLY refuses.
        for bad in [
            "hello",
            "default_src 'self'",
            "defaultsrc 'self'; scriptsrc 'none'",
            "'self'",
        ] {
            assert!(
                matches!(
                    resolve_csp_policy(Some(bad.into())),
                    Err(ConfigError::UnrecognizedCspPolicy(_))
                ),
                "expected UnrecognizedCspPolicy for {bad:?}"
            );
        }
        // Not a quality check: one recognized directive is enough whatever else
        // rides along, and directive names are case-insensitive.
        for ok in [
            "frame-ancestors 'none'",
            "DEFAULT-SRC 'self'",
            "default-src 'self'; made-up-directive x",
            "; default-src 'self'",
        ] {
            assert!(
                resolve_csp_policy(Some(ok.into())).is_ok(),
                "{ok:?} names a directive and must resolve"
            );
        }
    }

    #[test]
    fn plausible_script_host_is_none_when_unset_or_blank_and_keeps_a_valid_value_trimmed() {
        assert_eq!(resolve_plausible_script_host(None).unwrap(), None);
        assert_eq!(
            resolve_plausible_script_host(Some("  ".into())).unwrap(),
            None
        );
        // The path is KEPT: the tracker appends /api/event to the value as
        // configured and a sub-path install is legitimate. Only the policy
        // reduces it to an origin.
        assert_eq!(
            resolve_plausible_script_host(Some(" https://analytics.example/js ".into()))
                .unwrap()
                .as_deref(),
            Some("https://analytics.example/js")
        );
    }

    #[test]
    fn plausible_script_host_that_cannot_stand_as_a_host_source_is_a_hard_boot_error() {
        // The value is interpolated into a `;`-joined policy, so anything that
        // is not an http(s) URL on a plain host must stop boot rather than
        // reach the header.
        for bad in [
            "analytics.example",
            "ftp://analytics.example",
            "https://",
            "https://a.example; frame-ancestors *",
            "https://a.example https://b.example",
            "https://a.example,https://b.example",
        ] {
            assert!(
                matches!(
                    resolve_plausible_script_host(Some(bad.into())),
                    Err(ConfigError::InvalidPlausibleScriptHost(_))
                ),
                "expected InvalidPlausibleScriptHost for {bad:?}"
            );
        }
    }

    #[test]
    fn validate_accepts_a_dir_with_index_html() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
        assert!(validate_static_dir(&fixture).is_ok());
    }

    #[test]
    fn validate_rejects_missing_dirs_and_dirs_without_index_html() {
        let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("no-such-dir");
        let no_index = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for dir in [missing, no_index] {
            assert!(
                matches!(
                    validate_static_dir(&dir),
                    Err(ConfigError::StaticDirInvalid(_))
                ),
                "expected StaticDirInvalid for {dir:?}"
            );
        }
    }
}
