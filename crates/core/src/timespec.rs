//! What `--time` and `--frame` read (M1 plan, section 5): a time in clip
//! time, which is 0 at the stream's first presented frame, or a frame
//! number.
//!
//! The grammar, with no spaces, signs or exponents:
//!
//! | Form | Examples | Is |
//! |---|---|---|
//! | seconds, with an optional decimal fraction | `12`, `12.5`, `0.040` | that many seconds |
//! | minutes and seconds | `1:02.5`, `90:00` | 62.5 s, 5400 s |
//! | hours, minutes and seconds | `01:02:03.250`, `100:00:00` | 3723.25 s, 360000 s |
//! | a fraction of seconds | `30000/1001`, `1/2` | 30000/1001 s |
//! | a frame number, with `f` | `370f`, `0f` | frame 370 |
//!
//! Decimals are read exactly, digit by digit, never through `f64`:
//! `12.345` is 12345/1000 s, and `0.1` and `0.2` add up to `0.3`. The
//! field before the first colon has any number of digits; the minutes
//! and seconds after a colon have two, 00 to 59, as a clock shows them,
//! so a typo such as `1:2` or `1:75` is refused rather than guessed at.
//! Negative times are refused: nothing comes before the first frame.
//!
//! **What a frame number means.** Frame `n` is the `n`-th frame the
//! stream presents, counting its first presented frame as 0: the same
//! numbering as a [`FrameTime`](crate::FrameTime)'s `frame`. On a
//! constant-rate stream that is the frame shown at `n` frame durations,
//! and the CLI seeks to (`n` + ½) frame durations, which lies inside
//! frame `n`'s interval even when the container rounds its timestamps
//! (Matroska's milliseconds). A variable-rate stream has no time that
//! names frame `n`, so its frames are counted from the start, whatever
//! their timestamps say: exact, and slower the further in the frame is
//! (`spherecut_media::seek::frame_address`).

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use crate::time::{Rational, Time, TimeError};

/// A time or a frame number, as `--time` gives it (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeSpec {
    /// A clip time, 0 or later.
    Time(Time),
    /// Frame `n`, counting the first presented frame as 0.
    Frame(u64),
}

impl Default for TimeSpec {
    /// The first frame: clip time 0.
    fn default() -> Self {
        TimeSpec::Time(Time::ZERO)
    }
}

/// Why a [`TimeSpec`] doesn't parse. The messages don't repeat the text,
/// since whoever shows them (clap, for one) names it.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum TimeSpecError {
    #[error(
        "expected seconds (12, 12.5), minutes and seconds (1:02.5), hours, minutes and \
         seconds (01:02:03.250), a fraction of seconds (30000/1001) or a frame number (370f)"
    )]
    Syntax,
    #[error("a time can't be negative: the clip starts at 0")]
    Negative,
    #[error("minutes and seconds after a colon are two digits, 00 to 59 (1:02.5, 01:02:03)")]
    Clock,
    #[error("the denominator is zero")]
    ZeroDenominator,
    #[error("too large, or too precise, for an exact time")]
    Overflow,
}

impl From<TimeError> for TimeSpecError {
    fn from(e: TimeError) -> Self {
        match e {
            TimeError::ZeroDenominator => TimeSpecError::ZeroDenominator,
            TimeError::Syntax => TimeSpecError::Syntax,
            TimeError::Overflow | TimeError::NonPositiveRate => TimeSpecError::Overflow,
        }
    }
}

/// `s` as one or more ASCII digits.
fn digits(s: &str) -> Result<&str, TimeSpecError> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        Ok(s)
    } else {
        Err(TimeSpecError::Syntax)
    }
}

/// `digits` as an integer, exactly.
fn integer(s: &str) -> Result<i128, TimeSpecError> {
    digits(s)?.bytes().try_fold(0i128, |n, b| {
        n.checked_mul(10)
            .and_then(|n| n.checked_add(i128::from(b - b'0')))
            .ok_or(TimeSpecError::Overflow)
    })
}

