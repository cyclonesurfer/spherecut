//! Exact time.
//!
//! Every position on a timeline, every stream timestamp and every frame rate
//! is a ratio of integers. Floating point appears only at the very edges (UI
//! display, audio resampling ratios) and never in stored state.
//!
//! * [`Rational`] is a reduced fraction with `i64` numerator and a positive
//!   `i64` denominator. Arithmetic is done in `i128` and reduced, and fails
//!   with [`TimeError::Overflow`] instead of wrapping.
//! * [`Time`] is a duration or position in seconds, stored as a [`Rational`].
//! * [`FrameRate`] is frames per second, e.g. `30000/1001` for 29.97.
//! * A stream timestamp is a `pts` integer plus a timebase [`Rational`]
//!   (seconds per tick), converted with [`Time::from_pts`] / [`Time::to_pts`].
//! * [`FrameTime`] is the time of a frame of a video stream, with the
//!   frame's index when it is known.
//!
//! Serialized, as in the CLI's JSON (DECISIONS D27), a [`Rational`] (and a
//! [`FrameRate`]) is a string: `"30000/1001"`, or `"25"` for an integer. A
//! [`Time`] is `{"exact": "1001/30000", "seconds": 0.0333…}` and a
//! [`FrameTime`] adds `"frame"` when it has an index (null reads as none).
//! `exact` reads back to the same value; `seconds` is for readers that want
//! a number, and is ignored when read.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum TimeError {
    #[error("denominator is zero")]
    ZeroDenominator,
    #[error("arithmetic overflow in exact time computation")]
    Overflow,
    #[error("frame rate must be positive")]
    NonPositiveRate,
    #[error("expected an integer or a fraction n/d, such as 30000/1001")]
    Syntax,
}

/// How to map a time that falls between two ticks onto an integer tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounding {
    /// Towards negative infinity.
    Floor,
    /// Towards positive infinity.
    Ceil,
    /// To the nearest tick, halves away from zero.
    Nearest,
}

/// A reduced fraction `num / den` with `den > 0`. It is serialized as the
/// string [`Display`](fmt::Display) writes, which [`FromStr`] reads back.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    num: i64,
    den: i64,
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

fn div_round(n: i128, d: i128, rounding: Rounding) -> i128 {
    debug_assert!(d > 0);
    let q = n.div_euclid(d);
    let r = n.rem_euclid(d);
    match rounding {
        Rounding::Floor => q,
        Rounding::Ceil => {
            if r == 0 {
                q
            } else {
                q + 1
            }
        }
        Rounding::Nearest => {
            // Ties away from zero, so -0.5 rounds to -1 and 0.5 to 1.
            let twice = 2 * r;
            match twice.cmp(&d) {
                Ordering::Less => q,
                Ordering::Greater => q + 1,
                Ordering::Equal => {
                    if n < 0 {
                        q
                    } else {
                        q + 1
                    }
                }
            }
        }
    }
}

impl Rational {
    pub const ZERO: Rational = Rational { num: 0, den: 1 };
    pub const ONE: Rational = Rational { num: 1, den: 1 };

    /// Build and reduce `num / den`.
    pub fn new(num: i64, den: i64) -> Result<Self, TimeError> {
        Self::from_i128(num as i128, den as i128)
    }

    pub const fn from_integer(n: i64) -> Self {
        Rational { num: n, den: 1 }
    }

    fn from_i128(num: i128, den: i128) -> Result<Self, TimeError> {
        if den == 0 {
            return Err(TimeError::ZeroDenominator);
        }
        let g = gcd(num, den).max(1);
        let (mut n, mut d) = (num / g, den / g);
        if d < 0 {
            n = -n;
            d = -d;
        }
        let num = i64::try_from(n).map_err(|_| TimeError::Overflow)?;
        let den = i64::try_from(d).map_err(|_| TimeError::Overflow)?;
        Ok(Rational { num, den })
    }

    pub const fn num(self) -> i64 {
        self.num
    }

    pub const fn den(self) -> i64 {
        self.den
    }

    pub fn is_zero(self) -> bool {
        self.num == 0
    }

    pub fn is_positive(self) -> bool {
        self.num > 0
    }

