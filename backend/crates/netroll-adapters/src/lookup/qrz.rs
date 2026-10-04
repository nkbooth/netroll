// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! QRZ.com XML callbook adapter, issued through the shared SSRF-safe
//! [`Egress`]. Session keys have NO fixed expiry, so this is lazy
//! re-login-on-error rather than a timer, and parsing is TOLERANT per the
//! spec. Username, password and key are secret-equivalent: no logs, the login
//! URL is never surfaced, and [`SessionKey`] has a redacting `Debug`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use netroll_domain::egress::{Egress, EgressMethod, EgressRequest};
use netroll_domain::lookup::{CallsignRecord, LookupError, LookupSource, join_fields, normalize};
use netroll_domain::ports::{BoxFuture, LookupProvider};
use netroll_domain::qrz::QrzCredentials;
use quick_xml::Reader;
use quick_xml::events::Event;
use url::Url;

use super::egress_error_to_lookup_error;

/// The QRZ XML data-port base. `current` selects the latest interface (a bare
/// URL falls back to legacy 1.24).
const QRZ_BASE: &str = "https://xmldata.qrz.com/xml/current/";

/// A cached QRZ session key. Secret-equivalent (it grants callbook access on
/// our subscription), so its `Debug` is redacting — a stray `{:?}` can never
/// leak it into a log or trace, mirroring `QrzUsername`.
#[derive(Clone)]
struct SessionKey(String);

impl core::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SessionKey(<redacted>)")
    }
}

/// What a single callsign-lookup exchange resolved to, before the re-login
/// policy is applied.
enum LookupOutcome {
    /// A callbook hit.
    Record(Box<CallsignRecord>),
    /// A definitive "not found" (the session is still valid).
    NotFound,
    /// The session key was rejected (`Session Timeout` / `Invalid session key`)
    /// — invalidate and re-login once.
    SessionExpired,
    /// Any other provider failure.
    Failed,
}

/// The QRZ.com XML callbook provider with a per-username session-key cache.
pub struct QrzLookupProvider {
    egress: Arc<dyn Egress + Send + Sync>,
    /// Cached session keys, keyed by QRZ username. Lazily populated on login and
    /// reused across lookups; invalidated only when a response rejects the key.
    /// The seam a future Valkey-backed shared key cache would swap in.
    sessions: Mutex<HashMap<String, SessionKey>>,
    /// The `agent=` identifier QRZ recommends (client name + version).
    agent: String,
}

impl QrzLookupProvider {
    /// Wires the provider onto the shared egress client. The `agent` string is
    /// `NetRoll/<crate-version>`.
    pub fn new(egress: Arc<dyn Egress + Send + Sync>) -> Self {
        Self {
            egress,
            sessions: Mutex::new(HashMap::new()),
            agent: format!("NetRoll/{}", env!("CARGO_PKG_VERSION")),
        }
    }

    fn cached_key(&self, username: &str) -> Option<SessionKey> {
        self.sessions.lock().expect("lock").get(username).cloned()
    }

    fn store_key(&self, username: &str, key: SessionKey) {
        self.sessions
            .lock()
            .expect("lock")
            .insert(username.to_owned(), key);
    }

    fn invalidate(&self, username: &str) {
        self.sessions.lock().expect("lock").remove(username);
    }

    /// Logs in and returns a fresh session key, caching it under the username.
    async fn login(&self, creds: &QrzCredentials) -> Result<SessionKey, LookupError> {
        let url = Url::parse_with_params(
            QRZ_BASE,
            &[
                ("username", creds.username.as_str()),
                ("password", creds.password.as_str()),
                ("agent", self.agent.as_str()),
            ],
        )
        .map_err(|_| LookupError::Unavailable)?;

        let fields = self.get_and_parse(url.as_str()).await?;

        if let Some(key) = normalize(fields.get("Key").map(String::as_str)) {
            let key = SessionKey(key);
            self.store_key(creds.username.as_str(), key.clone());
            return Ok(key);
        }

        // No <Key> ⇒ login failure. Only a wrong-credentials message is
        // InvalidCredentials (no retry); everything else is Unavailable.
        match fields.get("Error") {
            Some(err) if is_invalid_credentials(err) => Err(LookupError::InvalidCredentials),
            _ => Err(LookupError::Unavailable),
        }
    }

