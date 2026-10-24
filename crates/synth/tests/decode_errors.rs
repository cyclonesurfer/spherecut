//! `VideoDecoder` on files it can't read to the end: a damaged file must
//! end with an error, never pass for a shorter one.
//!
//! The files are cut at offsets taken from their own sample tables, at
//! every video sample, so what each test expects doesn't depend on how
//! the encoder or muxer laid out the bytes.

// Test helpers outside #[test] functions; panicking is the point here.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use spherecut_core::FrameRate;
use spherecut_media::MediaError;
use spherecut_media::decode::VideoDecoder;
use spherecut_media::ff;
use spherecut_synth::Pattern;
use spherecut_synth::media::{self, Audio, Codec, Spec};

/// A test's own directory in the system's temp dir, removed with what it
/// holds when dropped, so also when the test fails.
struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn tmp(name: &str) -> TempDir {
    let d = std::env::temp_dir().join(format!(
        "spherecut-decode-errors-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    TempDir(d)
}

#[test]
fn test_directories_are_removed_when_dropped() {
    let dir = tmp("dropped");
    std::fs::write(dir.join("left.mp4"), b"").unwrap();
    let path = dir.to_path_buf();
    drop(dir);
    assert!(!path.exists());
}

/// An H.264 clip with stereo audio.
fn clip(dir: &Path, name: &str, frames: u32) -> PathBuf {
    let spec = Spec {
        name: name.into(),
        pattern: Pattern::Grid,
        width: 512,
        height: 256,
        rate: FrameRate::FPS_30,
        frames,
        codec: Codec::H264,
        bits: 8,
        audio: Audio::Stereo,
        spherical: false,
    };
    media::write(&spec, dir).unwrap()
}

#[derive(Clone, Copy, PartialEq)]
enum Layout {
    /// `moov` before `mdat`, samples interleaved like a camera file's.
    Faststart,
    /// `moov` after `mdat`.
    MoovAtEnd,
    /// `moov` first, then every video sample, then every audio sample.
    VideoThenAudio,
}

/// Remux `src` into `dst` laid out as `layout` says.
// Reads a file this test wrote with ffmpeg-next alone, apart from
// Spherecut's confined open (D24).
#[allow(clippy::disallowed_methods)]
fn remux(src: &Path, dst: &Path, layout: Layout) {
    let mut ictx = ff::format::input(src).unwrap();
    let video = ictx.streams().best(ff::media::Type::Video).unwrap().index();
    let mut octx = ff::format::output(dst).unwrap();
    for st in ictx.streams() {
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .unwrap();
        out.set_parameters(st.parameters());
        out.set_time_base(st.time_base());
    }
    let mut opts = ff::Dictionary::new();
    if layout != Layout::MoovAtEnd {
        opts.set("movflags", "faststart");
    }
    octx.write_header_with(opts).unwrap();
    let mut packets: Vec<_> = ictx
        .packets()
        .map(|(st, mut pkt)| {
            pkt.rescale_ts(st.time_base(), octx.stream(st.index()).unwrap().time_base());
            pkt.set_position(-1);
            pkt
        })
        .collect();
    if layout == Layout::VideoThenAudio {
        // A stable sort, so each stream keeps its order.
        packets.sort_by_key(|p| p.stream() != video);
        for pkt in packets {
            pkt.write(&mut octx).unwrap();
        }
    } else {
        for pkt in packets {
            pkt.write_interleaved(&mut octx).unwrap();
        }
    }
    octx.write_trailer().unwrap();
}

/// Where each sample of `path`'s best stream of type `kind` lies, as
/// (offset, size), in file order.
#[allow(clippy::disallowed_methods)] // As in `remux`.
fn samples(path: &Path, kind: ff::media::Type) -> Vec<(u64, u64)> {
    let mut ictx = ff::format::input(path).unwrap();
    let index = ictx.streams().best(kind).unwrap().index();
    let mut v: Vec<_> = ictx
        .packets()
        .filter(|(st, _)| st.index() == index)
        .map(|(_, p)| (p.position() as u64, p.size() as u64))
        .collect();
    v.sort_unstable();
    v
}

/// Decode until the stream ends: the frame count, and the error if it
/// ended with one.
fn decode(d: &mut VideoDecoder) -> (usize, Option<MediaError>) {
    let mut n = 0;
    loop {
        match d.next_frame() {
            Ok(Some(_)) => n += 1,
            Ok(None) => return (n, None),
            Err(e) => return (n, Some(e)),
        }
    }
}

fn decode_reason(e: &MediaError) -> &str {
    match e {
        MediaError::Decode { reason, .. } => reason,
        e => panic!("expected a decode error, got {e:?}"),
    }
}

/// Decode `path` and check that it decodes whole: `frames` frames, then
/// the end of the stream.
fn assert_decodes_whole(path: &Path, frames: usize) {
    let mut d = VideoDecoder::open(path, None, None).unwrap();
    let (n, e) = decode(&mut d);
    assert!(e.is_none(), "{}: {e:?}", path.display());
    assert_eq!(n, frames, "{}", path.display());
    assert_eq!(d.skipped_packets(), 0);
}

#[test]
fn a_file_cut_in_its_video_data_ends_with_an_error_after_the_whole_frames() {
    let dir = tmp("cut");
    let src = clip(&dir, "t-cut", 30);
    let fast = dir.join("t-cut-faststart.mp4");
    remux(&src, &fast, Layout::Faststart);
    assert_decodes_whole(&fast, 30);
    let bytes = std::fs::read(&fast).unwrap();
    let video = samples(&fast, ff::media::Type::Video);
    assert_eq!(video.len(), 30);
    let video_end = video.iter().map(|(pos, size)| pos + size).max().unwrap();

    // Cut where each video sample starts, in its middle, and in whatever
    // lies between it and the one before (audio, in this file). The moov
    // survives every cut, and so does each sample before the cut.
    let cut = dir.join("t-cut-part.mp4");
    let mut between = 0;
    for k in 1..video.len() {
        let (pos, size) = video[k];
        let prev_end = video[k - 1].0 + video[k - 1].1;
        let mut cuts = vec![pos, pos + size / 2];
        if prev_end < pos {
            between += 1;
            cuts.push((prev_end + pos) / 2);
        }
        for at in cuts {
            std::fs::write(&cut, &bytes[..at as usize]).unwrap();
            let mut d = VideoDecoder::open(&cut, None, None).unwrap();
            let (n, e) = decode(&mut d);
            let Some(e) = e else {
                panic!("cut at byte {at}, in sample {k}: ended like a whole file after {n} frames");
            };
            // The k whole samples decode; the one the cut falls in, if any,
            // is not decoded.
            let want = format!(
                "reading: the file is truncated: it ends at byte {at}, but its index \
                 lists video data up to byte {video_end} (after {k} frames)"
            );
            assert_eq!(decode_reason(&e), want, "cut in sample {k}");
            assert_eq!(n, k, "cut at byte {at}");
            assert_eq!(d.skipped_packets(), 0);
            // The stream stays ended with the same error.
            assert_eq!(decode_reason(&d.next_frame().unwrap_err()), want);
        }
    }
    assert!(between > 0, "the audio must be interleaved with the video");
}

#[test]
fn with_moov_at_the_end_a_cut_in_the_media_data_fails_to_open() {
    let dir = tmp("tail");
    let src = clip(&dir, "t-tail", 30);
    let tail = dir.join("t-tail-moov-last.mp4");
    remux(&src, &tail, Layout::MoovAtEnd);
    assert_decodes_whole(&tail, 30);
    let bytes = std::fs::read(&tail).unwrap();
    let video = samples(&tail, ff::media::Type::Video);
    let cut = dir.join("t-tail-part.mp4");
    std::fs::write(&cut, &bytes[..video[15].0 as usize]).unwrap();
    assert!(matches!(
        VideoDecoder::open(&cut, None, None),
        Err(MediaError::Open { .. })
    ));
}

#[test]
fn damage_in_another_stream_does_not_end_the_video_early() {
    let dir = tmp("other");
    // Longer than a second: within one, the MOV demuxer reads samples in
    // file order, so all of this file's video would come first anyway.
    let src = clip(&dir, "t-other", 90);
    let split = dir.join("t-other-split.mp4");
    remux(&src, &split, Layout::VideoThenAudio);
    assert_decodes_whole(&split, 90);
    let bytes = std::fs::read(&split).unwrap();
    let video = samples(&split, ff::media::Type::Video);
    let audio = samples(&split, ff::media::Type::Audio);
    let video_end = video.iter().map(|(pos, size)| pos + size).max().unwrap();
    assert!(audio[0].0 >= video_end, "the audio must follow the video");

    // Cut in the audio: every video sample is still in the file, so every
    // frame decodes and the stream ends cleanly.
    let cut = dir.join("t-other-part.mp4");
    for (pos, size) in [audio[0], audio[audio.len() / 2]] {
        let at = pos + size / 2;
        std::fs::write(&cut, &bytes[..at as usize]).unwrap();
        assert_decodes_whole(&cut, 90);
    }
}

#[test]
fn a_cancelled_read_is_an_error_not_the_end_of_the_stream() {
    let dir = tmp("cancel");
    let path = clip(&dir, "t-cancel", 120);
    // A cancel takes effect at the next read from the file, which FFmpeg
    // reads 32 KiB at a time. The cancels below come in the first third of
    // a clip several times that size, so reads are still to come.
    let len = std::fs::metadata(&path).unwrap().len();
    assert!(len > 3 * 32 * 1024, "{len} bytes");

    // Uncancelled, the file decodes to the end.
    let mut d = VideoDecoder::open_cancellable(&path, None, None, Arc::default()).unwrap();
    let (n, e) = decode(&mut d);
    assert!(e.is_none(), "{e:?}");
    assert_eq!(n, 120);

    // Cancelled before the first frame or after some, the stream ends with
    // that error, whichever read the cancel stops and whichever sample that
    // read leaves unfinished.
    for after in [0, 1, 10, 20, 30, 40] {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut d = VideoDecoder::open_cancellable(&path, None, None, cancel.clone()).unwrap();
        for _ in 0..after {
            assert!(d.next_frame().unwrap().is_some());
        }
        cancel.store(true, Ordering::Relaxed);
        let (n, e) = decode(&mut d);
        let Some(e) = e else {
            panic!("cancelled after {after} frames, ended like the file did after {n} more");
        };
        let reason = decode_reason(&e).to_string();
        assert_eq!(
            reason,
            format!("reading: cancelled (after {} frames)", after + n)
        );
        assert_eq!(d.skipped_packets(), 0);
        assert_eq!(decode_reason(&d.next_frame().unwrap_err()), reason);
    }
}
