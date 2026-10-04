// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The concrete [`LookupProvider`] implementations behind the domain port.
//! Both issue every outbound call through the SHARED [`Egress`] chokepoint,
//! never a second HTTP client, and map any transport failure to
//! [`LookupError`] so the service falls through to the next provider.
//!
//! [`LookupProvider`]: netroll_domain::ports::LookupProvider

pub mod hamcall;
pub mod qrz;

use netroll_domain::egress::EgressError;
use netroll_domain::lookup::LookupError;

/// Maps an [`EgressError`] onto the coarse [`LookupError`] the port speaks.
///
/// Matched EXHAUSTIVELY (no `_` arm) exactly like the delivery
/// `is_transient_egress`: a future `EgressError` variant MUST fail to compile
/// here until a human classifies it, rather than silently defaulting. Every
/// current variant collapses to [`LookupError::Unavailable`] — a lookup failure
/// is best-effort, so the service simply tries the next provider. The
/// `BlockedAddress`/`NotHttps`/`InvalidUrl` refusals should never fire for our
/// fixed HTTPS hosts, but they are mapped (not panicked) all the same.
pub(crate) fn egress_error_to_lookup_error(err: &EgressError) -> LookupError {
    match err {
        EgressError::Timeout
        | EgressError::Transport(_)
        | EgressError::ResponseTooLarge
        | EgressError::TooManyRedirects
        | EgressError::BlockedAddress
        | EgressError::NotHttps
        | EgressError::InvalidUrl => LookupError::Unavailable,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared `Egress` fake for the adapter unit tests — captures every
    //! outbound [`EgressRequest`] and replays a scripted queue of
    //! `Result<EgressResponse, EgressError>` responses in order (mirrors the
    //! `FakeEgress` pattern in `api_on_close_delivery.rs`, but scriptable so the
    //! QRZ login→lookup two-call and re-login sequences can be canned).

    use std::sync::Mutex;

    use netroll_domain::egress::{Egress, EgressError, EgressRequest, EgressResponse};
    use netroll_domain::ports::BoxFuture;

    /// A single scripted egress outcome.
    pub enum Reply {
        /// Return this response body with a given status.
        Ok(u16, Vec<u8>),
        /// Return this transport error.
        Err(EgressError),
    }

    /// Canned-reply, request-capturing [`Egress`] fake.
    pub struct FakeEgress {
        replies: Mutex<std::collections::VecDeque<Reply>>,
        requests: Mutex<Vec<EgressRequest>>,
    }

    impl FakeEgress {
        /// Builds a fake that replays `replies` in order (one per `send`).
        pub fn new(replies: Vec<Reply>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }
        }

        /// A fake that returns one canned 200 JSON/XML body.
        pub fn ok_body(status: u16, body: &str) -> Self {
            Self::new(vec![Reply::Ok(status, body.as_bytes().to_vec())])
        }

        /// Every captured outbound request, in order.
        pub fn requests(&self) -> Vec<EgressRequest> {
            self.requests.lock().expect("lock").clone()
        }

        /// How many outbound calls were issued.
        pub fn call_count(&self) -> usize {
            self.requests.lock().expect("lock").len()
        }
    }

    impl Egress for FakeEgress {
        fn send<'a>(
            &'a self,
            req: EgressRequest,
        ) -> BoxFuture<'a, Result<EgressResponse, EgressError>> {
            Box::pin(async move {
                self.requests.lock().expect("lock").push(req);
                match self.replies.lock().expect("lock").pop_front() {
                    Some(Reply::Ok(status, body)) => Ok(EgressResponse { status, body }),
                    Some(Reply::Err(err)) => Err(err),
                    // Ran off the end of the script — surface loudly.
                    None => Err(EgressError::Transport(
                        "fake egress: no scripted reply".into(),
                    )),
                }
            })
        }
    }
}
