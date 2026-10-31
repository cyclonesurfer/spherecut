//! Which frames an export makes, and when each one is (M2 plan section
//! 8.2): output frame `n` is at sequence time `n / rate`, which shows the
//! source at `in + n / rate`, and there is one for every `n` with
//! `n / rate < out − in`, counted exactly.

use spherecut_core::TimeError;
use spherecut_core::project::Range;
use spherecut_core::time::{FrameRate, Rounding, Time};

use crate::error::ExportError;

/// The frames of an export of `trim` at `rate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePlan {
    /// The trim's start, in the source's time.
    start: Time,
    rate: FrameRate,
    count: u64,
    /// `count / rate`.
    duration: Time,
}

impl FramePlan {
    /// The frames of `trim` (in the source's time) at `rate`. `Invalid`
    /// for a trim with no frame in it.
    pub fn new(trim: Range, rate: FrameRate) -> Result<FramePlan, ExportError> {
        let length = trim
            .end
            .checked_sub(trim.start)
            .map_err(|e| ExportError::Invalid(format!("the trim's length: {e}")))?;
        let count = frame_count(length, rate)
            .map_err(|e| ExportError::Invalid(format!("the trim's frames: {e}")))?;
        if count == 0 {
            return Err(ExportError::Invalid(format!(
                "the trim from {:.3} s to {:.3} s has no frames",
                trim.start.to_secs_f64(),
                trim.end.to_secs_f64()
            )));
        }
        let mut plan = FramePlan {
            start: trim.start,
            rate,
            count,
            duration: Time::ZERO,
        };
        // The last frame's end fits, so every frame's time does.
        let too_long = |e| ExportError::Invalid(format!("the trim's frames: {e}"));
        plan.duration = plan.sequence_time(count).map_err(too_long)?;
        plan.source_time(count).map_err(too_long)?;
        Ok(plan)
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn rate(&self) -> FrameRate {
        self.rate
    }

    /// Frame `n`'s time in the sequence, `n / rate`: it fits for every
    /// `n` up to [`count`](Self::count).
    pub fn sequence_time(&self, n: u64) -> Result<Time, TimeError> {
        let n = i64::try_from(n).map_err(|_| TimeError::Overflow)?;
        self.rate.frame_to_time(n)
    }

    /// The source time frame `n` shows, `in + n / rate`: it fits for
    /// every `n` up to [`count`](Self::count).
    pub fn source_time(&self, n: u64) -> Result<Time, TimeError> {
        self.start.checked_add(self.sequence_time(n)?)
    }

    /// How long the export lasts: `count / rate`.
    pub fn duration(&self) -> Time {
        self.duration
    }
}

/// The number of `n ≥ 0` with `n / rate < length`: `⌈length · rate⌉`,
/// none for a length that isn't positive.
pub fn frame_count(length: Time, rate: FrameRate) -> Result<u64, TimeError> {
    if !length.as_rational().is_positive() {
        return Ok(0);
    }
    let frames = length.as_rational().checked_mul(rate.as_rational())?;
    Ok(frames.round(Rounding::Ceil) as u64)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use spherecut_core::time::Rational;

    use super::*;

    fn secs(num: i64, den: i64) -> Time {
        Time::seconds(Rational::new(num, den).unwrap())
    }

    #[test]
    fn frames_are_those_that_start_before_the_end() {
        let count = |len, rate| frame_count(len, rate).unwrap();
        // Two seconds: whole frames at 30, and at 29.97 the 60th starts at
        // 1.9686 s, before the end.
        assert_eq!(count(Time::from_secs(2), FrameRate::FPS_30), 60);
        assert_eq!(count(Time::from_secs(2), FrameRate::FPS_29_97), 60);
        // Exactly one frame of 29.97, and a tick more.
        assert_eq!(count(secs(1001, 30000), FrameRate::FPS_29_97), 1);
        assert_eq!(count(secs(1002, 30000), FrameRate::FPS_29_97), 2);
        // Less than a frame is still the frame at the start.
        assert_eq!(count(secs(1, 1000), FrameRate::FPS_24), 1);
        assert_eq!(count(Time::ZERO, FrameRate::FPS_24), 0);
        assert_eq!(count(secs(-1, 2), FrameRate::FPS_24), 0);
    }

    #[test]
    fn a_plan_times_its_frames_from_the_trims_start() {
        let trim = Range {
            start: secs(1, 2),
            end: secs(5, 2),
        };
        let p = FramePlan::new(trim, FrameRate::FPS_29_97).unwrap();
        assert_eq!(p.count(), 60);
        assert_eq!(p.sequence_time(0).unwrap(), Time::ZERO);
        assert_eq!(p.source_time(0).unwrap(), secs(1, 2));
        assert_eq!(
            p.source_time(3).unwrap(),
            secs(1, 2).checked_add(secs(3003, 30000)).unwrap()
        );
        assert_eq!(p.duration(), secs(60 * 1001, 30000));
        let empty = Range {
            start: secs(1, 2),
            end: secs(1, 2),
        };
        assert!(matches!(
            FramePlan::new(empty, FrameRate::FPS_30),
            Err(ExportError::Invalid(_))
        ));
    }

    proptest! {
        /// Every counted frame starts before the end, and the next one
        /// doesn't.
        #[test]
        fn the_count_is_exact(
            num in 1i64..10_000_000,
            den in 1i64..100_000,
            rate in prop::sample::select(vec![
                FrameRate::FPS_23_976, FrameRate::FPS_24, FrameRate::FPS_25,
                FrameRate::FPS_29_97, FrameRate::FPS_30, FrameRate::FPS_50,
                FrameRate::FPS_59_94, FrameRate::FPS_60,
            ]),
        ) {
            let len = secs(num, den);
            let n = frame_count(len, rate).unwrap();
            prop_assert!(n >= 1);
            let start = |k: u64| rate.frame_to_time(k as i64).unwrap();
            prop_assert!(start(n - 1) < len);
            prop_assert!(start(n) >= len);
        }
    }
}
