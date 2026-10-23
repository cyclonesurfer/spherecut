//! `VideoDecoder` on files it can't read to the end: a damaged file must
//! end with an error, never pass for a shorter one.

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

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "spherecut-decode-errors-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An H.264 clip with stereo audio, interleaved like a camera file.
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

/// Remux `src` with `moov` before `mdat`, so that a cut in the media data
/// leaves an index FFmpeg can open.
fn faststart(src: &Path, dst: &Path) {
    let mut ictx = ff::format::input(src).unwrap();
    let mut octx = ff::format::output(dst).unwrap();
    for st in ictx.streams() {
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .unwrap();
        out.set_parameters(st.parameters());
        out.set_time_base(st.time_base());
    }
    let mut opts = ff::Dictionary::new();
    opts.set("movflags", "faststart");
    octx.write_header_with(opts).unwrap();
    for (st, mut pkt) in ictx.packets() {
        pkt.rescale_ts(st.time_base(), octx.stream(st.index()).unwrap().time_base());
        pkt.set_position(-1);
        pkt.write_interleaved(&mut octx).unwrap();
    }
    octx.write_trailer().unwrap();
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

#[test]
fn a_file_cut_in_its_media_data_ends_with_an_error_after_its_frames() {
    let dir = tmp("cut");
    let good = clip(&dir, "t-cut", 30);
    let fast = dir.join("t-cut-faststart.mp4");
    faststart(&good, &fast);
    let bytes = std::fs::read(&fast).unwrap();

    // Whole files decode to the end without complaint, whichever end moov
    // is at.
    for p in [&good, &fast] {
        let mut d = VideoDecoder::open(p, None, None).unwrap();
        let (n, e) = decode(&mut d);
        assert!(e.is_none(), "{}: {e:?}", p.display());
        assert_eq!(n, 30);
        assert_eq!(d.skipped_packets(), 0);
    }

    // Cut at 60%: moov survives, the last samples it lists don't.
    let cut = dir.join("t-cut-60.mp4");
    std::fs::write(&cut, &bytes[..bytes.len() * 6 / 10]).unwrap();
    let mut d = VideoDecoder::open(&cut, None, None).unwrap();
    let (n, e) = decode(&mut d);
    assert!(n > 0 && n < 30, "{n} frames");
    let e = e.expect("a truncated file must not end like a whole one");
    let reason = decode_reason(&e).to_string();
    assert!(reason.contains("truncated"), "{reason}");
    assert!(
        reason.contains(&format!("ends at byte {}", bytes.len() * 6 / 10)),
        "{reason}"
    );
    assert!(reason.contains(&format!("after {n} frames")), "{reason}");
    // The stream stays ended with the same error.
    let again = d.next_frame().unwrap_err();
    assert_eq!(decode_reason(&again), reason);

    // With moov at the end, the same cut loses the index: opening fails.
    let good_bytes = std::fs::read(&good).unwrap();
    let cut_tail = dir.join("t-cut-tail.mp4");
    std::fs::write(&cut_tail, &good_bytes[..good_bytes.len() * 6 / 10]).unwrap();
    assert!(matches!(
        VideoDecoder::open(&cut_tail, None, None),
        Err(MediaError::Open { .. })
    ));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_cancelled_read_is_an_error_not_the_end_of_the_stream() {
    let dir = tmp("cancel");
    // Several times FFmpeg's 32 KiB read buffer, so reads go on after the
    // first frame.
    let path = clip(&dir, "t-cancel", 90);
    assert!(std::fs::metadata(&path).unwrap().len() > 3 * 32 * 1024);

    let cancel = Arc::new(AtomicBool::new(false));
    let mut d = VideoDecoder::open_cancellable(&path, None, None, cancel.clone()).unwrap();
    assert!(d.next_frame().unwrap().is_some());
    cancel.store(true, Ordering::Relaxed);
    let (n, e) = decode(&mut d);
    assert!(n < 89, "decoded {n} more frames after the cancel");
    let e = e.expect("a cancelled read must not end like the file did");
    let reason = decode_reason(&e).to_string();
    assert!(reason.contains("cancelled"), "{reason}");
    assert_eq!(decode_reason(&d.next_frame().unwrap_err()), reason);

    // Uncancelled, the same file decodes to the end.
    let mut d = VideoDecoder::open_cancellable(&path, None, None, Arc::default()).unwrap();
    let (n, e) = decode(&mut d);
    assert!(e.is_none(), "{e:?}");
    assert_eq!(n, 90);
    std::fs::remove_dir_all(&dir).ok();
}
