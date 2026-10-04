// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
use core::future::Future;
use core::pin::Pin;

use thiserror::Error;
use uuid::Uuid;

use crate::lookup::{CallsignRecord, LookupError};
use crate::qrz::{CipherError, QrzCredentials, SealedQrzCredentials};

/// Boxed future used by async ports, so the domain crate stays free of any
/// async runtime (the I/O ban: no tokio/sqlx/axum/fred here).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Delivery failure reported by a [`Mailer`] implementation.
#[derive(Debug, Error)]
#[error("magic-link email delivery failed: {0}")]
pub struct MailError(pub String);

/// One file attachment on a net-summary email: a filename, a MIME
/// content-type, and the already-generated body text.
///
/// The delivery layer fills these from `export::to_csv`/`to_adif`
/// — never a hand-rolled generator — so the bytes are injection-safe by
/// construction: the CSV is already formula-neutralized and RFC-4180 quoted.
pub struct MailAttachment {
    /// The download filename (ASCII-slugified upstream).
    pub filename: String,
    /// The MIME content-type, e.g. `text/csv; charset=utf-8`.
    pub content_type: String,
    /// The attachment body (a UTF-8 export document).
    pub content: String,
}

/// The on-close net-session summary to email: a subject, a
/// plain-text body (net identity, session window, participant count, a compact
/// roster listing), and any file attachments (the CSV/ADIF roster export).
///
/// An owned value type built by the app layer from the folded session — the
/// `Mailer` port stays free of the fold/projection types.
pub struct NetSummaryMail {
    /// The email subject line.
    pub subject: String,
    /// The plain-text summary body.
    pub body: String,
    /// The branded HTML alternative to [`Self::body`], carrying the SAME
    /// station lines from the same roster loop. Built
    /// alongside `body` in the app layer rather than in the mail adapter,
    /// because the adapter never sees the fold types the roster comes from
    /// — so the two parts provably share one projection of the roster
    /// instead of two.
    ///
    /// User-controlled text (net title, station name, location) is
    /// HTML-escaped in THIS field and deliberately NOT escaped in `body`.
    pub html_body: String,
    /// The file attachments (CSV/ADIF), possibly empty.
    pub attachments: Vec<MailAttachment>,
    /// The LOCAL PART of the message's RFC 5322 `Message-ID` —
    /// stable for a session, so every attempt of this summary is, to a
    /// receiver, the same message. Duplicate suppression on it is common MUA
    /// behaviour (Gmail and Thunderbird both do it), not a guarantee.
    ///
    /// The local part only, deliberately: the domain must be one NetRoll owns
    /// and only the mail adapter knows the configured sender, so it supplies
    /// the domain from `MAIL_FROM` — never the process's hostname, which in a
    /// container is a random hex string and which lettre's own default would
    /// use. Before this field the summary went out with NO `Message-ID` at all
    /// and the relay minted a fresh one per submission, so two retries were two
    /// unrelated messages with certainty.
    pub message_id: String,
}

/// Failure reported by an [`AvatarStore`] implementation. Carries a message
/// for the operator log only — the HTTP layer maps this to a generic problem,
/// since a filesystem path is not a caller's business.
#[derive(Debug, Error)]
#[error("avatar storage failed: {0}")]
pub struct AvatarStoreError(pub String);

/// Blob storage for uploaded avatars, keyed by the filename the domain derived
/// (`<account-id>.<ext>`; see [`crate::avatar`]).
///
/// A port rather than direct filesystem calls so the HTTP layer stays testable
/// without a real volume, and so a future object-store adapter is a swap rather
/// than a rewrite.
pub trait AvatarStore {
    /// Writes `bytes` under `file_name`, REPLACING any existing file with that
    /// name. Replacement is the normal path: one avatar per account.
    fn put<'a>(
        &'a self,
        file_name: &'a str,
        bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<(), AvatarStoreError>>;

    /// Removes `file_name`. Succeeds when the file is already gone — callers
    /// delete on replace and on account erasure, and neither should fail
    /// because the bytes were already absent.
    fn delete<'a>(&'a self, file_name: &'a str) -> BoxFuture<'a, Result<(), AvatarStoreError>>;
}

/// Outbound email port: delivers the magic-link message plus the
/// identifying-email change confirmation and notice mails.
///
/// # A new method on this trait owes an HTML part, and nothing checks that
///
/// Every message this port sends goes out as
/// `multipart/alternative` — a plain-text part and a branded HTML part carrying
/// the same content. That is bound by ONE test walking a list of the four
/// builders (`netroll-adapters/src/mail.rs`,
/// `every_one_of_the_four_kinds_carries_a_text_and_an_html_alternative`).
///
/// That list is HAND-MAINTAINED. Rust cannot reflect over a trait's methods, so
/// a FIFTH message kind added here gets no HTML part, is absent from that test's
/// list, and NOTHING goes red. This doc comment is the whole mechanism: adding a
/// method here means adding a row to that test and an MJML source under
/// `tools/email/src/`. That coupling is not mechanically enforced.
pub trait Mailer {
    /// Sends the sign-in link to `to`. The link carries the raw token —
    /// implementations must never log it.
    fn send_magic_link<'a>(
        &'a self,
        to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>>;

