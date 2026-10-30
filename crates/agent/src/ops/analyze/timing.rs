//! `analyze timing`: the video stream's packets, read without decoding
//! (`media::packets`), in presentation order: how long each frame lasts
//! until the next is shown, duplicate timestamps, gaps, keyframes, the
//! bitrate, and pictures coded as repeats of the one before.
//!
//! Durations come from pts in presentation order, not from the packets'
//! own durations (amendment 25). Two frames within a tick of the time
//! base share a timestamp, and the rate is constant when the durations
//! vary by no more than [`VFR_SPREAD`] of the nominal plus a tick:
//! Matroska's millisecond ticks make 29.97 fps last 33 and 34 ms by
//! turns.

use serde_json::{Value, json};
use spherecut_core::{Rational, Rounding, Time};
use spherecut_media::Source;
use spherecut_media::packets::{NextPacket, PacketInfo, PacketReader};
use spherecut_media::probe::StreamInfo;

use super::{Measured, Pace, Span, listed, median, none_in_time, round, secs};
use crate::error::ToolError;
use crate::limits::{GAP_OVER, REPEAT_UNDER, TIMING_PACKETS, VFR_SPREAD};
use crate::ops::Ctx;

/// Packets read from the start for clip time 0, the first frame shown:
/// up to the second keyframe, and no more than this.
const HEAD: usize = 300;

/// Packets read past `to`, when the demuxer gives no dts, for the
/// duration of the last frame before it.
const PAST_TO: usize = 32;

/// Packets between checks of the time.
const CHECK_EVERY: usize = 64;

/// The frame rates a measured one snaps to: within a tick, or 0.2 %.
const STANDARD_RATES: [(i64, i64); 14] = [
    (24000, 1001),
    (24, 1),
    (25, 1),
    (30000, 1001),
    (30, 1),
    (48, 1),
    (50, 1),
    (60000, 1001),
    (60, 1),
    (90, 1),
    (100, 1),
    (120000, 1001),
    (120, 1),
    (240, 1),
];

/// A frame of the stretch: its packet, and how long it lasts until the
/// next is shown, in ticks, when that is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Frame {
    pub pts: i64,
    pub size: usize,
    pub key: bool,
    pub dur: Option<i64>,
}

/// The packets read so far, those shown in the stretch kept.
#[derive(Debug, Default)]
struct Reading {
    /// The stretch, in ticks: `[from, to)`.
    from: i64,
    to: i64,
    kept: Vec<(PacketInfo, i64)>,
    /// The first pts at or past `to`.
    after_to: Option<i64>,
    past_to: usize,
    /// The dts of the last packet read that had one.
    last_dts: Option<i64>,
    untimed: usize,
}

impl Reading {
    /// Take `p`; true once every frame before `to` is in, and the first
    /// one after it: any packet after has a dts past it, and so a pts.
    fn take(&mut self, p: PacketInfo) -> bool {
        let Some(pts) = p.pts.or(p.dts) else {
            self.untimed += 1;
            return false;
        };
        if p.dts.is_some() {
            self.last_dts = p.dts;
        }
        if pts >= self.to {
            self.after_to = Some(self.after_to.map_or(pts, |a| a.min(pts)));
            self.past_to += 1;
        } else if pts >= self.from {
            self.kept.push((p, pts));
        }
        match (p.dts, self.after_to) {
            (Some(d), Some(a)) => d >= a,
            (None, Some(_)) => self.past_to >= PAST_TO,
            _ => false,
        }
    }
}

/// How the read ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// Past `to`.
    Done,
    /// The stream's end; `damaged` when a read failed before the file's.
    End { damaged: bool },
    /// For time, or for [`TIMING_PACKETS`].
    Stopped,
}

