//! The stored calibration as sphere's types and the built-in geometry
//! table (M3 plan 5.2 and 5.3; M3.5's unit (b)): `insta360::to_calibration`,
//! `insta360::known` and `insta360::stitching`, held to Pat's truths in
//! `spherecut-boundary` through synth's printer and trailer writer, which
//! share no code with camera's parser and reader (4.1 rule 2). These are
//! integration tests for the reason `synth_insv.rs` gives.
//!
//! What a calibration should be comes from synth's truth, written field
//! by field by the boundary crate, so this file holds no copy of the
//! conversion. Tier 2c's test reads Pat's 14 files:
//!
//! ```text
//! SPHERECUT_SAMPLES=/home/claude/spherecut-samples/insta360-x5 \
//!   cargo test -p spherecut-camera --test geometry -- --ignored
//! ```
//!
//! Budget (M3 plan 19.6): camera's suite ≤ 1 s, which each test prints
//! its time against; nothing here codes media.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::io::Cursor;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use spherecut_boundary as boundary;
use spherecut_camera::insta360::calib::{self, Calib, Focal, Generation, Token};
use spherecut_camera::insta360::known::{self, Known};
use spherecut_camera::insta360::limits::MAX_METADATA;
use spherecut_camera::insta360::{
    Metadata, Stitching, Window, parse_metadata, read_record, read_trailer, stitching,
    to_calibration,
};
use spherecut_camera::{CameraError, Part};
use spherecut_core::FrameRate;
use spherecut_sphere::fisheye::V6Reading;
use spherecut_sphere::rig::{
    self, Binding, CalibTerms, Calibration, CanvasMap, EulerConv, Extrinsics, Geometry,
    QuarterTurn, Rig, RigError, Status, Tune, TurnAxis, TzSign,
};
use spherecut_synth::insv::{self, Field, InsvFault, Records};
use spherecut_synth::lens::{self as truth, CalibTruth};
use spherecut_testkit::control::must_fail;
use spherecut_testkit::samples;
use spherecut_testkit::tier2;

/// The suite's budget in seconds: all of camera's (19.6).
const BUDGET_S: f64 = 1.0;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run, against the budget. The tests run side by
/// side, so the last line printed is the suite's time.
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
            "{}: {:.3} s; the suite {suite:.3} s so far, of camera's {BUDGET_S} s budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

/// The X5's model and firmware as fields 2 and 3 hold them, which
/// synth's files give too.
const X5: &str = "Insta360 X5";
const X5_FIRMWARE: &str = "v1.1.22_build1";

/// The X5's track side in Tier 1's rig (4.2).
const SIDE: u32 = truth::X5_LIKE_SIDE;

/// camera's generation for synth's.
fn generation(t: &CalibTruth) -> Generation {
    match t.generation() {
        truth::Generation::V3 => Generation::V3,
        truth::Generation::V6 => Generation::V6,
    }
}

/// camera's parse of synth's print of `t`.
fn parsed(t: &CalibTruth) -> Calib {
    let s = insv::print(t, &truth::x5_like_planted());
    calib::parse(generation(t), &s).unwrap()
}

/// Field 27 as synth writes it for `t`: the canvas's square for src,
/// `t`'s dst, at 0, 0.
fn window(t: &CalibTruth) -> Window {
    Window {
        src: [t.canvas[1]; 2],
        dst: [t.dst; 2],
        offset: [0; 2],
    }
}

/// `to_calibration` of `t` alone, under its generation.
fn alone(t: &CalibTruth) -> Result<Calibration, CameraError> {
    let c = parsed(t);
    match t.generation() {
        truth::Generation::V3 => to_calibration(Some(&c), None, Some(&window(t))),
        truth::Generation::V6 => to_calibration(None, Some(&c), Some(&window(t))),
    }
}

/// The kinds of number a block stores, each printed to its own
/// precision ("Pat's X5 samples").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// ξ, to 6 decimals.
    Xi,
    /// The focal lengths and the centre, to 3.
    Pixels,
    /// The angles, to 3.
    Angle,
    /// The translation, to 6.
    T,
    /// The distortion terms, to 8.
    Term,
}

