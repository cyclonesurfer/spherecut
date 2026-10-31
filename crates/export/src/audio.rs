//! The export's audio (M2 plan section 8.5): the clip's first audio
//! stream, or its ambiX one when it has one, from the trim's start to its
//! end, handed to the writer on the encode stage's thread between video
//! frames, so that the muxer interleaves the two as it goes.
//!
//! Copy takes the source's packets as they are, at its own rate and
//! channels, cut exactly (media's [`AudioCopy`]): AAC and PCM. Anything
//! else, ambiX in a flat export, and a stream whose ticks aren't its
//! samples are encoded again instead, with a warning; PCM ambiX in a 360
//! export is refused, as YouTube takes it only in a MOV.
//!
//! AAC decodes the stream at 48 kHz and encodes it again. A 360 export
//! keeps first-order ambiX as four channels, labelled `4.0` (FFmpeg's AAC
//! encoder refuses an ambisonic layout) and without intensity stereo or
//! noise substitution, which would smear the components; the file says
//! ambiX in an `SA3D` box. A flat export hears ambiX as media's stereo
//! decode does (two cardioids at ±90°), and any other layout as stereo.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use spherecut_core::cancel::StopToken;
use spherecut_core::project::{AudioChoice, Orientation, Output, Range};
use spherecut_core::time::{Rational, Rounding, Time};
use spherecut_media::audio::FfmpegAudio;
use spherecut_media::backend::{AudioChunk, AudioDecode, AudioOutFormat, AudioRequest, NextAudio};
use spherecut_media::copy::{AudioCopy, CopyError, NextCopied};
use spherecut_media::encode::{AudioSettings, AudioTrack, EncodeError, Mp4Writer};
use spherecut_media::probe::{self, StreamKind};

use crate::error::{ExportError, Stage};
use crate::warning::{Warning, codes};

/// The sample rate the export encodes at, and then its movie timescale,
/// so that an edit can end on a sample (amendment 2 item 7). A copy keeps
/// the source's rate, and the movie's timescale is that.
pub(crate) const RATE: u32 = 48_000;

/// AAC's bitrate for stereo and for four channels of ambiX.
const STEREO_BITS: usize = 256_000;
const AMBIX_BITS: usize = 384_000;

/// The source stream an export's audio comes from, and what it becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub stream: usize,
    /// Four channels of first-order ambiX, kept as they are.
    pub ambix: bool,
    /// The packets copied, rather than the samples encoded again.
    pub copy: Option<CopyPlan>,
}

/// What a copied stream is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyPlan {
    /// FFmpeg's name for its codec.
    pub codec: String,
    pub rate: u32,
    pub channels: u16,
    /// Bits a second ([`AudioCopy::bit_rate`]).
    pub bits: u64,
}

impl Plan {
    pub fn channels(&self) -> u16 {
        match &self.copy {
            Some(c) => c.channels,
            None if self.ambix => 4,
            None => 2,
        }
    }

    /// What the report calls the writer's audio: `copy`, or the encoder.
    pub fn encoder(&self) -> &'static str {
        if self.copy.is_some() { "copy" } else { "aac" }
    }

    /// FFmpeg's name for the codec the file's audio is in.
    pub fn codec(&self) -> &str {
        self.copy.as_ref().map_or("aac", |c| c.codec.as_str())
    }

    /// The file's sample rate.
    pub fn rate(&self) -> u32 {
        self.copy.as_ref().map_or(RATE, |c| c.rate)
    }

    /// Bits a second, for an estimate of the file's size.
    pub fn bits(&self) -> u64 {
        match &self.copy {
            Some(c) => c.bits,
            None => self.settings().bit_rate as u64,
        }
    }

    pub fn settings(&self) -> AudioSettings {
        let mut options = vec![("aac_pns".to_string(), "0".to_string())];
        if self.ambix {
            options.push(("aac_is".into(), "0".into()));
        }
        AudioSettings {
            encoder: "aac".into(),
            sample_rate: RATE,
            channels: u32::from(self.channels()),
            bit_rate: if self.ambix { AMBIX_BITS } else { STEREO_BITS },
            options,
        }
    }
}

/// Why `path`'s audio can't be copied into `output` as it is, or `None`
/// when it can: what the export window says beside a greyed Copy (M2
/// plan section 10.6). It reads the file's header, so never on the UI
/// thread.
pub fn copy_refusal(path: &Path, output: &Output) -> Option<String> {
    match choose(path, AudioChoice::Copy, output) {
        Ok((Some(plan), _)) if plan.copy.is_some() => None,
        Ok((_, warnings)) => Some(
            warnings
                .into_iter()
                .find(|w| w.code == codes::AUDIO_ENCODED || w.code == codes::NO_AUDIO)
                .map_or_else(|| "the audio is encoded again".to_owned(), |w| w.message),
        ),
        Err(e) => Some(e.to_string()),
    }
}

