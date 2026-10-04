// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Structured net taxonomies, each stored as a lowercase-kebab token and
//! validated in the domain rather than by a DB CHECK, because taxonomies
//! evolve. The variant list is the ONLY place a taxonomy lives, so a new band
//! or category is a one-line addition that never touches the adapter or the
//! HTTP layer.

/// Amateur band the net operates on, as a lowercase-kebab token. A starter
/// band plan (HF/VHF/UHF plus `other`); the list is extensible without an
/// adapter or migration change (domain-validated, no DB CHECK).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// 2200 metres (135.7 kHz LF).
    TwentyTwoHundredMeters,
    /// 630 metres (472 kHz MF).
    SixThirtyMeters,
    /// 160 metres.
    OneSixtyMeters,
    /// 80 metres.
    EightyMeters,
    /// 60 metres.
    SixtyMeters,
    /// 40 metres.
    FortyMeters,
    /// 30 metres.
    ThirtyMeters,
    /// 20 metres.
    TwentyMeters,
    /// 17 metres.
    SeventeenMeters,
    /// 15 metres.
    FifteenMeters,
    /// 12 metres.
    TwelveMeters,
    /// 10 metres.
    TenMeters,
    /// 6 metres.
    SixMeters,
    /// 4 metres.
    FourMeters,
    /// 2 metres.
    TwoMeters,
    /// 1.25 metres (222 MHz).
    OneAndAQuarterMeters,
    /// 70 centimetres.
    SeventyCentimeters,
    /// 33 centimetres.
    ThirtyThreeCentimeters,
    /// 23 centimetres.
    TwentyThreeCentimeters,
    /// Any band not in the starter plan.
    Other,
}

impl Band {
    /// The stable lowercase-kebab wire/storage spelling of this band.
    pub fn as_str(self) -> &'static str {
        match self {
            Band::TwentyTwoHundredMeters => "2200m",
            Band::SixThirtyMeters => "630m",
            Band::OneSixtyMeters => "160m",
            Band::EightyMeters => "80m",
            Band::SixtyMeters => "60m",
            Band::FortyMeters => "40m",
            Band::ThirtyMeters => "30m",
            Band::TwentyMeters => "20m",
            Band::SeventeenMeters => "17m",
            Band::FifteenMeters => "15m",
            Band::TwelveMeters => "12m",
            Band::TenMeters => "10m",
            Band::SixMeters => "6m",
            Band::FourMeters => "4m",
            Band::TwoMeters => "2m",
            Band::OneAndAQuarterMeters => "1.25m",
            Band::SeventyCentimeters => "70cm",
            Band::ThirtyThreeCentimeters => "33cm",
            Band::TwentyThreeCentimeters => "23cm",
            Band::Other => "other",
        }
    }
}

impl TryFrom<&str> for Band {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "2200m" => Ok(Band::TwentyTwoHundredMeters),
            "630m" => Ok(Band::SixThirtyMeters),
            "160m" => Ok(Band::OneSixtyMeters),
            "80m" => Ok(Band::EightyMeters),
            "60m" => Ok(Band::SixtyMeters),
            "40m" => Ok(Band::FortyMeters),
            "30m" => Ok(Band::ThirtyMeters),
            "20m" => Ok(Band::TwentyMeters),
            "17m" => Ok(Band::SeventeenMeters),
            "15m" => Ok(Band::FifteenMeters),
            "12m" => Ok(Band::TwelveMeters),
            "10m" => Ok(Band::TenMeters),
            "6m" => Ok(Band::SixMeters),
            "4m" => Ok(Band::FourMeters),
            "2m" => Ok(Band::TwoMeters),
            "1.25m" => Ok(Band::OneAndAQuarterMeters),
            "70cm" => Ok(Band::SeventyCentimeters),
            "33cm" => Ok(Band::ThirtyThreeCentimeters),
            "23cm" => Ok(Band::TwentyThreeCentimeters),
            "other" => Ok(Band::Other),
            _ => Err(()),
        }
    }
}

/// Emission mode the net runs, as a lowercase-kebab token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Single sideband voice.
    Ssb,
    /// Morse code.
    Cw,
    /// Amplitude modulation voice.
    Am,
    /// Frequency modulation voice.
    Fm,
    /// Any digital mode (FT8, PSK31, packet, …).
    Digital,
    /// Deliberately multi-mode.
    Mixed,
}

impl Mode {
    /// The stable lowercase wire/storage spelling of this mode.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Ssb => "ssb",
            Mode::Cw => "cw",
            Mode::Am => "am",
            Mode::Fm => "fm",
            Mode::Digital => "digital",
            Mode::Mixed => "mixed",
        }
    }
}

impl TryFrom<&str> for Mode {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "ssb" => Ok(Mode::Ssb),
            "cw" => Ok(Mode::Cw),
            "am" => Ok(Mode::Am),
            "fm" => Ok(Mode::Fm),
            "digital" => Ok(Mode::Digital),
            "mixed" => Ok(Mode::Mixed),
            _ => Err(()),
        }
    }
}