impl Kind {
    /// Half a unit in the last decimal the X5 prints it to.
    fn half_unit(self) -> f64 {
        match self {
            Kind::Xi | Kind::T => 0.5e-6,
            Kind::Pixels | Kind::Angle => 0.5e-3,
            Kind::Term => 0.5e-8,
        }
    }
}

/// Every number of `c`'s blocks, named, by generation, block and field.
fn numbers(c: &Calibration) -> Vec<(String, Kind, f64)> {
    let mut out = Vec::new();
    for (g, blocks) in [("V3", &c.v3), ("V6", &c.v6)] {
        for (b, l) in blocks.iter().flatten().enumerate() {
            let mut push = |field: &str, kind: Kind, v: f64| {
                out.push((format!("{g} block {b}'s {field}"), kind, v));
            };
            push("ξ", Kind::Xi, l.xi);
            push("fx", Kind::Pixels, l.fx);
            push("fy", Kind::Pixels, l.fy);
            push("cx", Kind::Pixels, l.cx);
            push("cy", Kind::Pixels, l.cy);
            for (name, v) in ["yaw", "pitch", "roll"].into_iter().zip(l.ypr_deg) {
                push(name, Kind::Angle, v);
            }
            for (name, v) in ["tx", "ty", "tz"].into_iter().zip(l.t_m) {
                push(name, Kind::T, v);
            }
            match l.terms {
                CalibTerms::V3 { k, p } => {
                    let names = ["k1", "k2", "k3", "p1", "p2"];
                    for (name, v) in names.into_iter().zip(k.iter().chain(&p)) {
                        push(name, Kind::Term, *v);
                    }
                }
                CalibTerms::V6 { d } => {
                    for (i, v) in d.into_iter().enumerate() {
                        push(&format!("d{i}"), Kind::Term, v);
                    }
                }
            }
        }
    }
    out
}

/// Whether `got` is `want`: src and dst and the generations it holds
/// the same, and each number within `tol` of its kind and `want`'s
/// value; or the first that isn't, named.
fn close(
    got: &Calibration,
    want: &Calibration,
    tol: impl Fn(Kind, f64) -> f64,
) -> Result<(), String> {
    if got.src != want.src {
        return Err(format!("src is {}, not {}", got.src, want.src));
    }
    if got.dst != want.dst {
        return Err(format!("dst is {}, not {}", got.dst, want.dst));
    }
    let held = |c: &Calibration| (c.v3.is_some(), c.v6.is_some());
    if held(got) != held(want) {
        return Err(format!(
            "V3 and V6 held {:?}, not {:?}",
            held(got),
            held(want)
        ));
    }
    let (a, b) = (numbers(got), numbers(want));
    if a.len() != b.len() {
        return Err(format!("{} numbers, not {}", a.len(), b.len()));
    }
    for ((name, kind, x), (want_name, _, y)) in a.into_iter().zip(b) {
        if name != want_name {
            return Err(format!("{name} where {want_name} should be"));
        }
        // NaN, which no tolerance holds, isn't close either.
        let within = (x - y).abs() <= tol(kind, y);
        if !within {
            return Err(format!("{name} is {x}, not {y}"));
        }
    }
    Ok(())
}

/// 1e-12 of a number's size, or of 1 for a smaller one: the cross
/// implementation's tolerance (5.5).
fn to_1e12(_: Kind, v: f64) -> f64 {
    1e-12 * v.abs().max(1.0)
}

/// The X5's row as 5.3 and plan 3.2's Meanwhile column give it, turn
/// axis Y as synth's `x5_like_planted` takes it.
fn x5_row() -> Known {
    Known {
        model: "X5".to_owned(),
        measured_on: &["v1.1.22_build1"],
        geometry: Geometry {
            binding: Binding::Reversed,
            canvas: CanvasMap::C,
            extrinsics: Extrinsics::Stored(EulerConv::PLACEHOLDER),
            turn: TurnAxis::Y,
            tz: TzSign::AsStored,
            upright: QuarterTurn::Zero,
            generation: rig::Generation::V6,
            v6: V6Reading::RADIAL_ONLY,
            status: Status::Provisional,
            evidence: "placeholder until the fit".to_owned(),
        },
        lrv_half_track: [1, 0],
        screen_side_track: None,
        stitchable: true,
        notice: None,
    }
}

