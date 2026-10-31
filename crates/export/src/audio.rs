//! The export's audio (M2 plan section 8.5): the clip's first audio
//! stream, or its ambiX one when it has one, from the trim's start to its
//! end at 48 kHz, encoded as AAC on the encode stage's thread between
//! video frames, so that the muxer interleaves the two as it goes.
//!
//! A 360 export keeps first-order ambiX as four channels, labelled `4.0`
//! (FFmpeg's AAC encoder refuses an ambisonic layout) and without
//! intensity stereo or noise substitution, which would smear the
//! components; the file says ambiX in an `SA3D` box. A flat export
//! hears ambiX as media's stereo decode does (two cardioids at ±90°),
//! and any other layout as stereo.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use spherecut_core::cancel::StopToken;
use spherecut_core::project::{AudioChoice, Orientation, Output, Range};
use spherecut_core::time::{Rational, Rounding, Time};
use spherecut_media::audio::FfmpegAudio;
use spherecut_media::backend::{AudioChunk, AudioDecode, AudioOutFormat, AudioRequest, NextAudio};
use spherecut_media::encode::{AudioSettings, Mp4Writer};
use spherecut_media::probe::{self, StreamKind};

use crate::error::{ExportError, Stage};

/// The export's sample rate, and its movie timescale, so that an edit
/// can end on a sample (amendment 2 item 7).
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
}

impl Plan {
    pub fn channels(&self) -> u16 {
        if self.ambix { 4 } else { 2 }
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

/// What the export does with `path`'s audio for `choice` into `output`:
/// the plan, `None` for no audio, and what it did that wasn't asked.
pub(crate) fn choose(
    path: &Path,
    choice: AudioChoice,
    output: &Output,
) -> Result<(Option<Plan>, Vec<String>), ExportError> {
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
        warnings.push("the clip has no audio".into());
        return Ok((None, warnings));
    };
    let ambix = is_ambix(&stream) && matches!(output, Output::Equirect { .. });
    if choice == AudioChoice::Copy {
        // Copying comes with the extended injector (M2.4); until then the
        // audio is encoded again, as AAC.
        warnings.push("the audio is encoded again as AAC: copying it isn't in this build".into());
    }
    if ambix
        && let Output::Equirect { orientation, .. } = output
        && *orientation != Orientation::IDENTITY
    {
        warnings.push("the ambiX sound field is not turned with the picture's orientation".into());
    }
    Ok((
        Some(Plan {
            stream: stream.index,
            ambix,
        }),
        warnings,
    ))
}

/// The output sample `t` falls on, at [`RATE`].
pub(crate) fn sample(t: Time) -> Result<i64, ExportError> {
    t.as_rational()
        .checked_mul(Rational::from_integer(i64::from(RATE)))
        .map(|r| r.round(Rounding::Nearest))
        .map_err(|e| ExportError::Invalid(format!("the trim's audio: {e}")))
}

/// The trim's audio, decoded and handed to the writer as far as each
/// video frame needs.
#[derive(Debug)]
pub(crate) struct Feed {
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
        let (first, end) = (sample(trim.start)?, sample(trim.end)?);
        dec.seek(trim.start, token).map_err(|e| decode_err(&e))?;
        Ok(Feed {
            dec,
            channels: usize::from(plan.channels()),
            total: u64::try_from(end - first).unwrap_or(0),
            pushed: 0,
            held: None,
            next: first,
            ended_at: None,
        })
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// Where the source's audio ran out, in output samples, if it did
    /// before the trim's end: silence follows.
    pub fn ended_at(&self) -> Option<u64> {
        self.ended_at
    }

    /// Push the samples up to output sample `upto` (at most the total).
    /// `false` when stopped.
    pub fn feed(
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
            ExportError::failed(Stage::Encode, Some(self.time()), format!("the audio: {e}"))
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