    pub fn checked_add(self, o: Rational) -> Result<Rational, TimeError> {
        let n = self.num as i128 * o.den as i128 + o.num as i128 * self.den as i128;
        let d = self.den as i128 * o.den as i128;
        Self::from_i128(n, d)
    }

    pub fn checked_sub(self, o: Rational) -> Result<Rational, TimeError> {
        let n = self.num as i128 * o.den as i128 - o.num as i128 * self.den as i128;
        let d = self.den as i128 * o.den as i128;
        Self::from_i128(n, d)
    }

    pub fn checked_neg(self) -> Result<Rational, TimeError> {
        Self::from_i128(-(self.num as i128), self.den as i128)
    }

    pub fn checked_mul(self, o: Rational) -> Result<Rational, TimeError> {
        Self::from_i128(
            self.num as i128 * o.num as i128,
            self.den as i128 * o.den as i128,
        )
    }

    pub fn checked_div(self, o: Rational) -> Result<Rational, TimeError> {
        Self::from_i128(
            self.num as i128 * o.den as i128,
            self.den as i128 * o.num as i128,
        )
    }

    pub fn recip(self) -> Result<Rational, TimeError> {
        Self::from_i128(self.den as i128, self.num as i128)
    }

    /// Round to an integer.
    pub fn round(self, rounding: Rounding) -> i64 {
        // |num / den| <= |num| always fits in i64.
        div_round(self.num as i128, self.den as i128, rounding) as i64
    }

    /// Lossy conversion for display and for interfaces that need floats.
    pub fn to_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Rational {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.num as i128 * other.den as i128).cmp(&(other.num as i128 * self.den as i128))
    }
}

impl fmt::Debug for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

impl TryFrom<(i64, i64)> for Rational {
    type Error = TimeError;
    fn try_from((n, d): (i64, i64)) -> Result<Self, Self::Error> {
        Rational::new(n, d)
    }
}

impl From<Rational> for (i64, i64) {
    fn from(r: Rational) -> Self {
        (r.num, r.den)
    }
}

/// A decimal integer of ASCII digits, after a `-` when `signed`.
fn parse_int(s: &str, signed: bool) -> Result<i64, TimeError> {
    let digits = if signed {
        s.strip_prefix('-').unwrap_or(s)
    } else {
        s
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(TimeError::Syntax);
    }
    s.parse().map_err(|_| TimeError::Overflow)
}

impl FromStr for Rational {
    type Err = TimeError;

    /// An integer (`"25"`, `"-3"`) or a fraction `n/d` (`"30000/1001"`), as
    /// [`Display`](fmt::Display) writes them: decimal digits, a `-` only
    /// before `n`, no spaces. A fraction that isn't reduced reads as the
    /// reduced one (`"2/4"` is 1/2).
    fn from_str(s: &str) -> Result<Self, TimeError> {
        match s.split_once('/') {
            Some((n, d)) => Rational::new(parse_int(n, true)?, parse_int(d, false)?),
            None => parse_int(s, true).map(Rational::from_integer),
        }
    }
}

impl Serialize for Rational {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rational {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Rational;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an integer or a fraction n/d in a string, such as \"30000/1001\"")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Rational, E> {
                v.parse().map_err(|e| E::custom(format_args!("{v:?}: {e}")))
            }
        }
        d.deserialize_str(Visitor)
    }
}

/// A position or duration in seconds, serialized as `{"exact",
/// "seconds"}` (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "TimeRepr", into = "TimeRepr")]
pub struct Time(pub Rational);

/// How a [`Time`] is written: exactly, and as a number of seconds for
/// readers that want one.
#[derive(Serialize, Deserialize)]
#[serde(rename = "Time")]
struct TimeRepr {
    exact: Rational,
    /// Lossy, so never read back.
    #[serde(default, skip_deserializing)]
    seconds: f64,
}

impl From<TimeRepr> for Time {
    fn from(r: TimeRepr) -> Self {
        Time(r.exact)
    }
}

impl From<Time> for TimeRepr {
    fn from(t: Time) -> Self {
        TimeRepr {
            exact: t.0,
            seconds: t.to_secs_f64(),
        }
    }
}

impl Time {
    pub const ZERO: Time = Time(Rational::ZERO);

