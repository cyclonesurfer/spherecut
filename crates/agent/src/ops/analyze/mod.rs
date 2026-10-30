//! `analyze` (a1-plan section 6.3, D46): a stretch of a file measured
//! over time, in buckets of equal length: its video packets' timing
//! ([`timing`], without decoding), the change from frame to frame
//! ([`motion`]), or its audio's levels ([`audio`]).
//!
//! Every kind stops before the call's deadline at a bucket's end, and
//! answers with the buckets it finished, `complete: false` and
//! `next_from`, where a call to measure the rest starts.

mod audio;
mod motion;
mod timing;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use spherecut_core::{FrameRate, Rational, Rounding, Time, TimeSpec};
use spherecut_media::probe::{ProbeReport, StreamInfo, StreamKind};

use super::Ctx;
use super::look::{self, TimeArg};
use crate::error::{ToolError, codes};
use crate::limits::{ANALYZE_RESERVE, BUCKETS, DBFS_FLOOR, LISTED};

/// What to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Timing,
    Motion,
    Audio,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Timing => "timing",
            Kind::Motion => "motion",
            Kind::Audio => "audio",
        }
    }

    /// The kind of stream it measures.
    fn stream_kind(self) -> StreamKind {
        match self {
            Kind::Timing | Kind::Motion => StreamKind::Video,
            Kind::Audio => StreamKind::Audio,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub file: PathBuf,
    pub kind: Kind,
    pub from: Option<TimeArg>,
    pub to: Option<TimeArg>,
    pub buckets: Option<u32>,
    pub stream: Option<usize>,
}

/// The stretch measured, cut into `n` buckets of equal length.
#[derive(Debug, Clone, Copy)]
struct Span {
    from: Time,
    to: Time,
    n: usize,
}

impl Span {
    fn start(&self) -> f64 {
        self.from.to_secs_f64()
    }

    fn end(&self) -> f64 {
        self.to.to_secs_f64()
    }

    /// A bucket's length, in seconds.
    fn width(&self) -> f64 {
        (self.end() - self.start()) / self.n as f64
    }

    /// How many bucket lengths clip time `t` is past `from`, rounded
    /// down: exactly, so that a frame on a boundary falls in the bucket it
    /// starts whatever the rate, as a call from that boundary finds it.
    /// In seconds only should the fractions overflow.
    fn position(&self, t: Time) -> i64 {
        let n = Rational::from_integer(i64::try_from(self.n).unwrap_or(i64::MAX));
        t.0.checked_sub(self.from.0)
            .and_then(|x| x.checked_mul(n))
            .and_then(|x| x.checked_div(self.to.0.checked_sub(self.from.0)?))
            .map(|x| x.round(Rounding::Floor))
            .unwrap_or_else(|_| ((t.to_secs_f64() - self.start()) / self.width()).floor() as i64)
    }

    /// The bucket clip time `t` falls in, if any.
    fn bucket(&self, t: Time) -> Option<usize> {
        if t < self.from || t >= self.to {
            return None;
        }
        usize::try_from(self.position(t))
            .ok()
            .map(|b| b.min(self.n - 1))
    }

    /// Where bucket `i` starts, in seconds.
    fn bucket_start(&self, i: usize) -> f64 {
        self.boundary(i).to_secs_f64()
    }

    /// Where bucket `i` starts, exactly.
    fn boundary(&self, i: usize) -> Time {
        look::lerp(
            self.from,
            self.to,
            u32::try_from(i).unwrap_or(u32::MAX),
            u32::try_from(self.n).unwrap_or(u32::MAX),
        )
    }

    /// The buckets that end at or before clip time `t`.
    fn done_by(&self, t: Time) -> usize {
        if t >= self.to {
            return self.n;
        }
        usize::try_from(self.position(t).max(0))
            .unwrap_or(0)
            .min(self.n)
    }
}

/// What a kind measured: the buckets it finished, its summary, and,
/// when it stopped for time, how many buckets that is.
struct Measured {
    buckets: Vec<Value>,
    summary: Value,
    /// `Some(k)` when only the first `k` buckets were measured.
    stopped: Option<usize>,
    warnings: Vec<Value>,
}

/// When to stop for time: [`ANALYZE_RESERVE`] before the deadline, plus
/// twice the longest step yet, so that the next step doesn't run past it.
struct Pace<'a> {
    cx: &'a Ctx<'a>,
    last: Instant,
    longest: Duration,
}

impl<'a> Pace<'a> {
    fn new(cx: &'a Ctx<'a>) -> Self {
        Pace {
            cx,
            last: Instant::now(),
            longest: Duration::ZERO,
        }
    }

    /// After a step: `Ok(true)` when it is time to stop and answer, Err
    /// when the call was cancelled.
    fn stop(&mut self) -> Result<bool, ToolError> {
        let now = Instant::now();
        self.longest = self.longest.max(now - self.last);
        self.last = now;
        match self.cx.call.check() {
            Ok(()) => Ok(self.cx.call.remaining() < ANALYZE_RESERVE + self.longest * 2),
            Err(e) if e.code == codes::TIMED_OUT => Ok(true),
            Err(e) => Err(e),
        }
    }