/// `to_calibration` of camera's parse of synth's print of x5_like,
/// x5_like_v6 and Pat's V3 and V6 truths, alone and together, with
/// field 27's window, is `spherecut_boundary::calibration` of the truths
/// to 1e-12: src the canvas's height, dst the window's, block 1's cx
/// local to its square, and every other number as stored (5.2, and 5.5's
/// cross-implementation rule). With no window, dst is src.
///
/// Controls: the calibration with block 1's cx left on the canvas, and
/// with dst taken as src, aren't the truth's.
#[test]
fn to_calibration_gives_the_truths_numbers() {
    let _time = timed("to_calibration_gives_the_truths_numbers");
    let truths = [
        ("x5_like", truth::x5_like()),
        ("x5_like_v6", truth::x5_like_v6()),
        ("Pat's V3", boundary::pats(truth::Generation::V3)),
        ("Pat's V6", boundary::pats(truth::Generation::V6)),
    ];
    for (name, t) in &truths {
        let got = alone(t).unwrap_or_else(|e| panic!("{name}: {e}"));
        close(&got, &boundary::calibration(t), to_1e12).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // Both generations, as an X5 file holds them.
    for (v3, v6) in [(&truths[0], &truths[1]), (&truths[2], &truths[3])] {
        let got = to_calibration(
            Some(&parsed(&v3.1)),
            Some(&parsed(&v6.1)),
            Some(&window(&v3.1)),
        )
        .unwrap();
        let want = Calibration {
            v6: boundary::calibration(&v6.1).v6,
            ..boundary::calibration(&v3.1)
        };
        close(&got, &want, to_1e12).unwrap_or_else(|e| panic!("{} and {}: {e}", v3.0, v6.0));
    }
    // No window: dst is src.
    let pats_v6 = &truths[3].1;
    let got = to_calibration(None, Some(&parsed(pats_v6)), None).unwrap();
    let want = Calibration {
        dst: boundary::SRC,
        ..boundary::calibration(pats_v6)
    };
    close(&got, &want, to_1e12).unwrap();

    // The controls.
    let got = alone(pats_v6).unwrap();
    let want = boundary::calibration(pats_v6);
    let mut on_canvas = got;
    if let Some(blocks) = on_canvas.v6.as_mut() {
        blocks[1].cx += f64::from(got.src);
    }
    let e = must_fail(
        "block 1's cx left on the canvas",
        close(&on_canvas, &want, to_1e12),
    );
    assert!(e.starts_with("V6 block 1's cx is 8075.73"), "{e}");
    let as_src = Calibration {
        dst: got.src,
        ..got
    };
    let e = must_fail("dst taken as src", close(&as_src, &want, to_1e12));
    assert_eq!(e, "dst is 5376, not 5312");
}

/// Strings and windows `to_calibration` can't hold, each with what it
/// is, the V3 and V6 strings, the window and the part the refusal names.
type Case = (
    &'static str,
    Option<Calib>,
    Option<Calib>,
    Option<Window>,
    Part,
);

/// Strings that parse but can't be a calibration are
/// `CameraError::NotCalibration`, naming the field, never a panic or a
/// guess (5.2): no V3 or V6 string at all (record 1), a block with a
/// radius for its focal, a term count that isn't the generation's, a
/// canvas that isn't two squares side by side, a V6 canvas other than
/// V3's, a string in the other generation's place, and a field 27 window
/// that isn't square, is empty (a dst of 0, which a rig can't build on),
/// or whose src isn't the canvas's square.
///
/// Control: the good strings, alone and together, are a calibration.
#[test]
fn to_calibration_refuses_what_it_cant_hold() {
    let _time = timed("to_calibration_refuses_what_it_cant_hold");
    let (v3, v6) = (truth::x5_like(), truth::x5_like_v6());
    let (good3, good6, w) = (parsed(&v3), parsed(&v6), window(&v3));
    let refused = |r: Result<Calibration, CameraError>| match r {
        Err(e @ CameraError::NotCalibration { .. }) => Ok(e),
        other => Err(format!("{other:?}")),
    };
    let part = |e: &CameraError| match e {
        CameraError::NotCalibration { part, .. } => Some(*part),
        _ => None,
    };
    let radius = |mut c: Calib, b: usize| {
        c.lenses[b].focal = Focal::Radius(Token::new("2670.937").unwrap());
        c
    };
    let terms = |mut c: Calib, n: usize| {
        let t = Token::new("0.00000000").unwrap();
        c.lenses[0].coefficients.resize(n, t);
        c
    };
    let canvas = |mut c: Calib, w: u32, h: u32| {
        c.canvas = (w, h);
        c
    };
    let skewed = Window {
        dst: [5312, 5300],
        ..w
    };
    let empty = Window { dst: [0; 2], ..w };
    let other_src = Window {
        src: [5760; 2],
        ..w
    };
    let (v3s, v6s, f54, f111) = (
        Some(good3.clone()),
        Some(good6.clone()),
        Part::Field(54),
        Part::Field(111),
    );
    let cases: Vec<Case> = vec![
        ("no string", None, None, Some(w), Part::Record(1)),
        ("no string nor window", None, None, None, Part::Record(1)),
        (
            "V3 block 0 with a radius",
            Some(radius(good3.clone(), 0)),
            None,
            Some(w),
            f54,
        ),
        (
            "V6 block 1 with a radius",
            None,
            Some(radius(good6.clone(), 1)),
            Some(w),
            f111,
        ),
        (
            "V3 with 6 terms",
            Some(terms(good3.clone(), 6)),
            None,
            Some(w),
            f54,
        ),
        (
            "V3 with 4 terms",
            Some(terms(good3.clone(), 4)),
            None,
            Some(w),
            f54,
        ),
        (
            "V6 with 12 terms",
            None,
            Some(terms(good6.clone(), 12)),
            Some(w),
            f111,
        ),
        (
            "V6 with 14 terms",
            None,
            Some(terms(good6.clone(), 14)),
            Some(w),
            f111,
        ),
        (
            "V3 on 10751 × 5376",
            Some(canvas(good3.clone(), 10751, 5376)),
            None,
            Some(w),
            f54,
        ),
        (
            "V6 on a square canvas",
            None,
            Some(canvas(good6.clone(), 5376, 5376)),
            Some(w),
            f111,
        ),
        (
            "V6 on a canvas of no height",
            None,
            Some(canvas(good6.clone(), 0, 0)),
            None,
            f111,
        ),
        (
            "V6 on V3's canvas but wider",
            v3s.clone(),
            Some(canvas(good6.clone(), 11520, 5760)),
            Some(w),
            f111,
        ),
        ("a V6 string as V3's", v6s.clone(), None, Some(w), f54),
        ("a V3 string as V6's", None, v3s.clone(), Some(w), f111),
        (
            "a window that isn't square",
            v3s.clone(),
            None,
            Some(skewed),
            Part::Field(27),
        ),
        (
            "an empty window",
            None,
            v6s.clone(),
            Some(empty),
            Part::Field(27),
        ),
        (
            "a window of another square",
            None,
            v6s.clone(),
            Some(other_src),
            Part::Field(27),
        ),
    ];
    for (what, a, b, win, want) in &cases {
        let e = refused(to_calibration(a.as_ref(), b.as_ref(), win.as_ref()))
            .unwrap_or_else(|got| panic!("{what}: {got}"));
        assert_eq!(part(&e), Some(*want), "{what}: {e}");
        eprintln!("{what}: {e}");
    }
    // One message in full.
    let e = refused(to_calibration(
        None,
        Some(&radius(good6.clone(), 1)),
        Some(&w),
    ))
    .unwrap();
    assert_eq!(
        e.to_string(),
        "the metadata's field 111 can't be a calibration: block 1 gives a radius, not Mei's ξ and focal lengths"
    );
    // The control: the good strings, alone and together.
    for (what, a, b) in [
        ("the good V3 string", Some(&good3), None),
        ("the good V6 string", None, Some(&good6)),
        ("the good strings", Some(&good3), Some(&good6)),
    ] {
        must_fail(
            &format!("{what}, refused"),
            refused(to_calibration(a, b, Some(&w))),
        );
    }
}

/// `known::lookup("Insta360 X5", "v1.1.22_build1")` is 5.3's row in
/// every field, plan 3.2's Meanwhile: block 0 on track 1 (reversed),
/// canvas map C, the placeholder `EulerConv` with "placeholder until the
/// fit" for its evidence, block 1's turn about Y, tz as stored, upright,
/// V6 read radial only, provisional; the LRV's left half track 1, no
/// screen side yet, stitchable and no notice. `Rig::build` takes it with
/// the boundary crate's calibration of x5_like_v6 at 768², and refuses
/// x5_like's V3 calibration under it: the row reads V6.
///
/// Control: the row with block 0 on track 0 (direct) isn't the table's.
#[test]
fn known_x5_is_the_placeholder() {
    let _time = timed("known_x5_is_the_placeholder");
    let k = known::lookup(X5, X5_FIRMWARE);
    let want = x5_row();
    assert_eq!(k, want);
    let tune = Tune::default();
    let v6 = boundary::calibration(&truth::x5_like_v6());
    Rig::build(&v6, &k.geometry, &tune, 0, [SIDE; 2]).unwrap();
    let v3 = boundary::calibration(&truth::x5_like());
    assert_eq!(
        Rig::build(&v3, &k.geometry, &tune, 0, [SIDE; 2]).unwrap_err(),
        RigError::NoGeneration(rig::Generation::V6)
    );
    // The control.
    let direct = Known {
        geometry: Geometry {
            binding: Binding::Direct,
            ..want.geometry.clone()
        },
        ..want
    };
    must_fail("the X5 row with binding Direct", k == direct);
}

/// The evidence an entry for a firmware it wasn't measured on gives.
const UNMEASURED: &str =
    "placeholder until the fit; measured on firmware v1.1.22_build1, not this file's";

/// A firmware the X5's `measured_on` doesn't list ("v9.9.9") gets its
/// row as provisional, with evidence that says so; a model with no row
/// ("Insta360 X9") gets the X5's conventions, provisional, with the
/// notice "geometry assumed from the X5" (5.3). Everything else is the
/// X5's row.
///
/// Control: the listed firmware's evidence isn't the unmeasured one's.
#[test]
fn known_unlisted_firmware_and_model_are_provisional() {
    let _time = timed("known_unlisted_firmware_and_model_are_provisional");
    let row = x5_row();
    let with = |status: Status, evidence: &str, notice: Option<&'static str>| Known {
        geometry: Geometry {
            status,
            evidence: evidence.to_owned(),
            ..row.geometry.clone()
        },
        notice,
        ..row.clone()
    };
    let firmware = known::lookup(X5, "v9.9.9");
    assert_eq!(firmware, with(Status::Provisional, UNMEASURED, None));
    let model = known::lookup("Insta360 X9", X5_FIRMWARE);
    let assumed = "placeholder until the fit; the X5's row, as no row is for this file's model";
    assert_eq!(
        model,
        with(
            Status::Provisional,
            assumed,
            Some("geometry assumed from the X5")
        )
    );
    // A model with no row, on firmware of its own, and with none.
    assert_eq!(known::lookup("Insta360 X9", "v9.9.9"), model);
    assert_eq!(known::lookup("", ""), model);
    // The control.
    let listed = known::lookup(X5, X5_FIRMWARE);
    must_fail(
        "the listed firmware's evidence, as an unmeasured one's",
        listed.geometry.evidence == UNMEASURED,
    );
}