pub(super) fn run(
    src: Source<'_>,
    stream: &StreamInfo,
    span: &Span,
    cx: &Ctx,
) -> Result<Measured, ToolError> {
    let mut reader =
        PacketReader::open(src.open, stream.index, cx.call.interrupt()).map_err(|e| {
            cx.call.check().err().unwrap_or_else(|| {
                let (code, message) = crate::ops::probe::words(e.code());
                ToolError::new(code, message).at("file").next("probe it")
            })
        })?;
    let tb = reader.time_base();
    let mut pace = Pace::new(cx);
    let (mut head, last) = read_head(&mut reader);
    let ended = match last {
        Some(NextPacket::End { damaged }) => Some(Ended::End { damaged }),
        Some(NextPacket::Stopped) => {
            pace.stopped_for_time()?;
            return Err(none_in_time(cx));
        }
        _ => None,
    };
    let origin = origin_of(&head, &reader);
    let tick = |t: Time| {
        t.to_pts(tb, Rounding::Ceil)
            .map(|t| t.saturating_add(origin))
            .map_err(|_| ToolError::internal("a time past what the stream's ticks hold"))
    };
    let mut r = Reading {
        from: tick(span.from)?,
        to: tick(span.to)?,
        ..Reading::default()
    };
    // Far enough in that the head doesn't reach `from`: seek there.
    let short = head
        .last()
        .and_then(|p| p.dts.or(p.pts))
        .is_some_and(|d| d < r.from);
    if short && ended.is_none() {
        reader.seek(r.from).map_err(|_| {
            ToolError::new(crate::error::codes::DAMAGED, "can't be read at from")
                .at("from")
                .next("an earlier from")
        })?;
        head.clear();
    }
    let mut how = None;
    for p in head {
        if r.take(p) {
            how = Some(Ended::Done);
            break;
        }
    }
    let how = match how.or(ended) {
        Some(how) => how,
        None => read(&mut reader, &mut r, &mut pace, span, tb, origin)?,
    };
    let clip = |pts: i64| (pts - origin) as f64 * tb.to_f64();
    // The buckets done: all, unless the read stopped short, when those
    // ending before the last frame whose next one is known.
    let done = match how {
        Ended::Stopped => {
            let reached = r.last_dts.unwrap_or(i64::MAX);
            let certain = r
                .kept
                .iter()
                .map(|(_, pts)| *pts)
                .filter(|&p| p < reached)
                .max();
            let k = certain
                .and_then(|p| clip_time(p, origin, tb))
                .map_or(0, |t| span.done_by(t));
            if k == 0 {
                return Err(none_in_time(cx));
            }
            k
        }
        _ => span.n,
    };
    let cut = if done == span.n {
        r.to
    } else {
        tick(span.boundary(done))?
    };
    let frames = frames(&r, how);
    let frames: Vec<Frame> = frames.into_iter().filter(|f| f.pts < cut).collect();
    let declared = stream.avg_frame_rate;
    let (buckets, summary) = measure(&frames, tb, origin, span, done, declared);
    let mut warnings = Vec::new();
    if let Ended::End { damaged: true } = how {
        let at = r
            .kept
            .iter()
            .map(|(_, p)| *p)
            .max()
            .map_or(span.start(), clip);
        warnings.push(json!({
            "code": "damaged",
            "message": format!(
                "the stream can't be read past {} s: what comes after isn't measured",
                secs(at)
            ),
        }));
    }
    if r.untimed > 0 {
        warnings.push(json!({
            "code": "untimed_packets",
            "message": format!("{} packets have no timestamp, and aren't counted", r.untimed),
        }));
    }
    Ok(Measured {
        buckets,
        summary,
        stopped: (done < span.n).then_some(done),
        warnings,
    })
}

/// Tick `pts` in clip time, exactly; `None` past what fractions hold.
fn clip_time(pts: i64, origin: i64, tb: Rational) -> Option<Time> {
    Time::from_pts(pts.checked_sub(origin)?, tb).ok()
}

/// The stream's packets from its start up to the second keyframe,
/// [`HEAD`] at most, and what ended the read if it wasn't that.
fn read_head(reader: &mut PacketReader) -> (Vec<PacketInfo>, Option<NextPacket>) {
    let mut head = Vec::new();
    let mut keys = 0;
    while head.len() < HEAD {
        match reader.next_packet() {
            NextPacket::Packet(p) => {
                keys += usize::from(p.key);
                head.push(p);
                if keys >= 2 {
                    break;
                }
            }
            other => return (head, Some(other)),
        }
    }
    (head, None)
}

/// Clip time 0, in ticks: the first frame shown (D29), as media's
/// decoders place it, which is the least pts of the stream's first
/// packets; else the container's start.
fn origin_of(head: &[PacketInfo], reader: &PacketReader) -> i64 {
    head.iter()
        .filter_map(|p| p.pts)
        .min()
        .or(reader.start_pts())
        .unwrap_or(0)
}