/// Purpose/category of the net, as a lowercase-kebab token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetCategory {
    /// Formal message-traffic handling.
    Traffic,
    /// Emergency communications.
    Emergency,
    /// ARES / RACES served-agency nets.
    AresRaces,
    /// DX / long-distance working.
    Dx,
    /// Contest / activity nets.
    Contest,
    /// Informal conversation.
    RagChew,
    /// Technical / elmering.
    Technical,
    /// Swap / for-sale.
    Swap,
    /// Club business.
    Club,
    /// Training / practice.
    Training,
    /// Anything else.
    Other,
}

impl NetCategory {
    /// The stable lowercase-kebab wire/storage spelling of this category.
    pub fn as_str(self) -> &'static str {
        match self {
            NetCategory::Traffic => "traffic",
            NetCategory::Emergency => "emergency",
            NetCategory::AresRaces => "ares-races",
            NetCategory::Dx => "dx",
            NetCategory::Contest => "contest",
            NetCategory::RagChew => "rag-chew",
            NetCategory::Technical => "technical",
            NetCategory::Swap => "swap",
            NetCategory::Club => "club",
            NetCategory::Training => "training",
            NetCategory::Other => "other",
        }
    }
}

impl TryFrom<&str> for NetCategory {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "traffic" => Ok(NetCategory::Traffic),
            "emergency" => Ok(NetCategory::Emergency),
            "ares-races" => Ok(NetCategory::AresRaces),
            "dx" => Ok(NetCategory::Dx),
            "contest" => Ok(NetCategory::Contest),
            "rag-chew" => Ok(NetCategory::RagChew),
            "technical" => Ok(NetCategory::Technical),
            "swap" => Ok(NetCategory::Swap),
            "club" => Ok(NetCategory::Club),
            "training" => Ok(NetCategory::Training),
            "other" => Ok(NetCategory::Other),
            _ => Err(()),
        }
    }
}

/// How the net runs its check-in flow: open free-for-all or a directed
/// roll-call (the "open vs roll-call" distinction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetType {
    /// Open, un-directed check-ins.
    Open,
    /// Directed roll-call: the NCS calls stations in order.
    RollCall,
}

impl NetType {
    /// The stable lowercase-kebab wire/storage spelling of this net type.
    pub fn as_str(self) -> &'static str {
        match self {
            NetType::Open => "open",
            NetType::RollCall => "roll-call",
        }
    }
}

impl TryFrom<&str> for NetType {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "open" => Ok(NetType::Open),
            "roll-call" => Ok(NetType::RollCall),
            _ => Err(()),
        }
    }
}

/// Repeater sub-audible tone scheme, as a lowercase token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneMode {
    /// Continuous Tone-Coded Squelch System (analog PL tone).
    Ctcss,
    /// Digital-Coded Squelch.
    Dcs,
    /// Split tone (different tx/rx tones).
    Split,
}

impl ToneMode {
    /// The stable lowercase wire/storage spelling of this tone mode.
    pub fn as_str(self) -> &'static str {
        match self {
            ToneMode::Ctcss => "ctcss",
            ToneMode::Dcs => "dcs",
            ToneMode::Split => "split",
        }
    }
}

impl TryFrom<&str> for ToneMode {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "ctcss" => Ok(ToneMode::Ctcss),
            "dcs" => Ok(ToneMode::Dcs),
            "split" => Ok(ToneMode::Split),
            _ => Err(()),
        }
    }
}

/// Whether a net is publicly listed in discovery or reachable only by its
/// unguessable link token, as a lowercase token.
/// `Listed` is the default when none is specified — the only
/// net enum carrying a default, so it derives `Default` with `#[default]` on
/// `Listed`. Visibility controls discovery inclusion only; every net gets a
/// link token regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Included in public discovery and reachable by link token.
    #[default]
    Listed,
    /// Excluded from discovery; reachable only by its link token.
    Unlisted,
}

impl Visibility {
    /// The stable lowercase wire/storage spelling of this visibility.
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Listed => "listed",
            Visibility::Unlisted => "unlisted",
        }
    }
}

impl TryFrom<&str> for Visibility {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "listed" => Ok(Visibility::Listed),
            "unlisted" => Ok(Visibility::Unlisted),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_round_trips_every_variant() {
        assert_eq!(Band::TwentyMeters.as_str(), "20m");
        assert_eq!(Band::try_from("20m"), Ok(Band::TwentyMeters));
        assert_eq!(Band::SeventyCentimeters.as_str(), "70cm");
        assert_eq!(Band::try_from("1.25m"), Ok(Band::OneAndAQuarterMeters));
        assert_eq!(Band::try_from("70cm"), Ok(Band::SeventyCentimeters));
        assert_eq!(Band::try_from("other"), Ok(Band::Other));
    }

    #[test]
    fn unknown_and_miscased_band_tokens_are_rejected() {
        assert_eq!(Band::try_from("21m"), Err(()));
        assert_eq!(Band::try_from("20M"), Err(()), "case-sensitive");
        assert_eq!(Band::try_from(""), Err(()));
    }