    /// Whether a read that stopped stopped for time (`Ok`), rather than
    /// for a cancel (Err).
    fn stopped_for_time(&self) -> Result<(), ToolError> {
        match self.cx.call.check() {
            Err(e) if e.code == codes::TIMED_OUT => Ok(()),
            Err(e) => Err(e),
            Ok(()) => Err(ToolError::new(codes::CANCELLED, "the call was stopped")),
        }
    }
}

pub fn run(req: Request, cx: &Ctx) -> Result<Value, ToolError> {
    let n = req.buckets.unwrap_or(BUCKETS).max(1) as usize;
    let opened = cx.roots.open_file("file", &req.file)?;
    let src = opened.source();
    let report =
        spherecut_media::probe::probe_cancellable(src, cx.call.interrupt()).map_err(|e| {
            cx.call.check().err().unwrap_or_else(|| {
                let (code, message) = super::probe::words(e.code());
                ToolError::new(code, message)
                    .at("file")
                    .next("probe it with detail \"full\" for FFmpeg's own words")
            })
        })?;
    let stream = pick(&report, req.kind, req.stream)?;
    let span = span(&report, &stream, &req, n)?;
    cx.call.check()?;
    let measured = match req.kind {
        Kind::Timing => timing::run(src, &stream, &span, cx)?,
        Kind::Motion => motion::run(src, &report, &stream, &span, cx)?,
        Kind::Audio => audio::run(src, &report, &stream, &span, cx)?,
    };
    let mut out = json!({
        "kind": req.kind.name(),
        "stream": stream.index,
        "from": span.from,
        "to": span.to,
        "bucket": round(span.width(), 6),
        "buckets": measured.buckets,
        "summary": measured.summary,
        "complete": measured.stopped.is_none(),
        "warnings": measured.warnings,
    });
    if let Some(k) = measured.stopped {
        out["next_from"] = json!(span.boundary(k));
    }
    Ok(out)
}

/// The stream to measure: `stream`, which must be of the kind's sort, or
/// the file's first (cover pictures aside).
fn pick(report: &ProbeReport, kind: Kind, stream: Option<usize>) -> Result<StreamInfo, ToolError> {
    let want = kind.stream_kind();
    let of_kind: Vec<&StreamInfo> = report
        .streams
        .iter()
        .filter(|s| s.kind == want && !s.disposition.iter().any(|d| d == "attached_pic"))
        .collect();
    let sort = match want {
        StreamKind::Audio => "audio",
        _ => "video",
    };
    match stream {
        None => of_kind.first().map(|s| (*s).clone()).ok_or_else(|| {
            let (code, message) = match want {
                StreamKind::Audio => (codes::NO_AUDIO, "has no audio stream"),
                _ => (codes::NO_VIDEO, "has no video stream"),
            };
            ToolError::new(code, message).at("file").next("probe it")
        }),
        Some(i) => of_kind
            .iter()
            .find(|s| s.index == i)
            .map(|s| (*s).clone())
            .ok_or_else(|| {
                let valid = if of_kind.is_empty() {
                    format!("the file has no {sort} stream")
                } else {
                    let list: Vec<String> = of_kind.iter().map(|s| s.index.to_string()).collect();
                    format!("{sort} streams: {}", list.join(", "))
                };
                ToolError::new(
                    codes::BAD_ARGUMENT,
                    format!("isn't a {sort} stream of the file"),
                )
                .at("stream")
                .valid(valid)
            }),
    }
}

/// The file's video frame rate, when frames come at one: what a frame
/// number in `from` or `to` counts in.
fn frame_rate(report: &ProbeReport) -> Option<FrameRate> {
    let v = report
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)?;
    let (avg, r) = (v.avg_frame_rate?, v.r_frame_rate?);
    (avg == r).then(|| FrameRate::new(avg).ok()).flatten()
}

/// `from` and `to`, in clip time: the start and the stream's end by
/// default; `to` past the end is the end.
fn span(
    report: &ProbeReport,
    stream: &StreamInfo,
    req: &Request,
    n: usize,
) -> Result<Span, ToolError> {
    let length = stream.duration.or(report.container.duration);
    let rate = frame_rate(report);
    let at = |arg: &str, t: &TimeArg| -> Result<Time, ToolError> {
        match look::time(arg, t)? {
            TimeSpec::Time(t) => Ok(t),
            TimeSpec::Frame(f) => rate
                .and_then(|r| r.frame_to_time(i64::try_from(f).ok()?).ok())
                .ok_or_else(|| {
                    ToolError::new(
                        codes::BAD_ARGUMENT,
                        "a frame number needs a constant frame rate, and this video's varies",
                    )
                    .at(arg)
                    .valid("a time in seconds, such as \"12.5\"")
                }),
        }
    };
    let from = req
        .from
        .as_ref()
        .map(|t| at("from", t))
        .transpose()?
        .unwrap_or(Time::ZERO);
    let to = match req.to.as_ref().map(|t| at("to", t)).transpose()? {
        Some(t) => Some(t),
        None => length,
    };
    let Some(mut to) = to else {
        return Err(
            ToolError::new(codes::BAD_ARGUMENT, "the file doesn't say how long it is")
                .at("to")
                .next("give to: the time to measure up to"),
        );
    };
    if let Some(l) = length {
        if from >= l {
            return Err(ToolError::new(codes::OUT_OF_RANGE, "is past the end")
                .at("from")
                .valid(format!("under {} s", crate::still::seconds(l))));
        }
        to = to.min(l);
    }
    if to <= from {
        return Err(ToolError::new(codes::BAD_ARGUMENT, "must come after from").at("to"));
    }
    Ok(Span { from, to, n })
}