/// Clip time 0 of video stream `index` in the file's own time, for audio
/// to be placed against (`media::audio`'s origin).
pub(super) fn video_origin(src: Source<'_>, index: usize, cx: &Ctx) -> Option<Time> {
    let mut reader = PacketReader::open(src.open, index, cx.call.interrupt()).ok()?;
    let (head, _) = read_head(&mut reader);
    Time::from_pts(origin_of(&head, &reader), reader.time_base()).ok()
}

/// Read on until the stretch is in, the stream ends, or it's time to stop.
fn read(
    reader: &mut PacketReader,
    r: &mut Reading,
    pace: &mut Pace,
    span: &Span,
    tb: Rational,
    origin: i64,
) -> Result<Ended, ToolError> {
    let total = span.end() - span.start();
    for n in 1usize.. {
        match reader.next_packet() {
            NextPacket::Packet(p) => {
                if r.take(p) {
                    return Ok(Ended::Done);
                }
                if r.kept.len() >= TIMING_PACKETS {
                    return Ok(Ended::Stopped);
                }
                if n % CHECK_EVERY == 0 {
                    if let Some(d) = r.last_dts {
                        let at = (d - origin) as f64 * tb.to_f64() - span.start();
                        pace.cx
                            .call
                            .progress(at.max(0.0), Some(total), Some("timing"));
                    }
                    if pace.stop()? {
                        return Ok(Ended::Stopped);
                    }
                }
            }
            NextPacket::End { damaged } => return Ok(Ended::End { damaged }),
            NextPacket::Stopped => {
                pace.stopped_for_time()?;
                return Ok(Ended::Stopped);
            }
        }
    }
    Ok(Ended::Stopped)
}

/// The kept packets in presentation order, each lasting until the next
/// one; the last until the first past `to`, or at the stream's end for
/// its own duration.
fn frames(r: &Reading, how: Ended) -> Vec<Frame> {
    let mut kept: Vec<(PacketInfo, i64)> = r.kept.clone();
    kept.sort_by_key(|(_, pts)| *pts);
    let last = match (r.after_to, how) {
        (Some(a), _) => Some(a),
        (None, Ended::End { .. }) => kept
            .last()
            .filter(|(p, _)| p.duration > 0)
            .map(|(p, pts)| pts + p.duration),
        _ => None,
    };
    let next: Vec<Option<i64>> = kept
        .iter()
        .skip(1)
        .map(|(_, pts)| Some(*pts))
        .chain([last])
        .collect();
    kept.iter()
        .zip(next)
        .map(|((p, pts), next)| Frame {
            pts: *pts,
            size: p.size,
            key: p.key,
            dur: next.map(|n| n - pts),
        })
        .collect()
}

/// A duration in ticks as milliseconds.
fn ms(ticks: f64, tb: Rational) -> f64 {
    round(ticks * tb.to_f64() * 1000.0, 3)
}

/// The nominal frame duration, in ticks: the mean of the durations that
/// are neither duplicates nor gaps against their median, snapped to the
/// nearest standard rate (or the declared one) within a tick or 0.2 %.
fn nominal(durs: &[i64], tb: Rational, declared: Option<Rational>) -> Option<f64> {
    let mut real: Vec<f64> = durs.iter().filter(|&&d| d > 1).map(|&d| d as f64).collect();
    let m = median(&mut real)?;
    let regular: Vec<f64> = real.into_iter().filter(|&d| d <= GAP_OVER * m).collect();
    if regular.is_empty() {
        return Some(m);
    }
    let mean = regular.iter().sum::<f64>() / regular.len() as f64;
    let tb = tb.to_f64();
    let within = 1.0f64.max(0.002 * mean);
    STANDARD_RATES
        .iter()
        .map(|&(n, d)| n as f64 / d as f64)
        .chain(declared.map(|r| r.to_f64()).filter(|r| *r > 0.0))
        .map(|rate| 1.0 / (rate * tb))
        .filter(|cand| (cand - mean).abs() <= within)
        .min_by(|a, b| (a - mean).abs().total_cmp(&(b - mean).abs()))
        .or(Some(mean))
}

/// One bucket's tallies.
#[derive(Debug, Default, Clone)]
struct Tally {
    frames: usize,
    durs: Vec<f64>,
    dups: usize,
    missing: u64,
    keys: usize,
    repeats: usize,
    bytes: usize,
}

