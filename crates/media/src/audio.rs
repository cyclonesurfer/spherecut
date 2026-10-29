//! Audio decoding for playback (M1 plan section 9.1, DECISIONS D35).
//!
//! [`FfmpegAudio`] reads one audio stream through its own confined
//! demuxer (D24) and decoder, opened by the codec id as an int (D11), and
//! hands out packed f32 at the output's rate and channel count, placed on
//! the clip's time axis: output sample `s` is clip time `s / rate`, where
//! clip time 0 is the video's first presented frame (D29), so that audio
//! and video line up by their own timestamps.
//!
//! **Channels.** Mono is duplicated and stereo passes through. First-order
//! ambiX (`SA3D`, D17) is read as four channels in no order, so that
//! swresample can't remix them as the 4.0 surround AAC calls them, and is
//! decoded here to two virtual cardioids at ±90°: L = ½(W + Y),
//! R = ½(W − Y) (ACN order W, Y, Z, X; SN3D). Anything else is mixed down
//! to stereo by swresample's default matrix. Stereo then goes to the
//! output's channels: averaged for one, the first two of more.
//!
//! **Timing.** libavcodec removes AAC's priming and the demuxer applies
//! edit lists, so frames carry their presentation time. Samples before
//! clip time 0 are dropped, and audio that starts after 0 (or after a
//! seek's time) is preceded by silence, so that chunks follow each other
//! without a gap.
//!
//! **Resampling** runs on a grid: the resampler is only ever started at
//! an input sample `P` samples apart from the stream's first one, where
//! `P` is the input rate over the rates' greatest common divisor (147 for
//! 44.1 to 48 kHz), so its phase there is the one a decode from the start
//! has, and a seek's output after its preroll is the same as a sequential
//! decode's.
//!
//! **Seeking** goes back to 100 ms before the time (at least two AAC
//! frames, for the MDCT's overlap), flushes the decoder, decodes, and
//! drops what comes before the exact sample: the next chunk starts at
//! sample round(at × rate).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use spherecut_core::{Rational, Rounding, StopToken, Time};

use crate::backend::{
    AudioChunk, AudioDecode, AudioOutFormat, AudioRequest, AudioStreamInfo, NextAudio,
};
use crate::ffi::{self, Channels, Resampler};
use crate::{MediaError, ff, init};

/// How far before a seek's time decoding starts, in milliseconds.
pub const SEEK_PREROLL_MS: i64 = 100;

/// The longest run of silence handed out as one chunk, in output frames.
const SILENCE_CHUNK: i64 = 4096;

/// Packets in a row the decoder may refuse before the stream is called
/// undecodable.
const MAX_ERRORS: u32 = 50;

/// How the stream's channels become stereo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mix {
    Mono,
    Stereo,
    /// First-order ambiX, decoded to two cardioids.
    Ambix,
    /// swresample's default downmix.
    Downmix,
}

impl Mix {
    fn of(channels: usize, ambisonic: bool) -> Mix {
        match channels {
            1 => Mix::Mono,
            2 => Mix::Stereo,
            4 if ambisonic => Mix::Ambix,
            _ => Mix::Downmix,
        }
    }

    /// What the resampler reads and writes.
    fn channels(self) -> (Channels, Channels) {
        match self {
            Mix::Mono => (Channels::Unordered(1), Channels::Unordered(1)),
            Mix::Stereo => (Channels::Unordered(2), Channels::Unordered(2)),
            Mix::Ambix => (Channels::Unordered(4), Channels::Unordered(4)),
            Mix::Downmix => (Channels::Frame, Channels::Default(2)),
        }
    }

    /// Channels the resampler writes.
    fn width(self) -> usize {
        match self {
            Mix::Mono => 1,
            Mix::Stereo | Mix::Downmix => 2,
            Mix::Ambix => 4,
        }
    }
}

/// Where the resampler may start: input samples `period` apart from the
/// stream's first one, `first` (both at the input rate, on the stream's
/// own axis), and the output sample each lands on.
#[derive(Debug, Clone, Copy)]
struct Grid {
    first: i64,
    period: i64,
    /// Output samples per period.
    out_period: i64,
    /// The output sample of input sample `first`.
    out_first: i64,
}