    pub fn seconds(r: Rational) -> Self {
        Time(r)
    }

    pub fn from_secs(s: i64) -> Self {
        Time(Rational::from_integer(s))
    }

    /// Time of stream tick `pts` in a stream whose timebase is `tb` seconds
    /// per tick.
    pub fn from_pts(pts: i64, tb: Rational) -> Result<Self, TimeError> {
        Rational::from_integer(pts).checked_mul(tb).map(Time)
    }

    /// Nearest stream tick for this time in timebase `tb`.
    pub fn to_pts(self, tb: Rational, rounding: Rounding) -> Result<i64, TimeError> {
        if tb.is_zero() {
            return Err(TimeError::ZeroDenominator);
        }
        // pts = t / tb, computed in i128 without reducing first.
        let n = self.0.num as i128 * tb.den as i128;
        let d = self.0.den as i128 * tb.num as i128;
        let (n, d) = if d < 0 { (-n, -d) } else { (n, d) };
        i64::try_from(div_round(n, d, rounding)).map_err(|_| TimeError::Overflow)
    }

    pub fn checked_add(self, o: Time) -> Result<Time, TimeError> {
        self.0.checked_add(o.0).map(Time)
    }

    pub fn checked_sub(self, o: Time) -> Result<Time, TimeError> {
        self.0.checked_sub(o.0).map(Time)
    }

    pub fn as_rational(self) -> Rational {
        self.0
    }

    pub fn to_secs_f64(self) -> f64 {
        self.0.to_f64()
    }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}s", self.0)
    }
}

/// The time of a frame of a video stream in clip time, which is 0 at the
/// stream's first presented frame (M1 plan, section 3.3), and the frame's
/// index from that frame (0), when it is known: for a constant-rate
/// stream, or a frame reached by counting from the start. A
/// variable-rate stream's frames are addressed by time alone. Serialized
/// as a [`Time`] with `"frame"` added when there is an index:
/// `{"exact": "1001/10000", "seconds": 0.1001, "frame": 3}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "FrameTimeRepr", into = "FrameTimeRepr")]
pub struct FrameTime {
    pub time: Time,
    pub frame: Option<u64>,
}

impl FrameTime {
    /// The start of frame `frame` of a stream at the constant rate `rate`,
    /// in clip time: frame n is at n / `rate`, whatever the pts of the
    /// stream's first frame. Add that frame's time to get the stream's.
    pub fn at(rate: FrameRate, frame: u64) -> Result<Self, TimeError> {
        let index = i64::try_from(frame).map_err(|_| TimeError::Overflow)?;
        Ok(FrameTime {
            time: rate.frame_to_time(index)?,
            frame: Some(frame),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename = "FrameTime")]
struct FrameTimeRepr {
    exact: Rational,
    #[serde(default, skip_deserializing)]
    seconds: f64,
    /// A bare integer in every format, or absent: ron would otherwise
    /// write `Some(3)`. Null (ron's `None`) reads as absent.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "frame_index::serialize",
        deserialize_with = "frame_index::deserialize"
    )]
    frame: Option<u64>,
}

/// [`FrameTimeRepr::frame`] as a bare integer.
mod frame_index {
    use std::fmt;

    use serde::de::{self, Unexpected, Visitor};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(frame: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        match frame {
            Some(n) => s.serialize_u64(*n),
            None => s.serialize_none(),
        }
    }

    /// A bare integer, or null, which is no index as for any other
    /// `Option`: JSON readers often send null for a value they don't know.
    /// ron's `None` and `Some(3)` read too.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        d.deserialize_any(Index)
    }

    struct Index;

    impl<'de> Visitor<'de> for Index {
        type Value = Option<u64>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a frame index (an integer, 0 or more) or null")
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            u64::try_from(v)
                .map(Some)
                .map_err(|_| E::invalid_value(Unexpected::Signed(v), &self))
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            u64::deserialize(d).map(Some)
        }
    }
}

impl From<FrameTimeRepr> for FrameTime {
    fn from(r: FrameTimeRepr) -> Self {
        FrameTime {
            time: Time(r.exact),
            frame: r.frame,
        }
    }
}