    /// Runs one `?s=&callsign=` exchange against a given key.
    async fn lookup_once(
        &self,
        key: &SessionKey,
        callsign: &str,
    ) -> Result<LookupOutcome, LookupError> {
        let url =
            Url::parse_with_params(QRZ_BASE, &[("s", key.0.as_str()), ("callsign", callsign)])
                .map_err(|_| LookupError::Unavailable)?;

        let fields = self.get_and_parse(url.as_str()).await?;
        Ok(interpret_lookup(&fields, callsign))
    }

    /// Issues a GET and parses the XML body into a flat local-name→text map.
    async fn get_and_parse(&self, url: &str) -> Result<HashMap<String, String>, LookupError> {
        let response = self
            .egress
            .send(EgressRequest {
                method: EgressMethod::Get,
                url: url.to_owned(),
                headers: Vec::new(),
                body: None,
            })
            .await
            .map_err(|err| egress_error_to_lookup_error(&err))?;

        // QRZ answers 200 even for auth/lookup errors (in the XML body); a
        // non-2xx is an infrastructure fault → Unavailable.
        if !(200..300).contains(&response.status) {
            return Err(LookupError::Unavailable);
        }
        parse_xml_fields(&response.body)
    }
}

impl LookupProvider for QrzLookupProvider {
    fn lookup<'a>(
        &'a self,
        callsign: &'a str,
        credentials: Option<&'a QrzCredentials>,
    ) -> BoxFuture<'a, Result<Option<CallsignRecord>, LookupError>> {
        Box::pin(async move {
            // QRZ requires a login; with no credentials it cannot answer, so the
            // service falls through to hamcall.
            let creds = credentials.ok_or(LookupError::Unavailable)?;
            let username = creds.username.as_str();

            // Reuse a cached key when present; otherwise log in once.
            let key = match self.cached_key(username) {
                Some(key) => key,
                None => self.login(creds).await?,
            };

            match self.lookup_once(&key, callsign).await? {
                LookupOutcome::Record(record) => Ok(Some(*record)),
                LookupOutcome::NotFound => Ok(None),
                LookupOutcome::Failed => Err(LookupError::Unavailable),
                LookupOutcome::SessionExpired => {
                    // Lazy re-login: invalidate, log in ONCE, retry ONCE.
                    self.invalidate(username);
                    let fresh = self.login(creds).await?;
                    match self.lookup_once(&fresh, callsign).await? {
                        LookupOutcome::Record(record) => Ok(Some(*record)),
                        LookupOutcome::NotFound => Ok(None),
                        LookupOutcome::SessionExpired | LookupOutcome::Failed => {
                            Err(LookupError::Unavailable)
                        }
                    }
                }
            }
        })
    }
}

/// Whether a QRZ login `<Error>` text is a wrong-credentials rejection.
fn is_invalid_credentials(error: &str) -> bool {
    let lowered = error.to_ascii_lowercase();
    lowered.contains("username/password incorrect") || lowered.contains("password incorrect")
}

/// Classifies a parsed callsign-lookup response.
fn interpret_lookup(fields: &HashMap<String, String>, queried: &str) -> LookupOutcome {
    if let Some(error) = fields.get("Error") {
        let lowered = error.to_ascii_lowercase();
        if lowered.contains("not found") {
            return LookupOutcome::NotFound;
        }
        if lowered.contains("session timeout") || lowered.contains("invalid session key") {
            return LookupOutcome::SessionExpired;
        }
        return LookupOutcome::Failed;
    }

    // A hit carries a `<Callsign>` block; `<call>` is its anchor field.
    if fields.contains_key("call")
        || fields.contains_key("fname")
        || fields.contains_key("name_fmt")
    {
        return LookupOutcome::Record(Box::new(build_record(fields, queried)));
    }

    // No error, no callsign fields (e.g. a bare Session/Message) — nothing to
    // autofill; treat as a definitive miss rather than a failure.
    LookupOutcome::NotFound
}

