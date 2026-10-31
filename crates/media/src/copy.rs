//! An audio stream's packets, copied into an export without decoding them
//! and cut to a trim at its exact samples (M2 plan section 8.5):
//!
//! - AAC from the packet before the one that holds the trim's first
//!   sample, which the decoder needs to rebuild it (AAC's windows
//!   overlap by a packet). The packets are stamped from the trim's start,
//!   so the first one or two come before 0, and FFmpeg's muxer writes an
//!   `elst` whose `media_time` skips to the first sample exactly
//!   (encode.md section 6). The last packet's duration is cut to end on
//!   the trim's end, where the muxer's edit then ends too: it runs to the
//!   end of the last packet.
//! - PCM cut at the exact samples, from the trim's start at 0.
//!
//! Nothing else is copied: how Opus's pre-skip and an edit list combine
//! is unverified (encode.md section 6), and no other codec is known to
//! cut exactly. The stream's ticks must be its samples (a time base of
//! 1/rate), as an MP4 or MOV's are.
//!
//! The file is opened through the confinement (D24), and the demuxer reads
//! no other stream's data.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use thiserror::Error;

use crate::{MediaError, ff, ffi, init};

/// Why a stream can't be copied, or couldn't be read.
#[derive(Debug, Error)]
pub enum CopyError {
    /// The stream can't be copied exactly into an MP4: why, as a clause
    /// ("Opus audio isn't copied: ...").
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Media(#[from] MediaError),
}

/// A packet for [`Mp4Writer::push_copied`](crate::encode::Mp4Writer::push_copied),
/// stamped in the stream's samples from the trim's start.
pub struct Copied {
    pub(crate) packet: ff::Packet,
}

impl Copied {
    /// Where it starts: before 0 for AAC's pre-roll.
    pub fn pts(&self) -> i64 {
        self.packet.pts().unwrap_or(0)
    }

    /// Samples it covers.
    pub fn duration(&self) -> i64 {
        self.packet.duration()
    }

    /// Bytes of coded data.
    pub fn size(&self) -> usize {
        self.packet.size()
    }
}

impl std::fmt::Debug for Copied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Copied")
            .field("pts", &self.pts())
            .field("duration", &self.duration())
            .field("size", &self.size())
            .finish()
    }
}

/// What [`AudioCopy::next_packet`] read.
#[derive(Debug)]
pub enum NextCopied {
    Packet(Copied),
    /// The trim's last packet has been handed out, or the stream ended.
    End,
    /// The interrupt flag was raised during a read.
    Stopped,
}

/// One audio stream's packets, cut to a trim.
pub struct AudioCopy {
    path: PathBuf,
    ictx: ff::format::context::Input,
    index: usize,
    pub(crate) params: ff::codec::Parameters,
    codec: String,
    /// Bytes per sample frame, for PCM; `None` for AAC.
    pcm: Option<usize>,
    rate: u32,
    channels: u16,
    /// The trim, in the stream's samples: its first and the one after its
    /// last.
    cut: (i64, i64),
    /// The last AAC packet read that ends at or before the trim's start:
    /// the first one's pre-roll.
    before: Option<ff::Packet>,
    /// The first packet in the trim, held while its pre-roll goes first.
    held: Option<Copied>,
    begun: bool,
    done: bool,
}

impl std::fmt::Debug for AudioCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioCopy")
            .field("path", &self.path)
            .field("index", &self.index)
            .field("codec", &self.codec)
            .field("rate", &self.rate)
            .field("cut", &self.cut)
            .finish_non_exhaustive()
    }
}

/// Bytes per sample of the PCM codecs an MP4 can hold (`ipcm`, `fpcm`).
fn pcm_bytes(id: ff::codec::Id) -> Option<usize> {
    use ff::codec::Id;
    match id {
        Id::PCM_S16LE | Id::PCM_S16BE => Some(2),
        Id::PCM_S24LE | Id::PCM_S24BE => Some(3),
        Id::PCM_S32LE | Id::PCM_S32BE | Id::PCM_F32LE | Id::PCM_F32BE => Some(4),
        Id::PCM_F64LE | Id::PCM_F64BE => Some(8),
        _ => None,
    }
}

impl AudioCopy {
    /// Stream `index` of the file at `path`, if it can be copied exactly.
    /// FFmpeg's reads stop once `interrupt` is raised.
    pub fn open(path: &Path, index: usize, interrupt: Arc<AtomicBool>) -> Result<Self, CopyError> {
        init();
        let mut ictx = ffi::open_input(path, Some(interrupt))
            .map_err(|e| MediaError::opening(path, e, |e| e.to_string()))?;
        let st = ictx.stream(index).ok_or_else(|| MediaError::NoStream {
            path: path.to_path_buf(),
            kind: "such",
        })?;
        let params = st.parameters().clone();
        let tb = st.time_base();
        let fields = ffi::codecpar_fields(&params);
        let codec = fields
            .codec_name
            .clone()
            .unwrap_or_else(|| "unknown".into());
        let id = params.id();
        let refused = |why: String| Err(CopyError::Refused(why));
        if ffi::media_type(&params) != ff::media::Type::Audio {
            return refused(format!("stream {index} isn't audio"));
        }
        let pcm = match id {
            ff::codec::Id::AAC => None,
            ff::codec::Id::OPUS => {
                return refused(
                    "Opus audio isn't copied: how its pre-skip and the edit list combine is unverified"
                        .into(),
                );
            }
            id => match pcm_bytes(id) {
                Some(bytes) => Some(bytes),
                None => {
                    return refused(format!(
                        "{codec} audio isn't copied: only AAC and PCM are cut exactly"
                    ));
                }
            },
        };
        if !ffi::mp4_takes(id) {
            return refused(format!("this FFmpeg's MP4 muxer doesn't store {codec}"));
        }
        let (rate, channels) = match (
            u32::try_from(fields.sample_rate),
            u16::try_from(fields.channels),
        ) {
            (Ok(r), Ok(c)) if r > 0 && c > 0 => (r, c),
            _ => {
                return refused(format!(
                    "the stream says {} Hz and {} channels",
                    fields.sample_rate, fields.channels
                ));
            }
        };
        if tb.numerator() != 1 || u32::try_from(tb.denominator()).ok() != Some(rate) {
            return refused(format!(
                "its timestamps (a tick of {}/{} s) aren't its samples",
                tb.numerator(),
                tb.denominator()
            ));
        }
        ffi::discard_other_streams(&mut ictx, index);
        Ok(AudioCopy {
            path: path.to_path_buf(),
            ictx,
            index,
            params,
            codec,
            pcm: pcm.map(|b| b * usize::from(channels)),
            rate,
            channels,
            cut: (0, 0),
            before: None,
            held: None,
            begun: false,
            done: true,
        })
    }