/// What the export does with `path`'s audio for `choice` into `output`:
/// the plan, `None` for no audio, and what it did that wasn't asked.
pub(crate) fn choose(
    path: &Path,
    choice: AudioChoice,
    output: &Output,
) -> Result<(Option<Plan>, Vec<Warning>), ExportError> {
    let mut warnings = Vec::new();
    if choice == AudioChoice::None {
        return Ok((None, warnings));
    }
    let report = probe::probe(path)
        .map_err(|e| ExportError::failed(Stage::Decode, None, format!("reading the audio: {e}")))?;
    let audio: Vec<_> = report
        .streams
        .iter()
        .filter(|s| s.kind == StreamKind::Audio)
        .collect();
    let is_ambix = |s: &&&probe::StreamInfo| {
        s.audio.as_ref().is_some_and(|a| a.channels == 4)
            && s.spherical
                .spatial_audio
                .as_ref()
                .is_some_and(|sa| sa.is_ambix() && sa.ambisonic_order == 1)
    };
    let Some(stream) = audio.iter().find(is_ambix).or(audio.first()) else {
        warnings.push(Warning::new(codes::NO_AUDIO, "the clip has no audio"));
        return Ok((None, warnings));
    };
    let ambix_source = is_ambix(&stream);
    let ambix = ambix_source && matches!(output, Output::Equirect { .. });
    let mut copy = None;
    if choice == AudioChoice::Copy {
        // Never interrupted: it reads only the file's header.
        let opened = AudioCopy::open(path, stream.index, Arc::new(AtomicBool::new(false)));
        match opened {
            Ok(_) if ambix_source && !ambix => warnings.push(Warning::new(
                codes::AUDIO_ENCODED,
                "the ambiX audio is encoded again as stereo AAC: a flat export can't carry ambiX",
            )),
            Ok(c) if c.is_pcm() && ambix => {
                return Err(ExportError::Invalid(format!(
                    "the clip's ambiX audio is {}, which can't be copied into a 360 MP4: YouTube \
                     takes PCM spatial audio only in a MOV. Choose AAC",
                    c.codec()
                )));
            }
            Ok(c) => {
                copy = Some(CopyPlan {
                    codec: c.codec().to_owned(),
                    rate: c.rate(),
                    channels: c.channels(),
                    bits: c.bit_rate(),
                });
            }
            Err(CopyError::Refused(why)) => {
                warnings.push(Warning::new(
                    codes::AUDIO_ENCODED,
                    format!("the audio is encoded again as AAC: {why}"),
                ));
            }
            Err(CopyError::Media(e)) => {
                return Err(ExportError::failed(
                    Stage::Decode,
                    None,
                    format!("the audio: {e}"),
                ));
            }
        }
    }
    if ambix
        && let Output::Equirect { orientation, .. } = output
        && *orientation != Orientation::IDENTITY
    {
        warnings.push(Warning::new(
            codes::AMBIX_NOT_TURNED,
            "the ambiX sound field is not turned with the picture's orientation",
        ));
    }
    Ok((
        Some(Plan {
            stream: stream.index,
            ambix,
            copy,
        }),
        warnings,
    ))
}

/// The sample `t` falls on, at `rate`.
fn sample(t: Time, rate: u32) -> Result<i64, ExportError> {
    t.as_rational()
        .checked_mul(Rational::from_integer(i64::from(rate)))
        .map(|r| r.round(Rounding::Nearest))
        .map_err(|e| ExportError::Invalid(format!("the trim's audio: {e}")))
}

/// The trim's audio, handed to the writer as far as each video frame
/// needs: decoded for the encoder, or copied.
#[derive(Debug)]
pub(crate) enum Feed {
    Decode(Decoded),
    Copy(Copying),
}

impl Feed {
    /// `path`'s audio for `plan`, from `trim`'s start on the clip's axis,
    /// whose 0 is the video's first frame, at `origin` in the file.
    pub fn open(
        path: &Path,
        plan: &Plan,
        trim: Range,
        origin: Time,
        interrupt: Arc<AtomicBool>,
        token: &StopToken,
    ) -> Result<Feed, ExportError> {
        match plan.copy {
            None => Decoded::open(path, plan, trim, origin, interrupt, token).map(Feed::Decode),
            Some(_) => Copying::open(path, plan, trim, origin, interrupt).map(Feed::Copy),
        }
    }

