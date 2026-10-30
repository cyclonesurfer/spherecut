//! `analyze audio`: the audio stream decoded at its own rate, every
//! channel unmixed (`media::audio`'s raw mode, amendment 11), placed on
//! the video's clip time: per bucket the peak and RMS of its loudest
//! channel in dBFS and the clipped samples, and the silences.

use serde_json::{Value, json};
use spherecut_core::{Rational, Time};
use spherecut_media::backend::{AudioDecode, AudioOutFormat, AudioRequest, NextAudio};
use spherecut_media::probe::{ProbeReport, StreamInfo, StreamKind};
use spherecut_media::{MediaError, Source, audio::FfmpegAudio};

use super::{Measured, Pace, Span, dbfs, none_in_time, round, secs, timing};
use crate::error::{ToolError, codes};
use crate::limits::{CLIP_LEVEL, CLIP_RUN, LISTED, SILENCE_DBFS, SILENCE_MIN};
use crate::ops::Ctx;

/// Silence's windows, a hundredth of a second.
const WINDOWS_A_SECOND: u32 = 100;

pub(super) fn run(
    src: Source<'_>,
    report: &ProbeReport,
    stream: &StreamInfo,
    span: &Span,
    cx: &Ctx,
) -> Result<Measured, ToolError> {
    // Clip time 0 is the video's first frame, so that audio and frames
    // line up as look and timing give them; without video, the audio's
    // own start.
    let video = report.streams.iter().find(|s| {
        s.kind == StreamKind::Video && !s.disposition.iter().any(|d| d == "attached_pic")
    });
    let origin = video.and_then(|v| timing::video_origin(src, v.index, cx));
    let req = AudioRequest {
        stream: Some(stream.index),
        // Not read in raw mode.
        out: AudioOutFormat {
            rate: 48_000,
            channels: 2,
        },
        origin,
        interrupt: cx.call.interrupt(),
    };
    let mut pace = Pace::new(cx);
    let opening = |e: MediaError| -> ToolError {
        if let Err(stop) = cx.call.check() {
            return stop;
        }
        match e {
            MediaError::NoStream { .. } => ToolError::new(codes::NO_AUDIO, "has no audio stream")
                .at("file")
                .next("probe it"),
            MediaError::NoDecoder { .. } => ToolError::new(
                codes::DECODE_FAILED,
                "has audio this FFmpeg has no decoder for",
            )
            .at("file")
            .next("call doctor: it lists the decoders"),
            e => {
                let (code, message) = crate::ops::probe::words(e.code());
                ToolError::new(code, message)
                    .at("file")
                    .next("probe it with detail \"full\"")
            }
        }
    };
    let mut a = FfmpegAudio::open_raw(src.open, &req).map_err(opening)?;
    let (rate, channels) = (a.info().rate, usize::from(a.info().channels));
    if rate == 0 || channels == 0 {
        return Err(
            ToolError::new(codes::DECODE_FAILED, "has audio with no rate or channels")
                .at("file")
                .next("probe it with detail \"full\""),
        );
    }
    let token = cx.call.token();
    a.seek(span.from, &token).map_err(|e| {
        cx.call.check().err().unwrap_or_else(|| {
            let _ = e;
            ToolError::new(codes::DAMAGED, "the audio can't be read at from")
                .at("from")
                .next("an earlier from")
        })
    })?;
    let sample = |t: Time| (t.to_secs_f64() * f64::from(rate)).round() as i64;
    let mut m = Meter::new(sample(span.from), sample(span.to), span.n, channels, rate);
    let total = span.end() - span.start();
    let mut halted = false;
    let mut broke = false;
    while !m.full() {
        match a.next_chunk(&token) {
            Ok(NextAudio::Chunk(c)) => {
                m.push(c.start, &c.data);
                cx.call.progress(
                    m.reached() as f64 / f64::from(rate) - span.start(),
                    Some(total),
                    Some("audio"),
                );
                if pace.stop()? {
                    halted = true;
                    break;
                }
            }
            Ok(NextAudio::End) => break,
            Ok(NextAudio::Stopped) => {
                pace.stopped_for_time()?;
                halted = true;
                break;
            }
            Err(_) => {
                if let Err(e) = cx.call.check() {
                    if e.code != codes::TIMED_OUT {
                        return Err(e);
                    }
                    halted = true;
                } else {
                    broke = true;
                }
                break;
            }
        }
    }
    let done = if halted { m.done() } else { span.n };
    if done == 0 {
        return Err(none_in_time(cx));
    }
    let reached = m.reached();
    let (buckets, summary) = m.finish(span, done);
    let mut warnings = Vec::new();
    if broke {
        warnings.push(json!({
            "code": "damaged",
            "message": format!(
                "the audio doesn't decode past {} s: what comes after isn't measured",
                secs(reached as f64 / f64::from(rate))
            ),
        }));
    }
    Ok(Measured {
        buckets,
        summary,
        stopped: (done < span.n).then_some(done),
        warnings,
    })
}