/// Projects the parsed QRZ `<Callsign>` fields onto a [`CallsignRecord`].
fn build_record(fields: &HashMap<String, String>, queried: &str) -> CallsignRecord {
    let get = |k: &str| fields.get(k).map(String::as_str);
    CallsignRecord {
        callsign: normalize(get("call")).unwrap_or_else(|| queried.to_uppercase()),
        name: normalize(get("name_fmt")).or_else(|| join_fields(get("fname"), get("name"), " ")),
        location: join_fields(get("addr2"), get("state"), ", ")
            .or_else(|| normalize(get("country"))),
        grid: normalize(get("grid")),
        source: LookupSource::Qrz,
    }
}

/// Parses a QRZ XML body into a flat `local-element-name → text` map, TOLERANTLY
/// (spec-mandated): matches LOCAL element names, ignores the default `xmlns`
/// namespace and any unknown/extra nodes, makes no ordering assumptions, and
/// never errors on an unexpected element. A genuinely malformed body (not
/// well-formed XML) maps to [`LookupError::Unavailable`].
fn parse_xml_fields(body: &[u8]) -> Result<HashMap<String, String>, LookupError> {
    let text = String::from_utf8_lossy(body);
    let mut reader = Reader::from_str(&text);
    let mut fields: HashMap<String, String> = HashMap::new();
    let mut current: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = local_name(e.local_name().into_inner());
                // Reset (not append) on each new Start of a given local name:
                // the spec warns "make no assumptions regarding the number of
                // nodes returned", so a document could repeat an element name
                // at unrelated points (e.g. `<Session>`'s own fields vs a
                // future/unknown node). Blindly appending would silently
                // concatenate two distinct elements' text into one corrupted
                // string; resetting here makes the LAST occurrence win instead
                // — still correct for the common case (each name appears once)
                // and for a name split across multiple `Text` events within
                // ONE element (e.g. an entity reference), which the `Text` arm
                // below still accumulates via `push_str`.
                fields.insert(name.clone(), String::new());
                current = Some(name);
            }
            Ok(Event::Text(e)) => {
                if let Some(name) = current.as_ref() {
                    // quick-xml 0.41 split the old `BytesText::unescape()` into
                    // `decode()` (charset -> str) + the free `escape::unescape()`
                    // (entity refs), so both are needed to match prior behavior.
                    let text = e.decode().map_err(|_| LookupError::Unavailable)?;
                    let decoded =
                        quick_xml::escape::unescape(&text).map_err(|_| LookupError::Unavailable)?;
                    fields.entry(name.clone()).or_default().push_str(&decoded);
                }
            }
            Ok(Event::End(_)) => current = None,
            Ok(Event::Eof) => break,
            // Tolerant: skip empties, comments, CDATA-as-text, declarations, etc.
            Ok(_) => {}
            Err(_) => return Err(LookupError::Unavailable),
        }
    }
    Ok(fields)
}