/// Seconds with an optional decimal fraction, `12` or `12.345`, as
/// `(numerator, denominator)`, the denominator a power of ten.
fn decimal(s: &str) -> Result<(i128, i128), TimeSpecError> {
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, digits(f)?),
        None => (s, ""),
    };
    let mut num = integer(whole)?;
    let mut den = 1i128;
    for b in frac.bytes() {
        num = num
            .checked_mul(10)
            .and_then(|n| n.checked_add(i128::from(b - b'0')))
            .ok_or(TimeSpecError::Overflow)?;
        den = den.checked_mul(10).ok_or(TimeSpecError::Overflow)?;
    }
    Ok((num, den))
}

/// A clock field after a colon: two digits, 00 to 59.
fn clock_field(s: &str) -> Result<i128, TimeSpecError> {
    match s.as_bytes() {
        [a @ b'0'..=b'5', b @ b'0'..=b'9'] => Ok(i128::from((a - b'0') * 10 + (b - b'0'))),
        _ => Err(TimeSpecError::Clock),
    }
}

/// `[h:]m:ss[.fff]`, as `(numerator, denominator)` seconds.
fn clock(s: &str) -> Result<(i128, i128), TimeSpecError> {
    let fields: Vec<&str> = s.split(':').collect();
    let (lead, middle, last) = match fields.as_slice() {
        [m, sec] => (*m, None, *sec),
        [h, m, sec] => (*h, Some(*m), *sec),
        _ => return Err(TimeSpecError::Syntax),
    };
    let (sec_whole, sec_frac) = match last.split_once('.') {
        Some((w, f)) => (w, Some(digits(f)?)),
        None => (last, None),
    };
    // A field that isn't digits at all is junk; digits of the wrong
    // count or value are a clock typo.
    let check = |f: &str| digits(f).and_then(clock_field);
    let secs = check(sec_whole)?;
    let lead = integer(lead)?;
    let total_whole = match middle {
        Some(m) => {
            let m = check(m)?;
            lead.checked_mul(3600)
                .and_then(|h| h.checked_add(m * 60))
                .ok_or(TimeSpecError::Overflow)?
        }
        None => lead.checked_mul(60).ok_or(TimeSpecError::Overflow)?,
    };
    let (frac, den) = match sec_frac {
        Some(f) => decimal(&format!("0.{f}"))?,
        None => (0, 1),
    };
    let num = total_whole
        .checked_add(secs)
        .and_then(|w| w.checked_mul(den))
        .and_then(|w| w.checked_add(frac))
        .ok_or(TimeSpecError::Overflow)?;
    Ok((num, den))
}

/// `s`, which has no sign, as a spec.
fn unsigned(s: &str) -> Result<TimeSpec, TimeSpecError> {
    if let Some(n) = s.strip_suffix('f') {
        let n = integer(n)?;
        return u64::try_from(n)
            .map(TimeSpec::Frame)
            .map_err(|_| TimeSpecError::Overflow);
    }
    let (num, den) = if let Some((n, d)) = s.split_once('/') {
        let (n, d) = (integer(n)?, integer(d)?);
        if d == 0 {
            return Err(TimeSpecError::ZeroDenominator);
        }
        (n, d)
    } else if s.contains(':') {
        clock(s)?
    } else {
        decimal(s)?
    };
    Ok(TimeSpec::Time(Time::seconds(Rational::from_i128(
        num, den,
    )?)))
}

impl FromStr for TimeSpec {
    type Err = TimeSpecError;

    /// See the module docs for the grammar.
    fn from_str(s: &str) -> Result<Self, TimeSpecError> {
        match s.strip_prefix('-') {
            // A negative time, when what follows is one (`-0` too: no
            // time takes a sign); junk otherwise.
            Some(rest) => Err(unsigned(rest).err().unwrap_or(TimeSpecError::Negative)),
            None => unsigned(s),
        }
    }
}