/// `v` rounded to `places` decimals, for JSON.
fn round(v: f64, places: i32) -> f64 {
    let s = 10f64.powi(places);
    (v * s).round() / s
}

/// Seconds, to the millisecond.
fn secs(t: f64) -> f64 {
    round(t, 3)
}

/// `level` (0 to 1 and past) in dBFS, to a hundredth, floored at
/// [`DBFS_FLOOR`].
fn dbfs(level: f64) -> f64 {
    if level <= 0.0 {
        return DBFS_FLOOR;
    }
    round((20.0 * level.log10()).max(DBFS_FLOOR), 2)
}

/// The median of `v`, which it sorts; `None` when empty.
fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[m]
    } else {
        (v[m - 1] + v[m]) / 2.0
    })
}

/// A list of moments, as a summary gives it: how many, and the first
/// [`LISTED`], exact, so that each can be sent back to `look`.
fn listed(times: &[Time]) -> Value {
    json!({
        "count": times.len(),
        "at": &times[..times.len().min(LISTED)],
    })
}

/// The error when not one bucket was done in time.
fn none_in_time(cx: &Ctx) -> ToolError {
    let e = match cx.call.check() {
        Err(e) => e,
        Ok(()) => ToolError::new(codes::TIMED_OUT, "the call's time ran out"),
    };
    let message = format!("{} before the first bucket was done", e.message);
    ToolError::new(codes::TIMED_OUT, message)
        .next("a shorter stretch: a later from, or an earlier to")
}

#[cfg(test)]
mod tests {
    use super::*;
    use spherecut_core::Rational;

    fn span(from: (i64, i64), to: (i64, i64), n: usize) -> Span {
        Span {
            from: Time::seconds(Rational::new(from.0, from.1).unwrap()),
            to: Time::seconds(Rational::new(to.0, to.1).unwrap()),
            n,
        }
    }

    #[test]
    fn buckets_cover_the_span_end_to_end() {
        let s = span((1, 1), (11, 1), 4);
        assert_eq!(s.width(), 2.5);
        let t = |n: i64, d: i64| Time::seconds(Rational::new(n, d).unwrap());
        assert_eq!(s.bucket(t(999, 1000)), None);
        assert_eq!(s.bucket(t(1, 1)), Some(0));
        assert_eq!(s.bucket(t(3499, 1000)), Some(0));
        assert_eq!(s.bucket(t(7, 2)), Some(1));
        assert_eq!(s.bucket(t(10999, 1000)), Some(3));
        assert_eq!(s.bucket(t(11, 1)), None);
        assert_eq!(s.boundary(2), Time::from_secs(6));
        assert_eq!(s.done_by(t(59, 10)), 1);
        assert_eq!(s.done_by(t(6, 1)), 2);
        assert_eq!(s.done_by(t(0, 1)), 0);
        assert_eq!(s.done_by(t(11, 1)), 4);
    }

    #[test]
    fn boundaries_are_exact() {
        let s = span((0, 1), (10010, 1000), 3);
        let b = s.boundary(1);
        assert_eq!(b.as_rational(), Rational::new(10010, 3000).unwrap());
        // 1.2 / 0.4 is 2.9999999999999996 in floats: frame 36 of 30 fps
        // starts bucket 3 of 0.4 s, and so does a call from there.
        let s = span((0, 1), (20, 1), 50);
        let frame = |i: i64| Time::seconds(Rational::new(i, 30).unwrap());
        for i in 0..600 {
            assert_eq!(s.bucket(frame(i)), Some(usize::try_from(i / 12).unwrap()));
            assert_eq!(s.done_by(frame(i)), usize::try_from(i / 12).unwrap());
        }
        assert_eq!(s.bucket(s.boundary(3)), Some(3));
    }

    #[test]
    fn levels_in_dbfs() {
        assert_eq!(dbfs(1.0), 0.0);
        assert_eq!(dbfs(0.5), -6.02);
        assert_eq!(dbfs(0.0), DBFS_FLOOR);
        assert_eq!(dbfs(1e-9), DBFS_FLOOR);
        assert_eq!(dbfs(2.0), 6.02);
    }

    #[test]
    fn medians() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), Some(2.5));
    }
}
