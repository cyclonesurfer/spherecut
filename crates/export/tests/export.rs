//! An export's refusals, its cancel, its timing and its size: what
//! `Export::start` refuses before any work (M2 plan section 8.6), a
//! cancel that leaves nothing behind, a 29.97 fps export from mid-GOP
//! whose frames are the source's at their own times, and libx264 and
//! libx265 under their cap, within the size estimate (amendment 2 item
//! 24).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::{Duration, Instant};

use common::{
    Scratch, best, clip, frames, gpu, grid, held, measure, project, references, secs, worst,
};
use spherecut_core::project::{Aspect, Codec, EncoderChoice, Output, Quality, Range};
use spherecut_core::time::{FrameRate, Time};
use spherecut_export::{EncoderSetup, Export, ExportError, ExportJob, FramePlan, Preset};
use spherecut_render::YuvLayout;
use spherecut_synth::Pattern;
use spherecut_synth::media::{self as synth, Audio, Spec};

const FLAT_1080: Output = Output::Flat {
    aspect: Aspect::Wide,
    short_side: 1080,
};

fn first_second() -> Range {
    Range {
        start: Time::ZERO,
        end: Time::from_secs(1),
    }
}

#[test]
fn start_refuses_a_file_at_the_output_and_va_api() {
    let src = grid("export-grid-h264-8", synth::Codec::H264, 8);
    let scratch = Scratch::new("refusals");
    let p = project(&src, first_second(), FLAT_1080, Codec::H264, 0.0);

    // Something at the output, without overwrite: left as it was.
    let out = scratch.0.join("taken.mp4");
    std::fs::write(&out, b"mine").unwrap();
    let e = Export::start(ExportJob::new(p.clone(), &out, gpu().clone())).unwrap_err();
    assert!(
        matches!(&e, ExportError::OutputExists(path) if *path == out),
        "{e:?}"
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"mine");

    let free = scratch.0.join("free.mp4");
    // VA-API isn't in this build.
    let mut job = ExportJob::new(p.clone(), &free, gpu().clone());
    job.settings.encoder = EncoderChoice::Vaapi;
    let e = Export::start(job).unwrap_err();
    assert!(
        matches!(&e, ExportError::NoEncoder(why) if why.contains("VA-API")),
        "{e:?}"
    );

    assert!(!free.exists());
}