/// `frames`' buckets (the first `done`) and summary.
pub(super) fn measure(
    frames: &[Frame],
    tb: Rational,
    origin: i64,
    span: &Span,
    done: usize,
    declared: Option<Rational>,
) -> (Vec<Value>, Value) {
    let durs: Vec<i64> = frames.iter().filter_map(|f| f.dur).collect();
    let nominal = nominal(&durs, tb, declared);
    let mut sizes: Vec<f64> = frames
        .iter()
        .filter(|f| !f.key)
        .map(|f| f.size as f64)
        .collect();
    let repeat_under = median(&mut sizes).map(|m| m * REPEAT_UNDER);
    let mut tallies = vec![Tally::default(); done];
    let (mut dups, mut gaps, mut repeats, mut keys) = (vec![], vec![], vec![], vec![]);
    let mut missing = 0u64;
    let mut all_durs: Vec<f64> = Vec::new();
    let mut regular: Vec<i64> = Vec::new();
    let mut bytes = 0usize;
    for f in frames {
        let Some(time) = clip_time(f.pts, origin, tb) else {
            continue;
        };
        let Some(b) = span.bucket(time).filter(|&b| b < done) else {
            continue;
        };
        let t = time.to_secs_f64();
        let tally = &mut tallies[b];
        tally.frames += 1;
        tally.bytes += f.size;
        bytes += f.size;
        if f.key {
            tally.keys += 1;
            keys.push(t);
        } else if repeat_under.is_some_and(|u| (f.size as f64) < u) {
            tally.repeats += 1;
            repeats.push(t);
        }
        let Some(d) = f.dur else { continue };
        if d <= 1 {
            tally.dups += 1;
            dups.push(t);
            continue;
        }
        tally.durs.push(d as f64);
        all_durs.push(d as f64);
        match nominal.filter(|&n| d as f64 > GAP_OVER * n) {
            Some(n) => {
                let m = ((d as f64 / n).round() as u64).saturating_sub(1).max(1);
                tally.missing += m;
                missing += m;
                gaps.push(t);
            }
            None => regular.push(d),
        }
    }
    let width = span.width();
    let buckets = tallies
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let (min, mean, max) = spread(&t.durs, tb);
            json!({
                "from": secs(span.bucket_start(i)),
                "frames": t.frames,
                "min_ms": min,
                "mean_ms": mean,
                "max_ms": max,
                "dups": t.dups,
                "missing": t.missing,
                "keys": t.keys,
                "repeats": t.repeats,
                "mbps": round(t.bytes as f64 * 8.0 / width / 1e6, 2),
            })
        })
        .collect();
    let vfr = match (regular.iter().min(), regular.iter().max(), nominal) {
        (Some(&lo), Some(&hi), Some(n)) => (hi - lo) as f64 > VFR_SPREAD * n + 1.0,
        _ => false,
    };
    let shown: f64 = all_durs.iter().sum::<f64>() * tb.to_f64();
    let (min, mean, max) = spread(&all_durs, tb);
    let mut intervals: Vec<f64> = keys.windows(2).map(|w| w[1] - w[0]).collect();
    let longest = intervals
        .iter()
        .copied()
        .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))));
    let interval = median(&mut intervals)
        .map(|m| json!({"median": secs(m), "longest": secs(longest.unwrap_or(m))}));
    let mut gaps_json = listed(&gaps);
    gaps_json["missing"] = json!(missing);
    let seconds = width * done as f64;
    let summary = json!({
        "frames": tallies.iter().map(|t| t.frames).sum::<usize>(),
        "nominal_fps": nominal.map(|n| round(1.0 / (n * tb.to_f64()), 3)),
        "declared_fps": declared.map(|r| round(r.to_f64(), 3)),
        "measured_fps": (shown > 0.0).then(|| round(all_durs.len() as f64 / shown, 3)),
        "vfr": vfr,
        "duration_ms": {"min": min, "mean": mean, "max": max},
        "dups": listed(&dups),
        "gaps": gaps_json,
        "keyframes": keys.len(),
        "keyframe_interval": interval,
        "mbps": round(bytes as f64 * 8.0 / seconds.max(f64::MIN_POSITIVE) / 1e6, 2),
        "repeated_pictures": listed(&repeats),
    });
    (buckets, summary)
}