/// The metadata of a trailer synth lays out for `records`, with `fault`
/// planted, read back by camera's reader and parser.
fn metadata_of(records: &Records, fault: Option<InsvFault>) -> Metadata {
    let (bytes, _) = insv::inst_box(records, 0, 1, fault);
    let len = bytes.len() as u64;
    let mut r = Cursor::new(bytes);
    let t = read_trailer(&mut r, len).unwrap().unwrap();
    let payload = read_record(&mut r, &t, 1, MAX_METADATA).unwrap().unwrap();
    parse_metadata(&payload).unwrap()
}

/// `records` with field 136 set to `v`, or left out.
fn with_136(records: &Records, v: Option<u64>) -> Records {
    let mut r = records.clone();
    r.metadata.retain(|f| f.number != 136);
    r.metadata.extend(v.map(|v| Field::varint(136, v)));
    r.metadata.sort_by_key(|f| f.number);
    r
}

/// Whether `meta` gives a stitching whose geometry reads `generation`
/// and is otherwise the X5's row, whose calibration is `want` to 1e-12,
/// and which `Rig::build` builds at 768²; or why not.
fn follows(
    meta: &Metadata,
    generation: rig::Generation,
    want: &Calibration,
) -> Result<Stitching, String> {
    let s = stitching(meta)
        .map_err(|e| e.to_string())?
        .ok_or("no stitching")?;
    let mut row = x5_row();
    row.geometry.generation = generation;
    if s.known != row {
        return Err(format!("{:?}", s.known));
    }
    close(&s.calibration, want, to_1e12)?;
    Rig::build(
        &s.calibration,
        &s.known.geometry,
        &Tune::default(),
        0,
        [SIDE; 2],
    )
    .map_err(|e| e.to_string())?;
    Ok(s)
}