/// One bucket's levels.
#[derive(Debug, Clone)]
struct Level {
    peak: f32,
    /// Each channel's sum of squares.
    squares: Vec<f64>,
    samples: u64,
    clipped: u64,
}

/// The levels of `[first, end)`, samples at the stream's rate on the
/// clip's axis, fed a chunk at a time.
#[derive(Debug)]
pub(super) struct Meter {
    first: i64,
    end: i64,
    channels: usize,
    rate: u32,
    levels: Vec<Level>,
    /// The next sample expected: every one before it is measured.
    next: i64,
    /// Each channel's run of samples at [`CLIP_LEVEL`] or past.
    runs: Vec<usize>,
    window: i64,
    /// The window being summed: its first sample, and each channel's sum
    /// of squares.
    window_at: i64,
    window_squares: Vec<f64>,
    /// Where the silence going on began.
    silent_from: Option<i64>,
    silences: Vec<(i64, i64)>,
}

impl Meter {
    pub(super) fn new(first: i64, end: i64, n: usize, channels: usize, rate: u32) -> Meter {
        Meter {
            first,
            end: end.max(first + 1),
            channels,
            rate,
            levels: vec![
                Level {
                    peak: 0.0,
                    squares: vec![0.0; channels],
                    samples: 0,
                    clipped: 0,
                };
                n
            ],
            next: first,
            runs: vec![0; channels],
            window: i64::from((rate / WINDOWS_A_SECOND).max(1)),
            window_at: first,
            window_squares: vec![0.0; channels],
            silent_from: None,
            silences: Vec::new(),
        }
    }

    fn full(&self) -> bool {
        self.next >= self.end
    }

    /// The sample measurement has reached.
    fn reached(&self) -> i64 {
        self.next
    }

    /// The bucket sample `s` falls in.
    fn bucket(&self, s: i64) -> usize {
        let n = self.levels.len() as i64;
        (((s - self.first) * n / (self.end - self.first)).clamp(0, n - 1)) as usize
    }

    /// The buckets every sample of which is measured.
    fn done(&self) -> usize {
        let n = self.levels.len() as i64;
        (((self.next - self.first) * n / (self.end - self.first)).clamp(0, n)) as usize
    }

    /// Take a chunk of interleaved samples, its first at sample `start`.
    pub(super) fn push(&mut self, start: i64, data: &[f32]) {
        let ch = self.channels;
        for (i, frame) in data.chunks_exact(ch).enumerate() {
            let s = start + i as i64;
            if s < self.next || s >= self.end {
                continue;
            }
            let b = self.bucket(s);
            let level = &mut self.levels[b];
            level.samples += 1;
            for (c, &x) in frame.iter().enumerate() {
                let ax = x.abs();
                level.peak = level.peak.max(ax);
                let sq = f64::from(x) * f64::from(x);
                level.squares[c] += sq;
                self.window_squares[c] += sq;
                if ax >= CLIP_LEVEL {
                    self.runs[c] += 1;
                    // The run counts once it is long enough, all of it.
                    level.clipped += match self.runs[c] {
                        r if r == CLIP_RUN => CLIP_RUN as u64,
                        r if r > CLIP_RUN => 1,
                        _ => 0,
                    };
                } else {
                    self.runs[c] = 0;
                }
            }
            self.next = s + 1;
            if self.next - self.window_at == self.window {
                self.close_window();
            }
        }
    }

    /// The window just summed: silent or not, and the silence it ends.
    fn close_window(&mut self) {
        let loudest = self
            .window_squares
            .iter()
            .map(|sq| (sq / self.window as f64).sqrt())
            .fold(0.0, f64::max);
        let silent = loudest <= 10f64.powf(SILENCE_DBFS / 20.0);
        let at = self.window_at;
        if silent {
            self.silent_from.get_or_insert(at);
        } else if let Some(from) = self.silent_from.take() {
            self.keep_silence(from, at);
        }
        self.window_at = self.next;
        self.window_squares.iter_mut().for_each(|s| *s = 0.0);
    }

    fn keep_silence(&mut self, from: i64, to: i64) {
        let min = (SILENCE_MIN.as_secs_f64() * f64::from(self.rate)).round() as i64;
        if to - from >= min {
            self.silences.push((from, to));
        }
    }

