// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! SMTP implementation of the domain `Mailer` port. Four message shapes, each
//! `multipart/alternative`; the raw token appears ONLY in the link, never in a
//! log or an error. The escaping asymmetry is deliberate: HTML parts are
//! escaped templates, text parts are not templates at all, so a link's `&` can
//! never ship as `&amp;` in the text part.

use std::time::Duration;

use askama::Template;
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use netroll_domain::auth::MAGIC_LINK_TTL_MILLIS;
use netroll_domain::ports::{BoxFuture, MailError, Mailer, NetSummaryMail};

/// Turns a template render result into a [`MailError`], so a render failure
/// joins the SMTP-failure path the four send sites already handle instead of
/// panicking inside a request task.
///
/// The message is STATIC and carries no recipient, no address and no token,
/// exactly like the other errors in this module.
fn rendered(result: askama::Result<String>) -> Result<String, MailError> {
    result.map_err(|_| MailError("email template failed to render".into()))
}

/// The MJML-generated HTML part of the sign-in mail.
#[derive(Template)]
#[template(path = "magic_link.html")]
struct MagicLinkHtml<'a> {
    link: &'a str,
    ttl_minutes: u64,
}

/// The MJML-generated HTML part of the email-change confirmation mail.
#[derive(Template)]
#[template(path = "email_change.html")]
struct EmailChangeHtml<'a> {
    link: &'a str,
    ttl_minutes: u64,
}

/// The MJML-generated HTML part of the email-change courtesy notice.
#[derive(Template)]
#[template(path = "email_change_notice.html")]
struct EmailChangeNoticeHtml<'a> {
    new_email: &'a str,
}

/// Cap on any SMTP exchange — a tarpit or unresponsive relay must not pin
/// the public request-handling task indefinitely.
const SMTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether `email` parses as a bare RFC 5321 mailbox (`user@domain`, no
/// display name). Guards the API boundary so an address this adapter cannot
/// deliver to fails request validation instead of surfacing later as a mail
/// error.
pub fn is_deliverable_address(email: &str) -> bool {
    email.parse::<Address>().is_ok()
}

/// Whether `value` parses as a message sender — either a bare address or the
/// `Display Name <user@domain>` form. Guards `MAIL_FROM` at boot so a sender
/// this adapter cannot use fails startup instead of every later send.
pub fn is_valid_sender(value: &str) -> bool {
    value.parse::<Mailbox>().is_ok()
}

/// SMTP connection settings, resolved from `SMTP_*` and `MAIL_FROM` env at boot.
#[derive(Clone)]
pub struct SmtpConfig {
    /// SMTP server hostname.
    pub host: String,
    /// SMTP server port.
    pub port: u16,
    /// Auth username; empty selects the unauthenticated dev path (mailpit).
    pub username: String,
    /// Auth password.
    pub password: String,
    /// Sender every message goes out as, bare or with a display name.
    pub from: String,
}

/// Sends the magic-link email over SMTP.
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl SmtpMailer {
    /// Builds the transport: STARTTLS + credentials when a username is
    /// configured, plain unauthenticated SMTP otherwise (dev mail catcher).
    pub fn new(config: &SmtpConfig) -> Result<Self, MailError> {
        let from: Mailbox = config
            .from
            .parse()
            .map_err(|_| MailError("MAIL_FROM is not a usable sender address".into()))?;
        let builder = if config.username.is_empty() {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)
                .map_err(|e| MailError(e.to_string()))?
                .credentials(Credentials::new(
                    config.username.clone(),
                    config.password.clone(),
                ))
        };
        Ok(Self {
            transport: builder
                .port(config.port)
                .timeout(Some(SMTP_TIMEOUT))
                .build(),
            from,
        })
    }
}

fn magic_link_body(link: &str) -> String {
    let ttl_minutes = MAGIC_LINK_TTL_MILLIS / 60_000;
    // The link sits on its own line so wire encoding (lettre picks 7bit or
    // quoted-printable) reassembles it into one clickable URL on decode.
    format!(
        "Sign in to NetRoll:\r\n\r\n{link}\r\n\r\nThis link works once and expires in \
         {ttl_minutes} minutes.\r\nIf you didn't request it, you can ignore this email.\r\n"
    )
}

fn magic_link_message(from: &Mailbox, to: &str, link: &str) -> Result<Message, MailError> {
    // Static message keeps the recipient address out of the error.
    let to: Mailbox = to
        .parse()
        .map_err(|_| MailError("recipient is not a deliverable address".into()))?;

    let html = rendered(
        MagicLinkHtml {
            link,
            ttl_minutes: MAGIC_LINK_TTL_MILLIS / 60_000,
        }
        .render(),
    )?;

    Message::builder()
        .from(from.clone())
        .to(to)
        .subject("Sign in to NetRoll")
        .multipart(MultiPart::alternative_plain_html(
            magic_link_body(link),
            html,
        ))
        .map_err(|e| MailError(e.to_string()))
}

fn email_change_body(link: &str) -> String {
    let ttl_minutes = MAGIC_LINK_TTL_MILLIS / 60_000;
    // Link on its own line so wire encoding reassembles one clickable URL.
    format!(
        "Confirm your new NetRoll email:\r\n\r\n{link}\r\n\r\nThis link works once and expires in \
         {ttl_minutes} minutes.\r\nIf you didn't request it, you can ignore this email.\r\n"
    )
}

fn email_change_message(from: &Mailbox, to: &str, link: &str) -> Result<Message, MailError> {
    // Static message keeps the recipient address out of the error.
    let to: Mailbox = to
        .parse()
        .map_err(|_| MailError("recipient is not a deliverable address".into()))?;

    let html = rendered(
        EmailChangeHtml {
            link,
            ttl_minutes: MAGIC_LINK_TTL_MILLIS / 60_000,
        }
        .render(),
    )?;

    Message::builder()
        .from(from.clone())
        .to(to)
        .subject("Confirm your new NetRoll email")
        .multipart(MultiPart::alternative_plain_html(
            email_change_body(link),
            html,
        ))
        .map_err(|e| MailError(e.to_string()))
}

