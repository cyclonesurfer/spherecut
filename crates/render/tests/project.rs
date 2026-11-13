//! A project's renders over time (M3 plan 11.4): only an edit changes a
//! project's geometry, so an old project renders the same bytes when a
//! newer build's table has measured its camera.
//!
//! M3.9's types stage names the test; unit (d) writes it and the suite's
//! budget.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use glam::DQuat;
use spherecut_core::project::{DualFisheye, INITIAL_VIEW, Project, Source, file};
use spherecut_media::backend::{FfmpegBackend, MediaBackend, VideoRequest};
use spherecut_media::probe::probe;
use spherecut_media::project::{
    Superseded, camera_source, project_media, superseded, superseded_by_table,
};
use spherecut_render::ViewParams;
use spherecut_render::reference::{ImageF64, LensPair, RigOptions, render_rig_of};
use spherecut_sphere::fisheye::V6Reading;
use spherecut_sphere::rig::{
    Binding, CanvasMap, EulerConv, Extrinsics, Generation, Geometry, QuarterTurn, Rig, Status,
    Tune, TurnAxis, TzSign,
};
use spherecut_synth::insv::{InsvSpec, Records, write_insv};
use spherecut_synth::lens::{compose, x5_like, x5_like_planted};
use spherecut_synth::rung::LensFrame;
use spherecut_testkit::Fnv64;
use spherecut_testkit::control::must_fail;

/// Each lens's square in the camera file, in pixels.
const SIDE: u32 = 192;

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

/// A project of a camera file whose stored geometry is an older guess,
/// superseded by a fake newer table entry that differs where a render
/// shows it, loads unchanged and renders through its `rig()` the same
/// bytes as through `Rig::build` from the old numbers written out by hand.
/// Control: the render under the newer entry's geometry differs.
#[test]
fn an_old_project_renders_the_same_bytes() {
    let _time = timed("an_old_project_renders_the_same_bytes");
    let dir = TempDir::new("old");
    // The small camera file of the app's and agent's tests made smaller:
    // x5_like's V3 under x5_like_planted, one frame of 2 × SIDE².
    let mut spec = InsvSpec::small();
    spec.frames = 1;
    spec.rig = compose(&x5_like(), &x5_like_planted(), 0, [SIDE; 2]);
    spec.records = Records::x5_like(
        std::slice::from_ref(&x5_like()),
        &x5_like_planted(),
        spec.frames,
        spec.rate,
    );
    let began = Instant::now();
    let master = write_insv(&spec, &dir).unwrap().master;
    eprintln!("wrote cam.insv in {:.3} s", began.elapsed().as_secs_f64());

    // Its project as `project create` makes it, made when the table's
    // guess was an older one: the X5 row's conventions (block b on track
    // 1 − b), provisional, under evidence of their own.
    let report = probe(&master).unwrap();
    let request = VideoRequest {
        stream: report.camera_file.lenses.as_ref().map(|l| l.tracks[0]),
        ..VideoRequest::default()
    };
    let info = FfmpegBackend::new()
        .open_video(&master, &request)
        .unwrap()
        .info()
        .clone();
    let src = spherecut_media::Source::path(&master);
    let mut media = project_media(src, &master, &info).unwrap();
    let mut old = camera_source(src, &report).unwrap().unwrap();
    old.geometry.evidence = "an older guess".into();
    media.source = Source::DualFisheye(Box::new(old.clone()));
    let made = Project::untitled(media, INITIAL_VIEW).unwrap();
    let path = dir.join("old.spherecut");
    file::save(&made, &path).unwrap();

    // A newer build's table measured the camera: settled, and the other
    // binding. Loading the project changes nothing; it only says so.
    let newer = Geometry {
        binding: Binding::Direct,
        status: Status::Settled,
        evidence: "settled on clips 8 to 10".into(),
        ..old.geometry.clone()
    };
    let loaded = file::load(&path).unwrap();
    assert!(loaded.same_content(&made));
    let Source::DualFisheye(stored) = &loaded.media()[0].source else {
        panic!("{:?}", loaded.media()[0].source)
    };
    assert_eq!(**stored, old);
    assert_eq!(
        superseded(&stored.geometry, &newer),
        Some(Superseded {
            status: Status::Settled,
            evidence: newer.evidence.clone(),
        })
    );
    // This build's table, whose X5 evidence is another, supersedes it too.
    assert!(superseded_by_table(stored).is_some());

    // The old numbers, written out by hand, and x5_like's calibration
    // through the boundary: never the project's reader.
    let by_hand = Geometry {
        binding: Binding::Reversed,
        canvas: CanvasMap::C,
        extrinsics: Extrinsics::Stored(EulerConv::PLACEHOLDER),
        turn: TurnAxis::Y,
        tz: TzSign::AsStored,
        upright: QuarterTurn::Zero,
        generation: Generation::V3,
        v6: V6Reading::RADIAL_ONLY,
        status: Status::Provisional,
        evidence: "an older guess".into(),
    };
    let sides = [SIDE; 2];
    let calibration = spherecut_boundary::calibration(&x5_like());
    let reference = Rig::build(&calibration, &by_hand, &Tune::default(), 0, sides).unwrap();

    // One pair of lens pictures, the file's (it is lossless), stitched
    // into a 160 × 80 equirect, wide enough that no seam pixel falls
    // between the lenses.
    let frames = [0, 1].map(|t| LensFrame::new(spec.planes(t, 0).unwrap(), sides[t]));
    let pair = LensPair::new([&frames[0], &frames[1]]).unwrap();
    let view = ViewParams::equirect(DQuat::IDENTITY);
    let render = |rig: &Rig| {
        let image = render_rig_of(
            &pair,
            rig,
            [[1.0; 3]; 2],
            &view,
            (160, 80),
            &RigOptions::default(),
        );
        hash(&image.unwrap())
    };
    let project = render(&stored.rig(sides).unwrap());
    let written = render(&reference);
    eprintln!("the project's render {project:016x}, the old numbers' {written:016x}");
    assert_eq!(project, written);

    // Control: the newer table's geometry renders otherwise.
    let taken = DualFisheye {
        geometry: newer,
        ..(**stored).clone()
    };
    let other = render(&taken.rig(sides).unwrap());
    eprintln!("under the newer table's geometry {other:016x}");
    must_fail("the newer table's geometry", other == written);
}

/// An image's bytes, hashed: every value's, in order.
fn hash(image: &ImageF64) -> u64 {
    let mut h = Fnv64::new();
    h.write(&image.width.to_le_bytes())
        .write(&image.height.to_le_bytes());
    for v in &image.data {
        h.write(&v.to_le_bytes());
    }
    h.finish()
}

/// A test's own folder, removed with what it holds when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("render-project-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }
}

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