impl From<FrameTime> for FrameTimeRepr {
    fn from(t: FrameTime) -> Self {
        FrameTimeRepr {
            exact: t.time.0,
            seconds: t.time.to_secs_f64(),
            frame: t.frame,
        }
    }
}

/// Frames per second, always positive.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "Rational", into = "Rational")]
pub struct FrameRate(Rational);

impl FrameRate {
    pub const FPS_23_976: FrameRate = FrameRate(Rational {
        num: 24000,
        den: 1001,
    });
    pub const FPS_24: FrameRate = FrameRate(Rational::from_integer(24));
    pub const FPS_25: FrameRate = FrameRate(Rational::from_integer(25));
    pub const FPS_29_97: FrameRate = FrameRate(Rational {
        num: 30000,
        den: 1001,
    });
    pub const FPS_30: FrameRate = FrameRate(Rational::from_integer(30));
    pub const FPS_50: FrameRate = FrameRate(Rational::from_integer(50));
    pub const FPS_59_94: FrameRate = FrameRate(Rational {
        num: 60000,
        den: 1001,
    });
    pub const FPS_60: FrameRate = FrameRate(Rational::from_integer(60));

    pub fn new(r: Rational) -> Result<Self, TimeError> {
        if r.is_positive() {
            Ok(FrameRate(r))
        } else {
            Err(TimeError::NonPositiveRate)
        }
    }

    pub fn as_rational(self) -> Rational {
        self.0
    }

    /// Duration of one frame.
    pub fn frame_duration(self) -> Time {
        // A positive reduced rational always has a representable reciprocal.
        Time(Rational {
            num: self.0.den,
            den: self.0.num,
        })
    }

    /// Start time of frame `index` (frame 0 starts at zero).
    pub fn frame_to_time(self, index: i64) -> Result<Time, TimeError> {
        Rational::from_integer(index)
            .checked_mul(self.frame_duration().0)
            .map(Time)
    }

    /// Index of the frame at time `t`. With [`Rounding::Floor`] this is the
    /// frame being displayed at `t`.
    pub fn time_to_frame(self, t: Time, rounding: Rounding) -> Result<i64, TimeError> {
        t.to_pts(self.frame_duration().0, rounding)
    }

    pub fn to_f64(self) -> f64 {
        self.0.to_f64()
    }
}

impl TryFrom<Rational> for FrameRate {
    type Error = TimeError;
    fn try_from(r: Rational) -> Result<Self, Self::Error> {
        FrameRate::new(r)
    }
}

impl From<FrameRate> for Rational {
    fn from(f: FrameRate) -> Self {
        f.0
    }
}

impl fmt::Debug for FrameRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}fps", self.0)
    }
}