    /// Sends the email-change confirmation link to the NEW address `to`.
    /// The link carries the raw token — never log it.
    fn send_email_change<'a>(
        &'a self,
        to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>>;

    /// Sends the courtesy notice to the OLD address after a completed
    /// change, naming the address the account moved to. No link, no token.
    fn send_email_change_notice<'a>(
        &'a self,
        to: &'a str,
        new_email: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>>;

    /// Sends the on-close net-session summary to `to`. No
    /// token is involved (the raw-token caveat does not apply), but the
    /// recipient address must still stay out of any error (the static-message
    /// posture the other methods keep).
    ///
    /// Defaulted so test fakes and any non-delivering `Mailer` need not
    /// implement it; the production `SmtpMailer` overrides it.
    ///
    /// The default returns an ERROR (never `Ok`), deliberately — a silent
    /// `Ok(())` default would let any future `Mailer` implementation (a test
    /// fake or otherwise) that forgets to override this method report success
    /// while actually delivering nothing, with no compiler error and no
    /// visible failure. Returning an error instead means an un-overridden
    /// implementation fails LOUDLY through the deliverer's own bounded-retry
    /// and per-target-failure logging.
    fn send_net_summary<'a>(
        &'a self,
        to: &'a str,
        summary: &'a NetSummaryMail,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        let _ = (to, summary);
        Box::pin(async {
            Err(MailError(
                "send_net_summary is not implemented on this Mailer".to_owned(),
            ))
        })
    }
}

/// Envelope-encryption port for QRZ credentials.
///
/// The concrete implementation (`netroll-adapters/src/crypto/`) holds the
/// instance KEK in memory and performs the two-level AES-256-GCM envelope
/// crypto; this trait is the pure seam the app layer calls. Object-safe so it
/// wires as `Arc<dyn CredentialCipher + Send + Sync>`, mirroring `Mailer`.
///
/// Both methods take `account_id`: it is bound as GCM associated data on the
/// credential ciphertext, so a sealed blob transplanted into another account's
/// row fails the tag check on `open` rather than decrypting into the wrong
/// account. A fail-closed implementation (no KEK) returns
/// [`CipherError::KekUnavailable`] from both — never a panic, never plaintext.
pub trait CredentialCipher {
    /// Seals a credential pair for `account_id`: a fresh per-record DEK
    /// encrypts the credentials, then the instance KEK wraps that DEK. Returns
    /// the opaque [`SealedQrzCredentials`] to persist.
    fn seal(
        &self,
        account_id: Uuid,
        creds: &QrzCredentials,
    ) -> Result<SealedQrzCredentials, CipherError>;

    /// Opens a previously-sealed record for `account_id`, reversing the
    /// envelope. Fails (never panics, never returns garbage) on an absent KEK,
    /// a wrong KEK, an `account_id`/AAD mismatch, or tampered bytes.
    fn open(
        &self,
        account_id: Uuid,
        sealed: &SealedQrzCredentials,
    ) -> Result<QrzCredentials, CipherError>;
}

/// Source of current wall-clock time, injected so domain logic stays pure.
///
/// Domain code must never call `Instant::now()` or `SystemTime::now()`
/// directly; all time enters through this port.
pub trait Clock {
    /// Returns the current time as milliseconds since the Unix epoch.
    fn now_epoch_millis(&self) -> u64;
}

/// Best-effort callbook lookup port.
///
/// One adapter per provider (QRZ XML, hamcall.dev); the app-layer
/// `LookupService` composes them behind a fallback chain so a new provider
/// (e.g. HamQTH) is added HERE — a third adapter behind this same trait —
/// without touching the check-in flow. Object-safe so it wires as
/// `Arc<dyn LookupProvider + Send + Sync>`, mirroring [`Mailer`] and
/// [`crate::egress::Egress`].
pub trait LookupProvider {
    /// Looks up `callsign`. `credentials` carries the acting user's QRZ
    /// credentials when available; providers that need no auth (hamcall)
    /// ignore it. It is an `Option` — and per-request rather than provider
    /// state — because credentials are per-acting-user, so a provider cannot be
    /// a static singleton holding one user's secret; the secret must flow per
    /// call. Returns:
    /// - `Ok(Some(record))` on a hit,
    /// - `Ok(None)` on a definitive "not found" (this provider has no data),
    /// - `Err(LookupError)` on a provider failure (network/timeout/upstream
    ///   error/invalid credentials) — the service treats this as "try the
    ///   next provider", never as a caller-facing error.
    fn lookup<'a>(
        &'a self,
        callsign: &'a str,
        credentials: Option<&'a QrzCredentials>,
    ) -> BoxFuture<'a, Result<Option<CallsignRecord>, LookupError>>;
}