fn email_change_notice_body(new_email: &str) -> String {
    // Informational only: no link and no token — a credential mailed to the
    // OLD (possibly hostile) mailbox would defeat the point of the change.
    format!(
        "Your NetRoll email was changed to {new_email}.\r\n\r\nIf this wasn't you, your account \
         may be compromised.\r\n"
    )
}

fn email_change_notice_message(
    from: &Mailbox,
    to: &str,
    new_email: &str,
) -> Result<Message, MailError> {
    // Static message keeps the recipient address out of the error.
    let to: Mailbox = to
        .parse()
        .map_err(|_| MailError("recipient is not a deliverable address".into()))?;

    let html = rendered(EmailChangeNoticeHtml { new_email }.render())?;

    Message::builder()
        .from(from.clone())
        .to(to)
        .subject("Your NetRoll email was changed")
        .multipart(MultiPart::alternative_plain_html(
            email_change_notice_body(new_email),
            html,
        ))
        .map_err(|e| MailError(e.to_string()))
}

/// Builds the on-close net-summary message. Both parts arrive pre-rendered on
/// [`NetSummaryMail`] — the text body
/// and its HTML alternative are built together in the app layer from ONE roster
/// loop, so the two parts cannot describe different sessions.
///
/// With no attachments it is a bare `multipart/alternative`; with attachments
/// that alternative is NESTED inside a `multipart/mixed` that also carries each
/// CSV/ADIF export. A flat `mixed[plain, html, csv, adi]` is the wrong shape
/// — a client would show the HTML part as a fourth attachment rather than as an
/// alternative to the text.
///
/// A recipient this adapter cannot deliver to fails here with a STATIC message
/// that never echoes the address, exactly like the other builders.
fn net_summary_message(
    from: &Mailbox,
    to: &str,
    summary: &NetSummaryMail,
) -> Result<Message, MailError> {
    let to: Mailbox = to
        .parse()
        .map_err(|_| MailError("recipient is not a deliverable address".into()))?;

    let builder = Message::builder()
        .from(from.clone())
        .to(to)
        .subject(summary.subject.clone())
        // A STABLE Message-ID, the email's dedupe basis across a
        // retry — the same id on every attempt of one session's summary. The
        // domain is the configured sender's, never `hostname`, which lettre's
        // own `message_id(None)` would use and which in a container is a
        // random hex string.
        .message_id(Some(format!(
            "<{}@{}>",
            summary.message_id,
            from.email.domain()
        )));

    let alternative =
        MultiPart::alternative_plain_html(summary.body.clone(), summary.html_body.clone());

    if summary.attachments.is_empty() {
        return builder
            .multipart(alternative)
            .map_err(|e| MailError(e.to_string()));
    }

    // `MultiPart::mixed()` yields a `MultiPartBuilder`, so this `.multipart` is
    // `MultiPartBuilder::multipart` — the sibling of the `.singlepart` used
    // below — and it is what puts the alternative INSIDE the mixed.
    let mut multipart = MultiPart::mixed().multipart(alternative);
    for attachment in &summary.attachments {
        let content_type =
            ContentType::parse(&attachment.content_type).map_err(|e| MailError(e.to_string()))?;
        multipart = multipart.singlepart(
            Attachment::new(attachment.filename.clone())
                .body(attachment.content.clone().into_bytes(), content_type),
        );
    }
    builder
        .multipart(multipart)
        .map_err(|e| MailError(e.to_string()))
}