impl fmt::Display for TimeSpec {
    /// Text that reads back as the same spec: seconds as an integer or a
    /// fraction (`62.5` is `125/2`), and a frame as `370f`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimeSpec::Time(t) => write!(f, "{}", t.as_rational()),
            TimeSpec::Frame(n) => write!(f, "{n}f"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn secs(n: i64, d: i64) -> TimeSpec {
        TimeSpec::Time(Time::seconds(Rational::new(n, d).unwrap()))
    }

    #[test]
    fn the_plan_s_examples_read_exactly() {
        for (text, want) in [
            ("12", secs(12, 1)),
            ("12.5", secs(25, 2)),
            ("1:02.5", secs(125, 2)),
            ("01:02:03.250", secs(3_723_250, 1000)),
            ("30000/1001", secs(30000, 1001)),
            ("370f", TimeSpec::Frame(370)),
            ("0", TimeSpec::Time(Time::ZERO)),
            ("0f", TimeSpec::Frame(0)),
            ("12.345", secs(12345, 1000)),
            ("0.040", secs(1, 25)),
            ("007", secs(7, 1)),
            ("90:00", secs(5400, 1)),
            ("100:00:00", secs(360_000, 1)),
            ("0:59.999", secs(59_999, 1000)),
            ("2/4", secs(1, 2)),
            // 18 decimals: past f64, and still exact.
            (
                "1.000000000000000001",
                secs(1_000_000_000_000_000_001, 1_000_000_000_000_000_000),
            ),
        ] {
            assert_eq!(text.parse::<TimeSpec>(), Ok(want), "{text}");
        }
    }

    #[test]
    fn decimals_never_go_through_floats() {
        let t = |s: &str| match s.parse::<TimeSpec>() {
            Ok(TimeSpec::Time(t)) => t,
            other => panic!("{s}: {other:?}"),
        };
        assert_eq!(t("0.1").checked_add(t("0.2")).unwrap(), t("0.3"));
        // 0.1 + 0.2 != 0.3 in f64, and 12.345 isn't 12.345 there.
        assert_ne!(0.1f64 + 0.2, 0.3);
        assert_eq!(t("12.345").as_rational(), Rational::new(2469, 200).unwrap());
    }

    #[test]
    fn refusals_say_why() {
        use TimeSpecError::*;
        for (text, want) in [
            ("", Syntax),
            ("-1", Negative),
            ("-1.5", Negative),
            ("-1:00", Negative),
            ("-1/2", Negative),
            ("-3f", Negative),
            ("-0", Negative),
            ("-0.000", Negative),
            ("-x", Syntax),
            ("--1", Syntax),
            ("+1", Syntax),
            (" 1", Syntax),
            ("1 ", Syntax),
            ("1.", Syntax),
            (".5", Syntax),
            ("1.2.3", Syntax),
            ("1e3", Syntax),
            ("0x10", Syntax),
            ("inf", Syntax),
            ("NaN", Syntax),
            ("12s", Syntax),
            ("1.5f", Syntax),
            ("f", Syntax),
            ("1/", Syntax),
            ("/2", Syntax),
            ("1/2.5", Syntax),
            ("1/2/3", Syntax),
            ("1:02/3", Syntax),
            ("1:2", Clock),
            ("1:60", Clock),
            ("1:002", Clock),
            ("1:00:60", Clock),
            ("1:75:00", Clock),
            ("1:0x", Syntax),
            (":30", Syntax),
            ("1:", Syntax),
            ("1:30:", Syntax),
            ("1:02:03:04", Syntax),
            ("1:02.", Syntax),
            ("١٢", Syntax),
            ("1/0", ZeroDenominator),
            ("99999999999999999999999999999999999999999", Overflow),
            ("18446744073709551616f", Overflow),
            ("9223372036854775808", Overflow),
            // 10^22 doesn't fit a denominator.
            ("0.0000000000000000000001", Overflow),
        ] {
            assert_eq!(text.parse::<TimeSpec>(), Err(want), "{text:?}");
        }
        assert!(Negative.to_string().contains("can't be negative"));
    }

    #[test]
    fn display_reads_back() {
        for text in ["12", "12.5", "1:02.5", "01:02:03.250", "30000/1001", "370f"] {
            let spec: TimeSpec = text.parse().unwrap();
            assert_eq!(spec.to_string().parse::<TimeSpec>(), Ok(spec), "{text}");
        }
        assert_eq!("1:02.5".parse::<TimeSpec>().unwrap().to_string(), "125/2");
    }

    proptest! {
        #[test]
        fn decimals_are_exact(whole in 0u64..=9_999_999_999, frac in "[0-9]{0,18}") {
            let text = if frac.is_empty() { whole.to_string() } else { format!("{whole}.{frac}") };
            let den = 10i128.pow(frac.len() as u32);
            let num = i128::from(whole) * den + frac.parse::<i128>().unwrap_or(0);
            // The exact fraction, or overflow when its reduced form doesn't
            // fit 64 bits: never a rounded one.
            let want = Rational::from_i128(num, den)
                .map(|r| TimeSpec::Time(Time::seconds(r)))
                .map_err(TimeSpecError::from);
            prop_assert_eq!(text.parse::<TimeSpec>(), want);
        }

        #[test]
        fn clocks_add_up(h in 0u32..10_000, m in 0u32..60, s in 0u32..60,
                         frac in "[0-9]{0,9}", hours in any::<bool>()) {
            let tail = if frac.is_empty() { String::new() } else { format!(".{frac}") };
            let (text, whole) = if hours {
                (format!("{h}:{m:02}:{s:02}{tail}"), i128::from(h) * 3600 + i128::from(m) * 60)
            } else {
                // Minutes before the first colon may pass 59.
                let minutes = h * 60 + m;
                (format!("{minutes}:{s:02}{tail}"), i128::from(minutes) * 60)
            };
            let den = 10i128.pow(frac.len() as u32);
            let num = (whole + i128::from(s)) * den + frac.parse::<i128>().unwrap_or(0);
            let want = Rational::from_i128(num, den).unwrap();
            prop_assert_eq!(text.parse::<TimeSpec>(), Ok(TimeSpec::Time(Time::seconds(want))));
            // And a field past 59 after a colon is a clock typo.
            let bad = if hours { format!("{h}:{:02}:{s:02}", m + 60) } else { format!("{h}:{}", s + 60) };
            prop_assert_eq!(bad.parse::<TimeSpec>(), Err(TimeSpecError::Clock));
        }

        #[test]
        fn fractions_are_exact(n in 0i64..=i64::MAX, d in 1i64..=i64::MAX) {
            let want = Rational::new(n, d).unwrap();
            prop_assert_eq!(
                format!("{n}/{d}").parse::<TimeSpec>(),
                Ok(TimeSpec::Time(Time::seconds(want)))
            );
        }

        #[test]
        fn frame_numbers_read(n in any::<u64>()) {
            prop_assert_eq!(format!("{n}f").parse::<TimeSpec>(), Ok(TimeSpec::Frame(n)));
            prop_assert_eq!(TimeSpec::Frame(n).to_string().parse::<TimeSpec>(), Ok(TimeSpec::Frame(n)));
        }

        #[test]
        fn negatives_are_refused(n in 1u64..=1 << 60, form in 0u8..4) {
            let text = match form {
                0 => format!("-{n}"),
                1 => format!("-{n}.5"),
                2 => format!("-{n}/7"),
                _ => format!("-{n}f"),
            };
            prop_assert_eq!(text.parse::<TimeSpec>(), Err(TimeSpecError::Negative));
        }

        #[test]
        fn junk_is_refused(s in "\\PC{0,24}") {
            // Anything parses or is refused, without a panic, and what
            // parses is in the grammar's alphabet and never negative.
            match s.parse::<TimeSpec>() {
                Ok(TimeSpec::Time(t)) => {
                    prop_assert!(t >= Time::ZERO);
                    prop_assert!(s.bytes().all(|b| b.is_ascii_digit() || b".:/".contains(&b)), "{s:?}");
                }
                Ok(TimeSpec::Frame(_)) => {
                    prop_assert!(s.ends_with('f') && s[..s.len() - 1].bytes().all(|b| b.is_ascii_digit()));
                }
                Err(_) => {}
            }
        }

        #[test]
        fn a_letter_anywhere_is_refused(n in 0u32..100_000, at in 0usize..6, c in "[a-eg-zA-Z !,;+]") {
            let mut text = format!("{n}.{n}");
            let at = at.min(text.len());
            text.insert_str(at, &c);
            prop_assert!(text.parse::<TimeSpec>().is_err(), "{text:?}");
        }
    }
}