/// `stitching` reads the generation field 136 names into the X5's row
/// (3 for V3, as synth's x5_like trailer writes it, and 4 for V6, as
/// x5_like_v6's and both strings' do), with the calibration of the
/// strings and field 27, which `Rig::build` builds; gives `None` for
/// `InsvFault::NoCalibration`'s metadata, which has neither string; and
/// refuses a field 136 that is absent, names V1 or V2, or names nothing,
/// with `NotCalibration` in field 136.
///
/// Control: field 136 = 4 over V3 strings alone is an error, naming
/// field 136, not a stitching.
#[test]
fn stitching_follows_field_136() {
    let _time = timed("stitching_follows_field_136");
    let planted = truth::x5_like_planted();
    let (v3, v6) = (truth::x5_like(), truth::x5_like_v6());
    let records = |cals: &[CalibTruth]| Records::x5_like(cals, &planted, 6, FrameRate::FPS_29_97);
    let both = Calibration {
        v6: boundary::calibration(&v6).v6,
        ..boundary::calibration(&v3)
    };
    for (name, cals, generation, want) in [
        (
            "V3",
            vec![v3],
            rig::Generation::V3,
            boundary::calibration(&v3),
        ),
        (
            "V6",
            vec![v6],
            rig::Generation::V6,
            boundary::calibration(&v6),
        ),
        ("V3 and V6", vec![v3, v6], rig::Generation::V6, both),
    ] {
        let meta = metadata_of(&records(&cals), None);
        follows(&meta, generation, &want).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // No string: no stitching, field 136 and 27 notwithstanding.
    let none = metadata_of(&records(&[v3, v6]), Some(InsvFault::NoCalibration));
    assert_eq!(none.offset_version, Some(4));
    assert!(none.window.is_some());
    assert!(stitching(&none).unwrap().is_none());
    // A field 136 that names no generation a rig reads: refused.
    let in_136 = |r: Result<Option<Stitching>, CameraError>| match r {
        Err(CameraError::NotCalibration {
            part: Part::Field(136),
            why,
        }) => Ok(why),
        other => Err(format!("{other:?}")),
    };
    let held = records(&[v3, v6]);
    for (v, what) in [
        (None, "absent"),
        (Some(1), "V1"),
        (Some(2), "V2"),
        (Some(5), "5"),
        (Some(0), "0"),
    ] {
        let why = in_136(stitching(&metadata_of(&with_136(&held, v), None)))
            .unwrap_or_else(|e| panic!("field 136 {what}: {e}"));
        eprintln!("field 136 {what}: {why}");
    }
    // The control: 4 over V3 strings alone.
    let v3_alone = metadata_of(&with_136(&records(&[v3]), Some(4)), None);
    let e = must_fail(
        "field 136 = 4 over V3 strings alone",
        follows(&v3_alone, rig::Generation::V6, &boundary::calibration(&v3)),
    );
    assert_eq!(
        e,
        "the metadata's field 136 can't be a calibration: it names V6, whose string the file doesn't hold"
    );
}

/// `path`'s metadata, read through its trailer.
fn metadata_at(path: &Path) -> Metadata {
    let name = path.file_name().unwrap().to_string_lossy();
    let mut f = File::open(path).unwrap();
    let len = f.metadata().unwrap().len();
    let t = read_trailer(&mut f, len)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .unwrap_or_else(|| panic!("{name} has no trailer"));
    let payload = read_record(&mut f, &t, 1, MAX_METADATA)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .unwrap_or_else(|| panic!("{name} has no metadata"));
    parse_metadata(&payload).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Half a unit in the last decimal insta360.md prints each kind to.
fn printed(kind: Kind, _: f64) -> f64 {
    kind.half_unit()
}

/// The side of each half of an X5 LRV's 1664 × 832 frame.
const LRV_SIDE: u32 = 832;

/// Tier 2c: on each of Pat's 14 files `stitching` reads, under the X5's
/// row with field 136's V6; each block's centre lies in its own square;
/// `Rig::build` builds the rig of V3 and of V6 at the track's side (the
/// mode's on a master, 832 on an LRV's halves) under the default tune;
/// and the calibration's numbers are insta360.md's, as
/// `spherecut_boundary::calibration` of Pat's truths gives them, at the
/// precision the format notes print them to, with dst field 27's 5312.
///
/// Control: the truth with one number, V6 block 0's fx, off by its last
/// printed digit isn't the file's.
#[test]
#[ignore = "tier2: Pat's X5 samples (Tier 2c: SPHERECUT_SAMPLES and the CPU only)"]
fn samples_calibrations_build_rigs() {
    let started = Instant::now();
    tier2::require_samples_cpu();
    let pats = Calibration {
        v6: boundary::calibration(&boundary::pats(truth::Generation::V6)).v6,
        ..boundary::calibration(&boundary::pats(truth::Generation::V3))
    };
    let mut row = x5_row();
    row.geometry.generation = rig::Generation::V6;
    let tune = Tune::default();
    let mut files = 0;
    let mut last = None;
    for role in samples::roles().iter().filter(|r| r.file.is_some()) {
        for (path, side) in [
            (samples::role(role.name), role.mode.side()),
            (samples::lrv(role.name), LRV_SIDE),
        ] {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let meta = metadata_at(&path);
            let s = stitching(&meta)
                .unwrap_or_else(|e| panic!("{name}: {e}"))
                .unwrap_or_else(|| panic!("{name} has no calibration"));
            assert_eq!(s.known, row, "{name}");
            let cal = s.calibration;
            assert_eq!((cal.src, cal.dst), (5376, 5312), "{name}");
            for (g, blocks) in [("V3", cal.v3), ("V6", cal.v6)] {
                let blocks = blocks.unwrap_or_else(|| panic!("{name} has no {g} blocks"));
                for (b, l) in blocks.iter().enumerate() {
                    let square = 0.0..f64::from(cal.src);
                    assert!(
                        square.contains(&l.cx) && square.contains(&l.cy),
                        "{name}: {g} block {b}'s centre ({}, {}) is outside its square",
                        l.cx,
                        l.cy
                    );
                }
            }
            for generation in [rig::Generation::V3, rig::Generation::V6] {
                let geometry = Geometry {
                    generation,
                    ..s.known.geometry.clone()
                };
                Rig::build(&cal, &geometry, &tune, 0, [side; 2])
                    .unwrap_or_else(|e| panic!("{name}: {generation:?} at {side}²: {e}"));
            }
            close(&cal, &pats, printed).unwrap_or_else(|e| panic!("{name}: {e}"));
            last = Some(cal);
            files += 1;
        }
    }
    assert_eq!(files, 14);
    // The control.
    let cal = last.unwrap();
    let mut off = pats;
    if let Some(blocks) = off.v6.as_mut() {
        blocks[0].fx += 0.001;
    }
    let e = must_fail(
        "Pat's V6 block 0's fx a unit off in its last digit, against a file's",
        close(&cal, &off, printed),
    );
    assert!(e.starts_with("V6 block 0's fx is 4277.52"), "{e}");
    eprintln!(
        "samples_calibrations_build_rigs: {files} files in {:.3} s",
        started.elapsed().as_secs_f64()
    );
}