/// Decodes a local element name (namespace prefix already stripped by
/// `local_name()`), lossily, into an owned `String`.
fn local_name(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{FakeEgress, Reply};
    use super::*;
    use netroll_domain::egress::EgressError;
    use netroll_domain::qrz::{parse_qrz_password, parse_qrz_username};

    const TEST_KEY: &str = "SESSIONKEYABCDEF0123456789";
    const TEST_PASSWORD: &str = "hunter2topsecret";
    const TEST_USERNAME: &str = "W1AWLOGIN";

    fn creds() -> QrzCredentials {
        QrzCredentials {
            username: parse_qrz_username(TEST_USERNAME).unwrap(),
            password: parse_qrz_password(TEST_PASSWORD).unwrap(),
        }
    }

    fn login_ok() -> Reply {
        Reply::Ok(
            200,
            format!(
                r#"<?xml version="1.0"?>
                <QRZDatabase version="1.34" xmlns="http://xmldata.qrz.com">
                  <Session><Key>{TEST_KEY}</Key><Count>123</Count></Session>
                </QRZDatabase>"#
            )
            .into_bytes(),
        )
    }

    fn hit_body() -> Reply {
        Reply::Ok(
            200,
            br#"<?xml version="1.0"?>
            <QRZDatabase version="1.34" xmlns="http://xmldata.qrz.com">
              <Callsign>
                <call>AA7BQ</call><fname>FRED L</fname><name>LLOYD</name>
                <addr2>SCOTTSDALE</addr2><state>AZ</state>
                <country>United States</country><grid>DM32af</grid>
              </Callsign>
              <Session><Key>SESSIONKEYABCDEF0123456789</Key></Session>
            </QRZDatabase>"#
                .to_vec(),
        )
    }

    #[tokio::test]
    async fn login_then_hit_returns_the_mapped_record() {
        let egress = Arc::new(FakeEgress::new(vec![login_ok(), hit_body()]));
        let provider = QrzLookupProvider::new(egress.clone());

        let record = provider
            .lookup("aa7bq", Some(&creds()))
            .await
            .expect("no error")
            .expect("a hit");

        assert_eq!(record.callsign, "AA7BQ");
        assert_eq!(record.name.as_deref(), Some("FRED L LLOYD"));
        assert_eq!(record.location.as_deref(), Some("SCOTTSDALE, AZ"));
        assert_eq!(record.grid.as_deref(), Some("DM32af"));
        assert_eq!(record.source, LookupSource::Qrz);

        // Login + lookup = two outbound calls; the login carried the agent.
        assert_eq!(egress.call_count(), 2);
        assert!(egress.requests()[0].url.contains("agent=NetRoll"));
    }

    #[tokio::test]
    async fn a_cached_key_is_reused_so_a_second_lookup_does_not_re_login() {
        let egress = Arc::new(FakeEgress::new(vec![login_ok(), hit_body(), hit_body()]));
        let provider = QrzLookupProvider::new(egress.clone());

        provider.lookup("AA7BQ", Some(&creds())).await.unwrap();
        assert_eq!(egress.call_count(), 2, "first lookup logs in then queries");

        provider.lookup("AA7BQ", Some(&creds())).await.unwrap();
        // Only ONE additional call — the cached key is reused, no re-login.
        assert_eq!(
            egress.call_count(),
            3,
            "second lookup reuses the cached key (one call, not a re-login)"
        );
    }

    #[tokio::test]
    async fn an_expired_key_triggers_one_relogin_and_one_retry() {
        let expired = Reply::Ok(
            200,
            br#"<QRZDatabase xmlns="http://xmldata.qrz.com"><Session>
                <Error>Session Timeout</Error></Session></QRZDatabase>"#
                .to_vec(),
        );
        let egress = Arc::new(FakeEgress::new(vec![
            login_ok(), // initial login
            expired,    // first lookup: key rejected
            login_ok(), // re-login
            hit_body(), // retry succeeds
        ]));
        let provider = QrzLookupProvider::new(egress.clone());

        let record = provider
            .lookup("AA7BQ", Some(&creds()))
            .await
            .expect("no error")
            .expect("retry hit");

        assert_eq!(record.callsign, "AA7BQ");
        assert_eq!(egress.call_count(), 4, "login, fail, re-login, retry");
    }

    #[tokio::test]
    async fn not_found_is_a_definitive_miss_and_keeps_the_key() {
        let not_found = Reply::Ok(
            200,
            br#"<QRZDatabase xmlns="http://xmldata.qrz.com"><Session>
                <Error>Not found: G1SRDD</Error>
                <Key>SESSIONKEYABCDEF0123456789</Key></Session></QRZDatabase>"#
                .to_vec(),
        );
        let egress = Arc::new(FakeEgress::new(vec![login_ok(), not_found]));
        let provider = QrzLookupProvider::new(egress.clone());

        let result = provider
            .lookup("G1SRDD", Some(&creds()))
            .await
            .expect("no error");

        assert_eq!(result, None);
        // No re-login was attempted (the key is still valid on a not-found).
        assert_eq!(egress.call_count(), 2);
    }

    #[tokio::test]
    async fn bad_credentials_map_to_invalid_credentials_without_retry() {
        let bad = Reply::Ok(
            200,
            br#"<QRZDatabase xmlns="http://xmldata.qrz.com"><Session>
                <Error>Username/password incorrect</Error></Session></QRZDatabase>"#
                .to_vec(),
        );
        let egress = Arc::new(FakeEgress::new(vec![bad]));
        let provider = QrzLookupProvider::new(egress.clone());

        let err = provider
            .lookup("AA7BQ", Some(&creds()))
            .await
            .expect_err("wrong credentials must surface");

        assert_eq!(err, LookupError::InvalidCredentials);
        // Exactly one call — no retry loop on bad credentials.
        assert_eq!(egress.call_count(), 1);
    }

    #[tokio::test]
    async fn malformed_xml_maps_to_unavailable() {
        let garbage = Reply::Ok(200, b"<QRZDatabase><Session><Key>oops".to_vec());
        let egress = Arc::new(FakeEgress::new(vec![garbage]));
        let provider = QrzLookupProvider::new(egress);

        let err = provider
            .lookup("AA7BQ", Some(&creds()))
            .await
            .expect_err("a truncated body is a failure");

        assert_eq!(err, LookupError::Unavailable);
    }

    #[test]
    fn repeated_element_names_do_not_concatenate_across_occurrences() {
        // The spec warns "make no assumptions
        // regarding the number of nodes returned" — a document could repeat a
        // local name at unrelated points. Blindly appending would corrupt
        // `<Error>` (and therefore the retry/InvalidCredentials classification)
        // by silently gluing two distinct messages together.
        let body = b"<QRZDatabase><Session><Error>first</Error></Session>\
                      <Extra><Error>second</Error></Extra></QRZDatabase>";
        let fields = parse_xml_fields(body).expect("well-formed XML");
        assert_eq!(fields.get("Error").map(String::as_str), Some("second"));
    }

    #[tokio::test]
    async fn a_transport_error_degrades_to_unavailable() {
        let egress = Arc::new(FakeEgress::new(vec![Reply::Err(EgressError::Timeout)]));
        let provider = QrzLookupProvider::new(egress);

        let err = provider
            .lookup("AA7BQ", Some(&creds()))
            .await
            .expect_err("a timeout is a failure");

        assert_eq!(err, LookupError::Unavailable);
    }

    #[tokio::test]
    async fn missing_credentials_makes_qrz_unavailable_so_the_service_falls_through() {
        let egress = Arc::new(FakeEgress::new(vec![]));
        let provider = QrzLookupProvider::new(egress.clone());

        let err = provider
            .lookup("AA7BQ", None)
            .await
            .expect_err("QRZ cannot answer without a login");

        assert_eq!(err, LookupError::Unavailable);
        // Never touched the network.
        assert_eq!(egress.call_count(), 0);
    }

    #[test]
    fn session_key_debug_is_redacting() {
        let rendered = format!("{:?}", SessionKey(TEST_KEY.to_owned()));
        assert!(
            !rendered.contains(TEST_KEY),
            "the session key must never render"
        );
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn login_url_carries_percent_encoded_credentials_but_is_never_logged() {
        // Documents the secrecy boundary: the login URL DOES carry the password
        // (percent-encoded) and therefore must never reach a log — the adapter
        // emits none. Here we only assert the encoding is correct.
        let creds = QrzCredentials {
            username: parse_qrz_username("W1AW").unwrap(),
            password: parse_qrz_password("p@ss word&;=").unwrap(),
        };
        let url = Url::parse_with_params(
            QRZ_BASE,
            &[
                ("username", creds.username.as_str()),
                ("password", creds.password.as_str()),
                ("agent", "NetRoll/test"),
            ],
        )
        .unwrap();
        // The raw special characters must be encoded, not literal.
        assert!(url.as_str().contains("password=p%40ss+word%26%3B%3D"));
    }
}