impl Grid {
    /// The first grid point at or after input sample `pos`, and its
    /// output sample.
    fn at_or_after(&self, pos: i64) -> (i64, i64) {
        let m = (pos - self.first).div_euclid(self.period)
            + i64::from((pos - self.first).rem_euclid(self.period) != 0);
        (
            self.first + m * self.period,
            self.out_first + m * self.out_period,
        )
    }
}

/// A run of input the resampler is converting.
#[derive(Debug)]
struct Run {
    resampler: Resampler,
    /// The input sample the next frame should start at.
    next_in: i64,
    /// The output sample the resampler's next output is.
    next_out: i64,
}

/// One audio stream, decoded with FFmpeg (see the module docs).
pub struct FfmpegAudio {
    path: PathBuf,
    ictx: ff::format::context::Input,
    dec: ff::decoder::Audio,
    index: usize,
    /// Seconds per tick of the stream's timestamps.
    tb: Rational,
    info: AudioStreamInfo,
    out: AudioOutFormat,
    /// Clip time 0, in seconds of the stream's time.
    origin: Rational,
    in_rate: u32,
    mix: Mix,
    grid: Grid,
    run: Option<Run>,
    /// The output sample the next chunk starts at.
    want: i64,
    /// Where the packets read so far end, in ticks of the time base: the
    /// last frame's padding (AAC codes whole frames of 1024) is cut there.
    /// `None` once a packet came without a duration.
    end_pts: Option<i64>,
    /// A tick of the stream's time base, in input samples, at least 1:
    /// how far a frame's timestamp may be from where the frames before it
    /// put it (Matroska's milliseconds) and still follow on.
    slack: i64,
    queue: VecDeque<AudioChunk>,
    eof_sent: bool,
    ended: bool,
    errors: u32,
    frame: ff::frame::Audio,
    /// Interleaved resampler output, reused.
    scratch: Vec<f32>,
}

impl std::fmt::Debug for FfmpegAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfmpegAudio")
            .field("path", &self.path)
            .field("info", &self.info)
            .field("out", &self.out)
            .field("mix", &self.mix)
            .field("want", &self.want)
            .finish_non_exhaustive()
    }
}