    #[test]
    fn mode_round_trips_and_rejects_unknown_and_miscased() {
        for (variant, token) in [
            (Mode::Ssb, "ssb"),
            (Mode::Cw, "cw"),
            (Mode::Am, "am"),
            (Mode::Fm, "fm"),
            (Mode::Digital, "digital"),
            (Mode::Mixed, "mixed"),
        ] {
            assert_eq!(variant.as_str(), token);
            assert_eq!(Mode::try_from(token), Ok(variant));
        }
        assert_eq!(Mode::try_from("SSB"), Err(()));
        assert_eq!(Mode::try_from("phone"), Err(()));
    }

    #[test]
    fn net_category_round_trips_and_rejects_unknown() {
        for (variant, token) in [
            (NetCategory::Traffic, "traffic"),
            (NetCategory::Emergency, "emergency"),
            (NetCategory::AresRaces, "ares-races"),
            (NetCategory::Dx, "dx"),
            (NetCategory::Contest, "contest"),
            (NetCategory::RagChew, "rag-chew"),
            (NetCategory::Technical, "technical"),
            (NetCategory::Swap, "swap"),
            (NetCategory::Club, "club"),
            (NetCategory::Training, "training"),
            (NetCategory::Other, "other"),
        ] {
            assert_eq!(variant.as_str(), token);
            assert_eq!(NetCategory::try_from(token), Ok(variant));
        }
        assert_eq!(NetCategory::try_from("ARES-RACES"), Err(()));
        assert_eq!(NetCategory::try_from("ragchew"), Err(()));
    }

    #[test]
    fn net_type_has_exactly_open_and_roll_call() {
        assert_eq!(NetType::Open.as_str(), "open");
        assert_eq!(NetType::RollCall.as_str(), "roll-call");
        assert_eq!(NetType::try_from("open"), Ok(NetType::Open));
        assert_eq!(NetType::try_from("roll-call"), Ok(NetType::RollCall));
        assert_eq!(NetType::try_from("Open"), Err(()), "case-sensitive");
        assert_eq!(NetType::try_from("rollcall"), Err(()));
    }

    #[test]
    fn tone_mode_has_ctcss_dcs_split() {
        assert_eq!(ToneMode::Ctcss.as_str(), "ctcss");
        assert_eq!(ToneMode::Dcs.as_str(), "dcs");
        assert_eq!(ToneMode::Split.as_str(), "split");
        assert_eq!(ToneMode::try_from("ctcss"), Ok(ToneMode::Ctcss));
        assert_eq!(ToneMode::try_from("dcs"), Ok(ToneMode::Dcs));
        assert_eq!(ToneMode::try_from("split"), Ok(ToneMode::Split));
        assert_eq!(ToneMode::try_from("CTCSS"), Err(()));
        assert_eq!(ToneMode::try_from("tone"), Err(()));
    }

    #[test]
    fn visibility_round_trips_and_defaults_to_listed() {
        assert_eq!(Visibility::Listed.as_str(), "listed");
        assert_eq!(Visibility::Unlisted.as_str(), "unlisted");
        assert_eq!(Visibility::try_from("listed"), Ok(Visibility::Listed));
        assert_eq!(Visibility::try_from("unlisted"), Ok(Visibility::Unlisted));
        // Listed is the default when none is specified.
        assert_eq!(Visibility::default(), Visibility::Listed);
    }

    #[test]
    fn unknown_and_miscased_visibility_tokens_are_rejected() {
        assert_eq!(Visibility::try_from("public"), Err(()));
        assert_eq!(Visibility::try_from("private"), Err(()));
        assert_eq!(Visibility::try_from(""), Err(()));
        assert_eq!(Visibility::try_from("Listed"), Err(()), "case-sensitive");
        assert_eq!(Visibility::try_from("LISTED"), Err(()), "case-sensitive");
    }

    #[test]
    fn every_band_variant_round_trips() {
        for (variant, token) in [
            (Band::TwentyTwoHundredMeters, "2200m"),
            (Band::SixThirtyMeters, "630m"),
            (Band::OneSixtyMeters, "160m"),
            (Band::EightyMeters, "80m"),
            (Band::SixtyMeters, "60m"),
            (Band::FortyMeters, "40m"),
            (Band::ThirtyMeters, "30m"),
            (Band::TwentyMeters, "20m"),
            (Band::SeventeenMeters, "17m"),
            (Band::FifteenMeters, "15m"),
            (Band::TwelveMeters, "12m"),
            (Band::TenMeters, "10m"),
            (Band::SixMeters, "6m"),
            (Band::FourMeters, "4m"),
            (Band::TwoMeters, "2m"),
            (Band::OneAndAQuarterMeters, "1.25m"),
            (Band::SeventyCentimeters, "70cm"),
            (Band::ThirtyThreeCentimeters, "33cm"),
            (Band::TwentyThreeCentimeters, "23cm"),
            (Band::Other, "other"),
        ] {
            assert_eq!(variant.as_str(), token);
            assert_eq!(Band::try_from(token), Ok(variant));
        }
    }
}
