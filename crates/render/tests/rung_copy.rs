//! Rung 1's fixtures where rung 3 reaches them (M3 plan amendment 5;
//! M3.5's unit (c)): the camera, far tune and pairs that
//! `tests/instrument.rs` keeps private are copied in `synth::rung`, which
//! rung 3 reaches from the agent crate. The copy is tied here to the
//! helpers it copies, built again from synth and the boundary as
//! `instrument.rs` builds them, so a change to one that isn't made to
//! the other fails this test.
//!
//! The views aren't tied: `instrument.rs`'s are private to it, and
//! `synth::rung`'s are sphere's types, not render's. M3.7's
//! `tests/common/rung1.rs` takes them when it lands.
//!
//! Budget (4.1 rule 5): 1 s. Each test prints its time and the suite's.

mod common;

use std::sync::OnceLock;
use std::time::Instant;

use spherecut_core::video::{ChromaSiting, Matrix, PlaneData, Range, VideoPlanes};
use spherecut_sphere::rig::{Rig, Tune};
use spherecut_synth::Pattern;
use spherecut_synth::lens::{Coding, RenderCache, compose, lens_planes, x5_like, x5_like_planted};
use spherecut_synth::rung;
use spherecut_testkit::control::must_fail;

use common::color;
use common::rig::{lens_frame, sphere_rig};

/// The suite's budget in seconds (4.1 rule 5).
const BUDGET_S: f64 = 1.0;

/// When the suite's first test began.
static BEGAN: OnceLock<Instant> = OnceLock::new();

/// A test's clock, which prints as the test ends how long it took and
/// how long the suite has run, against the suite's budget.
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
            "{}: {:.3} s; the suite {suite:.3} s so far, of its {BUDGET_S} s budget",
            self.name,
            self.began.elapsed().as_secs_f64()
        );
    }
}

/// Whether two pictures are the same: layout, size, colour and every
/// plane's bytes, stride and size. Says where they differ.
fn same_picture(a: &dyn VideoPlanes, b: &dyn VideoPlanes) -> Result<(), String> {
    if a.layout() != b.layout() {
        return Err(format!("layouts {:?} and {:?}", a.layout(), b.layout()));
    }
    if a.size() != b.size() {
        return Err(format!("sizes {:?} and {:?}", a.size(), b.size()));
    }
    if a.color() != b.color() {
        return Err(format!("colours {:?} and {:?}", a.color(), b.color()));
    }
    let shape = |p: &PlaneData<'_>| (p.stride, p.width, p.height);
    for i in 0..=a.layout().planes.len() {
        match (a.plane(i), b.plane(i)) {
            (None, None) => {}
            (Some(p), Some(q)) if shape(&p) != shape(&q) => {
                return Err(format!("plane {i}: {:?} and {:?}", shape(&p), shape(&q)));
            }
            (Some(p), Some(q)) if p.bytes != q.bytes => {
                return Err(format!("plane {i}'s bytes"));
            }
            (Some(_), Some(_)) => {}
            _ => return Err(format!("plane {i} on one only")),
        }
    }
    Ok(())
}

/// The copy of rung 1 that rung 3 uses is rung 1's own: its two tracks'
/// frames of `SeamLines` are, plane by plane, in layout and in colour,
/// the frames `instrument.rs` makes of the same camera, and its camera
/// and far tune through the boundary's `sphere_rig` give the rig
/// `instrument.rs` stitches them with, so `render_rig` of the one
/// renders what it renders of the other.
#[test]
fn synth_rung_matches_rung1() {
    let _t = timed("synth_rung_matches_rung1");
    let (cal, planted) = (x5_like(), x5_like_planted());
    let truth = compose(&cal, &planted, 0, [768; 2]);
    let camera = rung::camera();
    assert_eq!(camera.truth, truth);
    // Control: the camera with track 1 in front.
    must_fail(
        "the camera turned to track 1",
        camera.truth == compose(&cal, &planted, 1, [768; 2]),
    );
    let rung1 = |track: usize| {
        let picture = RenderCache::shared().render(Pattern::SeamLines, &truth, track, 0, 2);
        lens_frame(&lens_planes(&picture, Coding::X5), 768)
    };
    let pair = rung::pair(Pattern::SeamLines);
    let expected = [rung1(0), rung1(1)];
    for (track, (ours, theirs)) in pair.iter().zip(&expected).enumerate() {
        same_picture(ours, theirs).unwrap_or_else(|e| panic!("track {track}: {e}"));
    }
    // Controls: the other track's frame, and a frame whose colour is
    // limited range.
    must_fail(
        "track 0's frame against track 1's",
        same_picture(&pair[0], &expected[1]),
    );
    let mut limited = expected[0].clone();
    limited.color = color(Range::Limited, Matrix::Bt709, ChromaSiting::Left);
    must_fail("a limited-range frame", same_picture(&pair[0], &limited));

    // instrument.rs's far tune, as it writes it.
    let far = Tune {
        q_per_m: 0.0,
        ..Tune::default()
    };
    let rig = |tune: &Tune| -> Rig {
        sphere_rig(
            &camera.cal,
            &camera.planted,
            tune,
            rung::FRONT_TRACK,
            rung::SIDES,
        )
        .unwrap()
    };
    let rung1_rig = sphere_rig(&cal, &planted, &far, 0, [768; 2]).unwrap();
    assert_eq!(rung::far(), far);
    assert_eq!(rig(&rung::far()), rung1_rig);
    // Control: the default tune stitches at 2 m, another rig.
    must_fail("the default tune's rig", rig(&Tune::default()) == rung1_rig);
}