    /// The codec, as FFmpeg names it (`aac`, `pcm_s24le`).
    pub fn codec(&self) -> &str {
        &self.codec
    }

    pub fn is_pcm(&self) -> bool {
        self.pcm.is_some()
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Bits a second, for an estimate of the copy's size: PCM's exactly;
    /// AAC's as the stream says or, where it doesn't, the most AAC holds
    /// (6,144 bits a channel for 1,024 samples).
    pub fn bit_rate(&self) -> u64 {
        let (rate, channels) = (u64::from(self.rate), u64::from(self.channels));
        match self.pcm {
            Some(frame) => rate * frame as u64 * 8,
            None => u64::try_from(self.params.bit_rate())
                .ok()
                .filter(|&b| b > 0)
                .unwrap_or(rate * channels * 6),
        }
    }

    /// Hand out the packets of samples `start` to `end` (not included), in
    /// the stream's ticks, from the next [`next_packet`](Self::next_packet).
    pub fn cut(&mut self, start: i64, end: i64) -> Result<(), MediaError> {
        spherecut_core::may_block("seeking in a media file");
        // A tenth of a second before, for the packet before the start.
        let from = start.saturating_sub(i64::from(self.rate / 10));
        if ffi::seek_frame(&mut self.ictx, self.index, from, true).is_err() {
            ffi::seek_frame(&mut self.ictx, self.index, i64::MIN, true)
                .or_else(|_| ffi::seek_to_byte_zero(&mut self.ictx))
                .map_err(|e| self.failed(format!("seeking to copy audio: {e}")))?;
        }
        self.cut = (start, end);
        self.before = None;
        self.held = None;
        self.begun = false;
        self.done = false;
        Ok(())
    }

    /// The next packet of the trim.
    pub fn next_packet(&mut self) -> Result<NextCopied, MediaError> {
        spherecut_core::may_block("reading a media file");
        if let Some(c) = self.held.take() {
            return Ok(NextCopied::Packet(c));
        }
        let (start, end) = self.cut;
        loop {
            if self.done {
                return Ok(NextCopied::End);
            }
            let mut pkt = ff::Packet::empty();
            match pkt.read(&mut self.ictx) {
                Ok(()) if pkt.stream() == self.index => {}
                Ok(()) => continue,
                Err(ff::Error::Eof) => {
                    self.done = true;
                    continue;
                }
                Err(ff::Error::Exit) => return Ok(NextCopied::Stopped),
                Err(e) => return Err(self.failed(format!("reading audio to copy: {e}"))),
            }
            let pts = pkt
                .pts()
                .ok_or_else(|| self.failed("an audio packet without a time".into()))?;
            if pts >= end {
                self.done = true;
                continue;
            }
            let data = pkt.data().unwrap_or_default();
            if let Some(frame) = self.pcm {
                // Every sample stands alone: keep the trim's.
                let n = (data.len() / frame) as i64;
                let (from, to) = (pts.max(start), (pts + n).min(end));
                if from >= to {
                    continue;
                }
                self.done = to == end;
                let bytes = &data[((from - pts) as usize * frame)..((to - pts) as usize * frame)];
                return Ok(NextCopied::Packet(stamped(bytes, from - start, to - from)));
            }
            let duration = pkt.duration();
            if duration <= 0 {
                return Err(self.failed(format!("an AAC packet at {pts} without a duration")));
            }
            if pts + duration <= start {
                self.before = Some(pkt);
                continue;
            }
            // The last packet ends on the trim's end.
            let shown = if pts + duration >= end {
                self.done = true;
                end - pts
            } else {
                duration
            };
            let c = stamped(data, pts - start, shown);
            if !self.begun {
                self.begun = true;
                if let Some(b) = self.before.take() {
                    let b_pts = b.pts().unwrap_or(pts - duration);
                    self.held = Some(c);
                    let pre = stamped(b.data().unwrap_or_default(), b_pts - start, b.duration());
                    return Ok(NextCopied::Packet(pre));
                }
            }
            return Ok(NextCopied::Packet(c));
        }
    }

    fn failed(&self, reason: String) -> MediaError {
        MediaError::Decode {
            path: self.path.clone(),
            reason,
        }
    }
}

/// A new packet of `data` at `pts` for `duration`: none of the source's
/// side data (its skip samples belong to its own edit) or discard flag.
fn stamped(data: &[u8], pts: i64, duration: i64) -> Copied {
    let mut packet = ff::Packet::copy(data);
    packet.set_pts(Some(pts));
    packet.set_dts(Some(pts));
    packet.set_duration(duration);
    packet.set_flags(ff::packet::Flags::KEY);
    Copied { packet }
}
