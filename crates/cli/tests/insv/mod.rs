//! A synthetic `.insv` and its LRV for the CLI's tests (M3 plan 7.2):
//! two lens clips of synth's remuxed into one MP4 of two video tracks,
//! as the X5 writes its masters, with synth's X5-like trailer after it
//! (`insv::append_trailer`), and a one-track LRV with the same exposure
//! record. Pat's V3 and V6 calibrations are insta360.md's "Pat's X5
//! samples", as camera's `synth_insv.rs` has them.
//!
//! `Mp4Writer` writes one video track and M3.5's `write_insv` the N-track
//! file; until then, the remux here is ffmpeg-next's alone, apart from
//! Spherecut's confined open (D24), as contract.rs's `fragmented` is.

use std::path::{Path, PathBuf};

use spherecut_core::FrameRate;
use spherecut_media::ff;
use spherecut_synth::Pattern;
use spherecut_synth::insv::{self, InsvFault, Records, TrailerLayout};
use spherecut_synth::lens::{
    Angle, Axis, BlockTruth, CalibTruth, Planted, QuarterTurn, Sign, StoredDist, TruthBinding,
    TruthCanvas, TruthEuler, TruthTurn, TruthTz, TruthV6,
};
use spherecut_synth::media::{self, Audio, Codec, Spec};

/// The frames each file holds, and their rate.
pub const FRAMES: u32 = 6;
pub const RATE: FrameRate = FrameRate::FPS_25;
/// Each lens's square, in pixels.
pub const SIDE: u32 = 128;
/// The names the files get: a recording of 22 October 2026 at 10:15,
/// number 001, as the X5 names its files (insta360.md §2.3).
pub const MASTER: &str = "VID_20261022_101500_00_001.insv";
pub const LRV: &str = "LRV_20261022_101500_01_001.lrv";

/// A master written into a folder, and where synth put its trailer.
pub struct Written {
    pub path: PathBuf,
    pub layout: TrailerLayout,
}

/// The two calibrations Pat's X5 writes, V3 and V6.
pub fn pats_calibrations() -> [CalibTruth; 2] {
    [pats_v3(), pats_v6()]
}

/// An X5-like master in `dir` named `name`: two H.264 lens tracks, each
/// SIDE square, and the trailer of `Records::x5_like` for Pat's two
/// calibrations, with `fault` planted in it.
pub fn master(dir: &Path, name: &str, fault: Option<InsvFault>) -> Written {
    let lens = |n: usize, pattern: Pattern| {
        media::write(
            &Spec {
                name: format!("lens{n}"),
                pattern,
                width: SIDE,
                height: SIDE,
                rate: RATE,
                frames: FRAMES,
                codec: Codec::H264,
                bits: 8,
                audio: Audio::None,
                spherical: false,
                ..Spec::default()
            },
            &dir.join(format!("lens{n}")),
        )
        .unwrap()
    };
    let tracks = dir.join(format!("{name}.tracks.mp4"));
    remux(&[lens(0, Pattern::Grid), lens(1, Pattern::Seam)], &tracks);
    let records = Records::x5_like(&pats_calibrations(), &planted(), FRAMES, RATE);
    let path = dir.join(name);
    let layout = insv::append_trailer(&tracks, &path, &records, fault).unwrap();
    Written { path, layout }
}

/// An LRV in `dir` named `name`: one track, both lenses side by side, and
/// the trailer of `Records::x5_like` for `frames` frames, so that its
/// exposure record (4) is the master's when `frames` is [`FRAMES`].
pub fn lrv(dir: &Path, name: &str, frames: u32) -> PathBuf {
    let clip = media::write(
        &Spec {
            name: "lrv".into(),
            pattern: Pattern::Grid,
            width: 2 * SIDE,
            height: SIDE,
            rate: RATE,
            frames,
            codec: Codec::H264,
            bits: 8,
            audio: Audio::None,
            spherical: false,
            ..Spec::default()
        },
        &dir.join(format!("{name}.clip")),
    )
    .unwrap();
    let records = Records::x5_like(&pats_calibrations(), &planted(), frames, RATE);
    let path = dir.join(name);
    insv::append_trailer(&clip, &path, &records, None).unwrap();
    path
}

