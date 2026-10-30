//! A stream's packets as the demuxer hands them out, without decoding
//! them: their timestamps, sizes and keyframe flags, for the agent's
//! timing analysis (a1-plan section 6.3). The file is opened through the
//! confinement (D24), and the demuxer reads no other stream's data.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use spherecut_core::Rational;

use crate::{MediaError, ff, ffi, init};

/// One packet of the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketInfo {
    /// In ticks of [`PacketReader::time_base`].
    pub pts: Option<i64>,
    pub dts: Option<i64>,
    /// In ticks; 0 when the container doesn't say.
    pub duration: i64,
    /// Bytes of coded data.
    pub size: usize,
    pub key: bool,
    /// The demuxer marked it damaged (cut short, say).
    pub corrupt: bool,
}

/// What [`PacketReader::next_packet`] read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextPacket {
    Packet(PacketInfo),
    /// The end of the stream. `damaged` when a read failed before the end
    /// of the file, so that the packets after it couldn't be read.
    End {
        damaged: bool,
    },
    /// The interrupt flag was raised during a read.
    Stopped,
}

/// One stream's packets, read in file order.
pub struct PacketReader {
    path: PathBuf,
    ictx: ff::format::context::Input,
    index: usize,
    time_base: Rational,
    start_pts: Option<i64>,
}

impl std::fmt::Debug for PacketReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PacketReader")
            .field("path", &self.path)
            .field("index", &self.index)
            .field("time_base", &self.time_base)
            .finish_non_exhaustive()
    }
}

impl PacketReader {
    /// Stream `index` of the file at `path`. FFmpeg's reads stop once
    /// `interrupt` is raised.
    pub fn open(path: &Path, index: usize, interrupt: Arc<AtomicBool>) -> Result<Self, MediaError> {
        init();
        let mut ictx = ffi::open_input(path, Some(interrupt))
            .map_err(|e| MediaError::opening(path, e, |e| e.to_string()))?;
        let st = ictx.stream(index).ok_or_else(|| MediaError::NoStream {
            path: path.to_path_buf(),
            kind: "such",
        })?;
        let tb = st.time_base();
        let time_base = Rational::new(i64::from(tb.numerator()), i64::from(tb.denominator()))
            .map_err(|e| MediaError::Decode {
                path: path.to_path_buf(),
                reason: format!("the stream's time base: {e}"),
            })?;
        let start = st.start_time();
        let start_pts = (start != ff::ffi::AV_NOPTS_VALUE).then_some(start);
        ffi::discard_other_streams(&mut ictx, index);
        Ok(PacketReader {
            path: path.to_path_buf(),
            ictx,
            index,
            time_base,
            start_pts,
        })
    }

    /// Seconds per tick of the packets' timestamps.
    pub fn time_base(&self) -> Rational {
        self.time_base
    }

    /// The stream's first presented timestamp, as the container gives it.
    pub fn start_pts(&self) -> Option<i64> {
        self.start_pts
    }

    /// Go to the keyframe at or before `pts`, or the start.
    pub fn seek(&mut self, pts: i64) -> Result<(), MediaError> {
        spherecut_core::may_block("seeking in a media file");
        if ffi::seek_frame(&mut self.ictx, self.index, pts, true).is_err() {
            ffi::seek_frame(&mut self.ictx, self.index, i64::MIN, true)
                .or_else(|_| ffi::seek_to_byte_zero(&mut self.ictx))
                .map_err(|e| MediaError::Decode {
                    path: self.path.clone(),
                    reason: format!("seeking to read packets: {e}"),
                })?;
        }
        Ok(())
    }

    /// The stream's next packet that is shown: packets an edit list hides
    /// (`AV_PKT_FLAG_DISCARD`) are passed over.
    pub fn next_packet(&mut self) -> NextPacket {
        spherecut_core::may_block("reading a media file");
        loop {
            let mut pkt = ff::Packet::empty();
            match pkt.read(&mut self.ictx) {
                Ok(()) if pkt.stream() == self.index && !ffi::packet_discarded(&pkt) => {
                    return NextPacket::Packet(PacketInfo {
                        pts: pkt.pts(),
                        dts: pkt.dts(),
                        duration: pkt.duration(),
                        size: pkt.size(),
                        key: pkt.is_key(),
                        corrupt: pkt.is_corrupt(),
                    });
                }
                Ok(()) => {}
                Err(ff::Error::Eof) => return NextPacket::End { damaged: false },
                Err(ff::Error::Exit) => return NextPacket::Stopped,
                Err(e) => {
                    tracing::warn!(path = %self.path.display(), "reading packets: {e}");
                    return NextPacket::End { damaged: true };
                }
            }
        }
    }
}