impl fmt::Display for FrameRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.den == 1 {
            write!(f, "{}", self.0.num)
        } else {
            write!(f, "{:.3}", self.to_f64())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn r(n: i64, d: i64) -> Rational {
        Rational::new(n, d).unwrap()
    }

    #[test]
    fn reduces_and_normalises_sign() {
        assert_eq!(r(2, 4), r(1, 2));
        assert_eq!(r(1, -2), r(-1, 2));
        assert_eq!(r(1, -2).den(), 2);
        assert_eq!(Rational::new(1, 0), Err(TimeError::ZeroDenominator));
    }

    #[test]
    fn ntsc_frame_times_are_exact() {
        let fr = FrameRate::FPS_29_97;
        // One hour of 29.97 is 107892.107892... frames; frame 107892 starts
        // at 3599.9964 s exactly.
        let t = fr.frame_to_time(107_892).unwrap();
        assert_eq!(t.0, r(107_892 * 1001, 30_000));
        assert_eq!(fr.time_to_frame(t, Rounding::Floor).unwrap(), 107_892);
        assert_eq!(
            fr.time_to_frame(Time::from_secs(3600), Rounding::Floor)
                .unwrap(),
            107_892
        );
    }

    #[test]
    fn pts_conversion_in_common_timebases() {
        // MP4 from cameras commonly uses 1/90000 or 1/30000 timebases.
        let tb = r(1, 90_000);
        let t = Time::from_pts(3003, tb).unwrap();
        assert_eq!(t.0, r(1001, 30_000));
        assert_eq!(
            FrameRate::FPS_29_97
                .time_to_frame(t, Rounding::Nearest)
                .unwrap(),
            1
        );
        assert_eq!(t.to_pts(r(1, 30_000), Rounding::Floor).unwrap(), 1001);
    }

    #[test]
    fn rounding_modes() {
        let tb = r(1, 2);
        let t = Time(r(-1, 4)); // -0.5 ticks
        assert_eq!(t.to_pts(tb, Rounding::Floor).unwrap(), -1);
        assert_eq!(t.to_pts(tb, Rounding::Ceil).unwrap(), 0);
        assert_eq!(t.to_pts(tb, Rounding::Nearest).unwrap(), -1);
        assert_eq!(Time(r(1, 4)).to_pts(tb, Rounding::Nearest).unwrap(), 1);
    }

    #[test]
    fn overflow_is_reported() {
        let big = Rational::from_integer(i64::MAX);
        assert_eq!(big.checked_add(big), Err(TimeError::Overflow));
    }

    #[test]
    fn rationals_read_back_from_their_text() {
        for (text, want) in [
            ("30000/1001", r(30000, 1001)),
            ("25", r(25, 1)),
            ("-1/2", r(-1, 2)),
            ("0", Rational::ZERO),
            ("2/4", r(1, 2)),
            ("-9223372036854775808", Rational::from_integer(i64::MIN)),
        ] {
            assert_eq!(text.parse::<Rational>(), Ok(want), "{text}");
        }
        assert_eq!(r(30000, 1001).to_string(), "30000/1001");
        assert_eq!(r(50, 2).to_string(), "25");
        assert_eq!(r(1, -2).to_string(), "-1/2");
        for bad in [
            "", "/", "1/", "/2", "+1", "1/-2", "1/+2", "-", "--1", "1.5", " 1", "1 ", "1/2/3",
            "0x10", "1e3",
        ] {
            assert_eq!(bad.parse::<Rational>(), Err(TimeError::Syntax), "{bad:?}");
        }
        assert_eq!("1/0".parse::<Rational>(), Err(TimeError::ZeroDenominator));
        assert_eq!(
            "9223372036854775808".parse::<Rational>(),
            Err(TimeError::Overflow)
        );
    }

    #[test]
    fn serde_round_trip() {
        // The text form of the CLI's JSON (D27), here through RON.
        let fr = FrameRate::FPS_59_94;
        let s = ron::to_string(&fr).unwrap();
        assert_eq!(s, "\"60000/1001\"");
        let back: FrameRate = ron::from_str(&s).unwrap();
        assert_eq!(back, fr);
        assert_eq!(ron::to_string(&r(25, 1)).unwrap(), "\"25\"");
        assert!(ron::from_str::<FrameRate>("\"0/1\"").is_err());
        assert!(ron::from_str::<Rational>("\"1/0\"").is_err());
        // M0's (num, den) pairs are not read any more.
        assert!(ron::from_str::<Rational>("(60000,1001)").is_err());

        let t = Time(r(1001, 30_000));
        let s = ron::to_string(&t).unwrap();
        assert!(s.starts_with("(exact:\"1001/30000\",seconds:0.0333"), "{s}");
        assert_eq!(ron::from_str::<Time>(&s).unwrap(), t);
        // `seconds` is not read: `exact` alone gives the time.
        assert_eq!(
            ron::from_str::<Time>("(exact:\"1001/30000\",seconds:7.0)").unwrap(),
            t
        );
        assert_eq!(
            ron::from_str::<Time>("(exact:\"-3\")").unwrap(),
            Time::from_secs(-3)
        );
        assert!(ron::from_str::<Time>("(seconds:1.0)").is_err());

        let f = FrameTime::at(FrameRate::FPS_29_97, 3).unwrap();
        assert_eq!(f.time, Time(r(3003, 30_000)));
        let s = ron::to_string(&f).unwrap();
        assert!(s.starts_with("(exact:\"1001/10000\",seconds:0.1001"), "{s}");
        assert!(s.ends_with(",frame:3)"), "{s}");
        assert_eq!(ron::from_str::<FrameTime>(&s).unwrap(), f);
        // Without a known index (a variable-rate stream), it is written as
        // a plain time, and a plain time reads back as one.
        let vfr = FrameTime {
            time: Time(r(3003, 30_000)),
            frame: None,
        };
        let s = ron::to_string(&vfr).unwrap();
        assert_eq!(s, ron::to_string(&vfr.time).unwrap());
        assert_eq!(ron::from_str::<FrameTime>(&s).unwrap(), vfr);
        assert!(ron::from_str::<FrameTime>("(frame:3)").is_err());
        // ron's own spellings of an Option read too; a negative index doesn't.
        let read = |frame: &str| {
            ron::from_str::<FrameTime>(&format!("(exact:\"1001/10000\",frame:{frame})"))
        };
        assert_eq!(read("None").unwrap(), vfr);
        assert_eq!(read("Some(3)").unwrap(), f);
        assert_eq!(read("3").unwrap(), f);
        assert!(read("-1").is_err());
        assert!(read("3.0").is_err());
    }

    fn rates() -> impl Strategy<Value = FrameRate> {
        prop_oneof![
            Just(FrameRate::FPS_23_976),
            Just(FrameRate::FPS_24),
            Just(FrameRate::FPS_25),
            Just(FrameRate::FPS_29_97),
            Just(FrameRate::FPS_30),
            Just(FrameRate::FPS_50),
            Just(FrameRate::FPS_59_94),
            Just(FrameRate::FPS_60),
            (1i64..=240_000, 1i64..=1001)
                .prop_map(|(n, d)| FrameRate::new(Rational::new(n, d).unwrap()).unwrap()),
        ]
    }

    fn timebases() -> impl Strategy<Value = Rational> {
        prop_oneof![
            Just(r(1, 90_000)),
            Just(r(1, 30_000)),
            Just(r(1, 60_000)),
            Just(r(1, 48_000)),
            Just(r(1001, 30_000)),
            (1i64..=1001, 1i64..=1_000_000).prop_map(|(n, d)| r(n, d)),
        ]
    }

    proptest! {
        #[test]
        fn frame_index_round_trips(fr in rates(), idx in -10_000_000i64..10_000_000) {
            let t = fr.frame_to_time(idx).unwrap();
            prop_assert_eq!(fr.time_to_frame(t, Rounding::Floor).unwrap(), idx);
            prop_assert_eq!(fr.time_to_frame(t, Rounding::Ceil).unwrap(), idx);
            prop_assert_eq!(fr.time_to_frame(t, Rounding::Nearest).unwrap(), idx);
        }

        #[test]
        fn pts_round_trips(tb in timebases(), pts in -1_000_000_000i64..1_000_000_000) {
            let t = Time::from_pts(pts, tb).unwrap();
            prop_assert_eq!(t.to_pts(tb, Rounding::Nearest).unwrap(), pts);
        }

        #[test]
        fn floor_is_the_displayed_frame(fr in rates(), n in -1_000_000i64..1_000_000, d in 1i64..100_000) {
            let t = Time(r(n, d));
            let f = fr.time_to_frame(t, Rounding::Floor).unwrap();
            let start = fr.frame_to_time(f).unwrap();
            let next = fr.frame_to_time(f + 1).unwrap();
            prop_assert!(start <= t && t < next);
        }

        #[test]
        fn rational_text_round_trips(n in any::<i64>(), d in 1i64..=i64::MAX) {
            let x = Rational::new(n, d).unwrap();
            prop_assert_eq!(x.to_string().parse::<Rational>(), Ok(x));
            let t = Time(x);
            prop_assert_eq!(ron::from_str::<Time>(&ron::to_string(&t).unwrap()).unwrap(), t);
        }

        #[test]
        fn ordering_matches_f64(a in -1_000_000i64..1_000_000, b in 1i64..10_000,
                                c in -1_000_000i64..1_000_000, d in 1i64..10_000) {
            let (x, y) = (r(a, b), r(c, d));
            let fx = a as f64 / b as f64;
            let fy = c as f64 / d as f64;
            if (fx - fy).abs() > 1e-9 {
                prop_assert_eq!(x < y, fx < fy);
            }
            prop_assert_eq!(x.checked_sub(y).unwrap().checked_add(y).unwrap(), x);
        }
    }
}