/// `srcs`' video streams, in order, as the tracks of one MP4 at `dst`.
// Reads files this test wrote and writes another with ffmpeg-next alone,
// apart from Spherecut's confined open (D24).
#[allow(clippy::disallowed_methods)]
fn remux(srcs: &[PathBuf], dst: &Path) {
    let mut inputs: Vec<_> = srcs.iter().map(|s| ff::format::input(s).unwrap()).collect();
    let mut octx = ff::format::output_as(dst, "mp4").unwrap();
    for ictx in &inputs {
        let st = ictx.streams().best(ff::media::Type::Video).unwrap();
        let mut out = octx
            .add_stream(ff::encoder::find(ff::codec::Id::None))
            .unwrap();
        out.set_parameters(st.parameters());
        out.set_time_base(st.time_base());
    }
    octx.write_header().unwrap();
    for (k, ictx) in inputs.iter_mut().enumerate() {
        let video = ictx.streams().best(ff::media::Type::Video).unwrap().index();
        for (st, mut pkt) in ictx.packets() {
            if st.index() != video {
                continue;
            }
            let out_tb = octx.stream(k).unwrap().time_base();
            pkt.rescale_ts(st.time_base(), out_tb);
            pkt.set_stream(k);
            pkt.set_position(-1);
            pkt.write_interleaved(&mut octx).unwrap();
        }
    }
    octx.write_trailer().unwrap();
}

/// One value of each convention synth plants; M3.2's `print` reads none.
pub fn planted() -> Planted {
    Planted {
        binding: TruthBinding::Direct,
        canvas: TruthCanvas::A,
        euler: TruthEuler {
            axes: [Axis::Z, Axis::Y, Axis::X],
            order: [Angle::Yaw, Angle::Pitch, Angle::Roll],
            signs: [Sign::Plus; 3],
            roll_offset: QuarterTurn::Minus90,
        },
        turn: TruthTurn::Y,
        upright: QuarterTurn::Zero,
        tz: TruthTz::AsStored,
        v6: TruthV6::RADIAL_ONLY,
    }
}

/// Pat's V3 calibration, from insta360.md's "Pat's X5 samples".
fn pats_v3() -> CalibTruth {
    let blocks = [
        BlockTruth {
            xi: 2.0,
            fx: 4284.62,
            fy: 4284.28,
            cx: 2701.13,
            cy: 2692.13,
            ypr_deg: [-0.198, 0.211, 90.28],
            t_m: [0.0; 3],
        },
        BlockTruth {
            xi: 2.0,
            fx: 4291.03,
            fy: 4291.03,
            cx: 8069.0,
            cy: 2684.95,
            ypr_deg: [0.239, 0.113, 89.107],
            t_m: [-0.00446, 0.000017, -0.03192],
        },
    ];
    CalibTruth {
        blocks,
        dist: StoredDist::V3 {
            k: [
                [0.18632051, 2.04984355, -3.24446917],
                [0.18940836, 2.07073069, -3.24103332],
            ],
            p: [[-0.00143576, -0.0033924], [0.00013831, -0.00299873]],
        },
        canvas: [10752, 5376],
        dst: 5312,
        lens_type: 113,
        body: 4,
    }
}

/// Pat's V6 calibration, from the same table.
fn pats_v6() -> CalibTruth {
    let blocks = [
        BlockTruth {
            xi: 2.0,
            fx: 4277.52,
            fy: 4277.27,
            cx: 2713.05,
            cy: 2689.21,
            ypr_deg: [-0.31, -0.271, 90.255],
            t_m: [0.0; 3],
        },
        BlockTruth {
            xi: 2.0,
            fx: 4284.4,
            fy: 4284.76,
            cx: 8075.73,
            cy: 2680.35,
            ypr_deg: [0.12, -0.173, 89.112],
            t_m: [0.000637, 0.000001, -0.031525],
        },
    ];
    let d = [
        [
            0.28032997,
            0.89045405,
            2.04927588,
            -8.05179787,
            0.0,
            0.001202,
            -0.00259487,
            0.02324706,
            0.0053392,
            -0.01560208,
            0.00019357,
            -0.04580523,
            0.00197187,
        ],
        [
            0.27665332,
            0.98522973,
            1.83605266,
            -7.99374485,
            0.0,
            0.00021056,
            -0.00147626,
            0.01940425,
            0.00331838,
            -0.01330665,
            0.00074511,
            -0.02859299,
            0.00888746,
        ],
    ];
    CalibTruth {
        blocks,
        dist: StoredDist::V6 { d },
        ..pats_v3()
    }
}
