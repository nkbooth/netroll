// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! System implementation of the domain `Clock` port.

use std::time::{SystemTime, UNIX_EPOCH};

use netroll_domain::ports::Clock;

/// Wall-clock time from the OS.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_epoch_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_millis() as u64
    }
}