fn err(path: &Path, what: &str, e: impl std::fmt::Display) -> MediaError {
    MediaError::Decode {
        path: path.to_path_buf(),
        reason: format!("{what}: {e}"),
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

impl FfmpegAudio {
    /// Open stream `req.stream` (or the file's first audio stream that has
    /// a decoder) and decode its first frame, which places the stream on
    /// the clip's axis. A file without one is [`MediaError::NoStream`].
    pub fn open(path: &Path, req: &AudioRequest) -> Result<Self, MediaError> {
        init();
        if req.out.rate == 0 || req.out.channels == 0 {
            return Err(err(
                path,
                "opening audio",
                "the output has no rate or channels",
            ));
        }
        let mut ictx = ffi::open_input(path, Some(req.interrupt.clone()))
            .map_err(|e| MediaError::opening(path, e, |e| e.to_string()))?;
        let audio_streams: Vec<(usize, i32)> = ictx
            .streams()
            .filter(|s| ffi::media_type(&s.parameters()) == ff::media::Type::Audio)
            .map(|s| (s.index(), s.id()))
            .collect();
        let (index, id) = match req.stream {
            Some(i) => *audio_streams.iter().find(|(s, _)| *s == i).ok_or_else(|| {
                MediaError::NoStream {
                    path: path.to_path_buf(),
                    kind: "audio",
                }
            })?,
            None => *audio_streams
                .iter()
                .find(|(i, _)| {
                    ictx.stream(*i)
                        .is_some_and(|s| ffi::find_decoder(&s.parameters()).is_some())
                })
                .or(audio_streams.first())
                .ok_or_else(|| MediaError::NoStream {
                    path: path.to_path_buf(),
                    kind: "audio",
                })?,
        };
        ffi::discard_other_streams(&mut ictx, index);
        let st = ictx
            .stream(index)
            .ok_or_else(|| err(path, "opening audio", "the stream vanished"))?;
        let tb = Rational::new(
            i64::from(st.time_base().numerator()),
            i64::from(st.time_base().denominator()),
        )
        .map_err(|e| err(path, "audio time base", e))?;
        let par = st.parameters();
        let fields = ffi::codecpar_fields(&par);
        let mut ctx = ff::codec::context::Context::from_parameters(par)
            .map_err(|e| err(path, "decoder parameters", e))?;
        ffi::set_packet_time_base(&mut ctx, st.time_base());
        let dec = ffi::open_audio_decoder(ctx).map_err(|e| match e {
            ff::Error::DecoderNotFound => MediaError::NoDecoder {
                path: path.to_path_buf(),
                codec: fields
                    .codec_name
                    .clone()
                    .filter(|n| n != "none" && n != "unknown_codec")
                    .unwrap_or_else(|| "this codec's".into()),
            },
            e => err(path, "opening the audio decoder", e),
        })?;
        let ambisonic = fields.channels == 4
            && crate::probe::spatial_audio(path, index, id)
                .is_some_and(|s| s.is_ambix() && s.ambisonic_order == 1);
        let mut a = FfmpegAudio {
            path: path.to_path_buf(),
            ictx,
            dec,
            index,
            tb,
            info: AudioStreamInfo {
                index,
                codec: fields.codec_name.clone().unwrap_or_default(),
                rate: u32::try_from(fields.sample_rate).unwrap_or(0),
                channels: u16::try_from(fields.channels).unwrap_or(0),
                layout: fields.channel_layout.clone(),
                ambisonic,
                start: Time::ZERO,
            },
            out: req.out,
            origin: req.origin.map_or(Rational::ZERO, Time::as_rational),
            in_rate: u32::try_from(fields.sample_rate).unwrap_or(0),
            mix: Mix::Stereo,
            grid: Grid {
                first: 0,
                period: 1,
                out_period: 1,
                out_first: 0,
            },
            run: None,
            want: 0,
            end_pts: Some(i64::MIN),
            slack: 1,
            queue: VecDeque::new(),
            eof_sent: false,
            ended: false,
            errors: 0,
            frame: ff::frame::Audio::empty(),
            scratch: Vec::new(),
        };
        let never = StopToken::never();
        if !a.receive(&never)? {
            return Err(err(path, "decoding audio", "the stream has no samples"));
        }
        let f = ffi::audio_frame_fields(&a.frame);
        if f.rate == 0 || f.channels == 0 {
            return Err(err(
                path,
                "decoding audio",
                "a frame with no rate or channels",
            ));
        }
        a.in_rate = f.rate;
        a.slack = a.tick_samples();
        a.info.rate = f.rate;
        a.info.channels = u16::try_from(f.channels).unwrap_or(u16::MAX);
        a.mix = Mix::of(f.channels, ambisonic);
        let first = a.input_position(f.pts.unwrap_or(0))?;
        if req.origin.is_none() {
            a.origin = Rational::new(first, i64::from(f.rate))
                .map_err(|e| err(path, "audio timing", e))?;
        }
        let g = gcd(i64::from(f.rate), i64::from(req.out.rate));
        let out_first = a.output_position(first)?;
        a.grid = Grid {
            first,
            period: i64::from(f.rate) / g,
            out_period: i64::from(req.out.rate) / g,
            out_first,
        };
        a.info.start = Time::seconds(
            Rational::new(first, i64::from(f.rate))
                .and_then(|t| t.checked_sub(a.origin))
                .map_err(|e| err(path, "audio timing", e))?,
        );
        tracing::debug!(
            path = %path.display(),
            stream = index,
            codec = %a.info.codec,
            rate = f.rate,
            channels = f.channels,
            mix = ?a.mix,
            start = ?a.info.start,
            "audio opened"
        );
        a.take_frame()?;
        Ok(a)
    }

    /// The input sample, at the input rate on the stream's own axis, that
    /// a frame with timestamp `pts` starts at.
    fn input_position(&self, pts: i64) -> Result<i64, MediaError> {
        let rate = self.in_rate.max(1);
        Time::from_pts(pts, self.tb)
            .and_then(|t| t.to_pts(Rational::new(1, i64::from(rate))?, Rounding::Nearest))
            .map_err(|e| err(&self.path, "audio timing", e))
    }

    /// A tick of the time base in input samples, rounded up, at least 1.
    fn tick_samples(&self) -> i64 {
        self.tb
            .checked_mul(Rational::from_integer(i64::from(self.in_rate)))
            .map_or(1, |r| r.round(Rounding::Ceil).max(1))
    }

    /// Where a frame of `n` samples whose timestamp says `pos` starts:
    /// where its frames' size puts it, from the stream's first sample,
    /// when that is within a tick of the timestamp. A time base coarser
    /// than a sample (Matroska's milliseconds) rounds every frame's time,
    /// and a seek must land where a decode from the start puts the frame.
    fn place(&self, pos: i64, n: i64) -> i64 {
        if self.slack <= 1 || n <= 0 {
            return pos;
        }
        let k = (pos - self.grid.first + n / 2).div_euclid(n);
        let snapped = self.grid.first + k * n;
        if (snapped - pos).abs() <= self.slack {
            snapped
        } else {
            pos
        }
    }

    /// The output sample that input sample `pos` lands on.
    fn output_position(&self, pos: i64) -> Result<i64, MediaError> {
        Rational::new(pos, i64::from(self.in_rate.max(1)))
            .and_then(|t| t.checked_sub(self.origin))
            .and_then(|t| t.checked_mul(Rational::from_integer(i64::from(self.out.rate))))
            .map(|r| r.round(Rounding::Nearest))
            .map_err(|e| err(&self.path, "audio timing", e))
    }

    /// Decode the next frame into `self.frame`. `false` at the end.
    fn receive(&mut self, stop: &StopToken) -> Result<bool, MediaError> {
        loop {
            if stop.is_stopped() {
                return Ok(false);
            }
            match self.dec.receive_frame(&mut self.frame) {
                Ok(()) => {
                    self.errors = 0;
                    return Ok(true);
                }
                Err(ff::Error::Eof) => return Ok(false),
                Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => {}
                Err(e) => {
                    self.refused(e)?;
                    continue;
                }
            }
            if self.eof_sent {
                return Ok(false);
            }
            match self.read_packet()? {
                Some(pkt) => {
                    if let Err(e) = self.dec.send_packet(&pkt) {
                        self.refused(e)?;
                    }
                }
                None => {
                    self.eof_sent = true;
                    if let Err(e) = self.dec.send_eof() {
                        tracing::debug!(path = %self.path.display(), "flushing audio: {e}");
                    }
                }
            }
        }
    }

    /// A packet the decoder couldn't decode: skipped, unless too many come
    /// in a row.
    fn refused(&mut self, e: ff::Error) -> Result<(), MediaError> {
        self.errors += 1;
        tracing::debug!(path = %self.path.display(), "audio packet skipped: {e}");
        if self.errors >= MAX_ERRORS {
            return Err(err(&self.path, "decoding audio", e));
        }
        Ok(())
    }

    fn read_packet(&mut self) -> Result<Option<ff::Packet>, MediaError> {
        loop {
            let mut pkt = ff::Packet::empty();
            match pkt.read(&mut self.ictx) {
                Ok(()) if pkt.stream() == self.index => {
                    self.note_end(&pkt);
                    return Ok(Some(pkt));
                }
                Ok(()) => {}
                Err(ff::Error::Eof) => return Ok(None),
                Err(ff::Error::Exit) => {
                    return Err(err(&self.path, "reading audio", "the read was cancelled"));
                }
                Err(e) => {
                    // A damaged packet ends the stream here, as the end of
                    // the file would: what decoded before it plays.
                    tracing::warn!(path = %self.path.display(), "reading audio: {e}");
                    return Ok(None);
                }
            }
        }
    }

    /// Move [`end_pts`](Self::end_pts) to where `pkt` ends.
    fn note_end(&mut self, pkt: &ff::Packet) {
        let Some(end) = self.end_pts else {
            return;
        };
        self.end_pts = match (pkt.pts(), pkt.duration()) {
            (Some(pts), d) if d > 0 => pts.checked_add(d).map(|e| e.max(end)),
            _ => None,
        };
    }

    /// Resample `self.frame` and queue what comes out.
    fn take_frame(&mut self) -> Result<(), MediaError> {
        let f = ffi::audio_frame_fields(&self.frame);
        if f.samples == 0 {
            return Ok(());
        }
        if f.rate != self.in_rate || Mix::of(f.channels, self.info.ambisonic) != self.mix {
            // A stream that changes its rate or channels mid-way: start
            // afresh from here, on a grid of the new rate.
            self.end_run()?;
            self.in_rate = f.rate;
            self.mix = Mix::of(f.channels, self.info.ambisonic);
            let pos = self.input_position(f.pts.unwrap_or(0))?;
            let g = gcd(i64::from(f.rate), i64::from(self.out.rate));
            self.grid = Grid {
                first: pos,
                period: i64::from(f.rate) / g,
                out_period: i64::from(self.out.rate) / g,
                out_first: self.output_position(pos)?,
            };
        }
        let n = i64::try_from(f.samples).unwrap_or(i64::MAX);
        let pos = match (f.pts, &self.run) {
            (Some(pts), Some(run)) => {
                let pos = self.input_position(pts)?;
                if (pos - run.next_in).abs() <= self.slack {
                    run.next_in
                } else {
                    pos
                }
            }
            (Some(pts), None) => self.place(self.input_position(pts)?, n),
            (None, run) => run.as_ref().map_or(self.grid.first, |r| r.next_in),
        };
        if self
            .run
            .as_ref()
            .is_some_and(|r| r.next_in != pos || !r.resampler.reads(&self.frame))
        {
            self.end_run()?;
        }
        // The last frame's padding, past where its packet ends.
        let end = match self.end_pts {
            Some(end) if end > i64::MIN => Some(self.input_position(end)?),
            _ => None,
        };
        let n = match end {
            Some(end) if pos + n > end => (end - pos).max(0),
            _ => n,
        };
        if n == 0 {
            return Ok(());
        }
        let skip = match &self.run {
            Some(_) => 0,
            None => {
                let (start, out) = self.grid.at_or_after(pos);
                if start >= pos + n {
                    return Ok(());
                }
                let (input, output) = self.mix.channels();
                let resampler = Resampler::new(&self.frame, input, self.out.rate, output)
                    .map_err(|e| err(&self.path, "resampling audio", e))?;
                self.run = Some(Run {
                    resampler,
                    next_in: pos,
                    next_out: out,
                });
                usize::try_from(start - pos).unwrap_or(0)
            }
        };
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        let Some(run) = self.run.as_mut() else {
            return Ok(());
        };
        let count = usize::try_from(n).unwrap_or(0).saturating_sub(skip);
        let converted = run
            .resampler
            .convert(&self.frame, skip, count, &mut scratch);
        run.next_in = pos + n;
        let start = run.next_out;
        if let Err(e) = converted {
            self.scratch = scratch;
            return Err(err(&self.path, "resampling audio", e));
        }
        self.emit(start, &scratch);
        self.scratch = scratch;
        Ok(())
    }

    /// Flush the resampler's held samples and end the run.
    fn end_run(&mut self) -> Result<(), MediaError> {
        let Some(mut run) = self.run.take() else {
            return Ok(());
        };
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        let flushed = run.resampler.flush(&mut scratch);
        if flushed.is_ok() {
            self.emit(run.next_out, &scratch);
        }
        self.scratch = scratch;
        flushed.map_err(|e| err(&self.path, "resampling audio", e))
    }

    /// Queue resampled samples (interleaved, [`Mix::width`] channels)
    /// whose first is output sample `start`: what lies before
    /// [`want`](Self::want) is dropped, and a gap after it is silence.
    fn emit(&mut self, start: i64, data: &[f32]) {
        let width = self.mix.width();
        let frames = i64::try_from(data.len() / width).unwrap_or(0);
        if let Some(run) = self.run.as_mut() {
            run.next_out = start + frames;
        }
        let end = start + frames;
        if end <= self.want {
            return;
        }
        if start > self.want {
            self.silence(start);
        }
        let from = usize::try_from(self.want - start)
            .unwrap_or(0)
            .min(data.len() / width);
        let out = self.out.channels as usize;
        let kept = &data[from * width..];
        let mut chunk = Vec::with_capacity(kept.len() / width * out);
        for frame in kept.chunks_exact(width) {
            let (l, r) = match self.mix {
                Mix::Mono => (frame[0], frame[0]),
                Mix::Stereo | Mix::Downmix => (frame[0], frame[1]),
                Mix::Ambix => (0.5 * (frame[0] + frame[1]), 0.5 * (frame[0] - frame[1])),
            };
            match out {
                1 => chunk.push(match self.mix {
                    Mix::Mono => l,
                    _ => 0.5 * (l + r),
                }),
                _ => {
                    chunk.push(l);
                    chunk.push(r);
                    chunk.extend(std::iter::repeat_n(0.0, out - 2));
                }
            }
        }
        let first = start.max(self.want);
        self.want = end;
        self.queue.push_back(AudioChunk {
            start: first,
            channels: self.out.channels,
            data: chunk,
        });
    }

    /// Queue silence from [`want`](Self::want) to output sample `until`.
    fn silence(&mut self, until: i64) {
        while self.want < until {
            let n = (until - self.want).min(SILENCE_CHUNK);
            self.queue.push_back(AudioChunk {
                start: self.want,
                channels: self.out.channels,
                data: vec![0.0; usize::try_from(n).unwrap_or(0) * self.out.channels as usize],
            });
            self.want += n;
        }
    }
}

impl AudioDecode for FfmpegAudio {
    fn info(&self) -> &AudioStreamInfo {
        &self.info
    }

    fn seek(&mut self, at: Time, _stop: &StopToken) -> Result<(), MediaError> {
        spherecut_core::may_block("seeking in an audio stream");
        let rate = Rational::from_integer(i64::from(self.out.rate));
        let target = at
            .as_rational()
            .checked_mul(rate)
            .map(|r| r.round(Rounding::Nearest))
            .map_err(|e| err(&self.path, "audio seek", e))?;
        let from = at
            .as_rational()
            .checked_add(self.origin)
            .and_then(|t| t.checked_sub(Rational::new(SEEK_PREROLL_MS, 1000)?))
            .and_then(|t| Time::seconds(t).to_pts(self.tb, Rounding::Floor))
            .map_err(|e| err(&self.path, "audio seek", e))?;
        if ffi::seek_frame(&mut self.ictx, self.index, from, true).is_err() {
            ffi::seek_frame(&mut self.ictx, self.index, i64::MIN, true)
                .or_else(|_| ffi::seek_to_byte_zero(&mut self.ictx))
                .map_err(|e| err(&self.path, "audio seek", e))?;
        }
        self.dec.flush();
        self.run = None;
        self.queue.clear();
        self.want = target;
        self.eof_sent = false;
        self.ended = false;
        self.errors = 0;
        // The packets after the seek move it again from where they end.
        if self.end_pts.is_some() {
            self.end_pts = Some(i64::MIN);
        }
        Ok(())
    }

    fn next_chunk(&mut self, stop: &StopToken) -> Result<NextAudio, MediaError> {
        loop {
            if let Some(chunk) = self.queue.pop_front() {
                return Ok(NextAudio::Chunk(chunk));
            }
            if self.ended {
                return Ok(NextAudio::End);
            }
            if stop.is_stopped() {
                return Ok(NextAudio::Stopped);
            }
            if self.receive(stop)? {
                self.take_frame()?;
            } else if stop.is_stopped() {
                return Ok(NextAudio::Stopped);
            } else {
                self.end_run()?;
                self.ended = true;
            }
        }
    }
}