    /// The writer's audio track.
    pub fn track(&self) -> AudioTrack<'_> {
        match self {
            Feed::Decode(d) => AudioTrack::Encode(&d.settings),
            Feed::Copy(c) => AudioTrack::Copy(&c.copy),
        }
    }

    pub fn rate(&self) -> u32 {
        match self {
            Feed::Decode(_) => RATE,
            Feed::Copy(c) => c.copy.rate(),
        }
    }

    /// Samples in all: the trim's length at the rate, to within a sample.
    pub fn total(&self) -> u64 {
        match self {
            Feed::Decode(d) => d.total,
            Feed::Copy(c) => c.total,
        }
    }

    /// What to say when the source's audio ran out before the trim's end.
    pub fn ended_early(&self) -> Option<Warning> {
        let at = |samples: u64| samples as f64 / f64::from(self.rate());
        match self {
            Feed::Decode(d) => d.ended_at.map(|s| {
                format!(
                    "the clip's audio ends {:.3} s into the export: silence after",
                    at(s)
                )
            }),
            Feed::Copy(c) => c.ended_at.map(|s| {
                format!(
                    "the clip's audio ends {:.3} s into the export, and so does the copy",
                    at(s)
                )
            }),
        }
        .map(|m| Warning::new(codes::AUDIO_ENDS_EARLY, m))
    }

    /// Hand the writer the samples up to export time `upto`, or all of
    /// them. `false` when stopped.
    pub fn feed(
        &mut self,
        writer: &mut Mp4Writer,
        upto: Option<Time>,
        token: &StopToken,
    ) -> Result<bool, ExportError> {
        let upto = match upto {
            Some(t) => u64::try_from(sample(t, self.rate())?).unwrap_or(0),
            None => u64::MAX,
        };
        match self {
            Feed::Decode(d) => d.feed(writer, upto, token),
            Feed::Copy(c) => c.feed(writer, upto, token),
        }
    }
}

/// The trim's packets, copied.
#[derive(Debug)]
pub(crate) struct Copying {
    copy: AudioCopy,
    /// Samples in all.
    total: u64,
    /// A packet read but not yet due.
    held: Option<spherecut_media::copy::Copied>,
    /// Where the last packet written ends.
    written: u64,
    done: bool,
    /// Where the stream ended, before the trim's end.
    ended_at: Option<u64>,
}

impl Copying {
    fn open(
        path: &Path,
        plan: &Plan,
        trim: Range,
        origin: Time,
        interrupt: Arc<AtomicBool>,
    ) -> Result<Copying, ExportError> {
        let failed = |e: &dyn std::fmt::Display| {
            ExportError::failed(Stage::Decode, None, format!("the audio: {e}"))
        };
        let mut copy = AudioCopy::open(path, plan.stream, interrupt).map_err(|e| failed(&e))?;
        let rate = copy.rate();
        // The trim on the stream's own axis, where the video's first frame
        // is at `origin`.
        let in_file = |t: Time| {
            origin
                .checked_add(t)
                .map_err(|e| ExportError::Invalid(format!("the trim's audio: {e}")))
                .and_then(|t| sample(t, rate))
        };
        let (start, end) = (in_file(trim.start)?, in_file(trim.end)?);
        copy.cut(start, end).map_err(|e| failed(&e))?;
        Ok(Copying {
            copy,
            total: u64::try_from(end - start).unwrap_or(0),
            held: None,
            written: 0,
            done: false,
            ended_at: None,
        })
    }

    /// Write the packets that start before sample `upto`.
    fn feed(
        &mut self,
        writer: &mut Mp4Writer,
        upto: u64,
        token: &StopToken,
    ) -> Result<bool, ExportError> {
        loop {
            let c = match self.held.take() {
                Some(c) => c,
                None if self.done => return Ok(true),
                None => match self.copy.next_packet() {
                    Ok(NextCopied::Packet(c)) => c,
                    Ok(NextCopied::End) => {
                        self.done = true;
                        if self.written < self.total {
                            self.ended_at = Some(self.written);
                        }
                        return Ok(true);
                    }
                    Ok(NextCopied::Stopped) => return Ok(false),
                    Err(_) if token.is_stopped() => return Ok(false),
                    Err(e) => return Err(self.failed(Stage::Decode, e.to_string())),
                },
            };
            if u64::try_from(c.pts()).is_ok_and(|p| p >= upto) {
                self.held = Some(c);
                return Ok(true);
            }
            self.written = u64::try_from(c.pts() + c.duration()).unwrap_or(0);
            writer.push_copied(c).map_err(|e| {
                let stage = if matches!(e, EncodeError::Write(_)) {
                    Stage::File
                } else {
                    Stage::Mux
                };
                self.failed(stage, e.to_string())
            })?;
        }
    }

    fn failed(&self, stage: Stage, message: String) -> ExportError {
        let at = Rational::new(self.written as i64, i64::from(self.copy.rate()))
            .map_or(Time::ZERO, Time::seconds);
        ExportError::failed(stage, Some(at), format!("the audio: {message}"))
    }
}