impl Mailer for SmtpMailer {
    fn send_magic_link<'a>(
        &'a self,
        to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            let message = magic_link_message(&self.from, to, link)?;
            self.transport
                .send(message)
                .await
                .map(|_| ())
                .map_err(|e| MailError(e.to_string()))
        })
    }

    fn send_email_change<'a>(
        &'a self,
        to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            let message = email_change_message(&self.from, to, link)?;
            self.transport
                .send(message)
                .await
                .map(|_| ())
                .map_err(|e| MailError(e.to_string()))
        })
    }

    fn send_email_change_notice<'a>(
        &'a self,
        to: &'a str,
        new_email: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            let message = email_change_notice_message(&self.from, to, new_email)?;
            self.transport
                .send(message)
                .await
                .map(|_| ())
                .map_err(|e| MailError(e.to_string()))
        })
    }

    fn send_net_summary<'a>(
        &'a self,
        to: &'a str,
        summary: &'a NetSummaryMail,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            let message = net_summary_message(&self.from, to, summary)?;
            self.transport
                .send(message)
                .await
                .map(|_| ())
                .map_err(|e| MailError(e.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn sender() -> Mailbox {
        "NetRoll by N1CCK <no-reply@n1cck.radio>"
            .parse()
            .expect("test sender parses")
    }

    #[test]
    fn every_message_is_sent_from_the_configured_sender() {
        // The FROM address is operator config (MAIL_FROM), not a baked-in
        // constant — each builder must carry the configured mailbox through.
        let from = sender();
        let summary = sample_summary();
        let messages = [
            magic_link_message(&from, "op@example.com", "http://localhost/x").expect("build"),
            email_change_message(&from, "new@example.com", "http://localhost/x").expect("build"),
            email_change_notice_message(&from, "old@example.com", "new@example.com")
                .expect("build"),
            net_summary_message(&from, "op@example.com", &summary).expect("build"),
        ];

        for message in messages {
            assert_eq!(
                message.envelope().from().map(ToString::to_string),
                Some("no-reply@n1cck.radio".to_string())
            );
        }
    }

    #[test]
    fn the_sender_display_name_rides_the_message_headers() {
        let raw = String::from_utf8_lossy(
            &magic_link_message(&sender(), "op@example.com", "http://localhost/x")
                .expect("build")
                .formatted(),
        )
        .into_owned();

        assert!(
            raw.contains("NetRoll by N1CCK") && raw.contains("no-reply@n1cck.radio"),
            "the configured display name and address must appear in the From header"
        );
    }

    #[test]
    fn message_is_addressed_to_the_recipient() {
        let message = magic_link_message(
            &sender(),
            "op@example.com",
            "http://localhost:5173/auth/verify?token=abc123",
        )
        .expect("build message");

        let to: Vec<String> = message
            .envelope()
            .to()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(to, vec!["op@example.com".to_string()]);
    }

    #[test]
    fn body_carries_the_link_intact_and_states_the_ttl() {
        let body = magic_link_body("http://localhost:5173/auth/verify?token=abc123");

        assert!(body.contains("http://localhost:5173/auth/verify?token=abc123"));
        assert!(
            body.contains("15 minutes"),
            "TTL note must match MAGIC_LINK_TTL_MILLIS"
        );
    }

    #[test]
    fn an_undeliverable_recipient_is_an_error_that_does_not_leak_the_address() {
        let err = magic_link_message(&sender(), "not-an-address", "http://localhost/x")
            .expect_err("must reject undeliverable recipient");
        assert!(
            !err.to_string().contains("not-an-address"),
            "error text must not echo the address"
        );
    }

    #[test]
    fn email_change_message_is_addressed_to_the_new_recipient() {
        let message = email_change_message(
            &sender(),
            "new@example.com",
            "http://localhost:5173/auth/confirm-email-change?token=abc123",
        )
        .expect("build message");

        let to: Vec<String> = message
            .envelope()
            .to()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(to, vec!["new@example.com".to_string()]);
    }

    #[test]
    fn email_change_body_carries_the_link_intact_and_states_the_ttl() {
        let body =
            email_change_body("http://localhost:5173/auth/confirm-email-change?token=abc123");

        assert!(
            body.contains("http://localhost:5173/auth/confirm-email-change?token=abc123"),
            "the confirmation link must survive intact"
        );
        assert!(
            body.contains("15 minutes"),
            "TTL note must match MAGIC_LINK_TTL_MILLIS"
        );
    }

    #[test]
    fn email_change_message_undeliverable_recipient_does_not_leak_the_address() {
        let err = email_change_message(&sender(), "not-an-address", "http://localhost/x")
            .expect_err("must reject undeliverable recipient");
        assert!(
            !err.to_string().contains("not-an-address"),
            "error text must not echo the address"
        );
    }

    #[test]
    fn email_change_notice_is_addressed_to_the_old_recipient_and_names_the_new_address() {
        let message = email_change_notice_message(&sender(), "old@example.com", "new@example.com")
            .expect("build notice");

        let to: Vec<String> = message
            .envelope()
            .to()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(to, vec!["old@example.com".to_string()]);

        let body = email_change_notice_body("new@example.com");
        assert!(
            body.contains("new@example.com"),
            "the notice names the address the account moved to"
        );
    }

    fn sample_summary() -> NetSummaryMail {
        use netroll_domain::ports::MailAttachment;
        NetSummaryMail {
            subject: "NetRoll session summary: Sunday Traffic Net".into(),
            body: "Net: Sunday Traffic Net\nParticipants: 3\n".into(),
            html_body: "<p>Net: Sunday Traffic Net</p>".into(),
            attachments: vec![MailAttachment {
                filename: "sunday-traffic-net-20260718.csv".into(),
                content_type: "text/csv; charset=utf-8".into(),
                content: "callsign,name\r\nW1AW,Maria\r\n".into(),
            }],
            message_id: "net-session-close.00000000-0000-0000-0000-000000001707".into(),
        }
    }

    /// The `Message-ID` header line of a built message, if it carries one.
    fn message_id_header(message: &Message) -> Option<String> {
        String::from_utf8_lossy(&message.formatted())
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("message-id:"))
            .map(|line| line["message-id:".len()..].trim().to_owned())
    }

    #[test]
    fn the_summary_carries_a_stable_message_id_under_the_senders_domain() {
        // The email's dedupe basis has to be on
        // the WIRE, not only on the struct. Two builds of the same summary carry
        // the same header; the domain is the configured sender's, never a
        // hostname; and the value is a bracketed addr-spec.
        let from = sender();
        let summary = sample_summary();
        let first = net_summary_message(&from, "op@example.com", &summary).expect("build");
        let second = net_summary_message(&from, "op@example.com", &summary).expect("build");
        let header = message_id_header(&first)
            .expect("the built summary carries a Message-ID header (it used to carry none)");
        assert_eq!(message_id_header(&second).as_deref(), Some(header.as_str()));
        assert_eq!(
            header,
            format!("<{}@n1cck.radio>", summary.message_id),
            "the local part is the summary's; the domain is the sender's"
        );
        let mut other = sample_summary();
        other.message_id = "net-session-close.00000000-0000-0000-0000-000000001708".into();
        assert_ne!(
            message_id_header(
                &net_summary_message(&from, "op@example.com", &other).expect("build")
            ),
            Some(header),
            "a different session is a different message"
        );
    }

    #[test]
    fn net_summary_message_is_addressed_to_the_recipient() {
        let message =
            net_summary_message(&sender(), "op@example.com", &sample_summary()).expect("build");
        let to: Vec<String> = message
            .envelope()
            .to()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(to, vec!["op@example.com".to_string()]);
    }

    #[test]
    fn net_summary_message_includes_the_csv_attachment_filename() {
        // The reused CSV export rides as a named attachment part — its
        // filename appears in the MIME Content-Disposition header.
        let message =
            net_summary_message(&sender(), "op@example.com", &sample_summary()).expect("build");
        let raw = String::from_utf8_lossy(&message.formatted()).into_owned();
        assert!(
            raw.contains("sunday-traffic-net-20260718.csv"),
            "the CSV attachment filename must appear in the MIME headers"
        );
        assert!(
            raw.contains("NetRoll session summary: Sunday Traffic Net"),
            "the subject rides the message headers"
        );
    }

    #[test]
    fn net_summary_message_without_attachments_is_still_an_alternative() {
        let summary = NetSummaryMail {
            subject: "s".into(),
            body: "just text".into(),
            html_body: "<p>just text</p>".into(),
            attachments: Vec::new(),
            message_id: "net-session-close.x".into(),
        };
        // All four kinds are multipart unconditionally, and a summary with an
        // empty roster still goes out — so the no-attachment branch is a bare
        // `multipart/alternative`, never a flat single part.
        let message = net_summary_message(&sender(), "op@example.com", &summary).expect("build");
        let to: Vec<String> = message
            .envelope()
            .to()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(to, vec!["op@example.com".to_string()]);
        let raw = String::from_utf8_lossy(&message.formatted()).into_owned();
        assert!(
            raw.contains("multipart/alternative"),
            "the no-attachment branch must still carry both parts"
        );
        assert!(
            !raw.contains("multipart/mixed"),
            "with nothing to attach there is no mixed wrapper to build"
        );
    }

    #[test]
    fn net_summary_undeliverable_recipient_does_not_leak_the_address() {
        let err = net_summary_message(&sender(), "not-an-address", &sample_summary())
            .expect_err("must reject undeliverable recipient");
        assert!(
            !err.to_string().contains("not-an-address"),
            "error text must not echo the address"
        );
    }

    #[test]
    fn email_change_notice_body_carries_no_link_or_raw_token() {
        // The notice is informational only — a link or token here would be a
        // second single-use credential mailed to the OLD (possibly hostile)
        // mailbox.
        let body = email_change_notice_body("new@example.com");
        assert!(!body.contains("http"), "the notice must carry no link");
        assert!(
            !body.to_ascii_lowercase().contains("token"),
            "the notice must carry no token"
        );
    }

    /// Every scheme-bearing URL in `part`, as `scheme://host` prefixes.
    ///
    /// The bare `!contains("http")` check the text part uses CANNOT be applied
    /// to an HTML part: MJML's output legitimately carries
    /// `http://www.w3.org/1999/xhtml` in `xmlns` and `http://schemas.microsoft.com`
    /// in its Outlook conditional blocks. Stripping those substrings, or relaxing
    /// the assertion to "no `?token=`", would both destroy the property the test
    /// exists for — the notice must carry NO navigable link to this product at
    /// all, not merely no link that looks like a credential. So the assertion is
    /// re-expressed against the product's own origin instead of against the
    /// substring `http`.
    ///
    /// This sees ABSOLUTE origins only, by construction. `mailto:`, `//host` and
    /// `/path` are navigable and carry no `://`; they are covered by
    /// [`link_attributes`] instead, which reads the attributes an email client
    /// would actually follow rather than guessing at URL shapes in a blob.
    fn origins(part: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = part;
        while let Some(at) = rest.find("://") {
            // `i + c.len_utf8()`, never `i + 1`: the delimiter before a scheme
            // can be multi-byte (the footer already carries U+2014), and slicing
            // at `i + 1` there is a mid-char boundary panic.
            let scheme_start = rest[..at]
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_ascii_alphanumeric() && *c != '+' && *c != '-' && *c != '.')
                .map_or(0, |(i, c)| i + c.len_utf8());
            let after = &rest[at + 3..];
            let host_end = after
                .find(|c: char| c == '/' || c == '"' || c == '\'' || c == '<' || c.is_whitespace())
                .unwrap_or(after.len());
            out.push(format!(
                "{}://{}",
                &rest[scheme_start..at],
                &after[..host_end]
            ));
            rest = &after[host_end..];
        }
        out
    }

    /// The value of every attribute an email client can navigate or fetch.
    ///
    /// Complements [`origins`], which only sees `scheme://host`. A `mailto:`, a
    /// protocol-relative `//host/x` and a root-relative `/x` are all navigable
    /// and all invisible to a scheme scan; every one of them has to live in one
    /// of these attributes to do anything, so reading the attributes catches the
    /// whole class regardless of URL shape.
    ///
    /// Verified 2026-08-27 against standalone payloads: `mailto:`, `//host/x`,
    /// `/path`, uppercase `SRC=`, `xlink:href=` (which contains `href=`),
    /// `formaction=`, `src="cid:…"` and `mj-section background-url` (MJML emits
    /// that as `background=`) are all caught. Anything carrying `scheme://` is
    /// caught by [`origins`] even outside these four attributes — `srcset`,
    /// `style="background:url(…)"`, a refresh `<meta>`.
    ///
    /// RESIDUAL, recorded rather than closed: a PROTOCOL-RELATIVE `//host` in an
    /// attribute outside these four — `srcset`, `poster`, `data`, a CSS
    /// `@import url("//h")`, a refresh `<meta>` — is blind to both helpers, and
    /// so is a whitespace-padded `src = "//h"` (the needle is `src=`). MJML
    /// emits none of those shapes: the one place it routinely emits a URL is
    /// `background-url`, which arrives as a `background=` attribute and IS
    /// caught. Root-relative URLs in those positions are inert in mail — there
    /// is no base URL to resolve them against. Widening the needle list without
    /// a shape MJML actually emits would be guessing.
    fn link_attributes(part: &str) -> Vec<String> {
        const ATTRIBUTES: [&str; 4] = ["href=", "src=", "background=", "action="];
        let lower = part.to_ascii_lowercase();
        let mut out = Vec::new();
        for attribute in ATTRIBUTES {
            let mut from = 0;
            while let Some(offset) = lower[from..].find(attribute) {
                let at = from + offset + attribute.len();
                let rest = &part[at..];
                let value = match rest.chars().next() {
                    Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or_default(),
                    _ => rest
                        .split(|c: char| c.is_whitespace() || c == '>')
                        .next()
                        .unwrap_or_default(),
                };
                out.push(value.to_owned());
                from = at;
            }
        }
        out
    }

    #[test]
    fn origins_survives_a_multi_byte_char_immediately_before_a_scheme() {
        // `origins` walks BACKWARDS from `://` to find where the scheme starts.
        // Written as `rfind(..).map_or(0, |i| i + 1)` that assumed a one-byte
        // delimiter and sliced mid-character on anything else — and the footer
        // this parser reads already carries U+2014. The parser is test-only, so
        // the blast radius was a panicking test rather than a panicking send,
        // but a panic here destroys the verdict of the assertion it feeds.
        assert_eq!(
            origins("NetRoll by N1CCK—https://evil.example/x"),
            vec!["https://evil.example".to_owned()],
            "a multi-byte delimiter must neither panic nor swallow the scheme"
        );
        assert_eq!(
            origins("plain text, no links here").len(),
            0,
            "text with no scheme yields nothing"
        );
    }

    /// Every generated email template that ships, as `(repo-relative path, bytes)`.
    ///
    /// The SET is derived, never restated. `tools/email/palette.lock.json` is
    /// written by `tools/email/build.mjs` keyed by the destination path of every
    /// entry in its own `TARGETS` list — the same list it compiles from, and the
    /// same list `verify-regeneration.sh` byte-diffs. So a fifth message kind
    /// becomes a fifth key the moment it is built and is swept by whatever reads
    /// this, with no second list here to fall out of step. A hand-written list of
    /// four paths in a test is precisely the shape a fifth template escapes.
    fn generated_email_templates() -> Vec<(String, String)> {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("backend/crates/netroll-adapters sits three levels below the repo root");
        let lock_path = repo.join("tools/email/palette.lock.json");
        let lock = fs::read_to_string(&lock_path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", lock_path.display()));
        let manifest: serde_json::Value =
            serde_json::from_str(&lock).expect("palette.lock.json is JSON");
        let templates = manifest
            .as_object()
            .expect("palette.lock.json is a JSON object keyed by template path");

        templates
            .keys()
            .map(|template| {
                let path = repo.join(template);
                let html = fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
                (template.clone(), html)
            })
            .collect()
    }

    #[test]
    fn no_generated_email_template_ships_a_remote_reference() {
        // The beacon guard, over EVERY generated template rather than one.
        //
        // The narrow version of this assertion lived inside the change-notice
        // test and therefore covered one of the four kinds. That is not a
        // hypothetical gap: the Google Fonts <link> MJML emits by default, which
        // the `fonts: {}` setting exists to suppress, reaches ALL FOUR
        // templates — it was caught incidentally, by the only kind that happened
        // to be asserted. Confined to the magic-link mail it would have shipped,
        // and every recipient's IP would have gone to Google on open. A
        // `<mj-image src="https://tracker.example/px.gif" />` in
        // magic-link.mjml shipped the absolute URL with the whole suite green.
        //
        // The template SOURCE is what is swept, not a rendered message: a remote
        // reference baked in at build time is in these bytes regardless of which
        // runtime values are interpolated, and sweeping the artifact means the
        // set can be derived from the build's own output rather than from a
        // hand-maintained roster of message kinds.
        //
        // EXACT origins, matched with `==`; a `starts_with` over an allowlist
        // passes every prefix of an allowed namespace, `http://` included.
        // Verified 2026-08-27: with `fonts: {}` in build.mjs, w3.org's XHTML
        // namespace is the only `scheme://host` any of the four carries.
        // Microsoft's Outlook schemas are `urn:`-form and carry no `://`.
        const MARKUP_ORIGINS: [&str; 1] = ["http://www.w3.org"];

        /// A link attribute may hold ONE askama expression and nothing else —
        /// a value interpolated at send time. Any literal in a followable
        /// attribute is a build-time-baked destination: absolute, `mailto:`,
        /// protocol-relative `//host` or root-relative `/path` alike.
        fn is_only_a_runtime_expression(value: &str) -> bool {
            let trimmed = value.trim();
            trimmed.len() >= 5
                && trimmed.starts_with("{{")
                && trimmed.ends_with("}}")
                && !trimmed[2..trimmed.len() - 2].contains(['{', '}'])
        }

        let templates = generated_email_templates();

        // POSITIVE CONTROLS, before any negative assertion. A derivation that
        // returned nothing, or files that read as empty, would make every
        // assertion below pass having examined no bytes — the failure mode this
        // whole finding is about.
        assert!(
            templates.len() >= 4,
            "palette.lock.json named {} template(s); the four message kinds are the \
             floor. The EXACT set is pinned by EXPECTED_ARTIFACTS in \
             tools/email/verify-regeneration.sh and EXPECTED_BINDINGS in \
             frontend/src/ui/tokens/palette-projections.test.ts",
            templates.len()
        );

        for (template, html) in &templates {
            assert!(
                html.contains("</html>") && html.len() > 500,
                "{template} does not read as a generated email template ({} bytes) — \
                 the sweep below would have examined nothing",
                html.len()
            );

            for origin in origins(html) {
                assert!(
                    MARKUP_ORIGINS.contains(&origin.as_str()),
                    "{template} carries the remote origin `{origin}` — a generated \
                     email template must reference nothing off-host, or opening the \
                     mail beacons the recipient's IP address to it"
                );
            }

            for value in link_attributes(html) {
                assert!(
                    is_only_a_runtime_expression(&value),
                    "{template} carries the literal followable reference `{value}` — \
                     every href/src/background/action must be an askama expression \
                     interpolated at send time, never a destination baked into the \
                     template"
                );
            }
        }
    }

    #[test]
    fn a_template_render_failure_becomes_a_mail_error_and_never_panics() {
        // Mechanism 4. askama is compile-time-checked, so these templates
        // have no reachable runtime render failure of their own — but
        // `Template::render` is fallible (it writes through `core::fmt::Write`),
        // and the requirement is that whatever comes out of that hole becomes a
        // `MailError` on the send path rather than a panic inside a request
        // task. So the failure is injected at the seam every builder funnels
        // through, which is the code under test.
        let err = rendered(Err(askama::Error::Fmt)).expect_err("a render failure is an error");
        // EQUALITY, not "does not contain the engine's type name". The previous
        // shape asserted `!contains("Fmt")` — and `askama::Error::Fmt`'s Display
        // text does not contain the literal "Fmt", so the assertion could not
        // fail: swapping the static message for `MailError(e.to_string())`, the
        // exact violation it names, left it green. Pinning the whole message
        // is the only form that reds on ANY engine text reaching the error,
        // which is what the sole authentication path requires.
        assert_eq!(
            err.0, "email template failed to render",
            "the error is STATIC — it never surfaces the engine's own text"
        );
        assert!(
            rendered(Ok("<p>ok</p>".to_owned())).is_ok(),
            "a successful render passes through unchanged"
        );
    }

    /// MIME-structure and part-extraction tests. Nested so it can
    /// reuse the fixtures above.
    mod mime {

        use super::*;
        use netroll_domain::ports::MailAttachment;

        /// A transfer-decoded leaf part, labelled by the `/`-joined chain of
        /// Content-Types from the message root down to it. The chain is what makes
        /// nesting assertable: a flattened `mixed[plain, html, csv]` message and a
        /// correctly nested `mixed[alternative[plain, html], csv]` one contain the
        /// same substrings but different chains.
        #[derive(Debug)]
        struct Leaf {
            chain: String,
            body: String,
            /// `Content-Disposition`'s `filename=`, when the part has one — the
            /// marker of an attachment. Carried on the leaf so an attachment can
            /// be asserted to sit where it belongs in the CHAIN, rather than
            /// merely to appear somewhere in the raw blob.
            filename: Option<String>,
        }

        fn header_value(headers: &str, name: &str) -> Option<String> {
            // Headers are ASCII, so the lowercased copy is byte-for-byte
            // index-compatible with the original.
            let padded = format!("\r\n{headers}");
            let lower = padded.to_ascii_lowercase();
            let needle = format!("\r\n{}:", name.to_ascii_lowercase());
            let start = lower.find(&needle)? + needle.len();
            let mut value = String::new();
            for (index, line) in padded[start..].split("\r\n").enumerate() {
                if index == 0 {
                    value.push_str(line.trim());
                } else if line.starts_with(' ') || line.starts_with('\t') {
                    value.push(' ');
                    value.push_str(line.trim());
                } else {
                    break;
                }
            }
            Some(value)
        }

        fn decode_quoted_printable(body: &str) -> String {
            let bytes = body.as_bytes();
            let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'=' && i + 2 < bytes.len() {
                    if bytes[i + 1] == b'\r' && bytes[i + 2] == b'\n' {
                        i += 3; // soft line break
                        continue;
                    }
                    let hex = &body[i + 1..i + 3];
                    if let Ok(byte) = u8::from_str_radix(hex, 16) {
                        out.push(byte);
                        i += 3;
                        continue;
                    }
                }
                out.push(bytes[i]);
                i += 1;
            }
            String::from_utf8_lossy(&out).into_owned()
        }

        fn decode(headers: &str, body: &str) -> String {
            match header_value(headers, "content-transfer-encoding")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "quoted-printable" => decode_quoted_printable(body),
                "base64" => {
                    let joined: String = body.chars().filter(|c| !c.is_whitespace()).collect();
                    use base64::Engine as _;
                    // PANIC, never `unwrap_or_default()`. An empty string here
                    // silently satisfies every NEGATIVE assertion downstream —
                    // `origins("")` is empty and a `for` over it runs zero times
                    // — so a decode failure would turn the notice's
                    // "links nowhere" test into a test of nothing. Unreachable
                    // today (both parts are quoted-printable), reachable the
                    // moment a >998-char line or a non-Latin net title makes
                    // lettre pick base64.
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(joined)
                        .unwrap_or_else(|e| panic!("a base64 part failed to decode: {e}"));
                    String::from_utf8_lossy(&bytes).into_owned()
                }
                _ => body.to_owned(),
            }
        }

        /// `attachment; filename="net-session-20260718.csv"` -> the filename.
        fn attachment_filename(disposition: &str) -> Option<String> {
            let lower = disposition.to_ascii_lowercase();
            let at = lower.find("filename=")? + "filename=".len();
            let rest = disposition[at..].trim();
            Some(
                rest.trim_start_matches('"')
                    .split(['"', ';'])
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            )
        }

        fn mime_type(content_type: &str) -> String {
            content_type
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        }

        fn boundary(content_type: &str) -> Option<String> {
            let lower = content_type.to_ascii_lowercase();
            let at = lower.find("boundary=")? + "boundary=".len();
            let rest = content_type[at..].trim();
            Some(
                rest.trim_start_matches('"')
                    .split(['"', ';'])
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            )
        }

        fn walk(entity: &str, prefix: &str, leaves: &mut Vec<Leaf>) {
            let (headers, body) = match entity.find("\r\n\r\n") {
                Some(at) => (&entity[..at], &entity[at + 4..]),
                None => (entity, ""),
            };
            let content_type =
                header_value(headers, "content-type").unwrap_or_else(|| "text/plain".to_owned());
            let kind = mime_type(&content_type);
            let chain = if prefix.is_empty() {
                kind.clone()
            } else {
                format!("{prefix}/{kind}")
            };

            match boundary(&content_type) {
                Some(marker) if kind.starts_with("multipart/") => {
                    let sep = format!("--{marker}\r\n");
                    let close = format!("--{marker}--");
                    let after_preamble = match body.find(&sep) {
                        Some(at) => &body[at + sep.len()..],
                        None => return,
                    };
                    let inner = match after_preamble.find(&close) {
                        Some(at) => &after_preamble[..at],
                        None => after_preamble,
                    };
                    for segment in inner.split(&format!("--{marker}\r\n")) {
                        let segment = segment.trim_end_matches("\r\n");
                        if segment.is_empty() {
                            continue;
                        }
                        walk(segment, &chain, leaves);
                    }
                }
                _ => leaves.push(Leaf {
                    chain,
                    body: decode(headers, body),
                    filename: header_value(headers, "content-disposition")
                        .as_deref()
                        .and_then(attachment_filename),
                }),
            }
        }

        /// Every transfer-decoded leaf of a formatted message, each labelled with
        /// its full Content-Type chain from the root.
        fn leaves(message: &Message) -> Vec<Leaf> {
            let raw = String::from_utf8_lossy(&message.formatted()).into_owned();
            let mut out = Vec::new();
            walk(&raw, "", &mut out);
            out
        }

        fn chains(message: &Message) -> Vec<String> {
            leaves(message).into_iter().map(|l| l.chain).collect()
        }

        fn part(message: &Message, chain_suffix: &str) -> String {
            leaves(message)
                .into_iter()
                .find(|l| l.chain.ends_with(chain_suffix))
                .unwrap_or_else(|| panic!("no part whose chain ends with `{chain_suffix}`"))
                .body
        }

        const LINK: &str = "http://localhost:5173/auth/verify?token=abc123";

        fn four_kinds() -> Vec<(&'static str, Message)> {
            let from = sender();
            vec![
                (
                    "magic link",
                    magic_link_message(&from, "op@example.com", LINK).expect("build"),
                ),
                (
                    "email change",
                    email_change_message(&from, "new@example.com", LINK).expect("build"),
                ),
                (
                    "email change notice",
                    email_change_notice_message(&from, "old@example.com", "new@example.com")
                        .expect("build"),
                ),
                (
                    "net summary",
                    net_summary_message(&from, "op@example.com", &sample_summary()).expect("build"),
                ),
            ]
        }

        #[test]
        fn every_one_of_the_four_kinds_carries_a_text_and_an_html_alternative() {
            // One test over the list, not four forgettable ones. The list is
            // hand-maintained — see the `trait Mailer` doc comment; a FIFTH message
            // kind added later is invisible here and nothing reds.
            for (kind, message) in four_kinds() {
                let chains = chains(&message);
                assert!(
                    chains.iter().any(|c| c.contains("multipart/alternative")),
                    "{kind}: no multipart/alternative anywhere in the message; chains were {chains:?}"
                );
                assert!(
                    chains
                        .iter()
                        .any(|c| c.contains("multipart/alternative") && c.ends_with("text/plain")),
                    "{kind}: no text/plain inside the alternative; chains were {chains:?}"
                );
                assert!(
                    chains
                        .iter()
                        .any(|c| c.contains("multipart/alternative") && c.ends_with("text/html")),
                    "{kind}: no text/html inside the alternative; chains were {chains:?}"
                );
            }
        }

        #[test]
        fn the_summary_nests_the_alternative_inside_the_mixed_and_keeps_both_attachments() {
            // Asserts the CHAIN, not substring presence: a flattened
            // mixed[plain, html, csv, adi] message contains every one of those
            // strings and is the exact wrong answer this test forbids.
            let summary = NetSummaryMail {
                subject: "s".into(),
                body: "text".into(),
                html_body: "<p>html</p>".into(),
                message_id: "net-session-close.x".into(),
                attachments: vec![
                    MailAttachment {
                        filename: "net-session-20260718.csv".into(),
                        content_type: "text/csv; charset=utf-8".into(),
                        content: "callsign\r\nW1AW\r\n".into(),
                    },
                    MailAttachment {
                        filename: "net-session-20260718.adi".into(),
                        content_type: "text/plain; charset=utf-8".into(),
                        content: "<EOR>\r\n".into(),
                    },
                ],
            };
            let message =
                net_summary_message(&sender(), "op@example.com", &summary).expect("build");
            let chains = chains(&message);

            assert!(
                chains
                    .iter()
                    .any(|c| c == "multipart/mixed/multipart/alternative/text/plain"),
                "the text part must sit inside an alternative inside the mixed; chains were {chains:?}"
            );
            assert!(
                chains
                    .iter()
                    .any(|c| c == "multipart/mixed/multipart/alternative/text/html"),
                "the html part must sit inside an alternative inside the mixed; chains were {chains:?}"
            );

            // The attachments are asserted through the same chain machinery, not
            // as substrings of the raw blob: a substring check passes just as
            // happily when an attachment is wrongly re-parented INSIDE the
            // alternative, where a client would render it as a body candidate
            // instead of offering it as a file.
            for filename in ["net-session-20260718.csv", "net-session-20260718.adi"] {
                let leaf = leaves(&message)
                    .into_iter()
                    .find(|l| l.filename.as_deref() == Some(filename))
                    .unwrap_or_else(|| {
                        panic!("no part is disposed as an attachment named `{filename}`; chains were {chains:?}")
                    });
                assert_eq!(
                    leaf.chain,
                    format!("multipart/mixed/{}", mime_type_of(filename)),
                    "`{filename}` must be a DIRECT child of the mixed — a sibling of the \
                     alternative, not a part inside it"
                );
            }
        }

        /// The Content-Type the summary's two exports go out as, by extension.
        fn mime_type_of(filename: &str) -> &'static str {
            if filename.ends_with(".csv") {
                "text/csv"
            } else {
                "text/plain"
            }
        }

        #[test]
        fn the_text_part_alone_carries_the_magic_link_verbatim_and_the_ttl() {
            // The plain-text part is a first-class output: extracted from the
            // formatted message and transfer-decoded, it must stand on its own.
            // Decoding matters — the link's `=` is quoted-printable-encoded on the
            // wire, so a raw-blob assertion would be testing the wrong bytes.
            let message = magic_link_message(&sender(), "op@example.com", LINK).expect("build");
            let text = part(&message, "multipart/alternative/text/plain");

            assert!(
                text.contains(LINK),
                "the text part must carry the link verbatim, got: {text}"
            );
            assert!(
                text.contains("15 minutes"),
                "TTL note must match MAGIC_LINK_TTL_MILLIS, got: {text}"
            );
            assert!(
                !text.contains("&#38;") && !text.contains("&#60;"),
                "the text part must NOT be HTML-escaped (askama's escaper emits \
                 numeric references), got: {text}"
            );
            // "First-class output, not a stub" also means it is not the HTML
            // part wearing a text/plain header.
            assert!(
                !text.contains("<html") && !text.contains("<div") && !text.contains("<table"),
                "the text part must be text, not markup, got: {text}"
            );
        }

        #[test]
        fn a_multi_parameter_link_survives_the_text_part_and_is_escaped_in_the_html_part() {
            // The concrete failure this guards against, made non-latent. Today's
            // links carry a single `?token=` parameter, so escaping the text part
            // would ship undetected until a second parameter was added — at
            // which point `&` would arrive as `&#38;` and sign-in would break
            // for exactly the text-only recipient the plain part protects.
            //
            // The asymmetry is the requirement: RAW in the text part, escaped in
            // the HTML part (where an unescaped `&` in an href is the bug).
            let link = "http://localhost:5173/auth/verify?token=abc123&next=%2Fnets";
            let message = magic_link_message(&sender(), "op@example.com", link).expect("build");

            let text = part(&message, "multipart/alternative/text/plain");
            assert!(
                text.contains(link),
                "the text part must carry the ampersand raw, got: {text}"
            );

            let html = part(&message, "multipart/alternative/text/html");
            assert!(
                html.contains("token=abc123&#38;next=%2Fnets"),
                "the html part must carry the ampersand escaped, got: {html}"
            );
            assert!(
                !html.contains("token=abc123&next="),
                "an unescaped `&` in an href is the html part's own bug"
            );
        }

        #[test]
        fn both_parts_of_a_kind_carry_the_same_link_and_the_same_ttl() {
            // "Same content" as an assertion rather than a slogan: the shared
            // values, not the prose.
            for (kind, link) in [
                ("magic link", LINK),
                (
                    "email change",
                    "http://localhost:5173/auth/confirm-email-change?token=abc123",
                ),
            ] {
                let message = if kind == "magic link" {
                    magic_link_message(&sender(), "op@example.com", link).expect("build")
                } else {
                    email_change_message(&sender(), "new@example.com", link).expect("build")
                };
                let text = part(&message, "multipart/alternative/text/plain");
                let html = part(&message, "multipart/alternative/text/html");
                assert!(text.contains(link), "{kind}: text part lost the link");
                assert!(html.contains(link), "{kind}: html part lost the link");
                assert!(
                    text.contains("15 minutes"),
                    "{kind}: text part lost the TTL"
                );
                assert!(
                    html.contains("15 minutes"),
                    "{kind}: html part lost the TTL"
                );
            }
        }

        #[test]
        fn both_parts_of_the_change_notice_name_the_new_address() {
            let message =
                email_change_notice_message(&sender(), "old@example.com", "new@example.com")
                    .expect("build");
            let text = part(&message, "multipart/alternative/text/plain");
            let html = part(&message, "multipart/alternative/text/html");
            assert!(text.contains("new@example.com"), "text part: {text}");
            assert!(html.contains("new@example.com"), "html part: {html}");
        }

        #[test]
        fn neither_part_of_the_change_notice_links_back_to_this_product() {
            // Re-expressed at the SAME strength on the HTML part. A link
            // here would be a second single-use credential mailed to the OLD
            // (possibly hostile) mailbox, so the bar is "no navigable link to
            // this product", not "no `?token=`".
            //
            // The only origins the HTML part may carry are the markup namespaces
            // MJML emits — w3.org's XHTML namespace and Microsoft's Outlook
            // conditional schemas. Anything else, product origin or not, reds.
            // Verified 2026-08-26: after `fonts: {}` in tools/email/build.mjs
            // these are the ONLY origins MJML's output carries. This assertion
            // is what caught the Google Fonts <link> MJML emits by default,
            // which would have beaconed every recipient's IP on open.
            // EXACT origins, matched with `==`. The previous form asked whether
            // an allowed NAMESPACE started with the observed origin, which
            // inverts the relation: every prefix of an allowed namespace passed,
            // `http://www.w3.org` and a truncated `http://` alike.
            const MARKUP_ORIGINS: [&str; 1] = ["http://www.w3.org"];
            let message =
                email_change_notice_message(&sender(), "old@example.com", "new@example.com")
                    .expect("build");

            let text = part(&message, "multipart/alternative/text/plain");
            let html = part(&message, "multipart/alternative/text/html");

            // POSITIVE CONTROL, first. Every assertion below is negative, and a
            // negative assertion over an empty string is a test of nothing — an
            // extraction that silently returned "" would make this whole test
            // pass while checking no bytes at all.
            assert!(
                text.contains("new@example.com"),
                "the extracted text part is not the notice: {text}"
            );
            assert!(
                html.contains("new@example.com") && html.contains("</html>"),
                "the extracted html part is not the notice: {html}"
            );

            assert!(
                origins(&text).is_empty(),
                "the text part must carry no URL at all"
            );
            for origin in origins(&html) {
                assert!(
                    MARKUP_ORIGINS.contains(&origin.as_str()),
                    "the html part carries a navigable origin `{origin}` — the \
                     notice must link nowhere"
                );
            }
            // Catches the whole navigable class a scheme scan cannot see:
            // `mailto:`, `//host` and `/path` all have to land in one of these
            // attributes to do anything, and the notice carries none of them.
            assert!(
                link_attributes(&html).is_empty(),
                "the html part carries followable attributes {:?} — the notice \
                 must link nowhere",
                link_attributes(&html)
            );
            assert!(
                !html.to_ascii_lowercase().contains("token"),
                "the html part must carry no token"
            );
        }
    }
}
