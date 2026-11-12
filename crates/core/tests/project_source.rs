//! A media's source in the project (M3 plan 11.1 and 11.2): `validate`'s
//! rules for the tune, the gains, the caps and the snapshot, the feather
//! held short of each lens's frame, the coverage rule, and the Auto
//! gains read along their track.
//!
//! The rules are read on the schema 2 fixture's camera media
//! (`tests/data/schema2.spherecut`), alone in the untitled project it
//! opens, as `validate` sees it after an edit.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use glam::DVec3;
use spherecut_core::project::source::{Analysed, AnalysisInput, Gains, Sampling, ValidateTest};
use spherecut_core::project::{
    Cap, CapMode, Code, DualFisheye, EacLayout, EacPair, Fingerprint, INITIAL_VIEW, Media, Problem,
    Project, Source, file,
};
use spherecut_core::{Rational, Time};
use spherecut_sphere::rig::{CanvasMap, Generation, LensTune, Rig, theta_frame};
use spherecut_testkit::control::must_fail;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run. The tests run side by side, so none of
/// them can time the rest: the last line printed is the suite's time.
struct Timed {
    name: &'static str,
    began: Instant,
}

fn timed(name: &'static str) -> Timed {
    BEGAN.get_or_init(Instant::now);
    Timed {
        name,
        began: Instant::now(),
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        let suite = BEGAN.get_or_init(Instant::now).elapsed().as_secs_f64();
        eprintln!(
            "{}: {:.3} s; the suite {suite:.3} s so far",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

/// The schema 2 fixture's camera media, read once from the file.
fn camera() -> Media {
    static CAMERA: OnceLock<Media> = OnceLock::new();
    CAMERA
        .get_or_init(|| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/schema2.spherecut");
            let project = file::load(&path).unwrap();
            project
                .media()
                .iter()
                .find(|m| matches!(m.source, Source::DualFisheye(_)))
                .unwrap()
                .clone()
        })
        .clone()
}

/// The camera media's dual-fisheye source.
fn fisheye(media: &Media) -> &DualFisheye {
    match &media.source {
        Source::DualFisheye(df) => df,
        other => panic!("{other:?}"),
    }
}

/// The fixture's camera media with its source as `change` leaves it.
fn changed(change: impl FnOnce(&mut DualFisheye)) -> Media {
    let mut media = camera();
    match &mut media.source {
        Source::DualFisheye(df) => change(df),
        other => panic!("{other:?}"),
    }
    media
}

/// `validate` on the untitled project `media` opens, where it is media 1.
fn validate(media: Media) -> Result<(), Problem> {
    Project::untitled(media, INITIAL_VIEW).map(drop)
}

/// [`validate`] with `test`'s rules left out. The project is the
/// fixture's camera's, with `media` put in its place as a file reads it,
/// so that a media `validate` refuses can be checked.
fn validate_leaving_out(media: &Media, test: ValidateTest) -> Result<(), Problem> {
    let base = Project::untitled(camera(), INITIAL_VIEW).unwrap();
    let mut value = serde_json::to_value(&base).unwrap();
    let id = value["media"][0]["id"].clone();
    value["media"][0] = serde_json::to_value(media).unwrap();
    value["media"][0]["id"] = id;
    serde_json::from_value::<Project>(value)
        .unwrap()
        .validate_leaving_out(test)
}

/// The words of each rule's refusal that tell it from the others'.
const RANGE: &str = "is outside";
const FEATHER: &str = "the feather's band";
const LIMIT: &str = "past its lens's limit";
const SUM: &str = "ξ + xi_delta must stay above −1";
const COVERAGE: &str = "covered by neither lens";
const NO_RIG: &str = "makes no rig";

/// What a row expects of `validate`.
#[derive(Debug)]
enum Expect {
    Valid,
    /// Refused with this code, the message naming media 1, the field and
    /// the rule's words.
    Refused {
        code: Code,
        field: String,
        rule: &'static str,
    },
}

fn refused(code: Code, field: impl Into<String>, rule: &'static str) -> Expect {
    Expect::Refused {
        code,
        field: field.into(),
        rule,
    }
}

/// One row: a name, the media and what `validate` does with it.
struct Row {
    name: String,
    media: Media,
    expect: Expect,
}

/// The bounds' table: each number `validate` reads (11.2, and the rules
/// this slice adds: `front_track`, `xi_delta`'s range and the coverage
/// rule) at each end and a step past it.
fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    let mut row = |name: String, media: Media, expect: Expect| {
        rows.push(Row {
            name,
            media,
            expect,
        })
    };
    let range = |field: String| refused(Code::OutOfRange, field, RANGE);
    row("the fixture".into(), camera(), Expect::Valid);

    // Structure.
    row(
        "both tracks one stream".into(),
        changed(|df| df.tracks = [1, 1]),
        refused(Code::Invalid, "source.tracks", "twice"),
    );
    row(
        "front track 1".into(),
        changed(|df| df.front_track = 1),
        Expect::Valid,
    );
    row(
        "front track 2".into(),
        changed(|df| df.front_track = 2),
        refused(Code::OutOfRange, "source.front_track", "neither 0 nor 1"),
    );
    row(
        "the geometry reads V3, which the snapshot holds".into(),
        changed(|df| df.geometry.generation = Generation::V3),
        Expect::Valid,
    );
    row(
        "the geometry reads V6 with no V3 set".into(),
        changed(|df| df.calibration.v3 = None),
        Expect::Valid,
    );
    row(
        "the geometry reads V6 with no V6 set".into(),
        changed(|df| df.calibration.v6 = None),
        refused(Code::Invalid, "source.calibration.v6", "is null"),
    );
    row(
        "the geometry reads V3 with no V3 set".into(),
        changed(|df| {
            df.geometry.generation = Generation::V3;
            df.calibration.v3 = None;
        }),
        refused(Code::Invalid, "source.calibration.v3", "is null"),
    );
    row(
        "a hash that isn't the strings'".into(),
        changed(|df| df.calibration.hash = "0123456789abcdef".into()),
        refused(
            Code::Invalid,
            "source.calibration.hash",
            "raw strings' hash",
        ),
    );
    row(
        "a string the hash isn't of".into(),
        changed(|df| df.calibration.raw.v1 = Some("1_2_3".into())),
        refused(
            Code::Invalid,
            "source.calibration.hash",
            "raw strings' hash",
        ),
    );
    row(
        "a focal of NaN in the set the geometry reads".into(),
        changed(|df| df.calibration.v6.as_mut().unwrap()[0].fx = f64::NAN),
        refused(Code::Invalid, "source", NO_RIG),
    );
    row(
        "a focal of 0".into(),
        changed(|df| df.calibration.v6.as_mut().unwrap()[1].fy = 0.0),
        refused(Code::Invalid, "source", NO_RIG),
    );
    let eac = |tracks: [u32; 2]| {
        let mut media = camera();
        media.source = Source::EacPair(Box::new(EacPair {
            camera: fisheye(&camera()).camera.clone(),
            tracks,
            layout: EacLayout {
                overlap: 64,
                pre: 1152,
                post: 1024,
                size: [4096, 1344],
            },
        }));
        media
    };
    row("an EAC pair".into(), eac([0, 1]), Expect::Valid);
    row(
        "an EAC pair of one stream".into(),
        eac([0, 0]),
        refused(Code::Invalid, "source.tracks", "twice"),
    );

    // The tune, each lens.
    for b in 0..2 {
        let lens = |f: &'static str| format!("source.tune.lenses[{b}].{f}");
        for i in 0..3 {
            for (end, past) in [(15.0, 15f64.next_up()), (-15.0, (-15f64).next_down())] {
                row(
                    format!("lens {b}'s ypr {i} at {end}°"),
                    changed(|df| df.tune.lenses[b].ypr_deg[i] = end),
                    Expect::Valid,
                );
                row(
                    format!("lens {b}'s ypr {i} at {past}°"),
                    changed(|df| df.tune.lenses[b].ypr_deg[i] = past),
                    range(format!("{}[{i}]", lens("ypr_deg"))),
                );
            }
        }
        for i in 0..2 {
            for (end, past) in [(64.0, 64f64.next_up()), (-64.0, (-64f64).next_down())] {
                row(
                    format!("lens {b}'s centre {i} at {end} px"),
                    changed(|df| df.tune.lenses[b].centre_px[i] = end),
                    Expect::Valid,
                );
                row(
                    format!("lens {b}'s centre {i} at {past} px"),
                    changed(|df| df.tune.lenses[b].centre_px[i] = past),
                    range(format!("{}[{i}]", lens("centre_px"))),
                );
            }
        }
        // The radius scale: at 1.1 the frame cuts each lens near 90.5°
        // under map C, so only a feather under about 0.5° is held there.
        row(
            format!("lens {b}'s radius scale at 0.9"),
            changed(|df| df.tune.lenses[b].radius_scale = 0.9),
            Expect::Valid,
        );
        row(
            format!("lens {b}'s radius scale at 1.1 with a feather of 0"),
            changed(|df| {
                df.tune.lenses[b].radius_scale = 1.1;
                df.tune.feather_deg = 0.0;
            }),
            Expect::Valid,
        );
        row(
            format!("lens {b}'s radius scale at 1.1 with the fixture's feather of 4°"),
            changed(|df| df.tune.lenses[b].radius_scale = 1.1),
            refused(
                Code::OutOfRange,
                "source.tune.feather_deg",
                ["lens 0's θ", "lens 1's θ"][b],
            ),
        );
        row(
            format!("lens {b}'s radius scale just under 0.9"),
            changed(|df| df.tune.lenses[b].radius_scale = 0.9f64.next_down()),
            range(lens("radius_scale")),
        );
        row(
            format!("lens {b}'s radius scale just over 1.1"),
            changed(|df| df.tune.lenses[b].radius_scale = 1.1f64.next_up()),
            range(lens("radius_scale")),
        );
        // θ_max: at 85° the floor leaves only a feather of 0.
        row(
            format!("lens {b}'s θ_max at 110°"),
            changed(|df| df.tune.lenses[b].theta_max_deg = 110.0),
            Expect::Valid,
        );
        row(
            format!("lens {b}'s θ_max at 85° with a feather of 0"),
            changed(|df| {
                df.tune.lenses[b].theta_max_deg = 85.0;
                df.tune.feather_deg = 0.0;
            }),
            Expect::Valid,
        );
        row(
            format!("lens {b}'s θ_max at 85° with the fixture's feather of 4°"),
            changed(|df| df.tune.lenses[b].theta_max_deg = 85.0),
            refused(
                Code::OutOfRange,
                "source.tune.feather_deg",
                ["lens 0's θ", "lens 1's θ"][b],
            ),
        );
        row(
            format!("lens {b}'s θ_max just under 85°"),
            changed(|df| {
                df.tune.lenses[b].theta_max_deg = 85f64.next_down();
                df.tune.feather_deg = 0.0;
            }),
            range(lens("theta_max_deg")),
        );
        row(
            format!("lens {b}'s θ_max just over 110°"),
            changed(|df| df.tune.lenses[b].theta_max_deg = 110f64.next_up()),
            range(lens("theta_max_deg")),
        );
        // ξ's delta: at −0.15 the frame cuts each lens near 90.3°.
        row(
            format!("lens {b}'s xi_delta at 0.15"),
            changed(|df| df.tune.lenses[b].xi_delta = 0.15),
            Expect::Valid,
        );
        row(
            format!("lens {b}'s xi_delta at −0.15 with a feather of 0"),
            changed(|df| {
                df.tune.lenses[b].xi_delta = -0.15;
                df.tune.feather_deg = 0.0;
            }),
            Expect::Valid,
        );
        row(
            format!("lens {b}'s xi_delta at −0.15 with the fixture's feather of 4°"),
            changed(|df| df.tune.lenses[b].xi_delta = -0.15),
            refused(
                Code::OutOfRange,
                "source.tune.feather_deg",
                ["lens 0's θ", "lens 1's θ"][b],
            ),
        );
        row(
            format!("lens {b}'s xi_delta just over 0.15"),
            changed(|df| df.tune.lenses[b].xi_delta = 0.15f64.next_up()),
            range(lens("xi_delta")),
        );
        row(
            format!("lens {b}'s xi_delta just under −0.15"),
            changed(|df| {
                df.tune.lenses[b].xi_delta = (-0.15f64).next_down();
                df.tune.feather_deg = 0.0;
            }),
            range(lens("xi_delta")),
        );
        // NaN is past every bound.
        row(
            format!("lens {b}'s pitch NaN"),
            changed(|df| df.tune.lenses[b].ypr_deg[1] = f64::NAN),
            range(format!("{}[1]", lens("ypr_deg"))),
        );
        row(
            format!("lens {b}'s centre infinite"),
            changed(|df| df.tune.lenses[b].centre_px[0] = f64::INFINITY),
            range(format!("{}[0]", lens("centre_px"))),
        );
        row(
            format!("lens {b}'s radius scale NaN"),
            changed(|df| df.tune.lenses[b].radius_scale = f64::NAN),
            range(lens("radius_scale")),
        );
        row(
            format!("lens {b}'s θ_max NaN"),
            changed(|df| df.tune.lenses[b].theta_max_deg = f64::NAN),
            range(lens("theta_max_deg")),
        );
        row(
            format!("lens {b}'s xi_delta NaN"),
            changed(|df| df.tune.lenses[b].xi_delta = f64::NAN),
            range(lens("xi_delta")),
        );
    }

    // The feather and the stitch distance. A feather of 15° wants both
    // lenses' min(θ_max, θ_frame) at 105° or more: the fixture's θ_max of
    // 100° and 98° hold it to 8°, a radius scale of 0.95 puts the frames
    // near 109° and 108°.
    let wide = |feather: f64| {
        changed(|df| {
            for lens in &mut df.tune.lenses {
                lens.radius_scale = 0.95;
                lens.theta_max_deg = 106.0;
            }
            df.tune.feather_deg = feather;
        })
    };
    row(
        "a feather of 0".into(),
        changed(|df| df.tune.feather_deg = 0.0),
        Expect::Valid,
    );
    row(
        "a feather just under 0".into(),
        changed(|df| df.tune.feather_deg = (-0.0f64).next_down()),
        range("source.tune.feather_deg".into()),
    );
    row(
        "a feather of 15° on the fixture's tune".into(),
        changed(|df| df.tune.feather_deg = 15.0),
        refused(Code::OutOfRange, "source.tune.feather_deg", "lens 0's θ"),
    );
    row(
        "a feather of 15° on a wide tune".into(),
        wide(15.0),
        Expect::Valid,
    );
    row(
        "a feather just over 15° on a wide tune".into(),
        wide(15f64.next_up()),
        range("source.tune.feather_deg".into()),
    );
    row(
        "a feather of NaN".into(),
        changed(|df| df.tune.feather_deg = f64::NAN),
        range("source.tune.feather_deg".into()),
    );
    row(
        "q at 0".into(),
        changed(|df| df.tune.q_per_m = 0.0),
        Expect::Valid,
    );
    row(
        "q at 3.34".into(),
        changed(|df| df.tune.q_per_m = 3.34),
        Expect::Valid,
    );
    row(
        "q just under 0".into(),
        changed(|df| df.tune.q_per_m = (-0.0f64).next_down()),
        range("source.tune.q_per_m".into()),
    );
    row(
        "q just over 3.34".into(),
        changed(|df| df.tune.q_per_m = 3.34f64.next_up()),
        range("source.tune.q_per_m".into()),
    );
    row(
        "q infinite".into(),
        changed(|df| df.tune.q_per_m = f64::INFINITY),
        range("source.tune.q_per_m".into()),
    );

    // Under the floor: min(θ_max, θ_frame) under 90° holds only a feather
    // of 0, which makes no hole of its own.
    let short = |feather: f64| {
        changed(|df| {
            df.tune.lenses[0].theta_max_deg = 85.0;
            df.tune.lenses[1] = LensTune::default();
            df.tune.feather_deg = feather;
        })
    };
    row(
        "lens 0 at θ_max 85°, a feather of 0".into(),
        short(0.0),
        Expect::Valid,
    );
    row(
        "lens 0 at θ_max 85°, a feather a step past 0".into(),
        short(0f64.next_up()),
        refused(Code::OutOfRange, "source.tune.feather_deg", "lens 0's θ"),
    );
    // Under map A a radius scale of 1.1 puts lens 0's θ_frame at 89.6°.
    let framed = |feather: f64| {
        changed(|df| {
            df.geometry.canvas = CanvasMap::A;
            df.tune.lenses[0].radius_scale = 1.1;
            df.tune.feather_deg = feather;
        })
    };
    row(
        "lens 0's θ_frame under 90°, a feather of 0".into(),
        framed(0.0),
        Expect::Valid,
    );
    row(
        "lens 0's θ_frame under 90°, a feather a step past 0".into(),
        framed(0f64.next_up()),
        refused(Code::OutOfRange, "source.tune.feather_deg", "lens 0's θ"),
    );
    // Both lenses at 85° leave a ring about the seam that Rig::weights
    // gives (0, 0), black, whatever the feather: the coverage rule.
    row(
        "both lenses at θ_max 85°, a feather of 0".into(),
        both_at_85(),
        refused(Code::OutOfRange, "source.tune", COVERAGE),
    );

    // The gains.
    let fixed = |b: usize, c: usize, gain: f64| {
        changed(|df| {
            let mut rgb = [[1.0; 3]; 2];
            rgb[b][c] = gain;
            df.gains = Gains::Fixed { rgb };
        })
    };
    for b in 0..2 {
        for c in 0..3 {
            let field = format!("source.gains.rgb[{b}][{c}]");
            row(
                format!("gain {b} {c} at 0.25"),
                fixed(b, c, 0.25),
                Expect::Valid,
            );
            row(
                format!("gain {b} {c} at 4"),
                fixed(b, c, 4.0),
                Expect::Valid,
            );
            row(
                format!("gain {b} {c} just under 0.25"),
                fixed(b, c, 0.25f64.next_down()),
                range(field.clone()),
            );
            row(
                format!("gain {b} {c} just over 4"),
                fixed(b, c, 4f64.next_up()),
                range(field),
            );
        }
    }
    row(
        "a gain of NaN".into(),
        fixed(1, 2, f64::NAN),
        range("source.gains.rgb[1][2]".into()),
    );
    row(
        "no gains".into(),
        changed(|df| df.gains = Gains::Off {}),
        Expect::Valid,
    );
    let auto =
        |rate_hz: f64, log_ratio: Vec<[f32; 3]>| changed(|df| df.gains = auto(rate_hz, log_ratio));
    let ramp = || (0..8).map(|k| [0.02 * k as f32; 3]).collect::<Vec<_>>();
    row("an Auto track".into(), auto(4.0, ramp()), Expect::Valid);
    row(
        "an Auto track at the least rate above 0".into(),
        auto(0f64.next_up(), ramp()),
        Expect::Valid,
    );
    for rate in [0.0, -4.0, f64::INFINITY, f64::NAN] {
        row(
            format!("an Auto track at {rate} a second"),
            auto(rate, ramp()),
            refused(Code::OutOfRange, "source.gains.rate_hz", "above 0"),
        );
    }
    row(
        "an Auto track of no samples".into(),
        auto(4.0, Vec::new()),
        refused(Code::Invalid, "source.gains.log_ratio", "no samples"),
    );
    row(
        "an Auto track's ratio at f32::MAX".into(),
        auto(4.0, vec![[0.0, f32::MAX, 0.0]]),
        Expect::Valid,
    );
    for ratio in [f32::INFINITY, f32::NAN] {
        let mut track = ramp();
        track[5][1] = ratio;
        row(
            format!("an Auto track with a ratio of {ratio}"),
            auto(4.0, track),
            refused(
                Code::OutOfRange,
                "source.gains.log_ratio[5][1]",
                "not finite",
            ),
        );
    }

    // The caps: the fixture fills the nadir.
    let cap = |change: fn(&mut Cap)| {
        let mut media = camera();
        change(media.caps.nadir.as_mut().unwrap());
        media
    };
    row(
        "a cap of 5°".into(),
        cap(|c| c.cap_deg = 5.0),
        Expect::Valid,
    );
    row(
        "a cap of 35°".into(),
        cap(|c| c.cap_deg = 35.0),
        Expect::Valid,
    );
    row(
        "a cap just under 5°".into(),
        cap(|c| c.cap_deg = 5f64.next_down()),
        range("caps.nadir.cap_deg".into()),
    );
    row(
        "a cap just over 35°".into(),
        cap(|c| c.cap_deg = 35f64.next_up()),
        range("caps.nadir.cap_deg".into()),
    );
    row(
        "an edge of 0".into(),
        cap(|c| c.edge_deg = 0.0),
        Expect::Valid,
    );
    row(
        "an edge of 5°".into(),
        cap(|c| c.edge_deg = 5.0),
        Expect::Valid,
    );
    row(
        "an edge just under 0".into(),
        cap(|c| c.edge_deg = (-0.0f64).next_down()),
        range("caps.nadir.edge_deg".into()),
    );
    row(
        "an edge just over 5°".into(),
        cap(|c| c.edge_deg = 5f64.next_up()),
        range("caps.nadir.edge_deg".into()),
    );
    row(
        "a fill's ring of NaN".into(),
        cap(|c| c.mode = CapMode::Fill { ring_deg: f64::NAN }),
        refused(Code::Invalid, "caps.nadir.mode.ring_deg", "not finite"),
    );
    row(
        "a rotation of infinity".into(),
        cap(|c| c.rotation_deg = f64::INFINITY),
        refused(Code::Invalid, "caps.nadir.rotation_deg", "not finite"),
    );
    let zenith = |cap_deg: f64| {
        let mut media = camera();
        let mut cap = media.caps.nadir.clone().unwrap();
        cap.cap_deg = cap_deg;
        media.caps.zenith = Some(cap);
        media
    };
    row("a zenith cap of 35°".into(), zenith(35.0), Expect::Valid);
    row(
        "a zenith cap just over 35°".into(),
        zenith(35f64.next_up()),
        range("caps.zenith.cap_deg".into()),
    );
    row(
        "an equirect media's cap just over 35°".into(),
        {
            let mut media = cap(|c| c.cap_deg = 35f64.next_up());
            media.source = Source::Equirect {};
            media
        },
        range("caps.nadir.cap_deg".into()),
    );
    rows
}

/// The fixture's camera with both lenses at θ_max 85° and a feather of 0.
fn both_at_85() -> Media {
    changed(|df| {
        for lens in &mut df.tune.lenses {
            lens.theta_max_deg = 85.0;
        }
        df.tune.feather_deg = 0.0;
    })
}

/// Every number `validate` bounds (11.2), at each end (valid) and a step
/// past it (refused, naming the media and the field), and the structural
/// rules (distinct tracks, front track 0 or 1, the geometry's generation
/// in the snapshot, the hash of the raw strings); the row count pinned so
/// no rule drops unseen. Where another rule refuses an end (a feather of
/// 15° on the fixture's tune, a radius scale of 1.1, θ_max at 85° or
/// `xi_delta` at −0.15 beside a feather of 4°), the row names that rule,
/// and another row shows the end valid with the feather it allows.
#[test]
fn validate_at_each_bound_and_just_past() {
    let _time = timed("validate_at_each_bound_and_just_past");
    let rows = rows();
    let began = Instant::now();
    let mut wrong = Vec::new();
    for Row {
        name,
        media,
        expect,
    } in &rows
    {
        let got = validate(media.clone());
        let right = match (expect, &got) {
            (Expect::Valid, Ok(())) => true,
            (Expect::Refused { code, field, rule }, Err(p)) => {
                p.code == *code
                    && p.message.contains("media 1's ")
                    && p.message.contains(field.as_str())
                    && p.message.contains(rule)
            }
            _ => false,
        };
        if !right {
            wrong.push(format!("{name}: expected {expect:?}, got {got:?}"));
        }
    }
    let each = began.elapsed().as_secs_f64() / rows.len() as f64;
    eprintln!("{} rows, validate {:.3} ms a call", rows.len(), each * 1e3);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert_eq!(rows.len(), 159, "a row was added or dropped");

    // What both lenses at 85° would have rendered: directions 90° off
    // lens 0's axis, which neither covers, weighed (0, 0).
    let media = both_at_85();
    let rig = fisheye(&media).rig([3840; 2]).unwrap();
    let side = axis(&rig, 0).any_orthonormal_vector();
    assert_eq!(rig.weights(side, [0.0; 2]), [0.0; 2]);
}

/// Lens `b`'s axis in the rig frame.
fn axis(rig: &Rig, b: usize) -> DVec3 {
    rig.lenses[b].lens_from_rig.inverse() * DVec3::Z
}

/// The largest feather that validates is max(0, min(θ_max, θ_frame) −
/// 90°), each lens in turn, θ_frame from `rig::theta_frame` (11.2).
/// Control: a rule from θ_max alone passes a feather the frame cuts.
#[test]
fn the_feather_stops_short_of_the_frame() {
    let _time = timed("the_feather_stops_short_of_the_frame");
    for b in 0..2 {
        // Both lenses at θ_max 110°, past where their frames cut them, and
        // the other lens's frame moved out by a radius scale of 0.95: lens
        // b's frame is the bound.
        let tuned = |feather: f64| {
            changed(|df| {
                for lens in &mut df.tune.lenses {
                    lens.theta_max_deg = 110.0;
                }
                df.tune.lenses[1 - b].radius_scale = 0.95;
                df.tune.feather_deg = feather;
            })
        };
        let media = tuned(0.0);
        let df = fisheye(&media);
        let frame = theta_frame(&df.calibration.calibration(), &df.geometry, &df.tune)
            .unwrap()
            .map(f64::to_degrees);
        let bound = |l: usize| (df.tune.lenses[l].theta_max_deg.min(frame[l]) - 90.0).max(0.0);
        assert!(frame[b] < 110.0 && bound(b) < bound(1 - b), "{frame:?}");
        let expected = bound(b).min(15.0);
        // The largest feather that validates, by bisection.
        let (mut lo, mut hi) = (0.0, 15.0);
        assert!(validate(tuned(lo)).is_ok());
        let p = validate(tuned(hi)).unwrap_err();
        assert!(
            p.message.contains(FEATHER) && p.message.contains(&format!("lens {b}'s θ")),
            "{p:?}"
        );
        while hi - lo > 1e-10 {
            let mid = 0.5 * (lo + hi);
            if validate(tuned(mid)).is_ok() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        eprintln!(
            "lens {b}: θ_frame {:.4}°, the feather up to {lo:.9}°",
            frame[b]
        );
        assert!(
            (lo - expected).abs() <= 1e-9,
            "lens {b}: {lo} against {expected}"
        );
        // Control: θ_max alone would allow 15°.
        let from_theta_max = (df.tune.lenses[b].theta_max_deg - 90.0).clamp(0.0, 15.0);
        must_fail(
            "a feather bound from θ_max alone",
            (lo - from_theta_max).abs() <= 1e-9,
        );
    }
}

/// `Gains::at`: ones for Off, `rgb` for Fixed, and Auto's track exact at
/// its samples, linear between them, held past its ends, with g0 · g1 = 1
/// (plan 16). Control: a nearest-sample read fails the midpoint.
#[test]
fn gains_at_follows_its_track() {
    let _time = timed("gains_at_follows_its_track");
    // Exact for every time here, each a multiple of 1/16 s.
    let at = |s: f64| Time::seconds(Rational::new((s * 1024.0).round() as i64, 1024).unwrap());
    assert_eq!(Gains::Off {}.at(at(3.0)), [[1.0; 3]; 2]);
    let rgb = [[1.1, 1.0, 0.9], [0.8, 1.0, 1.25]];
    assert_eq!(Gains::Fixed { rgb }.at(at(3.0)), rgb);

    // A ramp of 0 to 0.2 over 8 samples at 4 a second, red rising, green
    // at half of it and blue falling.
    let rate = 4.0;
    let track: Vec<[f32; 3]> = (0..8)
        .map(|k| {
            let v = 0.2 * k as f32 / 7.0;
            [v, v / 2.0, -v]
        })
        .collect();
    let auto = auto(rate, track.clone());
    // What the split gives for a log ratio `l` per channel.
    let split = |l: [f64; 3]| {
        let mut g = [[0.0; 3]; 2];
        for c in 0..3 {
            g[0][c] = (l[c] / 2.0).exp();
            g[1][c] = (-l[c] / 2.0).exp();
        }
        g
    };
    let sample = |k: usize| track[k].map(f64::from);
    // Exact at each sample.
    for k in 0..8 {
        assert_eq!(auto.at(at(k as f64 / rate)), split(sample(k)), "sample {k}");
    }
    // Linear between: a quarter and half of the way.
    let near = |a: [[f64; 3]; 2], b: [[f64; 3]; 2]| {
        a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(x, y)| (x - y).abs() <= 1e-12)
    };
    let between = |k: usize, f: f64| {
        let (a, b) = (sample(k), sample(k + 1));
        split([0, 1, 2].map(|c| a[c] + (b[c] - a[c]) * f))
    };
    for k in 0..7 {
        for f in [0.25, 0.5] {
            let g = auto.at(at((k as f64 + f) / rate));
            assert!(near(g, between(k, f)), "sample {k} + {f}: {g:?}");
        }
    }
    // Held past its ends, a time before 0 too.
    assert_eq!(auto.at(at(-1.0)), split(sample(0)));
    assert_eq!(auto.at(at(100.0)), split(sample(7)));
    // Both blocks meet at the geometric mean: g0 · g1 = 1.
    for s in [-1.0, 0.0, 0.375, 1.0, 1.125, 1.75, 9.0] {
        let g = auto.at(at(s));
        for c in 0..3 {
            assert!((g[0][c] * g[1][c] - 1.0).abs() <= 1e-15, "{s} s: {g:?}");
        }
    }
    // A track with no samples (validate refuses it) gives ones.
    assert_eq!(auto_empty().at(at(1.0)), [[1.0; 3]; 2]);

    // Control: a nearest-sample read is off at the midpoint.
    let nearest = |t: f64| split(sample(((t * rate).round() as usize).min(7)));
    must_fail(
        "a nearest-sample read of the track",
        near(nearest(3.5 / rate), between(3, 0.5)),
    );
}

/// An Auto track of `log_ratio` at `rate_hz`, its provenance a stand-in.
fn auto(rate_hz: f64, log_ratio: Vec<[f32; 3]>) -> Gains {
    Gains::Auto {
        rate_hz,
        log_ratio,
        basis: "0000000000000000".into(),
        input: AnalysisInput {
            file: Analysed::Master,
            fingerprint: Fingerprint {
                bytes: 1,
                head: "0000000000000000".into(),
            },
            frames: Sampling::Every { n: 1 },
        },
        estimator: 1,
    }
}

fn auto_empty() -> Gains {
    auto(4.0, Vec::new())
}

/// F038: `validate` refuses ξ + `xi_delta` ≤ −1 for each lens, the rule
/// `Rig::build` applies, and holds `xi_delta` to ±0.15, the range this
/// slice proposes beside the tune's others (11.2). The fixture's ξ is
/// 2, where the range refuses long before the sum: the sum binds on a
/// snapshot of a ξ within 0.15 of −1.
#[test]
fn xi_delta_keeps_xi_above_minus_one() {
    let _time = timed("xi_delta_keeps_xi_above_minus_one");
    let sum = |p: &Problem| p.code == Code::OutOfRange && p.message.contains(SUM);
    let field = "media 1's source.tune.lenses[0].xi_delta";
    // A snapshot of my own: block 0's ξ in the set the geometry reads, V6,
    // at −0.9.
    let near = |delta: f64| {
        changed(|df| {
            df.calibration.v6.as_mut().unwrap()[0].xi = -0.9;
            df.tune.lenses[0].xi_delta = delta;
        })
    };
    // The range's low end takes ξ to −1.05: the sum rule refuses it.
    let low = validate(near(-0.15)).unwrap_err();
    assert!(sum(&low) && low.message.contains(field), "{low:?}");
    // A delta of 0 passes the sum, and the limit refuses the snapshot: at
    // ξ = −0.9 the model images nothing past 25.8°, far inside θ_max.
    let zero = validate(near(0.0)).unwrap_err();
    assert!(!zero.message.contains(SUM), "{zero:?}");
    assert!(
        zero.code == Code::OutOfRange
            && zero.message.contains(LIMIT)
            && zero
                .message
                .contains("media 1's source.tune.lenses[0].theta_max_deg"),
        "{zero:?}"
    );
    // So does a delta that keeps ξ just above −1, at −0.9999.
    let above = validate(near(-0.0999)).unwrap_err();
    assert!(
        !above.message.contains(SUM) && above.message.contains(LIMIT),
        "{above:?}"
    );

    // Handed-on's rows at ξ = 0: −1.5 and −0.5 are both past the range.
    for delta in [-1.5, -0.5] {
        let media = changed(|df| {
            df.calibration.v6.as_mut().unwrap()[0].xi = 0.0;
            df.tune.lenses[0].xi_delta = delta;
        });
        let p = validate(media).unwrap_err();
        assert!(
            p.code == Code::OutOfRange && p.message.contains(field) && p.message.contains(RANGE),
            "{delta}: {p:?}"
        );
    }

    // The range's ends and a step past each, on the fixture, each lens.
    for b in 0..2 {
        let at = |delta: f64| {
            changed(|df| {
                df.tune.lenses[b].xi_delta = delta;
                df.tune.feather_deg = 0.0;
            })
        };
        validate(at(0.15)).unwrap();
        validate(at(-0.15)).unwrap();
        for past in [0.15f64.next_up(), (-0.15f64).next_down()] {
            let p = validate(at(past)).unwrap_err();
            assert!(
                p.code == Code::OutOfRange && p.message.contains(RANGE),
                "{past}: {p:?}"
            );
        }
    }

    // Control: with the sum rule left out the low end's refusal is the
    // rig's, which doesn't name the sum.
    let left_out = validate_leaving_out(
        &near(-0.15),
        ValidateTest {
            no_xi_sum: true,
            ..ValidateTest::default()
        },
    )
    .unwrap_err();
    assert!(left_out.message.contains(NO_RIG), "{left_out:?}");
    must_fail("the sum rule left out", sum(&left_out));
}

/// The directions of the brief's grid about the seam that `rig` weighs
/// (0, 0) at LOD 0: each degree of azimuth about lens 0's axis, 70° to
/// 110° off it by 0.5°.
fn holes(rig: &Rig) -> usize {
    let a = axis(rig, 0);
    let u = a.any_orthonormal_vector();
    let v = a.cross(u);
    let mut count = 0;
    for off in 0..=80 {
        let (s, c) = (70.0 + 0.5 * f64::from(off)).to_radians().sin_cos();
        for azimuth in 0..360 {
            let (sa, ca) = f64::from(azimuth).to_radians().sin_cos();
            let d = a * c + (u * ca + v * sa) * s;
            if rig.weights(d, [0.0; 2]) == [0.0; 2] {
                count += 1;
            }
        }
    }
    count
}

/// A tune under which some direction is covered by neither lens at the
/// project's q is refused (the coverage rule): `Rig::weights` about the
/// seam is never (0, 0) at tunes the rule passes, and is somewhere at
/// tunes it refuses. Control: the rule with the angle between the lenses'
/// axes left out passes a yawed tune whose hole the grid finds.
#[test]
fn the_coverage_rule_finds_the_holes() {
    let _time = timed("the_coverage_rule_finds_the_holes");
    let yawed = || {
        changed(|df| {
            for lens in &mut df.tune.lenses {
                lens.theta_max_deg = 95.0;
            }
            df.tune.lenses[1].ypr_deg[0] = 15.0;
            df.tune.q_per_m = 3.34;
        })
    };
    let tunes: [(&str, Media, bool); 5] = [
        ("the fixture's tune", camera(), true),
        (
            "lens 0 at θ_max 85°",
            changed(|df| {
                df.tune.lenses[0].theta_max_deg = 85.0;
                df.tune.feather_deg = 0.0;
            }),
            true,
        ),
        (
            "lens 1 yawed 5° at q 3.34",
            changed(|df| {
                df.tune.lenses[1].ypr_deg[0] = 5.0;
                df.tune.q_per_m = 3.34;
            }),
            true,
        ),
        ("both lenses at θ_max 85°", both_at_85(), false),
        (
            "both at θ_max 95°, lens 1 yawed 15° at q 3.34",
            yawed(),
            false,
        ),
    ];
    for (name, media, passes) in tunes {
        let got = validate(media.clone());
        let rig = fisheye(&media).rig([media.video.width; 2]).unwrap();
        let holes = holes(&rig);
        eprintln!("{name}: {got:?}, {holes} of 29160 directions weighed (0, 0)");
        if passes {
            assert_eq!(got, Ok(()), "{name}");
            assert_eq!(holes, 0, "{name}");
        } else {
            let p = got.unwrap_err();
            assert!(
                p.code == Code::OutOfRange && p.message.contains(COVERAGE),
                "{name}: {p:?}"
            );
            assert!(holes > 0, "{name}: refused, and covers");
        }
    }

    // Control: the axes taken as opposite, the yawed tune's caps sum past
    // 180° and pass, while the grid finds its hole.
    let media = yawed();
    let passes = validate_leaving_out(
        &media,
        ValidateTest {
            axes_opposite: true,
            ..ValidateTest::default()
        },
    )
    .is_ok();
    let rig = fisheye(&media).rig([media.video.width; 2]).unwrap();
    must_fail(
        "the coverage rule with the axes taken as opposite",
        !passes || holes(&rig) == 0,
    );
}