/// The trim's audio, decoded at [`RATE`] for the encoder.
#[derive(Debug)]
pub(crate) struct Decoded {
    settings: AudioSettings,
    dec: FfmpegAudio,
    channels: usize,
    /// Output samples in all: the trim's length at [`RATE`], to within a
    /// sample.
    total: u64,
    /// Output samples pushed so far.
    pushed: u64,
    /// What is left of the last chunk, and where in it.
    held: Option<(AudioChunk, usize)>,
    /// The output sample the next one decoded is, on the clip's axis.
    next: i64,
    /// Where the stream ended, before the trim's end.
    ended_at: Option<u64>,
}

impl Decoded {
    fn open(
        path: &Path,
        plan: &Plan,
        trim: Range,
        origin: Time,
        interrupt: Arc<AtomicBool>,
        token: &StopToken,
    ) -> Result<Decoded, ExportError> {
        let req = AudioRequest {
            stream: Some(plan.stream),
            out: AudioOutFormat {
                rate: RATE,
                channels: plan.channels(),
            },
            origin: Some(origin),
            interrupt,
        };
        let open = if plan.ambix {
            FfmpegAudio::open_unmixed(path, &req)
        } else {
            FfmpegAudio::open(path, &req)
        };
        let decode_err = |e: &dyn std::fmt::Display| {
            ExportError::failed(Stage::Decode, None, format!("the audio: {e}"))
        };
        let mut dec = open.map_err(|e| decode_err(&e))?;
        let channels = usize::from(dec.info().channels);
        if plan.ambix && channels != 4 {
            return Err(decode_err(&format!(
                "an ambiX stream decodes {channels} channels"
            )));
        }
        let (first, end) = (sample(trim.start, RATE)?, sample(trim.end, RATE)?);
        dec.seek(trim.start, token).map_err(|e| decode_err(&e))?;
        Ok(Decoded {
            settings: plan.settings(),
            dec,
            channels: usize::from(plan.channels()),
            total: u64::try_from(end - first).unwrap_or(0),
            pushed: 0,
            held: None,
            next: first,
            ended_at: None,
        })
    }

    /// Push the samples up to output sample `upto` (at most the total).
    /// `false` when stopped.
    fn feed(
        &mut self,
        writer: &mut Mp4Writer,
        upto: u64,
        token: &StopToken,
    ) -> Result<bool, ExportError> {
        let upto = upto.min(self.total);
        while self.pushed < upto {
            let want = (upto - self.pushed) as usize;
            let (chunk, at) = match self.held.take() {
                Some(h) => h,
                None if self.ended_at.is_some() => {
                    self.push(writer, None, want)?;
                    continue;
                }
                None => match self.dec.next_chunk(token) {
                    Ok(NextAudio::Chunk(c)) => {
                        if c.start != self.next {
                            return Err(self.failed(format!(
                                "a chunk at sample {} where {} should be",
                                c.start, self.next
                            )));
                        }
                        self.next += (c.data.len() / self.channels.max(1)) as i64;
                        (c, 0)
                    }
                    Ok(NextAudio::End) => {
                        self.ended_at = Some(self.pushed);
                        continue;
                    }
                    Ok(NextAudio::Stopped) => return Ok(false),
                    Err(_) if token.is_stopped() => return Ok(false),
                    Err(e) => return Err(self.failed(e.to_string())),
                },
            };
            let frames = chunk.data.len() / self.channels.max(1);
            let n = (frames - at).min(want);
            let interleaved = &chunk.data[at * self.channels..(at + n) * self.channels];
            self.push(writer, Some(interleaved), n)?;
            if at + n < frames {
                self.held = Some((chunk, at + n));
            }
        }
        Ok(true)
    }

    /// Push `n` frames of `interleaved`, or of silence.
    fn push(
        &mut self,
        writer: &mut Mp4Writer,
        interleaved: Option<&[f32]>,
        n: usize,
    ) -> Result<(), ExportError> {
        let planar: Vec<Vec<f32>> = (0..self.channels)
            .map(|c| match interleaved {
                Some(d) => d.iter().skip(c).step_by(self.channels).copied().collect(),
                None => vec![0.0; n],
            })
            .collect();
        writer.push_audio(&planar).map_err(|e| {
            let stage = if matches!(e, EncodeError::Write(_)) {
                Stage::File
            } else {
                Stage::Encode
            };
            ExportError::failed(stage, Some(self.time()), format!("the audio: {e}"))
        })?;
        self.pushed += n as u64;
        Ok(())
    }

    /// The export's time at the next sample.
    fn time(&self) -> Time {
        Rational::new(self.pushed as i64, i64::from(RATE)).map_or(Time::ZERO, Time::seconds)
    }

    fn failed(&self, message: String) -> ExportError {
        ExportError::failed(
            Stage::Decode,
            Some(self.time()),
            format!("the audio: {message}"),
        )
    }
}