#[test]
fn a_cancelled_export_ends_soon_and_leaves_nothing() {
    let src = grid("export-grid-h264-8", synth::Codec::H264, 8);
    let scratch = Scratch::new("cancel");
    let trim = Range {
        start: Time::ZERO,
        end: Time::from_secs(2),
    };
    let p = project(&src, trim, FLAT_1080, Codec::H264, 0.0);
    let out = scratch.0.join("cancelled.mp4");
    let mut e = Export::start(ExportJob::new(p, &out, gpu().clone())).unwrap();
    while e.progress().frames_done < 2 {
        if let Some(r) = e.wait(Duration::from_millis(10)) {
            panic!("the export ended before the cancel: {r:?}");
        }
    }
    let asked = Instant::now();
    e.cancel();
    let r = e.wait(Duration::from_secs(30)).expect("the export ends");
    let took = asked.elapsed();
    assert!(matches!(r, Err(ExportError::Cancelled)), "{r:?}");
    eprintln!(
        "cancelled after {} frames in {took:?}",
        e.progress().frames_done
    );
    // Section 8.6 asks 2 s; each stage stops within its frame's work.
    assert!(took < Duration::from_secs(2), "the cancel took {took:?}");
    // No file, and no temporary one beside it.
    let left: Vec<_> = std::fs::read_dir(&scratch.0)
        .unwrap()
        .map(|d| d.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

#[test]
fn a_29_97_export_from_mid_gop_shows_the_source_at_each_frames_time() {
    // A second at 29.97 with one keyframe: the trim's start, 0.5 s, is
    // between frames 14 and 15 and far from it.
    let src = clip(&Spec {
        name: "export-grid-2997".into(),
        pattern: Pattern::Grid,
        width: 2048,
        height: 1024,
        rate: FrameRate::FPS_29_97,
        frames: 30,
        audio: Audio::None,
        spherical: true,
        ..Spec::default()
    });
    let scratch = Scratch::new("2997");
    let frame = |n: i64| secs(n * 1001, 30000);
    let start = secs(1, 2);
    let trim = Range {
        start,
        end: start.checked_add(frame(10)).unwrap(),
    };
    let p = project(&src, trim, FLAT_1080, Codec::H264, 0.0);
    assert_eq!(p.the_sequence().rate, FrameRate::FPS_29_97);

    // Over a file there, with overwrite.
    let out = scratch.0.join("2997.mp4");
    std::fs::write(&out, b"old").unwrap();
    let mut job = ExportJob::new(p.clone(), &out, gpu().clone());
    job.overwrite = true;
    let mut e = Export::start(job).unwrap();
    let report = e.wait(Duration::from_secs(1800)).expect("the export ends");
    let report = report.unwrap();
    assert_eq!((report.frames, report.rate), (10, FrameRate::FPS_29_97));
    assert_eq!(report.duration, frame(10));
    assert_eq!(report.bytes, std::fs::metadata(&out).unwrap().len());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    // Ten frames at n · 1001/30000 s.
    let got = frames(&out);
    let times: Vec<Time> = got.iter().map(|f| f.time).collect();
    let want: Vec<Time> = (0..10).map(frame).collect();
    assert_eq!(times, want);

    // Output frame n shows source frame 14 + n.
    let source = frames(&src);
    let plan = FramePlan::new(trim, FrameRate::FPS_29_97).unwrap();
    for n in 0..10u64 {
        let t = plan.source_time(n).unwrap();
        assert_eq!(
            held(&source, t).time,
            source[14 + n as usize].time,
            "frame {n}"
        );
    }
    // And looks it: against the reference of the frame it shows, and not
    // of the frame after.
    let all: Vec<u64> = (0..10).collect();
    let refs = references(&p, &source, YuvLayout::I420, &all);
    let decoded = || got.iter().map(|g| g.as_ref());
    let m = measure(decoded().zip(refs.iter().map(Vec::as_slice)));
    let (psnr, ssim) = worst(&m);
    let off = measure(decoded().zip(refs.iter().skip(1).map(Vec::as_slice)));
    let (off_psnr, _) = best(&off);
    eprintln!("29.97: worst {psnr:.2} dB, SSIM {ssim:.5}; a frame on, best {off_psnr:.2} dB");
    assert!(psnr >= 40.0 && ssim >= 0.98, "{psnr:.2} dB, SSIM {ssim:.5}");
    assert!(off_psnr < psnr, "a frame on reaches {off_psnr:.2} dB");
}

#[test]
fn libx264_and_libx265_run_under_the_cap_and_within_the_estimate() {
    let src = grid("export-grid-h264-8", synth::Codec::H264, 8);
    let scratch = Scratch::new("cap");
    let square = Output::Flat {
        aspect: Aspect::Square,
        short_side: 1080,
    };
    let preset = Preset::of(&square).unwrap();
    for (codec, keys) in [
        (Codec::H264, ("vbv_maxrate", "vbv_bufsize")),
        (Codec::Hevc, ("vbv-maxrate", "vbv-bufsize")),
    ] {
        let p = project(&src, first_second(), square, codec, 0.0);
        let out = scratch.0.join(format!("{codec:?}.mp4"));
        let r = common::export(&p, &out);
        let setup = EncoderSetup::software(codec, Quality::High, preset, r.rate, false);
        // The encoder writes how it was set into its stream; given a cap
        // without a buffer, it names neither.
        let said = format!(
            "{}={} {}={}",
            keys.0,
            setup.maxrate / 1000,
            keys.1,
            setup.bufsize / 1000
        );
        let file = std::fs::read(&out).unwrap();
        assert!(
            file.windows(said.len()).any(|w| w == said.as_bytes()),
            "{codec:?}: the stream doesn't say `{said}`"
        );
        let estimate = setup.estimate(0, r.duration);
        eprintln!("{codec:?}: {} bytes, the estimate {estimate}", r.bytes);
        assert!(
            r.bytes <= estimate,
            "{codec:?}: {} bytes, past the estimate of {estimate}",
            r.bytes
        );
    }
}