    /// The first `done` buckets, and the summary over them.
    pub(super) fn finish(mut self, span: &Span, done: usize) -> (Vec<Value>, Value) {
        let edge = if done == self.levels.len() {
            self.end
        } else {
            self.first + (self.end - self.first) * done as i64 / self.levels.len() as i64
        };
        // A silence going on at the edge, up to the last whole window.
        if let Some(from) = self.silent_from.take() {
            self.keep_silence(from, self.window_at.min(edge));
        }
        let rate = f64::from(self.rate);
        let at = |s: i64| Rational::new(s, i64::from(self.rate)).map(Time).ok();
        let silences: Vec<(i64, i64)> = self
            .silences
            .iter()
            .map(|&(a, b)| (a, b.min(edge)))
            .filter(|&(a, b)| a < edge && b > a)
            .collect();
        let levels = &self.levels[..done];
        let rms = |l: &Level| {
            l.squares
                .iter()
                .map(|sq| (sq / l.samples.max(1) as f64).sqrt())
                .fold(0.0, f64::max)
        };
        let buckets = levels
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let heard = l.samples > 0;
                json!({
                    "from": secs(span.bucket_start(i)),
                    "peak_db": heard.then(|| dbfs(f64::from(l.peak))),
                    "rms_db": heard.then(|| dbfs(rms(l))),
                    "clipped": l.clipped,
                })
            })
            .collect();
        let samples: u64 = levels.iter().map(|l| l.samples).sum();
        let mut squares = vec![0.0; self.channels];
        for l in levels {
            for (s, q) in squares.iter_mut().zip(&l.squares) {
                *s += q;
            }
        }
        let overall = Level {
            peak: levels.iter().map(|l| l.peak).fold(0.0, f32::max),
            squares,
            samples,
            clipped: levels.iter().map(|l| l.clipped).sum(),
        };
        let heard = samples > 0;
        let summary = json!({
            "rate": self.rate,
            "channels": self.channels,
            "seconds": round(samples as f64 / rate, 3),
            "peak_db": heard.then(|| dbfs(f64::from(overall.peak))),
            "rms_db": heard.then(|| dbfs(rms(&overall))),
            "clipped": overall.clipped,
            "silences": {
                "count": silences.len(),
                "at": silences.iter().take(LISTED).map(|&(a, b)| [at(a), at(b)]).collect::<Vec<_>>(),
            },
        });
        (buckets, summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(secs: i64, n: usize) -> Span {
        Span {
            from: Time::ZERO,
            to: Time::from_secs(secs),
            n,
        }
    }

    /// Stereo at 1 kHz rate: `f(s)` for both channels of sample `s`.
    fn feed(m: &mut Meter, from: i64, to: i64, f: impl Fn(i64) -> [f32; 2]) {
        let data: Vec<f32> = (from..to).flat_map(f).collect();
        // In chunks of 100, as a decoder hands them out.
        for (i, c) in data.chunks(200).enumerate() {
            m.push(from + i as i64 * 100, c);
        }
    }

    #[test]
    fn levels_clipping_and_silence() {
        let mut m = Meter::new(0, 4000, 4, 2, 1000);
        feed(&mut m, 0, 4000, |s| match s {
            // A second at half scale on the left, a quarter on the right.
            0..1000 => [0.5 * if s % 2 == 0 { 1.0 } else { -1.0 }, 0.25],
            // Silence, with a window's blip of 0.0005, under -60 dBFS.
            1000..2000 => [if s == 1500 { 0.0005 } else { 0.0 }, 0.0],
            // Full scale on the left: clipped throughout.
            2000..3000 => [1.0, 0.0],
            // Two samples at full scale in a row, then quiet: no clip.
            _ => [if s % 10 >= 8 { 1.0 } else { 0.01 }, 0.01],
        });
        assert!(m.full());
        let (b, s) = m.finish(&span(4, 4), 4);
        assert_eq!(b[0]["peak_db"], -6.02);
        assert_eq!(b[0]["rms_db"], -6.02);
        assert_eq!(b[1]["peak_db"], -66.02);
        assert_eq!(b[2]["clipped"], 1000);
        assert_eq!(b[3]["clipped"], 0);
        assert_eq!(s["clipped"], 1000);
        assert_eq!(s["peak_db"], 0.0);
        assert_eq!(s["silences"]["count"], 1);
        assert_eq!(
            s["silences"]["at"],
            json!([[{"exact": "1", "seconds": 1.0}, {"exact": "2", "seconds": 2.0}]])
        );
        assert_eq!(s["seconds"], 4.0);
    }

    #[test]
    fn a_short_silence_isnt_one_and_a_stop_keeps_whole_buckets() {
        let mut m = Meter::new(0, 4000, 4, 2, 1000);
        feed(&mut m, 0, 2500, |s| match s {
            300..700 => [0.0, 0.0],
            1200..2500 => [0.0, 0.0],
            _ => [0.1, 0.1],
        });
        assert_eq!(m.done(), 2);
        let (b, s) = m.finish(&span(4, 4), 2);
        assert_eq!(b.len(), 2);
        // 0.4 s is too short; the one from 1.2 s goes on past the edge,
        // and is given up to it.
        assert_eq!(
            s["silences"]["at"],
            json!([[{"exact": "6/5", "seconds": 1.2}, {"exact": "2", "seconds": 2.0}]])
        );
        assert_eq!(s["seconds"], 2.0);
    }

    #[test]
    fn a_bucket_without_samples_says_so() {
        let mut m = Meter::new(0, 2000, 2, 1, 1000);
        let data = vec![0.5f32; 1000];
        m.push(0, &data);
        let (b, _) = m.finish(&span(2, 2), 2);
        assert_eq!(b[1]["peak_db"], Value::Null);
        assert_eq!(b[0]["peak_db"], -6.02);
    }
}