/// The least, mean and most of `durs` (ticks) in milliseconds, or nulls.
fn spread(durs: &[f64], tb: Rational) -> (Value, Value, Value) {
    if durs.is_empty() {
        return (Value::Null, Value::Null, Value::Null);
    }
    let lo = durs.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = durs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mean = durs.iter().sum::<f64>() / durs.len() as f64;
    (json!(ms(lo, tb)), json!(ms(mean, tb)), json!(ms(hi, tb)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb(den: i64) -> Rational {
        Rational::new(1, den).unwrap()
    }

    fn span(secs: i64, n: usize) -> Span {
        Span {
            from: Time::ZERO,
            to: Time::from_secs(secs),
            n,
        }
    }

    /// Frames at `pts`, each lasting to the next, the last one `last`.
    fn at(pts: &[i64], last: i64) -> Vec<Frame> {
        let next = pts
            .iter()
            .skip(1)
            .copied()
            .chain([pts[pts.len() - 1] + last]);
        pts.iter()
            .zip(next)
            .enumerate()
            .map(|(i, (&p, n))| Frame {
                pts: p,
                size: if i % 30 == 0 { 50_000 } else { 10_000 },
                key: i % 30 == 0,
                dur: Some(n - p),
            })
            .collect()
    }

    #[test]
    fn a_constant_rate_is_constant_even_in_milliseconds() {
        // 29.97 fps in Matroska: 33 and 34 ms by turns.
        let pts: Vec<i64> = (0..300)
            .map(|i| (i as f64 * 1001.0 / 30.0).round() as i64)
            .collect();
        let (_, s) = measure(&at(&pts, 33), tb(1000), 0, &span(10, 10), 10, None);
        assert_eq!(s["vfr"], false, "{s}");
        assert_eq!(s["nominal_fps"], 29.97, "{s}");
        assert_eq!(s["gaps"]["count"], 0);
        assert_eq!(s["dups"]["count"], 0);
        assert_eq!(s["frames"], 300);
        assert_eq!(s["keyframes"], 10);
        assert_eq!(s["keyframe_interval"]["median"], 1.001);
    }

    #[test]
    fn jitter_gaps_and_dups_are_told_apart() {
        // 30 fps in 90 kHz: frames 10 to 19 jitter by ±10 %, 3 frames
        // after 40 are missing, and 60 has a twin a tick later.
        let mut pts = Vec::new();
        for i in 0..90i64 {
            if (41..44).contains(&i) {
                continue;
            }
            let shift = match i {
                10..20 if i % 2 == 0 => -300,
                10..20 => 300,
                _ => 0,
            };
            pts.push(i * 3000 + shift);
            if i == 60 {
                pts.push(i * 3000 + 1);
            }
        }
        let (b, s) = measure(&at(&pts, 3000), tb(90_000), 0, &span(3, 3), 3, None);
        assert_eq!(s["vfr"], true, "{s}");
        assert_eq!(s["nominal_fps"], 30.0);
        assert_eq!(s["gaps"]["count"], 1);
        assert_eq!(s["gaps"]["missing"], 3);
        assert_eq!(s["gaps"]["at"], json!([1.333]));
        assert_eq!(s["dups"]["count"], 1);
        assert_eq!(s["dups"]["at"], json!([2.0]));
        assert_eq!(b[1]["missing"], 3, "{b:?}");
        assert_eq!(b[1]["max_ms"], 133.333);
        assert_eq!(b[0]["min_ms"], 26.667);
        assert_eq!(b[2]["dups"], 1);
        assert_eq!(s["frames"], 88);
    }

    #[test]
    fn tiny_packets_are_repeats_and_only_complete_buckets_count() {
        let pts: Vec<i64> = (0..60).map(|i| i * 3000).collect();
        let mut f = at(&pts, 3000);
        f[7].size = 100;
        f[45].size = 100;
        let (b, s) = measure(&f, tb(90_000), 0, &span(2, 2), 1, None);
        assert_eq!(b.len(), 1);
        assert_eq!(s["repeated_pictures"]["at"], json!([0.233]));
        assert_eq!(s["frames"], 30);
        assert_eq!(b[0]["repeats"], 1);
        // 28 × 10 kB, the repeat's 100 bytes and a 50 kB keyframe in a
        // second.
        assert_eq!(b[0]["mbps"], 2.64);
    }

    #[test]
    fn a_declared_rate_off_the_standard_ones_is_taken() {
        let pts: Vec<i64> = (0..100).map(|i| i * 4000).collect();
        let r = Rational::new(45, 2).unwrap();
        let (_, s) = measure(&at(&pts, 4000), tb(90_000), 0, &span(5, 5), 5, Some(r));
        assert_eq!(s["nominal_fps"], 22.5, "{s}");
        assert_eq!(s["declared_fps"], 22.5);
    }
}
